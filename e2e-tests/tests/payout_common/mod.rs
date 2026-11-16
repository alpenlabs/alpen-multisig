//! Shared helpers for the Admin Block Payouts e2e tests.
#![allow(dead_code)]

use std::net::TcpListener;
use std::process::{Child, Command};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use bitcoin::bip32::{ChildNumber, DerivationPath};
use bitcoin::consensus::encode::{deserialize_hex, serialize_hex};
use bitcoin::hashes::Hash;
use bitcoin::key::{Keypair, TapTweak};
use bitcoin::opcodes::all::{OP_CHECKSIGVERIFY, OP_CSV};
use bitcoin::psbt::Psbt;
use bitcoin::secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin::sign_message::signed_msg_hash;
use bitcoin::taproot::{LeafVersion, TapLeafHash, TaprootBuilder};
use bitcoin::{
    absolute, taproot, transaction::Version, Address, Amount, Network, OutPoint, ScriptBuf,
    Sequence, TapSighashType, Transaction, TxIn, TxOut, Txid, Witness,
};
use desktop_app::application::payouts::{self, fingerprint_u32, PayoutDevice, PayoutState};
use desktop_app::infrastructure::bitcoin_rpc::HttpBitcoinRpcClient;
use desktop_app::infrastructure::payout_policy_store::PolicyStore;
use desktop_app::infrastructure::signing::SignatureResult;
use musig2::KeyAggContext;
use payout_admin_core::payout_tx::key_sighash;
use payout_admin_core::policy::{self, PolicySpec};
use payout_admin_core::test_support::{sign_as, Fixture};
use payout_admin_core::Deployment;
use serde_json::{json, Value};
use strata_bridge_connectors::prelude::{ClaimContestConnector, KeyedAnchor};
use strata_bridge_connectors::Connector;

// ─── Software "Ledger" ───────────────────────────────────────────────────────

pub struct SoftDevice {
    pub fx: Arc<Fixture>,
    pub dep: Deployment,
    pub admin: usize,
}

impl PayoutDevice for SoftDevice {
    fn app(&self) -> Result<(String, String), String> {
        Ok(("Bitcoin Test".into(), "2.5.1".into()))
    }

    fn identity(&self, _account: &DerivationPath) -> Result<(u32, String), String> {
        Ok((
            fingerprint_u32(self.fx.fingerprint(self.admin)),
            self.fx.account_xpub(self.admin).to_string(),
        ))
    }

    fn show_address(&self, _p: &PolicySpec, index: u32) -> Result<String, String> {
        // What the Ledger shows on regtest: the testnet encoding.
        let a = wallet_address(&self.fx, self.admin, index);
        Ok(
            Address::from_script(&a.script_pubkey(), bitcoin::Network::Testnet)
                .unwrap()
                .to_string(),
        )
    }

    fn register_policy(&self, _p: &PolicySpec, _fp: u32) -> Result<[u8; 32], String> {
        Ok([7; 32])
    }

    fn policy_address(&self, p: &PolicySpec, _hmac: [u8; 32]) -> Result<String, String> {
        let cov = self
            .dep
            .covenants
            .iter()
            .find(|c| policy::admin_policy_name(c) == p.name)
            .ok_or("unknown policy")?;
        Ok(policy::connector_address(&self.dep, cov).to_string())
    }

    fn sign_psbt(
        &self,
        psbt: &mut Psbt,
        p: &PolicySpec,
        _hmac: Option<[u8; 32]>,
        fp: u32,
    ) -> Result<usize, String> {
        assert_eq!(fp, fingerprint_u32(self.fx.fingerprint(self.admin)));
        if p.template.contains("multi_a") {
            sign_as(&self.fx, &self.dep, psbt, self.admin);
            return Ok(1);
        }
        // Admin Wallet: key-path spends of inputs whose key origin is this device.
        let secp = Secp256k1::new();
        let prevouts: Vec<_> = psbt
            .inputs
            .iter()
            .map(|i| i.witness_utxo.clone().unwrap())
            .collect();
        let mut n = 0;
        for i in 0..psbt.inputs.len() {
            let Some(internal) = psbt.inputs[i].tap_internal_key else {
                continue;
            };
            let Some((_, (origin_fp, path))) = psbt.inputs[i].tap_key_origins.get(&internal) else {
                continue;
            };
            if *origin_fp != self.fx.fingerprint(self.admin)
                || psbt.inputs[i].tap_merkle_root.is_some()
            {
                continue;
            }
            let tail: Vec<ChildNumber> = path.into_iter().rev().take(2).rev().copied().collect();
            let k = self
                .fx
                .account_xpriv(self.admin)
                .derive_priv(&secp, &tail)
                .unwrap();
            let kp = Keypair::from_secret_key(&secp, &k.private_key);
            assert_eq!(kp.x_only_public_key().0, internal);
            let msg = key_sighash(&psbt.unsigned_tx, &prevouts, i).unwrap();
            let sig = secp.sign_schnorr(&msg, &kp.tap_tweak(&secp, None).to_keypair());
            psbt.inputs[i].tap_key_sig = Some(taproot::Signature {
                signature: sig,
                sighash_type: TapSighashType::Default,
            });
            n += 1;
        }
        Ok(n)
    }

    fn sign_message(
        &self,
        message: &str,
        _path: &DerivationPath,
        _fp: u32,
    ) -> Result<SignatureResult, String> {
        let secp = Secp256k1::new();
        let sk = self.fx.admin_keypair(self.admin).secret_key();
        let msg = Message::from_digest(signed_msg_hash(message).to_byte_array());
        let (recid, compact) = secp.sign_ecdsa_recoverable(&msg, &sk).serialize_compact();
        let mut out = compact.to_vec();
        out.push(recid.to_i32() as u8);
        Ok(SignatureResult {
            public_key_hex: bitcoin::secp256k1::PublicKey::from_secret_key(&secp, &sk).to_string(),
            signature_hex: hex::encode(out),
        })
    }
}

// ─── Coordinator subprocess ──────────────────────────────────────────────────

pub struct Coordinator {
    pub(super) child: Option<Child>,
    pub base_url: String,
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub fn orchestrator_binary() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let status = Command::new("cargo")
        .current_dir(root)
        .args(["build", "-p", "orchestrator-be", "--features", "dev-mocks"])
        .status()
        .expect("cargo build");
    assert!(status.success());
    let out = Command::new("cargo")
        .current_dir(root)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .unwrap();
    let meta: Value = serde_json::from_slice(&out.stdout).unwrap();
    format!(
        "{}/debug/server",
        meta["target_directory"].as_str().unwrap()
    )
}

pub async fn start_coordinator(params_path: &str, rpc: (&str, &str, &str)) -> Coordinator {
    start_coordinator_with_asm(params_path, rpc, "mock://asm-membership").await
}

/// The coordinator with `asm_url` as its ASM endpoint (Block Payouts must not need it).
pub async fn start_coordinator_with_asm(
    params_path: &str,
    rpc: (&str, &str, &str),
    asm_url: &str,
) -> Coordinator {
    let port = free_port();
    let child = Command::new(orchestrator_binary())
        .env("SERVER_HOST", "127.0.0.1")
        .env("SERVER_PORT", port.to_string())
        .env("RUST_LOG", "warn")
        .env("STRATA_ADMIN_STATE_RPC_URL", asm_url)
        .env("PAYOUT_ADMIN_PARAMS_PATH", params_path)
        .env("BITCOIN_NETWORK", "regtest")
        .env("BITCOIN_RPC_URL", rpc.0)
        .env("BITCOIN_RPC_USER", rpc.1)
        .env("BITCOIN_RPC_PASS", rpc.2)
        .env_remove("DATABASE_URL")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("start coordinator");
    // Owned by `Coordinator` from here on, so it is killed and reaped on every path.
    let c = Coordinator {
        child: Some(child),
        base_url: format!("http://127.0.0.1:{port}/api/v1"),
    };
    let http = reqwest::Client::new();
    for _ in 0..100 {
        if http
            .get(format!("{}/health", c.base_url))
            .send()
            .await
            .is_ok()
        {
            return c;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("coordinator did not start");
}

// ─── Node helpers ────────────────────────────────────────────────────────────

pub async fn call(node: &HttpBitcoinRpcClient, method: &str, params: Value) -> Value {
    node.rpc(method, params)
        .await
        .unwrap_or_else(|e| panic!("{method}: {}", e.message))
}

pub async fn mine(node: &HttpBitcoinRpcClient, n: u64) {
    let addr = call(node, "getnewaddress", json!([])).await;
    call(node, "generatetoaddress", json!([n, addr])).await;
}

/// A "claim": vout 0 to the miner wallet, vout 1 = 330 sat at the covenant connector.
pub async fn fund_claim(node: &HttpBitcoinRpcClient, connector: &Address) -> Txid {
    let addr = call(node, "getnewaddress", json!([])).await;
    let raw = call(
        node,
        "createrawtransaction",
        json!([[], [{ addr.as_str().unwrap(): 0.001 }, { connector.to_string(): 0.0000033 }]]),
    )
    .await;
    let funded = call(
        node,
        "fundrawtransaction",
        json!([raw, { "changePosition": 2 }]),
    )
    .await;
    let signed = call(node, "signrawtransactionwithwallet", json!([funded["hex"]])).await;
    let txid = call(node, "sendrawtransaction", json!([signed["hex"]])).await;
    Txid::from_str(txid.as_str().unwrap()).unwrap()
}

pub fn wallet_address(fx: &Fixture, admin: usize, index: u32) -> Address {
    branch_address(fx, admin, 0, index)
}

/// Admin Wallet change address (`…/1/index`), as a string.
#[allow(dead_code)]
pub fn change_address_of(fx: &Fixture, admin: usize, index: u32) -> String {
    branch_address(fx, admin, 1, index).to_string()
}

fn branch_address(fx: &Fixture, admin: usize, branch: u32, index: u32) -> Address {
    let secp = Secp256k1::new();
    let k = fx
        .account_xpub(admin)
        .derive_pub(
            &secp,
            &[
                ChildNumber::Normal { index: branch },
                ChildNumber::Normal { index },
            ],
        )
        .unwrap();
    Address::p2tr(&secp, k.public_key.x_only_public_key().0, None, fx.network)
}

pub async fn admin_state(
    fx: &Arc<Fixture>,
    dep: &Deployment,
    admin: usize,
    params: &str,
    roster: &str,
    coordinator: &Coordinator,
) -> PayoutState {
    let device = Arc::new(SoftDevice {
        fx: fx.clone(),
        dep: dep.clone(),
        admin,
    });
    signed_in_state(device, admin, params, roster, coordinator).await
}

pub async fn signed_in_state(
    device: Arc<dyn PayoutDevice>,
    admin: usize,
    params: &str,
    roster: &str,
    coordinator: &Coordinator,
) -> PayoutState {
    let state = PayoutState::new(device, PolicyStore::new(None));
    state
        .load(payouts::load_deployment(params, Some(roster)).unwrap())
        .await;
    payouts::sign_in(&state, &coordinator.base_url)
        .await
        .expect("sign in");
    assert_eq!(state.ctx().await.unwrap().me.index, admin);
    state
}

/// The covenant's N/N secret: MuSig2 KeyAgg of the even-lifted operator keys, as the bridge does.
pub fn n_of_n_keypair(fx: &Fixture, dep: &Deployment) -> Keypair {
    let secp = Secp256k1::new();
    let cov = &dep.covenants[0];
    let (mut pks, mut sks) = (Vec::new(), Vec::new());
    for &i in &cov.operator_indices {
        let mut sk = fx.operator_keys[i as usize];
        if sk.x_only_public_key(&secp).1 == bitcoin::secp256k1::Parity::Odd {
            sk = sk.negate();
        }
        pks.push(PublicKey::from_secret_key(&secp, &sk));
        sks.push(secp::Scalar::from(sk));
    }
    let agg: SecretKey = KeyAggContext::new(pks)
        .unwrap()
        .aggregated_seckey(sks)
        .unwrap();
    let kp = Keypair::from_secret_key(&secp, &agg);
    assert_eq!(kp.x_only_public_key().0, cov.n_of_n_xonly, "N/N secret");
    kp
}

pub fn btc(sat: u64) -> f64 {
    Amount::from_sat(sat).to_btc()
}

pub async fn new_addr(node: &HttpBitcoinRpcClient) -> String {
    call(node, "getnewaddress", json!(["", "bech32m"]))
        .await
        .as_str()
        .unwrap()
        .to_string()
}

/// Funds and signs `outputs` from the wallet and broadcasts. Output 2 is a wallet output of our
/// own, so the layout holds when the wallet finds coins that need no change; any change is last.
pub async fn send_claim(node: &HttpBitcoinRpcClient, mut outputs: Vec<Value>) -> Transaction {
    outputs.insert(2, json!({new_addr(node).await: btc(10_000)}));
    let n = outputs.len();
    let raw = call(node, "createrawtransaction", json!([[], outputs])).await;
    let funded = call(
        node,
        "fundrawtransaction",
        json!([raw, {"changePosition": n}]),
    )
    .await;
    sign_send(node, funded["hex"].as_str().unwrap()).await
}

pub async fn sign_send(node: &HttpBitcoinRpcClient, hex: &str) -> Transaction {
    let signed = call(node, "signrawtransactionwithwallet", json!([hex])).await;
    assert!(signed["complete"].as_bool().unwrap(), "{signed}");
    let hex = signed["hex"].as_str().unwrap();
    call(node, "sendrawtransaction", json!([hex])).await;
    deserialize_hex(hex).unwrap()
}

/// Shared game construction accepts either a Core wallet or the tester kit's funding adapter.
pub trait Funding {
    fn node(&self) -> &HttpBitcoinRpcClient;
    async fn address(&self) -> String;
    async fn mine(&self, n: u64);
    async fn sign_send(&self, raw: &str) -> Transaction;
    async fn claim(&self, outputs: Vec<TxOut>) -> Transaction;
}

struct WalletFunding<'a>(&'a HttpBitcoinRpcClient);

impl Funding for WalletFunding<'_> {
    fn node(&self) -> &HttpBitcoinRpcClient {
        self.0
    }
    async fn address(&self) -> String {
        new_addr(self.0).await
    }
    async fn mine(&self, n: u64) {
        call(
            self.0,
            "generatetoaddress",
            json!([n, self.address().await]),
        )
        .await;
    }
    async fn sign_send(&self, raw: &str) -> Transaction {
        sign_send(self.0, raw).await
    }
    async fn claim(&self, outputs: Vec<TxOut>) -> Transaction {
        let outputs = outputs.iter().map(|o| json!({Address::from_script(&o.script_pubkey, Network::Regtest).unwrap().to_string(): btc(o.value.to_sat())})).collect();
        send_claim(self.0, outputs).await
    }
}

pub struct Game {
    pub claim: Transaction,
    pub contest: Transaction,
}

/// A claim (outputs: contest connector, admin payout connector, change, CPFP anchor to the
/// operator) and its contest through watchtower 0's leaf (N/N + watchtower signatures).
pub async fn claim_and_contest(
    node: &HttpBitcoinRpcClient,
    fx: &Fixture,
    dep: &Deployment,
    nn: &Keypair,
) -> Game {
    claim_and_contest_funded(&WalletFunding(node), fx, dep, nn).await
}

pub async fn claim_and_contest_funded(
    funding: &impl Funding,
    fx: &Fixture,
    dep: &Deployment,
    nn: &Keypair,
) -> Game {
    let node = funding.node();
    let secp = Secp256k1::new();
    let net = dep.network;
    let wts = [fx.operator_keys[1], fx.operator_keys[2]];
    let contest_conn = ClaimContestConnector::new(
        net,
        nn.x_only_public_key().0,
        wts.iter().map(|k| k.x_only_public_key(&secp).0).collect(),
        bitcoin::relative::Height::from_height(dep.contest_timelock),
        Amount::from_sat(200_000),
    );
    let cov = &dep.covenants[0];
    let anchor = KeyedAnchor::new(net, fx.operator_xonly(0), Amount::from_sat(330));
    let c3 = anchor.tx_out();
    let claim = funding
        .claim(vec![
            contest_conn.tx_out(),
            cov.connector.tx_out(),
            c3.clone(),
        ])
        .await;
    assert_eq!(claim.output[1].script_pubkey, cov.connector_script);
    assert_eq!(claim.output[3].script_pubkey, c3.script_pubkey);

    // Contest: proof, payout, slash, one per watchtower, anchor — all to the wallet.
    let mut outputs = Vec::new();
    for i in 0..6 {
        // Output 1 is the payout connector: a key-path output under the N/N key, as the ACK
        // spends it. The others go to the wallet.
        let script_pubkey = if i == 1 {
            ScriptBuf::new_p2tr(&secp, nn.x_only_public_key().0, None)
        } else {
            Address::from_str(&funding.address().await)
                .unwrap()
                .assume_checked()
                .script_pubkey()
        };
        outputs.push(TxOut {
            value: Amount::from_sat(30_000),
            script_pubkey,
        });
    }
    let mut contest = Transaction {
        version: Version(2),
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(claim.compute_txid(), 0),
            sequence: Sequence::MAX,
            ..Default::default()
        }],
        output: outputs,
    };
    let leaf = contest_conn.leaf_scripts()[0].clone();
    let cb = contest_conn
        .spend_info()
        .control_block(&(leaf.clone(), LeafVersion::TapScript))
        .unwrap();
    let sighash = SighashCache::new(&contest)
        .taproot_script_spend_signature_hash(
            0,
            &Prevouts::All(&[claim.output[0].clone()]),
            TapLeafHash::from_script(&leaf, LeafVersion::TapScript),
            TapSighashType::Default,
        )
        .unwrap();
    let msg = Message::from_digest(sighash.to_byte_array());
    let wt = Keypair::from_secret_key(&secp, &wts[0]);
    let sig_wt = secp.sign_schnorr(&msg, &wt);
    let sig_nn = secp.sign_schnorr(&msg, nn);
    contest.input[0].witness = Witness::from_slice(&[
        sig_wt.as_ref().to_vec(),
        sig_nn.as_ref().to_vec(),
        leaf.to_bytes(),
        cb.serialize(),
    ]);
    call(node, "sendrawtransaction", json!([serialize_hex(&contest)])).await;
    Game { claim, contest }
}

/// The counterproof's ack/nack connector: an ACK spends it through the timeout leaf
/// `<N/N> OP_CHECKSIGVERIFY <delta> OP_CSV`, which is what a verifier reads off the ACK.
fn ack_connector(nn: &Keypair) -> (ScriptBuf, ScriptBuf, bitcoin::taproot::ControlBlock) {
    let secp = Secp256k1::new();
    let nn_x = nn.x_only_public_key().0;
    let leaf = bitcoin::script::Builder::new()
        .push_x_only_key(&nn_x)
        .push_opcode(OP_CHECKSIGVERIFY)
        .push_int(1)
        .push_opcode(OP_CSV)
        .into_script();
    let info = TaprootBuilder::new()
        .add_leaf(0, leaf.clone())
        .unwrap()
        .finalize(&secp, nn_x)
        .unwrap();
    let cb = info
        .control_block(&(leaf.clone(), LeafVersion::TapScript))
        .unwrap();
    (
        ScriptBuf::new_p2tr(&secp, nn_x, info.merkle_root()),
        leaf,
        cb,
    )
}

/// Mines a claim/contest, counterproof and consensus-valid ACK of that contest.
pub async fn acked_game(
    node: &HttpBitcoinRpcClient,
    fx: &Fixture,
    dep: &Deployment,
    nn: &Keypair,
) -> (Game, Transaction) {
    acked_game_funded(&WalletFunding(node), fx, dep, nn).await
}

pub async fn acked_game_funded(
    funding: &impl Funding,
    fx: &Fixture,
    dep: &Deployment,
    nn: &Keypair,
) -> (Game, Transaction) {
    let node = funding.node();
    let old = claim_and_contest_funded(funding, fx, dep, nn).await;
    funding.mine(1).await;
    let ct = old.contest.compute_txid();
    // The counterproof spends watchtower 0's output; its output 0 is the ack/nack connector.
    let (cp_script, cp_leaf, cp_cb) = ack_connector(nn);
    let cp_addr = Address::from_script(&cp_script, Network::Regtest).unwrap();
    let raw = call(
        node,
        "createrawtransaction",
        json!([[{"txid": ct.to_string(), "vout": 3}], [{cp_addr.to_string(): btc(29_000)}]]),
    )
    .await;
    let cp = funding.sign_send(raw.as_str().unwrap()).await;
    funding.mine(1).await;
    // The ACK spends the counterproof's connector through the timeout leaf and the contest's
    // payout connector by key path, as one version 3 transaction with one taproot output.
    let secp = Secp256k1::new();
    let prevouts = [cp.output[0].clone(), old.contest.output[1].clone()];
    let mut ack = Transaction {
        version: Version(3),
        lock_time: absolute::LockTime::ZERO,
        input: vec![
            TxIn {
                previous_output: OutPoint::new(cp.compute_txid(), 0),
                sequence: Sequence::from_height(1),
                ..Default::default()
            },
            TxIn {
                previous_output: OutPoint::new(ct, 1),
                sequence: Sequence::MAX,
                ..Default::default()
            },
        ],
        output: vec![TxOut {
            value: Amount::from_sat(58_000),
            script_pubkey: Address::from_str(&funding.address().await)
                .unwrap()
                .assume_checked()
                .script_pubkey(),
        }],
    };
    let (leaf_sighash, key_sighash) = {
        let mut cache = SighashCache::new(&ack);
        (
            cache
                .taproot_script_spend_signature_hash(
                    0,
                    &Prevouts::All(&prevouts),
                    TapLeafHash::from_script(&cp_leaf, LeafVersion::TapScript),
                    TapSighashType::Default,
                )
                .unwrap(),
            cache
                .taproot_key_spend_signature_hash(
                    1,
                    &Prevouts::All(&prevouts),
                    TapSighashType::Default,
                )
                .unwrap(),
        )
    };
    let leaf_sig = secp.sign_schnorr(&Message::from_digest(leaf_sighash.to_byte_array()), nn);
    let key_sig = secp.sign_schnorr(
        &Message::from_digest(key_sighash.to_byte_array()),
        &nn.tap_tweak(&secp, None).to_keypair(),
    );
    ack.input[0].witness = Witness::from_slice(&[
        leaf_sig.as_ref().to_vec(),
        cp_leaf.to_bytes(),
        cp_cb.serialize(),
    ]);
    ack.input[1].witness = Witness::from_slice(&[key_sig.as_ref().to_vec()]);
    call(node, "sendrawtransaction", json!([serialize_hex(&ack)])).await;
    // A report counts only once its ACK has 6 confirmations.
    funding.mine(6).await;

    (old, ack)
}

/// Owns the node for the full test and gives each scenario its own deployment directory.
pub struct PayoutEnv {
    pub fx: Arc<Fixture>,
    pub dep: Deployment,
    pub params: String,
    pub roster: String,
    pub dir: std::path::PathBuf,
    pub node: HttpBitcoinRpcClient,
    pub coordinator: Coordinator,
    pub bitcoind: corepc_node::Node,
}

pub async fn payout_env(tag: &str) -> PayoutEnv {
    payout_env_with_params(tag, "mock://asm-membership", Fixture::params_toml).await
}

pub async fn payout_env_with_params(
    tag: &str,
    asm_url: &str,
    params_toml: impl FnOnce(&Fixture) -> String,
) -> PayoutEnv {
    let mut conf = corepc_node::Conf::default();
    conf.args.push("-txindex");
    let bitcoind = corepc_node::Node::with_conf("bitcoind", &conf).expect("bitcoind on PATH");
    let cookie = bitcoind.params.get_cookie_values().unwrap().unwrap();
    let url = bitcoind.rpc_url();
    let node = HttpBitcoinRpcClient::new(&url, &cookie.user, &cookie.password);
    mine(&node, 110).await;
    let fx = Arc::new(Fixture::new(3, 2));
    let dir = std::env::temp_dir().join(format!("{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let params = dir.join("params.toml");
    let roster = dir.join("roster.toml");
    std::fs::write(&params, params_toml(&fx)).unwrap();
    std::fs::write(&roster, fx.roster_toml()).unwrap();
    let params = params.to_str().unwrap().to_string();
    let roster = roster.to_str().unwrap().to_string();
    let dep = payouts::load_deployment(&params, Some(&roster)).unwrap();
    let coordinator =
        start_coordinator_with_asm(&params, (&url, &cookie.user, &cookie.password), asm_url).await;
    PayoutEnv {
        fx,
        dep,
        params,
        roster,
        dir,
        node,
        coordinator,
        bitcoind,
    }
}

/// Check signatures against stored PSBTs, never coordinator summary counts.
pub async fn stored_signature_counts(ctx: &payouts::Ctx, txid: &str) -> (usize, usize) {
    let psbt = match ctx.coordinator.get_pending(txid).await {
        Ok(p) => p.psbt,
        Err(_) => {
            ctx.coordinator
                .past()
                .await
                .unwrap()
                .into_iter()
                .find(|p| p.txid == txid)
                .unwrap()
                .psbt
        }
    };
    let analysis = payout_admin_core::payout_tx::analyze(&ctx.dep, &psbt.parse().unwrap()).unwrap();
    (analysis.signatures_received, analysis.signatures_needed)
}
