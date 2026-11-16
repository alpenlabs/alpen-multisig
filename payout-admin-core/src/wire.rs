//! Coordinator payout wire contract. All timestamps are epoch milliseconds.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PayoutConfigView {
    pub deployment_fingerprint: String,
    pub expiry_days: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PendingPayoutView {
    pub txid: String,
    pub psbt: String,
    pub created_by: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub broadcast_error: Option<String>,
    pub signatures_received: usize,
    pub signatures_needed: usize,
    pub duplicate_outpoints: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PastPayoutView {
    pub txid: String,
    pub raw_tx: String,
    pub psbt: String,
    pub broadcast_at: i64,
    pub connector_count: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PasteSignaturesView {
    pub pending: PendingPayoutView,
    pub added: usize,
    pub already_present: usize,
    pub invalid: Vec<crate::payout_tx::InvalidSignature>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RefreshOutcome {
    Unchanged,
    Broadcast,
    Expired { spent_outpoint: String },
}
