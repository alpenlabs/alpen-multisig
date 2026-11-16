use super::{
    bitcoin_client_for_speculos, map_ledger_error, sign_with, with_ledger_device,
    with_speculos_auto_approve, HidTransport,
};
use crate::infrastructure::signing::SignatureResult;
use bitcoin::bip32::DerivationPath;
use ledger_bitcoin_client::async_client::BitcoinClient;
use ledger_transport_hid::{hidapi::HidApi, TransportNativeHID};
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
pub fn app() -> Result<(String, String), String> {
    with_client!(client => {
        let (name, version, _flags) = client
            .get_version()
            .await
            .map_err(|e| map_ledger_error("get_version", &format!("{e:?}")))?;
        Ok((name, version))
    })
}
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
