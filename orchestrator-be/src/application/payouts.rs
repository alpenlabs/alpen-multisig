//! Admin Block Payouts coordination: storing pending payout transactions, merging signatures,
//! expiring them and moving broadcast ones to the past list.
//!
//! The coordinator only checks hygiene against the trusted `params.toml` (the transaction is the
//! bridge's admin block payout shape, signatures verify, the signer is a Payout Administrator).
//! The desktop verifies reports; the coordinator verifies broadcast acceptance against its node.
//! `payout-admin-core` holds the bitcoin rules; no `strata_*` crate is imported here (P-028).

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use bitcoin::{psbt::Psbt, secp256k1::XOnlyPublicKey, OutPoint};
use chrono::{DateTime, Duration, Utc};
use payout_admin_core::{payout_tx, Deployment};

use crate::domain::payout::{PastPayout, PendingPayout};
use crate::error::AppError;
use crate::infrastructure::bitcoin_rpc::BitcoinRpcClient;

/// Design: a pending payout transaction expires 4 days after its first signature.
pub(crate) const PAYOUT_EXPIRY_DAYS: i64 = 4;
const MAX_PSBT_LEN: usize = 4 * 1024 * 1024;
const MAX_ERROR_LEN: usize = 1_000;
const MAX_WRITE_RETRIES: usize = 8;

/// Called while the repository holds its lifecycle lock, never as a preflight check.
pub(crate) fn can_save_pending(past: bool, expired: bool) -> Result<bool, AppError> {
    if past {
        return Ok(false);
    }
    if expired {
        return Err(AppError::Conflict(
            "this payout expired; create and sign a new transaction".to_string(),
        ));
    }
    Ok(true)
}

#[async_trait]
pub(crate) trait PayoutRepository: Send + Sync {
    async fn list_pending(&self) -> Result<Vec<PendingPayout>, AppError>;
    async fn get_pending(&self, txid: &str) -> Result<Option<PendingPayout>, AppError>;
    /// Inserts when `expected_version` is `None`, else updates only if the stored version still
    /// matches. Returns `false` when another write got there first.
    async fn save_pending(
        &self,
        record: &PendingPayout,
        expected_version: Option<i64>,
    ) -> Result<bool, AppError>;
    async fn delete_pending(&self, observed: &PendingPayout) -> Result<bool, AppError>;
    /// Removes the pending record and stores the past one, atomically.
    async fn move_to_past(&self, past: &PastPayout) -> Result<(), AppError>;
    async fn list_past(&self) -> Result<Vec<PastPayout>, AppError>;
    /// Ids of the payouts that expired: they cannot be submitted again.
    async fn list_expired(&self) -> Result<Vec<String>, AppError>;
    async fn get_past(&self, txid: &str) -> Result<Option<PastPayout>, AppError>;
}

pub use payout_admin_core::wire::{
    PastPayoutView, PasteSignaturesView, PayoutConfigView, PendingPayoutView, RefreshOutcome,
};

pub(crate) struct PayoutService {
    pub deployment: Arc<Deployment>,
    repo: Arc<dyn PayoutRepository>,
    btc: Arc<dyn BitcoinRpcClient>,
    expiry: Duration,
}

fn bad(m: impl std::fmt::Display) -> AppError {
    AppError::BadRequest(m.to_string())
}

pub(crate) fn decode_psbt(b64: &str) -> Result<Psbt, AppError> {
    if b64.len() > MAX_PSBT_LEN {
        return Err(bad("the PSBT is too large"));
    }
    Psbt::from_str(b64.trim()).map_err(|e| bad(format!("invalid PSBT: {e}")))
}

fn normalize_txid(txid: &str) -> Result<String, AppError> {
    bitcoin::Txid::from_str(txid)
        .map(|t| t.to_string())
        .map_err(|_| AppError::NotFound)
}

impl PayoutService {
    pub(crate) fn new(
        deployment: Arc<Deployment>,
        repo: Arc<dyn PayoutRepository>,
        btc: Arc<dyn BitcoinRpcClient>,
    ) -> Self {
        Self {
            deployment,
            repo,
            btc,
            expiry: Duration::days(PAYOUT_EXPIRY_DAYS),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_expiry(mut self, expiry: Duration) -> Self {
        self.expiry = expiry;
        self
    }

    /// Whether a compressed public key (hex) is one of the Payout Administrators in params.toml.
    pub(crate) fn is_admin_pubkey(&self, pubkey_hex: &str) -> bool {
        bitcoin::PublicKey::from_str(pubkey_hex)
            .ok()
            .map(|pk| XOnlyPublicKey::from(pk.inner))
            .is_some_and(|x| self.deployment.admin_index(&x).is_some())
    }

    pub(crate) fn config(&self) -> PayoutConfigView {
        let d = &self.deployment;
        PayoutConfigView {
            deployment_fingerprint: d.fingerprint(),
            expiry_days: self.expiry.num_days(),
        }
    }

    fn view(&self, rec: &PendingPayout) -> Result<(PendingPayoutView, Vec<OutPoint>), AppError> {
        let psbt = decode_psbt(&rec.psbt)?;
        let a = payout_tx::analyze(&self.deployment, &psbt).map_err(bad)?;
        let outpoints = a.outpoints();
        Ok((
            PendingPayoutView {
                txid: rec.txid.clone(),
                psbt: rec.psbt.clone(),
                created_by: rec.created_by.clone(),
                created_at: rec.created_at.timestamp_millis(),
                expires_at: rec.expires_at.timestamp_millis(),
                broadcast_error: rec.broadcast_error.clone(),
                signatures_received: a.signatures_received,
                signatures_needed: a.signatures_needed,
                duplicate_outpoints: vec![],
            },
            outpoints,
        ))
    }

    /// Live pending transactions (expired ones are removed on the way), with duplicate inputs
    /// marked.
    pub(crate) async fn list_pending(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<PendingPayoutView>, AppError> {
        let mut views = Vec::new();
        for snapshot in self.repo.list_pending().await? {
            let rec = match self.live_pending(&snapshot.txid, now).await {
                Ok(Some(rec)) => rec,
                Ok(None) => continue,
                // The node could not say whether this expired payout was completed elsewhere, so
                // it is kept. That must not hide every other payout: it stays in the list, and
                // signing it is still refused by the same check.
                Err(e) => {
                    tracing::warn!(txid = %snapshot.txid, error = %e, "expired payout could not be reconciled");
                    snapshot
                }
            };
            match self.view(&rec) {
                Ok(v) => views.push(v),
                Err(e) => tracing::warn!(txid = %rec.txid, error = %e, "unreadable pending payout"),
            }
        }
        let mut count: HashMap<OutPoint, usize> = HashMap::new();
        for (_, ops) in &views {
            for o in ops {
                *count.entry(*o).or_default() += 1;
            }
        }
        let mut out: Vec<PendingPayoutView> = views
            .into_iter()
            .map(|(mut v, ops)| {
                v.duplicate_outpoints = ops
                    .iter()
                    .filter(|o| count[o] > 1)
                    .map(ToString::to_string)
                    .collect();
                v
            })
            .collect();
        out.sort_by(|a, b| a.expires_at.cmp(&b.expires_at).then(a.txid.cmp(&b.txid)));
        Ok(out)
    }

    async fn live_pending(
        &self,
        txid: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<PendingPayout>, AppError> {
        for _ in 0..MAX_WRITE_RETRIES {
            match self.repo.get_pending(txid).await? {
                Some(rec) if rec.expires_at <= now => {
                    if self.record_known_payout(&rec, now).await? {
                        return Ok(None);
                    }
                    if self.repo.delete_pending(&rec).await? {
                        return Ok(None);
                    }
                }
                other => return Ok(other),
            }
        }
        Err(AppError::Conflict(
            "payout changed during expiry; try again".to_string(),
        ))
    }

    pub(crate) async fn get_pending(
        &self,
        txid: &str,
        now: DateTime<Utc>,
    ) -> Result<PendingPayoutView, AppError> {
        let txid = normalize_txid(txid)?;
        let rec = self
            .live_pending(&txid, now)
            .await?
            .ok_or(AppError::NotFound)?;
        self.view_with_duplicates(&rec, now).await
    }

    async fn view_with_duplicates(
        &self,
        rec: &PendingPayout,
        now: DateTime<Utc>,
    ) -> Result<PendingPayoutView, AppError> {
        self.list_pending(now)
            .await?
            .into_iter()
            .find(|v| v.txid == rec.txid)
            .map_or_else(|| self.view(rec).map(|(v, _)| v), Ok)
    }

    /// Creates a pending transaction, or merges the signatures of one already stored.
    pub(crate) async fn submit(
        &self,
        psbt_b64: &str,
        submitted_by: &str,
        now: DateTime<Utc>,
    ) -> Result<PendingPayoutView, AppError> {
        let mut incoming = decode_psbt(psbt_b64)?;
        let a = payout_tx::normalize(&self.deployment, &mut incoming).map_err(bad)?;
        let txid = a.txid.to_string();
        if self.repo.get_past(&txid).await?.is_some() {
            return Err(AppError::Conflict(
                "this payout transaction was already broadcast".to_string(),
            ));
        }
        for _ in 0..MAX_WRITE_RETRIES {
            let existing = self.live_pending(&txid, now).await?;
            let (rec, expected) = match existing {
                Some(mut rec) => {
                    let mut stored = decode_psbt(&rec.psbt)?;
                    payout_tx::merge(&self.deployment, &mut stored, &incoming).map_err(bad)?;
                    rec.psbt = stored.to_string();
                    let expected = rec.version;
                    rec.version += 1;
                    (rec, Some(expected))
                }
                None => (
                    PendingPayout {
                        txid: txid.clone(),
                        psbt: incoming.to_string(),
                        created_by: submitted_by.to_string(),
                        created_at: now,
                        first_signature_at: None,
                        expires_at: now + self.expiry,
                        broadcast_error: None,
                        broadcast_error_at: None,
                        version: 0,
                    },
                    None,
                ),
            };
            let rec = self.with_expiry_from_signatures(rec, now)?;
            if self.repo.save_pending(&rec, expected).await? {
                return self.view_with_duplicates(&rec, now).await;
            }
        }
        Err(AppError::Conflict(
            "the transaction is being updated by another admin; try again".to_string(),
        ))
    }

    /// Starts the expiry clock at the first signature (or at creation when it has none yet).
    fn with_expiry_from_signatures(
        &self,
        mut rec: PendingPayout,
        now: DateTime<Utc>,
    ) -> Result<PendingPayout, AppError> {
        if rec.first_signature_at.is_none() {
            let a = payout_tx::analyze(&self.deployment, &decode_psbt(&rec.psbt)?).map_err(bad)?;
            let any_sig = a.connector_inputs.iter().any(|c| !c.signed_by.is_empty());
            if any_sig {
                rec.first_signature_at = Some(now);
                rec.expires_at = now + self.expiry;
            }
        }
        Ok(rec)
    }

    pub(crate) async fn paste_signatures(
        &self,
        txid: &str,
        text: &str,
        now: DateTime<Utc>,
    ) -> Result<PasteSignaturesView, AppError> {
        let txid = normalize_txid(txid)?;
        for _ in 0..MAX_WRITE_RETRIES {
            let mut rec = self
                .live_pending(&txid, now)
                .await?
                .ok_or(AppError::NotFound)?;
            let mut psbt = decode_psbt(&rec.psbt)?;
            let outcome = payout_tx::paste_signatures(&self.deployment, &mut psbt, text);
            let expected = rec.version;
            if !outcome.added.is_empty() {
                rec.psbt = psbt.to_string();
                rec.version += 1;
                rec = self.with_expiry_from_signatures(rec, now)?;
                if !self.repo.save_pending(&rec, Some(expected)).await? {
                    continue;
                }
            }
            return Ok(PasteSignaturesView {
                pending: self.view_with_duplicates(&rec, now).await?,
                added: outcome.added.len(),
                already_present: outcome.already_present,
                message: payout_tx::invalid_signatures_message(&outcome.invalid),
                invalid: outcome.invalid,
            });
        }
        Err(AppError::Conflict(
            "the transaction is being updated by another admin; try again".to_string(),
        ))
    }

    /// A desktop reports the outcome of broadcasting the fully signed transaction.
    pub(crate) async fn report_broadcast(
        &self,
        txid: &str,
        error: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<RefreshOutcome, AppError> {
        let txid = normalize_txid(txid)?;
        if self.repo.get_past(&txid).await?.is_some() {
            return Ok(RefreshOutcome::Broadcast);
        }
        let mut rec = self
            .live_pending(&txid, now)
            .await?
            .ok_or(AppError::NotFound)?;
        let psbt = decode_psbt(&rec.psbt)?;
        let tx = payout_tx::finalize(&self.deployment, &psbt)
            .map_err(|e| AppError::Conflict(e.to_string()))?;
        match error {
            None => {
                self.btc.accept_payout(&tx).await?;
                self.move_to_past(&rec, &tx, now).await?;
                Ok(RefreshOutcome::Broadcast)
            }
            Some(mut e) => {
                let mut end = e.len().min(MAX_ERROR_LEN);
                while !e.is_char_boundary(end) {
                    end -= 1;
                }
                e.truncate(end);
                let expected = rec.version;
                rec.version += 1;
                rec.broadcast_error = Some(e);
                rec.broadcast_error_at = Some(now);
                // Losing this race only loses an error message another write already replaced.
                self.repo.save_pending(&rec, Some(expected)).await?;
                Ok(RefreshOutcome::Unchanged)
            }
        }
    }

    async fn move_to_past(
        &self,
        rec: &PendingPayout,
        tx: &bitcoin::Transaction,
        now: DateTime<Utc>,
    ) -> Result<(), AppError> {
        self.repo
            .move_to_past(&PastPayout {
                txid: rec.txid.clone(),
                raw_tx: bitcoin::consensus::encode::serialize_hex(tx),
                psbt: rec.psbt.clone(),
                created_by: rec.created_by.clone(),
                created_at: rec.created_at,
                broadcast_at: now,
            })
            .await
    }

    async fn record_known_payout(
        &self,
        rec: &PendingPayout,
        now: DateTime<Utc>,
    ) -> Result<bool, AppError> {
        let Some(tx) = self.btc.payout_transaction(&rec.txid).await? else {
            return Ok(false);
        };
        let psbt = decode_psbt(&rec.psbt)?;
        let mut unsigned = tx.clone();
        for input in &mut unsigned.input {
            input.witness = bitcoin::Witness::new();
        }
        if tx.compute_txid().to_string() != rec.txid || unsigned != psbt.unsigned_tx {
            return Err(bad("node returned a different payout transaction"));
        }
        self.move_to_past(rec, &tx, now).await?;
        Ok(true)
    }

    /// Asks the coordinator's own node whether the transaction was broadcast (moves it to the
    /// past list) or one of its inputs was spent by another confirmed transaction (removes it).
    pub(crate) async fn refresh(
        &self,
        txid: &str,
        now: DateTime<Utc>,
    ) -> Result<RefreshOutcome, AppError> {
        let txid = normalize_txid(txid)?;
        if self.repo.get_past(&txid).await?.is_some() {
            return Ok(RefreshOutcome::Broadcast);
        }
        let rec = self
            .repo
            .get_pending(&txid)
            .await?
            .ok_or(AppError::NotFound)?;
        if self.record_known_payout(&rec, now).await? {
            return Ok(RefreshOutcome::Broadcast);
        }
        let rec = self
            .live_pending(&txid, now)
            .await?
            .ok_or(AppError::NotFound)?;
        let psbt = decode_psbt(&rec.psbt)?;
        for input in &psbt.unsigned_tx.input {
            let o = input.previous_output;
            if !self
                .btc
                .tx_out_unspent(&o.txid.to_string(), o.vout, false)
                .await?
            {
                // It may have confirmed between the first lookup and the output lookup.
                if self.record_known_payout(&rec, now).await? {
                    return Ok(RefreshOutcome::Broadcast);
                }
                // Spent in a block by something else (the transaction itself is not known), or
                // never existed: either way it can never confirm.
                if !self.repo.delete_pending(&rec).await? {
                    return Err(AppError::Conflict(
                        "payout changed during refresh; try again".to_string(),
                    ));
                }
                tracing::info!(%txid, spent = %o, "pending payout transaction removed: input spent");
                return Ok(RefreshOutcome::Expired {
                    spent_outpoint: o.to_string(),
                });
            }
        }
        Ok(RefreshOutcome::Unchanged)
    }

    /// Ids of expired payouts, sorted. The app avoids building one of these again: the same
    /// claims, fee coin and change address give the same transaction, which would be refused.
    pub(crate) async fn list_expired(&self) -> Result<Vec<String>, AppError> {
        let mut txids = self.repo.list_expired().await?;
        txids.sort();
        Ok(txids)
    }

    pub(crate) async fn list_past(&self) -> Result<Vec<PastPayoutView>, AppError> {
        let mut out: Vec<PastPayoutView> = self
            .repo
            .list_past()
            .await?
            .into_iter()
            .map(|p| {
                let outpoints = decode_psbt(&p.psbt)
                    .ok()
                    .and_then(|psbt| payout_tx::analyze(&self.deployment, &psbt).ok())
                    .map(|a| a.outpoints())
                    .unwrap_or_default();
                PastPayoutView {
                    txid: p.txid,
                    raw_tx: p.raw_tx,
                    psbt: p.psbt,
                    broadcast_at: p.broadcast_at.timestamp_millis(),
                    connector_count: outpoints.len(),
                }
            })
            .collect();
        out.sort_by_key(|p| std::cmp::Reverse(p.broadcast_at));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::payout_repo::InMemoryPayoutRepository;

    #[test]
    fn terminal_states_take_precedence_over_pending_writes() {
        assert!(can_save_pending(false, false).unwrap());
        assert!(!can_save_pending(true, false).unwrap());
        assert!(!can_save_pending(true, true).unwrap());
        assert!(
            matches!(can_save_pending(false, true), Err(AppError::Conflict(message))
            if message == "this payout expired; create and sign a new transaction")
        );
    }

    fn record() -> PendingPayout {
        let now = Utc::now();
        PendingPayout {
            txid: "test".to_string(),
            psbt: String::new(),
            created_by: String::new(),
            created_at: now,
            first_signature_at: None,
            expires_at: now + Duration::days(4),
            broadcast_error: None,
            broadcast_error_at: None,
            version: 0,
        }
    }

    #[tokio::test]
    async fn stale_expiry_cannot_delete_extended_record() {
        let repo = InMemoryPayoutRepository::new();
        let mut rec = record();
        rec.expires_at = Utc::now();
        assert!(repo.save_pending(&rec, None).await.unwrap());
        let snapshot = repo.get_pending(&rec.txid).await.unwrap().unwrap();
        rec.version += 1;
        rec.first_signature_at = Some(rec.expires_at - Duration::seconds(1));
        rec.expires_at += Duration::days(4);
        assert!(repo
            .save_pending(&rec, Some(snapshot.version))
            .await
            .unwrap());
        assert!(!repo.delete_pending(&snapshot).await.unwrap());
        assert_eq!(repo.get_pending(&rec.txid).await.unwrap(), Some(rec));
    }

    #[tokio::test]
    async fn stale_submission_cannot_recreate_past() {
        let repo = InMemoryPayoutRepository::new();
        let mut rec = record();
        assert!(repo.save_pending(&rec, None).await.unwrap());
        let snapshot = repo.get_pending(&rec.txid).await.unwrap().unwrap();
        repo.move_to_past(&PastPayout {
            txid: rec.txid.clone(),
            raw_tx: String::new(),
            psbt: rec.psbt.clone(),
            created_by: rec.created_by.clone(),
            created_at: rec.created_at,
            broadcast_at: Utc::now(),
        })
        .await
        .unwrap();
        rec.version += 1;
        assert!(!repo
            .save_pending(&rec, Some(snapshot.version))
            .await
            .unwrap());
        assert!(!repo.save_pending(&rec, None).await.unwrap());
        assert!(repo.get_pending(&rec.txid).await.unwrap().is_none());
        assert!(repo.get_past(&rec.txid).await.unwrap().is_some());
    }
}
