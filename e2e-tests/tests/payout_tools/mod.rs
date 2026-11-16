//! Helpers for the opt-in payout tester tools; product tests do not include this module.
#![allow(dead_code)]
use crate::payout_common::*;
use bitcoin::bip32::ChildNumber;
use bitcoin::consensus::encode::{deserialize_hex, serialize_hex};
use bitcoin::hashes::Hash;
use bitcoin::key::{Keypair, TapTweak};
use bitcoin::secp256k1::{Message, Secp256k1};
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin::{
    absolute, transaction::Version, Address, Amount, Network, OutPoint, Sequence, TapSighashType,
    Transaction, TxIn, TxOut, Txid, Witness,
};
use desktop_app::infrastructure::bitcoin_rpc::HttpBitcoinRpcClient;
use payout_admin_core::test_support::Fixture;
use serde_json::{json, Value};
use std::str::FromStr;
// ─── A physical Ledger as admin 0 ────────────────────────────────────────────

/// The connected Ledger's master fingerprint and payout account xpub (regtest). Runs the device
/// call on its own thread: it starts a runtime, which an async test's thread cannot.
#[allow(dead_code)]
pub fn physical_ledger_identity() -> (bitcoin::bip32::Fingerprint, bitcoin::bip32::Xpub) {
    use desktop_app::application::payouts::{LedgerPayoutDevice, PayoutDevice as _};
    let (fp, xpub) = std::thread::spawn(|| {
        LedgerPayoutDevice.identity(&payout_admin_core::deployment::account_path(
            bitcoin::Network::Regtest,
        ))
    })
    .join()
    .unwrap()
    .expect("Ledger connected and unlocked, with the Bitcoin Test app open");
    (
        bitcoin::bip32::Fingerprint::from(fp.to_le_bytes()),
        bitcoin::bip32::Xpub::from_str(&xpub).unwrap(),
    )
}

/// `account/branch/index` of a device's account xpub, as a regtest address.
#[allow(dead_code)]
pub fn xpub_address(xpub: &bitcoin::bip32::Xpub, branch: u32, index: u32) -> Address {
    let secp = Secp256k1::new();
    let k = xpub
        .derive_pub(
            &secp,
            &[
                ChildNumber::Normal { index: branch },
                ChildNumber::Normal { index },
            ],
        )
        .unwrap();
    Address::p2tr(
        &secp,
        k.public_key.x_only_public_key().0,
        None,
        bitcoin::Network::Regtest,
    )
}

/// The fixture's params.toml and roster with admin 0 replaced by a device's public identity.
/// Admin 0 then signs only on that device; the fixture's own admin 0 key is unused.
#[allow(dead_code)]
pub fn with_device_as_admin0(
    fx: &Fixture,
    params_toml: &str,
    roster_toml: &str,
    fp: bitcoin::bip32::Fingerprint,
    xpub: &bitcoin::bip32::Xpub,
) -> (String, String) {
    // The admin key is account/0/0.
    let xonly = xpub
        .derive_pub(
            &Secp256k1::new(),
            &[
                ChildNumber::Normal { index: 0 },
                ChildNumber::Normal { index: 0 },
            ],
        )
        .unwrap()
        .public_key
        .x_only_public_key()
        .0;
    (
        params_toml.replace(&fx.admin_xonly(0).to_string(), &xonly.to_string()),
        roster_toml
            .replace(&fx.fingerprint(0).to_string(), &fp.to_string())
            .replace(&fx.account_xpub(0).to_string(), &xpub.to_string()),
    )
}

/// Funding for real games, optionally without a Bitcoin Core wallet.
pub struct GameFunding<'a> {
    pub node: &'a HttpBitcoinRpcClient,
    pub key: Option<&'a Keypair>,
}

impl GameFunding<'_> {
    pub async fn address(&self) -> String {
        match self.key {
            Some(key) => Address::p2tr(
                &Secp256k1::new(),
                key.x_only_public_key().0,
                None,
                Network::Regtest,
            )
            .to_string(),
            None => new_addr(self.node).await,
        }
    }

    pub async fn mine(&self, n: u64) -> Value {
        call(
            self.node,
            "generatetoaddress",
            json!([n, self.address().await]),
        )
        .await
    }

    pub async fn sign_send(&self, raw: &str) -> Transaction {
        let Some(key) = self.key else {
            return sign_send(self.node, raw).await;
        };
        let mut tx: Transaction = deserialize_hex(raw).unwrap();
        let mut prevouts = Vec::new();
        for input in &tx.input {
            let hex = call(
                self.node,
                "getrawtransaction",
                json!([input.previous_output.txid.to_string()]),
            )
            .await;
            let prev: Transaction = deserialize_hex(hex.as_str().unwrap()).unwrap();
            prevouts.push(prev.output[input.previous_output.vout as usize].clone());
        }
        let secp = Secp256k1::new();
        for i in 0..tx.input.len() {
            let hash = SighashCache::new(&tx)
                .taproot_key_spend_signature_hash(
                    i,
                    &Prevouts::All(&prevouts),
                    TapSighashType::Default,
                )
                .unwrap();
            let sig = secp.sign_schnorr(
                &Message::from_digest(hash.to_byte_array()),
                &key.tap_tweak(&secp, None).to_keypair(),
            );
            tx.input[i].witness = Witness::from_slice(&[sig.as_ref()]);
        }
        call(self.node, "sendrawtransaction", json!([serialize_hex(&tx)])).await;
        tx
    }

    /// Mine to a tool-held key and find spendable coins without a Core wallet.
    pub async fn fund(&self, mut outputs: Vec<TxOut>) -> Transaction {
        assert!(self.key.is_some());
        let needed = outputs.iter().map(|o| o.value.to_sat()).sum::<u64>() + 10_000;
        self.mine(1).await;
        let mut selected = None;
        for attempt in 0..2 {
            let scan = call(
                self.node,
                "scantxoutset",
                json!(["start", [format!("addr({})", self.address().await)]]),
            )
            .await;
            for coin in scan["unspents"].as_array().unwrap() {
                let amount = Amount::from_btc(coin["amount"].as_f64().unwrap())
                    .unwrap()
                    .to_sat();
                if amount < needed {
                    continue;
                }
                let utxo = call(
                    self.node,
                    "gettxout",
                    json!([coin["txid"], coin["vout"], true]),
                )
                .await;
                if utxo.is_null()
                    || (utxo["coinbase"] == true && utxo["confirmations"].as_u64().unwrap() < 100)
                {
                    continue;
                }
                selected = Some((coin.clone(), amount));
                break;
            }
            if selected.is_some() {
                break;
            }
            if attempt == 0 {
                self.mine(101).await;
            }
        }
        let (coin, amount) = selected
            .expect("tool mining coins cannot cover funding; regtest subsidy may be exhausted");
        outputs.push(TxOut {
            value: Amount::from_sat(amount - needed),
            script_pubkey: Address::from_str(&self.address().await)
                .unwrap()
                .assume_checked()
                .script_pubkey(),
        });
        let tx = Transaction {
            version: Version(2),
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::new(
                    Txid::from_str(coin["txid"].as_str().unwrap()).unwrap(),
                    coin["vout"].as_u64().unwrap() as u32,
                ),
                sequence: Sequence::MAX,
                ..Default::default()
            }],
            output: outputs,
        };
        self.sign_send(&serialize_hex(&tx)).await
    }

    async fn claim(&self, mut outputs: Vec<TxOut>) -> Transaction {
        if self.key.is_none() {
            let outputs = outputs.iter().map(|o| json!({Address::from_script(&o.script_pubkey, Network::Regtest).unwrap().to_string(): btc(o.value.to_sat())})).collect();
            return send_claim(self.node, outputs).await;
        }
        outputs.insert(
            2,
            TxOut {
                value: Amount::from_sat(10_000),
                script_pubkey: Address::from_str(&self.address().await)
                    .unwrap()
                    .assume_checked()
                    .script_pubkey(),
            },
        );
        self.fund(outputs).await
    }
}

impl Funding for GameFunding<'_> {
    fn node(&self) -> &HttpBitcoinRpcClient {
        self.node
    }
    async fn address(&self) -> String {
        self.address().await
    }
    async fn mine(&self, n: u64) {
        self.mine(n).await;
    }
    async fn sign_send(&self, raw: &str) -> Transaction {
        self.sign_send(raw).await
    }
    async fn claim(&self, outputs: Vec<TxOut>) -> Transaction {
        self.claim(outputs).await
    }
}

impl Coordinator {
    /// A coordinator this process did not start (left running on drop).
    pub fn external(base_url: &str) -> Self {
        Self {
            child: None,
            base_url: base_url.to_string(),
        }
    }
}
