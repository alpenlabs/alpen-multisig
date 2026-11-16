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
//! `type`, `version`, `network` and `createdAt` are optional; when present they must match. The
//! "acked" claim is the earlier claim of the same operator whose game ended in the ACK.
//!
//! # Verification
//!
//! Against mined transactions fetched by txid from the admin's own Bitcoin Core node (`-txindex`):
//!
//! 1. Every transaction is mined, and the ACK has at least [`ACK_MIN_CONFIRMATIONS`]
//!    confirmations. Reorganisations deeper than that are not handled.
//! 2. The acked contest spends the acked claim's claim-contest connector (claim output 0), and the
//!    new contest spends the new claim's. Each spend goes through a contest leaf
//!    `<N/N> OP_CHECKSIGVERIFY <watchtower> OP_CHECKSIG` whose N/N key is one of the trusted
//!    `params.toml` covenants, and the control block commits that leaf to the claim output. The
//!    contest therefore carries a valid N/N signature, which authenticates the chain.
//! 3. The ack is a watchtower ACK of the acked contest, as the bridge's `is_ack_of` tells it from
//!    the two other spenders of the contest's payout connector (the bridge proof timeout and the
//!    contested payout): it spends the contest's payout connector but not its proof connector, it
//!    is a version 3 transaction with locktime 0, two inputs and one taproot output, and its
//!    counterproof input reveals the timeout leaf `<N/N> OP_CHECKSIGVERIFY <delta> OP_CSV` with
//!    the N/N key of the acked contest's covenant. For a mined transaction consensus has already
//!    validated that spend, so that key proves the covenant signed the ack. The counterproof
//!    itself is not needed.
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
    opcodes::all::{OP_CHECKSIG, OP_CHECKSIGVERIFY, OP_CSV},
    script::Instruction,
    taproot::{ControlBlock, LeafVersion},
    transaction::Version,
    Amount, OutPoint, Script, Transaction, Txid, XOnlyPublicKey,
};
use serde::{Deserialize, Serialize};
use strata_bridge_connectors::{prelude::KeyedAnchor, Connector};

use crate::deployment::{Covenant, Deployment};

pub const REPORT_TYPE: &str = "strata-false-claim-report";
pub const REPORT_VERSION: u32 = 1;
/// Largest accepted paste, in bytes.
pub const MAX_PASTE_BYTES: usize = 4_000_000;
/// Most reports accepted in one paste.
pub const MAX_REPORTS: usize = 10_000;

/// Claim transaction output indices (bridge `ClaimTx`).
pub const CLAIM_CONTEST_VOUT: u32 = 0;
pub const CLAIM_ADMIN_PAYOUT_VOUT: u32 = 1;
pub const CLAIM_CPFP_VOUT: u32 = 3;
/// Contest transaction output indices (bridge `ContestTx`).
pub const CONTEST_PROOF_VOUT: u32 = 0;
pub const CONTEST_PAYOUT_VOUT: u32 = 1;
/// Inputs of the watchtower ack (bridge `CounterproofAckTx::N_INPUTS`): the contest's payout
/// connector and the counterproof's ack/nack connector.
pub const ACK_N_INPUTS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FalseClaimReport {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub report_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    pub operator_pubkey: XOnlyPublicKey,
    pub new_claim_txid: Txid,
    pub new_contest_txid: Txid,
    pub acked_claim_txid: Txid,
    pub acked_contest_txid: Txid,
    pub counterproof_ack_txid: Txid,
}

impl FalseClaimReport {
    /// The transactions verification needs, in report order.
    pub fn txids(&self) -> [Txid; 5] {
        [
            self.new_claim_txid,
            self.new_contest_txid,
            self.acked_claim_txid,
            self.acked_contest_txid,
            self.counterproof_ack_txid,
        ]
    }

    /// The admin payout connector this report asks to block.
    pub fn blocked_outpoint(&self) -> OutPoint {
        OutPoint::new(self.new_claim_txid, CLAIM_ADMIN_PAYOUT_VOUT)
    }
}

/// A paste item that could not be read as a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MalformedReport {
    /// 0-based position in the paste.
    pub position: usize,
    pub error: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedPaste {
    pub reports: Vec<FalseClaimReport>,
    pub malformed: Vec<MalformedReport>,
}

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
    for v in serde_json::Deserializer::from_str(text).into_iter::<serde_json::Value>() {
        match v.map_err(|e| PasteError::NotJson(e.to_string()))? {
            serde_json::Value::Array(items) => values.extend(items),
            v => values.push(v),
        }
        if values.len() > MAX_REPORTS {
            return Err(PasteError::TooMany);
        }
    }
    if values.is_empty() {
        return Err(PasteError::Empty);
    }
    let mut out = ParsedPaste::default();
    for (position, value) in values.into_iter().enumerate() {
        match parse_report_value(value, network) {
            Ok(r) => out.reports.push(r),
            Err(error) => out.malformed.push(MalformedReport { position, error }),
        }
    }
    Ok(out)
}

fn parse_report_value(
    value: serde_json::Value,
    network: bitcoin::Network,
) -> Result<FalseClaimReport, String> {
    if let Some(v) = value.get("version").and_then(serde_json::Value::as_u64) {
        if v != u64::from(REPORT_VERSION) {
            return Err(format!(
                "unsupported report version {v}; this app reads version {REPORT_VERSION}"
            ));
        }
    }
    let r: FalseClaimReport = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if let Some(t) = &r.report_type {
        if t != REPORT_TYPE {
            return Err(format!("type must be \"{REPORT_TYPE}\", got \"{t}\""));
        }
    }
    if let Some(n) = &r.network {
        let parsed = bitcoin::Network::from_core_arg(n)
            .or_else(|_| n.parse::<bitcoin::Network>())
            .map_err(|_| format!("unknown network \"{n}\""))?;
        if parsed != network {
            return Err(format!("report is for {parsed}, this app is on {network}"));
        }
    }
    if r.new_claim_txid == r.acked_claim_txid {
        return Err("the new claim and the acked claim are the same transaction".into());
    }
    Ok(r)
}

/// A transaction as the admin's node returns it.
#[derive(Debug, Clone)]
pub struct MinedTx {
    pub tx: Transaction,
    /// `None` when the node has the transaction only in its mempool.
    pub block_height: Option<u64>,
    /// As the node reports them: 1 for a transaction in the tip block.
    pub confirmations: u64,
}

/// A report counts only once its ACK is this deep: the ACK is the proof of the false claim, and a
/// shallow one could still be reorganised away.
pub const ACK_MIN_CONFIRMATIONS: u64 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReportVerdict {
    Valid,
    Invalid,
    /// The connector was already spent in a confirmed transaction; ignored.
    Spent,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedReport {
    pub report: FalseClaimReport,
    pub verdict: ReportVerdict,
    /// Why the report is invalid or ignored; empty when valid.
    pub reasons: Vec<String>,
    /// The new claim's admin payout connector.
    #[serde(serialize_with = "ser_outpoint")]
    pub outpoint: OutPoint,
    /// Index into [`Deployment::covenants`] of the new claim's N/N key.
    pub covenant_index: Option<usize>,
    pub new_claim_height: Option<u64>,
    /// Height at which the operator's uncontested payout becomes valid.
    pub payout_height: Option<u64>,
    /// The operator's index in `params.toml`, when the key is a covenant key there.
    pub operator_index: Option<u32>,
}

pub(crate) fn ser_outpoint<S: serde::Serializer>(o: &OutPoint, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(o)
}

/// What the node says about the blocked connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorState {
    Unspent,
    /// Spent by a confirmed transaction.
    SpentConfirmed,
}

/// The x-only key pushed first in a contest leaf, if `leaf` is exactly
/// `<32 bytes> OP_CHECKSIGVERIFY <32 bytes> OP_CHECKSIG`.
fn contest_leaf_n_of_n(leaf: &Script) -> Option<XOnlyPublicKey> {
    let ins: Vec<_> = leaf.instructions().collect::<Result<_, _>>().ok()?;
    match ins.as_slice() {
        [Instruction::PushBytes(nn), Instruction::Op(v), Instruction::PushBytes(wt), Instruction::Op(c)]
            if *v == OP_CHECKSIGVERIFY && *c == OP_CHECKSIG && nn.len() == 32 && wt.len() == 32 =>
        {
            XOnlyPublicKey::from_slice(nn.as_bytes()).ok()
        }
        _ => None,
    }
}

/// Leaf script and control block of a script-path witness (annex aware).
fn script_path_parts(w: &bitcoin::Witness) -> Option<(&Script, ControlBlock)> {
    let mut items: Vec<&[u8]> = w.iter().collect();
    if items.len() >= 2 && items.last().is_some_and(|a| a.first() == Some(&0x50)) {
        items.pop();
    }
    if items.len() < 2 {
        return None;
    }
    let cb = ControlBlock::decode(items[items.len() - 1]).ok()?;
    let script = Script::from_bytes(items[items.len() - 2]);
    Some((script, cb))
}

/// Checks that `contest` spends `claim:0` through a contest leaf committed to that output, and
/// returns the trusted covenant whose N/N key the leaf uses.
fn check_contest_spends_claim<'a>(
    dep: &'a Deployment,
    which: &str,
    claim: &Transaction,
    contest: &Transaction,
) -> Result<&'a Covenant, String> {
    let claim_txid = claim.compute_txid();
    let outpoint = OutPoint::new(claim_txid, CLAIM_CONTEST_VOUT);
    let input = contest
        .input
        .iter()
        .find(|i| i.previous_output == outpoint)
        .ok_or_else(|| {
            format!("The {which} contest does not spend output 0 of the {which} claim.")
        })?;
    let prevout = claim
        .output
        .get(CLAIM_CONTEST_VOUT as usize)
        .ok_or_else(|| format!("The {which} claim has no output 0."))?;
    if !prevout.script_pubkey.is_p2tr() {
        return Err(format!(
            "Output 0 of the {which} claim is not a taproot output."
        ));
    }
    let output_key = XOnlyPublicKey::from_slice(&prevout.script_pubkey.as_bytes()[2..34])
        .map_err(|_| format!("Output 0 of the {which} claim has an invalid taproot key."))?;
    let (leaf, cb) = script_path_parts(&input.witness).ok_or_else(|| {
        format!("The {which} contest does not spend the claim-contest connector through a leaf.")
    })?;
    if cb.leaf_version != LeafVersion::TapScript {
        return Err(format!("The {which} contest uses an unknown leaf version."));
    }
    let secp = Secp256k1::verification_only();
    if !cb.verify_taproot_commitment(&secp, output_key, leaf) {
        return Err(format!(
            "The {which} contest's leaf is not committed to the {which} claim's output 0."
        ));
    }
    let n_of_n = contest_leaf_n_of_n(leaf).ok_or_else(|| {
        format!("The {which} contest did not spend the claim through a contest leaf (N/N + watchtower).")
    })?;
    dep.covenant_for_n_of_n_xonly(&n_of_n).ok_or_else(|| {
        format!(
            "The {which} claim's contest connector uses N/N key {n_of_n}, which is not derived from \
             the trusted params.toml."
        )
    })
}

/// The x-only key pushed first in a counterproof timeout leaf, if `leaf` is exactly
/// `<32 bytes> OP_CHECKSIGVERIFY <delta> OP_CSV`.
fn ack_leaf_n_of_n(leaf: &Script) -> Option<XOnlyPublicKey> {
    let ins: Vec<_> = leaf.instructions().collect::<Result<_, _>>().ok()?;
    match ins.as_slice() {
        [Instruction::PushBytes(nn), Instruction::Op(v), Instruction::PushBytes(_) | Instruction::Op(_), Instruction::Op(c)]
            if *v == OP_CHECKSIGVERIFY && *c == OP_CSV && nn.len() == 32 =>
        {
            XOnlyPublicKey::from_slice(nn.as_bytes()).ok()
        }
        _ => None,
    }
}

fn spends(tx: &Transaction, outpoint: OutPoint) -> bool {
    tx.input.iter().any(|i| i.previous_output == outpoint)
}

/// Checks that `ack` is the watchtower ACK of `contest` (the bridge's `is_ack_of`), without the
/// counterproof: see the module docs. `covenant` is the one whose N/N key the contest used.
fn check_ack_of_contest(
    covenant: &Covenant,
    contest: &Transaction,
    ack: &Transaction,
) -> Result<(), String> {
    let contest_txid = contest.compute_txid();
    if !spends(ack, OutPoint::new(contest_txid, CONTEST_PAYOUT_VOUT)) {
        return Err(
            "The ack does not spend the acked contest's payout connector, so it is not an ACK."
                .into(),
        );
    }
    if spends(ack, OutPoint::new(contest_txid, CONTEST_PROOF_VOUT)) {
        return Err(
            "The ack also spends the acked contest's proof connector, so it is the bridge proof \
             timeout, not an ACK."
                .into(),
        );
    }
    let shape = ack.version == Version(3)
        && ack.lock_time == LockTime::ZERO
        && ack.input.len() == ACK_N_INPUTS
        && ack.output.len() == 1
        && ack.output[0].script_pubkey.is_p2tr();
    if !shape {
        return Err(
            "The ack does not have the shape of a counterproof ack (version 3, locktime 0, two \
             inputs, one taproot output), so it is not an ACK."
                .into(),
        );
    }
    let covenant_signed = ack.input.iter().any(|input| {
        script_path_parts(&input.witness)
            .filter(|(_, cb)| cb.leaf_version == LeafVersion::TapScript)
            .and_then(|(leaf, _)| ack_leaf_n_of_n(leaf))
            .is_some_and(|k| k == covenant.n_of_n_xonly)
    });
    if !covenant_signed {
        return Err(format!(
            "The ack does not reveal the timeout leaf of N/N key {}, so the covenant did not sign \
             it.",
            covenant.n_of_n_xonly
        ));
    }
    Ok(())
}

/// Verifies one report. `txs` holds whatever the node returned for [`FalseClaimReport::txids`]
/// (missing entries mean the node does not know the transaction). `connector` is the node's
/// answer for the blocked outpoint (`gettxout` without the mempool), if known.
pub fn verify_report(
    dep: &Deployment,
    report: &FalseClaimReport,
    txs: &HashMap<Txid, MinedTx>,
    connector: Option<ConnectorState>,
) -> VerifiedReport {
    let mut v = VerifiedReport {
        report: report.clone(),
        verdict: ReportVerdict::Invalid,
        reasons: Vec::new(),
        outpoint: report.blocked_outpoint(),
        covenant_index: None,
        new_claim_height: None,
        payout_height: None,
        operator_index: dep.operator_index_for_key(&report.operator_pubkey),
    };
    if let Err(reason) = check(dep, report, txs, &mut v) {
        v.reasons.push(reason);
        return v;
    }
    match connector {
        Some(ConnectorState::SpentConfirmed) => {
            v.verdict = ReportVerdict::Spent;
            v.reasons.push(
                "The claim's admin payout connector is already spent in a confirmed transaction."
                    .into(),
            );
        }
        Some(ConnectorState::Unspent) | None => v.verdict = ReportVerdict::Valid,
    }
    v
}

fn check(
    dep: &Deployment,
    r: &FalseClaimReport,
    txs: &HashMap<Txid, MinedTx>,
    v: &mut VerifiedReport,
) -> Result<(), String> {
    // 1. Every transaction is mined.
    let names = [
        "new claim",
        "new contest",
        "acked claim",
        "acked contest",
        "ack",
    ];
    let mut got = Vec::with_capacity(names.len());
    for (txid, name) in r.txids().iter().zip(names) {
        let m = txs.get(txid).ok_or_else(|| {
            format!("Your Bitcoin node does not know the {name} transaction {txid} (it needs txindex=1).")
        })?;
        if m.tx.compute_txid() != *txid {
            return Err(format!(
                "The node returned a different transaction for the {name}."
            ));
        }
        if m.block_height.is_none() {
            return Err(format!("The {name} transaction {txid} is not mined yet."));
        }
        got.push(m);
    }
    let [new_claim, new_contest, acked_claim, acked_contest, ack] =
        [got[0], got[1], got[2], got[3], got[4]];
    if ack.confirmations < ACK_MIN_CONFIRMATIONS {
        return Err(format!(
            "The ack transaction {} has {} of the {ACK_MIN_CONFIRMATIONS} confirmations required. \
             Paste the report again later.",
            r.txids()[4],
            ack.confirmations
        ));
    }
    v.new_claim_height = new_claim.block_height;

    // 2. Both contests spend their claim's contest connector through an N/N leaf.
    let new_cov = check_contest_spends_claim(dep, "new", &new_claim.tx, &new_contest.tx)?;
    let acked_cov = check_contest_spends_claim(dep, "acked", &acked_claim.tx, &acked_contest.tx)?;

    // 3. The ack is the watchtower ACK of the acked contest.
    check_ack_of_contest(acked_cov, &acked_contest.tx, &ack.tx)?;

    // 4. Both claims pay the CPFP anchor to the operator key.
    let anchor = KeyedAnchor::new(dep.network, r.operator_pubkey, Amount::ZERO).script_pubkey();
    for (name, claim) in [("new", new_claim), ("acked", acked_claim)] {
        let out = claim.tx.output.get(CLAIM_CPFP_VOUT as usize);
        if out.map(|o| &o.script_pubkey) != Some(&anchor) {
            return Err(format!(
                "The {name} claim's CPFP output is not locked to operator key {}.",
                r.operator_pubkey
            ));
        }
    }

    // The connector to spend is the one of the new claim's N/N key.
    let out = new_claim
        .tx
        .output
        .get(CLAIM_ADMIN_PAYOUT_VOUT as usize)
        .ok_or("The new claim has no output 1.")?;
    if out.script_pubkey != new_cov.connector_script {
        return Err(
            "Output 1 of the new claim is not the admin payout connector of its N/N key and this \
             admin set."
                .into(),
        );
    }
    if out.value != new_cov.connector.value() {
        return Err(format!(
            "The new claim's admin payout connector holds {} instead of {}.",
            out.value,
            new_cov.connector.value()
        ));
    }
    v.covenant_index = Some(new_cov.index);
    v.payout_height = new_claim
        .block_height
        .map(|h| h + u64::from(dep.contest_timelock));
    Ok(())
}

/// Verifies many reports. Reports that block the same connector are merged: the first valid one
/// wins. Results keep first-seen order, one per blocked connector.
pub fn verify_reports(
    dep: &Deployment,
    reports: &[FalseClaimReport],
    txs: &HashMap<Txid, MinedTx>,
    connectors: &HashMap<OutPoint, ConnectorState>,
) -> Vec<VerifiedReport> {
    let mut positions = HashMap::new();
    let mut results: Vec<VerifiedReport> = Vec::new();
    for r in reports {
        let op = r.blocked_outpoint();
        let index = *positions.entry(op).or_insert(results.len());
        let result = verify_report(dep, r, txs, connectors.get(&op).copied());
        match results.get_mut(index) {
            Some(previous) if previous.verdict == ReportVerdict::Invalid => *previous = result,
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

    fn setup() -> (Fixture, Deployment, GameTxs) {
        let fx = Fixture::new(3, 2);
        let dep = Deployment::from_params_toml(&fx.params_toml()).unwrap();
        let g = fx.game(&dep, 0);
        (fx, dep, g)
    }

    #[test]
    fn a_genuine_report_is_valid() {
        let (_fx, dep, g) = setup();
        let v = verify_report(&dep, &g.report, &g.lookup(), Some(ConnectorState::Unspent));
        assert_eq!(v.verdict, ReportVerdict::Valid, "{:?}", v.reasons);
        assert_eq!(v.covenant_index, Some(0));
        assert_eq!(v.operator_index, Some(0));
        assert_eq!(v.payout_height, Some(500 + 144));
    }

    #[test]
    fn a_spent_connector_is_ignored() {
        let (_fx, dep, g) = setup();
        let v = verify_report(
            &dep,
            &g.report,
            &g.lookup(),
            Some(ConnectorState::SpentConfirmed),
        );
        assert_eq!(v.verdict, ReportVerdict::Spent);
    }

    #[test]
    fn an_ack_with_fewer_than_six_confirmations_is_refused() {
        let (_fx, dep, g) = setup();
        let ack = g.report.txids()[4];
        let verdict = |confirmations| {
            let mut l = g.lookup();
            l.get_mut(&ack).unwrap().confirmations = confirmations;
            verify_report(&dep, &g.report, &l, None)
        };
        let v = verdict(3);
        assert_eq!(v.verdict, ReportVerdict::Invalid);
        assert!(
            v.reasons[0].contains("has 3 of the 6 confirmations required"),
            "{:?}",
            v.reasons
        );
        assert_eq!(verdict(6).verdict, ReportVerdict::Valid);
    }

    #[test]
    fn every_transaction_must_be_mined() {
        let (_fx, dep, g) = setup();
        for txid in g.report.txids() {
            let mut l = g.lookup();
            l.get_mut(&txid).unwrap().block_height = None;
            let v = verify_report(&dep, &g.report, &l, None);
            assert_eq!(v.verdict, ReportVerdict::Invalid);
            assert!(v.reasons[0].contains("not mined"), "{:?}", v.reasons);
            let mut l = g.lookup();
            l.remove(&txid);
            let v = verify_report(&dep, &g.report, &l, None);
            assert!(v.reasons[0].contains("does not know"), "{:?}", v.reasons);
        }
    }

    #[test]
    fn an_untrusted_n_of_n_is_rejected() {
        let (fx, dep, _) = setup();
        // The same game built with an N/N key that params.toml does not derive.
        let g = fx.game_with_n_of_n(&dep, 0, fx.foreign_n_of_n());
        let v = verify_report(&dep, &g.report, &g.lookup(), None);
        assert_eq!(v.verdict, ReportVerdict::Invalid);
        assert!(v.reasons[0].contains("not derived from"), "{:?}", v.reasons);
    }

    fn with_wrong_claim_operator(acked: bool) -> (Deployment, GameTxs) {
        let (fx, dep, mut g) = setup();
        let claim_index = if acked { 2 } else { 0 };
        g.txs[claim_index].0.output[CLAIM_CPFP_VOUT as usize].script_pubkey =
            KeyedAnchor::new(dep.network, fx.operator_xonly(1), Amount::ZERO).script_pubkey();
        let claim_txid = g.txs[claim_index].0.compute_txid();
        g.txs[claim_index + 1].0.input[0].previous_output.txid = claim_txid;
        let contest_txid = g.txs[claim_index + 1].0.compute_txid();
        // Keep the chain links intact so only this claim's operator binding is wrong.
        if acked {
            g.report.acked_claim_txid = claim_txid;
            g.report.acked_contest_txid = contest_txid;
            g.tamper_ack(|ack| ack.input[1].previous_output.txid = contest_txid);
        } else {
            g.report.new_claim_txid = claim_txid;
            g.report.new_contest_txid = contest_txid;
        }
        (dep, g)
    }

    #[test]
    fn only_the_new_claim_has_the_wrong_operator() {
        let (dep, g) = with_wrong_claim_operator(false);
        let v = verify_report(&dep, &g.report, &g.lookup(), Some(ConnectorState::Unspent));
        assert_eq!(v.verdict, ReportVerdict::Invalid);
        assert!(v.reasons[0].contains("new claim's CPFP"), "{:?}", v.reasons);
    }

    #[test]
    fn only_the_acked_claim_has_the_wrong_operator() {
        let (dep, g) = with_wrong_claim_operator(true);
        let v = verify_report(&dep, &g.report, &g.lookup(), Some(ConnectorState::Unspent));
        assert_eq!(v.verdict, ReportVerdict::Invalid);
        assert!(
            v.reasons[0].contains("acked claim's CPFP"),
            "{:?}",
            v.reasons
        );
    }

    #[test]
    fn a_nack_is_not_an_ack() {
        let (fx, dep, g) = setup();
        let mut g2 = g.clone();
        g2.replace_ack_with_nack(&fx);
        let v = verify_report(&dep, &g2.report, &g2.lookup(), None);
        assert!(v.reasons[0].contains("not an ACK"), "{:?}", v.reasons);
    }

    #[test]
    fn mismatched_chain_links_are_rejected() {
        let (fx, dep, g) = setup();
        let other = fx.game(&dep, 1);
        let mut l = g.lookup();
        l.extend(other.lookup());
        // An ack of a different game.
        let mut r = g.report.clone();
        r.counterproof_ack_txid = other.report.counterproof_ack_txid;
        let v = verify_report(&dep, &r, &l, None);
        assert!(
            v.reasons[0].contains("does not spend the acked contest's payout connector"),
            "{:?}",
            v.reasons
        );
        // New contest from a different game.
        let mut r = g.report.clone();
        r.new_contest_txid = other.report.new_contest_txid;
        let v = verify_report(&dep, &r, &l, None);
        assert!(
            v.reasons[0].contains("new contest does not spend"),
            "{:?}",
            v.reasons
        );
    }

    #[test]
    fn the_ack_must_be_told_from_the_two_other_spenders_of_the_payout_connector() {
        let (_fx, dep, g) = setup();
        let ct = g.acked_contest_txid();
        let verdict = |g: &GameTxs| verify_report(&dep, &g.report, &g.lookup(), None);

        // The bridge proof timeout also spends the proof connector.
        let mut t = g.clone();
        t.tamper_ack(|a| a.input[0].previous_output = OutPoint::new(ct, CONTEST_PROOF_VOUT));
        assert!(
            verdict(&t).reasons[0].contains("proof timeout"),
            "{:?}",
            verdict(&t).reasons
        );

        // The contested payout has more inputs.
        let mut t = g.clone();
        t.tamper_ack(|a| {
            let extra = a.input[0].clone();
            a.input.push(bitcoin::TxIn {
                previous_output: OutPoint::new(ct, 2),
                ..extra
            });
        });
        assert!(
            verdict(&t).reasons[0].contains("shape"),
            "{:?}",
            verdict(&t).reasons
        );

        // Version and locktime are part of the shape.
        let mut t = g.clone();
        t.tamper_ack(|a| a.version = bitcoin::transaction::Version(2));
        assert!(verdict(&t).reasons[0].contains("shape"));
        let mut t = g.clone();
        t.tamper_ack(|a| a.lock_time = bitcoin::absolute::LockTime::from_height(5).unwrap());
        assert!(verdict(&t).reasons[0].contains("shape"));

        // Without the timeout leaf there is no proof that the covenant signed it.
        let mut t = g.clone();
        t.tamper_ack(|a| a.input[0].witness = key_witness());
        assert!(
            verdict(&t).reasons[0].contains("did not sign"),
            "{:?}",
            verdict(&t).reasons
        );
    }

    #[test]
    fn the_ack_leaf_must_name_the_covenant_of_the_acked_contest() {
        let (fx, dep, g) = setup();
        // An ack whose timeout leaf carries some other N/N key.
        let mut t = g.clone();
        let foreign = fx.foreign_n_of_n();
        let forged = fx.game_with_n_of_n(&dep, 0, foreign).txs[4].0.clone();
        t.tamper_ack(|a| a.input[0].witness = forged.input[0].witness.clone());
        let v = verify_report(&dep, &t.report, &t.lookup(), None);
        assert!(v.reasons[0].contains("did not sign"), "{:?}", v.reasons);
    }

    #[test]
    fn a_report_of_another_version_is_refused_with_a_clear_message() {
        let (_fx, dep, g) = setup();
        let mut other = serde_json::to_value(&g.report).unwrap();
        other["version"] = serde_json::json!(2);
        let p = parse_paste(&other.to_string(), dep.network).unwrap();
        assert!(p.reports.is_empty());
        assert!(
            p.malformed[0]
                .error
                .contains("unsupported report version 2; this app reads version 1"),
            "{:?}",
            p.malformed
        );
        // The field names of an earlier draft are refused too, with or without a version.
        let mut draft = serde_json::json!({
            "type": "strata-false-claim-report", "version": 1, "network": "regtest",
            "operatorPubkey": g.report.operator_pubkey,
            "newClaimTxid": g.report.new_claim_txid,
            "newContestTxid": g.report.new_contest_txid,
            "oldClaimTxid": g.report.acked_claim_txid,
            "oldContestTxid": g.report.acked_contest_txid,
            "oldCounterproofTxid": g.counterproof_txid,
            "oldCounterproofAckTxid": g.report.counterproof_ack_txid,
        });
        let p = parse_paste(&draft.to_string(), dep.network).unwrap();
        assert!(p.reports.is_empty() && p.malformed.len() == 1);
        draft.as_object_mut().unwrap().remove("version");
        let p = parse_paste(&draft.to_string(), dep.network).unwrap();
        assert!(p.reports.is_empty() && p.malformed.len() == 1);
    }

    #[test]
    fn parses_single_array_and_stream_pastes() {
        let (_fx, dep, g) = setup();
        let one = serde_json::to_string(&g.report).unwrap();
        let p = parse_paste(&one, dep.network).unwrap();
        assert_eq!(p.reports.len(), 1);
        let arr = format!("[{one},{one}]");
        assert_eq!(parse_paste(&arr, dep.network).unwrap().reports.len(), 2);
        let stream = format!("{one}\n{one}\n{{\"nope\":1}}");
        let p = parse_paste(&stream, dep.network).unwrap();
        assert_eq!(p.reports.len(), 2);
        assert_eq!(p.malformed.len(), 1);
        assert_eq!(p.malformed[0].position, 2);
        assert!(matches!(
            parse_paste("hello", dep.network),
            Err(PasteError::NotJson(_))
        ));
        assert_eq!(parse_paste("  ", dep.network), Err(PasteError::Empty));
    }

    #[test]
    fn optional_fields_must_match() {
        let (_fx, dep, g) = setup();
        let mut r = g.report.clone();
        r.network = Some("bitcoin".into());
        let p = parse_paste(&serde_json::to_string(&r).unwrap(), dep.network).unwrap();
        assert!(p.malformed[0].error.contains("report is for"));
        let mut r = g.report.clone();
        r.report_type = Some("other".into());
        let p = parse_paste(&serde_json::to_string(&r).unwrap(), dep.network).unwrap();
        assert!(p.malformed[0].error.contains("type must be"));
        // Unknown fields mean a different kind of report.
        let mut j = serde_json::to_value(&g.report).unwrap();
        j["proof"] = serde_json::json!({});
        let p = parse_paste(&j.to_string(), dep.network).unwrap();
        assert!(p.malformed[0].error.contains("unknown field"));
    }

    #[test]
    fn duplicate_reports_merge() {
        let (_fx, dep, g) = setup();
        let mut bad = g.report.clone();
        bad.counterproof_ack_txid = g.report.acked_contest_txid;
        let res = verify_reports(&dep, &[bad, g.report.clone()], &g.lookup(), &HashMap::new());
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].verdict, ReportVerdict::Valid);
    }
}
