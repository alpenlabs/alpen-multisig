//! False claim reports.
//!
//! A false claim report proves that a watchtower's ACK was mined against an operator's game, and
//! names claims by the same operator that the admins should block. Only this one report
//! type exists. JSON (camelCase):
//!
//! ```json
//! {
//!   "type": "strata-false-claim-report",
//!   "version": 2,
//!   "network": "regtest",
//!   "createdAt": "2026-11-11T12:00:00Z",
//!   "counterproofAckTxid": "…",
//!   "contestTxids": ["…", "…"]
//! }
//! ```
//!
//! `type` and `network` are optional and checked when present. `createdAt` is ignored. Version 2 is required.
//! Claims may precede or follow the ACK because fault is per operator.
//!
//! # Verification
//!
//! Against mined transactions fetched by txid from the caller's Electrum server:
//!
//! 1. Every transaction is mined, and the ACK has at least [`ACK_MIN_CONFIRMATIONS`]
//!    confirmations. Reorganizations deeper than that are not handled.
//! 2. Follow ACK input 1 to the authenticated acked contest. Its single
//!    input identifies the acked claim. Each listed contest likewise spends its claim's output 0.
//!    Each spend goes through a contest leaf
//!    `<N/N> OP_CHECKSIGVERIFY <watchtower> OP_CHECKSIG` whose N/N key is the trusted
//!    deployment N/N key, and the control block commits that leaf to the claim output. The
//!    contest therefore carries a valid N/N signature, which authenticates the chain.
//! 3. The mined watchtower ACK spends the authenticated contest's payout connector, skips its
//!    proof connector and has version 3, locktime 0, two inputs and one taproot output. In the
//!    v1 bridge graph these properties identify the ACK. Consensus requires the N/N signature
//!    to spend the payout connector. Input 0 references output 0 of a different transaction.
//!    The counterproof itself is unchecked.
//! 4. Each claim's CPFP script (output 3) must equal the acked claim's. The contest's N/N
//!    signature covers the claim txid and therefore commits to all its outputs.
//!
//! On top of that, the new claim's admin payout connector (output 1) must be the connector of the
//! N/N key its contest revealed. A report whose connector is already spent in a confirmed
//! transaction is reported as [`ReportVerdict::Spent`] and ignored.

use std::collections::{HashMap, HashSet};

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
use strata_bridge_connectors::{prelude::ClaimContestConnector, Connector};
use strata_bridge_tx_graph::transactions::prelude::{
    ClaimTx, ContestTx, CounterproofAckTx, CounterproofTx,
};

use crate::deployment::{Covenant, Deployment};

pub const REPORT_TYPE: &str = "strata-false-claim-report";
pub const REPORT_VERSION: u32 = 2;
/// Largest accepted paste, in bytes.
pub const MAX_PASTE_BYTES: usize = 4_000_000;
/// Most reports accepted in one paste.
pub const MAX_REPORTS: usize = 10_000;

/// A false-claim report linking an operator’s claims to a game with a counterproof ACK.
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
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    network: Option<String>,
    contest_txids: Vec<Txid>,
    counterproof_ack_txid: Txid,
}

impl Report {
    /// Identifies the ACK offered as evidence against the operator.
    pub fn counterproof_ack_txid(&self) -> Txid {
        self.fields.counterproof_ack_txid
    }

    /// Contests authenticating the claims to block, in report order.
    pub fn contest_txids(&self) -> &[Txid] {
        &self.fields.contest_txids
    }

    /// Fetch the ACK, contests, and their claims by following prevouts backwards.
    pub fn fetch_transactions<E>(
        &self,
        transactions: &mut HashMap<Txid, MinedTx>,
        mut fetch: impl FnMut(Txid) -> Result<Option<MinedTx>, E>,
    ) -> Result<(), E> {
        fn insert<E>(
            transactions: &mut HashMap<Txid, MinedTx>,
            txid: Txid,
            fetch: &mut impl FnMut(Txid) -> Result<Option<MinedTx>, E>,
        ) -> Result<(), E> {
            if let std::collections::hash_map::Entry::Vacant(entry) = transactions.entry(txid) {
                if let Some(transaction) = fetch(txid)? {
                    entry.insert(transaction);
                }
            }

            Ok(())
        }

        let ack = self.counterproof_ack_txid();
        insert(transactions, ack, &mut fetch)?;
        let mut contests = self.contest_txids().to_vec();

        if let Ok(contest) = mined(transactions, ack, "ack").and_then(|ack| acked_contest(&ack.tx))
        {
            contests.push(contest);
        }

        for contest in contests {
            insert(transactions, contest, &mut fetch)?;

            if let Ok(outpoint) = claim_outpoint(transactions, contest) {
                insert(transactions, outpoint.txid, &mut fetch)?;
            }
        }

        Ok(())
    }

    pub(crate) fn with_contests(&self, contest_txids: Vec<Txid>) -> Self {
        Self {
            fields: ReportFields {
                contest_txids,
                report_type: self.fields.report_type.clone(),
                network: self.fields.network.clone(),
                version: self.fields.version,
                counterproof_ack_txid: self.counterproof_ack_txid(),
            },
        }
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
    #[error("more than {MAX_REPORTS} reports or claims in one paste")]
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

    let mut claim_count = 0;
    let mut parsed = ParsedPaste::default();
    for (position, value) in values.into_iter().enumerate() {
        claim_count += value
            .get("contestTxids")
            .and_then(|claims| claims.as_array())
            .map_or(0, Vec::len);

        if MAX_REPORTS < claim_count {
            return Err(PasteError::TooMany);
        }

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
        if value.get("version").and_then(serde_json::Value::as_u64)
            != Some(u64::from(REPORT_VERSION))
        {
            return Err(
                "Version 2 is required. Version 1 and unversioned reports are no longer accepted."
                    .into(),
            );
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

        if fields.contest_txids.is_empty() || MAX_REPORTS < fields.contest_txids.len() {
            return Err(format!("Provide between 1 and {MAX_REPORTS} claims"));
        }

        let mut seen = HashSet::new();
        for contest in &fields.contest_txids {
            if !seen.insert(contest) {
                return Err(format!("Duplicate contest {contest}"));
            }
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
    outpoint: Option<OutPoint>,
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
    pub fn new_claim_admin_payout_outpoint(&self) -> Option<OutPoint> {
        self.outpoint
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
            new_claim_admin_payout_outpoint: Option<OutPoint>,
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

fn mined<'a>(
    transactions: &'a HashMap<Txid, MinedTx>,
    txid: Txid,
    name: &str,
) -> Result<&'a MinedTx, String> {
    let transaction = transactions.get(&txid).ok_or_else(|| {
        format!("Your Electrum server does not know the {name} transaction {txid}.")
    })?;

    if transaction.tx.compute_txid() != txid {
        return Err(format!(
            "The Electrum server returned a different transaction for the {name}."
        ));
    }

    if transaction.block.is_none() {
        return Err(format!("The {name} transaction {txid} is not mined yet."));
    }

    Ok(transaction)
}

fn acked_contest(ack: &Transaction) -> Result<Txid, String> {
    const ACK_COUNTERPROOF_INPUT: usize = 0;
    const ACK_CONTEST_INPUT: usize = 1;

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

    let counterproof = ack.input[ACK_COUNTERPROOF_INPUT].previous_output;
    let contest = ack.input[ACK_CONTEST_INPUT].previous_output;

    if contest.vout != ContestTx::PAYOUT_VOUT || counterproof.vout != CounterproofTx::ACK_NACK_VOUT
    {
        return Err("The inputs do not spend the required outputs, so this is not an ACK.".into());
    }

    if counterproof.txid == contest.txid {
        return Err("Both inputs spend the same contest, as in a bridge proof timeout.".into());
    }

    Ok(contest.txid)
}

/// Resolves the admin payout outpoint from the mined contest's single input.
pub fn claim_outpoint(
    transactions: &HashMap<Txid, MinedTx>,
    contest: Txid,
) -> Result<OutPoint, String> {
    let contest = mined(transactions, contest, "contest")?;
    let [input] = contest.tx.input.as_slice() else {
        return Err("The contest must have exactly one input.".into());
    };

    if input.previous_output.vout != ClaimTx::CONTEST_VOUT {
        return Err("The contest does not spend output 0 of its claim.".into());
    }

    Ok(OutPoint::new(
        input.previous_output.txid,
        ClaimTx::ADMIN_PAYOUT_VOUT,
    ))
}

fn check_evidence<'a>(
    deployment: &Deployment,
    report: &Report,
    transactions: &'a HashMap<Txid, MinedTx>,
) -> Result<(Txid, &'a Script), String> {
    let ack = mined(transactions, report.fields.counterproof_ack_txid, "ack")?;
    let confirmations = ack.block.map_or(0, |block| block.confirmations);

    if confirmations < ACK_MIN_CONFIRMATIONS {
        return Err(format!(
            "The ack transaction {} has {} of the {ACK_MIN_CONFIRMATIONS} confirmations required. \
             Paste the report again later.",
            report.fields.counterproof_ack_txid, confirmations
        ));
    }

    let ack = ack.tx();
    let contest_txid = acked_contest(ack)?;
    let contest = mined(transactions, contest_txid, "acked contest")?;
    let outpoint = claim_outpoint(transactions, contest_txid)?;
    let claim = mined(transactions, outpoint.txid, "acked claim")?;
    check_contest_spends_claim(deployment, "acked", &claim.tx, &contest.tx)?;

    let anchor = claim
        .tx
        .output
        .get(ClaimTx::CPFP_VOUT as usize)
        .ok_or("The acked claim has no CPFP output.")?;

    Ok((outpoint.txid, anchor.script_pubkey.as_script()))
}

fn check_claim(
    deployment: &Deployment,
    acked_claim: (Txid, &Script),
    outpoint: OutPoint,
    contest: Txid,
    transactions: &HashMap<Txid, MinedTx>,
    verified_report: &mut VerifiedReport,
) -> Result<(), String> {
    let new_claim = mined(transactions, outpoint.txid, "new claim")?;

    let (acked_claim_txid, anchor) = acked_claim;

    if outpoint.txid == acked_claim_txid {
        return Err("The report lists the acked claim as a claim to block.".into());
    }

    let new_contest = mined(transactions, contest, "new contest")?;
    let new_covenant =
        check_contest_spends_claim(deployment, "new", &new_claim.tx, &new_contest.tx)?;

    if !new_claim
        .tx
        .output
        .get(ClaimTx::CPFP_VOUT as usize)
        .is_some_and(|output| output.script_pubkey.as_script() == anchor)
    {
        return Err(
            "The new claim's CPFP script differs from the acked claim's CPFP script.".into(),
        );
    }

    verified_report.new_claim_height = new_claim.block.map(|block| block.height);

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
        let evidence = check_evidence(deployment, report, transactions);
        for &contest in report.contest_txids() {
            let resolved = claim_outpoint(transactions, contest);
            let outpoint = resolved.as_ref().ok().copied();
            let index = outpoint.map_or(results.len(), |outpoint| {
                *positions.entry(outpoint).or_insert(results.len())
            });
            let mut result = VerifiedReport {
                key: None,
                report: report.with_contests(vec![contest]),
                verdict: ReportVerdict::Valid,
                new_claim_height: None,
                outpoint,
            };
            let checked = resolved.and_then(|outpoint| {
                check_claim(
                    deployment,
                    evidence.clone()?,
                    outpoint,
                    contest,
                    transactions,
                    &mut result,
                )
            });
            result.verdict = match checked {
                Err(reason) => ReportVerdict::Invalid(reason),
                Ok(()) if outpoint.and_then(|outpoint| connectors.get(&outpoint)) == Some(&ConnectorState::SpentConfirmed) => {
                    ReportVerdict::Spent("The claim's admin payout connector is already spent in a confirmed transaction.".into())
                }
                Ok(()) => ReportVerdict::Valid,
            };

            match results.get_mut(index) {
                Some(previous) if matches!(previous.verdict, ReportVerdict::Invalid(_)) => {
                    *previous = result
                }
                None => results.push(result),
                _ => {}
            }
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

    fn verify_report(
        deployment: &Deployment,
        report: &Report,
        transactions: &HashMap<Txid, MinedTx>,
        connector: Option<ConnectorState>,
    ) -> VerifiedReport {
        let connectors = connector
            .into_iter()
            .filter_map(|state| {
                Some((
                    claim_outpoint(transactions, report.contest_txids()[0]).ok()?,
                    state,
                ))
            })
            .collect();

        verify_reports(
            deployment,
            std::slice::from_ref(report),
            transactions,
            &connectors,
        )
        .remove(0)
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
            OutPoint::new(game.claim_txid(), ClaimTx::ADMIN_PAYOUT_VOUT).to_string()
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
        let ack = game.report().counterproof_ack_txid();
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
        for txid in game.lookup().into_keys() {
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
    fn contests_must_have_one_input_spending_claim_output_zero() {
        let (_, deployment, game) = setup();
        let mut lookup = game.lookup();
        let mut contest = lookup[&game.report().contest_txids()[0]].tx.clone();
        let extra = contest.input[0].clone();
        contest.input.push(extra);

        for expected in ["exactly one input", "output 0"] {
            let txid = contest.compute_txid();
            lookup.insert(
                txid,
                MinedTx::new(contest.clone(), Some(MinedBlock::new(10, 6))),
            );
            let report = game.report().with_contests(vec![txid]);
            assert!(verify_report(&deployment, &report, &lookup, None)
                .invalid_reason()
                .contains(expected));
            contest.input.truncate(1);
            contest.input[0].previous_output.vout = ClaimTx::ADMIN_PAYOUT_VOUT;
        }
    }

    #[test]
    fn rejects_a_mismatched_transaction_from_electrum() {
        let (_, deployment, game) = setup();
        let mut lookup = game.lookup();
        let wrong = lookup[&game.claim_txid()].clone();
        lookup.insert(game.report().contest_txids()[0], wrong);
        assert!(verify_report(&deployment, &game.report(), &lookup, None)
            .invalid_reason()
            .contains("different transaction"));
    }

    #[test]
    fn rejects_ack_inputs_that_do_not_spend_the_required_outputs_in_order() {
        let (_, deployment, game) = setup();
        let assert_rejected = |game: &GameTxs| {
            let result = verify_report(&deployment, &game.report(), &game.lookup(), None);
            assert!(result.invalid_reason().contains("required outputs"));
        };

        let mut reversed = game.clone();
        reversed.tamper_ack(|ack| ack.input.swap(0, 1));
        assert_rejected(&reversed);

        let mut wrong_output = game;
        wrong_output.tamper_ack(|ack| ack.input[0].previous_output.vout = 1);
        assert_rejected(&wrong_output);
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
        other["version"] = serde_json::json!(1);
        let parsed = parse_paste(&other.to_string(), deployment.network()).unwrap();
        assert!(parsed.reports.is_empty());
        assert!(
            parsed.malformed[0].error.contains("Version 2 is required"),
            "{:?}",
            parsed.malformed
        );
        for field in [
            "operatorPubkey",
            "claims",
            "ackedClaimTxid",
            "ackedContestTxid",
        ] {
            let mut draft = serde_json::to_value(game.report()).unwrap();
            draft[field] = true.into();
            assert!(Report::from_value(draft, deployment.network())
                .unwrap_err()
                .contains("unknown field"));
        }
        let mut draft = serde_json::to_value(game.report()).unwrap();
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
    fn claims_must_be_distinct_from_each_other_and_the_acked_claim() {
        let (_, deployment, game) = setup();
        let original = game.report();
        let contest = original.contest_txids()[0];
        for contests in [vec![], vec![contest, contest]] {
            let mut value = serde_json::to_value(&original).unwrap();
            value["contestTxids"] = serde_json::json!(contests);
            assert!(Report::from_value(value, deployment.network()).is_err());
        }
        let acked = original.with_contests(vec![game.acked_contest_txid()]);
        assert!(verify_report(&deployment, &acked, &game.lookup(), None)
            .invalid_reason()
            .contains("acked claim"));
    }

    #[test]
    fn checks_shared_evidence_then_each_claim_regardless_of_height() {
        let (fixture, deployment, game) = setup();
        let spent = fixture.game(&deployment, 1);
        let unknown = fixture.game(&deployment, 2);
        let first = game.report().contest_txids()[0];
        let second = spent.report().contest_txids()[0];
        let third = unknown.report().contest_txids()[0];
        let report = game.report().with_contests(vec![first, second, third]);
        let mut lookup = game.lookup();
        lookup.extend(spent.lookup());
        lookup
            .get_mut(&game.claim_txid())
            .unwrap()
            .block
            .as_mut()
            .unwrap()
            .height = 2;
        let connectors = HashMap::from([(
            claim_outpoint(&lookup, second).unwrap(),
            ConnectorState::SpentConfirmed,
        )]);
        let checked = verify_reports(&deployment, &[report], &lookup, &connectors);
        assert_eq!(checked.len(), 3);
        assert_eq!(checked[0].verdict, ReportVerdict::Valid);
        assert!(matches!(checked[1].verdict, ReportVerdict::Spent(_)));
        assert!(checked[2].invalid_reason().contains("does not know"));
    }

    #[test]
    fn duplicate_reports_merge() {
        let (_fixture, deployment, game) = setup();
        let mut bad = game.report();
        bad.fields.counterproof_ack_txid = game.acked_contest_txid();
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
