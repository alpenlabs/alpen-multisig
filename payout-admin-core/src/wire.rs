//! Coordinator payout wire contract. All timestamps are epoch milliseconds.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PayoutConfigView {
    pub deployment_fingerprint: String,
    pub expiry_days: i64,
}
