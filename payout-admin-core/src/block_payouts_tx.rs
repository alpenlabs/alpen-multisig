//! Admin Block Payouts transactions.
//!
//! Shape (the bridge's `AdminBlockPayoutsTx`, batched):
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
use strata_bridge_tx_graph::transactions::prelude::ClaimTx;

pub const TX_VERSION: Version = Version(3);
/// Bitcoin Core policy: BIP-431 TRUC transactions are limited to 10,000 vB.
pub const TRUC_MAX_VSIZE: u64 = 10_000;
/// P2TR dust limit at Bitcoin Core's default dust relay fee.
pub const P2TR_DUST_SAT: u64 = 330;
/// Ledger Bitcoin app limit on inputs it can sign in one transaction.
pub const LEDGER_MAX_INPUTS: usize = 512;

/// A block payouts PSBT could not be decoded, failed a shape check, or lacks required signatures.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BlockPayoutsTxError {
    #[error("{0}")]
    Shape(String),
    #[error("not enough signatures: {0}")]
    NotEnoughSignatures(String),
    #[error("invalid PSBT: {0}")]
    Psbt(String),
}

fn shape_error(message: impl Into<String>) -> BlockPayoutsTxError {
    BlockPayoutsTxError::Shape(message.into())
}

/// Imports only unfinalized PSBTs, including ones with no signatures. Validation is shared
/// with the coordinator; no chain lookups happen here.
pub fn import_psbt(deployment: &Deployment, text: &str) -> Result<Psbt, BlockPayoutsTxError> {
    let mut psbt: Psbt = text
        .trim()
        .parse()
        .map_err(|error: bitcoin::psbt::PsbtParseError| {
            BlockPayoutsTxError::Psbt(error.to_string())
        })?;

    if psbt
        .inputs
        .iter()
        .any(|input| input.final_script_witness.is_some() || input.final_script_sig.is_some())
    {
        return Err(shape_error(
            "already finalized transactions cannot be imported; use an unfinalized PSBT",
        ));
    }

    normalize(deployment, &mut psbt)?;

    Ok(psbt)
}

/// Encodes a PSBT as base64 for another admin. Re-importing checks that the recipient can load it for this deployment.
pub fn export_psbt(deployment: &Deployment, psbt: &Psbt) -> Result<String, BlockPayoutsTxError> {
    let text = psbt.to_string();
    import_psbt(deployment, &text)?;

    Ok(text)
}

/// An Admin Wallet coin used to pay the fee.
#[derive(Debug, Clone)]
pub struct FeeInput {
    outpoint: OutPoint,
    value: Amount,
    /// Untweaked BIP-86 key.
    internal_key: XOnlyPublicKey,
    fingerprint: Fingerprint,
    /// Full path, e.g. m/86'/1'/73'/0/4.
    path: DerivationPath,
}

impl FeeInput {
    /// Returns the BIP-86 output spent by this fee input.
    pub fn txout(&self) -> TxOut {
        TxOut {
            value: self.value,
            script_pubkey: bip86_script(self.internal_key),
        }
    }

    /// Identifies the Admin Wallet coin selected to pay fees.
    pub fn outpoint(&self) -> OutPoint {
        self.outpoint
    }

    /// Checks that the wallet output pays the supplied BIP-86 key.
    pub fn new(
        outpoint: OutPoint,
        txout: &TxOut,
        internal_key: XOnlyPublicKey,
        fingerprint: Fingerprint,
        path: DerivationPath,
    ) -> Result<Self, BlockPayoutsTxError> {
        if txout.script_pubkey != bip86_script(internal_key) {
            return Err(shape_error(format!(
                "fee coin {outpoint} is not the BIP-86 output of its key"
            )));
        }

        Ok(Self {
            outpoint,
            value: txout.value,
            internal_key,
            fingerprint,
            path,
        })
    }

    /// Records the fee coin's witness UTXO and BIP-86 key origin for the signer.
    pub(crate) fn insert_into_psbt(&self, input: &mut bitcoin::psbt::Input) {
        input.witness_utxo = Some(self.txout());
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
    value: Amount,
    internal_key: XOnlyPublicKey,
    fingerprint: Fingerprint,
    path: DerivationPath,
}

impl ChangeOutput {
    /// Creates an Admin Wallet change output at or above the dust limit.
    pub fn new(
        value: Amount,
        internal_key: XOnlyPublicKey,
        fingerprint: Fingerprint,
        path: DerivationPath,
    ) -> Result<Self, BlockPayoutsTxError> {
        if value.to_sat() < P2TR_DUST_SAT {
            return Err(shape_error("the change output is below the dust limit"));
        }

        Ok(Self {
            value,
            internal_key,
            fingerprint,
            path,
        })
    }

    /// Derives the BIP-86 script that receives the creator's change.
    pub fn script_pubkey(&self) -> ScriptBuf {
        ScriptBuf::new_p2tr(&Secp256k1::verification_only(), self.internal_key, None)
    }

    /// Records the internal key and its origin so the signer can identify wallet change.
    pub(crate) fn insert_into_psbt(&self, output: &mut bitcoin::psbt::Output) {
        output.tap_internal_key = Some(self.internal_key);
        output.tap_key_origins.insert(
            self.internal_key,
            (vec![], (self.fingerprint, self.path.clone())),
        );
    }
}

/// Identifies the admin payout output to spend by its parent claim's txid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectorRef {
    claim_txid: Txid,
}

impl ConnectorRef {
    /// Selects the claim’s admin payout output.
    pub fn new(claim_txid: Txid) -> Self {
        Self { claim_txid }
    }

    /// Pairs the claim txid with the bridge's admin payout output index.
    pub fn outpoint(&self) -> OutPoint {
        OutPoint::new(self.claim_txid, ClaimTx::ADMIN_PAYOUT_VOUT)
    }
}

impl Covenant {
    fn to_leaf(&self) -> (ScriptBuf, TapLeafHash, taproot::ControlBlock) {
        let leaf = self.connector().leaf_scripts().remove(0);
        let hash = TapLeafHash::from_script(&leaf, LeafVersion::TapScript);
        let control_block = self
            .connector()
            .spend_info()
            .control_block(&(leaf.clone(), LeafVersion::TapScript))
            .expect("connector has its leaf");

        (leaf, hash, control_block)
    }
}

/// Rebuilds the witness UTXO, taproot spend data, and internal key origin from the deployment.
/// Includes the admin key origins when a signer roster is available.
fn fill_connector_input(deployment: &Deployment, input: &mut bitcoin::psbt::Input) {
    let covenant = deployment.covenant();
    let (leaf, leaf_hash, control_block) = covenant.to_leaf();
    input.witness_utxo = Some(covenant.connector_tx_out());
    input.tap_internal_key = Some(covenant.connector().internal_key());
    input.tap_merkle_root = covenant.connector().spend_info().merkle_root();
    input.tap_scripts = BTreeMap::from([(control_block, (leaf, LeafVersion::TapScript))]);
    input.tap_key_origins.clear();
    input.bip32_derivation.clear();

    if let Ok(signers) = deployment.signers() {
        for signer in signers {
            input.tap_key_origins.insert(
                signer.xonly(),
                (
                    vec![leaf_hash],
                    (signer.fingerprint(), signer.key_path(deployment.network())),
                ),
            );
        }
    }

    let synthetic_xpub = covenant.synthetic_xpub(deployment.network());
    input.tap_key_origins.insert(
        covenant.connector().internal_key(),
        (
            vec![],
            (
                synthetic_xpub.fingerprint(),
                DerivationPath::from_str("m/0/0").expect("static"),
            ),
        ),
    );
}

fn bip86_script(key: XOnlyPublicKey) -> ScriptBuf {
    ScriptBuf::new_p2tr(&Secp256k1::verification_only(), key, None)
}

/// Builds a PSBT with the claim connectors first, followed by fee inputs and one change output.
pub fn build_psbt(
    deployment: &Deployment,
    connectors: &[ConnectorRef],
    fees: &[FeeInput],
    change: &ChangeOutput,
) -> Result<Psbt, BlockPayoutsTxError> {
    if connectors.is_empty() {
        return Err(shape_error("a transaction needs at least one claim input"));
    }

    if connectors.len() + fees.len() > LEDGER_MAX_INPUTS {
        return Err(shape_error(format!(
            "a Ledger can sign at most {LEDGER_MAX_INPUTS} inputs"
        )));
    }

    let mut seen = BTreeSet::new();
    let mut inputs = Vec::new();
    for connector_input in connectors {
        if !seen.insert(connector_input.outpoint()) {
            return Err(shape_error(format!(
                "claim {} is listed twice",
                connector_input.claim_txid
            )));
        }

        inputs.push(connector_input.outpoint());
    }
    for fee_input in fees {
        if !seen.insert(fee_input.outpoint) {
            return Err(shape_error(format!(
                "fee coin {} is listed twice",
                fee_input.outpoint
            )));
        }

        inputs.push(fee_input.outpoint);
    }
    let transaction = Transaction {
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
    let mut psbt = Psbt::from_unsigned_tx(transaction)
        .map_err(|error| BlockPayoutsTxError::Psbt(error.to_string()))?;
    for input_index in 0..connectors.len() {
        fill_connector_input(deployment, &mut psbt.inputs[input_index]);
    }
    for (input, fee) in psbt.inputs[connectors.len()..].iter_mut().zip(fees) {
        fee.insert_into_psbt(input);
    }
    change.insert_into_psbt(&mut psbt.outputs[0]);

    Ok(psbt)
}

// ── Inspection ───────────────────────────────────────────────────────────────

/// Identifies a claim input and the admins whose signatures verify for it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorInputInfo {
    input_index: usize,
    #[serde(serialize_with = "ser_display")]
    admin_payout_outpoint: OutPoint,
    /// Admin indices with a valid signature on this input.
    signed_by: Vec<usize>,
    #[serde(skip)]
    sighash: Message,
    #[serde(skip)]
    leaf_hash: TapLeafHash,
}

/// Describes the Admin Wallet coin spent by an input and whether its signature verifies.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeeInputInfo {
    input_index: usize,
    #[serde(serialize_with = "ser_display")]
    outpoint: OutPoint,
    #[serde(rename = "valueSat", with = "bitcoin::amount::serde::as_sat")]
    value: Amount,
    signed: bool,
    #[serde(skip)]
    sighash: Message,
    #[serde(skip)]
    output_key: XOnlyPublicKey,
}

/// Summarizes a checked PSBT's inputs, change, fee, and progress toward quorum.
#[derive(Debug, Clone)]
pub struct Analysis {
    block_payouts_txid: Txid,
    connector_inputs: Vec<ConnectorInputInfo>,
    fee_inputs: Vec<FeeInputInfo>,
    change: Amount,
    change_script: ScriptBuf,
    input_total: Amount,
    vsize: u64,
    signatures_needed: usize,
}

impl Serialize for Analysis {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut fields = serializer.serialize_struct("Analysis", 13)?;
        fields.serialize_field("blockPayoutsTxid", &self.block_payouts_txid.to_string())?;
        fields.serialize_field("connectorInputs", &self.connector_inputs)?;
        fields.serialize_field("feeInputs", &self.fee_inputs)?;
        fields.serialize_field("changeSat", &self.change.to_sat())?;
        fields.serialize_field("changeScriptHex", &self.change_script)?;
        fields.serialize_field("inputTotalSat", &self.input_total.to_sat())?;
        fields.serialize_field("feeSat", &self.fee().to_sat())?;
        fields.serialize_field("vsize", &self.vsize)?;
        let signers = self.signers();
        fields.serialize_field("signers", &signers)?;
        fields.serialize_field("adminsSigned", &signers.len())?;
        fields.serialize_field("signaturesNeeded", &self.signatures_needed)?;
        fields.serialize_field("hasQuorum", &self.has_quorum())?;
        fields.serialize_field("isComplete", &self.is_complete())?;

        fields.end()
    }
}

fn ser_display<T: std::fmt::Display, S: serde::Serializer>(
    value: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}

impl Analysis {
    /// Identifies the unsigned transaction shared by all co-signers.
    pub fn block_payouts_txid(&self) -> Txid {
        self.block_payouts_txid
    }

    /// Value of the single Admin Wallet output.
    pub fn change(&self) -> Amount {
        self.change
    }

    /// The wallet inputs analyzed for key-path signatures.
    pub fn fee_inputs(&self) -> &[FeeInputInfo] {
        &self.fee_inputs
    }

    /// Combined value of all connector and fee inputs.
    pub fn input_total(&self) -> Amount {
        self.input_total
    }

    /// Returns the signature threshold for each connector input.
    pub fn signatures_needed(&self) -> usize {
        self.signatures_needed
    }

    /// Returns the admin indices that signed every connector input.
    pub fn signers(&self) -> Vec<usize> {
        self.signing_admins().collect()
    }

    fn signing_admins(&self) -> impl Iterator<Item = usize> + '_ {
        self.connector_inputs[0]
            .signed_by
            .iter()
            .copied()
            .filter(|admin| self.signed_by(*admin))
    }

    /// Returns the fully signed transaction size in virtual bytes.
    pub fn vsize(&self) -> u64 {
        self.vsize
    }

    /// The fee paid by the transaction.
    pub fn fee(&self) -> Amount {
        self.input_total - self.change
    }

    /// Number of admins who signed every connector input.
    pub fn admins_signed(&self) -> usize {
        self.signing_admins().count()
    }

    /// Whether every connector input has enough admin signatures.
    pub fn has_quorum(&self) -> bool {
        self.connector_inputs
            .iter()
            .all(|input| self.signatures_needed <= input.signed_by.len())
    }

    /// Whether the transaction has quorum and every fee input is signed.
    pub fn is_complete(&self) -> bool {
        self.has_quorum() && self.fee_inputs.iter().all(|input| input.signed)
    }

    /// Claim inputs with their verified admin signatures.
    pub fn connector_inputs(&self) -> &[ConnectorInputInfo] {
        &self.connector_inputs
    }

    /// Whether this admin has a valid signature on every connector input.
    pub fn signed_by(&self, admin_index: usize) -> bool {
        self.connector_inputs
            .iter()
            .all(|input| input.signed_by.contains(&admin_index))
    }
}

impl ConnectorInputInfo {
    /// The claim output this input spends to block the operator payout.
    pub fn admin_payout_outpoint(&self) -> OutPoint {
        self.admin_payout_outpoint
    }

    /// Returns the indices of admins who signed this input.
    pub fn signed_by(&self) -> &[usize] {
        &self.signed_by
    }

    /// Returns the position of this input in the block payouts transaction.
    pub fn input_index(&self) -> usize {
        self.input_index
    }

    /// The taproot script-path digest an admin signs for this connector input.
    pub fn sighash(&self) -> Message {
        self.sighash
    }

    fn signatures<'a>(
        &'a self,
        deployment: &'a Deployment,
        psbt: &'a Psbt,
    ) -> impl Iterator<Item = (usize, (XOnlyPublicKey, TapLeafHash), taproot::Signature)> + 'a {
        self.signed_by.iter().map(move |&admin| {
            let key = (deployment.admin_keys()[admin], self.leaf_hash);

            (
                admin,
                key,
                psbt.inputs[self.input_index].tap_script_sigs[&key],
            )
        })
    }
}

impl FeeInputInfo {
    /// Returns the position of this input in the block payouts transaction.
    pub fn input_index(&self) -> usize {
        self.input_index
    }

    /// Identifies the wallet output spent by this fee input.
    pub fn outpoint(&self) -> OutPoint {
        self.outpoint
    }

    /// Returns the signature verification result for this fee input.
    pub fn signed(&self) -> bool {
        self.signed
    }

    /// Value of the wallet output consumed by this input.
    pub fn value(&self) -> Amount {
        self.value
    }

    fn verifies(&self, signature: Option<taproot::Signature>) -> bool {
        signature.is_some_and(|signature| {
            signature.sighash_type == TapSighashType::Default
                && verify_schnorr(&self.sighash, &signature.signature, &self.output_key)
        })
    }
}

fn prevouts(psbt: &Psbt) -> Result<Vec<TxOut>, BlockPayoutsTxError> {
    psbt.inputs
        .iter()
        .enumerate()
        .map(|(input_index, input)| {
            input
                .witness_utxo
                .clone()
                .ok_or_else(|| shape_error(format!("input {input_index} has no witness UTXO")))
        })
        .collect()
}

/// One cache per transaction: it hashes the prevouts once, so every input's sighash is O(1) after
/// the first instead of O(inputs).
fn script_sighash(
    cache: &mut SighashCache<&Transaction>,
    prevouts: &[TxOut],
    input_index: usize,
    leaf_hash: TapLeafHash,
) -> Result<Message, BlockPayoutsTxError> {
    let sighash = cache
        .taproot_script_spend_signature_hash(
            input_index,
            &Prevouts::All(prevouts),
            leaf_hash,
            TapSighashType::Default,
        )
        .map_err(|error| BlockPayoutsTxError::Psbt(error.to_string()))?;

    Ok(Message::from_digest(sighash.to_byte_array()))
}

/// BIP-341 key-path sighash (SIGHASH_DEFAULT) of input `input_index`.
pub fn key_sighash(
    transaction: &Transaction,
    prevouts: &[TxOut],
    input_index: usize,
) -> Result<Message, BlockPayoutsTxError> {
    key_sighash_cached(&mut SighashCache::new(transaction), prevouts, input_index)
}

fn key_sighash_cached(
    cache: &mut SighashCache<&Transaction>,
    prevouts: &[TxOut],
    input_index: usize,
) -> Result<Message, BlockPayoutsTxError> {
    let sighash = cache
        .taproot_key_spend_signature_hash(
            input_index,
            &Prevouts::All(prevouts),
            TapSighashType::Default,
        )
        .map_err(|error| BlockPayoutsTxError::Psbt(error.to_string()))?;

    Ok(Message::from_digest(sighash.to_byte_array()))
}

fn verify_schnorr(message: &Message, signature: &schnorr::Signature, key: &XOnlyPublicKey) -> bool {
    Secp256k1::verification_only()
        .verify_schnorr(signature, message, key)
        .is_ok()
}

/// Checks the shape of a PSBT and reports which valid signatures it carries. Invalid signatures
/// are not counted (and [`normalize`] removes them).
pub fn analyze(deployment: &Deployment, psbt: &Psbt) -> Result<Analysis, BlockPayoutsTxError> {
    let transaction = &psbt.unsigned_tx;

    if transaction.version != TX_VERSION {
        return Err(shape_error("not a version 3 transaction"));
    }

    if transaction.lock_time != absolute::LockTime::ZERO {
        return Err(shape_error("locktime must be 0"));
    }

    if transaction.output.len() != 1 {
        return Err(shape_error(
            "must have exactly one output (the Admin Wallet change)",
        ));
    }

    let change = &transaction.output[0];

    if !change.script_pubkey.is_p2tr() || change.value.to_sat() < P2TR_DUST_SAT {
        return Err(shape_error(
            "the change output must be a taproot output above the dust limit",
        ));
    }

    // Every input is a native taproot spend. A scriptSig carried in the PSBT is outside what the
    // signatures commit to, and the finalized transaction would take it over.
    if psbt.inputs.iter().any(|input| {
        input
            .final_script_sig
            .as_ref()
            .is_some_and(|script| !script.is_empty())
    }) {
        return Err(shape_error(
            "an input carries a scriptSig; taproot inputs have none",
        ));
    }

    // Checked block payouts transactions request only Taproot DEFAULT. Normalization clears
    // the redundant request and keeps only verified default-type signatures.
    if psbt
        .inputs
        .iter()
        .any(|input| input.sighash_type.is_some_and(|kind| kind.to_u32() != 0))
    {
        return Err(shape_error(
            "only the default Taproot signature type is supported",
        ));
    }

    let prevouts = prevouts(psbt)?;
    let mut cache = SighashCache::new(transaction);
    let leaf_hash = deployment.covenant().to_leaf().1;
    let mut seen = BTreeSet::new();
    let mut connector_inputs = Vec::new();
    let mut fee_inputs = Vec::new();
    let mut input_total = Amount::ZERO;
    let connector_script = deployment.covenant().connector_script();
    for (input_index, (transaction_input, previous_output)) in
        transaction.input.iter().zip(&prevouts).enumerate()
    {
        if !seen.insert(transaction_input.previous_output) {
            return Err(shape_error("an input is spent twice"));
        }

        if transaction_input.sequence != Sequence::MAX {
            return Err(shape_error(format!(
                "input {input_index} must use sequence 0xffffffff"
            )));
        }

        input_total = input_total
            .checked_add(previous_output.value)
            .ok_or_else(|| shape_error("input total overflows"))?;

        if previous_output.script_pubkey == connector_script {
            if transaction_input.previous_output.vout != ClaimTx::ADMIN_PAYOUT_VOUT {
                return Err(shape_error(format!(
                    "input {input_index} is a connector but not a claim's output 1"
                )));
            }

            if !fee_inputs.is_empty() {
                return Err(shape_error("claim inputs must come before the fee inputs"));
            }

            if previous_output.value != deployment.covenant().connector().value() {
                return Err(shape_error(format!(
                    "input {input_index} has the wrong connector value"
                )));
            }

            let sighash = script_sighash(&mut cache, &prevouts, input_index, leaf_hash)?;
            let signed_by = (0..deployment.admin_keys().len())
                .filter(|&admin_index| {
                    psbt.inputs[input_index]
                        .tap_script_sigs
                        .get(&(deployment.admin_keys()[admin_index], leaf_hash))
                        .is_some_and(|signature| {
                            signature.sighash_type == TapSighashType::Default
                                && verify_schnorr(
                                    &sighash,
                                    &signature.signature,
                                    &deployment.admin_keys()[admin_index],
                                )
                        })
                })
                .collect();
            connector_inputs.push(ConnectorInputInfo {
                input_index,
                admin_payout_outpoint: transaction_input.previous_output,
                signed_by,
                sighash,
                leaf_hash,
            });
        } else {
            let internal = psbt.inputs[input_index].tap_internal_key.ok_or_else(|| {
                shape_error(format!(
                    "fee input {input_index} has no taproot internal key"
                ))
            })?;

            if previous_output.script_pubkey != bip86_script(internal) {
                return Err(shape_error(format!(
                    "fee input {input_index} is not a BIP-86 key-path output"
                )));
            }

            let mut fee = FeeInputInfo {
                input_index,
                outpoint: transaction_input.previous_output,
                value: previous_output.value,
                signed: false,
                sighash: key_sighash_cached(&mut cache, &prevouts, input_index)?,
                output_key: internal
                    .tap_tweak(&Secp256k1::verification_only(), None)
                    .0
                    .to_x_only_public_key(),
            };
            fee.signed = fee.verifies(psbt.inputs[input_index].tap_key_sig);
            fee_inputs.push(fee);
        }
    }

    if connector_inputs.is_empty() {
        return Err(shape_error(
            "no input is an admin payout connector of this deployment",
        ));
    }

    if input_total < change.value {
        return Err(shape_error("the outputs exceed the inputs"));
    }

    let vsize = final_vsize(
        deployment,
        connector_inputs.len(),
        fee_inputs.len(),
        &change.script_pubkey,
    );

    // Check imported PSBTs against the same signed-size limit the bundler uses.
    if vsize > TRUC_MAX_VSIZE {
        return Err(shape_error(format!(
            "the signed transaction would be {vsize} vB, over the {TRUC_MAX_VSIZE} vB limit of a version 3 transaction"
        )));
    }

    Ok(Analysis {
        block_payouts_txid: transaction.compute_txid(),
        signatures_needed: deployment.threshold(),
        connector_inputs,
        fee_inputs,
        change: change.value,
        change_script: change.script_pubkey.clone(),
        input_total,
        vsize,
    })
}

/// Rewrites every connector input's signing data from the trusted deployment and drops signatures
/// that do not verify. Use on any PSBT that came from elsewhere before showing or signing it.
pub fn normalize(
    deployment: &Deployment,
    psbt: &mut Psbt,
) -> Result<Analysis, BlockPayoutsTxError> {
    let analysis = analyze(deployment, psbt)?;
    for input in &mut psbt.inputs {
        input.sighash_type = None;
        input.partial_sigs.clear();
    }

    for connector_input in &analysis.connector_inputs {
        let keep = connector_input
            .signatures(deployment, psbt)
            .map(|(_, key, signature)| (key, signature))
            .collect();
        let input = &mut psbt.inputs[connector_input.input_index];
        let keep_origins = input.tap_key_origins.clone();
        fill_connector_input(deployment, input);

        if deployment.signers().is_err() {
            // Keep the origins the creator wrote when this app has no roster to rebuild them,
            // but only for keys of this deployment.
            for (key, origin) in keep_origins {
                if deployment.admin_index(&key).is_some() {
                    input.tap_key_origins.insert(key, origin);
                }
            }
        }

        input.tap_script_sigs = keep;
        input.tap_key_sig = None;
        input.final_script_witness = None;
    }
    for fee_input in &analysis.fee_inputs {
        psbt.inputs[fee_input.input_index].tap_script_sigs.clear();

        if !fee_input.signed {
            psbt.inputs[fee_input.input_index].tap_key_sig = None;
        }

        psbt.inputs[fee_input.input_index].final_script_witness = None;
    }

    Ok(analysis)
}

/// Verifies and adds an admin signature using the analysis taken
/// before the first one (each analysis hashes every input, so redoing it per signature is
/// quadratic) and `added` remembers the (input, admin) pairs this batch already filled.
pub(crate) fn add_signature_with_analysis(
    deployment: &Deployment,
    psbt: &mut Psbt,
    analysis: &Analysis,
    added: &mut BTreeSet<(usize, XOnlyPublicKey)>,
    input_index: usize,
    admin: &XOnlyPublicKey,
    signature: schnorr::Signature,
) -> Result<bool, String> {
    let admin_index = deployment
        .admin_index(admin)
        .ok_or("the key is not in the admin set")?;
    let connector_input = analysis
        .connector_inputs
        .iter()
        .find(|connector_input| connector_input.input_index == input_index)
        .ok_or("the input is not a claim input of this transaction")?;

    if !verify_schnorr(&connector_input.sighash, &signature, admin) {
        return Err("the signature does not verify".into());
    }

    if connector_input.signed_by.contains(&admin_index) || added.contains(&(input_index, *admin)) {
        return Ok(false);
    }

    psbt.inputs[input_index].tap_script_sigs.insert(
        (*admin, connector_input.leaf_hash),
        taproot::Signature {
            signature,
            sighash_type: TapSighashType::Default,
        },
    );
    added.insert((input_index, *admin));

    Ok(true)
}

// ── Signature text ───────────────────────────────────────────────────────────

/// One copyable admin signature with the transaction, input position, and key needed to verify it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PastedSignature {
    block_payouts_txid: Txid,
    input_index: usize,
    admin_key: XOnlyPublicKey,
    signature: schnorr::Signature,
}

impl PastedSignature {
    /// The x-only key used to verify this pasted signature.
    pub fn admin_key(&self) -> XOnlyPublicKey {
        self.admin_key
    }
}

impl std::fmt::Display for PastedSignature {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}:{}:{}:{}",
            self.block_payouts_txid, self.input_index, self.admin_key, self.signature
        )
    }
}

/// All valid admin signatures in the transaction, one line each (input order, then admin order).
pub fn export_signatures(
    deployment: &Deployment,
    psbt: &Psbt,
) -> Result<Vec<PastedSignature>, BlockPayoutsTxError> {
    let analysis = analyze(deployment, psbt)?;
    let mut signatures = Vec::new();
    for connector_input in &analysis.connector_inputs {
        for (_, (key, _), signature) in connector_input.signatures(deployment, psbt) {
            signatures.push(PastedSignature {
                block_payouts_txid: analysis.block_payouts_txid,
                input_index: connector_input.input_index,
                admin_key: key,
                signature: signature.signature,
            });
        }
    }

    Ok(signatures)
}

/// Formats the signatures for pasting, one colon-delimited record per line.
pub fn signatures_text(lines: &[PastedSignature]) -> String {
    lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Preserves a rejected paste item alongside the reason it could not be added.
#[derive(Debug, Clone, Default, serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvalidSignature {
    raw: String,
    reason: String,
}

/// Separates newly added signatures, duplicates, and rejected paste items.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteOutcome {
    added: Vec<PastedSignature>,
    already_present: usize,
    invalid: Vec<InvalidSignature>,
}

impl PasteOutcome {
    /// Returns the signatures added to the PSBT.
    pub fn added(&self) -> &[PastedSignature] {
        &self.added
    }

    /// Returns the number of pasted signatures already in the PSBT.
    pub fn already_present(&self) -> usize {
        self.already_present
    }

    /// Returns the rejected paste items and their reasons.
    pub fn into_invalid(self) -> Vec<InvalidSignature> {
        self.invalid
    }
}

fn parse_line(
    raw: &str,
    txid: &Txid,
) -> Result<(usize, XOnlyPublicKey, schnorr::Signature), String> {
    let parts: Vec<&str> = raw.split(':').collect();
    let [pasted_txid, input_index, key, signature] = parts.as_slice() else {
        return Err("expected <txid>:<input>:<admin key>:<signature>".into());
    };
    let pasted_txid = Txid::from_str(pasted_txid).map_err(|_| "malformed transaction id")?;

    if &pasted_txid != txid {
        return Err("the signature is for a different transaction".into());
    }

    let input_index: usize = input_index.parse().map_err(|_| "malformed input index")?;
    let key = XOnlyPublicKey::from_str(key).map_err(|_| "malformed admin key")?;
    let bytes = hex::decode(signature).map_err(|_| "the signature is not hex")?;

    if bytes.len() != 64 {
        return Err("the signature must be 64 bytes (SIGHASH_DEFAULT)".into());
    }

    let signature = schnorr::Signature::from_slice(&bytes).map_err(|_| "malformed signature")?;

    Ok((input_index, key, signature))
}

/// Verifies every pasted signature; only valid ones are added to `psbt`.
pub fn paste_signatures(deployment: &Deployment, psbt: &mut Psbt, text: &str) -> PasteOutcome {
    let txid = psbt.unsigned_tx.compute_txid();
    let mut outcome = PasteOutcome::default();
    let analysis = analyze(deployment, psbt).map_err(|error| error.to_string());
    let mut filled = BTreeSet::new();
    for raw in text
        .split(|character: char| character.is_whitespace() || character == ',' || character == ';')
        .filter(|line| !line.is_empty())
    {
        let result = parse_line(raw, &txid).and_then(|(input_index, key, signature)| {
            let analysis = analysis.as_ref().map_err(Clone::clone)?;

            add_signature_with_analysis(
                deployment,
                psbt,
                analysis,
                &mut filled,
                input_index,
                &key,
                signature,
            )
            .map(|added| (added, input_index, key, signature))
        });
        match result {
            Ok((true, input_index, key, signature)) => outcome.added.push(PastedSignature {
                block_payouts_txid: txid,
                input_index,
                admin_key: key,
                signature,
            }),
            Ok((false, ..)) => outcome.already_present += 1,
            Err(reason) => outcome.invalid.push(InvalidSignature {
                raw: raw.to_string(),
                reason,
            }),
        }
    }

    outcome
}

/// Copies the admin signatures and fee input signatures of `other` that are valid for `base` into
/// `base`. Both must be the same transaction.
pub fn merge(
    deployment: &Deployment,
    base: &mut Psbt,
    other: &Psbt,
) -> Result<usize, BlockPayoutsTxError> {
    if base.unsigned_tx != other.unsigned_tx {
        return Err(shape_error("the two PSBTs are different transactions"));
    }

    let theirs = analyze(deployment, other)?;
    let ours = analyze(deployment, base)?;
    let mut filled = BTreeSet::new();
    let mut added = 0;
    for connector_input in &theirs.connector_inputs {
        for (_, (key, _), signature) in connector_input.signatures(deployment, other) {
            if add_signature_with_analysis(
                deployment,
                base,
                &ours,
                &mut filled,
                connector_input.input_index,
                &key,
                signature.signature,
            ) == Ok(true)
            {
                added += 1;
            }
        }
    }
    // A fee signature counts only if it verifies against `base`'s own prevouts and key: `other`
    // may describe the fee input differently, and its signature is then valid only there. A slot
    // holding a signature that does not verify is free.
    for fee_input in ours.fee_inputs.iter().filter(|fee_input| !fee_input.signed) {
        let signature = other.inputs[fee_input.input_index].tap_key_sig;

        if fee_input.verifies(signature) {
            base.inputs[fee_input.input_index].tap_key_sig = signature;
            added += 1;
        }
    }

    Ok(added)
}

// ── Finalizing ───────────────────────────────────────────────────────────────

/// Builds the final transaction: each connector witness through the bridge's own
/// `ClaimAdminPayoutConnector::finalize_input`, each fee input as a key-path spend.
pub fn finalize(deployment: &Deployment, psbt: &Psbt) -> Result<Transaction, BlockPayoutsTxError> {
    let analysis = analyze(deployment, psbt)?;

    if !analysis.has_quorum() {
        return Err(BlockPayoutsTxError::NotEnoughSignatures(format!(
            "{} of {} admins signed every input",
            analysis.admins_signed(),
            analysis.signatures_needed
        )));
    }

    if let Some(fee_input) = analysis
        .fee_inputs
        .iter()
        .find(|fee_input| !fee_input.signed)
    {
        return Err(BlockPayoutsTxError::NotEnoughSignatures(format!(
            "fee input {} is not signed",
            fee_input.outpoint
        )));
    }

    let mut working_psbt = psbt.clone();
    for connector_input in &analysis.connector_inputs {
        let covenant = deployment.covenant();
        let admin_signatures = connector_input
            .signatures(deployment, &working_psbt)
            .map(|(pubkey_index, _, signature)| AdminSignature {
                pubkey_index,
                signature: signature.signature,
            })
            .collect();
        covenant.connector().finalize_input(
            &mut working_psbt.inputs[connector_input.input_index],
            &ClaimAdminPayoutWitness::AdminBlockPayout { admin_signatures },
        );
    }
    for fee_input in &analysis.fee_inputs {
        let signature = working_psbt.inputs[fee_input.input_index]
            .tap_key_sig
            .expect("checked above");
        working_psbt.inputs[fee_input.input_index].final_script_witness =
            Some(Witness::p2tr_key_spend(&signature));
    }
    let transaction = working_psbt.extract_tx_unchecked_fee_rate();

    Ok(transaction)
}

// ── Size ─────────────────────────────────────────────────────────────────────

/// vsize of the fully signed transaction. Exact: signatures are 64 bytes (SIGHASH_DEFAULT), admins
/// that do not sign leave an empty push, and the control block has no merkle path (one leaf).
pub fn final_vsize(
    deployment: &Deployment,
    connector_count: usize,
    fee_input_count: usize,
    change: &ScriptBuf,
) -> u64 {
    final_weight(deployment, connector_count, fee_input_count, change).div_ceil(4)
}

fn final_weight(
    deployment: &Deployment,
    connector_count: usize,
    fee_input_count: usize,
    change: &ScriptBuf,
) -> u64 {
    let signature = schnorr::Signature::from_slice(&[0; 64]).expect("64-byte signature");
    let admin_signatures = (0..deployment.threshold())
        .map(|pubkey_index| AdminSignature {
            pubkey_index,
            signature,
        })
        .collect();
    let mut connector_input = bitcoin::psbt::Input::default();
    deployment.covenant().connector().finalize_input(
        &mut connector_input,
        &ClaimAdminPayoutWitness::AdminBlockPayout { admin_signatures },
    );
    let connector_witness = connector_input
        .final_script_witness
        .expect("bridge finalized the input");
    let fee_witness = Witness::from_slice(&[vec![0u8; 64]]);
    let input = |witness: &Witness| TxIn {
        previous_output: OutPoint::null(),
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: witness.clone(),
    };
    let transaction = Transaction {
        version: TX_VERSION,
        lock_time: absolute::LockTime::ZERO,
        input: std::iter::repeat_n(input(&connector_witness), connector_count)
            .chain(std::iter::repeat_n(input(&fee_witness), fee_input_count))
            .collect(),
        output: vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: change.clone(),
        }],
    };

    transaction.weight().to_wu()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    fn two_claims(fixture: &Fixture, deployment: &Deployment) -> Vec<ConnectorRef> {
        (0..2)
            .map(|variant| {
                ConnectorRef::new(fixture.game(deployment, variant).report().new_claim_txid())
            })
            .collect()
    }

    /// The largest bundler transaction (about 90 claims) signs, merges and finalizes in seconds,
    /// not minutes: each step analyzes the transaction once, with one sighash cache.
    #[test]
    fn psbt_exchange_accepts_unsigned_and_rejects_finalized() {
        let fixture = Fixture::new(3, 2);
        let deployment = fixture.deployment();
        let coin = fee_coin(&fixture, 0, 4, 50_000);
        let claims = vec![ConnectorRef::new(
            fixture.game(&deployment, 1).report().new_claim_txid(),
        )];
        let mut working_psbt =
            build_psbt(&deployment, &claims, &[coin], &change(&fixture, 49_000)).unwrap();
        let text = export_psbt(&deployment, &working_psbt).unwrap();
        let imported = import_psbt(&deployment, &text).unwrap();
        assert_eq!(imported, working_psbt);
        assert_eq!(
            analyze(&deployment, &imported)
                .unwrap()
                .connector_inputs()
                .len(),
            1
        );
        working_psbt.inputs[0].final_script_witness = Some(Witness::new());
        assert!(import_psbt(&deployment, &working_psbt.to_string())
            .unwrap_err()
            .to_string()
            .contains("already finalized"));
        assert!(export_psbt(&deployment, &working_psbt).is_err());
    }

    #[test]
    fn import_accepts_only_default_signature_requests() {
        let fixture = Fixture::new(3, 2);
        let deployment = fixture.deployment();
        let mut psbt = build_psbt(
            &deployment,
            &[ConnectorRef::new(Txid::from_byte_array([9; 32]))],
            &[fee_coin(&fixture, 0, 0, 50_000)],
            &change(&fixture, 49_000),
        )
        .unwrap();

        for index in 0..psbt.inputs.len() {
            psbt.inputs[index].sighash_type = Some(TapSighashType::All.into());
            assert!(import_psbt(&deployment, &psbt.to_string()).is_err());
            psbt.inputs[index].sighash_type = Some(TapSighashType::Default.into());
            psbt = import_psbt(&deployment, &psbt.to_string()).unwrap();
            assert!(psbt.inputs[index].sighash_type.is_none());
        }
    }

    #[test]
    fn ninety_claims_sign_merge_and_finalize() {
        use std::time::Instant;
        let fixture = Fixture::new(3, 2);
        let deployment = fixture.deployment();
        let coin = fee_coin(&fixture, 0, 4, 500_000);
        let claims: Vec<ConnectorRef> = (0..90u32)
            .map(|i| {
                let mut bytes = [0u8; 32];
                bytes[..4].copy_from_slice(&i.to_le_bytes());

                ConnectorRef::new(bitcoin::Txid::from_byte_array(bytes))
            })
            .collect();
        let base = build_psbt(
            &deployment,
            &claims,
            std::slice::from_ref(&coin),
            &change(&fixture, 300_000),
        )
        .unwrap();
        let started = Instant::now();
        let mut first_psbt = base.clone();
        sign_as(&fixture, &deployment, &mut first_psbt, 0);
        println!("sign_as {:?}", started.elapsed());
        let mut second_psbt = base.clone();
        sign_as(&fixture, &deployment, &mut second_psbt, 1);
        sign_fee(&fixture, &mut second_psbt, &coin, 4);
        let started = Instant::now();
        let added = merge(&deployment, &mut first_psbt, &second_psbt).unwrap();
        println!("merge {:?}", started.elapsed());
        assert_eq!(added, 90 + 1);
        let started = Instant::now();
        let transaction = finalize(&deployment, &first_psbt).unwrap();
        println!("finalize {:?}", started.elapsed());
        assert!(transaction.vsize() as u64 <= TRUC_MAX_VSIZE);
    }

    #[test]
    fn build_sign_finalize() {
        let fixture = Fixture::new(3, 2);
        let deployment = fixture.deployment();
        let coin = fee_coin(&fixture, 0, 4, 50_000);
        let claims = two_claims(&fixture, &deployment);
        let mut psbt = build_psbt(
            &deployment,
            &claims,
            std::slice::from_ref(&coin),
            &change(&fixture, 49_000),
        )
        .unwrap();
        let analysis = analyze(&deployment, &psbt).unwrap();
        assert_eq!(analysis.connector_inputs().len(), 2);
        assert_eq!(analysis.fee().to_sat(), 50_000 + 660 - 49_000);
        let json = serde_json::to_value(&analysis).unwrap();
        assert_eq!(json["feeSat"], 1_660);
        assert_eq!(json["adminsSigned"], 0);
        assert_eq!(json["hasQuorum"], false);
        assert_eq!(json["isComplete"], false);
        assert_eq!(
            json["changeScriptHex"],
            hex::encode(psbt.unsigned_tx.output[0].script_pubkey.as_bytes())
        );
        assert_eq!(
            (analysis.admins_signed(), analysis.signatures_needed()),
            (0, 2)
        );
        assert!(!analysis.has_quorum());
        let txid = analysis.block_payouts_txid();

        sign_as(&fixture, &deployment, &mut psbt, 0);
        sign_fee(&fixture, &mut psbt, &coin, 4);
        let analysis = analyze(&deployment, &psbt).unwrap();
        assert_eq!(analysis.signers(), vec![0]);
        assert!(analysis.fee_inputs()[0].signed());
        assert!(finalize(&deployment, &psbt).is_err());

        sign_as(&fixture, &deployment, &mut psbt, 2);
        let analysis = analyze(&deployment, &psbt).unwrap();
        assert_eq!(analysis.signers(), vec![0, 2]);
        assert!(analysis.is_complete());
        let transaction = finalize(&deployment, &psbt).unwrap();
        assert_eq!(
            transaction.compute_txid(),
            txid,
            "signatures do not change the txid"
        );
        // The size model is exact.
        assert_eq!(transaction.vsize() as u64, analysis.vsize());
        // Witness layout: one slot per admin in reverse order; admin 1 did not sign.
        let witness: Vec<&[u8]> = transaction.input[0].witness.iter().collect();
        assert_eq!(witness.len(), 3 + 2);
        assert_eq!(witness[0].len(), 64); // admin 2
        assert!(witness[1].is_empty()); // admin 1
        assert_eq!(witness[2].len(), 64); // admin 0
                                          // Each connector witness verifies against the script (sanity: through bitcoinconsensus is
                                          // covered by the regtest suite in the desktop crate).
    }

    #[test]
    fn signature_lines_round_trip_and_reject_bad_ones() {
        let fixture = Fixture::new(3, 2);
        let deployment = fixture.deployment();
        let coin = fee_coin(&fixture, 0, 1, 20_000);
        let base = build_psbt(
            &deployment,
            &two_claims(&fixture, &deployment),
            &[coin],
            &change(&fixture, 19_000),
        )
        .unwrap();
        let mut signed = base.clone();
        sign_as(&fixture, &deployment, &mut signed, 1);
        let lines = export_signatures(&deployment, &signed).unwrap();
        assert_eq!(lines.len(), 2);
        let text = signatures_text(&lines);

        let mut target = base.clone();
        let bad_signature = format!(
            "{}:0:{}:{}",
            lines[0].block_payouts_txid,
            lines[0].admin_key,
            "00".repeat(64)
        );
        let wrong_transaction = format!(
            "{}:0:{}:{}",
            "11".repeat(32),
            lines[0].admin_key,
            lines[0].signature
        );
        let other_admin = format!(
            "{}:0:{}:{}",
            lines[0].block_payouts_txid,
            fixture.admin_xonly(0),
            lines[0].signature
        );
        let pasted = format!("{text}\n{bad_signature}\n{wrong_transaction}\n{other_admin}\njunk");
        let outcome = paste_signatures(&deployment, &mut target, &pasted);
        assert_eq!(outcome.added.len(), 2);
        assert_eq!(outcome.invalid.len(), 4);
        assert_eq!(analyze(&deployment, &target).unwrap().signers(), vec![1]);
        // Pasting again adds nothing.
        let again = paste_signatures(&deployment, &mut target, &text);
        assert_eq!((again.added.len(), again.already_present), (0, 2));
    }

    #[test]
    fn merge_and_normalize() {
        let fixture = Fixture::new(3, 2);
        let deployment = fixture.deployment();
        let coin = fee_coin(&fixture, 0, 1, 20_000);
        let base = build_psbt(
            &deployment,
            &two_claims(&fixture, &deployment),
            &[coin],
            &change(&fixture, 19_000),
        )
        .unwrap();
        let mut first_psbt = base.clone();
        let mut second_psbt = base.clone();
        sign_as(&fixture, &deployment, &mut first_psbt, 0);
        sign_as(&fixture, &deployment, &mut second_psbt, 1);
        assert_eq!(
            merge(&deployment, &mut first_psbt, &second_psbt).unwrap(),
            2
        );
        assert_eq!(
            analyze(&deployment, &first_psbt).unwrap().signers(),
            vec![0, 1]
        );

        // A tampered PSBT: a forged signature and a foreign leaf are dropped / overwritten.
        let mut tampered = first_psbt.clone();
        let leaf_hash = *tampered.inputs[0]
            .tap_script_sigs
            .keys()
            .next()
            .map(|(_, leaf_hash)| leaf_hash)
            .unwrap();
        tampered.inputs[0].tap_script_sigs.insert(
            (fixture.admin_xonly(2), leaf_hash),
            taproot::Signature {
                signature: schnorr::Signature::from_slice(&[7u8; 64]).unwrap(),
                sighash_type: TapSighashType::Default,
            },
        );
        tampered.inputs[0].tap_scripts.clear();
        let analysis = normalize(&deployment, &mut tampered).unwrap();
        // Reusing the pre-normalization analysis must describe exactly the rewritten PSBT.
        assert_eq!(
            serde_json::to_value(&analysis).unwrap(),
            serde_json::to_value(analyze(&deployment, &tampered).unwrap()).unwrap()
        );
        assert_eq!(analysis.connector_inputs()[0].signed_by(), vec![0, 1]);
        assert_eq!(tampered.inputs[0].tap_scripts.len(), 1);
        assert_eq!(tampered.inputs[0].tap_script_sigs.len(), 2);
    }

    #[test]
    fn a_fee_signature_is_merged_only_if_it_is_valid_for_the_base() {
        let fixture = Fixture::new(3, 2);
        let deployment = fixture.deployment();
        let coin = fee_coin(&fixture, 0, 1, 20_000);
        let base = build_psbt(
            &deployment,
            &two_claims(&fixture, &deployment),
            std::slice::from_ref(&coin),
            &change(&fixture, 19_000),
        )
        .unwrap();
        let fee_index = analyze(&deployment, &base).unwrap().fee_inputs()[0].input_index();

        // The same transaction, but describing the fee coin with another amount: the signature is
        // valid for that description only.
        let mut foreign = base.clone();
        foreign.inputs[fee_index]
            .witness_utxo
            .as_mut()
            .unwrap()
            .value += Amount::from_sat(1);
        sign_fee(&fixture, &mut foreign, &coin, 1);
        assert!(analyze(&deployment, &foreign).unwrap().fee_inputs()[0].signed());
        let mut target = base.clone();
        assert_eq!(merge(&deployment, &mut target, &foreign).unwrap(), 0);
        assert!(target.inputs[fee_index].tap_key_sig.is_none());

        // A slot already holding such a signature is repaired by the right one.
        target.inputs[fee_index].tap_key_sig = foreign.inputs[fee_index].tap_key_sig;
        assert!(!analyze(&deployment, &target).unwrap().fee_inputs()[0].signed());
        let mut owner = base.clone();
        sign_fee(&fixture, &mut owner, &coin, 1);
        assert_eq!(merge(&deployment, &mut target, &owner).unwrap(), 1);
        assert!(analyze(&deployment, &target).unwrap().fee_inputs()[0].signed());
        // Merging it again adds nothing.
        assert_eq!(merge(&deployment, &mut target, &owner).unwrap(), 0);
    }

    #[test]
    fn a_script_sig_or_an_oversized_transaction_is_refused() {
        let fixture = Fixture::new(3, 2);
        let deployment = fixture.deployment();
        let coin = fee_coin(&fixture, 0, 1, 20_000);
        let good = build_psbt(
            &deployment,
            &two_claims(&fixture, &deployment),
            std::slice::from_ref(&coin),
            &change(&fixture, 19_000),
        )
        .unwrap();
        // A scriptSig on a claim input or on the fee input would end up in the final transaction.
        for input_index in [0, 2] {
            let mut working_psbt = good.clone();
            working_psbt.inputs[input_index].final_script_sig = Some(ScriptBuf::from(vec![0x51]));
            assert!(
                analyze(&deployment, &working_psbt).is_err(),
                "input {input_index}"
            );
            assert!(normalize(&deployment, &mut working_psbt).is_err());
            assert!(finalize(&deployment, &working_psbt).is_err());
        }
        let mut working_psbt = good.clone();
        working_psbt.inputs[0].final_script_sig = Some(ScriptBuf::new());
        assert!(
            analyze(&deployment, &working_psbt).is_ok(),
            "an empty scriptSig is none"
        );

        // 100 claims at 2-of-3 are over 10,000 vB: refused wherever the transaction comes from.
        let claims = |claim_count: u8| -> Vec<ConnectorRef> {
            (0..claim_count)
                .map(|variant| ConnectorRef::new(Txid::from_byte_array([variant + 1; 32])))
                .collect()
        };
        let big_coin = fee_coin(&fixture, 0, 1, 100_000);
        let fits = build_psbt(
            &deployment,
            &claims(90),
            std::slice::from_ref(&big_coin),
            &change(&fixture, 50_000),
        )
        .unwrap();
        assert!(analyze(&deployment, &fits).unwrap().vsize() <= TRUC_MAX_VSIZE);
        let too_big = build_psbt(
            &deployment,
            &claims(100),
            std::slice::from_ref(&big_coin),
            &change(&fixture, 50_000),
        );
        let error = match too_big {
            Ok(working_psbt) => analyze(&deployment, &working_psbt).unwrap_err(),
            Err(error) => error,
        };
        assert!(error.to_string().contains("10000 vB limit"), "{error}");
    }

    #[test]
    fn rejects_foreign_shapes() {
        let fixture = Fixture::new(3, 2);
        let deployment = fixture.deployment();
        let coin = fee_coin(&fixture, 0, 1, 20_000);
        let good = build_psbt(
            &deployment,
            &two_claims(&fixture, &deployment),
            std::slice::from_ref(&coin),
            &change(&fixture, 19_000),
        )
        .unwrap();
        let mut working_psbt = good.clone();
        working_psbt.unsigned_tx.version = Version(2);
        assert!(analyze(&deployment, &working_psbt).is_err());
        let mut working_psbt = good.clone();
        working_psbt
            .unsigned_tx
            .output
            .push(working_psbt.unsigned_tx.output[0].clone());
        working_psbt.outputs.push(Default::default());
        assert!(analyze(&deployment, &working_psbt).is_err());
        // Only fee inputs: nothing to block.
        assert!(build_psbt(
            &deployment,
            &[],
            std::slice::from_ref(&coin),
            &change(&fixture, 19_000)
        )
        .is_err());
        let change = change(&fixture, 19_000);
        assert!(ChangeOutput::new(
            Amount::from_sat(100),
            change.internal_key,
            change.fingerprint,
            change.path
        )
        .is_err());
    }

    #[test]
    fn size_model_matches_trucs_limit() {
        let deployment = Fixture::new(3, 2).deployment();
        let script = &P2TR_PLACEHOLDER_SCRIPT;
        let one = final_vsize(&deployment, 1, 1, script);
        let two = final_vsize(&deployment, 2, 1, script);
        // A 2-of-3 connector input is about 107 vB.
        assert!((100..115).contains(&(two - one)), "{}", two - one);
    }
}
