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
    pub policies: PolicyStore,
    coins: CoinCache,
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
    pub coins: CoinCache,
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

use crate::infrastructure::payout_node::PayoutNode;
use bitcoin::{OutPoint, Txid};
use payout_admin_core::report::{self, ConnectorState, MinedTx, VerifiedReport};
use std::collections::{BTreeSet, HashMap, HashSet};
mod reports;
pub use reports::*;

use crate::infrastructure::payout_node::branch_index_of;
use bitcoin::{bip32::ChildNumber, key::Secp256k1, ScriptBuf};
use payout_admin_core::policy;
use serde::Deserialize;
mod wallet;
pub use wallet::*;
mod plan;
pub use plan::*;
pub type CoinCache = Arc<tokio::sync::Mutex<Option<(std::time::Instant, usize, Vec<WalletCoin>)>>>;
pub const COIN_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

use crate::infrastructure::{
    payout_coordinator::{CoordinatorPast, CoordinatorPending},
    payout_node::SCAN_RANGE,
    payout_policy_store::PolicyStore,
};
use bitcoin::{psbt::Psbt, Amount, TxOut};
use payout_admin_core::{
    payout_tx::{self, ChangeOutput, FeeInput},
    planner::{self, PlanClaim},
    policy::PolicySpec,
    wallet_send as wallet_tx,
};
mod pending;
#[cfg(test)]
use payout_admin_core::payout_tx::ConnectorRef;
pub use pending::*;

use payout_admin_core::{fees, planner::PlanCoin};
