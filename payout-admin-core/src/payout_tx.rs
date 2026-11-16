//! Admin Block Payouts transactions.
//!
//! Shape (the bridge's `AdminBlockPayoutTx`, batched):
//!
//! - version 3 (TRUC), locktime 0;
//! - inputs: one admin payout connector per blocked claim (claim output 1, `Sequence::MAX`), then
//!   the creator's Admin Wallet fee input(s) (BIP-86 key path);
//! - outputs: exactly one P2TR change output to the creator's Admin Wallet.
//!
//! Every signature is `SIGHASH_DEFAULT`, so each one commits to the whole transaction, and the txid
//! never changes as signatures are added (they live in the witness).
//!
//! Pending transactions travel as PSBTs (base64): that is the "raw copy including signatures" that
//! admins import and export, and what the coordinator stores. Admin signatures can also be copied
//! as text, one per line: `<txid>:<input index>:<admin x-only key>:<64-byte signature hex>`.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use bitcoin::{
    absolute,
    bip32::{DerivationPath, Fingerprint},
    hashes::Hash,
    key::{Secp256k1, TapTweak},
    psbt::Psbt,
    secp256k1::{schnorr, Message, XOnlyPublicKey},
    sighash::{Prevouts, SighashCache},
    taproot::{self, LeafVersion, TapLeafHash},
    transaction::Version,
    Amount, OutPoint, ScriptBuf, Sequence, TapSighashType, Transaction, TxIn, TxOut, Txid, Witness,
};
use serde::Serialize;
use strata_bridge_connectors::{
    prelude::{AdminSignature, ClaimAdminPayoutWitness},
    Connector,
};

use crate::deployment::{Covenant, Deployment};
use crate::report::CLAIM_ADMIN_PAYOUT_VOUT;

pub const TX_VERSION: Version = Version(3);
/// Bitcoin Core policy: BIP-431 TRUC transactions are limited to 10,000 vB.
pub const TRUC_MAX_VSIZE: u64 = 10_000;
/// P2TR dust limit at Bitcoin Core's default dust relay fee.
pub const P2TR_DUST_SAT: u64 = 330;
/// Ledger Bitcoin app limit on inputs it can sign in one transaction.
pub const LEDGER_MAX_INPUTS: usize = 512;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PayoutTxError {
    #[error("{0}")]
    Shape(String),
    #[error("not enough signatures: {0}")]
    NotEnoughSignatures(String),
    #[error("invalid PSBT: {0}")]
    Psbt(String),
}

fn shape<T>(m: impl Into<String>) -> Result<T, PayoutTxError> {
    Err(PayoutTxError::Shape(m.into()))
}

/// An Admin Wallet coin used to pay the fee.
#[derive(Debug, Clone)]
pub struct FeeInput {
    pub outpoint: OutPoint,
    pub txout: TxOut,
    /// Untweaked BIP-86 key.
    pub internal_key: XOnlyPublicKey,
    pub fingerprint: Fingerprint,
    /// Full path, e.g. m/86'/1'/73'/0/4.
    pub path: DerivationPath,
}

impl FeeInput {
    pub(crate) fn fill_input(&self, input: &mut bitcoin::psbt::Input) {
        input.witness_utxo = Some(self.txout.clone());
        input.tap_internal_key = Some(self.internal_key);
        input.tap_key_origins.insert(
            self.internal_key,
            (vec![], (self.fingerprint, self.path.clone())),
        );
    }
}

/// The Admin Wallet change output.
#[derive(Debug, Clone)]
pub struct ChangeOutput {
    pub value: Amount,
    pub internal_key: XOnlyPublicKey,
    pub fingerprint: Fingerprint,
    pub path: DerivationPath,
}

impl ChangeOutput {
    pub fn script_pubkey(&self) -> ScriptBuf {
        ScriptBuf::new_p2tr(&Secp256k1::verification_only(), self.internal_key, None)
    }

    pub(crate) fn fill_output(&self, output: &mut bitcoin::psbt::Output) {
        output.tap_internal_key = Some(self.internal_key);
        output.tap_key_origins.insert(
            self.internal_key,
            (vec![], (self.fingerprint, self.path.clone())),
        );
    }
}

/// A connector to spend: the claim's admin payout output and the covenant it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectorRef {
    pub claim_txid: Txid,
    pub covenant_index: usize,
}

impl ConnectorRef {
    pub fn outpoint(&self) -> OutPoint {
        OutPoint::new(self.claim_txid, CLAIM_ADMIN_PAYOUT_VOUT)
    }
}

fn leaf_of(cov: &Covenant) -> (ScriptBuf, TapLeafHash, taproot::ControlBlock) {
    let leaf = cov.connector.leaf_scripts().remove(0);
    let hash = TapLeafHash::from_script(&leaf, LeafVersion::TapScript);
    let cb = cov
        .connector
        .spend_info()
        .control_block(&(leaf.clone(), LeafVersion::TapScript))
        .expect("connector has its leaf");
    (leaf, hash, cb)
}

/// Writes the fields a signer needs on a connector input, derived from the trusted deployment
/// (never taken from a PSBT someone else built): witness UTXO, internal key, merkle root, the leaf,
/// and every admin key's origin. Admin origins need the roster; without it only the script data is
/// written.
fn fill_connector_input(dep: &Deployment, cov: &Covenant, input: &mut bitcoin::psbt::Input) {
    let (leaf, leaf_hash, cb) = leaf_of(cov);
    input.witness_utxo = Some(cov.connector_tx_out());
    input.tap_internal_key = Some(cov.connector.internal_key());
    input.tap_merkle_root = cov.connector.spend_info().merkle_root();
    input.tap_scripts = BTreeMap::from([(cb, (leaf, LeafVersion::TapScript))]);
    input.tap_key_origins.clear();
    input.bip32_derivation.clear();
    if let Some(signers) = &dep.signers {
        for s in signers {
            input.tap_key_origins.insert(
                s.xonly,
                (vec![leaf_hash], (s.fingerprint, s.key_path(dep.network))),
            );
        }
    }
    let synth = cov.synthetic_xpub(dep.network);
    input.tap_key_origins.insert(
        cov.connector.internal_key(),
        (
            vec![],
            (
                synth.fingerprint(),
                DerivationPath::from_str("m/0/0").expect("static"),
            ),
        ),
    );
}

fn bip86_script(key: XOnlyPublicKey) -> ScriptBuf {
    ScriptBuf::new_p2tr(&Secp256k1::verification_only(), key, None)
}

/// Builds the unsigned PSBT.
pub fn build_psbt(
    dep: &Deployment,
    connectors: &[ConnectorRef],
    fees: &[FeeInput],
    change: &ChangeOutput,
) -> Result<Psbt, PayoutTxError> {
    if connectors.is_empty() {
        return shape("a transaction needs at least one claim input");
    }
    if connectors.len() + fees.len() > LEDGER_MAX_INPUTS {
        return shape(format!(
            "a Ledger can sign at most {LEDGER_MAX_INPUTS} inputs"
        ));
    }
    if change.value.to_sat() < P2TR_DUST_SAT {
        return shape("the change output is below the dust limit");
    }
    let mut seen = BTreeSet::new();
    let mut inputs = Vec::new();
    for c in connectors {
        if dep.covenants.get(c.covenant_index).is_none() {
            return shape(format!("unknown covenant {}", c.covenant_index));
        }
        if !seen.insert(c.outpoint()) {
            return shape(format!("claim {} is listed twice", c.claim_txid));
        }
        inputs.push(c.outpoint());
    }
    for f in fees {
        if !seen.insert(f.outpoint) {
            return shape(format!("fee coin {} is listed twice", f.outpoint));
        }
        if f.txout.script_pubkey != bip86_script(f.internal_key) {
            return shape(format!(
                "fee coin {} is not the BIP-86 output of its key",
                f.outpoint
            ));
        }
        inputs.push(f.outpoint);
    }
    let tx = Transaction {
        version: TX_VERSION,
        lock_time: absolute::LockTime::ZERO,
        input: inputs
            .into_iter()
            .map(|previous_output| TxIn {
                previous_output,
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            })
            .collect(),
        output: vec![TxOut {
            value: change.value,
            script_pubkey: change.script_pubkey(),
        }],
    };
    let mut psbt = Psbt::from_unsigned_tx(tx).map_err(|e| PayoutTxError::Psbt(e.to_string()))?;
    for (i, c) in connectors.iter().enumerate() {
        fill_connector_input(dep, &dep.covenants[c.covenant_index], &mut psbt.inputs[i]);
    }
    for (input, fee) in psbt.inputs[connectors.len()..].iter_mut().zip(fees) {
        fee.fill_input(input);
    }
    change.fill_output(&mut psbt.outputs[0]);
    Ok(psbt)
}

// ── Inspection ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorInputInfo {
    pub index: usize,
    #[serde(serialize_with = "ser_display")]
    pub outpoint: OutPoint,
    pub covenant_index: usize,
    /// Admin indices with a valid signature on this input.
    pub signed_by: Vec<usize>,
    #[serde(skip)]
    pub sighash: Message,
    #[serde(skip)]
    pub leaf_hash: TapLeafHash,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeeInputInfo {
    pub index: usize,
    #[serde(serialize_with = "ser_display")]
    pub outpoint: OutPoint,
    pub value_sat: u64,
    pub signed: bool,
    #[serde(skip)]
    pub sighash: Message,
    #[serde(skip)]
    pub output_key: XOnlyPublicKey,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    #[serde(serialize_with = "ser_display")]
    pub txid: Txid,
    pub connector_inputs: Vec<ConnectorInputInfo>,
    pub fee_inputs: Vec<FeeInputInfo>,
    pub change_sat: u64,
    pub change_script_hex: String,
    pub input_total_sat: u64,
    pub fee_sat: u64,
    /// Size once fully signed (exact: every witness element has a fixed size).
    pub vsize: u64,
    /// Admin indices that signed every connector input.
    pub signers: Vec<usize>,
    pub signatures_received: usize,
    pub signatures_needed: usize,
    /// Enough admin signatures on every connector input.
    pub has_quorum: bool,
    /// Quorum and every fee input signed: ready to finalize and broadcast.
    pub complete: bool,
}

fn ser_display<T: std::fmt::Display, S: serde::Serializer>(v: &T, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(v)
}

impl Analysis {
    pub fn outpoints(&self) -> Vec<OutPoint> {
        self.connector_inputs.iter().map(|i| i.outpoint).collect()
    }

    pub fn signed_by(&self, admin_index: usize) -> bool {
        self.signers.contains(&admin_index)
    }
}

impl ConnectorInputInfo {
    fn signatures<'a>(
        &'a self,
        dep: &'a Deployment,
        psbt: &'a Psbt,
    ) -> impl Iterator<Item = (usize, (XOnlyPublicKey, TapLeafHash), taproot::Signature)> + 'a {
        self.signed_by.iter().map(move |&admin| {
            let key = (dep.admin_keys[admin], self.leaf_hash);
            (admin, key, psbt.inputs[self.index].tap_script_sigs[&key])
        })
    }
}

impl FeeInputInfo {
    fn verifies(&self, signature: Option<taproot::Signature>) -> bool {
        signature.is_some_and(|sig| {
            sig.sighash_type == TapSighashType::Default
                && verify_schnorr(&self.sighash, &sig.signature, &self.output_key)
        })
    }
}

fn prevouts(psbt: &Psbt) -> Result<Vec<TxOut>, PayoutTxError> {
    psbt.inputs
        .iter()
        .enumerate()
        .map(|(i, inp)| {
            inp.witness_utxo
                .clone()
                .ok_or_else(|| PayoutTxError::Shape(format!("input {i} has no witness UTXO")))
        })
        .collect()
}

/// One cache per transaction: it hashes the prevouts once, so every input's sighash is O(1) after
/// the first instead of O(inputs).
fn script_sighash(
    cache: &mut SighashCache<&Transaction>,
    prevouts: &[TxOut],
    index: usize,
    leaf_hash: TapLeafHash,
) -> Result<Message, PayoutTxError> {
    let h = cache
        .taproot_script_spend_signature_hash(
            index,
            &Prevouts::All(prevouts),
            leaf_hash,
            TapSighashType::Default,
        )
        .map_err(|e| PayoutTxError::Psbt(e.to_string()))?;
    Ok(Message::from_digest(h.to_byte_array()))
}

/// BIP-341 key-path sighash (SIGHASH_DEFAULT) of input `index`.
pub fn key_sighash(
    tx: &Transaction,
    prevouts: &[TxOut],
    index: usize,
) -> Result<Message, PayoutTxError> {
    key_sighash_in(&mut SighashCache::new(tx), prevouts, index)
}

fn key_sighash_in(
    cache: &mut SighashCache<&Transaction>,
    prevouts: &[TxOut],
    index: usize,
) -> Result<Message, PayoutTxError> {
    let h = cache
        .taproot_key_spend_signature_hash(index, &Prevouts::All(prevouts), TapSighashType::Default)
        .map_err(|e| PayoutTxError::Psbt(e.to_string()))?;
    Ok(Message::from_digest(h.to_byte_array()))
}

fn verify_schnorr(msg: &Message, sig: &schnorr::Signature, key: &XOnlyPublicKey) -> bool {
    Secp256k1::verification_only()
        .verify_schnorr(sig, msg, key)
        .is_ok()
}

/// Checks the shape of a PSBT and reports which valid signatures it carries. Invalid signatures
/// are not counted (and [`normalize`] removes them).
pub fn analyze(dep: &Deployment, psbt: &Psbt) -> Result<Analysis, PayoutTxError> {
    let tx = &psbt.unsigned_tx;
    if tx.version != TX_VERSION {
        return shape("not a version 3 transaction");
    }
    if tx.lock_time != absolute::LockTime::ZERO {
        return shape("locktime must be 0");
    }
    if tx.output.len() != 1 {
        return shape("must have exactly one output (the Admin Wallet change)");
    }
    let change = &tx.output[0];
    if !change.script_pubkey.is_p2tr() || change.value.to_sat() < P2TR_DUST_SAT {
        return shape("the change output must be a taproot output above the dust limit");
    }
    // Every input is a native taproot spend. A scriptSig carried in the PSBT is outside what the
    // signatures commit to, and the finalized transaction would take it over.
    if psbt
        .inputs
        .iter()
        .any(|i| i.final_script_sig.as_ref().is_some_and(|s| !s.is_empty()))
    {
        return shape("an input carries a scriptSig; taproot inputs have none");
    }
    let prevouts = prevouts(psbt)?;
    let mut cache = SighashCache::new(tx);
    let mut leaves = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut connector_inputs = Vec::new();
    let mut fee_inputs = Vec::new();
    let mut input_total = Amount::ZERO;
    for (i, (txin, prev)) in tx.input.iter().zip(&prevouts).enumerate() {
        if !seen.insert(txin.previous_output) {
            return shape("an input is spent twice");
        }
        if txin.sequence != Sequence::MAX {
            return shape(format!("input {i} must use sequence 0xffffffff"));
        }
        input_total = input_total
            .checked_add(prev.value)
            .ok_or_else(|| PayoutTxError::Shape("input total overflows".into()))?;
        if let Some(cov) = dep.covenant_for_script(&prev.script_pubkey) {
            if txin.previous_output.vout != CLAIM_ADMIN_PAYOUT_VOUT {
                return shape(format!(
                    "input {i} is a connector but not a claim's output 1"
                ));
            }
            if !fee_inputs.is_empty() {
                return shape("claim inputs must come before the fee inputs");
            }
            if prev.value != cov.connector.value() {
                return shape(format!("input {i} has the wrong connector value"));
            }
            let leaf_hash = *leaves.entry(cov.index).or_insert_with(|| leaf_of(cov).1);
            let sighash = script_sighash(&mut cache, &prevouts, i, leaf_hash)?;
            let signed_by = (0..dep.admin_keys.len())
                .filter(|&a| {
                    psbt.inputs[i]
                        .tap_script_sigs
                        .get(&(dep.admin_keys[a], leaf_hash))
                        .is_some_and(|s| {
                            s.sighash_type == TapSighashType::Default
                                && verify_schnorr(&sighash, &s.signature, &dep.admin_keys[a])
                        })
                })
                .collect();
            connector_inputs.push(ConnectorInputInfo {
                index: i,
                outpoint: txin.previous_output,
                covenant_index: cov.index,
                signed_by,
                sighash,
                leaf_hash,
            });
        } else {
            let internal = psbt.inputs[i].tap_internal_key.ok_or_else(|| {
                PayoutTxError::Shape(format!("fee input {i} has no taproot internal key"))
            })?;
            if prev.script_pubkey != bip86_script(internal) {
                return shape(format!("fee input {i} is not a BIP-86 key-path output"));
            }
            let mut fee = FeeInputInfo {
                index: i,
                outpoint: txin.previous_output,
                value_sat: prev.value.to_sat(),
                signed: false,
                sighash: key_sighash_in(&mut cache, &prevouts, i)?,
                output_key: internal
                    .tap_tweak(&Secp256k1::verification_only(), None)
                    .0
                    .to_x_only_public_key(),
            };
            fee.signed = fee.verifies(psbt.inputs[i].tap_key_sig);
            fee_inputs.push(fee);
        }
    }
    if connector_inputs.is_empty() {
        return shape("no input is an admin payout connector of this deployment");
    }
    let output_total = change.value;
    let fee = input_total
        .checked_sub(output_total)
        .ok_or_else(|| PayoutTxError::Shape("the outputs exceed the inputs".into()))?;
    let signers: Vec<usize> = (0..dep.admin_keys.len())
        .filter(|a| connector_inputs.iter().all(|c| c.signed_by.contains(a)))
        .collect();
    let has_quorum = connector_inputs
        .iter()
        .all(|c| c.signed_by.len() >= dep.threshold);
    let complete = has_quorum && fee_inputs.iter().all(|f| f.signed);
    let vsize = final_vsize(
        dep,
        connector_inputs.len(),
        fee_inputs.len(),
        &change.script_pubkey,
    );
    // The planner never builds a larger one, but a transaction can also arrive from elsewhere;
    // Bitcoin Core would refuse it only after every admin has signed.
    if vsize > TRUC_MAX_VSIZE {
        return shape(format!(
            "the signed transaction would be {vsize} vB, over the {TRUC_MAX_VSIZE} vB limit of a version 3 transaction"
        ));
    }
    Ok(Analysis {
        txid: tx.compute_txid(),
        signatures_received: signers.len(),
        signatures_needed: dep.threshold,
        signers,
        has_quorum,
        complete,
        connector_inputs,
        fee_inputs,
        change_sat: change.value.to_sat(),
        change_script_hex: hex::encode(change.script_pubkey.as_bytes()),
        input_total_sat: input_total.to_sat(),
        fee_sat: fee.to_sat(),
        vsize,
    })
}

/// Rewrites every connector input's signing data from the trusted deployment and drops signatures
/// that do not verify. Use on any PSBT that came from elsewhere before showing or signing it.
pub fn normalize(dep: &Deployment, psbt: &mut Psbt) -> Result<Analysis, PayoutTxError> {
    let a = analyze(dep, psbt)?;
    for c in &a.connector_inputs {
        let cov = &dep.covenants[c.covenant_index];
        let keep = c
            .signatures(dep, psbt)
            .map(|(_, key, sig)| (key, sig))
            .collect();
        let input = &mut psbt.inputs[c.index];
        let keep_origins = input.tap_key_origins.clone();
        fill_connector_input(dep, cov, input);
        if dep.signers.is_none() {
            // Keep the origins the creator wrote when this app has no roster to rebuild them,
            // but only for keys of this deployment.
            for (k, v) in keep_origins {
                if dep.admin_index(&k).is_some() {
                    input.tap_key_origins.insert(k, v);
                }
            }
        }
        input.tap_script_sigs = keep;
        input.partial_sigs.clear();
        input.final_script_witness = None;
    }
    for f in &a.fee_inputs {
        if !f.signed {
            psbt.inputs[f.index].tap_key_sig = None;
        }
        psbt.inputs[f.index].final_script_witness = None;
    }
    Ok(a)
}

/// Adds one admin signature after verifying it. Returns `Ok(false)` when it was already present.
pub fn add_admin_signature(
    dep: &Deployment,
    psbt: &mut Psbt,
    input_index: usize,
    admin: &XOnlyPublicKey,
    sig: schnorr::Signature,
) -> Result<bool, String> {
    let a = analyze(dep, psbt).map_err(|e| e.to_string())?;
    add_signature_in(dep, psbt, &a, &mut BTreeSet::new(), input_index, admin, sig)
}

/// [`add_admin_signature`] for many signatures of one transaction: `a` is the analysis taken
/// before the first one (each analysis hashes every input, so redoing it per signature is
/// quadratic) and `added` remembers the (input, admin) pairs this batch already filled.
pub(crate) fn add_signature_in(
    dep: &Deployment,
    psbt: &mut Psbt,
    a: &Analysis,
    added: &mut BTreeSet<(usize, XOnlyPublicKey)>,
    input_index: usize,
    admin: &XOnlyPublicKey,
    sig: schnorr::Signature,
) -> Result<bool, String> {
    let admin_idx = dep
        .admin_index(admin)
        .ok_or("the key is not in the admin set")?;
    let c = a
        .connector_inputs
        .iter()
        .find(|c| c.index == input_index)
        .ok_or("the input is not a claim input of this transaction")?;
    if !verify_schnorr(&c.sighash, &sig, admin) {
        return Err("the signature does not verify".into());
    }
    if c.signed_by.contains(&admin_idx) || added.contains(&(input_index, *admin)) {
        return Ok(false);
    }
    psbt.inputs[input_index].tap_script_sigs.insert(
        (*admin, c.leaf_hash),
        taproot::Signature {
            signature: sig,
            sighash_type: TapSighashType::Default,
        },
    );
    added.insert((input_index, *admin));
    Ok(true)
}

// ── Signature text ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureLine {
    pub txid: String,
    pub input_index: usize,
    pub admin_key: String,
    pub signature: String,
}

impl std::fmt::Display for SignatureLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}:{}",
            self.txid, self.input_index, self.admin_key, self.signature
        )
    }
}

/// All valid admin signatures in the transaction, one line each (input order, then admin order).
pub fn export_signatures(
    dep: &Deployment,
    psbt: &Psbt,
) -> Result<Vec<SignatureLine>, PayoutTxError> {
    let a = analyze(dep, psbt)?;
    let mut out = Vec::new();
    for c in &a.connector_inputs {
        for (_, (key, _), sig) in c.signatures(dep, psbt) {
            out.push(SignatureLine {
                txid: a.txid.to_string(),
                input_index: c.index,
                admin_key: key.to_string(),
                signature: hex::encode(sig.signature.serialize()),
            });
        }
    }
    Ok(out)
}

pub fn signatures_text(lines: &[SignatureLine]) -> String {
    lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone, Default, serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvalidSignature {
    pub raw: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteOutcome {
    pub added: Vec<SignatureLine>,
    pub already_present: usize,
    pub invalid: Vec<InvalidSignature>,
}

/// User-facing text for invalid pasted signatures: the sentence, then the invalid ones, one per
/// line.
pub fn invalid_signatures_message(invalid: &[InvalidSignature]) -> Option<String> {
    match invalid.len() {
        0 => None,
        1 => Some(format!(
            "Warning: Invalid signature. Please provide a valid signature.\n{}",
            invalid[0].raw
        )),
        _ => Some(format!(
            "Warning: Invalid signatures. Please provide valid signatures.\n{}",
            invalid
                .iter()
                .map(|i| i.raw.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        )),
    }
}

fn parse_line(
    raw: &str,
    txid: &Txid,
) -> Result<(usize, XOnlyPublicKey, schnorr::Signature), String> {
    let parts: Vec<&str> = raw.split(':').collect();
    let [t, idx, key, sig] = parts.as_slice() else {
        return Err("expected <txid>:<input>:<admin key>:<signature>".into());
    };
    let t = Txid::from_str(t).map_err(|_| "malformed transaction id")?;
    if &t != txid {
        return Err("the signature is for a different transaction".into());
    }
    let idx: usize = idx.parse().map_err(|_| "malformed input index")?;
    let key = XOnlyPublicKey::from_str(key).map_err(|_| "malformed admin key")?;
    let bytes = hex::decode(sig).map_err(|_| "the signature is not hex")?;
    if bytes.len() != 64 {
        return Err("the signature must be 64 bytes (SIGHASH_DEFAULT)".into());
    }
    let sig = schnorr::Signature::from_slice(&bytes).map_err(|_| "malformed signature")?;
    Ok((idx, key, sig))
}

/// Verifies every pasted signature; only valid ones are added to `psbt`.
pub fn paste_signatures(dep: &Deployment, psbt: &mut Psbt, text: &str) -> PasteOutcome {
    let txid = psbt.unsigned_tx.compute_txid();
    let mut out = PasteOutcome::default();
    let analysis = analyze(dep, psbt).map_err(|e| e.to_string());
    let mut filled = BTreeSet::new();
    for raw in text
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .filter(|s| !s.is_empty())
    {
        let res = parse_line(raw, &txid).and_then(|(idx, key, sig)| {
            let a = analysis.as_ref().map_err(Clone::clone)?;
            add_signature_in(dep, psbt, a, &mut filled, idx, &key, sig)
                .map(|added| (added, idx, key, sig))
        });
        match res {
            Ok((true, idx, key, sig)) => out.added.push(SignatureLine {
                txid: txid.to_string(),
                input_index: idx,
                admin_key: key.to_string(),
                signature: hex::encode(sig.serialize()),
            }),
            Ok((false, ..)) => out.already_present += 1,
            Err(reason) => out.invalid.push(InvalidSignature {
                raw: raw.to_string(),
                reason,
            }),
        }
    }
    out
}

/// Copies the admin signatures and fee input signatures of `other` that are valid for `base` into
/// `base`. Both must be the same transaction.
pub fn merge(dep: &Deployment, base: &mut Psbt, other: &Psbt) -> Result<usize, PayoutTxError> {
    if base.unsigned_tx != other.unsigned_tx {
        return shape("the two PSBTs are different transactions");
    }
    let theirs = analyze(dep, other)?;
    let ours = analyze(dep, base)?;
    let mut filled = BTreeSet::new();
    let mut added = 0;
    for c in &theirs.connector_inputs {
        for (_, (key, _), sig) in c.signatures(dep, other) {
            if add_signature_in(dep, base, &ours, &mut filled, c.index, &key, sig.signature)
                == Ok(true)
            {
                added += 1;
            }
        }
    }
    // A fee signature counts only if it verifies against `base`'s own prevouts and key: `other`
    // may describe the fee input differently, and its signature is then valid only there. A slot
    // holding a signature that does not verify is free.
    for f in ours.fee_inputs.iter().filter(|f| !f.signed) {
        let sig = other.inputs[f.index].tap_key_sig;
        if f.verifies(sig) {
            base.inputs[f.index].tap_key_sig = sig;
            added += 1;
        }
    }
    Ok(added)
}

// ── Finalizing ───────────────────────────────────────────────────────────────

/// Builds the final transaction: each connector witness through the bridge's own
/// `ClaimAdminPayoutConnector::finalize_input`, each fee input as a key-path spend.
pub fn finalize(dep: &Deployment, psbt: &Psbt) -> Result<Transaction, PayoutTxError> {
    let a = analyze(dep, psbt)?;
    if !a.has_quorum {
        return Err(PayoutTxError::NotEnoughSignatures(format!(
            "{} of {} admins signed every input",
            a.signatures_received, a.signatures_needed
        )));
    }
    if let Some(f) = a.fee_inputs.iter().find(|f| !f.signed) {
        return Err(PayoutTxError::NotEnoughSignatures(format!(
            "fee input {} is not signed",
            f.outpoint
        )));
    }
    let mut p = psbt.clone();
    for c in &a.connector_inputs {
        let cov = &dep.covenants[c.covenant_index];
        let admin_signatures = c
            .signatures(dep, &p)
            .map(|(pubkey_index, _, sig)| AdminSignature {
                pubkey_index,
                signature: sig.signature,
            })
            .collect();
        cov.connector.finalize_input(
            &mut p.inputs[c.index],
            &ClaimAdminPayoutWitness::AdminBlockPayout { admin_signatures },
        );
    }
    for f in &a.fee_inputs {
        let sig = p.inputs[f.index].tap_key_sig.expect("checked above");
        p.inputs[f.index].final_script_witness = Some(Witness::p2tr_key_spend(&sig));
    }
    let tx = p.extract_tx_unchecked_fee_rate();
    Ok(tx)
}

// ── Size ─────────────────────────────────────────────────────────────────────

/// vsize of the fully signed transaction. Exact: signatures are 64 bytes (SIGHASH_DEFAULT), admins
/// that do not sign leave an empty push, and the control block has no merkle path (one leaf).
pub fn final_vsize(
    dep: &Deployment,
    n_connectors: usize,
    n_fee_inputs: usize,
    change: &ScriptBuf,
) -> u64 {
    final_weight(dep, n_connectors, n_fee_inputs, change).div_ceil(4)
}

pub fn final_weight(
    dep: &Deployment,
    n_connectors: usize,
    n_fee_inputs: usize,
    change: &ScriptBuf,
) -> u64 {
    let cov = &dep.covenants[0];
    let (leaf, _, cb) = leaf_of(cov);
    let mut connector_witness: Vec<Vec<u8>> = (0..dep.admin_keys.len())
        .map(|i| {
            if i < dep.threshold {
                vec![0u8; 64]
            } else {
                vec![]
            }
        })
        .collect();
    connector_witness.push(leaf.to_bytes());
    connector_witness.push(cb.serialize());
    let connector_witness = Witness::from_slice(&connector_witness);
    let fee_witness = Witness::from_slice(&[vec![0u8; 64]]);
    let input = |w: &Witness| TxIn {
        previous_output: OutPoint::null(),
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: w.clone(),
    };
    let tx = Transaction {
        version: TX_VERSION,
        lock_time: absolute::LockTime::ZERO,
        input: std::iter::repeat_n(input(&connector_witness), n_connectors)
            .chain(std::iter::repeat_n(input(&fee_witness), n_fee_inputs))
            .collect(),
        output: vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: change.clone(),
        }],
    };
    tx.weight().to_wu()
}

/// A typical P2TR change script, for size estimates before the change address is known.
pub fn p2tr_placeholder_script() -> ScriptBuf {
    ScriptBuf::from_bytes([&[0x51, 0x20][..], &[0x02; 32][..]].concat())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    fn two_claims(fx: &Fixture, dep: &Deployment) -> Vec<ConnectorRef> {
        (0..2)
            .map(|v| ConnectorRef {
                claim_txid: fx.game(dep, v).report.new_claim_txid,
                covenant_index: 0,
            })
            .collect()
    }

    /// The largest planner transaction (about 90 claims) signs, merges and finalizes in seconds,
    /// not minutes: each step analyzes the transaction once, with one sighash cache.
    #[test]
    fn ninety_claims_sign_merge_and_finalize() {
        use std::time::Instant;
        let fx = Fixture::new(3, 2);
        let dep = fx.deployment();
        let coin = fee_coin(&fx, 0, 4, 500_000);
        let claims: Vec<ConnectorRef> = (0..90u32)
            .map(|i| {
                let mut b = [0u8; 32];
                b[..4].copy_from_slice(&i.to_le_bytes());
                ConnectorRef {
                    claim_txid: bitcoin::Txid::from_byte_array(b),
                    covenant_index: 0,
                }
            })
            .collect();
        let base = build_psbt(
            &dep,
            &claims,
            std::slice::from_ref(&coin),
            &change(&fx, 300_000),
        )
        .unwrap();
        let t = Instant::now();
        let mut a0 = base.clone();
        sign_as(&fx, &dep, &mut a0, 0);
        println!("sign_as {:?}", t.elapsed());
        let mut a1 = base.clone();
        sign_as(&fx, &dep, &mut a1, 1);
        sign_fee(&fx, &mut a1, &coin, 4);
        let t = Instant::now();
        let n = merge(&dep, &mut a0, &a1).unwrap();
        println!("merge {:?}", t.elapsed());
        assert_eq!(n, 90 + 1);
        let t = Instant::now();
        let tx = finalize(&dep, &a0).unwrap();
        println!("finalize {:?}", t.elapsed());
        assert!(tx.vsize() as u64 <= TRUC_MAX_VSIZE);
    }

    #[test]
    fn build_sign_finalize() {
        let fx = Fixture::new(3, 2);
        let dep = fx.deployment();
        let coin = fee_coin(&fx, 0, 4, 50_000);
        let claims = two_claims(&fx, &dep);
        let mut psbt = build_psbt(
            &dep,
            &claims,
            std::slice::from_ref(&coin),
            &change(&fx, 49_000),
        )
        .unwrap();
        let a = analyze(&dep, &psbt).unwrap();
        assert_eq!(a.connector_inputs.len(), 2);
        assert_eq!(a.fee_sat, 50_000 + 660 - 49_000);
        assert_eq!((a.signatures_received, a.signatures_needed), (0, 2));
        assert!(!a.has_quorum);
        let txid = a.txid;

        sign_as(&fx, &dep, &mut psbt, 0);
        sign_fee(&fx, &mut psbt, &coin, 4);
        let a = analyze(&dep, &psbt).unwrap();
        assert_eq!(a.signers, vec![0]);
        assert!(a.fee_inputs[0].signed);
        assert!(finalize(&dep, &psbt).is_err());

        sign_as(&fx, &dep, &mut psbt, 2);
        let a = analyze(&dep, &psbt).unwrap();
        assert_eq!(a.signers, vec![0, 2]);
        assert!(a.complete);
        let tx = finalize(&dep, &psbt).unwrap();
        assert_eq!(tx.compute_txid(), txid, "signatures do not change the txid");
        // The size model is exact.
        assert_eq!(tx.vsize() as u64, a.vsize);
        // Witness layout: one slot per admin in reverse order; admin 1 did not sign.
        let w: Vec<&[u8]> = tx.input[0].witness.iter().collect();
        assert_eq!(w.len(), 3 + 2);
        assert_eq!(w[0].len(), 64); // admin 2
        assert!(w[1].is_empty()); // admin 1
        assert_eq!(w[2].len(), 64); // admin 0
                                    // Each connector witness verifies against the script (sanity: through bitcoinconsensus is
                                    // covered by the regtest suite in the desktop crate).
    }

    #[test]
    fn signature_lines_round_trip_and_reject_bad_ones() {
        let fx = Fixture::new(3, 2);
        let dep = fx.deployment();
        let coin = fee_coin(&fx, 0, 1, 20_000);
        let base = build_psbt(&dep, &two_claims(&fx, &dep), &[coin], &change(&fx, 19_000)).unwrap();
        let mut signed = base.clone();
        sign_as(&fx, &dep, &mut signed, 1);
        let lines = export_signatures(&dep, &signed).unwrap();
        assert_eq!(lines.len(), 2);
        let text = signatures_text(&lines);

        let mut target = base.clone();
        let bad_sig = format!(
            "{}:0:{}:{}",
            lines[0].txid,
            lines[0].admin_key,
            "00".repeat(64)
        );
        let wrong_tx = format!(
            "{}:0:{}:{}",
            "11".repeat(32),
            lines[0].admin_key,
            lines[0].signature
        );
        let other_admin = format!(
            "{}:0:{}:{}",
            lines[0].txid,
            fx.admin_xonly(0),
            lines[0].signature
        );
        let pasted = format!("{text}\n{bad_sig}\n{wrong_tx}\n{other_admin}\njunk");
        let out = paste_signatures(&dep, &mut target, &pasted);
        assert_eq!(out.added.len(), 2);
        assert_eq!(out.invalid.len(), 4);
        let msg = invalid_signatures_message(&out.invalid).unwrap();
        assert!(msg.starts_with("Warning: Invalid signatures."));
        assert_eq!(analyze(&dep, &target).unwrap().signers, vec![1]);
        // Pasting again adds nothing.
        let again = paste_signatures(&dep, &mut target, &text);
        assert_eq!((again.added.len(), again.already_present), (0, 2));
    }

    #[test]
    fn merge_and_normalize() {
        let fx = Fixture::new(3, 2);
        let dep = fx.deployment();
        let coin = fee_coin(&fx, 0, 1, 20_000);
        let base = build_psbt(&dep, &two_claims(&fx, &dep), &[coin], &change(&fx, 19_000)).unwrap();
        let mut a = base.clone();
        let mut b = base.clone();
        sign_as(&fx, &dep, &mut a, 0);
        sign_as(&fx, &dep, &mut b, 1);
        assert_eq!(merge(&dep, &mut a, &b).unwrap(), 2);
        assert_eq!(analyze(&dep, &a).unwrap().signers, vec![0, 1]);

        // A tampered PSBT: a forged signature and a foreign leaf are dropped / overwritten.
        let mut t = a.clone();
        let leaf_hash = *t.inputs[0]
            .tap_script_sigs
            .keys()
            .next()
            .map(|(_, h)| h)
            .unwrap();
        t.inputs[0].tap_script_sigs.insert(
            (fx.admin_xonly(2), leaf_hash),
            taproot::Signature {
                signature: schnorr::Signature::from_slice(&[7u8; 64]).unwrap(),
                sighash_type: TapSighashType::Default,
            },
        );
        t.inputs[0].tap_scripts.clear();
        let an = normalize(&dep, &mut t).unwrap();
        // Reusing the pre-normalization analysis must describe exactly the rewritten PSBT.
        assert_eq!(
            serde_json::to_value(&an).unwrap(),
            serde_json::to_value(analyze(&dep, &t).unwrap()).unwrap()
        );
        assert_eq!(an.connector_inputs[0].signed_by, vec![0, 1]);
        assert_eq!(t.inputs[0].tap_scripts.len(), 1);
        assert_eq!(t.inputs[0].tap_script_sigs.len(), 2);
    }

    #[test]
    fn a_fee_signature_is_merged_only_if_it_is_valid_for_the_base() {
        let fx = Fixture::new(3, 2);
        let dep = fx.deployment();
        let coin = fee_coin(&fx, 0, 1, 20_000);
        let base = build_psbt(
            &dep,
            &two_claims(&fx, &dep),
            std::slice::from_ref(&coin),
            &change(&fx, 19_000),
        )
        .unwrap();
        let fee_index = analyze(&dep, &base).unwrap().fee_inputs[0].index;

        // The same transaction, but describing the fee coin with another amount: the signature is
        // valid for that description only.
        let mut foreign = base.clone();
        foreign.inputs[fee_index]
            .witness_utxo
            .as_mut()
            .unwrap()
            .value += Amount::from_sat(1);
        sign_fee(&fx, &mut foreign, &coin, 1);
        assert!(analyze(&dep, &foreign).unwrap().fee_inputs[0].signed);
        let mut target = base.clone();
        assert_eq!(merge(&dep, &mut target, &foreign).unwrap(), 0);
        assert!(target.inputs[fee_index].tap_key_sig.is_none());

        // A slot already holding such a signature is repaired by the right one.
        target.inputs[fee_index].tap_key_sig = foreign.inputs[fee_index].tap_key_sig;
        assert!(!analyze(&dep, &target).unwrap().fee_inputs[0].signed);
        let mut owner = base.clone();
        sign_fee(&fx, &mut owner, &coin, 1);
        assert_eq!(merge(&dep, &mut target, &owner).unwrap(), 1);
        assert!(analyze(&dep, &target).unwrap().fee_inputs[0].signed);
        // Merging it again adds nothing.
        assert_eq!(merge(&dep, &mut target, &owner).unwrap(), 0);
    }

    #[test]
    fn a_script_sig_or_an_oversized_transaction_is_refused() {
        let fx = Fixture::new(3, 2);
        let dep = fx.deployment();
        let coin = fee_coin(&fx, 0, 1, 20_000);
        let good = build_psbt(
            &dep,
            &two_claims(&fx, &dep),
            std::slice::from_ref(&coin),
            &change(&fx, 19_000),
        )
        .unwrap();
        // A scriptSig on a claim input or on the fee input would end up in the final transaction.
        for i in [0, 2] {
            let mut p = good.clone();
            p.inputs[i].final_script_sig = Some(ScriptBuf::from(vec![0x51]));
            assert!(analyze(&dep, &p).is_err(), "input {i}");
            assert!(normalize(&dep, &mut p).is_err());
            assert!(finalize(&dep, &p).is_err());
        }
        let mut p = good.clone();
        p.inputs[0].final_script_sig = Some(ScriptBuf::new());
        assert!(analyze(&dep, &p).is_ok(), "an empty scriptSig is none");

        // 100 claims at 2-of-3 are over 10,000 vB: refused wherever the transaction comes from.
        let claims = |n: u8| -> Vec<ConnectorRef> {
            (0..n)
                .map(|v| ConnectorRef {
                    claim_txid: Txid::from_byte_array([v + 1; 32]),
                    covenant_index: 0,
                })
                .collect()
        };
        let big_coin = fee_coin(&fx, 0, 1, 100_000);
        let fits = build_psbt(
            &dep,
            &claims(90),
            std::slice::from_ref(&big_coin),
            &change(&fx, 50_000),
        )
        .unwrap();
        assert!(analyze(&dep, &fits).unwrap().vsize <= TRUC_MAX_VSIZE);
        let too_big = build_psbt(
            &dep,
            &claims(100),
            std::slice::from_ref(&big_coin),
            &change(&fx, 50_000),
        );
        let err = match too_big {
            Ok(p) => analyze(&dep, &p).unwrap_err(),
            Err(e) => e,
        };
        assert!(err.to_string().contains("10000 vB limit"), "{err}");
    }

    #[test]
    fn rejects_foreign_shapes() {
        let fx = Fixture::new(3, 2);
        let dep = fx.deployment();
        let coin = fee_coin(&fx, 0, 1, 20_000);
        let good = build_psbt(
            &dep,
            &two_claims(&fx, &dep),
            std::slice::from_ref(&coin),
            &change(&fx, 19_000),
        )
        .unwrap();
        let mut p = good.clone();
        p.unsigned_tx.version = Version(2);
        assert!(analyze(&dep, &p).is_err());
        let mut p = good.clone();
        p.unsigned_tx.output.push(p.unsigned_tx.output[0].clone());
        p.outputs.push(Default::default());
        assert!(analyze(&dep, &p).is_err());
        // Only fee inputs: nothing to block.
        assert!(build_psbt(&dep, &[], std::slice::from_ref(&coin), &change(&fx, 19_000)).is_err());
        let p = build_psbt(
            &dep,
            &two_claims(&fx, &dep),
            std::slice::from_ref(&coin),
            &change(&fx, 100),
        );
        assert!(p.is_err());
    }

    #[test]
    fn size_model_matches_trucs_limit() {
        let dep = Fixture::new(3, 2).deployment();
        let s = p2tr_placeholder_script();
        let one = final_vsize(&dep, 1, 1, &s);
        let two = final_vsize(&dep, 2, 1, &s);
        // A 2-of-3 connector input is about 107 vB.
        assert!((100..115).contains(&(two - one)), "{}", two - one);
    }
}
