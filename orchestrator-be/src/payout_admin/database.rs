//! Payout-only storage. Compare-and-swap prevents lost signatures across server instances.
use super::*;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One pending or past Admin Block Payouts transaction as stored by the coordinator.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Record {
    /// ID of the unsigned block payouts transaction. It stays the same as signatures are collected.
    pub(super) txid: String,
    /// Base64-encoded PSBT for the block payouts transaction, including every signature merged so far.
    pub(super) psbt: String,
    #[serde(skip)]
    pub(super) report: Option<String>,
    /// Expiry time for the pending record, in seconds since the Unix epoch. The four-day
    /// period starts at upload and restarts when the coordinator first sees an admin signature.
    #[serde(with = "unix_seconds", rename = "expires_at_unix_seconds")]
    pub(super) expires_at: SystemTime,
    /// Time the coordinator first saw an admin signature, in seconds since the Unix epoch.
    /// This is `None` until an admin signs a claim input.
    #[serde(
        default,
        with = "unix_seconds::optional",
        rename = "first_signature_at_unix_seconds"
    )]
    pub(super) first_signature_at: Option<SystemTime>,
    /// Last error from a broadcast attempt or chain check, including an Electrum outage.
    /// It is not cleared when a later check succeeds. The app displays it while the block payouts
    /// transaction is pending.
    pub(super) broadcast_error: Option<String>,
    /// Final transaction, confirmation status, and block time if known. Set after a successful
    /// broadcast or when seen on chain. This is `None` while pending.
    pub(super) past: Option<PastBlockPayoutsView>,
    /// Increases by one on every save. An update succeeds only if the stored version still
    /// matches the version read, so concurrent saves cannot overwrite each other's signatures.
    pub(super) version: i64,
}

#[async_trait]
pub(super) trait Database: Send + Sync {
    /// Reads the stored payout records, including both pending and past transactions.
    async fn list(&self) -> Result<Vec<Record>>;

    /// Writes only at the expected version, or inserts if absent when no version is supplied.
    async fn save(&self, record: &Record, expected: Option<i64>) -> Result<bool>;

    /// Removes the payout only if its stored version still matches the supplied record.
    async fn delete(&self, record: &Record) -> Result<()>;
}

/// Process-local payout records keyed by transaction ID, used when no database pool is supplied.
#[derive(Default)]
pub struct InMemory(tokio::sync::Mutex<HashMap<String, Record>>);

#[async_trait]
impl Database for InMemory {
    async fn list(&self) -> Result<Vec<Record>> {
        Ok(self.0.lock().await.values().cloned().collect())
    }

    async fn save(&self, record: &Record, expected: Option<i64>) -> Result<bool> {
        let mut rows = self.0.lock().await;

        if rows.get(&record.txid).map(|record| record.version) != expected {
            return Ok(false);
        }

        rows.insert(record.txid.clone(), record.clone());

        Ok(true)
    }

    async fn delete(&self, record: &Record) -> Result<()> {
        let mut rows = self.0.lock().await;

        if rows
            .get(&record.txid)
            .is_some_and(|stored_record| stored_record.version == record.version)
        {
            rows.remove(&record.txid);
        }

        Ok(())
    }
}

/// Stores versioned payout records in PostgreSQL so concurrent updates can detect a stale version.
pub struct Postgres(pub sqlx::PgPool);

#[async_trait]
impl Database for Postgres {
    async fn list(&self) -> Result<Vec<Record>> {
        let rows: Vec<(String, Option<Vec<u8>>)> =
            sqlx::query_as("SELECT body, report FROM payout_admin_records")
                .fetch_all(&self.0)
                .await
                .map_err(internal)?;

        rows.into_iter()
            .map(|(body, report)| {
                let mut record: Record = serde_json::from_str(&body).map_err(internal)?;
                record.report = report
                    .map(String::from_utf8)
                    .transpose()
                    .map_err(internal)?;

                Ok(record)
            })
            .collect()
    }

    async fn save(&self, record: &Record, expected: Option<i64>) -> Result<bool> {
        let body = serde_json::to_string(record).map_err(internal)?;
        let report = record.report.as_deref().map(str::as_bytes);
        let result = match expected {
            None => sqlx::query("INSERT INTO payout_admin_records (txid, version, body, report) VALUES ($1,$2,$3,$4) ON CONFLICT DO NOTHING")
                .bind(&record.txid).bind(record.version).bind(body).bind(report).execute(&self.0).await,
            Some(version) => sqlx::query("UPDATE payout_admin_records SET version=$2, body=$3, report=$5 WHERE txid=$1 AND version=$4")
                .bind(&record.txid).bind(record.version).bind(body).bind(version).bind(report).execute(&self.0).await,
        }.map_err(internal)?;

        Ok(result.rows_affected() == 1)
    }

    async fn delete(&self, record: &Record) -> Result<()> {
        sqlx::query("DELETE FROM payout_admin_records WHERE txid=$1 AND version=$2")
            .bind(&record.txid)
            .bind(record.version)
            .execute(&self.0)
            .await
            .map_err(internal)?;

        Ok(())
    }
}
