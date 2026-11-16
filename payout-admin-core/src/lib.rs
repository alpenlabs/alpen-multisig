//! Payout Administrator logic shared by the desktop app and the coordinator (`orchestrator-be`).
//!
//! The Payout Administrators hold the admin multisig of the strata-bridge: every claim transaction
//! has an *admin payout connector* (claim output 1) whose single tap leaf is
//! `multi_a(threshold, admin keys…)`. Spending that connector through the leaf is an
//! *Admin Block Payouts* transaction: it stops the operator's payout for a claim the admins have
//! proven false.
//!
//! This crate has no I/O. It covers:
//!
//! - [`deployment`]: the trusted strata-bridge `params.toml` (admin keys, threshold, operator
//!   schedule → N/N keys) and the separate signer roster with each admin's account xpub.
//! - [`report`]: false claim reports and their verification against mined transactions.
//! - [`payout_tx`]: building, inspecting, signing and finalizing Admin Block Payouts
//!   transactions (PSBT), the signature copy/paste format, and the exact size model.
//! - [`planner`]: bundling verified claims into as few transactions as the size limit allows and
//!   funding them from the Admin Wallet.
//! - [`policy`]: the Ledger (BIP-388) wallet policies the admins sign with.
//!
//! The connector itself is built with the bridge's own code (`strata-bridge-connectors`), so its
//! script, internal key and witness layout are the bridge's, byte for byte.

pub mod deployment;
pub mod fees;
pub mod payout_tx;
pub mod planner;
pub mod policy;
pub mod report;

/// Deterministic fixtures for this crate's tests and for the coordinator's and desktop's tests
/// (feature `test-support`). Never enable it in a release build: the keys are public.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use deployment::{AdminSigner, Covenant, Deployment, DeploymentError, SignerRoster};

pub mod wallet_send;

pub mod wire;
