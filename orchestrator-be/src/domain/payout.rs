//! Admin Block Payouts records kept by the coordinator.
//!
//! Stored apart from the ASM proposals: a payout transaction is a Bitcoin transaction the Payout
//! Administrators sign, never an ASM action. The PSBT (base64) is the record; everything shown
//! (inputs, signers, fee) is derived from it against the trusted `params.toml`.

use chrono::{DateTime, Utc};

/// A payout transaction collecting signatures.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct PendingPayout {
    /// Lowercase hex txid. Signatures are SIGHASH_DEFAULT witness data, so it never changes.
    pub txid: String,
    pub psbt: String,
    /// x-only key (hex) of the admin who submitted it first.
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub first_signature_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    /// The last failed broadcast, kept so the admins see it and can retry.
    pub broadcast_error: Option<String>,
    pub broadcast_error_at: Option<DateTime<Utc>>,
    /// Bumped on every write; writes check it so concurrent merges never lose a signature.
    pub version: i64,
}

/// A payout transaction that reached quorum and was broadcast.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct PastPayout {
    pub txid: String,
    /// Fully signed transaction (hex), for Rebroadcast and Copy.
    pub raw_tx: String,
    pub psbt: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub broadcast_at: DateTime<Utc>,
}
