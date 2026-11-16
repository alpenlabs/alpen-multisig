//! Own sends, isolated by Bitcoin network and Ledger master fingerprint.
use bitcoin::{psbt::Psbt, Transaction};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct WalletSend {
    pub transaction: Transaction,
    pub psbt: Psbt,
    pub recipient: String,
    pub fee_sat: u64,
    pub sent_at: i64,
    pub max: bool,
    pub change_index: u32,
}
static STORE_LOCK: Mutex<()> = Mutex::new(());
static TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl WalletSend {
    fn validate(&self) -> Result<(), &'static str> {
        let mut unsigned = self.transaction.clone();
        for input in &mut unsigned.input {
            input.witness.clear();
        }
        if unsigned.input.is_empty()
            || !(1..=2).contains(&unsigned.output.len())
            || unsigned != self.psbt.unsigned_tx
            || self.psbt.inputs.len() != unsigned.input.len()
        {
            return Err("invalid transaction or PSBT");
        }
        let inputs = self.psbt.inputs.iter().try_fold(0u64, |sum, i| {
            sum.checked_add(i.witness_utxo.as_ref()?.value.to_sat())
        });
        let outputs = unsigned
            .output
            .iter()
            .try_fold(0u64, |sum, o| sum.checked_add(o.value.to_sat()));
        if inputs.zip(outputs).and_then(|(i, o)| i.checked_sub(o)) != Some(self.fee_sat) {
            return Err("invalid fee or prevouts");
        }
        Ok(())
    }
}
#[derive(Clone, Default)]
pub struct WalletSendStore(Arc<Inner>);
#[derive(Default)]
struct Inner {
    path: Option<PathBuf>,
    entries: Mutex<HashMap<String, Vec<WalletSend>>>,
    pub gate: tokio::sync::Mutex<()>,
}
impl WalletSendStore {
    pub fn new(dir: Option<PathBuf>) -> Self {
        let path = dir.map(|d| d.join("payout-wallet-sends.json"));
        Self(Arc::new(Inner {
            path,
            entries: Mutex::default(),
            gate: Default::default(),
        }))
    }
    pub async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.0.gate.lock().await
    }
    fn damaged(&self, reason: impl std::fmt::Display) -> String {
        format!(
            "Wallet sends file {} is damaged and was left untouched: {reason}",
            self.0
                .path
                .as_ref()
                .map_or_else(|| "<memory>".into(), |p| p.display().to_string())
        )
    }
    fn load(
        &self,
        memory: &HashMap<String, Vec<WalletSend>>,
    ) -> Result<HashMap<String, Vec<WalletSend>>, String> {
        let entries: HashMap<String, Vec<WalletSend>> = if let Some(path) = &self.0.path {
            match std::fs::read(path) {
                Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| self.damaged(e))?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
                Err(e) => return Err(self.damaged(e)),
            }
        } else {
            memory.clone()
        };
        for send in entries.values().flatten() {
            send.validate().map_err(|e| self.damaged(e))?;
        }
        Ok(entries)
    }
    pub fn get(&self, key: &str) -> Result<Vec<WalletSend>, String> {
        let _process = STORE_LOCK
            .lock()
            .map_err(|_| "wallet store lock poisoned")?;
        let memory = self
            .0
            .entries
            .lock()
            .map_err(|_| "wallet store lock poisoned")?;
        Ok(self.load(&memory)?.remove(key).unwrap_or_default())
    }
    pub fn update(
        &self,
        key: &str,
        update: impl FnOnce(&mut Vec<WalletSend>),
    ) -> Result<(), String> {
        let _process = STORE_LOCK
            .lock()
            .map_err(|_| "wallet store lock poisoned")?;
        let mut memory = self
            .0
            .entries
            .lock()
            .map_err(|_| "wallet store lock poisoned")?;
        let mut next = self.load(&memory)?;
        update(next.entry(key.into()).or_default());
        for send in next.values().flatten() {
            send.validate().map_err(|e| self.damaged(e))?;
        }
        if let Some(path) = &self.0.path {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let temp = path.with_extension(format!(
                "{}.{}.tmp",
                std::process::id(),
                TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let result = (|| {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temp)
                    .map_err(|e| e.to_string())?;
                file.write_all(&serde_json::to_vec(&next).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
                file.sync_all().map_err(|e| e.to_string())?;
                std::fs::rename(&temp, path).map_err(|e| e.to_string())
            })();
            if result.is_err() {
                let _ = std::fs::remove_file(temp);
            }
            result?;
        }
        *memory = next;
        Ok(())
    }
    #[cfg(test)]
    pub fn save(&self, key: &str, sends: Vec<WalletSend>) -> Result<(), String> {
        self.update(key, |current| *current = sends)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unreadable_store_is_not_treated_as_missing() {
        let dir = std::env::temp_dir().join(format!("unreadable-wallet-{}", std::process::id()));
        let path = dir.join("payout-wallet-sends.json");
        std::fs::create_dir_all(&path).unwrap();
        let store = WalletSendStore::new(Some(dir.clone()));
        assert!(store
            .get("wallet")
            .err()
            .unwrap()
            .contains("damaged and was left untouched"));
        assert!(store.update("wallet", |_| {}).is_err());
        assert!(path.is_dir());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stores_are_isolated_and_persist() {
        let dir = std::env::temp_dir().join(format!("wallet-sends-{}", std::process::id()));
        let store = WalletSendStore::new(Some(dir.clone()));
        let tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn::default()],
            output: vec![bitcoin::TxOut {
                value: bitcoin::Amount::from_sat(877),
                script_pubkey: bitcoin::ScriptBuf::new(),
            }],
        };
        let mut psbt = Psbt::from_unsigned_tx(tx.clone()).unwrap();
        psbt.inputs[0].witness_utxo = Some(bitcoin::TxOut {
            value: bitcoin::Amount::from_sat(1000),
            script_pubkey: bitcoin::ScriptBuf::new(),
        });
        let mut send = WalletSend {
            psbt,
            transaction: tx,
            recipient: "recipient".into(),
            fee_sat: 123,
            sent_at: 1,
            max: true,
            change_index: 0,
        };
        let second = WalletSendStore::new(Some(dir.clone()));
        store.save("regtest:1234", vec![send.clone()]).unwrap();
        let original = send.transaction.compute_txid();
        send.transaction.lock_time = bitcoin::absolute::LockTime::from_height(1).unwrap();
        send.psbt.unsigned_tx = send.transaction.clone();
        second
            .update("regtest:1234", |sends| sends.push(send))
            .unwrap();
        assert_eq!(store.get("regtest:1234").unwrap().len(), 2);
        store
            .update("regtest:1234", |sends| {
                sends.retain(|s| s.transaction.compute_txid() != original)
            })
            .unwrap();
        assert_eq!(second.get("regtest:1234").unwrap().len(), 1);
        let restored = WalletSendStore::new(Some(dir.clone()));
        assert_eq!(restored.get("regtest:1234").unwrap()[0].fee_sat, 123);
        assert!(restored.get("bitcoin:1234").unwrap().is_empty());
        assert!(restored.get("regtest:5678").unwrap().is_empty());
        restored.save("regtest:1234", vec![]).unwrap();
        assert!(WalletSendStore::new(Some(dir.clone()))
            .get("regtest:1234")
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
