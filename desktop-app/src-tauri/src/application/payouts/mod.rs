use crate::infrastructure::{payout_coordinator::PayoutCoordinator, signing::SignatureResult};
use bitcoin::{
    bip32::{DerivationPath, Fingerprint, Xpub},
    secp256k1::XOnlyPublicKey,
};
use payout_admin_core::{deployment::account_path, AdminSigner, Deployment};
use serde::Serialize;
use std::{str::FromStr, sync::Arc};
pub const MSG_NOT_A_PAYOUT_ADMIN: &str =
    "This Ledger is not one of the Payout Administrators in params.toml.";
pub const MSG_DEPLOYMENT_MISMATCH: &str =
    "The coordinator uses a different params.toml than this app. Check the bridge parameters \
     file in Node settings before signing.";

/// The orchestrator authority string for Payout Administrators.
pub const PAYOUT_AUTHORITY: &str = "payout_admin";

/// State behind the Block Payouts commands: the deployment loaded from params.toml, which admin
/// the connected Ledger is, and the coordinator session.
pub struct PayoutState {
    inner: tokio::sync::Mutex<SessionInner>,
    device: Arc<dyn PayoutDevice>,
}

#[derive(Default)]
struct SessionInner {
    deployment: Option<Arc<Deployment>>,
    me: Option<AdminSigner>,
    /// The Bitcoin app read at connect; picks the screens the checklist lists.
    app: Option<LedgerApp>,
    /// A sign-in challenge already shown in the checklist: `(challenge id, message)`.
    challenge: Option<(String, String)>,
    coordinator: Option<Arc<PayoutCoordinator>>,
}

/// Everything a signed-in flow needs.
pub type Ctx = PayoutContext;

pub struct PayoutContext {
    pub dep: Arc<Deployment>,
    pub me: AdminSigner,
    pub app: LedgerApp,
    pub coordinator: Arc<PayoutCoordinator>,
    pub device: Arc<dyn PayoutDevice>,
}

pub const MSG_NOT_SIGNED_IN: &str = "Sign in as a Payout Administrator first.";

mod deployment;
pub use deployment::*;

mod sign_in;
pub use sign_in::*;

mod ledger;
pub use ledger::*;
#[cfg(test)]
mod test_support;
