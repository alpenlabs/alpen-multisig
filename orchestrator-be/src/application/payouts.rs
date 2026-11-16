use bitcoin::secp256k1::XOnlyPublicKey;
use chrono::Duration;
pub use payout_admin_core::wire::PayoutConfigView;
use payout_admin_core::Deployment;
use std::{str::FromStr, sync::Arc};
pub(crate) const PAYOUT_EXPIRY_DAYS: i64 = 4;
pub(crate) struct PayoutService {
    pub deployment: Arc<Deployment>,
    expiry: Duration,
}
impl PayoutService {
    pub(crate) fn new(deployment: Arc<Deployment>) -> Self {
        Self {
            deployment,
            expiry: Duration::days(PAYOUT_EXPIRY_DAYS),
        }
    }
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
}
