//! Test fixtures: deterministic admins and operators, `params.toml` and roster text, and a
//! structurally exact bridge game (claim → contest → counterproof → ack) built with the bridge's
//! connectors. A report carries only the claims, the contests and the ack, not the counterproof.

use std::collections::HashMap;
use std::str::FromStr;

use bitcoin::{
    absolute,
    bip32::{Xpriv, Xpub},
    hashes::Hash,
    opcodes::all::{OP_CHECKSIGVERIFY, OP_CSV},
    script::Builder,
    secp256k1::{Keypair, PublicKey, Secp256k1, SecretKey, XOnlyPublicKey},
    taproot::{LeafVersion, TaprootBuilder},
    transaction::Version,
    Amount, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
};
use strata_bridge_connectors::{
    prelude::{ClaimContestConnector, KeyedAnchor},
    Connector,
};

use crate::deployment::{account_path, Deployment};
use crate::report::{FalseClaimReport, MinedTx};

pub const DOCKER_PARAMS: &str = r#"network = "regtest"
genesis_height = 101

[keys]

[keys.admin]
pubkeys = [
  "b49092f76d06f8002e0b7f1c63b5058db23fd4465b4f6954b53e1f352a04754d",
  "1e62d54af30569fd7269c14b6766f74d85ea00c911c4e1a423d4ba2ae4c34dc4",
  "a4d869ccd09c470f8f86d3f1b0997fa2695933aaea001875b9db145ae9c1f4ba",
]
threshold = 2

[[keys.operators]]
index = 0
covenant_key = "b49092f76d06f8002e0b7f1c63b5058db23fd4465b4f6954b53e1f352a04754d"
p2p_key = "0de7729dcbeb5069136ee4bff1c4f2fd822fe8fbc9b518df434d4f0c6312d8f5"
payout_descriptor = "0494047994e5ac5dabf9737a176f0c79eeb9f09c9749a087a138ddb1ff3caceb2f"
activation_height = 101

[[keys.operators]]
index = 1
covenant_key = "1e62d54af30569fd7269c14b6766f74d85ea00c911c4e1a423d4ba2ae4c34dc4"
p2p_key = "255ab0da6d468a22910a7cf54021763417c63c28bbafd4e2359daf103bb61e9d"
payout_descriptor = "044f935f6e983bd4bca4e512cc1a5f50137e3b4a03e20c90f60ac7387a88c2e114"
activation_height = 101

[[keys.operators]]
index = 2
covenant_key = "a4d869ccd09c470f8f86d3f1b0997fa2695933aaea001875b9db145ae9c1f4ba"
p2p_key = "aeabd3a377c160590a0927bca0cb315eaebf5acc87476fbb317c6260b6f123c6"
payout_descriptor = "04b63fea4640fbbf630e2bd949a588b63426bb2ea2771ce5b2caf44dbfa90d6cd3"
activation_height = 101

[protocol]
deposit_index_offset = 0
bury_depth = 2
magic_bytes = "ALPN"
deposit_amount = 1_000_000_000
stake_amount = 100_000_000
operator_fee = 10_000_000
sweep_fee_rate = 10
recovery_delay = 1_008
contest_timelock = 144
proof_timelock = 144
ack_timelock = 432
nack_timelock = 144
contested_payout_timelock = 1_008
unstaking_timelock = 2_016
"#;

const P2P_KEYS: [&str; 3] = [
    "0de7729dcbeb5069136ee4bff1c4f2fd822fe8fbc9b518df434d4f0c6312d8f5",
    "255ab0da6d468a22910a7cf54021763417c63c28bbafd4e2359daf103bb61e9d",
    "aeabd3a377c160590a0927bca0cb315eaebf5acc87476fbb317c6260b6f123c6",
];
const PAYOUT_DESCS: [&str; 3] = [
    "0494047994e5ac5dabf9737a176f0c79eeb9f09c9749a087a138ddb1ff3caceb2f",
    "044f935f6e983bd4bca4e512cc1a5f50137e3b4a03e20c90f60ac7387a88c2e114",
    "04b63fea4640fbbf630e2bd949a588b63426bb2ea2771ce5b2caf44dbfa90d6cd3",
];

pub const NEW_CLAIM_HEIGHT: u64 = 500;

pub struct Fixture {
    pub network: Network,
    pub threshold: usize,
    pub admin_masters: Vec<Xpriv>,
    pub operator_keys: Vec<SecretKey>,
}

fn sk(byte: u8) -> SecretKey {
    SecretKey::from_slice(&[byte; 32]).unwrap()
}

impl Fixture {
    pub fn new(n_admins: usize, threshold: usize) -> Self {
        let network = Network::Regtest;
        let admin_masters = (0..n_admins)
            .map(|i| Xpriv::new_master(network, &[i as u8 + 1; 32]).unwrap())
            .collect();
        let operator_keys = (0..3).map(|i| sk(100 + i as u8)).collect();
        Self {
            network,
            threshold,
            admin_masters,
            operator_keys,
        }
    }

    pub fn account_xpriv(&self, i: usize) -> Xpriv {
        let secp = Secp256k1::new();
        self.admin_masters[i]
            .derive_priv(&secp, &account_path(self.network))
            .unwrap()
    }

    pub fn account_xpub(&self, i: usize) -> Xpub {
        Xpub::from_priv(&Secp256k1::new(), &self.account_xpriv(i))
    }

    pub fn fingerprint(&self, i: usize) -> bitcoin::bip32::Fingerprint {
        self.admin_masters[i].fingerprint(&Secp256k1::new())
    }

    /// The admin's signing key: account/0/0.
    pub fn admin_keypair(&self, i: usize) -> Keypair {
        let secp = Secp256k1::new();
        let k = self
            .account_xpriv(i)
            .derive_priv(
                &secp,
                &bitcoin::bip32::DerivationPath::from_str("m/0/0").unwrap(),
            )
            .unwrap();
        Keypair::from_secret_key(&secp, &k.private_key)
    }

    pub fn admin_xonly(&self, i: usize) -> XOnlyPublicKey {
        self.admin_keypair(i).x_only_public_key().0
    }

    pub fn operator_xonly(&self, i: usize) -> XOnlyPublicKey {
        self.operator_keys[i].x_only_public_key(&Secp256k1::new()).0
    }

    pub fn foreign_n_of_n(&self) -> PublicKey {
        PublicKey::from_secret_key(&Secp256k1::new(), &sk(77))
    }

    pub fn params_toml(&self) -> String {
        self.params_toml_with_schedule(&[(0, 101, None), (1, 101, None), (2, 101, None)])
    }

    pub fn params_toml_with_schedule(&self, ops: &[(usize, u64, Option<u64>)]) -> String {
        let mut s = format!(
            "network = \"regtest\"\ngenesis_height = 101\n\n[keys.admin]\npubkeys = [{}]\nthreshold = {}\n\n",
            (0..self.admin_masters.len())
                .map(|i| format!("\"{}\"", self.admin_xonly(i)))
                .collect::<Vec<_>>()
                .join(", "),
            self.threshold
        );
        for &(i, act, deact) in ops {
            s.push_str(&format!(
                "[[keys.operators]]\nindex = {i}\ncovenant_key = \"{}\"\np2p_key = \"{}\"\npayout_descriptor = \"{}\"\nactivation_height = {act}\n",
                self.operator_xonly(i),
                P2P_KEYS[i],
                PAYOUT_DESCS[i]
            ));
            if let Some(d) = deact {
                s.push_str(&format!("deactivation_height = {d}\n"));
            }
            s.push('\n');
        }
        let proto = DOCKER_PARAMS.split("[protocol]").nth(1).unwrap();
        s.push_str("[protocol]");
        s.push_str(proto);
        s
    }

    pub fn roster_toml(&self) -> String {
        let order: Vec<usize> = (0..self.admin_masters.len()).collect();
        self.roster_toml_with_order(&order)
    }

    pub fn roster_toml_with_order(&self, order: &[usize]) -> String {
        let mut s = String::from("network = \"regtest\"\n\n");
        for &i in order {
            s.push_str(&format!(
                "[[admins]]\nlabel = \"Admin {i}\"\nfingerprint = \"{}\"\nxpub = \"{}\"\n\n",
                self.fingerprint(i),
                self.account_xpub(i)
            ));
        }
        s
    }

    pub fn deployment(&self) -> Deployment {
        Deployment::load(&self.params_toml(), Some(&self.roster_toml())).unwrap()
    }

    pub fn game(&self, dep: &Deployment, variant: u32) -> GameTxs {
        self.game_with_n_of_n(dep, variant, dep.covenants[0].n_of_n)
    }

    pub fn game_with_n_of_n(&self, dep: &Deployment, variant: u32, n_of_n: PublicKey) -> GameTxs {
        GameTxs::build(self, dep, variant, n_of_n)
    }
}

fn dummy_out(byte: u8) -> TxOut {
    let k = sk(byte).x_only_public_key(&Secp256k1::new()).0;
    TxOut {
        value: Amount::from_sat(1_000),
        script_pubkey: ScriptBuf::new_p2tr(&Secp256k1::new(), k, None),
    }
}

fn tx(inputs: Vec<TxIn>, outputs: Vec<TxOut>) -> Transaction {
    Transaction {
        version: Version(3),
        lock_time: absolute::LockTime::ZERO,
        input: inputs,
        output: outputs,
    }
}

fn txin(prev: OutPoint, witness: Witness) -> TxIn {
    TxIn {
        previous_output: prev,
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness,
    }
}

pub fn key_witness() -> Witness {
    Witness::from_slice(&[[1u8; 64].to_vec()])
}

/// A watchtower ack as the bridge builds it: it spends the contest's payout connector and the
/// counterproof's ack/nack connector, the latter through the timeout leaf
/// `<N/N> OP_CHECKSIGVERIFY <delta> OP_CSV`.
fn ack_tx(n_of_n: PublicKey, counterproof: Txid, contest: Txid) -> Transaction {
    let nn_x = n_of_n.x_only_public_key().0;
    let leaf = Builder::new()
        .push_x_only_key(&nn_x)
        .push_opcode(OP_CHECKSIGVERIFY)
        .push_int(144)
        .push_opcode(OP_CSV)
        .into_script();
    let cb = TaprootBuilder::new()
        .add_leaf(0, leaf.clone())
        .unwrap()
        .finalize(&Secp256k1::new(), nn_x)
        .unwrap()
        .control_block(&(leaf.clone(), LeafVersion::TapScript))
        .unwrap();
    let timeout_witness =
        Witness::from_slice(&[[4u8; 64].to_vec(), leaf.to_bytes(), cb.serialize()]);
    tx(
        vec![
            txin(OutPoint::new(counterproof, 0), timeout_witness),
            txin(OutPoint::new(contest, 1), key_witness()),
        ],
        vec![dummy_out(32)],
    )
}

#[derive(Clone)]
pub struct GameTxs {
    pub report: FalseClaimReport,
    /// What a node returns for the report's txids: new claim, new contest, acked claim, acked
    /// contest, ack.
    pub txs: Vec<(Transaction, Option<u64>)>,
    /// The counterproof between the acked contest and the ack. It is on chain but not in a report.
    pub counterproof_txid: Txid,
    #[allow(dead_code)]
    pub new_claim: Transaction,
}

const N_WATCHTOWERS: usize = 2;

impl GameTxs {
    fn claim_and_contest(
        fx: &Fixture,
        dep: &Deployment,
        n_of_n: PublicKey,
        seed: u32,
    ) -> (Transaction, Transaction) {
        let nn_x = n_of_n.x_only_public_key().0;
        let wts: Vec<XOnlyPublicKey> = (0..N_WATCHTOWERS)
            .map(|i| fx.operator_xonly(i + 1))
            .collect();
        let contest_conn = ClaimContestConnector::new(
            dep.network,
            nn_x,
            wts,
            bitcoin::relative::Height::from_height(dep.contest_timelock),
            Amount::ZERO,
        );
        let admin_conn = strata_bridge_connectors::prelude::ClaimAdminPayoutConnector::new(
            dep.network,
            n_of_n,
            dep.admin_keys.clone(),
            dep.threshold,
        );
        let anchor = KeyedAnchor::new(dep.network, fx.operator_xonly(0), Amount::from_sat(330));
        let funding = OutPoint::new(Txid::from_byte_array([seed as u8 + 1; 32]), seed);
        let claim = tx(
            vec![txin(funding, key_witness())],
            vec![
                contest_conn.tx_out(),
                admin_conn.tx_out(),
                dummy_out(9),
                anchor.tx_out(),
            ],
        );
        // Contest: spends claim:0 through watchtower 0's contest leaf.
        let leaf = contest_conn.leaf_scripts()[0].clone();
        let cb = contest_conn
            .spend_info()
            .control_block(&(leaf.clone(), LeafVersion::TapScript))
            .unwrap();
        let witness = Witness::from_slice(&[
            [2u8; 64].to_vec(),
            [3u8; 64].to_vec(),
            leaf.to_bytes(),
            cb.serialize(),
        ]);
        let mut outs = vec![dummy_out(10), dummy_out(11), dummy_out(12)];
        for i in 0..N_WATCHTOWERS {
            outs.push(dummy_out(13 + i as u8));
        }
        outs.push(dummy_out(20));
        let contest = tx(
            vec![txin(OutPoint::new(claim.compute_txid(), 0), witness)],
            outs,
        );
        (claim, contest)
    }

    fn build(fx: &Fixture, dep: &Deployment, variant: u32, n_of_n: PublicKey) -> Self {
        let (acked_claim, acked_contest) =
            Self::claim_and_contest(fx, dep, n_of_n, 10 + variant * 2);
        let (new_claim, new_contest) = Self::claim_and_contest(fx, dep, n_of_n, 11 + variant * 2);
        let ct = acked_contest.compute_txid();
        let cp = tx(
            vec![txin(OutPoint::new(ct, 4), key_witness())],
            vec![dummy_out(30), dummy_out(31)],
        );
        let counterproof_txid = cp.compute_txid();
        let ack = ack_tx(n_of_n, counterproof_txid, ct);
        let report = FalseClaimReport {
            report_type: Some(crate::report::REPORT_TYPE.into()),
            version: Some(crate::report::REPORT_VERSION),
            network: Some("regtest".into()),
            created_at: Some("2026-11-11T00:00:00Z".into()),
            operator_pubkey: fx.operator_xonly(0),
            new_claim_txid: new_claim.compute_txid(),
            new_contest_txid: new_contest.compute_txid(),
            acked_claim_txid: acked_claim.compute_txid(),
            acked_contest_txid: ct,
            counterproof_ack_txid: ack.compute_txid(),
        };
        Self {
            report,
            txs: vec![
                (new_claim.clone(), Some(NEW_CLAIM_HEIGHT)),
                (new_contest, Some(NEW_CLAIM_HEIGHT + 1)),
                (acked_claim, Some(300)),
                (acked_contest, Some(301)),
                (ack, Some(400)),
            ],
            counterproof_txid,
            new_claim,
        }
    }

    /// The acked contest's txid.
    pub fn acked_contest_txid(&self) -> Txid {
        self.report.acked_contest_txid
    }

    /// Replaces the ack with a variant of it and points the report at the variant.
    pub fn tamper_ack(&mut self, f: impl FnOnce(&mut Transaction)) {
        let mut ack = self.txs[4].0.clone();
        f(&mut ack);
        self.report.counterproof_ack_txid = ack.compute_txid();
        self.txs[4] = (ack, Some(400));
    }

    /// A NACK: it spends the counterproof's ack/nack connector, but not the contest's payout
    /// connector (a NACK leaves the payout connector alone).
    pub fn replace_ack_with_nack(&mut self, _fx: &Fixture) {
        let cp = self.counterproof_txid;
        let nack = tx(
            vec![
                txin(OutPoint::new(cp, 0), key_witness()),
                txin(
                    OutPoint::new(Txid::from_byte_array([99; 32]), 0),
                    key_witness(),
                ),
            ],
            vec![dummy_out(33)],
        );
        self.report.counterproof_ack_txid = nack.compute_txid();
        self.txs[4] = (nack, Some(400));
    }

    pub fn lookup(&self) -> HashMap<Txid, MinedTx> {
        self.txs
            .iter()
            .map(|(t, h)| {
                (
                    t.compute_txid(),
                    MinedTx {
                        tx: t.clone(),
                        block_height: *h,
                        confirmations: 100,
                    },
                )
            })
            .collect()
    }
}

// ── Payout transaction helpers ──────────────────────────────────────────────

use crate::payout_tx::{add_signature_in, analyze, key_sighash, ChangeOutput, FeeInput};
use bitcoin::{bip32::DerivationPath, key::TapTweak, psbt::Psbt, taproot, TapSighashType};

pub fn fee_coin(fx: &Fixture, admin: usize, idx: u32, value: u64) -> FeeInput {
    let secp = Secp256k1::new();
    let k = fee_key(fx, admin, idx);
    let internal = k.x_only_public_key().0;
    FeeInput {
        outpoint: OutPoint::new(Txid::from_byte_array([200 + idx as u8; 32]), idx),
        txout: TxOut {
            value: Amount::from_sat(value),
            script_pubkey: ScriptBuf::new_p2tr(&secp, internal, None),
        },
        internal_key: internal,
        fingerprint: fx.fingerprint(admin),
        path: DerivationPath::from_str(&format!("m/86'/1'/73'/0/{idx}")).unwrap(),
    }
}

pub fn fee_key(fx: &Fixture, admin: usize, idx: u32) -> Keypair {
    let secp = Secp256k1::new();
    let x = fx
        .account_xpriv(admin)
        .derive_priv(
            &secp,
            &DerivationPath::from_str(&format!("m/0/{idx}")).unwrap(),
        )
        .unwrap();
    Keypair::from_secret_key(&secp, &x.private_key)
}

pub fn change(fx: &Fixture, value: u64) -> ChangeOutput {
    let secp = Secp256k1::new();
    let x = fx
        .account_xpriv(0)
        .derive_priv(&secp, &DerivationPath::from_str("m/1/0").unwrap())
        .unwrap();
    ChangeOutput {
        value: Amount::from_sat(value),
        internal_key: x.private_key.x_only_public_key(&secp).0,
        fingerprint: fx.fingerprint(0),
        path: DerivationPath::from_str("m/86'/1'/73'/1/0").unwrap(),
    }
}

/// Signs every connector input as admin `admin`, the way a device does.
pub fn sign_as(fx: &Fixture, dep: &Deployment, psbt: &mut Psbt, admin: usize) {
    let a = analyze(dep, psbt).unwrap();
    let kp = fx.admin_keypair(admin);
    let secp = Secp256k1::new();
    let mut added = std::collections::BTreeSet::new();
    for c in &a.connector_inputs {
        let sig = secp.sign_schnorr(&c.sighash, &kp);
        add_signature_in(
            dep,
            psbt,
            &a,
            &mut added,
            c.index,
            &kp.x_only_public_key().0,
            sig,
        )
        .unwrap();
    }
}

pub fn sign_fee(fx: &Fixture, psbt: &mut Psbt, coin: &FeeInput, idx: u32) {
    let secp = Secp256k1::new();
    let i = psbt
        .unsigned_tx
        .input
        .iter()
        .position(|t| t.previous_output == coin.outpoint)
        .unwrap();
    let prevouts: Vec<_> = psbt
        .inputs
        .iter()
        .map(|x| x.witness_utxo.clone().unwrap())
        .collect();
    let msg = key_sighash(&psbt.unsigned_tx, &prevouts, i).unwrap();
    let tweaked = fee_key(fx, 0, idx).tap_tweak(&secp, None);
    let sig = secp.sign_schnorr(&msg, &tweaked.to_keypair());
    psbt.inputs[i].tap_key_sig = Some(taproot::Signature {
        signature: sig,
        sighash_type: TapSighashType::Default,
    });
}
