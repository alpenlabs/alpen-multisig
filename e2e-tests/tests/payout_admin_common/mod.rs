//! Consensus-valid game builders cut from payout-admin-reference. RPC is test funding/mining only.
use bitcoin::consensus::encode::{deserialize_hex, serialize_hex};
use bitcoin::hashes::Hash;
use bitcoin::key::{Keypair, TapTweak};
use std::str::FromStr;

use crate::payout_admin_tools::Node;
use bitcoin::secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin::taproot::{LeafVersion, TapLeafHash};
use bitcoin::{
    absolute, transaction::Version, Address, Amount, Network, OutPoint, ScriptBuf, Sequence,
    TapSighashType, Transaction, TxIn, TxOut, Witness,
};
use musig2::KeyAggContext;
use payout_admin_core::{test_support::Fixture, Deployment};
use serde_json::{json, Value};
use strata_bridge_connectors::prelude::{
    ClaimContestConnector, CounterproofConnector, KeyedAnchor,
};
use strata_bridge_connectors::Connector;

pub async fn call(node: &Node, method: &str, params: Value) -> Value {
    node.rpc(method, params)
        .await
        .unwrap_or_else(|error| panic!("{method}: {}", error))
}

pub async fn mine(node: &Node, block_count: u64) {
    let address = call(node, "getnewaddress", json!([])).await;
    call(node, "generatetoaddress", json!([block_count, address])).await;
}

/// The covenant's N/N secret: MuSig2 KeyAgg of the even-lifted operator keys, as the bridge does.
pub fn n_of_n_keypair(fixture: &Fixture, deployment: &Deployment) -> Keypair {
    let secp = Secp256k1::new();
    let covenant = deployment.covenant();
    let (mut public_keys, mut secret_keys) = (Vec::new(), Vec::new());
    for i in 0..fixture.operator_keys().len() {
        let mut secret_key = fixture.operator_keys()[i];

        if secret_key.x_only_public_key(&secp).1 == bitcoin::secp256k1::Parity::Odd {
            secret_key = secret_key.negate();
        }

        public_keys.push(PublicKey::from_secret_key(&secp, &secret_key));
        secret_keys.push(secp::Scalar::from(secret_key));
    }
    let aggregate_secret: SecretKey = KeyAggContext::new(public_keys)
        .unwrap()
        .aggregated_seckey(secret_keys)
        .unwrap();
    let keypair = Keypair::from_secret_key(&secp, &aggregate_secret);
    assert_eq!(
        keypair.x_only_public_key().0,
        covenant.n_of_n_xonly(),
        "N/N secret"
    );

    keypair
}

pub fn btc(sat: u64) -> f64 {
    Amount::from_sat(sat).to_btc()
}

pub async fn new_addr(node: &Node) -> String {
    call(node, "getnewaddress", json!(["", "bech32m"]))
        .await
        .as_str()
        .unwrap()
        .to_string()
}

/// Funds and signs `outputs` from the wallet and broadcasts. Output 2 is a wallet output of our
/// own, so the layout holds when the wallet finds coins that need no change; any change is last.
pub async fn send_claim(node: &Node, mut outputs: Vec<Value>) -> Transaction {
    outputs.insert(2, json!({new_addr(node).await: btc(10_000)}));
    let output_count = outputs.len();
    let raw = call(node, "createrawtransaction", json!([[], outputs])).await;
    let funded = call(
        node,
        "fundrawtransaction",
        json!([raw, {"changePosition": output_count}]),
    )
    .await;

    sign_send(node, funded["hex"].as_str().unwrap()).await
}

pub async fn sign_send(node: &Node, hex: &str) -> Transaction {
    let signed = call(node, "signrawtransactionwithwallet", json!([hex])).await;
    assert!(signed["complete"].as_bool().unwrap(), "{signed}");
    let hex = signed["hex"].as_str().unwrap();
    call(node, "sendrawtransaction", json!([hex])).await;

    deserialize_hex(hex).unwrap()
}

/// Shared game construction accepts either a Core wallet or the tester kit's funding adapter.
pub trait Funding {
    fn node(&self) -> &Node;

    async fn address(&self) -> String;

    async fn mine(&self, block_count: u64);

    async fn sign_send(&self, raw: &str) -> Transaction;

    async fn claim(&self, outputs: Vec<TxOut>) -> Transaction;
}

struct WalletFunding<'a>(&'a Node);

impl Funding for WalletFunding<'_> {
    fn node(&self) -> &Node {
        self.0
    }

    async fn address(&self) -> String {
        new_addr(self.0).await
    }

    async fn mine(&self, block_count: u64) {
        call(
            self.0,
            "generatetoaddress",
            json!([block_count, self.address().await]),
        )
        .await;
    }

    async fn sign_send(&self, raw: &str) -> Transaction {
        sign_send(self.0, raw).await
    }

    async fn claim(&self, outputs: Vec<TxOut>) -> Transaction {
        let outputs = outputs.iter().map(|output| json!({Address::from_script(&output.script_pubkey, Network::Regtest).unwrap().to_string(): btc(output.value.to_sat())})).collect();

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
    node: &Node,
    fixture: &Fixture,
    deployment: &Deployment,
    n_of_n: &Keypair,
) -> Game {
    claim_and_contest_funded(&WalletFunding(node), fixture, deployment, n_of_n).await
}

pub async fn claim_and_contest_funded(
    funding: &impl Funding,
    fixture: &Fixture,
    deployment: &Deployment,
    n_of_n: &Keypair,
) -> Game {
    let node = funding.node();
    let secp = Secp256k1::new();
    let network = deployment.network();
    let watchtower_keys = [fixture.operator_keys()[1], fixture.operator_keys()[2]];
    let contest_connector = ClaimContestConnector::new(
        network,
        n_of_n.x_only_public_key().0,
        watchtower_keys
            .iter()
            .map(|watchtower_secret| watchtower_secret.x_only_public_key(&secp).0)
            .collect(),
        *payout_admin_core::test_support::CONTEST_TIMELOCK,
        Amount::from_sat(200_000),
    );
    let covenant = deployment.covenant();
    let anchor = KeyedAnchor::new(network, fixture.operator_xonly(0), Amount::from_sat(330));
    let anchor_output = anchor.tx_out();
    let claim = funding
        .claim(vec![
            contest_connector.tx_out(),
            covenant.connector_tx_out(),
            anchor_output.clone(),
        ])
        .await;
    assert_eq!(claim.output[1].script_pubkey, covenant.connector_script());
    assert_eq!(claim.output[3].script_pubkey, anchor_output.script_pubkey);

    // Contest: proof, payout, slash, one per watchtower, anchor — all to the wallet.
    let mut outputs = Vec::new();
    for i in 0..6 {
        // Output 1 is the payout connector: a key-path output under the N/N key, as the ACK
        // spends it. The others go to the wallet.
        let script_pubkey = if i == 1 {
            ScriptBuf::new_p2tr(&secp, n_of_n.x_only_public_key().0, None)
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
    let leaf = contest_connector.leaf_scripts()[0].clone();
    let control_block = contest_connector
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
    let message = Message::from_digest(sighash.to_byte_array());
    let watchtower_key = Keypair::from_secret_key(&secp, &watchtower_keys[0]);
    let watchtower_signature = secp.sign_schnorr(&message, &watchtower_key);
    let n_of_n_signature = secp.sign_schnorr(&message, n_of_n);
    contest.input[0].witness = Witness::from_slice(&[
        watchtower_signature.as_ref().to_vec(),
        n_of_n_signature.as_ref().to_vec(),
        leaf.to_bytes(),
        control_block.serialize(),
    ]);
    call(node, "sendrawtransaction", json!([serialize_hex(&contest)])).await;

    Game { claim, contest }
}

/// The counterproof's ack/nack connector: an ACK spends it through the timeout leaf
/// `<N/N> OP_CHECKSIGVERIFY <delta> OP_CSV`, which is what a verifier reads off the ACK.
fn ack_connector(
    deployment: &Deployment,
    n_of_n: &Keypair,
) -> (ScriptBuf, ScriptBuf, bitcoin::taproot::ControlBlock) {
    let key = n_of_n.x_only_public_key().0;
    let connector = CounterproofConnector::new(
        deployment.network(),
        key,
        key,
        deployment.nack_timelock(),
        Amount::ZERO,
    );
    let leaf = connector.leaf_scripts().remove(0);
    let control_block = connector
        .spend_info()
        .control_block(&(leaf.clone(), LeafVersion::TapScript))
        .unwrap();

    (connector.script_pubkey(), leaf, control_block)
}

/// Mines a claim/contest, counterproof and consensus-valid ACK of that contest.
pub async fn acked_game(
    node: &Node,
    fixture: &Fixture,
    deployment: &Deployment,
    n_of_n: &Keypair,
) -> (Game, Transaction) {
    acked_game_funded(&WalletFunding(node), fixture, deployment, n_of_n).await
}

pub async fn acked_game_funded(
    funding: &impl Funding,
    fixture: &Fixture,
    deployment: &Deployment,
    n_of_n: &Keypair,
) -> (Game, Transaction) {
    let node = funding.node();
    let old = claim_and_contest_funded(funding, fixture, deployment, n_of_n).await;
    funding.mine(1).await;
    let contest_txid = old.contest.compute_txid();
    // The counterproof spends watchtower 0's output; its output 0 is the ack/nack connector.
    let (counterproof_script, counterproof_leaf, counterproof_control_block) =
        ack_connector(deployment, n_of_n);
    let counterproof_address =
        Address::from_script(&counterproof_script, Network::Regtest).unwrap();
    let raw = call(
        node,
        "createrawtransaction",
        json!([[{"txid": contest_txid.to_string(), "vout": 3}], [{counterproof_address.to_string(): btc(29_000)}]]),
    )
    .await;
    let counterproof = funding.sign_send(raw.as_str().unwrap()).await;
    let delay = deployment.nack_timelock().value();
    funding.mine(u64::from(delay).max(1)).await;
    // The ACK spends the counterproof's connector through the timeout leaf and the contest's
    // payout connector by key path, as one version 3 transaction with one taproot output.
    let secp = Secp256k1::new();
    let prevouts = [
        counterproof.output[0].clone(),
        old.contest.output[1].clone(),
    ];
    let mut ack = Transaction {
        version: Version(3),
        lock_time: absolute::LockTime::ZERO,
        input: vec![
            TxIn {
                previous_output: OutPoint::new(counterproof.compute_txid(), 0),
                sequence: Sequence::from_height(delay),
                ..Default::default()
            },
            TxIn {
                previous_output: OutPoint::new(contest_txid, 1),
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
                    TapLeafHash::from_script(&counterproof_leaf, LeafVersion::TapScript),
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
    let leaf_signature =
        secp.sign_schnorr(&Message::from_digest(leaf_sighash.to_byte_array()), n_of_n);
    let key_signature = secp.sign_schnorr(
        &Message::from_digest(key_sighash.to_byte_array()),
        &n_of_n.tap_tweak(&secp, None).to_keypair(),
    );
    ack.input[0].witness = Witness::from_slice(&[
        leaf_signature.as_ref().to_vec(),
        counterproof_leaf.to_bytes(),
        counterproof_control_block.serialize(),
    ]);
    ack.input[1].witness = Witness::from_slice(&[key_signature.as_ref().to_vec()]);
    call(node, "sendrawtransaction", json!([serialize_hex(&ack)])).await;
    // Leave the ACK young so the caller can exercise the six-confirmation gate.
    funding.mine(1).await;

    (old, ack)
}
