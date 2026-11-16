//! Payout Administrator deployment and signer roster.

pub mod deployment;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use deployment::{AdminSigner, Covenant, Deployment, DeploymentError, SignerRoster};

pub mod wire;
