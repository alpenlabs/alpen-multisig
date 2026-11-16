use super::{DerivationPath, Fingerprint, Serialize, SignatureResult};
use crate::infrastructure::hw_wallet::ledger::payout as device_api;
pub trait PayoutDevice: Send + Sync {
    /// The open app's name and version, e.g. `("Bitcoin Test", "2.4.6")`.
    fn app(&self) -> Result<(String, String), String>;
    /// Master fingerprint (`u32::from_le_bytes`) and account xpub at `account`.
    fn identity(&self, account: &DerivationPath) -> Result<(u32, String), String>;
    fn sign_message(
        &self,
        message: &str,
        path: &DerivationPath,
        fingerprint: u32,
    ) -> Result<SignatureResult, String>;
}

/// The Ledger (HID, or Speculos with `LEDGER_SPECULOS_URL`).
pub struct LedgerPayoutDevice;

impl PayoutDevice for LedgerPayoutDevice {
    fn app(&self) -> Result<(String, String), String> {
        device_api::app()
    }
    fn identity(&self, account: &DerivationPath) -> Result<(u32, String), String> {
        device_api::identity(account)
    }
    fn sign_message(
        &self,
        message: &str,
        path: &DerivationPath,
        fingerprint: u32,
    ) -> Result<SignatureResult, String> {
        device_api::sign_message(message, path, fingerprint)
    }
}

pub fn fingerprint_u32(fp: Fingerprint) -> u32 {
    u32::from_le_bytes(fp.to_bytes())
}
pub(super) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("device task failed: {e}"))?
}
/// One line the device shows. `sat` is set for amounts (the screen formats them like the device).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailDto {
    /// The label as the device shows it.
    pub label: String,
    pub value: String,
    pub sat: Option<u64>,
    /// Context only the app can add (who a key belongs to); not on the device.
    pub note: Option<String>,
}

/// One interaction with the device, in the order it happens.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerStepDto {
    pub title: String,
    /// A device warning that is expected at this step, with why it is safe to continue.
    pub warning: Option<String>,
    pub details: Vec<DetailDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerReviewDto {
    pub steps: Vec<LedgerStepDto>,
    /// Set when the Ledger's app is newer than the versions whose screens were checked.
    pub note: Option<String>,
    /// Set when a transaction has so many claims that the Ledger may lock while it works.
    pub lock_warning: Option<String>,
    /// The unsigned ids of the transactions this checklist describes, in order (none for the
    /// sign-in). Confirm signs only if it builds exactly these.
    pub txids: Vec<String>,
}

pub(super) fn text(label: impl Into<String>, value: impl Into<String>) -> DetailDto {
    DetailDto {
        label: label.into(),
        value: value.into(),
        sat: None,
        note: None,
    }
}
