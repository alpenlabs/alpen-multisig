use super::*;
use async_trait::async_trait;
use bdk_electrum::electrum_client::{Client, ConfigBuilder, ElectrumApi, Error};
use bitcoin::{ScriptBuf, Transaction, Txid};

pub(super) type History = Vec<(Transaction, Option<usize>)>;

#[async_trait]
pub trait Blockchain: Send + Sync {
    async fn history(&self, script: ScriptBuf) -> Result<History>;

    async fn block_time(&self, height: usize) -> Result<SystemTime>;

    async fn broadcast(&self, transaction: Transaction) -> Result<()>;
}

#[derive(Default)]
struct Connection {
    client: Option<Client>,
    transactions: HashMap<Txid, Transaction>,
}

pub struct Electrum {
    url: String,
    connection: Arc<std::sync::Mutex<Connection>>,
}

impl Electrum {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.into(),
            connection: Default::default(),
        }
    }

    async fn request<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Client, &mut HashMap<Txid, Transaction>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let url = self.url.clone();
        let connection = self.connection.clone();

        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| internal(anyhow::anyhow!("Electrum connection lock poisoned")))?;
            let Connection {
                client,
                transactions,
            } = &mut *connection;

            if client.is_none() {
                *client = Some(
                    Client::from_config(
                        &url,
                        ConfigBuilder::new()
                            .timeout(Some(Duration::from_secs(10)))
                            .build(),
                    )
                    .map_err(internal)?,
                );
            }

            let result = operation(client.as_ref().expect("connected above"), transactions);

            if result.is_err() {
                *client = None;
            }

            result
        })
        .await
        .map_err(internal)?
    }
}

#[async_trait]
impl Blockchain for Electrum {
    async fn history(&self, script: ScriptBuf) -> Result<History> {
        self.request(move |client, transactions| {
            client
                .script_get_history(&script)
                .map_err(internal)?
                .into_iter()
                .map(|entry| {
                    let transaction = match transactions.entry(entry.tx_hash) {
                        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                        std::collections::hash_map::Entry::Vacant(slot) => {
                            let transaction =
                                client.transaction_get(&entry.tx_hash).map_err(internal)?;

                            if transaction.compute_txid() != entry.tx_hash {
                                return Err(bad("Electrum transaction mismatch"));
                            }

                            slot.insert(transaction)
                        }
                    };

                    Ok((
                        transaction.clone(),
                        (0 < entry.height).then_some(entry.height as usize),
                    ))
                })
                .collect()
        })
        .await
    }

    async fn block_time(&self, height: usize) -> Result<SystemTime> {
        self.request(move |client, _| {
            unix_seconds::moment(u64::from(
                client.block_header(height).map_err(internal)?.time,
            ))
            .map_err(bad)
        })
        .await
    }

    async fn broadcast(&self, transaction: Transaction) -> Result<()> {
        self.request(move |client, _| {
            let expected = transaction.compute_txid();
            match client.transaction_broadcast(&transaction) {
                Ok(id) if id == expected => Ok(()),
                Ok(_) => Err(bad("Electrum broadcast txid mismatch")),
                Err(Error::Protocol(response)) => {
                    if client
                        .transaction_get(&expected)
                        .is_ok_and(|known| known.compute_txid() == expected)
                    {
                        return Ok(());
                    }

                    let reason = response["message"]
                        .as_str()
                        .or(response.as_str())
                        .unwrap_or("transaction rejected");
                    let reason: String = reason
                        .chars()
                        .filter(|character| !character.is_control())
                        .take(200)
                        .collect();

                    Err(bad(reason))
                }
                Err(error) => Err(internal(error)),
            }
        })
        .await
    }
}
