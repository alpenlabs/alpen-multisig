//! Bundles Admin Block Payouts transactions from valid reports.
//!
//! - Claims are grouped by N/N key, ordered within each group by claim txid and packed into as few
//!   transactions as the TRUC size limit allows, counting the fee input(s) and the one change
//!   output.
//! - Each transaction is funded from distinct Admin Wallet coins: the smallest single coin that
//!   covers the fee, else the largest coins first until it is covered.

use bitcoin::{Amount, OutPoint, ScriptBuf};
use serde::Serialize;

use crate::block_payouts_tx::{final_vsize, ConnectorRef, P2TR_DUST_SAT, TRUC_MAX_VSIZE};
use crate::deployment::Deployment;
use crate::fees::FeeRate;
use strata_bridge_connectors::Connector;
use strata_bridge_tx_graph::transactions::prelude::ClaimTx;

pub const MSG_INSUFFICIENT_FUNDS: &str =
    "Admin Wallet has insufficient funds to pay this transaction fee.";
pub const MSG_SIZE_LIMIT: &str =
    "Your transaction exceeds the size limit, please remove one or more inputs to reduce its size.";

/// A claim chosen for blocking (from a valid report).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct BundledClaim {
    #[serde(skip)]
    n_of_n: bitcoin::secp256k1::PublicKey,
    outpoint: OutPoint,
}

impl BundledClaim {
    /// Identifies the claim output to spend through the admin multisig leaf.
    pub fn outpoint(&self) -> OutPoint {
        self.outpoint
    }

    /// The N/N key whose admin payout connector this claim spends.
    pub fn n_of_n(&self) -> bitcoin::secp256k1::PublicKey {
        self.n_of_n
    }

    /// Selects the admin payout output of a claim.
    pub fn new(claim_txid: bitcoin::Txid, n_of_n: bitcoin::secp256k1::PublicKey) -> Self {
        Self {
            n_of_n,
            outpoint: OutPoint::new(claim_txid, ClaimTx::ADMIN_PAYOUT_VOUT),
        }
    }
}

impl From<BundleError> for String {
    fn from(error: BundleError) -> Self {
        error.message().into()
    }
}

/// An Admin Wallet coin available for fees (confirmed, not used by another pending transaction).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableCoin {
    outpoint: OutPoint,
    #[serde(rename = "valueSat", with = "bitcoin::amount::serde::as_sat")]
    value: Amount,
}

impl AvailableCoin {
    /// Identifies a wallet output available to fund a transaction.
    pub fn outpoint(&self) -> OutPoint {
        self.outpoint
    }

    /// Identifies a wallet output available for fees.
    pub fn new(outpoint: OutPoint, value: Amount) -> Self {
        Self { outpoint, value }
    }
}

/// A funding or size constraint that prevents a proposed transaction from being used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(into = "String")]
pub enum BundleError {
    InsufficientFunds,
    SizeLimit,
}

impl BundleError {
    /// Explains the constraint to the admin reviewing the bundle.
    pub fn message(self) -> &'static str {
        match self {
            BundleError::InsufficientFunds => MSG_INSUFFICIENT_FUNDS,
            BundleError::SizeLimit => MSG_SIZE_LIMIT,
        }
    }
}

/// Claims grouped for one transaction, with the selected fee coins, amounts, and any funding or size errors.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BundledTx {
    claims: Vec<BundledClaim>,
    fee_coins: Vec<AvailableCoin>,
    vsize: u64,
    #[serde(rename = "feeSat", with = "bitcoin::amount::serde::as_sat")]
    fee: Amount,
    /// 0 when unfunded.
    #[serde(rename = "changeSat", with = "bitcoin::amount::serde::as_sat")]
    change: Amount,
    errors: Vec<BundleError>,
}

impl BundledTx {
    /// Amount returned to the Admin Wallet, or zero when funding failed.
    pub fn change(&self) -> Amount {
        self.change
    }

    /// The claims this transaction groups into one block payouts.
    pub fn claims(&self) -> &[BundledClaim] {
        &self.claims
    }

    /// Funding and size constraints this transaction failed.
    pub fn errors(&self) -> &[BundleError] {
        &self.errors
    }

    /// Fee budget for the estimated fully signed size.
    pub fn fee(&self) -> Amount {
        self.fee
    }

    /// Admin Wallet outputs allocated to fund this transaction.
    pub fn fee_coins(&self) -> &[AvailableCoin] {
        &self.fee_coins
    }

    /// Returns the fully signed transaction size in virtual bytes.
    pub fn vsize(&self) -> u64 {
        self.vsize
    }

    /// Selects the bridge outputs the PSBT must spend to block this group of claims.
    pub fn connectors(&self) -> Vec<ConnectorRef> {
        self.claims
            .iter()
            .map(|claim| ConnectorRef::new(claim.outpoint.txid))
            .collect()
    }

    /// Whether the bundler found a way to fund this group within its size limit.
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// The proposed transactions for a set of claims, including any groups that could not be funded.
#[derive(Debug, Clone)]
pub struct Bundle {
    txs: Vec<BundledTx>,
}

impl Bundle {
    /// The transaction groups in the order produced by the bundler.
    pub fn txs(&self) -> &[BundledTx] {
        &self.txs
    }

    /// Sums fees across all proposed transactions, returning `None` if the sum overflows.
    pub fn total_fee(&self) -> Option<Amount> {
        self.txs
            .iter()
            .try_fold(Amount::ZERO, |total, transaction| {
                total.checked_add(transaction.fee)
            })
    }

    /// Whether every proposed transaction passed the bundler's funding and size checks.
    pub fn is_ok(&self) -> bool {
        self.txs.iter().all(BundledTx::is_ok)
    }
}

/// Largest number of claim inputs that fits under the size limit with `fee_input_count` fee inputs.
fn max_claims_per_tx(deployment: &Deployment, fee_input_count: usize, change: &ScriptBuf) -> usize {
    let (mut lower, mut upper) = (0usize, 1usize);
    while final_vsize(deployment, upper, fee_input_count, change) <= TRUC_MAX_VSIZE {
        lower = upper;
        upper *= 2;
    }
    while 1 < upper - lower {
        let midpoint = (lower + upper) / 2;

        if final_vsize(deployment, midpoint, fee_input_count, change) <= TRUC_MAX_VSIZE {
            lower = midpoint;
        } else {
            upper = midpoint;
        }
    }

    lower
}

/// Chooses fee coins for one transaction out of `available` (which it shrinks).
fn fund(
    deployment: &Deployment,
    claims: &[BundledClaim],
    available: &mut Vec<AvailableCoin>,
    fee_rate: FeeRate,
    change: &ScriptBuf,
) -> Result<BundledTx, String> {
    let claim_count = claims.len();
    let connector_value = deployment
        .covenant()
        .connector()
        .value()
        .checked_mul(claim_count as u64)
        .ok_or("claim value overflow")?;
    let vsize_and_fee = |fee_input_count: usize| -> Result<_, String> {
        let vsize = final_vsize(deployment, claim_count, fee_input_count, change);

        Ok((
            vsize,
            fee_rate.fee_for(u32::try_from(vsize).map_err(|_| MSG_SIZE_LIMIT)?),
        ))
    };
    // Value the fee coins must bring in so the change stays above dust.
    let required_fee_coin_value = |fee: Amount| {
        (fee + Amount::from_sat(P2TR_DUST_SAT))
            .checked_sub(connector_value)
            .unwrap_or(Amount::ZERO)
    };

    let mut chosen: Vec<AvailableCoin> = Vec::new();
    // Smallest single coin that covers it.
    let (_, single_input_fee) = vsize_and_fee(1)?;
    let single = available
        .iter()
        .filter(|coin| required_fee_coin_value(single_input_fee) <= coin.value)
        .min_by_key(|coin| (coin.value, coin.outpoint))
        .copied();
    let mut total = Amount::ZERO;

    if let Some(coin) = single {
        total = coin.value;
        chosen.push(coin);
    } else {
        let mut sorted = available.clone();
        sorted.sort_by_key(|coin| (std::cmp::Reverse(coin.value), coin.outpoint));
        for coin in sorted {
            chosen.push(coin);
            total = total
                .checked_add(coin.value)
                .ok_or("fee coin value overflow")?;

            if required_fee_coin_value(vsize_and_fee(chosen.len())?.1) <= total {
                break;
            }
        }
    }

    let fee_input_count = chosen.len().max(1);
    let (vsize, fee) = vsize_and_fee(fee_input_count)?;
    let funded = required_fee_coin_value(fee) <= total && !chosen.is_empty();
    let (vsize, fee) = if funded {
        (vsize, fee)
    } else {
        vsize_and_fee(1)?
    };
    let mut errors = Vec::new();

    if TRUC_MAX_VSIZE < vsize {
        errors.push(BundleError::SizeLimit);
    }

    if !funded {
        errors.push(BundleError::InsufficientFunds);
        chosen.clear();
    } else {
        available.retain(|coin| !chosen.contains(coin));
    }

    Ok(BundledTx {
        claims: claims.to_vec(),
        change: if funded {
            total
                .checked_add(connector_value)
                .ok_or("input value overflow")?
                - fee
        } else {
            Amount::ZERO
        },
        fee_coins: chosen,
        vsize,
        fee,
        errors,
    })
}

/// Groups distinct claims in txid order and selects fee coins at a rate in tenths of a sat/vB.
pub fn bundle(
    deployment: &Deployment,
    claims: &[BundledClaim],
    coins: &[AvailableCoin],
    fee_rate: FeeRate,
    change: &ScriptBuf,
) -> Result<Bundle, String> {
    let mut sorted = claims.to_vec();
    sorted.sort_by_key(|claim| claim.outpoint.txid);
    sorted.dedup_by_key(|claim| claim.outpoint.txid);
    sorted.sort_by_key(|claim| claim.n_of_n);
    // Start with one fee input; shrink below if funding needs more.
    let claims_per_transaction = max_claims_per_tx(deployment, 1, change).max(1);
    let mut available = coins.to_vec();
    let mut transactions = Vec::new();
    let mut rest = sorted.as_slice();
    while !rest.is_empty() {
        let mut take = rest
            .iter()
            .take(claims_per_transaction)
            .take_while(|claim| claim.n_of_n == rest[0].n_of_n)
            .count();
        let transaction = loop {
            let mut left = available.clone();
            let transaction = fund(deployment, &rest[..take], &mut left, fee_rate, change)?;

            // Funded, but with so many small coins that it no longer fits: carry fewer claims
            // and leave the others to the next transaction. An unfunded one stays as it is; the
            // screen shows what is missing.
            if transaction.errors == [BundleError::SizeLimit] && 1 < take {
                take -= 1;
                continue;
            }

            available = left;
            break transaction;
        };
        transactions.push(transaction);
        rest = &rest[take..];
    }

    Ok(Bundle { txs: transactions })
}

/// Allocate fee coins across evidence groups and bind each transaction to its exact claim proofs.
pub fn bundle_with_reports(
    deployment: &Deployment,
    claims: &[(BundledClaim, crate::report::Report)],
    coins: &[AvailableCoin],
    fee_rate: FeeRate,
    change: &ScriptBuf,
) -> Result<Vec<(BundledTx, crate::report::Report)>, String> {
    let mut groups = std::collections::BTreeMap::new();
    for (claim, report) in claims {
        groups
            .entry(report.counterproof_ack_txid())
            .or_insert_with(Vec::new)
            .push((claim, report));
    }

    let mut available = coins.to_vec();
    let mut transactions = Vec::new();
    for group in groups.values() {
        let selected: Vec<_> = group.iter().map(|(claim, _)| **claim).collect();
        let proofs: std::collections::HashMap<_, _> = group
            .iter()
            .map(|(claim, report)| match report.contest_txids() {
                [contest] => Ok((claim.outpoint(), *contest)),
                _ => Err("Each claim needs a report cut down to its own contest"),
            })
            .collect::<Result<_, _>>()?;
        for transaction in bundle(deployment, &selected, &available, fee_rate, change)?.txs {
            available.retain(|coin| !transaction.fee_coins.contains(coin));
            let claims = transaction
                .claims
                .iter()
                .map(|claim| {
                    proofs
                        .get(&claim.outpoint())
                        .copied()
                        .ok_or("Missing claim evidence")
                })
                .collect::<Result<_, _>>()?;
            transactions.push((transaction, group[0].1.with_contests(claims)));
        }
    }

    Ok(transactions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, P2TR_PLACEHOLDER_SCRIPT};
    use bitcoin::{hashes::Hash, Txid};

    #[test]
    fn transactions_carry_only_their_own_claims_and_evidence() {
        let fixture = Fixture::new(3, 2);
        let deployment = fixture.deployment();
        let key = deployment.covenant().n_of_n();
        let first = fixture.game(&deployment, 0).report();
        let second = fixture.game(&deployment, 1).report();
        let third = fixture.game(&deployment, 2).report();
        let shared = first.with_contests(third.contest_txids().to_vec());
        let claims = [first.clone(), second.clone(), shared]
            .into_iter()
            .enumerate()
            .map(|(index, report)| {
                (
                    BundledClaim::new(fixture.game(&deployment, index as u32).claim_txid(), key),
                    report,
                )
            })
            .collect::<Vec<_>>();
        let transactions = bundle_with_reports(
            &deployment,
            &claims,
            &[coin(1, 100_000), coin(2, 100_000)],
            FeeRate::new(10).unwrap(),
            &P2TR_PLACEHOLDER_SCRIPT,
        )
        .unwrap();
        assert_eq!(transactions.len(), 2);
        let mut fees = std::collections::HashSet::new();
        for (transaction, report) in transactions {
            assert!(transaction.is_ok());
            assert!(fees.insert(transaction.fee_coins()[0].outpoint()));
            let inputs: Vec<_> = transaction
                .claims()
                .iter()
                .map(BundledClaim::outpoint)
                .collect();
            assert_eq!(
                report
                    .for_inputs(&inputs, false, |contest| claims
                        .iter()
                        .find(|(_, proof)| proof.contest_txids() == [contest])
                        .map(|(claim, _)| claim.outpoint())
                        .ok_or_else(|| String::from("unknown contest")))
                    .unwrap(),
                report
            );

            if report.counterproof_ack_txid() == first.counterproof_ack_txid() {
                assert_eq!(inputs.len(), 2);
                assert!(report.contest_txids().contains(&first.contest_txids()[0]));
                assert!(report.contest_txids().contains(&third.contest_txids()[0]));
            } else {
                assert_eq!(report, second);
            }
        }

        // A report that still names several contests is refused, not dropped.
        let mut uncut = claims.clone();
        uncut[0].1 = first.with_contests(vec![first.contest_txids()[0], third.contest_txids()[0]]);
        assert!(bundle_with_reports(
            &deployment,
            &uncut,
            &[coin(1, 100_000), coin(2, 100_000)],
            FeeRate::new(10).unwrap(),
            &P2TR_PLACEHOLDER_SCRIPT,
        )
        .is_err());
    }

    #[test]
    fn claim_wire_contains_the_admin_outpoint() {
        let original = claim(1);
        let wire = serde_json::to_value(original).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({"outpoint": original.outpoint().to_string()})
        );
        assert_eq!(
            serde_json::to_value(BundleError::SizeLimit).unwrap(),
            MSG_SIZE_LIMIT
        );
    }

    fn claim(i: u32) -> BundledClaim {
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&i.to_le_bytes());

        static KEY: std::sync::LazyLock<bitcoin::secp256k1::PublicKey> =
            std::sync::LazyLock::new(|| Fixture::new(3, 2).deployment().covenant().n_of_n());

        BundledClaim::new(Txid::from_byte_array(bytes), *KEY)
    }

    fn coin(i: u8, value_sat: u64) -> AvailableCoin {
        AvailableCoin::new(
            OutPoint::new(Txid::from_byte_array([i; 32]), 0),
            Amount::from_sat(value_sat),
        )
    }

    #[test]
    fn groups_claims_by_nn_key() {
        let deployment = Fixture::new(3, 2).after_operator_removal();
        let old = deployment.covenants()[0].n_of_n();
        let current = deployment.covenant().n_of_n();
        let claims = [old, current, old]
            .into_iter()
            .enumerate()
            .map(|(index, key)| BundledClaim::new(Txid::from_byte_array([index as u8; 32]), key))
            .collect::<Vec<_>>();
        let bundle = bundle(
            &deployment,
            &claims,
            &[coin(4, 10000), coin(5, 10000)],
            FeeRate::new(10).unwrap(),
            &P2TR_PLACEHOLDER_SCRIPT,
        )
        .unwrap();
        assert_eq!(bundle.txs().len(), 2);
        for transaction in bundle.txs() {
            assert!(transaction
                .claims()
                .iter()
                .all(|claim| claim.n_of_n == transaction.claims()[0].n_of_n));
            assert!(transaction.is_ok());
        }
    }

    #[test]
    fn a_batch_that_needs_many_small_coins_is_split_until_every_transaction_fits() {
        let deployment = Fixture::new(3, 2).deployment();
        let script = &P2TR_PLACEHOLDER_SCRIPT;
        // A full transaction at 10 sat/vB costs far more than its claims bring in, and the
        // wallet only has small coins: more than the single fee input initially budgeted.
        let claims_per_transaction = max_claims_per_tx(&deployment, 1, script);
        let claims: Vec<BundledClaim> = (0..claims_per_transaction as u32).map(claim).collect();
        let coins: Vec<AvailableCoin> = (1..=40).map(|i| coin(i, 20_000)).collect();
        let generated_bundle = bundle(
            &deployment,
            &claims,
            &coins,
            FeeRate::new(100).unwrap(),
            script,
        )
        .unwrap();
        assert!(
            generated_bundle.is_ok(),
            "{:?}",
            generated_bundle
                .txs()
                .iter()
                .map(|transaction| transaction.errors())
                .collect::<Vec<_>>()
        );
        assert!(
            1 < generated_bundle.txs().len(),
            "split instead of a size error"
        );
        assert!(2 < generated_bundle.txs()[0].fee_coins().len());
        assert!(generated_bundle
            .txs()
            .iter()
            .all(|transaction| transaction.vsize() <= TRUC_MAX_VSIZE));
        // Every claim is in exactly one transaction, and no coin is used twice.
        assert_eq!(
            generated_bundle
                .txs()
                .iter()
                .map(|transaction| transaction.claims().len())
                .sum::<usize>(),
            claims_per_transaction
        );
        let used: Vec<_> = generated_bundle
            .txs()
            .iter()
            .flat_map(|transaction| transaction.fee_coins())
            .collect();
        let distinct: std::collections::BTreeSet<_> =
            used.iter().map(|coin| coin.outpoint()).collect();
        assert_eq!(used.len(), distinct.len());

        // Not enough money at all is still reported as that, not split into single claims.
        let generated_bundle = bundle(
            &deployment,
            &claims,
            &coins[..2],
            FeeRate::new(100).unwrap(),
            script,
        )
        .unwrap();
        assert_eq!(generated_bundle.txs().len(), 1);
        assert!(generated_bundle.txs()[0]
            .errors()
            .contains(&BundleError::InsufficientFunds));
    }

    #[test]
    fn one_small_batch_picks_smallest_sufficient_coin() {
        let deployment = Fixture::new(3, 2).deployment();
        let script = &P2TR_PLACEHOLDER_SCRIPT;
        let generated_bundle = bundle(
            &deployment,
            &[claim(2), claim(1), claim(2)],
            &[coin(1, 100_000), coin(2, 5_000), coin(3, 100)],
            FeeRate::new(20).unwrap(),
            script,
        )
        .unwrap();
        assert_eq!(generated_bundle.txs().len(), 1);
        let transaction = &generated_bundle.txs()[0];
        assert!(transaction.is_ok(), "{:?}", transaction.errors());
        assert_eq!(transaction.fee_coins(), vec![coin(2, 5_000)]);
        assert_eq!(
            transaction.connectors(),
            [claim(1), claim(2)].map(|claim| ConnectorRef::new(claim.outpoint().txid))
        );
        assert_eq!(
            transaction.change().to_sat(),
            5_000 + 660 - transaction.fee().to_sat()
        );
        assert_eq!(
            transaction.fee(),
            FeeRate::new(20)
                .unwrap()
                .fee_for(transaction.vsize().try_into().unwrap())
        );
    }

    #[test]
    fn accumulates_largest_first() {
        let deployment = Fixture::new(3, 2).deployment();
        let script = &P2TR_PLACEHOLDER_SCRIPT;
        let generated_bundle = bundle(
            &deployment,
            &[claim(1)],
            &[coin(1, 300), coin(2, 600), coin(3, 500)],
            FeeRate::new(30).unwrap(),
            script,
        )
        .unwrap();
        let transaction = &generated_bundle.txs()[0];
        assert!(transaction.is_ok(), "{:?}", transaction.errors());
        assert_eq!(transaction.fee_coins().len(), 2);
        assert_eq!(transaction.fee_coins()[0].value.to_sat(), 600);
    }

    #[test]
    fn insufficient_funds() {
        let deployment = Fixture::new(3, 2).deployment();
        let script = &P2TR_PLACEHOLDER_SCRIPT;
        let generated_bundle = bundle(
            &deployment,
            &[claim(1)],
            &[coin(1, 10), coin(2, 10)],
            FeeRate::new(1000).unwrap(),
            script,
        )
        .unwrap();
        assert_eq!(
            generated_bundle.txs()[0].errors(),
            vec![BundleError::InsufficientFunds]
        );
        assert!(generated_bundle.txs()[0].fee_coins().is_empty());
        assert_eq!(
            generated_bundle.txs()[0].vsize(),
            final_vsize(&deployment, 1, 1, script)
        );
        let generated_bundle = bundle(
            &deployment,
            &[claim(1)],
            &[],
            FeeRate::new(1).unwrap(),
            script,
        )
        .unwrap();
        assert_eq!(
            generated_bundle.txs()[0].errors(),
            vec![BundleError::InsufficientFunds]
        );
    }

    #[test]
    fn splits_at_the_size_limit_and_uses_distinct_coins() {
        let deployment = Fixture::new(3, 2).deployment();
        let script = &P2TR_PLACEHOLDER_SCRIPT;
        let claims_per_transaction = max_claims_per_tx(&deployment, 1, script);
        assert!(final_vsize(&deployment, claims_per_transaction, 1, script) <= TRUC_MAX_VSIZE);
        assert!(final_vsize(&deployment, claims_per_transaction + 1, 1, script) > TRUC_MAX_VSIZE);
        let claims: Vec<_> = (0..(claims_per_transaction as u32 * 2 + 3))
            .map(claim)
            .collect();
        let generated_bundle = bundle(
            &deployment,
            &claims,
            &[coin(1, 1_000_000), coin(2, 1_000_000), coin(3, 1_000_000)],
            FeeRate::new(10).unwrap(),
            script,
        )
        .unwrap();
        assert_eq!(generated_bundle.txs().len(), 3);
        assert!(generated_bundle.is_ok());
        let mut used: Vec<_> = generated_bundle
            .txs()
            .iter()
            .flat_map(|transaction| transaction.fee_coins())
            .collect();
        used.dedup();
        assert_eq!(used.len(), 3);
        assert_eq!(generated_bundle.txs()[2].claims().len(), 3);
    }
}
