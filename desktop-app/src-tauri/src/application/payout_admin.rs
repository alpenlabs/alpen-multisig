//! Payout workflows. Reviews are single-use snapshots held in Rust; device calls are blocking.
use super::wallet_session::WalletSession;
use crate::infrastructure::{
    hw_wallet::ledger::payout_admin as device, node_config_store::NodeConfig,
    payout_admin_policy_store::PolicyStore,
};
use bdk_wallet::KeychainKind;
use bitcoin::secp256k1::XOnlyPublicKey;
use bitcoin::{
    bip32::{ChildNumber, DerivationPath, Fingerprint},
    psbt::Psbt,
    Network, OutPoint, Transaction, Txid,
};
use payout_admin_core::{
    block_payouts_tx::{self, ChangeOutput, FeeInput},
    bundler::{self, AvailableCoin, BundledClaim},
    deployment, policy,
    report::{self, ConnectorState, MinedBlock, MinedTx},
    wire, AdminSigner, Deployment,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    str::FromStr,
    sync::{Arc, RwLock},
    time::{Duration, Instant, SystemTime},
};

macro_rules! session_id {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
        struct $name(uuid::Uuid);

        impl FromStr for $name {
            type Err = String;

            fn from_str(text: &str) -> Result<Self, String> {
                uuid::Uuid::parse_str(text)
                    .map(Self)
                    .map_err(|_| format!("Invalid {}", stringify!($name)))
            }
        }

        impl $name {
            fn new() -> Self {
                Self(uuid::Uuid::new_v4())
            }
        }
    };
}

session_id!(ReviewId);
session_id!(VerificationId);
session_id!(BundleId);
session_id!(BundledTransactionId);

async fn run_blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| error.to_string())?
}

#[derive(Clone)]
struct Coordinator {
    url: reqwest::Url,
    session: Option<Session>,
}

#[derive(Clone)]
struct Session {
    token: [u8; 32],
    expires: SystemTime,
}

impl Coordinator {
    async fn request<T: DeserializeOwned>(
        &self,
        route: &str,
        body: Option<Value>,
    ) -> Result<T, String> {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.expires <= SystemTime::now())
        {
            return Err("Sign in again: session expired".into());
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| error.to_string())?;
        let url = self.url.join(route).map_err(|error| error.to_string())?;
        let request = match body {
            Some(body) => client.post(url).json(&body),
            None => client.get(url),
        };
        let request = if let Some(session) = &self.session {
            request.bearer_auth(hex::encode(session.token))
        } else {
            request
        };

        let response = request.send().await.map_err(|error| {
            let origin = self.url.origin().ascii_serialization();

            format!("Coordinator at {origin}: {}", error.without_url())
        })?;

        if !response.status().is_success() {
            return Err(format!(
                "Coordinator {}: {}",
                response.status(),
                response.text().await.map_err(|error| error.to_string())?
            ));
        }

        response.json().await.map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
struct Identity {
    signer: AdminSigner,
    app: device::LedgerApp,
    network: Network,
}

#[derive(Clone)]
enum Payload {
    SignIn(Coordinator, wire::ChallengeResponse),
    Transaction(Psbt, bool),
}

enum Deadline {
    Challenge(SystemTime),
    Transaction(Instant),
}

struct Review {
    expires: Deadline,
    payload: Payload,
}

/// One sitting in the desktop app. One lock lets these fields change together.
/// Closing the app drops this state. Here “session” means the payout admin's sitting,
/// distinct from both the coordinator sign-in session and `WalletSession`.
#[derive(Default)]
struct SessionState {
    /// The params.toml and signer file from the most recent load.
    deployment: Option<Deployment>,
    /// Identity from the most recent Ledger read.
    identity: Option<Identity>,
    /// Set at sign-in, unusable after expiry, cleared by identity reads and deployment loads.
    coordinator: Option<Coordinator>,
    /// Single-use reviews refused after their deadline. A sign-in review, identity read
    /// or deployment load clears them all. Transaction reviews can remain here after expiry.
    open_reviews: HashMap<ReviewId, Review>,
    /// Valid claims and ID of the last report check, cleared on identity read or deployment load.
    ready_for_bundling: Option<(VerificationId, Vec<BundledClaim>)>,
    /// Last bundle and its PSBTs by transaction ID, cleared on identity read or deployment load.
    last_bundle: Option<(BundleId, HashMap<BundledTransactionId, Psbt>)>,
    /// IDs of this admin's own pending block payouts transactions. `WalletService` holds
    /// their fee coin reservations. This list identifies which reservations to release.
    /// A transaction this admin created is added just before the Ledger signs it, so it
    /// can be listed before the coordinator accepts it. Failed or rejected signing removes
    /// the ID and releases its coins. Refreshing Pending or bundling does the same for IDs
    /// the coordinator no longer lists as pending. Before each Admin Wallet Send, Estimate,
    /// or Fee bump, all entries and reservations are cleared and rebuilt from pending
    /// transactions that spend this wallet's coins. Closing the app drops them all.
    own_pending_txids: Vec<Txid>,
}

/// Desktop state shared across payout commands, including the active session, verified claims, and signing reviews.
pub struct PayoutAdminState {
    inner: tokio::sync::Mutex<SessionState>,
    policies: PolicyStore,
    config: Arc<RwLock<NodeConfig>>,
    wallet: WalletSession,
}

impl PayoutAdminState {
    /// Starts an empty payout session using the supplied node config, wallet session, and policy store directory.
    pub fn new(
        config: Arc<RwLock<NodeConfig>>,
        wallet: WalletSession,
        directory: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            inner: tokio::sync::Mutex::new(SessionState::default()),
            policies: PolicyStore::new(directory),
            config,
            wallet,
        }
    }

    fn admin_wallet_network(&self) -> Result<Network, String> {
        self.wallet
            .current()
            .map(|wallet| Ok(wallet.network()))
            .unwrap_or_else(|| {
                crate::infrastructure::network_env::network_from_env()
                    .map_err(|error| error.to_string())
            })
    }

    async fn with_electrum<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&bdk_electrum::electrum_client::Client) -> Result<T, String>
            + Send
            + 'static,
    ) -> Result<T, String> {
        let url = self
            .config
            .read()
            .map_err(|error| error.to_string())?
            .electrum_url()
            .to_string();

        run_blocking(move || {
            let electrum = bdk_electrum::electrum_client::Client::new(&url)
                .map_err(|error| format!("Electrum server at {url}: {error}"))?;

            operation(&electrum)
        })
        .await
    }

    /// Reads the Ledger account identity and optionally displays and checks its first receive address.
    pub async fn ledger_identity(&self, show_address: bool) -> Result<Value, String> {
        let mut state = self.inner.lock().await;
        let network = self.admin_wallet_network()?;
        let (app, fingerprint, xpub) = run_blocking(move || {
            let app = device::open_app()?;
            app.check_supported(network)?;
            let (fingerprint, xpub) = device::read_identity(&deployment::account_path(network))?;

            Ok((app, fingerprint, xpub))
        })
        .await?;
        let signer =
            AdminSigner::from_device(fingerprint, xpub).map_err(|error| error.to_string())?;

        if show_address {
            let policy = policy::admin_wallet_policy(&signer, network);
            let address =
                run_blocking(move || device::policy_address(&policy, None, 0, true)).await?;
            let expected = bitcoin::ScriptBuf::new_p2tr(
                &bitcoin::secp256k1::Secp256k1::new(),
                signer.xonly(),
                None,
            );

            if address.script_pubkey() != expected {
                return Err("Device address does not match admin key".into());
            }
        }

        state.open_reviews.clear();
        state.coordinator = None;
        state.last_bundle = None;
        state.ready_for_bundling = None;
        let result = json!({"adminKey":signer.xonly().to_string(), "expectedAddress":bitcoin::Address::p2tr(&bitcoin::secp256k1::Secp256k1::new(), signer.xonly(), None, device_network(network)).to_string(),"accountXpub":xpub,"fingerprint":signer.fingerprint().to_string()});
        state.identity = Some(Identity {
            signer,
            app,
            network,
        });

        Ok(result)
    }

    /// Checks both deployment files and the wallet network, then clears the previous coordinator session and reviews.
    pub async fn load_deployment(
        &self,
        params: std::path::PathBuf,
        signers: std::path::PathBuf,
    ) -> Result<Value, String> {
        let deployment = run_blocking(move || {
            Deployment::load(
                &std::fs::read_to_string(params).map_err(|error| error.to_string())?,
                Some(&std::fs::read_to_string(signers).map_err(|error| error.to_string())?),
            )
            .map_err(|error| error.to_string())
        })
        .await?;

        if deployment.network() != self.admin_wallet_network()? {
            return Err("Deployment network differs from Admin Wallet".into());
        }

        let result = json!({"network":deployment.network().to_string(),"deploymentDigest":deployment.digest(),"threshold":deployment.threshold(),"adminKeys":deployment.admin_keys().iter().map(ToString::to_string).collect::<Vec<_>>(),"nOfN":deployment.covenant().n_of_n_xonly().to_string(),"connectorAddress":policy::connector_address(&deployment).to_string(),"warnings":deployment.warnings()});
        let mut state = self.inner.lock().await;
        state.deployment = Some(deployment);
        state.coordinator = None;
        state.open_reviews.clear();
        state.last_bundle = None;
        state.ready_for_bundling = None;

        Ok(result)
    }

    /// Fetches a sign-in nonce and saves the challenge for the admin to review before signing.
    pub async fn sign_in_review(&self, url: String) -> Result<Value, String> {
        let mut state = self.inner.lock().await;
        let (deployment, identity) = state.checked_deployment_and_identity()?;
        let coordinator_client = Coordinator::new(&url)?;
        let challenge: wire::ChallengeResponse = coordinator_client
            .request(wire::CHALLENGE, Some(json!({})))
            .await?;
        let review_id = ReviewId::new();
        let message = wire::sign_in_message(
            &coordinator_client.url.origin().ascii_serialization(),
            &challenge.nonce,
        );
        let result = json!({"reviewId":review_id,"coordinatorUrl":coordinator_client.url.origin().ascii_serialization(),"adminKey":identity.signer.xonly().to_string(),"nonce":challenge.nonce,"expiresAtUnixSeconds":wire::unix_seconds::seconds(challenge.expires_at).map_err(|error| error.to_string())?,"deviceScreens":["Review message",format!("Path: {}",deployment::admin_key_path(deployment.network())),format!("Message: {message}"),"Sign message"]});
        state.open_reviews.clear();
        state.open_reviews.insert(
            review_id,
            Review {
                expires: Deadline::Challenge(challenge.expires_at),
                payload: Payload::SignIn(coordinator_client, challenge),
            },
        );

        Ok(result)
    }

    /// Consumes the review, signs the sign-in message on the Ledger, and saves the verified coordinator session.
    pub async fn sign_in(&self, review_id: String) -> Result<Value, String> {
        let mut state = self.inner.lock().await;
        let Payload::SignIn(mut coordinator_client, challenge) =
            state.take_review(review_id.parse()?)?
        else {
            return Err("Not a sign-in review".into());
        };
        state.coordinator = None;
        let (deployment, identity) = state.checked_deployment_and_identity()?;
        let signer = identity.signer.clone();
        let app = identity.app.clone();
        let network = deployment.network();
        let message = wire::sign_in_message(
            &coordinator_client.url.origin().ascii_serialization(),
            &challenge.nonce,
        );
        let signature = run_blocking(move || {
            if device::open_app()? != app {
                return Err("Ledger app changed; read identity and review again".into());
            }

            let signature = device::sign_message(
                &message,
                &deployment::admin_key_path(network),
                signer.fingerprint(),
            )?;

            if bitcoin::PublicKey::from_str(&signature.public_key_hex)
                .map_err(|error| error.to_string())?
                .inner
                .x_only_public_key()
                .0
                != signer.xonly()
            {
                return Err("Ledger signed with another key".into());
            }

            to_bip137_base64(
                &hex::decode(signature.signature_hex).map_err(|error| error.to_string())?,
            )
        })
        .await?;
        let session: wire::VerifyResponse = coordinator_client
            .request(
                wire::VERIFY,
                Some(
                    serde_json::to_value(wire::VerifyRequest {
                        nonce: challenge.nonce,
                        signature,
                    })
                    .map_err(|error| error.to_string())?,
                ),
            )
            .await?;

        if session.admin_key != identity.signer.xonly() || session.expires_at <= SystemTime::now() {
            return Err("Coordinator returned another identity or expired session".into());
        }

        coordinator_client.session = Some(Session {
            token: session.session_token,
            expires: session.expires_at,
        });
        state.coordinator = Some(coordinator_client);

        Ok(
            json!({"adminKey":session.admin_key,"expiresAtUnixSeconds":wire::unix_seconds::seconds(session.expires_at).map_err(|error| error.to_string())?}),
        )
    }
}

impl SessionState {
    fn checked_deployment_and_identity(&self) -> Result<(&Deployment, Identity), String> {
        let deployment = self.deployment.as_ref().ok_or("Load deployment first")?;
        let mut identity = self.identity.clone().ok_or("Read Ledger identity first")?;

        if identity.network != deployment.network() {
            return Err("Ledger identity network changed".into());
        }

        identity.signer = deployment
            .signers()
            .map_err(|error| error.to_string())?
            .iter()
            .find(|signer| {
                signer.xonly() == identity.signer.xonly()
                    && signer.fingerprint() == identity.signer.fingerprint()
                    && signer.account_xpub() == identity.signer.account_xpub()
            })
            .cloned()
            .ok_or("Ledger identity is not in signers.toml")?;

        Ok((deployment, identity))
    }

    fn take_review(&mut self, key: ReviewId) -> Result<Payload, String> {
        let review = self
            .open_reviews
            .remove(&key)
            .ok_or("Review is stale or already consumed")?;

        if match review.expires {
            Deadline::Challenge(time) => time <= SystemTime::now(),
            Deadline::Transaction(time) => time <= Instant::now(),
        } {
            return Err("Review expired; review again".into());
        }

        Ok(review.payload)
    }

    fn signed_in_coordinator(&self) -> Result<Coordinator, String> {
        self.coordinator.clone().ok_or("Sign in first".into())
    }
}

impl device::LedgerApp {
    fn check_supported(&self, network: Network) -> Result<(), String> {
        let expected = if network == Network::Bitcoin {
            "Bitcoin"
        } else {
            "Bitcoin Test"
        };

        if self.name != expected || self.version < (2, 4, 6) {
            return Err(format!("Open {expected} 2.4.6 or newer on Ledger"));
        }

        Ok(())
    }
}

use bdk_electrum::electrum_client::{ElectrumApi, Param};

fn fetch_transaction_with_block(
    electrum: &bdk_electrum::electrum_client::Client,
    txid: Txid,
) -> Result<Option<MinedTx>, String> {
    let value = match electrum.raw_call(
        "blockchain.transaction.get",
        [Param::String(txid.to_string()), Param::Bool(true)],
    ) {
        Ok(value) => value,
        Err(bdk_electrum::electrum_client::Error::Protocol(response))
            if response["code"] == -5
                || response["message"]
                    .as_str()
                    .or(response.as_str())
                    .is_some_and(|message| {
                        let message = message.to_ascii_lowercase();
                        message.contains("no such mempool or blockchain transaction")
                            || message.contains("transaction not found")
                            || message.contains("missing transaction")
                    }) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error.to_string()),
    };
    let transaction: Transaction = bitcoin::consensus::deserialize(
        &hex::decode(
            value["hex"]
                .as_str()
                .ok_or("Electrum verbose transaction has no hex")?,
        )
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    if transaction.compute_txid() != txid {
        return Err("Electrum returned a different transaction".into());
    }

    let confirmations = value["confirmations"].as_u64().unwrap_or(0);
    let block = if 0 < confirmations {
        Some(MinedBlock::new(
            (electrum
                .block_headers_subscribe()
                .map_err(|error| error.to_string())?
                .height as u64
                + 1)
            .checked_sub(confirmations)
            .ok_or("Electrum returned inconsistent confirmations")?,
            confirmations,
        ))
    } else {
        None
    };

    Ok(Some(MinedTx::new(transaction, block)))
}

#[derive(Default)]
struct SpendCheck {
    unspent: HashMap<bitcoin::ScriptBuf, HashSet<OutPoint>>,
    mined_spends: HashMap<bitcoin::ScriptBuf, HashSet<OutPoint>>,
}

impl SpendCheck {
    fn is_spent_in_block(
        &mut self,
        electrum: &bdk_electrum::electrum_client::Client,
        outpoint: OutPoint,
        script: &bitcoin::Script,
    ) -> Result<bool, String> {
        let unspent = match self.unspent.entry(script.to_owned()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(
                electrum
                    .script_list_unspent(script)
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .filter_map(|coin| {
                        Some(OutPoint::new(
                            coin.tx_hash,
                            u32::try_from(coin.tx_pos).ok()?,
                        ))
                    })
                    .collect(),
            ),
        };

        if unspent.contains(&outpoint) {
            return Ok(false);
        }

        // Absence from listunspent can be a mempool spend. Only a mined spender expires it.
        let spent = match self.mined_spends.entry(script.to_owned()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let mut spends = HashSet::new();
                for record in electrum
                    .script_get_history(script)
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .filter(|record| 0 < record.height)
                {
                    let transaction = electrum
                        .transaction_get(&record.tx_hash)
                        .map_err(|error| error.to_string())?;
                    spends.extend(
                        transaction
                            .input
                            .into_iter()
                            .map(|input| input.previous_output),
                    );
                }

                entry.insert(spends)
            }
        };

        Ok(spent.contains(&outpoint))
    }
}

fn check_inputs_on_chain(
    electrum: &bdk_electrum::electrum_client::Client,
    psbt: &Psbt,
) -> Result<(), String> {
    let mut spends = SpendCheck::default();
    for (input, metadata) in psbt.unsigned_tx.input.iter().zip(&psbt.inputs) {
        let parent = fetch_transaction_with_block(electrum, input.previous_output.txid)?
            .ok_or("Electrum does not know the input transaction")?;
        let prevout = parent
            .tx()
            .output
            .get(input.previous_output.vout as usize)
            .ok_or("Input output index is missing")?;

        if parent.block().is_none() || metadata.witness_utxo.as_ref() != Some(prevout) {
            return Err("Input is unconfirmed or its amount/script differs from the chain".into());
        }

        if spends.is_spent_in_block(electrum, input.previous_output, &prevout.script_pubkey)? {
            return Err(format!(
                "Input {} already spent in a block",
                input.previous_output
            ));
        }
    }

    Ok(())
}

impl PayoutAdminState {
    /// Checks pasted reports against Electrum and saves the valid claims under a verification ID for bundling.
    pub async fn check_reports(&self, text: String) -> Result<Value, String> {
        let mut state = self.inner.lock().await;
        let deployment = state.deployment.clone().ok_or("Load deployment first")?;
        let (parsed_reports, malformed) = report::parse_paste(&text, deployment.network())
            .map_err(|error| error.to_string())?
            .into_parts();
        let reports = self
            .with_electrum(move |electrum| {
                let connector_script = deployment.covenant().connector_script();

                let mut spends = SpendCheck::default();
                parsed_reports
                    .iter()
                    .map(|report| {
                        let mut transactions = HashMap::new();
                        for txid in report.txids() {
                            if let Some(transaction) = fetch_transaction_with_block(electrum, txid)?
                            {
                                transactions.insert(txid, transaction);
                            }
                        }
                        let state = if spends.is_spent_in_block(
                            electrum,
                            report.blocked_outpoint(),
                            &connector_script,
                        )? {
                            ConnectorState::SpentConfirmed
                        } else {
                            ConnectorState::Unspent
                        };

                        Ok(report::verify_report(
                            &deployment,
                            report,
                            &transactions,
                            Some(state),
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()
            })
            .await?;
        let claims = reports
            .iter()
            .filter(|report| matches!(report.verdict(), report::ReportVerdict::Valid))
            .map(|report| BundledClaim::new(report.new_claim_admin_payout_outpoint().txid))
            .collect();
        let key = VerificationId::new();
        state.ready_for_bundling = Some((key, claims));

        Ok(json!({"verificationId":key,"reports":reports,"malformed":malformed}))
    }

    /// Lists pending and past transactions, recomputing pending signing progress from the PSBTs.
    pub async fn pending_and_past(&self) -> Result<Value, String> {
        let mut state = self.inner.lock().await;
        let coordinator_client = state.signed_in_coordinator()?;
        let mut pending: Vec<wire::PendingBlockPayoutsView> =
            coordinator_client.request(wire::PENDING, None).await?;

        if let Some(wallet) = self.wallet.current() {
            release_stale_reservations(&mut state, &wallet, &pending);
        }

        let past: Vec<wire::PastBlockPayoutsView> =
            coordinator_client.request(wire::PAST, None).await?;
        let deployment = state.deployment.as_ref().ok_or("Load deployment first")?;
        let mut views = Vec::new();
        for pending_payout in &mut pending {
            let checked = block_payouts_tx::import_psbt(deployment, &pending_payout.psbt)
                .and_then(|psbt| block_payouts_tx::analyze(deployment, &psbt));
            let Ok(analysis) = checked else { continue };

            if analysis.block_payouts_txid() != pending_payout.txid {
                continue;
            }

            pending_payout.claim_input_count = analysis.connector_inputs().len();
            pending_payout.admins_signed = analysis.admins_signed();
            pending_payout.signatures_needed = analysis.signatures_needed();
            pending_payout.signed_by = analysis
                .signers()
                .iter()
                .map(|i| deployment.admin_keys()[*i])
                .collect();
            let mut view =
                serde_json::to_value(&*pending_payout).map_err(|error| error.to_string())?;
            view["claimInputs"] = json!(analysis
                .connector_inputs()
                .iter()
                .map(|connector_input| connector_input.admin_payout_outpoint().to_string())
                .collect::<Vec<_>>());
            views.push(view);
        }

        Ok(json!({"hidden":pending.len() - views.len(),"pending":views,"past":past}))
    }

    /// Copies the pending PSBT or its admin signatures, or the raw transaction for a past payout.
    pub async fn copy_transaction_text(
        &self,
        txid: String,
        signatures: bool,
    ) -> Result<String, String> {
        let state = self.inner.lock().await;
        let coordinator_client = state.signed_in_coordinator()?;
        let copy: wire::TransactionCopy = coordinator_client
            .request(
                &wire::TRANSACTIONS.replace(":txid", &parse_txid(&txid)?.to_string()),
                None,
            )
            .await?;
        match copy {
            wire::TransactionCopy::RawTx(raw) => Ok(raw),
            wire::TransactionCopy::Psbt(text) => {
                if !signatures {
                    return Ok(text);
                }

                let deployment = state.deployment.as_ref().ok_or("Load deployment first")?;
                let psbt = block_payouts_tx::import_psbt(deployment, &text)
                    .map_err(|error| error.to_string())?;

                Ok(block_payouts_tx::signatures_text(
                    &block_payouts_tx::export_signatures(deployment, &psbt)
                        .map_err(|error| error.to_string())?,
                ))
            }
        }
    }

    /// Checks the PSBT, its change, and its inputs against local data before submitting it to the coordinator.
    pub async fn import_psbt(&self, text: String) -> Result<wire::PendingBlockPayoutsView, String> {
        let state = self.inner.lock().await;
        let deployment = state.deployment.as_ref().ok_or("Load deployment first")?;
        let psbt =
            block_payouts_tx::import_psbt(deployment, &text).map_err(|error| error.to_string())?;
        check_change_output(deployment, &psbt)?;
        let check = psbt.clone();
        self.with_electrum(move |electrum| check_inputs_on_chain(electrum, &check))
            .await?;

        state.signed_in_coordinator()?
            .request(
                wire::PENDING,
                Some(json!({"psbt":block_payouts_tx::export_psbt(deployment, &psbt).map_err(|error| error.to_string())?})),
            )
            .await
    }

    /// Sends pasted signature lines to the coordinator for the specified pending transaction.
    pub async fn paste_signatures(
        &self,
        txid: String,
        signatures: String,
    ) -> Result<wire::PasteSignaturesView, String> {
        let state = self.inner.lock().await;

        state
            .signed_in_coordinator()?
            .request(
                &wire::SIGNATURES.replace(":txid", &parse_txid(&txid)?.to_string()),
                Some(
                    serde_json::to_value(wire::SignaturesRequest { signatures })
                        .map_err(|error| error.to_string())?,
                ),
            )
            .await
    }

    /// Attempts local Electrum broadcast, then requests the coordinator's broadcast result for the same txid.
    pub async fn broadcast_block_payouts_tx(
        &self,
        txid: String,
    ) -> Result<wire::BroadcastResponse, String> {
        let state = self.inner.lock().await;
        let coordinator_client = state.signed_in_coordinator()?;
        let txid = parse_txid(&txid)?;
        let copy: wire::TransactionCopy = coordinator_client
            .request(
                &wire::TRANSACTIONS.replace(":txid", &txid.to_string()),
                None,
            )
            .await?;
        let transaction = match copy {
            wire::TransactionCopy::RawTx(raw) => bitcoin::consensus::deserialize::<Transaction>(
                &hex::decode(raw).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?,
            wire::TransactionCopy::Psbt(text) => {
                let deployment = state.deployment.as_ref().ok_or("Load deployment first")?;

                block_payouts_tx::finalize(
                    deployment,
                    &block_payouts_tx::import_psbt(deployment, &text)
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?
            }
        };

        if transaction.compute_txid() != txid {
            return Err("Coordinator returned a different transaction".into());
        }

        let local_result = self
            .with_electrum(move |client| {
                client
                    .transaction_broadcast(&transaction)
                    .map_err(|error| error.to_string())
            })
            .await;

        let response: Result<wire::BroadcastResponse, String> = coordinator_client
            .request(
                &wire::BROADCAST.replace(":txid", &txid.to_string()),
                Some(json!({})),
            )
            .await;

        if local_result == Ok(txid) {
            let error = match &response {
                Ok(reply) => reply.broadcast_error.as_ref(),
                Err(error) => Some(error),
            };

            if let Some(error) = error {
                return Err(format!(
                    "Local Electrum broadcast succeeded. Coordinator broadcast failed: {error}"
                ));
            }
        }

        response
    }

    /// Groups selected verified claims and funds them at the requested rate in tenths of a sat/vB.
    pub async fn bundle_claims(
        &self,
        verification_id: String,
        fee_rate: u64,
        removed: Vec<String>,
    ) -> Result<Value, String> {
        let mut state = self.inner.lock().await;
        let (deployment, identity) = state.checked_deployment_and_identity()?;
        let deployment = deployment.clone();
        let removed = removed
            .iter()
            .map(|text| OutPoint::from_str(text))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let verification_id = verification_id.parse()?;
        let claims = state
            .ready_for_bundling
            .as_ref()
            .filter(|(key, _)| *key == verification_id)
            .map(|(_, claims)| claims)
            .ok_or("Reports are stale; verify again")?
            .iter()
            .filter(|claim| !removed.contains(&claim.outpoint()))
            .cloned()
            .collect::<Vec<_>>();
        let connector_script = deployment.covenant().connector_script();
        let recheck = claims.clone();
        let claims = self
            .with_electrum(move |electrum| {
                let mut spends = SpendCheck::default();
                recheck
                    .into_iter()
                    .filter_map(|claim| {
                        match spends.is_spent_in_block(
                            electrum,
                            claim.outpoint(),
                            &connector_script,
                        ) {
                            Ok(false) => Some(Ok(claim)),
                            Ok(true) => None,
                            Err(error) => Some(Err(error)),
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .await?;
        let wallet_service = self
            .wallet
            .current()
            .ok_or("Connect the Admin Wallet first")?;
        wallet_service
            .sync()
            .await
            .map_err(|error| error.to_string())?;
        let wallet = wallet_service.wallet.lock().await;
        check_admin_wallet_matches_signer(&wallet, &identity.signer, deployment.network())?;
        let pending: Vec<wire::PendingBlockPayoutsView> = state
            .signed_in_coordinator()?
            .request(wire::PENDING, None)
            .await?;
        release_stale_reservations(&mut state, &wallet_service, &pending);
        let reserved = wallet_service.reserved_outpoints();
        let blocked = inputs_of_pending_block_payouts_txs(&deployment, &pending)?;
        let mut fees = HashMap::new();
        for coin in wallet.list_unspent().filter(|unspent| {
            unspent.chain_position.is_confirmed()
                && !reserved.contains(&unspent.outpoint)
                && !blocked.contains(&unspent.outpoint)
        }) {
            let AdminWalletKey { key, path } = derive_admin_wallet_key(
                &identity.signer,
                deployment.network(),
                coin.keychain,
                coin.derivation_index,
            )?;
            fees.insert(coin.outpoint, (coin.txout, key, path));
        }
        let index = first_unused_change_index(&wallet);
        let AdminWalletKey {
            key: change_key,
            path: change_path,
        } = derive_admin_wallet_key(
            &identity.signer,
            deployment.network(),
            KeychainKind::Internal,
            index,
        )?;
        let change_script = bitcoin::ScriptBuf::new_p2tr(
            &bitcoin::secp256k1::Secp256k1::verification_only(),
            change_key,
            None,
        );
        let coins = fees
            .iter()
            .map(|(&outpoint, (txout, _, _))| AvailableCoin::new(outpoint, txout.value))
            .collect::<Vec<_>>();
        let bundle = bundler::bundle(
            &deployment,
            &claims,
            &coins,
            payout_admin_core::fees::FeeRate::new(fee_rate).map_err(|error| error.to_string())?,
            &change_script,
        )
        .map_err(|error| error.to_string())?;
        let mut transactions = Vec::new();
        let mut psbts = HashMap::new();
        for transaction in bundle.txs() {
            let key = BundledTransactionId::new();
            transactions.push(json!({"transactionId":key,"claims":transaction.claims(),"claimInputCount":transaction.claims().len(),"feeSat":transaction.fee().to_sat(),"vsize":transaction.vsize(),"changeSat":transaction.change().to_sat(),"errors":transaction.errors().iter().map(|error| error.message()).collect::<Vec<_>>() }));

            if transaction.is_ok() {
                let change = ChangeOutput::new(
                    transaction.change(),
                    change_key,
                    identity.signer.fingerprint(),
                    change_path.clone(),
                )
                .map_err(|error| error.to_string())?;
                let selected = transaction
                    .fee_coins()
                    .iter()
                    .map(|coin| {
                        let (txout, internal_key, path) = &fees[&coin.outpoint()];

                        FeeInput::new(
                            coin.outpoint(),
                            txout,
                            *internal_key,
                            identity.signer.fingerprint(),
                            path.clone(),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| error.to_string())?;
                psbts.insert(
                    key,
                    block_payouts_tx::build_psbt(
                        &deployment,
                        &transaction.connectors(),
                        &selected,
                        &change,
                    )
                    .map_err(|error| error.to_string())?,
                );
            }
        }
        let total_fee = bundle.total_fee().ok_or("total fee overflow")?;
        let bundle_id = BundleId::new();
        state.last_bundle = Some((bundle_id, psbts));

        Ok(
            json!({"bundleId":bundle_id,"transactions":transactions,"totalFeeSat":total_fee.to_sat()}),
        )
    }

    /// Checks a bundled or pending PSBT and saves it under a review ID for Ledger signing.
    pub async fn review_block_payouts_tx(
        &self,
        bundle: Option<(String, String)>,
        txid: Option<String>,
    ) -> Result<Value, String> {
        let mut state = self.inner.lock().await;
        let (deployment, identity) = state.checked_deployment_and_identity()?;
        let own = bundle.is_some();
        let mut psbt = match bundle {
            Some((bundle, transaction)) => {
                let bundle = bundle.parse()?;
                let transaction: BundledTransactionId = transaction.parse()?;

                state
                    .last_bundle
                    .as_ref()
                    .filter(|(key, _)| *key == bundle)
                    .and_then(|(_, transactions)| transactions.get(&transaction))
                    .cloned()
                    .ok_or("Bundle is stale or has errors")?
            }
            None => {
                let txid = parse_txid(&txid.ok_or("Missing transaction")?)?;
                let copy: wire::TransactionCopy = state
                    .signed_in_coordinator()?
                    .request(
                        &wire::TRANSACTIONS.replace(":txid", &txid.to_string()),
                        None,
                    )
                    .await?;
                let wire::TransactionCopy::Psbt(text) = copy else {
                    return Err("Transaction is already past".into());
                };
                let psbt = block_payouts_tx::import_psbt(deployment, &text)
                    .map_err(|error| error.to_string())?;

                if psbt.unsigned_tx.compute_txid() != txid {
                    return Err("Coordinator returned a different transaction".into());
                }

                psbt
            }
        };
        let analysis = block_payouts_tx::normalize(deployment, &mut psbt)
            .map_err(|error| error.to_string())?;
        check_change_output(deployment, &psbt)?;

        if analysis.vsize().saturating_mul(10_000) < analysis.fee().to_sat() {
            return Err("Transaction fee exceeds 10,000 sat/vB".into());
        }

        let copy = psbt.clone();
        self.with_electrum(move |electrum| check_inputs_on_chain(electrum, &copy))
            .await?;
        let key = ReviewId::new();
        let screens = identity.ledger_screens(
            deployment,
            &psbt,
            own || has_unsigned_own_fee_inputs(deployment, &identity.signer, &psbt)?,
            &self.policies,
        )?;
        let result = json!({"reviewId":key,"blockPayoutsTxid":analysis.block_payouts_txid().to_string(),"claims":analysis.connector_inputs().iter().map(|connector_input| BundledClaim::new(connector_input.admin_payout_outpoint().txid)).collect::<Vec<_>>(),"claimInputCount":analysis.connector_inputs().len(),"feeSat":analysis.fee().to_sat(),"vsize":analysis.vsize(),"changeAddress":bitcoin::Address::from_script(&psbt.unsigned_tx.output[0].script_pubkey,device_network(deployment.network())).map_err(|error| error.to_string())?.to_string(),"changeSat":analysis.change().to_sat(),"deviceScreens":screens,"pinLockWarning":15 <= analysis.connector_inputs().len(),"warnings":deployment.warnings()});
        state.open_reviews.insert(
            key,
            Review {
                expires: Deadline::Transaction(
                    Instant::now()
                        .checked_add(Duration::from_secs(300))
                        .ok_or("Review deadline out of range")?,
                ),
                payload: Payload::Transaction(psbt, own),
            },
        );

        Ok(result)
    }

    /// Consumes the transaction review, signs with the Ledger, and submits the PSBT or co-signer signatures.
    pub async fn sign_block_payouts_tx(
        &self,
        review_id: String,
        own: bool,
    ) -> Result<wire::PasteSignaturesView, String> {
        let mut state = self.inner.lock().await;
        let Payload::Transaction(mut psbt, expected_own) = state.take_review(review_id.parse()?)?
        else {
            return Err("Not a transaction review".into());
        };

        if own != expected_own {
            return Err("Review belongs to another action".into());
        }

        let (deployment, identity) = state.checked_deployment_and_identity()?;
        let deployment = deployment.clone();
        let coordinator_client = state.signed_in_coordinator()?;
        let analysis =
            block_payouts_tx::analyze(&deployment, &psbt).map_err(|error| error.to_string())?;

        if !own {
            let pending: Vec<wire::PendingBlockPayoutsView> =
                coordinator_client.request(wire::PENDING, None).await?;
            let current = pending
                .iter()
                .find(|pending_payout| pending_payout.txid == analysis.block_payouts_txid())
                .ok_or("Pending transaction expired or became past")?;
            let mut latest = block_payouts_tx::import_psbt(&deployment, &current.psbt)
                .map_err(|error| error.to_string())?;
            block_payouts_tx::normalize(&deployment, &mut latest)
                .map_err(|error| error.to_string())?;

            if latest != psbt {
                return Err("Pending transaction changed; review again".into());
            }
        }

        let copy = psbt.clone();
        self.with_electrum(move |electrum| check_inputs_on_chain(electrum, &copy))
            .await?;
        let wallet_service = self.wallet.current().ok_or("Connect Admin Wallet first")?;

        let fee_pass = own || has_unsigned_own_fee_inputs(&deployment, &identity.signer, &psbt)?;
        let previously_reserved = state
            .own_pending_txids
            .contains(&analysis.block_payouts_txid());

        if fee_pass {
            if !own {
                let status = wallet_service
                    .sync()
                    .await
                    .map_err(|error| error.to_string())?;

                if let Some(error) = status.last_error {
                    return Err(error.message);
                }
            }

            let wallet = wallet_service.wallet.lock().await;
            check_admin_wallet_matches_signer(&wallet, &identity.signer, deployment.network())?;

            if own
                && psbt.unsigned_tx.output[0].script_pubkey
                    != wallet
                        .peek_address(
                            bdk_wallet::KeychainKind::Internal,
                            first_unused_change_index(&wallet),
                        )
                        .address
                        .script_pubkey()
            {
                return Err("Change address is no longer first unused; bundle again".into());
            }

            let mut pending: Vec<wire::PendingBlockPayoutsView> =
                coordinator_client.request(wire::PENDING, None).await?;

            if !own {
                pending.retain(|record| record.txid != analysis.block_payouts_txid());
            }

            let blocked = inputs_of_pending_block_payouts_txs(&deployment, &pending)?;
            let reserved = wallet_service.reserved_outpoints();

            if analysis
                .fee_inputs()
                .iter()
                .filter(|input| own || !input.signed())
                .any(|fee_input| {
                    blocked.contains(&fee_input.outpoint())
                        || (own || !previously_reserved) && reserved.contains(&fee_input.outpoint())
                        || wallet.get_utxo(fee_input.outpoint()).is_none()
                })
            {
                return Err("Fee coins are no longer available; bundle again".into());
            }

            wallet_service.reserve_inputs(&psbt.unsigned_tx);

            if !previously_reserved {
                state.own_pending_txids.push(analysis.block_payouts_txid());
            }
        }

        let signed = self
            .sign_on_ledger(&deployment, &identity, &mut psbt, fee_pass)
            .await;

        if let Err(error) = signed {
            if fee_pass && !previously_reserved {
                wallet_service.release_reservation(analysis.block_payouts_txid());
                state
                    .own_pending_txids
                    .retain(|txid| *txid != analysis.block_payouts_txid());
            }

            return Err(error);
        }

        // Keep reservations on transport failure: the coordinator may have received the PSBT.
        if fee_pass {
            let pending: wire::PendingBlockPayoutsView = coordinator_client
                .request(
                    wire::PENDING,
                    Some(json!({"psbt":block_payouts_tx::export_psbt(&deployment,&psbt).map_err(|error| error.to_string())?})),
                )
                .await?;

            Ok(wire::PasteSignaturesView {
                added: pending.added,
                already_present: 0,
                invalid: vec![],
                pending: Some(pending),
            })
        } else {
            let signatures = block_payouts_tx::signatures_text(
                &block_payouts_tx::export_signatures(&deployment, &psbt)
                    .map_err(|error| error.to_string())?,
            );

            coordinator_client
                .request(
                    &wire::SIGNATURES.replace(":txid", &analysis.block_payouts_txid().to_string()),
                    Some(json!({"signatures":signatures})),
                )
                .await
        }
    }

    async fn sign_on_ledger(
        &self,
        deployment: &Deployment,
        identity: &Identity,
        psbt: &mut Psbt,
        own: bool,
    ) -> Result<(), String> {
        let app = identity.app.clone();
        run_blocking(move || {
            if device::open_app()? == app {
                Ok(())
            } else {
                Err("Ledger app changed; read identity again".into())
            }
        })
        .await?;
        let fingerprint = identity.signer.fingerprint();
        let policy =
            policy::admin_multisig_policy(deployment).map_err(|error| error.to_string())?;
        let policy_id = device::policy_id(&policy);
        let hmac = match self.policies.get(fingerprint, &policy_id) {
            Some(hmac) => hmac,
            None => {
                let registration_policy = policy.clone();
                let expected = deployment.covenant().connector_script();
                let hmac = run_blocking(move || {
                    let hmac = device::register_policy(&registration_policy, fingerprint)?;
                    let address =
                        device::policy_address(&registration_policy, Some(hmac), 0, false)?;

                    if address.script_pubkey() != expected {
                        return Err("Ledger policy does not match connector".into());
                    }

                    Ok(hmac)
                })
                .await?;
                self.policies.save(fingerprint, &policy_id, &hmac)?;

                hmac
            }
        };
        let mut work = psbt.clone();
        let fee_policy = policy::admin_wallet_policy(&identity.signer, deployment.network());
        work = run_blocking(move || {
            device::sign_psbt(&mut work, &policy, Some(hmac), fingerprint)?;

            if own {
                device::sign_psbt(&mut work, &fee_policy, None, fingerprint)?;
            }

            Ok(work)
        })
        .await?;

        if work.unsigned_tx != psbt.unsigned_tx {
            return Err("Device changed reviewed transaction".into());
        }

        let analysis = block_payouts_tx::normalize(deployment, &mut work)
            .map_err(|error| error.to_string())?;

        if !analysis.signed_by(identity.signer.index())
            || own
                && analysis
                    .fee_inputs()
                    .iter()
                    .any(|fee_input| !fee_input.signed())
        {
            return Err("Ledger did not sign every required input".into());
        }

        *psbt = work;

        Ok(())
    }
}

impl Coordinator {
    /// Builds the coordinator origin from the app's coordinator URL. The other roles use
    /// the `/api/v1` path, which the payout routes already include.
    fn new(url: &str) -> Result<Self, String> {
        let parsed = reqwest::Url::parse(url.trim()).map_err(|error| error.to_string())?;

        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !matches!(parsed.path(), "/" | "/api/v1" | "/api/v1/")
            || !(parsed.scheme() == "https"
                || parsed.scheme() == "http"
                    && matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
        {
            return Err("Use an HTTPS coordinator origin (HTTP is allowed on localhost)".into());
        }

        Ok(Self {
            url: parsed.join("/").map_err(|error| error.to_string())?,
            session: None,
        })
    }
}

fn parse_txid(text: &str) -> Result<Txid, String> {
    Txid::from_str(text).map_err(|error| error.to_string())
}

fn release_stale_reservations(
    state: &mut SessionState,
    wallet: &super::wallet_service::WalletService,
    pending: &[wire::PendingBlockPayoutsView],
) {
    state.own_pending_txids.retain(|txid| {
        let live = pending.iter().any(|transaction| transaction.txid == *txid);

        if !live {
            wallet.release_reservation(*txid);
        }

        live
    });
}

fn inputs_of_pending_block_payouts_txs(
    deployment: &Deployment,
    pending: &[wire::PendingBlockPayoutsView],
) -> Result<Vec<OutPoint>, String> {
    let mut result = Vec::new();
    for pending_payout in pending {
        result.extend(
            block_payouts_tx::import_psbt(deployment, &pending_payout.psbt)
                .map_err(|_| "A pending block payouts transaction from the coordinator does not match this machine's params.toml.")?
                .unsigned_tx
                .input
                .iter()
                .map(|input| input.previous_output),
        );
    }

    Ok(result)
}

struct AdminWalletKey {
    key: XOnlyPublicKey,
    path: DerivationPath,
}

fn derive_admin_wallet_key(
    me: &AdminSigner,
    network: Network,
    keychain: KeychainKind,
    index: u32,
) -> Result<AdminWalletKey, String> {
    let branch = u32::from(keychain == KeychainKind::Internal);
    let suffix = [
        ChildNumber::Normal { index: branch },
        ChildNumber::from_normal_idx(index).map_err(|error| error.to_string())?,
    ];
    let key = me
        .account_xpub()
        .derive_pub(&bitcoin::secp256k1::Secp256k1::new(), &suffix)
        .map_err(|error| error.to_string())?
        .public_key
        .x_only_public_key()
        .0;

    Ok(AdminWalletKey {
        key,
        path: deployment::account_path(network).extend(suffix),
    })
}

fn check_admin_wallet_matches_signer(
    wallet: &bdk_wallet::Wallet,
    me: &AdminSigner,
    network: Network,
) -> Result<(), String> {
    if wallet.network() != network
        || wallet
            .peek_address(bdk_wallet::KeychainKind::External, 0)
            .address
            .script_pubkey()
            != bitcoin::ScriptBuf::new_p2tr(&bitcoin::secp256k1::Secp256k1::new(), me.xonly(), None)
    {
        return Err("Admin Wallet differs from payout Ledger identity".into());
    }

    Ok(())
}

fn check_change_output(deployment: &Deployment, psbt: &Psbt) -> Result<(), String> {
    let output = &psbt.outputs[0];
    for (key, (leaves, (fingerprint, path))) in &output.tap_key_origins {
        if !leaves.is_empty() {
            continue;
        }

        let parts = path.as_ref();

        if parts.len() != 5 || parts[3] != (ChildNumber::Normal { index: 1 }) {
            continue;
        }

        let ChildNumber::Normal { index } = parts[4] else {
            continue;
        };
        for signer in deployment
            .signers()
            .map_err(|error| error.to_string())?
            .iter()
            .filter(|signer| signer.fingerprint() == *fingerprint)
        {
            let AdminWalletKey {
                key: expected,
                path: expected_path,
            } = derive_admin_wallet_key(
                signer,
                deployment.network(),
                KeychainKind::Internal,
                index,
            )?;

            if *key == expected
                && *path == expected_path
                && psbt.unsigned_tx.output[0].script_pubkey
                    == bitcoin::ScriptBuf::new_p2tr(
                        &bitcoin::secp256k1::Secp256k1::new(),
                        expected,
                        None,
                    )
            {
                return Ok(());
            }
        }
    }

    Err("Change is not a verified Admin Wallet change address".into())
}

// Both supported app versions use this threshold. In 2.5.1 it requires FULL display:
// Taproot DEFAULT/ALL passes commit all input amounts and outputs.
fn is_fee_high(fee: u64, inputs: u64) -> bool {
    u128::from(inputs) <= u128::from(fee) * 10 && 100_000 < inputs
}

fn device_network(network: Network) -> Network {
    if network == Network::Bitcoin {
        Network::Bitcoin
    } else {
        Network::Testnet
    }
}

fn format_key_slots(slot_count: usize) -> String {
    let slots: Vec<String> = (1..=slot_count).map(|i| format!("@{i}")).collect();
    match slots.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => slots.join(""),
    }
}

impl Identity {
    fn ledger_screens(
        &self,
        deployment: &Deployment,
        psbt: &Psbt,
        own: bool,
        store: &PolicyStore,
    ) -> Result<Vec<String>, String> {
        let analysis =
            block_payouts_tx::analyze(deployment, psbt).map_err(|error| error.to_string())?;
        let policy =
            policy::admin_multisig_policy(deployment).map_err(|error| error.to_string())?;
        let newer = (2, 5, 0) <= self.app.version;
        let warns_high_fee = is_fee_high(analysis.fee().to_sat(), analysis.input_total().to_sat());
        let mut lines = Vec::new();

        if (2, 5, 1) < self.app.version {
            lines.push("Screens checked on Bitcoin 2.4.6 and 2.5.1; reject if this newer app shows different values".into());
        }

        if store
            .get(self.signer.fingerprint(), &device::policy_id(&policy))
            .is_none()
        {
            lines.push(format!("Account name: {}", policy.name()));

            if newer {
                lines.extend([
                    "Primary spending path: Main path: spendable by @0".into(),
                    format!(
                        "Spending path #1: Any {} of {} must sign",
                        deployment.threshold(),
                        format_key_slots(policy.keys().len() - 1)
                    ),
                ]);
            }

            lines.push(format!("Wallet policy: {}", policy.template()));
            lines.extend(
                device::to_ledger_policy(&policy)
                    .keys
                    .iter()
                    .enumerate()
                    .map(|(i, derived_key)| {
                        format!(
                            "{} key@{i}: {derived_key}",
                            if i == self.signer.index() + 1 {
                                "Our"
                            } else {
                                "Their"
                            }
                        )
                    }),
            );
        }

        let device_network = device_network(deployment.network());
        let amount = |sats| {
            if newer {
                let unit = if device_network == Network::Bitcoin {
                    "BTC"
                } else {
                    "TEST"
                };
                let decimal =
                    bitcoin::Amount::from_sat(sats).to_string_in(bitcoin::Denomination::Bitcoin);

                format!("{decimal} {unit}")
            } else {
                format!("{sats} sats")
            }
        };
        let fee_inputs: u64 = analysis
            .fee_inputs()
            .iter()
            .map(|fee_input| fee_input.value().to_sat())
            .sum();
        let claims = analysis.input_total().to_sat() - fee_inputs;
        let warning = "Security risk: There are external inputs. Reject if not sure. Continue anyway only if all reviewed values match (the other signing pass owns these inputs).";
        lines.extend([
            "Sign claim inputs".into(),
            warning.into(),
            format!(
                "{}: {}",
                if newer { "From account" } else { "From" },
                policy.name()
            ),
            format!("Amount: {}", amount(analysis.change().to_sat())),
            format!(
                "To: {}",
                bitcoin::Address::from_script(
                    &psbt.unsigned_tx.output[0].script_pubkey,
                    device_network
                )
                .map_err(|error| error.to_string())?
            ),
        ]);

        if warns_high_fee {
            lines.push("High fees warning — Fees are above 10% of total amount".into());
        }

        if newer {
            lines.extend([
                format!("External amounts: {}", amount(fee_inputs)),
                format!("You spend: {}", amount(claims)),
            ]);
        }

        lines.push(format!("Fees: {}", amount(analysis.fee().to_sat())));

        if own {
            lines.extend(["Sign fee inputs".into(), warning.into()]);

            if !newer {
                lines.push(format!(
                    "Amount: 0 {} (self-transfer)",
                    if deployment.network() == Network::Bitcoin {
                        "BTC"
                    } else {
                        "TEST"
                    }
                ));
            }

            if warns_high_fee {
                lines.push("High fees warning — Fees are above 10% of total amount".into());
            }

            if newer {
                lines.extend([
                    format!("External amounts: {}", amount(claims)),
                    if analysis.change().to_sat() <= fee_inputs {
                        format!(
                            "You spend: {}",
                            amount(fee_inputs - analysis.change().to_sat())
                        )
                    } else {
                        format!(
                            "You receive: {}",
                            amount(analysis.change().to_sat() - fee_inputs)
                        )
                    },
                ]);
            }

            lines.push(format!("Fees: {}", amount(analysis.fee().to_sat())));
        }

        Ok(lines)
    }
}

impl PayoutAdminState {
    /// Refresh before Send, including after restart. An unavailable coordinator fails closed.
    pub async fn refresh_own_pending_reservations(&self) -> Result<impl Drop + '_, String> {
        let mut state = self.inner.lock().await;

        if state.deployment.is_none()
            && self
                .config
                .read()
                .map_err(|error| error.to_string())?
                .payout_params_path
                .is_none()
        {
            return Ok(state);
        }

        let coordinator_client = state.signed_in_coordinator()?;
        let deployment = state
            .deployment
            .as_ref()
            .ok_or("Load payout deployment before sending")?;
        let pending: Vec<wire::PendingBlockPayoutsView> =
            coordinator_client.request(wire::PENDING, None).await?;
        let transactions = pending
            .iter()
            .map(|pending_payout| {
                block_payouts_tx::import_psbt(deployment, &pending_payout.psbt)
                    .map_err(|_| "A pending block payouts transaction from the coordinator does not match this machine's params.toml.")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let wallet_service = self.wallet.current().ok_or("Connect Admin Wallet first")?;
        let wallet = wallet_service.wallet.lock().await;
        for txid in &state.own_pending_txids {
            wallet_service.release_reservation(*txid);
        }
        state.own_pending_txids.clear();
        for psbt in transactions {
            if !psbt.inputs.iter().any(|input| {
                input
                    .witness_utxo
                    .as_ref()
                    .is_some_and(|output| wallet.is_mine(output.script_pubkey.clone()))
            }) {
                continue;
            }

            let transaction = psbt.unsigned_tx;
            wallet_service.reserve_inputs(&transaction);
            state.own_pending_txids.push(transaction.compute_txid());
        }

        Ok(state)
    }
}

fn has_unsigned_own_fee_inputs(
    deployment: &Deployment,
    me: &AdminSigner,
    psbt: &Psbt,
) -> Result<bool, String> {
    let analysis =
        block_payouts_tx::analyze(deployment, psbt).map_err(|error| error.to_string())?;

    if !analysis
        .fee_inputs()
        .iter()
        .any(|fee_input| !fee_input.signed())
    {
        return Ok(false);
    }

    let ours = |origins: &std::collections::BTreeMap<
        XOnlyPublicKey,
        (Vec<bitcoin::TapLeafHash>, (Fingerprint, DerivationPath)),
    >,
                script: &bitcoin::Script| {
        origins.iter().any(|(key, (_, (fingerprint, path)))| {
            let parts = path.as_ref();

            if *fingerprint != me.fingerprint() || parts.len() != 5 {
                return false;
            }

            let (ChildNumber::Normal { index: branch }, ChildNumber::Normal { index }) =
                (parts[3], parts[4])
            else {
                return false;
            };
            let keychain = match branch {
                0 => KeychainKind::External,
                1 => KeychainKind::Internal,
                _ => return false,
            };

            derive_admin_wallet_key(me, deployment.network(), keychain, index).is_ok_and(
                |AdminWalletKey {
                     key: derived_key,
                     path: derived_path,
                 }| {
                    derived_key == *key
                        && derived_path == *path
                        && script
                            == &bitcoin::ScriptBuf::new_p2tr(
                                &bitcoin::secp256k1::Secp256k1::new(),
                                derived_key,
                                None,
                            )
                },
            )
        })
    };

    Ok(ours(
        &psbt.outputs[0].tap_key_origins,
        &psbt.unsigned_tx.output[0].script_pubkey,
    ) && analysis
        .fee_inputs()
        .iter()
        .filter(|fee_input| !fee_input.signed())
        .all(|fee_input| {
            psbt.inputs[fee_input.input_index()]
                .witness_utxo
                .as_ref()
                .is_some_and(|previous_output| {
                    ours(
                        &psbt.inputs[fee_input.input_index()].tap_key_origins,
                        &previous_output.script_pubkey,
                    )
                })
        }))
}

fn first_unused_change_index(wallet: &bdk_wallet::Wallet) -> u32 {
    wallet
        .spk_index()
        .unused_keychain_spks(bdk_wallet::KeychainKind::Internal)
        .next()
        .map(|(index, _)| index)
        .unwrap_or_else(|| wallet.next_derivation_index(bdk_wallet::KeychainKind::Internal))
}

fn to_bip137_base64(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() != 65 || bytes[64] > 3 {
        return Err("Invalid Ledger signature".into());
    }

    let mut compact = vec![31 + bytes[64]];
    compact.extend_from_slice(&bytes[..64]);

    Ok(
        bitcoin::sign_message::MessageSignature::from_slice(&compact)
            .map_err(|error| error.to_string())?
            .to_base64(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::hashes::Hash;
    use payout_admin_core::test_support::{change, fee_coin, Fixture};

    #[test]
    fn coordinator_origin_accepts_the_app_coordinator_url() {
        for url in [
            "http://127.0.0.1:3000/api/v1",
            "http://127.0.0.1:3000/api/v1/",
            "http://127.0.0.1:3000",
        ] {
            assert_eq!(
                Coordinator::new(url).unwrap().url.as_str(),
                "http://127.0.0.1:3000/"
            );
        }
        for url in [
            "http://example.com",
            "https://example.com/other",
            "https://u:p@example.com",
        ] {
            assert!(Coordinator::new(url).is_err());
        }
    }

    #[test]
    fn sign_in_compact_signature_preserves_recovery_id_and_key() {
        let fixture = Fixture::new(2, 1);
        let key = fixture.admin_keypair(0);
        let message = bitcoin::secp256k1::Message::from_digest(
            bitcoin::sign_message::signed_msg_hash("nonce").to_byte_array(),
        );
        let signature = bitcoin::secp256k1::Secp256k1::new()
            .sign_ecdsa_recoverable(&message, &key.secret_key());
        let (recovery, bytes) = signature.serialize_compact();
        let mut ledger = bytes.to_vec();
        ledger.push(recovery.to_i32() as u8);
        let encoded = to_bip137_base64(&ledger).unwrap();
        let parsed = bitcoin::sign_message::MessageSignature::from_base64(&encoded).unwrap();
        assert_eq!(parsed.signature, signature);
        assert!(parsed.compressed);
        ledger[64] = 4;
        assert!(to_bip137_base64(&ledger).is_err());
    }

    #[test]
    fn reviews_are_single_use_and_expire_before_device_access() {
        let mut inner = SessionState::default();
        let live = ReviewId::new();
        let expired = ReviewId::new();
        let payload = Payload::SignIn(
            Coordinator {
                url: "http://localhost".parse().unwrap(),
                session: None,
            },
            wire::ChallengeResponse {
                nonce: "nonce".into(),
                expires_at: SystemTime::now()
                    .checked_add(Duration::from_secs(1))
                    .unwrap(),
            },
        );
        inner.open_reviews.insert(
            live,
            Review {
                expires: Deadline::Challenge(
                    SystemTime::now()
                        .checked_add(Duration::from_secs(1))
                        .unwrap(),
                ),
                payload: payload.clone(),
            },
        );
        assert!(inner.take_review(live).is_ok());
        assert!(inner.take_review(live).is_err());
        inner.open_reviews.insert(
            expired,
            Review {
                expires: Deadline::Challenge(SystemTime::UNIX_EPOCH),
                payload,
            },
        );
        assert!(inner.take_review(expired).is_err());
        assert!(inner.open_reviews.is_empty());
    }

    #[test]
    fn ledger_high_fee_boundaries() {
        assert!(!is_fee_high(10_000, 100_000));
        assert!(!is_fee_high(10_000, 100_001));
        assert!(is_fee_high(10_001, 100_001));
        assert!(is_fee_high(20_000, 200_000));
        assert!(!is_fee_high(19_999, 200_000));
        assert_eq!(format_key_slots(3), "@1, @2 and @3");
    }

    #[test]
    fn change_and_version_specific_device_checklists_are_bound_to_transaction() {
        let fixture = Fixture::new(3, 2);
        let deployment = fixture.deployment();
        let coin = fee_coin(&fixture, 0, 0, 50_000);
        let psbt = block_payouts_tx::build_psbt(
            &deployment,
            &[block_payouts_tx::ConnectorRef::new(Txid::from_byte_array(
                [7; 32],
            ))],
            &[coin],
            &change(&fixture, 49_000),
        )
        .unwrap();
        check_change_output(&deployment, &psbt).unwrap();
        assert!(
            has_unsigned_own_fee_inputs(&deployment, &deployment.signers().unwrap()[0], &psbt)
                .unwrap()
        );
        assert!(!has_unsigned_own_fee_inputs(
            &deployment,
            &deployment.signers().unwrap()[1],
            &psbt
        )
        .unwrap());
        // Correctly derived origins do not prove ownership of a different script.
        let mut misleading = psbt.clone();
        let other = deployment.signers().unwrap()[1].clone();
        let AdminWalletKey { key, path } =
            derive_admin_wallet_key(&other, deployment.network(), KeychainKind::Internal, 0)
                .unwrap();
        misleading.outputs[0]
            .tap_key_origins
            .insert(key, (vec![], (other.fingerprint(), path)));
        misleading.unsigned_tx.output[0].script_pubkey =
            bitcoin::ScriptBuf::new_p2tr(&bitcoin::secp256k1::Secp256k1::new(), key, None);
        misleading.outputs[0].tap_internal_key = Some(key);
        check_change_output(&deployment, &misleading).unwrap();
        assert!(!has_unsigned_own_fee_inputs(
            &deployment,
            &deployment.signers().unwrap()[0],
            &misleading
        )
        .unwrap());
        misleading = psbt.clone();
        misleading.inputs[1].tap_internal_key = Some(other.xonly());
        misleading.inputs[1]
            .witness_utxo
            .as_mut()
            .unwrap()
            .script_pubkey = bitcoin::ScriptBuf::new_p2tr(
            &bitcoin::secp256k1::Secp256k1::new(),
            other.xonly(),
            None,
        );
        assert!(!has_unsigned_own_fee_inputs(
            &deployment,
            &deployment.signers().unwrap()[0],
            &misleading
        )
        .unwrap());
        let pending = wire::PendingBlockPayoutsView {
            txid: psbt.unsigned_tx.compute_txid(),
            psbt: psbt.to_string(),
            expires_at: SystemTime::now()
                .checked_add(Duration::from_secs(1))
                .unwrap(),
            claim_input_count: 1,
            admins_signed: 0,
            added: 0,
            signatures_needed: 2,
            signed_by: vec![],
            duplicate_outpoints: vec![],
            broadcast_error: None,
        };
        let blocked = inputs_of_pending_block_payouts_txs(&deployment, &[pending]).unwrap();
        assert_eq!(
            blocked,
            psbt.unsigned_tx
                .input
                .iter()
                .map(|input| input.previous_output)
                .collect::<Vec<_>>()
        );
        let mut identity = Identity {
            signer: deployment.signers().unwrap()[0].clone(),
            app: device::LedgerApp {
                name: "Bitcoin Test".into(),
                version: (2, 4, 6),
            },
            network: deployment.network(),
        };
        let store = PolicyStore::new(None);
        let old = identity
            .ledger_screens(&deployment, &psbt, true, &store)
            .unwrap();
        assert!(old
            .iter()
            .any(|screen| screen == "Amount: 0 TEST (self-transfer)"));
        assert!(old.iter().any(|screen| screen.starts_with("To: tb1p")));
        assert!(!old
            .iter()
            .any(|screen| screen.starts_with("External amounts:")));
        identity.app.version = (2, 5, 1);
        let new = identity
            .ledger_screens(&deployment, &psbt, true, &store)
            .unwrap();
        assert!(new
            .iter()
            .any(|screen| screen == "External amounts: 0.0005 TEST"));
        assert!(new
            .iter()
            .any(|screen| screen == "External amounts: 0.0000033 TEST"));
        assert!(new
            .iter()
            .any(|screen| screen == "Spending path #1: Any 2 of @1, @2 and @3 must sign"));
        let expensive = block_payouts_tx::build_psbt(
            &deployment,
            &[block_payouts_tx::ConnectorRef::new(Txid::from_byte_array(
                [9; 32],
            ))],
            &[fee_coin(&fixture, 0, 0, 200_000)],
            &change(&fixture, 150_000),
        )
        .unwrap();
        for version in [(2, 4, 6), (2, 5, 1)] {
            identity.app.version = version;
            for own in [false, true] {
                let lines = identity
                    .ledger_screens(&deployment, &expensive, own, &store)
                    .unwrap();
                assert_eq!(
                    lines
                        .iter()
                        .filter(|screen| screen.starts_with("High fees warning"))
                        .count(),
                    if own { 2 } else { 1 }
                );
            }
        }
        assert!(!old
            .iter()
            .any(|screen_line| screen_line.starts_with("High fees warning")));
        let policy = policy::admin_multisig_policy(&deployment).unwrap();
        store
            .save(
                identity.signer.fingerprint(),
                &device::policy_id(&policy),
                &[1; 32],
            )
            .unwrap();
        assert!(!identity
            .ledger_screens(&deployment, &psbt, false, &store)
            .unwrap()
            .iter()
            .any(|screen_line| screen_line.starts_with("Account name:")));
        let mut foreign = psbt.clone();
        foreign.outputs[0].tap_key_origins.clear();
        assert!(check_change_output(&deployment, &foreign).is_err());
        assert!((device::LedgerApp {
            name: "Bitcoin Test".into(),
            version: (2, 4, 5)
        })
        .check_supported(Network::Regtest)
        .is_err());
        assert!(identity.app.check_supported(Network::Bitcoin).is_err());
    }

    #[test]
    fn electrum_mempool_conflict_does_not_expire_a_connector() {
        use std::io::{BufRead, BufReader, Write};
        for height in [0, 120] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("tcp://{}", listener.local_addr().unwrap());
            let outpoint = OutPoint {
                txid: Txid::from_byte_array([8; 32]),
                vout: 1,
            };
            let transaction = Transaction {
                version: bitcoin::transaction::Version(3),
                lock_time: bitcoin::absolute::LockTime::ZERO,
                input: vec![bitcoin::TxIn {
                    previous_output: outpoint,
                    ..Default::default()
                }],
                output: vec![],
            };
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();

                    if reader.read_line(&mut line).unwrap() == 0 {
                        break;
                    }

                    let request: Value = serde_json::from_str(&line).unwrap();
                    let response = match request["method"].as_str().unwrap() {
                        "server.version" => json!(["fixture", "1.4"]),
                        "blockchain.scripthash.listunspent" => json!([]),
                        "blockchain.scripthash.get_history" => {
                            json!([{"tx_hash":transaction.compute_txid().to_string(),"height":height}])
                        }
                        "blockchain.transaction.get" => {
                            json!(bitcoin::consensus::encode::serialize_hex(&transaction))
                        }
                        other => panic!("unexpected Electrum method {other}"),
                    };
                    writeln!(stream, "{}", json!({"id":request["id"],"result":response})).unwrap();
                }
            });
            let electrum = bdk_electrum::electrum_client::Client::new(&url).unwrap();
            assert_eq!(
                SpendCheck::default()
                    .is_spent_in_block(&electrum, outpoint, &bitcoin::ScriptBuf::new())
                    .unwrap(),
                height > 0
            );
            drop(electrum);
            server.join().unwrap();
        }
    }

    #[test]
    fn first_change_index_does_not_advance_during_bundling() {
        let fixture = Fixture::new(2, 1);
        let me = fixture.deployment().signers().unwrap()[0].clone();
        let mut wallet = crate::infrastructure::admin_wallet::wallet::load_watch_only_admin_wallet(
            &me.account_xpub().to_string(),
            Network::Regtest,
            Some(u32::from_le_bytes(me.fingerprint().to_bytes())),
            "86'/1'/73'",
        )
        .unwrap();
        assert_eq!(first_unused_change_index(&wallet), 0);
        wallet.reveal_next_address(bdk_wallet::KeychainKind::Internal);
        assert_eq!(first_unused_change_index(&wallet), 0);
        wallet.mark_used(bdk_wallet::KeychainKind::Internal, 0);
        assert_eq!(first_unused_change_index(&wallet), 1);
        check_admin_wallet_matches_signer(&wallet, &me, Network::Regtest).unwrap();
        assert!(check_admin_wallet_matches_signer(&wallet, &me, Network::Bitcoin).is_err());
    }
}
