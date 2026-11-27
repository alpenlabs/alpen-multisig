//! False claim reports.
//!
//! A false claim report proves that a watchtower's ACK was mined against an operator's game, and
//! names a newer claim by the same operator that the admins should block. Only this one report
//! type exists. JSON (camelCase):
//!
//! ```json
//! {
//!   "type": "strata-false-claim-report",
//!   "version": 1,
//!   "network": "regtest",
//!   "createdAt": "2026-11-11T12:00:00Z",
//!   "operatorPubkey": "<x-only hex>",
//!   "newClaimTxid": "…",
//!   "newContestTxid": "…",
//!   "ackedClaimTxid": "…",
//!   "ackedContestTxid": "…",
//!   "counterproofAckTxid": "…"
//! }
//! ```
//!
//! `type`, `version` and `network` are optional and checked when present. `createdAt` is ignored. The
//! "acked" claim is the earlier claim of the same operator whose game ended in the ACK.
//!
//! # Verification
//!
//! Against mined transactions fetched by txid from the caller's Electrum server:
//!
//! 1. Every transaction is mined, and the ACK has at least [`ACK_MIN_CONFIRMATIONS`]
//!    confirmations. Reorganizations deeper than that are not handled.
//! 2. The acked contest spends the acked claim's claim-contest connector (claim output 0), and the
//!    new contest spends the new claim's. Each spend goes through a contest leaf
//!    `<N/N> OP_CHECKSIGVERIFY <watchtower> OP_CHECKSIG` whose N/N key is the trusted
//!    deployment N/N key, and the control block commits that leaf to the claim output. The
//!    contest therefore carries a valid N/N signature, which authenticates the chain.
//! 3. The mined watchtower ACK spends the authenticated contest's payout connector, skips its
//!    proof connector and has version 3, locktime 0, two inputs and one taproot output. In the
//!    v1 bridge graph these properties identify the ACK. Consensus requires the N/N signature
//!    to spend the payout connector. The counterproof and the ACK's other input are unchecked.
//! 4. Both claims pay their CPFP connector (output 3) to the operator's x-only key (a key-path
//!    only taproot output), which authenticates the operator.
//!
//! On top of that, the new claim's admin payout connector (output 1) must be the connector of the
//! N/N key its contest revealed. A report whose connector is already spent in a confirmed
//! transaction is reported as [`ReportVerdict::Spent`] and ignored.

use std::collections::HashMap;

use bitcoin::{
    absolute::LockTime,
    key::Secp256k1,
    relative,
    script::Instruction,
    taproot::{ControlBlock, LeafVersion},
    transaction::Version,
    Amount, OutPoint, Script, Transaction, Txid, XOnlyPublicKey,
};
use serde::{Deserialize, Serialize};
use strata_bridge_connectors::{
    prelude::{ClaimContestConnector, KeyedAnchor},
    Connector,
};
use strata_bridge_tx_graph::transactions::prelude::{ClaimTx, ContestTx, CounterproofAckTx};

use crate::deployment::{Covenant, Deployment};

pub const REPORT_TYPE: &str = "strata-false-claim-report";
pub const REPORT_VERSION: u32 = 1;
/// Largest accepted paste, in bytes.
pub const MAX_PASTE_BYTES: usize = 4_000_000;
/// Most reports accepted in one paste.
pub const MAX_REPORTS: usize = 10_000;

/// A false-claim report linking an operator's new claim to an earlier game with a counterproof ACK.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Report {
    fields: ReportFields,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReportFields {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    report_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    network: Option<String>,
    operator_pubkey: XOnlyPublicKey,
    new_claim_txid: Txid,
    new_contest_txid: Txid,
    acked_claim_txid: Txid,
    acked_contest_txid: Txid,
    counterproof_ack_txid: Txid,
}

impl Report {
    /// Identifies the ACK offered as evidence against the operator.
    pub fn counterproof_ack_txid(&self) -> Txid {
        self.fields.counterproof_ack_txid
    }

    /// Identifies the claim the admins are asked to block.
    pub fn new_claim_txid(&self) -> Txid {
        self.fields.new_claim_txid
    }

    /// The transactions verification needs, in report order.
    pub fn txids(&self) -> [Txid; 5] {
        [
            self.fields.new_claim_txid,
            self.fields.new_contest_txid,
            self.fields.acked_claim_txid,
            self.fields.acked_contest_txid,
            self.fields.counterproof_ack_txid,
        ]
    }

    /// The admin payout connector this report asks to block.
    pub fn blocked_outpoint(&self) -> OutPoint {
        OutPoint::new(self.fields.new_claim_txid, ClaimTx::ADMIN_PAYOUT_VOUT)
    }
}

/// A paste item that could not be read as a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MalformedReport {
    /// 0-based position in the paste.
    position: usize,
    error: String,
}

/// Collects the readable false-claim reports and the failures for individual paste items.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedPaste {
    reports: Vec<Report>,
    malformed: Vec<MalformedReport>,
}

impl ParsedPaste {
    /// Separates the parsed reports from the malformed items in the same paste.
    pub fn into_parts(self) -> (Vec<Report>, Vec<MalformedReport>) {
        (self.reports, self.malformed)
    }
}

/// A failure that prevents the paste from being processed as a batch.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PasteError {
    #[error("the pasted text is too large")]
    TooLarge,
    #[error("more than {MAX_REPORTS} reports in one paste")]
    TooMany,
    #[error("nothing to read: paste one or more false claim reports in JSON")]
    Empty,
    #[error("the pasted text is not JSON: {0}")]
    NotJson(String),
}

/// Reads one report, a JSON array of reports, or several reports one after another (e.g. one per
/// line). Items that are not well-formed reports are returned in `malformed`; the rest are kept.
pub fn parse_paste(text: &str, network: bitcoin::Network) -> Result<ParsedPaste, PasteError> {
    if text.len() > MAX_PASTE_BYTES {
        return Err(PasteError::TooLarge);
    }

    let mut values = Vec::new();
    for value in serde_json::Deserializer::from_str(text).into_iter::<serde_json::Value>() {
        match value.map_err(|error| PasteError::NotJson(error.to_string()))? {
            serde_json::Value::Array(items) => values.extend(items),
            value => values.push(value),
        }

        if values.len() > MAX_REPORTS {
            return Err(PasteError::TooMany);
        }
    }

    if values.is_empty() {
        return Err(PasteError::Empty);
    }

    let mut parsed = ParsedPaste::default();
    for (position, value) in values.into_iter().enumerate() {
        match Report::from_value(value, network) {
            Ok(report) => parsed.reports.push(report),
            Err(error) => parsed.malformed.push(MalformedReport { position, error }),
        }
    }

    Ok(parsed)
}

impl Report {
    /// Reads and checks a single pasted report for the given network.
    pub fn from_value(
        value: serde_json::Value,
        network: bitcoin::Network,
    ) -> Result<Report, String> {
        if let Some(version) = value.get("version").and_then(serde_json::Value::as_u64) {
            if version != u64::from(REPORT_VERSION) {
                return Err(format!(
                    "unsupported report version {version}; this app reads version {REPORT_VERSION}"
                ));
            }
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Incoming {
            #[serde(rename = "createdAt")]
            _created_at: Option<serde::de::IgnoredAny>,
            #[serde(flatten)]
            fields: ReportFields,
        }

        let Incoming { fields, .. } =
            serde_json::from_value(value).map_err(|error| error.to_string())?;

        if let Some(report_type) = &fields.report_type {
            if report_type != REPORT_TYPE {
                return Err(format!(
                    "type must be \"{REPORT_TYPE}\", got \"{report_type}\""
                ));
            }
        }

        if let Some(network_name) = &fields.network {
            let parsed = bitcoin::Network::from_core_arg(network_name)
                .or_else(|_| network_name.parse::<bitcoin::Network>())
                .map_err(|_| format!("unknown network \"{network_name}\""))?;

            if parsed != network {
                return Err(format!("report is for {parsed}, this app is on {network}"));
            }
        }

        if fields.new_claim_txid == fields.acked_claim_txid {
            return Err("the new claim and the acked claim are the same transaction".into());
        }

        Ok(Self { fields })
    }
}

/// A transaction as the admin's Electrum server returns it.
#[derive(Debug, Clone)]
pub struct MinedTx {
    tx: Transaction,
    /// `None` when the Electrum server has the transaction only in its mempool.
    block: Option<MinedBlock>,
}

impl MinedTx {
    /// Returns the mining details, if the transaction is confirmed.
    pub fn block(&self) -> Option<MinedBlock> {
        self.block
    }

    /// Returns the transaction reported by Electrum.
    pub fn tx(&self) -> &Transaction {
        &self.tx
    }

    /// Records a transaction and its optional block information.
    pub fn new(tx: Transaction, block: Option<MinedBlock>) -> Self {
        Self { tx, block }
    }
}

/// Block height and confirmation count reported for a mined transaction.
#[derive(Debug, Clone, Copy)]
pub struct MinedBlock {
    height: u64,
    /// As the Electrum server reports them: 1 for a transaction in the tip block.
    confirmations: u64,
}

impl MinedBlock {
    /// Depth reported by Electrum, counting the block containing the transaction.
    pub fn confirmations(&self) -> u64 {
        self.confirmations
    }

    /// Records a confirmed transaction's block height and reported depth.
    pub fn new(height: u64, confirmations: u64) -> Self {
        Self {
            height,
            confirmations,
        }
    }
}

/// A report counts only once its ACK is this deep: the ACK is the proof of the false claim, and a
/// shallow one could still be reorganized away.
pub const ACK_MIN_CONFIRMATIONS: u64 = 6;

/// Whether verification accepts the report, rejects it, or finds its connector already spent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportVerdict {
    Valid,
    Invalid(String),
    /// The connector was already spent in a confirmed transaction; ignored.
    Spent(String),
}

impl Serialize for ReportVerdict {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let (verdict, reasons): (_, &[String]) = match self {
            Self::Valid => ("valid", &[]),
            Self::Invalid(reason) => ("invalid", std::slice::from_ref(reason)),
            Self::Spent(reason) => ("spent", std::slice::from_ref(reason)),
        };
        let mut fields = serializer.serialize_struct("ReportVerdict", 2)?;
        fields.serialize_field("verdict", verdict)?;
        fields.serialize_field("reasons", reasons)?;

        fields.end()
    }
}

/// Pairs the submitted report with its verification result, including the reason when skipped or rejected.
#[derive(Debug, Clone)]
pub struct VerifiedReport {
    key: Option<bitcoin::secp256k1::PublicKey>,
    report: Report,
    verdict: ReportVerdict,
    new_claim_height: Option<u64>,
}

impl VerifiedReport {
    pub fn n_of_n(&self) -> Option<bitcoin::secp256k1::PublicKey> {
        self.key
    }

    /// The decision on blocking this claim and any reason it was skipped or rejected.
    pub fn verdict(&self) -> &ReportVerdict {
        &self.verdict
    }

    /// The new claim’s admin payout connector.
    pub fn new_claim_admin_payout_outpoint(&self) -> OutPoint {
        self.report.blocked_outpoint()
    }
}

impl Serialize for VerifiedReport {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct View<'a> {
            report: &'a Report,
            #[serde(flatten)]
            verdict: &'a ReportVerdict,
            new_claim_admin_payout_outpoint: OutPoint,
            new_claim_height: Option<u64>,
        }

        View {
            report: &self.report,
            verdict: &self.verdict,
            new_claim_admin_payout_outpoint: self.new_claim_admin_payout_outpoint(),
            new_claim_height: self.new_claim_height,
        }
        .serialize(serializer)
    }
}

/// What the Electrum server says about the blocked connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorState {
    Unspent,
    /// Spent by a confirmed transaction.
    SpentConfirmed,
}

/// Extract the keys needed to reconstruct the bridge leaf, then compare its exact encoding.
fn contest_leaf_n_of_n(network: bitcoin::Network, leaf: &Script) -> Option<XOnlyPublicKey> {
    let mut instructions = leaf.instructions();
    let key = |instruction: Instruction<'_>| {
        XOnlyPublicKey::from_slice(instruction.push_bytes()?.as_bytes()).ok()
    };
    let n_of_n = key(instructions.next()?.ok()?)?;
    let watchtower = key(instructions.nth(1)?.ok()?)?;
    let connector = ClaimContestConnector::new(
        network,
        n_of_n,
        vec![watchtower],
        relative::Height::ZERO,
        Amount::ZERO,
    );

    (connector.leaf_scripts()[0] == *leaf).then_some(n_of_n)
}

/// Leaf script and control block of a script-path witness (annex aware).
fn script_path_parts(witness: &bitcoin::Witness) -> Option<(&Script, ControlBlock)> {
    let mut items: Vec<&[u8]> = witness.iter().collect();

    if items.len() >= 2
        && items
            .last()
            .is_some_and(|annex| annex.first() == Some(&0x50))
    {
        items.pop();
    }

    if items.len() < 2 {
        return None;
    }

    let control_block = ControlBlock::decode(items[items.len() - 1]).ok()?;
    let script = Script::from_bytes(items[items.len() - 2]);

    Some((script, control_block))
}

/// Checks that `contest` spends `claim:0` through a contest leaf committed to that output, and
/// returns the trusted covenant whose N/N key the leaf uses.
fn check_contest_spends_claim<'a>(
    deployment: &'a Deployment,
    which: &str,
    claim: &Transaction,
    contest: &Transaction,
) -> Result<&'a Covenant, String> {
    let claim_txid = claim.compute_txid();
    let outpoint = OutPoint::new(claim_txid, ClaimTx::CONTEST_VOUT);
    let input = contest
        .input
        .iter()
        .find(|input| input.previous_output == outpoint)
        .ok_or_else(|| {
            format!("The {which} contest does not spend output 0 of the {which} claim.")
        })?;
    let prevout = claim
        .output
        .get(ClaimTx::CONTEST_VOUT as usize)
        .ok_or_else(|| format!("The {which} claim has no output 0."))?;

    if !prevout.script_pubkey.is_p2tr() {
        return Err(format!(
            "Output 0 of the {which} claim is not a taproot output."
        ));
    }

    let output_key = XOnlyPublicKey::from_slice(&prevout.script_pubkey.as_bytes()[2..34])
        .map_err(|_| format!("Output 0 of the {which} claim has an invalid taproot key."))?;
    let (leaf, control_block) = script_path_parts(&input.witness).ok_or_else(|| {
        format!("The {which} contest does not spend the claim-contest connector through a leaf.")
    })?;

    if control_block.leaf_version != LeafVersion::TapScript {
        return Err(format!("The {which} contest uses an unknown leaf version."));
    }

    let secp = Secp256k1::verification_only();

    if !control_block.verify_taproot_commitment(&secp, output_key, leaf) {
        return Err(format!(
            "The {which} contest's leaf is not committed to the {which} claim's output 0."
        ));
    }

    let n_of_n = contest_leaf_n_of_n(deployment.network(), leaf).ok_or_else(|| {
        format!("The {which} contest did not spend the claim through a contest leaf (N/N + watchtower).")
    })?;

    deployment
        .covenants()
        .iter()
        .find(|covenant| covenant.n_of_n_xonly() == n_of_n)
        .ok_or_else(|| {
            format!(
                "The {which} claim's contest connector uses N/N key {n_of_n}, which is absent from \
                 ASM history."
            )
        })
}

fn spends(transaction: &Transaction, outpoint: OutPoint) -> bool {
    transaction
        .input
        .iter()
        .any(|input| input.previous_output == outpoint)
}

/// Identifies the ACK by its contest outpoints and transaction shape.
fn check_ack_of_contest(contest: &Transaction, ack: &Transaction) -> Result<(), String> {
    let contest_txid = contest.compute_txid();

    if !spends(ack, OutPoint::new(contest_txid, ContestTx::PAYOUT_VOUT)) {
        return Err(
            "The ack does not spend the acked contest's payout connector, so it is not an ACK."
                .into(),
        );
    }

    if spends(ack, OutPoint::new(contest_txid, ContestTx::PROOF_VOUT)) {
        return Err(
            "The ack also spends the acked contest's proof connector, so it is the bridge proof \
             timeout, not an ACK."
                .into(),
        );
    }

    let shape = ack.version == Version(3)
        && ack.lock_time == LockTime::ZERO
        && ack.input.len() == CounterproofAckTx::N_INPUTS
        && ack.output.len() == 1
        && ack.output[0].script_pubkey.is_p2tr();

    if !shape {
        return Err(
            "The ack does not have the shape of a counterproof ack (version 3, locktime 0, two \
             inputs, one taproot output), so it is not an ACK."
                .into(),
        );
    }

    Ok(())
}

/// Verifies one report. `transactions` holds whatever the Electrum server returned for [`Report::txids`]
/// (missing entries mean the Electrum server does not know the transaction). `connector` is the Electrum server's
/// answer for the blocked outpoint (confirmed spends only), if known.
pub fn verify_report(
    deployment: &Deployment,
    report: &Report,
    transactions: &HashMap<Txid, MinedTx>,
    connector: Option<ConnectorState>,
) -> VerifiedReport {
    let mut verified_report = VerifiedReport {
        key: None,
        report: report.clone(),
        verdict: ReportVerdict::Valid,
        new_claim_height: None,
    };

    if let Err(reason) = check(deployment, report, transactions, &mut verified_report) {
        verified_report.verdict = ReportVerdict::Invalid(reason);

        return verified_report;
    }

    match connector {
        Some(ConnectorState::SpentConfirmed) => {
            verified_report.verdict = ReportVerdict::Spent(
                "The claim's admin payout connector is already spent in a confirmed transaction."
                    .into(),
            );
        }
        Some(ConnectorState::Unspent) | None => verified_report.verdict = ReportVerdict::Valid,
    }

    verified_report
}

fn check(
    deployment: &Deployment,
    report: &Report,
    transactions: &HashMap<Txid, MinedTx>,
    verified_report: &mut VerifiedReport,
) -> Result<(), String> {
    // 1. Every transaction is mined.
    let names = [
        "new claim",
        "new contest",
        "acked claim",
        "acked contest",
        "ack",
    ];
    let mut mined_transactions = Vec::with_capacity(names.len());
    for (txid, name) in report.txids().iter().zip(names) {
        let mined_transaction = transactions.get(txid).ok_or_else(|| {
            format!("Your Electrum server does not know the {name} transaction {txid}.")
        })?;

        if mined_transaction.tx.compute_txid() != *txid {
            return Err(format!(
                "The Electrum server returned a different transaction for the {name}."
            ));
        }

        if mined_transaction.block.is_none() {
            return Err(format!("The {name} transaction {txid} is not mined yet."));
        }

        mined_transactions.push(mined_transaction);
    }
    let [new_claim, new_contest, acked_claim, acked_contest, ack] = [
        mined_transactions[0],
        mined_transactions[1],
        mined_transactions[2],
        mined_transactions[3],
        mined_transactions[4],
    ];
    let confirmations = ack.block.map_or(0, |block| block.confirmations);

    if confirmations < ACK_MIN_CONFIRMATIONS {
        return Err(format!(
            "The ack transaction {} has {} of the {ACK_MIN_CONFIRMATIONS} confirmations required. \
             Paste the report again later.",
            report.txids()[4],
            confirmations
        ));
    }

    verified_report.new_claim_height = new_claim.block.map(|block| block.height);

    // 2. Both contests spend their claim's contest connector through an N/N leaf.
    let new_covenant =
        check_contest_spends_claim(deployment, "new", &new_claim.tx, &new_contest.tx)?;
    check_contest_spends_claim(deployment, "acked", &acked_claim.tx, &acked_contest.tx)?;

    // 3. The ack is the watchtower ACK of the acked contest.
    check_ack_of_contest(&acked_contest.tx, &ack.tx)?;

    // 4. Both claims pay the CPFP anchor to the operator key.
    let anchor = KeyedAnchor::new(
        deployment.network(),
        report.fields.operator_pubkey,
        Amount::ZERO,
    )
    .script_pubkey();
    for (name, claim) in [("new", new_claim), ("acked", acked_claim)] {
        let output = claim.tx.output.get(ClaimTx::CPFP_VOUT as usize);

        if output.map(|output| &output.script_pubkey) != Some(&anchor) {
            return Err(format!(
                "The {name} claim's CPFP output is not locked to operator key {}.",
                report.fields.operator_pubkey
            ));
        }
    }

    // The connector to spend is the one of the new claim's N/N key.
    let output = new_claim
        .tx
        .output
        .get(ClaimTx::ADMIN_PAYOUT_VOUT as usize)
        .ok_or("The new claim has no output 1.")?;

    if output.script_pubkey != new_covenant.connector_script() {
        return Err(
            "Output 1 of the new claim is not the admin payout connector of its N/N key and this \
             admin set."
                .into(),
        );
    }

    if output.value != new_covenant.connector().value() {
        return Err(format!(
            "The new claim's admin payout connector holds {} instead of {}.",
            output.value,
            new_covenant.connector().value()
        ));
    }

    verified_report.key = Some(new_covenant.n_of_n());

    Ok(())
}

/// Verifies many reports. Reports that block the same connector are merged: the first valid one
/// wins. Results keep first-seen order, one per blocked connector.
pub fn verify_reports(
    deployment: &Deployment,
    reports: &[Report],
    transactions: &HashMap<Txid, MinedTx>,
    connectors: &HashMap<OutPoint, ConnectorState>,
) -> Vec<VerifiedReport> {
    let mut positions = HashMap::new();
    let mut results: Vec<VerifiedReport> = Vec::new();
    for report in reports {
        let outpoint = report.blocked_outpoint();
        let index = *positions.entry(outpoint).or_insert(results.len());
        let result = verify_report(
            deployment,
            report,
            transactions,
            connectors.get(&outpoint).copied(),
        );
        match results.get_mut(index) {
            Some(previous) if matches!(previous.verdict, ReportVerdict::Invalid(_)) => {
                *previous = result
            }
            None => results.push(result),
            _ => {}
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    impl VerifiedReport {
        fn invalid_reason(&self) -> &str {
            match &self.verdict {
                ReportVerdict::Invalid(reason) => reason,
                other => panic!("expected invalid report, got {other:?}"),
            }
        }
    }

    fn setup() -> (Fixture, Deployment, GameTxs) {
        let fixture = Fixture::new(3, 2);
        let deployment = fixture.deployment();
        let game = fixture.game(&deployment, 0);

        (fixture, deployment, game)
    }

    #[test]
    fn a_genuine_report_is_valid() {
        let (_fixture, deployment, game) = setup();
        let verified_report = verify_report(
            &deployment,
            &game.report(),
            &game.lookup(),
            Some(ConnectorState::Unspent),
        );
        assert_eq!(verified_report.verdict, ReportVerdict::Valid);
        let json = serde_json::to_value(verified_report).unwrap();
        assert_eq!(
            json["newClaimAdminPayoutOutpoint"],
            game.report().blocked_outpoint().to_string()
        );
        assert_eq!(json["verdict"], "valid");
        assert_eq!(json["reasons"], serde_json::json!([]));
    }

    #[test]
    fn a_spent_connector_is_ignored() {
        let (_fixture, deployment, game) = setup();
        let verified_report = verify_report(
            &deployment,
            &game.report(),
            &game.lookup(),
            Some(ConnectorState::SpentConfirmed),
        );
        assert!(matches!(verified_report.verdict, ReportVerdict::Spent(_)));
    }

    #[test]
    fn an_ack_with_fewer_than_six_confirmations_is_refused() {
        let (_fixture, deployment, game) = setup();
        let ack = game.report().txids()[4];
        let verdict = |confirmations| {
            let mut lookup = game.lookup();
            lookup
                .get_mut(&ack)
                .unwrap()
                .block
                .as_mut()
                .unwrap()
                .confirmations = confirmations;

            verify_report(&deployment, &game.report(), &lookup, None)
        };
        let verified_report = verdict(3);
        assert!(
            verified_report
                .invalid_reason()
                .contains("has 3 of the 6 confirmations required"),
            "{:?}",
            verified_report.verdict
        );
        assert_eq!(verdict(6).verdict, ReportVerdict::Valid);
    }

    #[test]
    fn every_transaction_must_be_mined() {
        let (_fixture, deployment, game) = setup();
        for txid in game.report().txids() {
            let mut lookup = game.lookup();
            lookup.get_mut(&txid).unwrap().block = None;
            let verified_report = verify_report(&deployment, &game.report(), &lookup, None);
            assert!(
                verified_report.invalid_reason().contains("not mined"),
                "{:?}",
                verified_report.verdict
            );
            let mut lookup = game.lookup();
            lookup.remove(&txid);
            let verified_report = verify_report(&deployment, &game.report(), &lookup, None);
            assert!(
                verified_report.invalid_reason().contains("does not know"),
                "{:?}",
                verified_report.verdict
            );
        }
    }

    #[test]
    fn acked_claim_can_use_an_older_key() {
        let fixture = Fixture::new(3, 2);
        let deployment = fixture.after_operator_removal();
        let current = deployment.covenant().n_of_n();
        let old = deployment.covenants()[0].n_of_n();
        assert_ne!(old, current);
        let game = GameTxs::build(&fixture, &deployment, 0, old, current);
        let checked = verify_report(&deployment, &game.report(), &game.lookup(), None);
        assert_eq!(checked.verdict(), &ReportVerdict::Valid);
        assert_eq!(checked.n_of_n(), Some(current));
    }

    #[test]
    fn an_untrusted_n_of_n_is_rejected() {
        let (fixture, deployment, _) = setup();
        // The same game built with an N/N key absent from the ASM history.
        let game = fixture.game_with_n_of_n(&deployment, 0, fixture.foreign_n_of_n());
        let verified_report = verify_report(&deployment, &game.report(), &game.lookup(), None);
        assert!(
            verified_report
                .invalid_reason()
                .contains("absent from ASM history"),
            "{:?}",
            verified_report.verdict
        );
    }

    #[test]
    fn an_acked_claim_under_an_untrusted_n_of_n_is_rejected() {
        let (fixture, deployment, _) = setup();
        // Only the acked game uses a key absent from the ASM history; the new claim is trusted.
        let game = GameTxs::build(
            &fixture,
            &deployment,
            0,
            fixture.foreign_n_of_n(),
            deployment.covenant().n_of_n(),
        );
        let verified_report = verify_report(&deployment, &game.report(), &game.lookup(), None);
        assert!(
            verified_report
                .invalid_reason()
                .contains("The acked claim's contest connector uses N/N key"),
            "{:?}",
            verified_report.verdict
        );
    }

    fn with_wrong_claim_operator(acked: bool) -> (Deployment, GameTxs) {
        let (fixture, deployment, mut game) = setup();
        game.change_claim_operator(acked, deployment.network(), fixture.operator_xonly(1));

        (deployment, game)
    }

    #[test]
    fn only_the_new_claim_has_the_wrong_operator() {
        let (deployment, game) = with_wrong_claim_operator(false);
        let verified_report = verify_report(
            &deployment,
            &game.report(),
            &game.lookup(),
            Some(ConnectorState::Unspent),
        );
        assert!(
            verified_report
                .invalid_reason()
                .contains("new claim's CPFP"),
            "{:?}",
            verified_report.verdict
        );
    }

    #[test]
    fn only_the_acked_claim_has_the_wrong_operator() {
        let (deployment, game) = with_wrong_claim_operator(true);
        let verified_report = verify_report(
            &deployment,
            &game.report(),
            &game.lookup(),
            Some(ConnectorState::Unspent),
        );
        assert!(
            verified_report
                .invalid_reason()
                .contains("acked claim's CPFP"),
            "{:?}",
            verified_report.verdict
        );
    }

    #[test]
    fn a_nack_is_not_an_ack() {
        let (fixture, deployment, game) = setup();
        let mut other_game = game.clone();
        other_game.replace_ack_with_nack(&fixture);
        let verified_report = verify_report(
            &deployment,
            &other_game.report(),
            &other_game.lookup(),
            None,
        );
        assert!(
            verified_report.invalid_reason().contains("not an ACK"),
            "{:?}",
            verified_report.verdict
        );
    }

    #[test]
    fn mismatched_chain_links_are_rejected() {
        let (fixture, deployment, game) = setup();
        let other = fixture.game(&deployment, 1);
        let mut lookup = game.lookup();
        lookup.extend(other.lookup());
        // An ack of a different game.
        let mut report = game.report();
        report.fields.counterproof_ack_txid = other.report().fields.counterproof_ack_txid;
        let verified_report = verify_report(&deployment, &report, &lookup, None);
        assert!(
            verified_report
                .invalid_reason()
                .contains("does not spend the acked contest's payout connector"),
            "{:?}",
            verified_report.verdict
        );
        // New contest from a different game.
        let mut report = game.report();
        report.fields.new_contest_txid = other.report().fields.new_contest_txid;
        let verified_report = verify_report(&deployment, &report, &lookup, None);
        assert!(
            verified_report
                .invalid_reason()
                .contains("new contest does not spend"),
            "{:?}",
            verified_report.verdict
        );
    }

    #[test]
    fn the_ack_must_be_told_from_the_two_other_spenders_of_the_payout_connector() {
        let (_fixture, deployment, game) = setup();
        let contest_txid = game.acked_contest_txid();
        let verdict =
            |game: &GameTxs| verify_report(&deployment, &game.report(), &game.lookup(), None);

        // The bridge proof timeout also spends the proof connector.
        let mut tampered_game = game.clone();
        tampered_game.tamper_ack(|acknowledgment| {
            acknowledgment.input[0].previous_output =
                OutPoint::new(contest_txid, ContestTx::PROOF_VOUT)
        });
        assert!(
            verdict(&tampered_game)
                .invalid_reason()
                .contains("proof timeout"),
            "{:?}",
            verdict(&tampered_game).verdict
        );

        // The contested payout has more inputs.
        let mut tampered_game = game.clone();
        tampered_game.tamper_ack(|acknowledgment| {
            let extra = acknowledgment.input[0].clone();
            acknowledgment.input.push(bitcoin::TxIn {
                previous_output: OutPoint::new(contest_txid, 2),
                ..extra
            });
        });
        assert!(
            verdict(&tampered_game).invalid_reason().contains("shape"),
            "{:?}",
            verdict(&tampered_game).verdict
        );

        // Version and locktime are part of the shape.
        let mut tampered_game = game.clone();
        tampered_game
            .tamper_ack(|acknowledgment| acknowledgment.version = bitcoin::transaction::Version(2));
        assert!(verdict(&tampered_game).invalid_reason().contains("shape"));
        let mut tampered_game = game.clone();
        tampered_game.tamper_ack(|acknowledgment| {
            acknowledgment.lock_time = bitcoin::absolute::LockTime::from_height(5).unwrap()
        });
        assert!(verdict(&tampered_game).invalid_reason().contains("shape"));
    }

    #[test]
    fn a_report_of_another_version_is_refused_with_a_clear_message() {
        let (_fixture, deployment, game) = setup();
        let mut other = serde_json::to_value(game.report()).unwrap();
        other["version"] = serde_json::json!(2);
        let parsed = parse_paste(&other.to_string(), deployment.network()).unwrap();
        assert!(parsed.reports.is_empty());
        assert!(
            parsed.malformed[0]
                .error
                .contains("unsupported report version 2; this app reads version 1"),
            "{:?}",
            parsed.malformed
        );
        // The field names of an earlier draft are refused too, with or without a version.
        let fields = game.report().fields;
        let mut draft = serde_json::json!({
            "type": "strata-false-claim-report", "version": 1, "network": "regtest",
            "operatorPubkey": fields.operator_pubkey,
            "newClaimTxid": fields.new_claim_txid,
            "newContestTxid": fields.new_contest_txid,
            "oldClaimTxid": fields.acked_claim_txid,
            "oldContestTxid": fields.acked_contest_txid,
            "oldCounterproofTxid": game.counterproof_txid(),
            "oldCounterproofAckTxid": fields.counterproof_ack_txid,
        });
        let parsed = parse_paste(&draft.to_string(), deployment.network()).unwrap();
        assert!(parsed.reports.is_empty() && parsed.malformed.len() == 1);
        draft.as_object_mut().unwrap().remove("version");
        let parsed = parse_paste(&draft.to_string(), deployment.network()).unwrap();
        assert!(parsed.reports.is_empty() && parsed.malformed.len() == 1);
    }

    #[test]
    fn parses_single_array_and_stream_pastes() {
        let (_fixture, deployment, game) = setup();
        let mut incoming = serde_json::to_value(game.report()).unwrap();
        incoming["createdAt"] = "2026-11-11T00:00:00Z".into();
        let one = incoming.to_string();
        incoming["unknown"] = true.into();
        assert!(Report::from_value(incoming, deployment.network()).is_err());
        let parsed = parse_paste(&one, deployment.network()).unwrap();
        assert_eq!(parsed.reports.len(), 1);
        let array = format!("[{one},{one}]");
        assert_eq!(
            parse_paste(&array, deployment.network())
                .unwrap()
                .reports
                .len(),
            2
        );
        let stream = format!("{one}\n{one}\n{{\"nope\":1}}");
        let parsed = parse_paste(&stream, deployment.network()).unwrap();
        assert_eq!(parsed.reports.len(), 2);
        assert_eq!(parsed.malformed.len(), 1);
        assert_eq!(parsed.malformed[0].position, 2);
        assert!(matches!(
            parse_paste("hello", deployment.network()),
            Err(PasteError::NotJson(_))
        ));
        assert_eq!(
            parse_paste("  ", deployment.network()),
            Err(PasteError::Empty)
        );
    }

    #[test]
    fn optional_fields_must_match() {
        let (_fixture, deployment, game) = setup();
        let mut report = serde_json::to_value(game.report()).unwrap();
        report["network"] = "bitcoin".into();
        let parsed = parse_paste(
            &serde_json::to_string(&report).unwrap(),
            deployment.network(),
        )
        .unwrap();
        assert!(parsed.malformed[0].error.contains("report is for"));
        let mut report = serde_json::to_value(game.report()).unwrap();
        report["type"] = "other".into();
        let parsed = parse_paste(
            &serde_json::to_string(&report).unwrap(),
            deployment.network(),
        )
        .unwrap();
        assert!(parsed.malformed[0].error.contains("type must be"));
        // Unknown fields mean a different kind of report.
        let mut json = serde_json::to_value(game.report()).unwrap();
        json["proof"] = serde_json::json!({});
        let parsed = parse_paste(&json.to_string(), deployment.network()).unwrap();
        assert!(parsed.malformed[0].error.contains("unknown field"));
    }

    #[test]
    fn duplicate_reports_merge() {
        let (_fixture, deployment, game) = setup();
        let mut bad = game.report();
        bad.fields.counterproof_ack_txid = game.report().fields.acked_contest_txid;
        let result = verify_reports(
            &deployment,
            &[bad, game.report()],
            &game.lookup(),
            &HashMap::new(),
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].verdict, ReportVerdict::Valid);
    }
}
