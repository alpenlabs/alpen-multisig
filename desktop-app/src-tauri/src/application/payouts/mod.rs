//! Admin Block Payouts for Payout Administrators: the flows behind the Block Payouts screen.
//!
//! Trust: `params.toml` (the bridge's own file) is the root. The signer roster only adds each
//! admin's account xpub and is checked against it (`xpub/0/0` must be the params key). Everything
//! acted on is re-derived locally: reports are verified against the admin's own Bitcoin Core node,
//! PSBTs from the coordinator are normalized (signing data rebuilt from the deployment, signatures
//! re-verified) before the device sees them.
//!
//! The device is behind [`PayoutDevice`] (the Ledger in the app, a software signer in tests).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use bitcoin::{
    bip32::{ChildNumber, DerivationPath, Fingerprint, Xpub},
    key::Secp256k1,
    psbt::Psbt,
    secp256k1::XOnlyPublicKey,
    Amount, OutPoint, ScriptBuf, TxOut, Txid,
};
use payout_admin_core::{
    deployment::account_path,
    fees,
    payout_tx::{self, ChangeOutput, FeeInput},
    planner::{self, PlanClaim, PlanCoin},
    policy::{self, PolicySpec},
    report::{self, ConnectorState, MinedTx, VerifiedReport},
    AdminSigner, Deployment,
};
use serde::{Deserialize, Serialize};

use crate::infrastructure::admin_wallet_reservations::{unix_now, AdminWalletReservations};
use crate::infrastructure::payout_coordinator::{
    CoordinatorOutcome, CoordinatorPast, CoordinatorPending, PayoutCoordinator,
};
use crate::infrastructure::payout_node::{branch_index_of, PayoutNode, SCAN_RANGE};
use crate::infrastructure::payout_policy_store::PolicyStore;
use crate::infrastructure::payout_used_addresses::{Floors, UsedAddresses};
use crate::infrastructure::signing::SignatureResult;
use payout_admin_core::wallet_send as wallet_tx;

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
    pub policies: PolicyStore,
    device: Arc<dyn PayoutDevice>,
    coins: CoinCache,
    reservations: AdminWalletReservations,
    used_addresses: UsedAddresses,
}

/// Admin Wallet coins from the last `scantxoutset` (a full UTXO-set scan; slow on mainnet), reused
/// for fee previews for [`COIN_CACHE_TTL`]. Confirm always rescans.
pub type CoinCache = Arc<tokio::sync::Mutex<Option<(std::time::Instant, usize, Vec<WalletCoin>)>>>;
pub const COIN_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

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
    pub coins: CoinCache,
    /// Coin reservations shared with the Admin Wallet panel.
    pub reservations: AdminWalletReservations,
    /// Addresses that held a coin once: not handed out again after the coin is spent.
    pub used_addresses: UsedAddresses,
}

pub const MSG_NOT_SIGNED_IN: &str = "Sign in as a Payout Administrator first.";

mod deployment;
pub use deployment::*;

mod sign_in;
pub use sign_in::*;

mod pending;
pub use pending::*;

mod reports;
pub use reports::*;

mod plan;
pub use plan::*;

mod wallet;
pub use wallet::*;

mod ledger;
pub use ledger::*;

mod wallet_send;
use wallet_send::*;

#[cfg(test)]
mod test_support;

#[cfg(test)]
use payout_admin_core::payout_tx::ConnectorRef;
