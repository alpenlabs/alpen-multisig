//! Payout transaction storage: in memory (local dev, tests) and Postgres.

use std::collections::{HashMap, HashSet};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use async_trait::async_trait;
use sqlx::PgPool;

use crate::application::payouts::{can_save_pending, PayoutRepository};
use crate::domain::payout::{PastPayout, PendingPayout};
use crate::error::AppError;

fn poisoned() -> AppError {
    AppError::Internal(anyhow::anyhow!("payout store lock poisoned"))
}

#[derive(Default)]
struct PayoutRecords {
    pending: HashMap<String, PendingPayout>,
    past: HashMap<String, PastPayout>,
    expired: HashSet<String>,
}

#[derive(Default)]
pub(crate) struct InMemoryPayoutRepository(RwLock<PayoutRecords>);

impl InMemoryPayoutRepository {
    pub(crate) fn new() -> Self {
        Self::default()
    }
    fn read(&self) -> Result<RwLockReadGuard<'_, PayoutRecords>, AppError> {
        self.0.read().map_err(|_| poisoned())
    }
    fn write(&self) -> Result<RwLockWriteGuard<'_, PayoutRecords>, AppError> {
        self.0.write().map_err(|_| poisoned())
    }
}

#[async_trait]
impl PayoutRepository for InMemoryPayoutRepository {
    async fn list_pending(&self) -> Result<Vec<PendingPayout>, AppError> {
        Ok(self.read()?.pending.values().cloned().collect())
    }
    async fn get_pending(&self, txid: &str) -> Result<Option<PendingPayout>, AppError> {
        Ok(self.read()?.pending.get(txid).cloned())
    }
    async fn save_pending(
        &self,
        record: &PendingPayout,
        expected_version: Option<i64>,
    ) -> Result<bool, AppError> {
        let mut records = self.write()?;
        if !can_save_pending(
            records.past.contains_key(&record.txid),
            records.expired.contains(&record.txid),
        )? {
            return Ok(false);
        }
        let matches = records.pending.get(&record.txid).map(|r| r.version) == expected_version;
        if matches {
            records.pending.insert(record.txid.clone(), record.clone());
        }
        Ok(matches)
    }
    async fn delete_pending(&self, observed: &PendingPayout) -> Result<bool, AppError> {
        let mut records = self.write()?;
        if !records
            .pending
            .get(&observed.txid)
            .is_some_and(|r| r.version == observed.version && r.expires_at == observed.expires_at)
        {
            return Ok(false);
        }
        records.expired.insert(observed.txid.clone());
        records.pending.remove(&observed.txid);
        Ok(true)
    }
    async fn move_to_past(&self, past: &PastPayout) -> Result<(), AppError> {
        // One lock covers every lifecycle collection, including terminal-state checks.
        let mut records = self.write()?;
        records.pending.remove(&past.txid);
        records
            .past
            .entry(past.txid.clone())
            .or_insert_with(|| past.clone());
        Ok(())
    }
    async fn list_past(&self) -> Result<Vec<PastPayout>, AppError> {
        Ok(self.read()?.past.values().cloned().collect())
    }
    async fn list_expired(&self) -> Result<Vec<String>, AppError> {
        Ok(self.read()?.expired.iter().cloned().collect())
    }
    async fn get_past(&self, txid: &str) -> Result<Option<PastPayout>, AppError> {
        Ok(self.read()?.past.get(txid).cloned())
    }
}

#[derive(Clone)]
pub(crate) struct PostgresPayoutRepository {
    pool: PgPool,
}

impl PostgresPayoutRepository {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn db(e: sqlx::Error) -> AppError {
    AppError::Internal(anyhow::anyhow!("payout store: {e}"))
}

// All lifecycle writes for a txid share this transaction-scoped lock.
async fn lock_payout(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    txid: &str,
) -> Result<(), AppError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(txid)
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    Ok(())
}

const PENDING_COLUMNS: &str =
    "txid, psbt, created_by, created_at, first_signature_at, expires_at, \
                               broadcast_error, broadcast_error_at, version";
const PAST_COLUMNS: &str = "txid, raw_tx, psbt, created_by, created_at, broadcast_at";

#[async_trait]
impl PayoutRepository for PostgresPayoutRepository {
    async fn list_pending(&self) -> Result<Vec<PendingPayout>, AppError> {
        sqlx::query_as(&format!("SELECT {PENDING_COLUMNS} FROM payout_pending"))
            .fetch_all(&self.pool)
            .await
            .map_err(db)
    }

    async fn get_pending(&self, txid: &str) -> Result<Option<PendingPayout>, AppError> {
        sqlx::query_as(&format!(
            "SELECT {PENDING_COLUMNS} FROM payout_pending WHERE txid = $1"
        ))
        .bind(txid)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn save_pending(
        &self,
        r: &PendingPayout,
        expected_version: Option<i64>,
    ) -> Result<bool, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        lock_payout(&mut tx, &r.txid).await?;
        let (past, expired) = sqlx::query_as(
            "SELECT EXISTS(SELECT 1 FROM payout_past WHERE txid = $1),
                    EXISTS(SELECT 1 FROM payout_expired WHERE txid = $1)",
        )
        .bind(&r.txid)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if !can_save_pending(past, expired)? {
            return Ok(false);
        }
        let result = match expected_version {
            None => sqlx::query(&format!(
                "INSERT INTO payout_pending ({PENDING_COLUMNS}) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (txid) DO NOTHING"
            ))
            .bind(&r.txid)
            .bind(&r.psbt)
            .bind(&r.created_by)
            .bind(r.created_at)
            .bind(r.first_signature_at)
            .bind(r.expires_at)
            .bind(&r.broadcast_error)
            .bind(r.broadcast_error_at)
            .bind(r.version)
            .execute(&mut *tx)
            .await
            .map_err(db)?,
            Some(v) => sqlx::query(
                "UPDATE payout_pending SET psbt = $2, first_signature_at = $3, expires_at = $4, \
                 broadcast_error = $5, broadcast_error_at = $6, version = $7 \
                 WHERE txid = $1 AND version = $8",
            )
            .bind(&r.txid)
            .bind(&r.psbt)
            .bind(r.first_signature_at)
            .bind(r.expires_at)
            .bind(&r.broadcast_error)
            .bind(r.broadcast_error_at)
            .bind(r.version)
            .bind(v)
            .execute(&mut *tx)
            .await
            .map_err(db)?,
        };
        tx.commit().await.map_err(db)?;
        Ok(result.rows_affected() == 1)
    }

    async fn delete_pending(&self, observed: &PendingPayout) -> Result<bool, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        lock_payout(&mut tx, &observed.txid).await?;
        let result = sqlx::query(
            "WITH removed AS (DELETE FROM payout_pending
             WHERE txid = $1 AND version = $2 AND expires_at = $3 RETURNING txid)
             INSERT INTO payout_expired (txid) SELECT txid FROM removed ON CONFLICT DO NOTHING",
        )
        .bind(&observed.txid)
        .bind(observed.version)
        .bind(observed.expires_at)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(result.rows_affected() == 1)
    }

    async fn move_to_past(&self, p: &PastPayout) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        lock_payout(&mut tx, &p.txid).await?;
        sqlx::query("DELETE FROM payout_pending WHERE txid = $1")
            .bind(&p.txid)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        sqlx::query(&format!(
            "INSERT INTO payout_past ({PAST_COLUMNS}) VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (txid) DO NOTHING"
        ))
        .bind(&p.txid)
        .bind(&p.raw_tx)
        .bind(&p.psbt)
        .bind(&p.created_by)
        .bind(p.created_at)
        .bind(p.broadcast_at)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)
    }

    async fn list_past(&self) -> Result<Vec<PastPayout>, AppError> {
        sqlx::query_as(&format!("SELECT {PAST_COLUMNS} FROM payout_past"))
            .fetch_all(&self.pool)
            .await
            .map_err(db)
    }

    async fn list_expired(&self) -> Result<Vec<String>, AppError> {
        sqlx::query_scalar("SELECT txid FROM payout_expired")
            .fetch_all(&self.pool)
            .await
            .map_err(db)
    }

    async fn get_past(&self, txid: &str) -> Result<Option<PastPayout>, AppError> {
        sqlx::query_as(&format!(
            "SELECT {PAST_COLUMNS} FROM payout_past WHERE txid = $1"
        ))
        .bind(txid)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }
}
