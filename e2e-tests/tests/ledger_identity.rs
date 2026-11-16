//! Reads the connected Ledger's Bitcoin app, master fingerprint and payout account xpub
//! (`m/86'/coin'/73'`), to build a deployment around a physical device. Nothing is signed and
//! nothing on the device changes. Runs only with `LEDGER_PHYSICAL=1`. Not a CI test.

use desktop_app::application::payouts::{LedgerPayoutDevice, PayoutDevice};
use payout_admin_core::deployment::account_path;

#[test]
fn ledger_identity() {
    if std::env::var("LEDGER_PHYSICAL").ok().as_deref() != Some("1") {
        eprintln!("skip: LEDGER_PHYSICAL=1 not set");
        return;
    }
    let network = match std::env::var("LEDGER_NETWORK").as_deref() {
        Ok("bitcoin") => bitcoin::Network::Bitcoin,
        _ => bitcoin::Network::Regtest,
    };
    let device = LedgerPayoutDevice;
    let (name, version) = device.app().expect("read the open app");
    let path = account_path(network);
    let (fp, xpub) = device.identity(&path).expect("read fingerprint and xpub");
    println!(
        "LEDGER_IDENTITY {}",
        serde_json::json!({
            "app": name,
            "version": version,
            "fingerprint": hex::encode(fp.to_le_bytes()),
            "accountPath": path.to_string(),
            "accountXpub": xpub,
        })
    );
}
