//! How the payout admin talks to the Ledger.
//!
//! This file handles Ledger policies with more than a single key.
//! It should be merged with `desktop-app/src-tauri/src/infrastructure/hw_wallet/ledger.rs`.
//! This involves re-testing the flows of all ASM admins on Speculos, which is why we defer this merge for now.

use bitcoin::bip32::{DerivationPath, Fingerprint, Xpub};
use bitcoin::psbt::Psbt;
use ledger_bitcoin_client::async_client::BitcoinClient;
use ledger_bitcoin_client::wallet::{Version, WalletPolicy, WalletPubKey};
use ledger_transport_hid::{hidapi::HidApi, TransportNativeHID};
use payout_admin_core::policy::LedgerPolicy;

use super::{
    apply_ledger_signatures, bitcoin_client_for_speculos, map_ledger_error, sign_with,
    with_ledger_device, with_speculos_auto_approve, HidTransport,
};
use crate::infrastructure::signing::SignatureResult;

macro_rules! with_client {
    ($client:ident => $body:expr) => {{
        with_ledger_device(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("tokio runtime: {error}"))?;

            if let Ok(url) = std::env::var("LEDGER_SPECULOS_URL") {
                runtime.block_on(async {
                    let $client = bitcoin_client_for_speculos(&url)
                        .await
                        .map_err(|error| error.to_string())?;

                    with_speculos_auto_approve(|| async { $body }).await
                })
            } else {
                let hidapi =
                    HidApi::new().map_err(|error| format!("HidApi init failed: {error}"))?;
                let transport = TransportNativeHID::new(&hidapi)
                    .map_err(|error| format!("Ledger not found or locked: {error}"))?;
                let $client = BitcoinClient::new(HidTransport(transport));

                runtime.block_on(async { $body })
            }
        })
    }};
}

/// Converts the descriptor and ordered keys to the Ledger client's version 2 policy format.
pub(crate) fn to_ledger_policy(policy: &LedgerPolicy) -> WalletPolicy {
    let keys = policy.keys().iter().map(|(source, xpub)| WalletPubKey {
        source: source.clone(),
        inner: *xpub,
        multipath: None,
    });

    WalletPolicy::new(
        policy.name().to_owned(),
        Version::V2,
        policy.template().to_owned(),
        keys,
    )
}

/// Computes the ID the Ledger should return when registering this policy.
pub fn policy_id(policy: &LedgerPolicy) -> [u8; 32] {
    to_ledger_policy(policy).id()
}

async fn check_fingerprint<T>(
    client: &BitcoinClient<T>,
    expected: Fingerprint,
) -> Result<(), String>
where
    T: ledger_bitcoin_client::async_client::Transport,
    T::Error: std::fmt::Debug,
{
    let fingerprint = client
        .get_master_fingerprint()
        .await
        .map_err(|error| map_ledger_error("get_master_fingerprint", &format!("{error:?}")))?;

    if fingerprint != expected {
        return Err(format!(
            "This Ledger is not the Payout Administrator's: expected fingerprint {expected}, got {fingerprint}"
        ));
    }

    Ok(())
}

/// Checks the device fingerprint and registered policy ID before returning the device's registration HMAC.
pub fn register_policy(
    policy: &LedgerPolicy,
    expected_fingerprint: Fingerprint,
) -> Result<[u8; 32], String> {
    let policy = to_ledger_policy(policy);
    let id = policy.id();

    with_client!(client => {
        check_fingerprint(&client, expected_fingerprint).await?;
        let (registered_policy_id, hmac) = client
            .register_wallet(&policy)
            .await
            .map_err(|error| map_ledger_error("register_wallet", &format!("{error:?}")))?;

        if registered_policy_id != id {
            return Err("The Ledger registered a different wallet policy than the one this app built.".to_string());
        }

        Ok(hmac)
    })
}

/// Requests a policy receive address at the given index, optionally displaying it on the device.
pub fn policy_address(
    policy: &LedgerPolicy,
    hmac: Option<[u8; 32]>,
    index: u32,
    display: bool,
) -> Result<bitcoin::Address, String> {
    let policy = to_ledger_policy(policy);

    with_client!(client => {
        client
            .get_wallet_address(&policy, hmac.as_ref(), false, index, display)
            .await
            .map(|address| address.assume_checked())
            .map_err(|error| map_ledger_error("get_wallet_address", &format!("{error:?}")))
    })
}

/// Checks the device fingerprint, applies its policy signatures to the PSBT, and returns their count.
pub fn sign_psbt(
    psbt: &mut Psbt,
    policy: &LedgerPolicy,
    hmac: Option<[u8; 32]>,
    expected_fingerprint: Fingerprint,
) -> Result<usize, String> {
    let policy = to_ledger_policy(policy);

    with_client!(client => {
        check_fingerprint(&client, expected_fingerprint).await?;
        let signatures = client
            .sign_psbt(psbt, &policy, hmac.as_ref())
            .await
            .map_err(|error| map_ledger_error("sign_psbt", &format!("{error:?}")))?;
        let signature_count = signatures.len();
        apply_ledger_signatures(psbt, signatures)?;

        Ok(signature_count)
    })
}

/// Checks the device fingerprint before requesting a Bitcoin message signature at the supplied path.
pub fn sign_message(
    message: &str,
    path: &DerivationPath,
    expected_fingerprint: Fingerprint,
) -> Result<SignatureResult, String> {
    let path = path.to_string();

    with_client!(client => {
        check_fingerprint(&client, expected_fingerprint).await?;

        sign_with(&client, message, &path).await
    })
}

/// The name and parsed version (major, minor, patch) of the app open on the Ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerApp {
    pub name: String,
    pub version: (u64, u64, u64),
}

/// Reads the name and version of the app currently open on the Ledger.
pub fn open_app() -> Result<LedgerApp, String> {
    with_client!(client => {
        let (name, version, _flags) = client
            .get_version()
            .await
            .map_err(|error| map_ledger_error("get_version", &format!("{error:?}")))?;
        let version = super::parse_app_version(&version).ok_or("Invalid Ledger app version")?;

        Ok(LedgerApp { name, version })
    })
}

/// Reads the master fingerprint and the account xpub at the supplied path.
pub fn read_identity(account_path: &DerivationPath) -> Result<(Fingerprint, Xpub), String> {
    with_client!(client => {
        let fingerprint = client
            .get_master_fingerprint()
            .await
            .map_err(|error| map_ledger_error("get_master_fingerprint", &format!("{error:?}")))?;
        let xpub = client
            .get_extended_pubkey(account_path, false)
            .await
            .map_err(|error| map_ledger_error("get_extended_pubkey", &format!("{error:?}")))?;

        Ok((fingerprint, xpub))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use payout_admin_core::{policy, test_support::Fixture};

    #[test]
    fn policies_convert_for_the_device() {
        let fixture = Fixture::new(3, 2);
        let deployment = fixture.deployment();
        let policy = policy::admin_multisig_policy(&deployment).unwrap();
        let wallet_policy = to_ledger_policy(&policy);
        // Key 0 is the N/N key, and keys 1 through 3 are the admins in deployment order.
        assert_eq!(wallet_policy.keys.len(), 4);
        assert!(wallet_policy.keys[0].source.is_none());
        assert!(wallet_policy.keys[1]
            .to_string()
            .starts_with(&format!("[{}/86'/1'/73']tpub", fixture.fingerprint(0))));
        assert!(wallet_policy.keys[1..]
            .iter()
            .all(|key| key.source.is_some()));
        assert_eq!(policy_id(&policy), wallet_policy.id());
        let wallet_policy =
            policy::admin_wallet_policy(&deployment.signers().unwrap()[0], deployment.network());
        assert_eq!(to_ledger_policy(&wallet_policy).keys.len(), 1);
    }
}
