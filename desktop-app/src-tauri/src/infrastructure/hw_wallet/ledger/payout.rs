//! Ledger operations for Payout Administrators (Admin Block Payouts).
//!
//! - The admin multisig of each covenant is a registered BIP-388 policy
//!   `tr(@0/**,multi_a(k,@1/**,…))` (see `payout_admin_core::policy`). Registration shows the
//!   policy on the device once and returns an HMAC this app keeps; signing then needs no review of
//!   the policy again.
//! - Claim connector inputs are signed under that policy, fee inputs under the default Admin
//!   Wallet policy `tr(@0/**)`. Each `sign_psbt` signs only the inputs it recognises as its own and
//!   shows the rest as external.
//! - Sign-in signs the coordinator challenge with `sign_message` at the admin key path
//!   (account/0/0), the key `params.toml` lists.
//!
//! Every function here is blocking (it runs its own runtime): call it from `spawn_blocking`.

use std::str::FromStr;

use bitcoin::bip32::DerivationPath;
use bitcoin::psbt::Psbt;
use ledger_bitcoin_client::async_client::BitcoinClient;
use ledger_bitcoin_client::wallet::{Version, WalletPolicy, WalletPubKey};
use ledger_transport_hid::{hidapi::HidApi, TransportNativeHID};
use payout_admin_core::policy::PolicySpec;

use super::{
    apply_ledger_signatures, bitcoin_client_for_speculos, map_ledger_error, sign_with,
    with_ledger_device, with_speculos_auto_approve, HidTransport,
};
use crate::infrastructure::signing::SignatureResult;

/// Runs `$body` with `$client` bound to a Speculos or HID Ledger client, on its own runtime,
/// holding the device lock.
macro_rules! with_client {
    ($client:ident => $body:expr) => {{
        with_ledger_device(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            if let Ok(url) = std::env::var("LEDGER_SPECULOS_URL") {
                rt.block_on(async {
                    let $client = bitcoin_client_for_speculos(&url)
                        .await
                        .map_err(|e| e.to_string())?;
                    with_speculos_auto_approve(|| async { $body }).await
                })
            } else {
                let hidapi = HidApi::new().map_err(|e| format!("HidApi init failed: {e}"))?;
                let transport = TransportNativeHID::new(&hidapi)
                    .map_err(|e| format!("Ledger not found or locked: {e}"))?;
                let $client = BitcoinClient::new(HidTransport(transport));
                rt.block_on(async { $body })
            }
        })
    }};
}

fn to_ledger_policy(p: &PolicySpec) -> Result<WalletPolicy, String> {
    let keys = p
        .keys
        .iter()
        .map(|k| WalletPubKey::from_str(k).map_err(|e| format!("invalid policy key {k}: {e:?}")))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(WalletPolicy::new(
        p.name.clone(),
        Version::V2,
        p.template.clone(),
        keys,
    ))
}

/// BIP-388 policy id (sha256 of the serialized policy), as the device computes it.
pub fn policy_id(p: &PolicySpec) -> Result<[u8; 32], String> {
    Ok(to_ledger_policy(p)?.id())
}

/// Device master fingerprint, in the `u32` form the rest of the app uses
/// (`u32::from_le_bytes(fingerprint bytes)`).
async fn check_fingerprint<T>(client: &BitcoinClient<T>, expected: u32) -> Result<(), String>
where
    T: ledger_bitcoin_client::async_client::Transport,
    T::Error: std::fmt::Debug,
{
    let fp = client
        .get_master_fingerprint()
        .await
        .map_err(|e| map_ledger_error("get_master_fingerprint", &format!("{e:?}")))?;
    let actual = u32::from_le_bytes(fp.to_bytes());
    if actual != expected {
        return Err(format!(
            "This Ledger is not the Payout Administrator's: expected fingerprint {}, got {}",
            bitcoin::bip32::Fingerprint::from(expected.to_le_bytes()),
            bitcoin::bip32::Fingerprint::from(actual.to_le_bytes()),
        ));
    }
    Ok(())
}

/// Registers the policy on the device (the signer reviews every key there). Returns the HMAC.
/// Refuses when the device answers for a different policy than the one built here.
pub fn register_policy(p: &PolicySpec, expected_fingerprint: u32) -> Result<[u8; 32], String> {
    let policy = to_ledger_policy(p)?;
    let id = policy.id();
    with_client!(client => {
        check_fingerprint(&client, expected_fingerprint).await?;
        let (dev_id, hmac) = client
            .register_wallet(&policy)
            .await
            .map_err(|e| map_ledger_error("register_wallet", &format!("{e:?}")))?;
        if dev_id != id {
            return Err("The Ledger registered a different wallet policy than the one this app built.".to_string());
        }
        Ok(hmac)
    })
}

/// The policy's address at `index` (receive branch), optionally shown on the device.
pub fn policy_address(
    p: &PolicySpec,
    hmac: Option<[u8; 32]>,
    index: u32,
    display: bool,
) -> Result<String, String> {
    let policy = to_ledger_policy(p)?;
    with_client!(client => {
        client
            .get_wallet_address(&policy, hmac.as_ref(), false, index, display)
            .await
            .map(|a| a.assume_checked().to_string())
            .map_err(|e| map_ledger_error("get_wallet_address", &format!("{e:?}")))
    })
}

/// Signs every input of `psbt` the device recognises under policy `p` and adds the signatures.
/// Returns how many inputs were signed.
pub fn sign_psbt(
    psbt: &mut Psbt,
    p: &PolicySpec,
    hmac: Option<[u8; 32]>,
    expected_fingerprint: u32,
) -> Result<usize, String> {
    let policy = to_ledger_policy(p)?;
    with_client!(client => {
        check_fingerprint(&client, expected_fingerprint).await?;
        let yielded = client
            .sign_psbt(psbt, &policy, hmac.as_ref())
            .await
            .map_err(|e| map_ledger_error("sign_psbt", &format!("{e:?}")))?;
        let n = yielded.len();
        apply_ledger_signatures(psbt, yielded)?;
        Ok(n)
    })
}

/// Signs `message` (BIP-137) with the key at `path`, checking the device first.
pub fn sign_message(
    message: &str,
    path: &DerivationPath,
    expected_fingerprint: u32,
) -> Result<SignatureResult, String> {
    let path = path.to_string();
    with_client!(client => {
        check_fingerprint(&client, expected_fingerprint).await?;
        sign_with(&client, message, &path).await
    })
}

/// The open app's name and version.
pub fn app() -> Result<(String, String), String> {
    with_client!(client => {
        let (name, version, _flags) = client
            .get_version()
            .await
            .map_err(|e| map_ledger_error("get_version", &format!("{e:?}")))?;
        Ok((name, version))
    })
}

/// The device's master fingerprint and its account xpub at `account_path`.
pub fn identity(account_path: &DerivationPath) -> Result<(u32, String), String> {
    with_client!(client => {
        let fp = client
            .get_master_fingerprint()
            .await
            .map_err(|e| map_ledger_error("get_master_fingerprint", &format!("{e:?}")))?;
        let xpub = client
            .get_extended_pubkey(account_path, false)
            .await
            .map_err(|e| map_ledger_error("get_extended_pubkey", &format!("{e:?}")))?;
        Ok((u32::from_le_bytes(fp.to_bytes()), xpub.to_string()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use payout_admin_core::{policy, test_support::Fixture};

    #[test]
    fn policies_convert_for_the_device() {
        let fx = Fixture::new(3, 2);
        let dep = fx.deployment();
        let p = policy::admin_multisig_policy(&dep, &dep.covenants[0]).unwrap();
        let lp = to_ledger_policy(&p).unwrap();
        assert_eq!(lp.keys.len(), 4);
        // The synthetic key carries no origin; the admins carry theirs.
        assert!(lp.keys[0].source.is_none());
        assert!(lp.keys[1..].iter().all(|k| k.source.is_some()));
        assert_eq!(policy_id(&p).unwrap(), lp.id());
        let w = policy::admin_wallet_policy(&dep.signers().unwrap()[0], dep.network);
        assert_eq!(to_ledger_policy(&w).unwrap().keys.len(), 1);
    }
}
