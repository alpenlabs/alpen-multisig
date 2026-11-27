//! Test fixtures: deterministic admins and operators, ASM history and signer roster, and a
//! structurally exact bridge game (claim → contest → counterproof → ack) built with the bridge's
//! connectors. A report carries only the claims, the contests and the ack, not the counterproof.

use std::collections::HashMap;
use std::str::FromStr;
#[cfg(test)]
use std::sync::LazyLock;

use bitcoin::{
    absolute,
    bip32::{Xpriv, Xpub},
    hashes::Hash,
    secp256k1::{Keypair, PublicKey, Secp256k1, SecretKey, XOnlyPublicKey},
    taproot::LeafVersion,
    transaction::Version,
    Amount, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
};
use strata_bridge_connectors::{
    prelude::{ClaimContestConnector, CounterproofConnector, KeyedAnchor},
    Connector,
};

use crate::deployment::{account_path, Deployment};
use crate::report::{MinedBlock, MinedTx, Report};

#[cfg(test)]
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

pub const CONTEST_TIMELOCK: bitcoin::relative::Height = bitcoin::relative::Height::from_height(144);

#[cfg(test)]
pub(crate) static P2TR_PLACEHOLDER_SCRIPT: LazyLock<ScriptBuf> =
    LazyLock::new(|| ScriptBuf::from_bytes([&[0x51, 0x20][..], &[0x02; 32][..]].concat()));

pub const NACK_TIMELOCK: bitcoin::relative::Height = bitcoin::relative::Height::from_height(144);

pub const NEW_CLAIM_HEIGHT: u64 = 500;

pub struct Fixture {
    network: Network,
    threshold: usize,
    admin_masters: Vec<Xpriv>,
    operator_keys: Vec<SecretKey>,
}

fn sk(byte: u8) -> SecretKey {
    SecretKey::from_slice(&[byte; 32]).unwrap()
}

impl Fixture {
    /// Returns the operator keys.
    pub fn operator_keys(&self) -> &[SecretKey] {
        &self.operator_keys
    }

    pub fn replace_admin_master(&mut self, index: usize, master: Xpriv) {
        self.admin_masters[index] = master;
    }

    pub fn new(admin_count: usize, threshold: usize) -> Self {
        let network = Network::Regtest;
        let admin_masters = (0..admin_count)
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
        let key = self
            .account_xpriv(i)
            .derive_priv(
                &secp,
                &bitcoin::bip32::DerivationPath::from_str("m/0/0").unwrap(),
            )
            .unwrap();

        Keypair::from_secret_key(&secp, &key.private_key)
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

    pub fn roster_toml(&self) -> String {
        let order: Vec<usize> = (0..self.admin_masters.len()).collect();

        self.roster_toml_with_order(&order)
    }

    pub fn roster_toml_with_order(&self, order: &[usize]) -> String {
        let mut toml = format!("network = \"regtest\"\nthreshold = {}\n\n", self.threshold);
        for &i in order {
            toml.push_str(&format!(
                "[[admins]]\nlabel = \"Admin {i}\"\nfingerprint = \"{}\"\nxpub = \"{}\"\n\n",
                self.fingerprint(i),
                self.account_xpub(i)
            ));
        }

        toml
    }

    pub fn operator_table(&self) -> strata_asm_proto_bridge_state::operator::OperatorTable {
        let keys = (0..self.operator_keys.len())
            .map(|index| self.operator_xonly(index).into())
            .collect::<Vec<_>>();

        strata_asm_proto_bridge_state::operator::OperatorTable::from_operator_list(&keys)
    }

    pub fn deployment(&self) -> Deployment {
        Deployment::load(
            &crate::deployment::SignerRoster::from_toml(&self.roster_toml()).unwrap(),
            &self.operator_table(),
        )
        .unwrap()
    }

    #[cfg(test)]
    pub(crate) fn after_operator_removal(&self) -> Deployment {
        let mut operators = self.operator_table();
        operators.apply_membership_changes(&[], &[1]);
        let roster = crate::SignerRoster::from_toml(&self.roster_toml()).unwrap();

        Deployment::load(&roster, &operators).unwrap()
    }

    pub fn game(&self, deployment: &Deployment, variant: u32) -> GameTxs {
        self.game_with_n_of_n(deployment, variant, deployment.covenant().n_of_n())
    }

    pub fn game_with_n_of_n(
        &self,
        deployment: &Deployment,
        variant: u32,
        n_of_n: PublicKey,
    ) -> GameTxs {
        GameTxs::build(self, deployment, variant, n_of_n, n_of_n)
    }
}

fn dummy_out(byte: u8) -> TxOut {
    let key = sk(byte).x_only_public_key(&Secp256k1::new()).0;

    TxOut {
        value: Amount::from_sat(1_000),
        script_pubkey: ScriptBuf::new_p2tr(&Secp256k1::new(), key, None),
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

fn transaction_input(previous_output: OutPoint, witness: Witness) -> TxIn {
    TxIn {
        previous_output,
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
fn ack_tx(
    deployment: &Deployment,
    n_of_n: PublicKey,
    counterproof: Txid,
    contest: Txid,
) -> Transaction {
    let n_of_n_xonly = n_of_n.x_only_public_key().0;
    let connector = CounterproofConnector::new(
        deployment.network(),
        n_of_n_xonly,
        n_of_n_xonly,
        NACK_TIMELOCK,
        Amount::ZERO,
    );
    let leaf = connector.leaf_scripts().remove(0);
    let control_block = connector
        .spend_info()
        .control_block(&(leaf.clone(), LeafVersion::TapScript))
        .unwrap();
    let timeout_witness = Witness::from_slice(&[
        [4u8; 64].to_vec(),
        leaf.to_bytes(),
        control_block.serialize(),
    ]);

    tx(
        vec![
            transaction_input(OutPoint::new(counterproof, 0), timeout_witness),
            transaction_input(OutPoint::new(contest, 1), key_witness()),
        ],
        vec![dummy_out(32)],
    )
}

#[derive(Clone)]
pub struct GameTxs {
    operator_pubkey: XOnlyPublicKey,
    /// What an Electrum server returns for the report's txids: new claim, new contest, acked claim, acked
    /// contest, ack.
    txs: Vec<(Transaction, Option<u64>)>,
    /// The counterproof between the acked contest and the ack. It is on chain but not in a report.
    counterproof_txid: Txid,
}

const N_WATCHTOWERS: usize = 2;

impl GameTxs {
    #[cfg(test)]
    pub(crate) fn change_claim_operator(
        &mut self,
        acked: bool,
        network: Network,
        operator: XOnlyPublicKey,
    ) {
        let claim_index = if acked { 2 } else { 0 };
        self.txs[claim_index].0.output
            [strata_bridge_tx_graph::transactions::prelude::ClaimTx::CPFP_VOUT as usize]
            .script_pubkey = KeyedAnchor::new(network, operator, Amount::ZERO).script_pubkey();
        let claim_txid = self.txs[claim_index].0.compute_txid();
        self.txs[claim_index + 1].0.input[0].previous_output.txid = claim_txid;
        let contest_txid = self.txs[claim_index + 1].0.compute_txid();

        // Keep the chain links intact so only this claim's operator binding is wrong.
        if acked {
            self.tamper_ack(|ack| ack.input[1].previous_output.txid = contest_txid);
        }
    }

    /// Returns the counterproof txid.
    pub fn counterproof_txid(&self) -> Txid {
        self.counterproof_txid
    }

    /// Returns the report.
    pub fn report(&self) -> Report {
        Report::from_value(serde_json::json!({
            "type": crate::report::REPORT_TYPE, "version": crate::report::REPORT_VERSION, "network": "regtest",
            "operatorPubkey": self.operator_pubkey, "newClaimTxid": self.txs[0].0.compute_txid(),
            "newContestTxid": self.txs[1].0.compute_txid(), "ackedClaimTxid": self.txs[2].0.compute_txid(),
            "ackedContestTxid": self.acked_contest_txid(), "counterproofAckTxid": self.txs[4].0.compute_txid(),
        }), Network::Regtest).unwrap()
    }

    /// Returns the transactions.
    pub fn txs(&self) -> &[(Transaction, Option<u64>)] {
        &self.txs
    }

    fn claim_and_contest(
        fixture: &Fixture,
        deployment: &Deployment,
        n_of_n: PublicKey,
        seed: u32,
    ) -> (Transaction, Transaction) {
        let n_of_n_xonly = n_of_n.x_only_public_key().0;
        let watchtower_keys: Vec<XOnlyPublicKey> = (0..N_WATCHTOWERS)
            .map(|i| fixture.operator_xonly(i + 1))
            .collect();
        let contest_connector = ClaimContestConnector::new(
            deployment.network(),
            n_of_n_xonly,
            watchtower_keys,
            CONTEST_TIMELOCK,
            Amount::ZERO,
        );
        let admin_connector = strata_bridge_connectors::prelude::ClaimAdminPayoutConnector::new(
            deployment.network(),
            n_of_n,
            deployment.admin_keys().to_vec(),
            deployment.threshold(),
        );
        let anchor = KeyedAnchor::new(
            deployment.network(),
            fixture.operator_xonly(0),
            Amount::from_sat(330),
        );
        let funding = OutPoint::new(Txid::from_byte_array([seed as u8 + 1; 32]), seed);
        let claim = tx(
            vec![transaction_input(funding, key_witness())],
            vec![
                contest_connector.tx_out(),
                admin_connector.tx_out(),
                dummy_out(9),
                anchor.tx_out(),
            ],
        );
        // Contest: spends claim:0 through watchtower 0's contest leaf.
        let leaf = contest_connector.leaf_scripts()[0].clone();
        let control_block = contest_connector
            .spend_info()
            .control_block(&(leaf.clone(), LeafVersion::TapScript))
            .unwrap();
        let witness = Witness::from_slice(&[
            [2u8; 64].to_vec(),
            [3u8; 64].to_vec(),
            leaf.to_bytes(),
            control_block.serialize(),
        ]);
        let mut outputs = vec![dummy_out(10), dummy_out(11), dummy_out(12)];
        for i in 0..N_WATCHTOWERS {
            outputs.push(dummy_out(13 + i as u8));
        }
        outputs.push(dummy_out(20));
        let contest = tx(
            vec![transaction_input(
                OutPoint::new(claim.compute_txid(), 0),
                witness,
            )],
            outputs,
        );

        (claim, contest)
    }

    pub(crate) fn build(
        fixture: &Fixture,
        deployment: &Deployment,
        variant: u32,
        acked_key: PublicKey,
        new_key: PublicKey,
    ) -> Self {
        let (acked_claim, acked_contest) =
            Self::claim_and_contest(fixture, deployment, acked_key, 10 + variant * 2);
        let (new_claim, new_contest) =
            Self::claim_and_contest(fixture, deployment, new_key, 11 + variant * 2);
        let contest_txid = acked_contest.compute_txid();
        let counterproof = tx(
            vec![transaction_input(
                OutPoint::new(contest_txid, 4),
                key_witness(),
            )],
            vec![dummy_out(30), dummy_out(31)],
        );
        let counterproof_txid = counterproof.compute_txid();
        let ack = ack_tx(deployment, acked_key, counterproof_txid, contest_txid);

        Self {
            operator_pubkey: fixture.operator_xonly(0),
            txs: vec![
                (new_claim, Some(NEW_CLAIM_HEIGHT)),
                (new_contest, Some(NEW_CLAIM_HEIGHT + 1)),
                (acked_claim, Some(300)),
                (acked_contest, Some(301)),
                (ack, Some(400)),
            ],
            counterproof_txid,
        }
    }

    /// The acked contest's txid.
    pub fn acked_contest_txid(&self) -> Txid {
        self.txs[3].0.compute_txid()
    }

    /// Replaces the ack with a variant of it and points the report at the variant.
    pub fn tamper_ack(&mut self, modify: impl FnOnce(&mut Transaction)) {
        let mut ack = self.txs[4].0.clone();
        modify(&mut ack);
        self.txs[4] = (ack, Some(400));
    }

    /// A NACK: it spends the counterproof's ack/nack connector, but not the contest's payout
    /// connector (a NACK leaves the payout connector alone).
    pub fn replace_ack_with_nack(&mut self, _fixture: &Fixture) {
        let counterproof_txid = self.counterproof_txid;
        let nack = tx(
            vec![
                transaction_input(OutPoint::new(counterproof_txid, 0), key_witness()),
                transaction_input(
                    OutPoint::new(Txid::from_byte_array([99; 32]), 0),
                    key_witness(),
                ),
            ],
            vec![dummy_out(33)],
        );
        self.txs[4] = (nack, Some(400));
    }

    pub fn lookup(&self) -> HashMap<Txid, MinedTx> {
        self.txs
            .iter()
            .map(|(transaction, block_height)| {
                (
                    transaction.compute_txid(),
                    MinedTx::new(
                        transaction.clone(),
                        block_height.map(|height| MinedBlock::new(height, 100)),
                    ),
                )
            })
            .collect()
    }
}

// ── Block payouts transaction helpers ──────────────────────────────────────────────

use crate::block_payouts_tx::{
    add_signature_with_analysis, analyze, key_sighash, ChangeOutput, FeeInput,
};
use bitcoin::{bip32::DerivationPath, key::TapTweak, psbt::Psbt, taproot, TapSighashType};

pub fn fee_coin(fixture: &Fixture, admin: usize, index: u32, value: u64) -> FeeInput {
    fee_coin_at(
        fixture,
        admin,
        index,
        value,
        OutPoint::new(Txid::from_byte_array([200 + index as u8; 32]), index),
    )
}

pub fn fee_coin_at(
    fixture: &Fixture,
    admin: usize,
    index: u32,
    value: u64,
    outpoint: OutPoint,
) -> FeeInput {
    let secp = Secp256k1::new();
    let key = fee_key(fixture, admin, index);
    let internal = key.x_only_public_key().0;

    FeeInput::new(
        outpoint,
        &TxOut {
            value: Amount::from_sat(value),
            script_pubkey: ScriptBuf::new_p2tr(&secp, internal, None),
        },
        internal,
        fixture.fingerprint(admin),
        DerivationPath::from_str(&format!("m/86'/1'/73'/0/{index}")).unwrap(),
    )
    .unwrap()
}

pub fn fee_key(fixture: &Fixture, admin: usize, index: u32) -> Keypair {
    let secp = Secp256k1::new();
    let derived_key = fixture
        .account_xpriv(admin)
        .derive_priv(
            &secp,
            &DerivationPath::from_str(&format!("m/0/{index}")).unwrap(),
        )
        .unwrap();

    Keypair::from_secret_key(&secp, &derived_key.private_key)
}

pub fn change(fixture: &Fixture, value: u64) -> ChangeOutput {
    let secp = Secp256k1::new();
    let derived_key = fixture
        .account_xpriv(0)
        .derive_priv(&secp, &DerivationPath::from_str("m/1/0").unwrap())
        .unwrap();

    ChangeOutput::new(
        Amount::from_sat(value),
        derived_key.private_key.x_only_public_key(&secp).0,
        fixture.fingerprint(0),
        DerivationPath::from_str("m/86'/1'/73'/1/0").unwrap(),
    )
    .unwrap()
}

/// Signs every connector input as admin `admin`, the way a device does.
pub fn sign_as(fixture: &Fixture, deployment: &Deployment, psbt: &mut Psbt, admin: usize) {
    let analysis = analyze(deployment, psbt).unwrap();
    let keypair = fixture.admin_keypair(admin);
    let secp = Secp256k1::new();
    let mut added = std::collections::BTreeSet::new();
    for connector_input in analysis.connector_inputs() {
        let signature = secp.sign_schnorr(&connector_input.sighash(), &keypair);
        add_signature_with_analysis(
            deployment,
            psbt,
            &analysis,
            &mut added,
            connector_input.input_index(),
            &keypair.x_only_public_key().0,
            signature,
        )
        .unwrap();
    }
}

pub fn sign_fee(fixture: &Fixture, psbt: &mut Psbt, coin: &FeeInput, index: u32) {
    let secp = Secp256k1::new();
    let i = psbt
        .unsigned_tx
        .input
        .iter()
        .position(|input| input.previous_output == coin.outpoint())
        .unwrap();
    let prevouts: Vec<_> = psbt
        .inputs
        .iter()
        .map(|input| input.witness_utxo.clone().unwrap())
        .collect();
    let message = key_sighash(&psbt.unsigned_tx, &prevouts, i).unwrap();
    let tweaked = fee_key(fixture, 0, index).tap_tweak(&secp, None);
    let signature = secp.sign_schnorr(&message, &tweaked.to_keypair());
    psbt.inputs[i].tap_key_sig = Some(taproot::Signature {
        signature,
        sighash_type: TapSighashType::Default,
    });
}
