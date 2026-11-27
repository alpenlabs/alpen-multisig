//! Coordinator lifecycle; Bitcoin validity and signature rules belong to payout-admin-core.
mod auth;
mod blockchain;
mod database;
mod handlers;
#[cfg(test)]
mod tests;
use crate::error::{AppError, Result};
use bitcoin::{psbt::Psbt, OutPoint};
use database::{Database, Record};
use payout_admin_core::{block_payouts_tx, wire::*, Deployment};
use std::{collections::HashMap, sync::Arc};

use std::time::{Duration, SystemTime};

const EXPIRY: Duration = Duration::from_secs(4 * 24 * 60 * 60);

fn deadline(time: SystemTime, length: Duration) -> Result<SystemTime> {
    let end = time
        .checked_add(length)
        .ok_or_else(|| bad("time out of range"))?;

    unix_seconds::moment(unix_seconds::seconds(end).map_err(internal)?).map_err(bad)
}

fn bad(error: impl std::fmt::Display) -> AppError {
    AppError::BadRequest(error.to_string())
}

fn internal(error: impl Into<anyhow::Error>) -> AppError {
    AppError::Internal(error.into())
}

fn check_report_size(report: &str) -> Result<()> {
    if MAX_REPORT_BYTES < report.len() {
        return Err(bad("Report exceeds 32 KiB"));
    }

    Ok(())
}

fn decode(text: &str) -> Result<Psbt> {
    text.parse().map_err(bad)
}

/// Signs admins in and, for one deployment, stores pending Admin Block Payouts transactions, collects
/// their signatures, broadcasts them at quorum and marks them expired or past.
///
/// It is the payout admin state shared by the coordinator routes and the background job that runs
/// every 30 seconds. Holds the verified ASM history, signer roster, storage and sign-in sessions.
pub struct Service {
    public_origin: String,
    submissions: tokio::sync::Mutex<()>,
    deployment: tokio::sync::watch::Sender<Arc<Deployment>>,
    database: Arc<dyn Database>,
    blockchain: Arc<dyn blockchain::Blockchain>,
    auth: tokio::sync::Mutex<auth::Sessions>,
    challenge_ttl: Duration,
    session_ttl: Duration,
}

impl Service {
    fn deployment(&self) -> Arc<Deployment> {
        self.deployment.borrow().clone()
    }

    fn view(&self, record: &Record) -> Result<(PendingBlockPayoutsView, Vec<OutPoint>)> {
        let analysis =
            block_payouts_tx::analyze(&self.deployment(), &decode(&record.psbt)?).map_err(bad)?;
        let outpoints = decode(&record.psbt)?
            .unsigned_tx
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect();

        Ok((
            PendingBlockPayoutsView {
                txid: analysis.block_payouts_txid(),
                psbt: record.psbt.clone(),
                report: record.report.clone(),
                expires_at: record.expires_at,
                claim_input_count: analysis.connector_inputs().len(),
                admins_signed: analysis.admins_signed(),
                added: 0,
                signatures_needed: analysis.signatures_needed(),
                signed_by: analysis
                    .signers()
                    .iter()
                    .map(|i| self.deployment().admin_keys()[*i])
                    .collect(),
                duplicate_outpoints: vec![],
                broadcast_error: record.broadcast_error.clone(),
            },
            outpoints,
        ))
    }

    async fn list_pending(&self) -> Result<Vec<PendingBlockPayoutsView>> {
        let mut views = Vec::new();
        for record in self.database.list().await? {
            if record.past.is_none() && SystemTime::now() < record.expires_at {
                match self.view(&record) {
                    Ok(view) => views.push(view),
                    Err(error) => {
                        tracing::warn!(txid = %record.txid, %error, "skipping incompatible pending transaction")
                    }
                }
            }
        }
        let mut count: HashMap<OutPoint, usize> = HashMap::new();
        for (_, outpoints) in &views {
            for outpoint in outpoints {
                *count.entry(*outpoint).or_default() += 1;
            }
        }
        let mut pending_views: Vec<PendingBlockPayoutsView> = views
            .into_iter()
            .map(|(mut view, outpoints)| {
                view.duplicate_outpoints = outpoints
                    .iter()
                    .filter(|outpoint| 1 < count[outpoint])
                    .copied()
                    .collect();

                view
            })
            .collect();
        pending_views.sort_by(|left, right| {
            left.expires_at
                .cmp(&right.expires_at)
                .then_with(|| left.txid.to_string().cmp(&right.txid.to_string()))
        });

        Ok(pending_views)
    }

    async fn get(&self, id: &str) -> Result<Record> {
        self.database
            .list()
            .await?
            .into_iter()
            .find(|record| record.txid == id)
            .ok_or(AppError::NotFound)
    }

    fn signed(&self, record: &mut Record, time: SystemTime) -> Result<()> {
        let analysis =
            block_payouts_tx::analyze(&self.deployment(), &decode(&record.psbt)?).map_err(bad)?;

        if record.first_signature_at.is_none()
            && analysis
                .connector_inputs()
                .iter()
                .any(|connector_input| !connector_input.signed_by().is_empty())
        {
            record.first_signature_at = Some(deadline(time, Duration::ZERO)?);
            record.expires_at = deadline(time, EXPIRY)?;
        }

        Ok(())
    }

    /// Stores the report as opaque text if the transaction has none; never replaces or
    /// verifies it. Signers check reports themselves.
    async fn attach_report(&self, id: &str, report: String, time: SystemTime) -> Result<()> {
        check_report_size(&report)?;
        for _ in 0..8 {
            let mut record = self.get(id).await?;

            if record.past.is_some() || record.expires_at <= time {
                return Err(AppError::NotFound);
            }

            if record.report.is_some() {
                return Err(AppError::Conflict("Report already attached".into()));
            }

            let version = record.version;
            record.report = Some(report.clone());
            record.version += 1;

            if self.database.save(&record, Some(version)).await? {
                return Ok(());
            }
        }

        Err(AppError::Internal(anyhow::anyhow!(
            "Could not attach report after concurrent updates"
        )))
    }

    async fn submit(
        &self,
        text: &str,
        report: Option<String>,
        time: SystemTime,
    ) -> Result<PendingBlockPayoutsView> {
        if let Some(text) = &report {
            check_report_size(text)?;
        }

        let incoming = block_payouts_tx::import_psbt(&self.deployment(), text).map_err(bad)?;
        let id = incoming.unsigned_tx.compute_txid().to_string();
        let submission = self.submissions.lock().await;
        for _ in 0..8 {
            let records = self.database.list().await?;
            let existing = records.iter().find(|record| record.txid == id).cloned();
            if existing.is_none() {
                let analysis =
                    block_payouts_tx::analyze(&self.deployment(), &incoming).map_err(bad)?;
                let fees: std::collections::HashSet<_> = analysis
                    .fee_inputs()
                    .iter()
                    .map(|input| input.outpoint())
                    .collect();

                for record in records {
                    let active = match record.past {
                        Some(past) => past.status == PastStatus::Unconfirmed,
                        None => time < record.expires_at,
                    };

                    if active
                        && decode(&record.psbt)?
                            .unsigned_tx
                            .input
                            .iter()
                            .any(|input| fees.contains(&input.previous_output))
                    {
                        return Err(AppError::Conflict(
                            "A fee coin is already used by another block payouts transaction"
                                .into(),
                        ));
                    }
                }
            }

            let expected = existing.as_ref().map(|record| record.version);
            let mut record = existing.unwrap_or(Record {
                txid: id.clone(),
                psbt: incoming.to_string(),
                report: None,
                expires_at: deadline(time, EXPIRY)?,
                first_signature_at: None,
                broadcast_error: None,
                past: None,
                version: 0,
            });

            if record.past.is_some() || record.expires_at <= time {
                return Err(AppError::Conflict("payout is past or expired".into()));
            }

            let mut psbt = decode(&record.psbt)?;

            if expected.is_none() {
                for input in &mut psbt.inputs {
                    input.tap_script_sigs.clear();
                    input.tap_key_sig = None;
                }
            }

            if record.report.is_none() {
                record.report.clone_from(&report);
            }

            let added =
                block_payouts_tx::merge(&self.deployment(), &mut psbt, &incoming).map_err(bad)?;
            record.psbt = psbt.to_string();
            record.version += 1;
            self.signed(&mut record, time)?;

            if self.database.save(&record, expected).await? {
                drop(submission);
                // The binding POST contract returns the accepted pending snapshot even if it immediately broadcasts.
                let mut view = self
                    .list_pending()
                    .await?
                    .into_iter()
                    .find(|view| view.txid.to_string() == record.txid)
                    .unwrap_or(self.view(&record)?.0);
                self.refresh(record, time).await?;

                view.added = added;

                return Ok(view);
            }
        }

        Err(AppError::Conflict("payout changed; retry".into()))
    }

    async fn paste(&self, id: &str, text: &str, time: SystemTime) -> Result<PasteSignaturesView> {
        for _ in 0..8 {
            let mut record = self.get(id).await?;

            if record.past.is_some() || record.expires_at <= time {
                return Err(AppError::NotFound);
            }

            let mut psbt = decode(&record.psbt)?;
            let outcome = block_payouts_tx::paste_signatures(&self.deployment(), &mut psbt, text);
            record.psbt = psbt.to_string();
            let version = record.version;
            record.version += 1;
            self.signed(&mut record, time)?;

            if self.database.save(&record, Some(version)).await? {
                self.refresh(record, time).await?;
                let pending = self
                    .list_pending()
                    .await?
                    .into_iter()
                    .find(|record| record.txid.to_string() == id);

                return Ok(PasteSignaturesView {
                    pending,
                    added: outcome.added().len(),
                    already_present: outcome.already_present(),
                    invalid: outcome.into_invalid(),
                });
            }
        }

        Err(AppError::Conflict("payout changed; retry".into()))
    }

    /// Reloads the N/N history from ASM. A failed reload keeps the previous configuration.
    async fn refresh_deployment(&self, url: &str, roster: &payout_admin_core::SignerRoster) {
        match fetch_deployment(url, roster).await {
            Ok(deployment) => {
                self.deployment.send_replace(Arc::new(deployment));
            }
            Err(error) => {
                tracing::warn!(%error, "ASM refresh failed; keeping previous payout configuration")
            }
        }
    }

    async fn refresh(&self, record: Record, time: SystemTime) -> Result<()> {
        self.refresh_cached(record, time, &mut HashMap::new()).await
    }

    async fn refresh_cached(
        &self,
        record: Record,
        time: SystemTime,
        histories: &mut HashMap<bitcoin::ScriptBuf, blockchain::History>,
    ) -> Result<()> {
        if let Err(error) = self.refresh_inner(record.clone(), time, histories).await {
            tracing::warn!(txid = %record.txid, %error, "payout refresh failed");

            if record.past.is_none() && record.expires_at <= time {
                return self.database.delete(&record).await;
            }
        }

        Ok(())
    }

    async fn refresh_inner(
        &self,
        mut record: Record,
        time: SystemTime,
        histories: &mut HashMap<bitcoin::ScriptBuf, blockchain::History>,
    ) -> Result<()> {
        let psbt = decode(&record.psbt)?;
        let version = record.version;
        let mut known = None;
        let mut spent = false;
        // Script history gives positive evidence of a confirmed conflicting spend. An absent
        // listunspent entry alone is insufficient: a mempool conflict must never expire a payout.
        let id = psbt.unsigned_tx.compute_txid();
        record.broadcast_error = None;
        for (input, input_metadata) in psbt.unsigned_tx.input.iter().zip(&psbt.inputs) {
            let script = input_metadata
                .witness_utxo
                .as_ref()
                .ok_or_else(|| bad("missing prevout"))?
                .script_pubkey
                .clone();

            if !histories.contains_key(&script) {
                histories.insert(
                    script.clone(),
                    self.blockchain.history(script.clone()).await?,
                );
            }

            for (transaction, height) in &histories[&script] {
                if transaction.compute_txid() == id {
                    let block_time = match height {
                        Some(height) => Some(self.blockchain.block_time(*height).await?),
                        None => None,
                    };
                    known = Some(PastBlockPayoutsView {
                        txid: transaction.compute_txid(),
                        raw_tx: bitcoin::consensus::encode::serialize_hex(&transaction),
                        status: if block_time.is_some() {
                            PastStatus::Confirmed
                        } else {
                            PastStatus::Unconfirmed
                        },
                        block_time,
                    });
                } else if height.is_some()
                    && transaction
                        .input
                        .iter()
                        .any(|other_input| other_input.previous_output == input.previous_output)
                {
                    spent = true;
                }
            }
        }

        if spent {
            return self.database.delete(&record).await;
        }

        if let Some(past) = known {
            record.past = Some(past);
        } else if let Some(past) = &mut record.past {
            past.status = PastStatus::Unconfirmed;
            past.block_time = None;
        } else if record.expires_at <= time {
            return self.database.delete(&record).await;
        } else if block_payouts_tx::analyze(&self.deployment(), &psbt)
            .map_err(bad)?
            .has_quorum()
        {
            let result = match block_payouts_tx::finalize(&self.deployment(), &psbt).map_err(bad) {
                Ok(transaction) => self
                    .blockchain
                    .broadcast(transaction.clone())
                    .await
                    .map(|()| transaction),
                Err(error) => Err(error),
            };
            match result {
                Ok(transaction) => {
                    record.past = Some(PastBlockPayoutsView {
                        txid: transaction.compute_txid(),
                        raw_tx: bitcoin::consensus::encode::serialize_hex(&transaction),
                        status: PastStatus::Unconfirmed,
                        block_time: None,
                    })
                }
                Err(error) => {
                    record.broadcast_error = Some(match error {
                        AppError::Internal(error) => error.to_string(),
                        other => other.to_string(),
                    })
                }
            }
        }

        record.version += 1;
        self.database.save(&record, Some(version)).await?;

        Ok(())
    }

    async fn tick(&self, time: SystemTime) -> Result<()> {
        let mut histories = HashMap::new();
        for record in self.database.list().await? {
            if let Err(error) = self.refresh_cached(record, time, &mut histories).await {
                tracing::warn!(error = %error, "payout refresh failed");
            }
        }

        Ok(())
    }
}

/// How often the coordinator tries ASM at startup before it refuses to start.
const STARTUP_ASM_ATTEMPTS: u32 = 5;

/// Pause between startup attempts to reach ASM.
const STARTUP_ASM_RETRY_DELAY: Duration = Duration::from_secs(2);

async fn fetch_deployment(
    url: &str,
    roster: &payout_admin_core::SignerRoster,
) -> anyhow::Result<Deployment> {
    use crate::infrastructure::asm_rpc::{decode_bridge_state, fetch_anchor_state};
    let anchor = fetch_anchor_state(url).await.map_err(anyhow::Error::msg)?;
    let bridge = decode_bridge_state(&anchor).map_err(anyhow::Error::msg)?;

    Ok(Deployment::load(roster, bridge.operators())?)
}

/// Starts the payout routes and refresh worker when bridge parameters are configured.
pub async fn router(
    config: &crate::config::Config,
    pool: Option<sqlx::PgPool>,
) -> anyhow::Result<axum::Router> {
    let Some(path) = &config.payout_signers_path else {
        return Ok(axum::Router::new());
    };
    let text = std::fs::read_to_string(path)
        .map_err(|error| anyhow::anyhow!("cannot read signers.toml at {path}: {error}"))?;
    let roster = payout_admin_core::SignerRoster::from_toml(&text)?;
    let url = config.strata_admin_state_rpc_url.clone();
    let mut attempt = 1;
    let deployment = loop {
        match fetch_deployment(&url, &roster).await {
            Ok(deployment) => break deployment,
            Err(error) if attempt < STARTUP_ASM_ATTEMPTS => {
                tracing::warn!(%error, attempt, "Cannot load payout configuration from ASM; retrying");
                tokio::time::sleep(STARTUP_ASM_RETRY_DELAY).await;
                attempt += 1;
            }
            Err(error) => {
                return Err(error.context("cannot load payout configuration from ASM"));
            }
        }
    };
    let database: Arc<dyn Database> = match pool {
        Some(pool) => Arc::new(database::Postgres(pool)),
        None => Arc::new(database::InMemory::default()),
    };
    let Some(public_url) = &config.payout_admin_public_url else {
        tracing::error!("PAYOUT_ADMIN_PUBLIC_URL is required; payout admin routes are disabled");
        return Ok(axum::Router::new());
    };
    let public_url = reqwest::Url::parse(public_url)?;
    anyhow::ensure!(
        matches!(public_url.scheme(), "https" | "http") && public_url.host().is_some(),
        "PAYOUT_ADMIN_PUBLIC_URL must be an HTTP(S) address"
    );
    let service = Arc::new(Service {
        public_origin: public_url.origin().ascii_serialization(),
        submissions: Default::default(),
        deployment: tokio::sync::watch::channel(Arc::new(deployment)).0,
        database,
        blockchain: Arc::new(blockchain::Electrum::new(&config.payout_electrum_url)),
        auth: Default::default(),
        challenge_ttl: Duration::from_millis(config.auth_challenge_ttl_ms),
        session_ttl: Duration::from_millis(config.auth_session_ttl_ms),
    });
    let worker = service.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;

            worker.refresh_deployment(&url, &roster).await;

            if let Err(error) = worker.tick(SystemTime::now()).await {
                tracing::warn!(error = %error, "payout worker failed");
            }
        }
    });

    Ok(handlers::router(service))
}
