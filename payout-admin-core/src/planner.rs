//! Plans Admin Block Payouts transactions from valid reports.
//!
//! - Claims are ordered most urgent first (earliest payout height) and packed into as few
//!   transactions as the TRUC size limit allows, counting the fee input(s) and the one change
//!   output.
//! - Each transaction is funded from distinct Admin Wallet coins: the smallest single coin that
//!   covers the fee, else the largest coins first until it is covered.
//! - When the admin removes inputs, [`plan_groups`] re-plans exactly the groups given, so a
//!   transaction never silently changes shape under them.

use bitcoin::{OutPoint, ScriptBuf};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use crate::deployment::Deployment;
use crate::fees::fee_for;
use crate::payout_tx::{final_vsize, ConnectorRef, P2TR_DUST_SAT, TRUC_MAX_VSIZE};
use strata_bridge_connectors::Connector;

pub const MSG_INSUFFICIENT_FUNDS: &str =
    "Admin Wallet has insufficient funds to pay this transaction fee.";
pub const MSG_SIZE_LIMIT: &str =
    "Your transaction exceeds the size limit, please remove one or more inputs to reduce its size.";

/// A claim to block.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(into = "ClaimInput")]
pub struct PlanClaim {
    pub connector: ConnectorRef,
    /// Lower is more urgent. `u64::MAX` when unknown.
    pub payout_height: u64,
}

/// A claim chosen for blocking (from a valid report).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimInput {
    pub outpoint: String,
    pub covenant_index: usize,
    pub payout_height: Option<u64>,
}

impl TryFrom<&ClaimInput> for PlanClaim {
    type Error = String;
    fn try_from(c: &ClaimInput) -> Result<Self, String> {
        let o = OutPoint::from_str(&c.outpoint).map_err(|e| format!("bad claim outpoint: {e}"))?;
        if o.vout != crate::report::CLAIM_ADMIN_PAYOUT_VOUT {
            return Err(format!("{o} is not a claim's admin payout output"));
        }
        Ok(PlanClaim {
            connector: ConnectorRef {
                claim_txid: o.txid,
                covenant_index: c.covenant_index,
            },
            payout_height: c.payout_height.unwrap_or(u64::MAX),
        })
    }
}
impl From<PlanClaim> for ClaimInput {
    fn from(c: PlanClaim) -> Self {
        Self {
            outpoint: c.connector.outpoint().to_string(),
            covenant_index: c.connector.covenant_index,
            payout_height: (c.payout_height != u64::MAX).then_some(c.payout_height),
        }
    }
}
impl From<PlanError> for String {
    fn from(error: PlanError) -> Self {
        error.message().into()
    }
}

/// An Admin Wallet coin available for fees (confirmed, not used by another pending transaction).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanCoin {
    #[serde(serialize_with = "crate::report::ser_outpoint")]
    pub outpoint: OutPoint,
    pub value_sat: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(into = "String")]
pub enum PlanError {
    InsufficientFunds,
    SizeLimit,
}

impl PlanError {
    pub fn message(self) -> &'static str {
        match self {
            PlanError::InsufficientFunds => MSG_INSUFFICIENT_FUNDS,
            PlanError::SizeLimit => MSG_SIZE_LIMIT,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedTx {
    pub claims: Vec<PlanClaim>,
    pub fee_coins: Vec<PlanCoin>,
    pub vsize: u64,
    pub fee_sat: u64,
    /// 0 when unfunded.
    pub change_sat: u64,
    pub errors: Vec<PlanError>,
}

impl PlannedTx {
    pub fn connectors(&self) -> Vec<ConnectorRef> {
        self.claims.iter().map(|c| c.connector).collect()
    }

    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub txs: Vec<PlannedTx>,
    pub fee_rate_tenths: u64,
}

impl Plan {
    pub fn total_fee_sat(&self) -> u64 {
        self.txs.iter().map(|t| t.fee_sat).sum()
    }

    pub fn is_ok(&self) -> bool {
        self.txs.iter().all(PlannedTx::is_ok)
    }
}

/// Largest number of claim inputs that fits under the size limit with `n_fee` fee inputs.
pub fn max_claims_per_tx(dep: &Deployment, n_fee: usize, change: &ScriptBuf) -> usize {
    let (mut lo, mut hi) = (0usize, 1usize);
    while final_vsize(dep, hi, n_fee, change) <= TRUC_MAX_VSIZE {
        lo = hi;
        hi *= 2;
    }
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if final_vsize(dep, mid, n_fee, change) <= TRUC_MAX_VSIZE {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Chooses fee coins for one transaction out of `available` (which it shrinks).
fn fund(
    dep: &Deployment,
    claims: &[PlanClaim],
    available: &mut Vec<PlanCoin>,
    fee_rate_tenths: u64,
    change: &ScriptBuf,
) -> PlannedTx {
    let n = claims.len();
    let connector_value = n as u64 * dep.covenants[0].connector.value().to_sat();
    let need = |n_fee: usize| {
        let vsize = final_vsize(dep, n, n_fee, change);
        (vsize, fee_for(fee_rate_tenths, vsize))
    };
    // Value the fee coins must bring in so the change stays above dust.
    let shortfall = |fee: u64| (fee + P2TR_DUST_SAT).saturating_sub(connector_value);

    let mut chosen: Vec<PlanCoin> = Vec::new();
    // Smallest single coin that covers it.
    let (_, fee1) = need(1);
    let single = available
        .iter()
        .filter(|c| c.value_sat >= shortfall(fee1))
        .min_by_key(|c| (c.value_sat, c.outpoint))
        .copied();
    if let Some(c) = single {
        chosen.push(c);
    } else {
        let mut sorted = available.clone();
        sorted.sort_by_key(|c| (std::cmp::Reverse(c.value_sat), c.outpoint));
        let mut total = 0;
        for c in sorted {
            chosen.push(c);
            total += c.value_sat;
            if total >= shortfall(need(chosen.len()).1) {
                break;
            }
        }
    }
    let n_fee = chosen.len().max(1);
    let (vsize, fee) = need(n_fee);
    let total: u64 = chosen.iter().map(|c| c.value_sat).sum();
    let mut errors = Vec::new();
    if vsize > TRUC_MAX_VSIZE {
        errors.push(PlanError::SizeLimit);
    }
    let funded = total >= shortfall(fee) && !chosen.is_empty();
    if !funded {
        errors.push(PlanError::InsufficientFunds);
        chosen.clear();
    } else {
        available.retain(|c| !chosen.contains(c));
    }
    PlannedTx {
        claims: claims.to_vec(),
        change_sat: if funded {
            total + connector_value - fee
        } else {
            0
        },
        fee_coins: chosen,
        vsize,
        fee_sat: fee,
        errors,
    }
}

/// Packs all claims into transactions and funds each one.
pub fn plan(
    dep: &Deployment,
    claims: &[PlanClaim],
    coins: &[PlanCoin],
    fee_rate_tenths: u64,
    change: &ScriptBuf,
) -> Plan {
    let mut sorted = claims.to_vec();
    sorted.sort_by_key(|c| (c.payout_height, c.connector.claim_txid));
    sorted.dedup_by_key(|c| c.connector.outpoint());
    // Leave room for two fee inputs so one coin not being enough rarely matters.
    let per_tx = max_claims_per_tx(dep, 2, change).max(1);
    let mut available = coins.to_vec();
    let mut txs = Vec::new();
    let mut rest = sorted.as_slice();
    while !rest.is_empty() {
        let mut take = per_tx.min(rest.len());
        let tx = loop {
            let mut left = available.clone();
            let tx = fund(dep, &rest[..take], &mut left, fee_rate_tenths, change);
            // Funded, but with so many small coins that it no longer fits: carry fewer claims
            // and leave the others to the next transaction. An unfunded one stays as it is; the
            // screen shows what is missing.
            if tx.errors == [PlanError::SizeLimit] && take > 1 {
                take -= 1;
                continue;
            }
            available = left;
            break tx;
        };
        txs.push(tx);
        rest = &rest[take..];
    }
    Plan {
        txs,
        fee_rate_tenths,
    }
}

/// Funds exactly the given groups (in order: earlier groups get first pick of the coins).
pub fn plan_groups(
    dep: &Deployment,
    groups: &[Vec<PlanClaim>],
    coins: &[PlanCoin],
    fee_rate_tenths: u64,
    change: &ScriptBuf,
) -> Plan {
    let mut available = coins.to_vec();
    let txs = groups
        .iter()
        .filter(|g| !g.is_empty())
        .map(|g| fund(dep, g, &mut available, fee_rate_tenths, change))
        .collect();
    Plan {
        txs,
        fee_rate_tenths,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payout_tx::p2tr_placeholder_script;
    use crate::test_support::Fixture;
    use bitcoin::{hashes::Hash, Txid};

    #[test]
    fn claim_wire_preserves_unknown_height_and_rejects_wrong_vout() {
        let unknown = claim(1, u64::MAX);
        let wire = serde_json::to_value(unknown).unwrap();
        assert!(wire["payoutHeight"].is_null());
        let mut input: ClaimInput = serde_json::from_value(wire).unwrap();
        let parsed = PlanClaim::try_from(&input).unwrap();
        assert_eq!(parsed.payout_height, u64::MAX);
        assert_eq!(parsed.connector, unknown.connector);
        input.outpoint = OutPoint::new(unknown.connector.claim_txid, 0).to_string();
        assert_eq!(
            PlanClaim::try_from(&input).unwrap_err(),
            format!("{} is not a claim's admin payout output", input.outpoint)
        );
        assert_eq!(
            serde_json::to_value(PlanError::SizeLimit).unwrap(),
            MSG_SIZE_LIMIT
        );
    }

    fn claim(i: u32, h: u64) -> PlanClaim {
        let mut b = [0u8; 32];
        b[..4].copy_from_slice(&i.to_le_bytes());
        PlanClaim {
            connector: ConnectorRef {
                claim_txid: Txid::from_byte_array(b),
                covenant_index: 0,
            },
            payout_height: h,
        }
    }

    fn coin(i: u8, v: u64) -> PlanCoin {
        PlanCoin {
            outpoint: OutPoint::new(Txid::from_byte_array([i; 32]), 0),
            value_sat: v,
        }
    }

    #[test]
    fn a_batch_that_needs_many_small_coins_is_split_until_every_transaction_fits() {
        let dep = Fixture::new(3, 2).deployment();
        let s = p2tr_placeholder_script();
        // A full transaction at 10 sat/vB costs far more than its claims bring in, and the
        // wallet only has small coins: more than the two fee inputs room was left for.
        let per_tx = max_claims_per_tx(&dep, 2, &s);
        let claims: Vec<PlanClaim> = (0..per_tx as u32).map(|i| claim(i, 900)).collect();
        let coins: Vec<PlanCoin> = (1..=40).map(|i| coin(i, 20_000)).collect();
        let p = plan(&dep, &claims, &coins, 100, &s);
        assert!(
            p.is_ok(),
            "{:?}",
            p.txs.iter().map(|t| &t.errors).collect::<Vec<_>>()
        );
        assert!(p.txs.len() > 1, "split instead of a size error");
        assert!(p.txs[0].fee_coins.len() > 2);
        assert!(p.txs.iter().all(|t| t.vsize <= TRUC_MAX_VSIZE));
        // Every claim is in exactly one transaction, and no coin is used twice.
        assert_eq!(p.txs.iter().map(|t| t.claims.len()).sum::<usize>(), per_tx);
        let used: Vec<_> = p.txs.iter().flat_map(|t| &t.fee_coins).collect();
        let distinct: std::collections::BTreeSet<_> = used.iter().map(|c| c.outpoint).collect();
        assert_eq!(used.len(), distinct.len());

        // Not enough money at all is still reported as that, not split into single claims.
        let p = plan(&dep, &claims, &coins[..2], 100, &s);
        assert_eq!(p.txs.len(), 1);
        assert!(p.txs[0].errors.contains(&PlanError::InsufficientFunds));
    }

    #[test]
    fn one_small_batch_picks_smallest_sufficient_coin() {
        let dep = Fixture::new(3, 2).deployment();
        let s = p2tr_placeholder_script();
        let p = plan(
            &dep,
            &[claim(1, 900), claim(2, 800)],
            &[coin(1, 100_000), coin(2, 5_000), coin(3, 100)],
            20,
            &s,
        );
        assert_eq!(p.txs.len(), 1);
        let t = &p.txs[0];
        assert!(t.is_ok(), "{:?}", t.errors);
        assert_eq!(t.fee_coins, vec![coin(2, 5_000)]);
        // Most urgent first.
        assert_eq!(t.claims[0].payout_height, 800);
        assert_eq!(t.change_sat, 5_000 + 660 - t.fee_sat);
        assert_eq!(t.fee_sat, fee_for(20, t.vsize));
    }

    #[test]
    fn accumulates_largest_first() {
        let dep = Fixture::new(3, 2).deployment();
        let s = p2tr_placeholder_script();
        let p = plan(
            &dep,
            &[claim(1, 1)],
            &[coin(1, 300), coin(2, 600), coin(3, 500)],
            30,
            &s,
        );
        let t = &p.txs[0];
        assert!(t.is_ok(), "{:?}", t.errors);
        assert_eq!(t.fee_coins.len(), 2);
        assert_eq!(t.fee_coins[0].value_sat, 600);
    }

    #[test]
    fn insufficient_funds() {
        let dep = Fixture::new(3, 2).deployment();
        let s = p2tr_placeholder_script();
        let p = plan(&dep, &[claim(1, 1)], &[coin(1, 10)], 1000, &s);
        assert_eq!(p.txs[0].errors, vec![PlanError::InsufficientFunds]);
        assert!(p.txs[0].fee_coins.is_empty());
        let p = plan(&dep, &[claim(1, 1)], &[], 1, &s);
        assert_eq!(p.txs[0].errors, vec![PlanError::InsufficientFunds]);
    }

    #[test]
    fn splits_at_the_size_limit_and_uses_distinct_coins() {
        let dep = Fixture::new(3, 2).deployment();
        let s = p2tr_placeholder_script();
        let per = max_claims_per_tx(&dep, 2, &s);
        assert!(final_vsize(&dep, per, 2, &s) <= TRUC_MAX_VSIZE);
        assert!(final_vsize(&dep, per + 1, 2, &s) > TRUC_MAX_VSIZE);
        let claims: Vec<_> = (0..(per as u32 * 2 + 3))
            .map(|i| claim(i, i as u64))
            .collect();
        let p = plan(
            &dep,
            &claims,
            &[coin(1, 1_000_000), coin(2, 1_000_000), coin(3, 1_000_000)],
            10,
            &s,
        );
        assert_eq!(p.txs.len(), 3);
        assert!(p.is_ok());
        let mut used: Vec<_> = p.txs.iter().flat_map(|t| t.fee_coins.clone()).collect();
        used.dedup();
        assert_eq!(used.len(), 3);
        assert_eq!(p.txs[2].claims.len(), 3);
        // Explicit oversize group is flagged, not split.
        let p = plan_groups(
            &dep,
            std::slice::from_ref(&claims),
            &[coin(1, 10_000_000)],
            10,
            &s,
        );
        assert!(p.txs[0].errors.contains(&PlanError::SizeLimit));
        assert_eq!(PlanError::SizeLimit.message(), MSG_SIZE_LIMIT);
    }
}
