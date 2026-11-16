use super::{
    account_path, admin_wallet_coins, change_or_receive_script, check_app_unchanged,
    check_claims_on_node, fees, ledger_review, payout_tx, planner, sign_and_submit, wallet_tx,
    AdminSigner, Amount, ChangeOutput, ChildNumber, CoordinatorPast, CoordinatorPending,
    Deployment, DerivationPath, Deserialize, FeeInput, FromStr, HashMap, HashSet, LedgerReviewDto,
    OutPoint, PayoutContext, PayoutNode, PlanClaim, PlanCoin, PolicyStore, Psbt, ScriptBuf,
    Secp256k1, Serialize, SubmitResultDto, TxOut, WalletCoin, XOnlyPublicKey, Xpub, COIN_CACHE_TTL,
    SCAN_RANGE,
};

pub use payout_admin_core::planner::{ClaimInput, PlannedTx as PlannedTxDto};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDto {
    pub txs: Vec<PlannedTxDto>,
    pub fee_rate_label: String,
    pub total_fee_sat: u64,
    pub ok: bool,
    pub available_sat: u64,
}

/// Outpoints spent by pending transactions: claim connectors and fee coins.
pub fn pending_spends(
    dep: &Deployment,
    pending: &[CoordinatorPending],
) -> (HashSet<OutPoint>, HashSet<OutPoint>, HashSet<ScriptBuf>) {
    let mut connectors = HashSet::new();
    let mut fee_coins = HashSet::new();
    let mut change_scripts = HashSet::new();
    for p in pending {
        let Ok(psbt) = Psbt::from_str(&p.psbt) else {
            continue;
        };
        if let Ok(a) = payout_tx::analyze(dep, &psbt) {
            connectors.extend(a.outpoints());
            fee_coins.extend(a.fee_inputs.iter().map(|f| f.outpoint));
        }
        change_scripts.extend(
            psbt.unsigned_tx
                .output
                .iter()
                .map(|o| o.script_pubkey.clone()),
        );
    }
    (connectors, fee_coins, change_scripts)
}

/// Fee coins usable now: confirmed (a TRUC transaction cannot spend an unconfirmed non-TRUC
/// parent) and not already spent by a pending transaction.
pub(super) fn usable_coins(coins: &[WalletCoin], reserved: &HashSet<OutPoint>) -> Vec<WalletCoin> {
    coins
        .iter()
        .filter(|c| c.confirmations >= 1 && !reserved.contains(&c.outpoint))
        .cloned()
        .collect()
}

fn typed_plan(
    dep: &Deployment,
    claims: &[ClaimInput],
    groups: Option<&[Vec<String>]>,
    fee_rate: &str,
    coins: &[WalletCoin],
) -> Result<planner::Plan, String> {
    let rate = fees::parse_fee_rate(fee_rate).map_err(|e| e.to_string())?;
    let by_outpoint: HashMap<String, PlanClaim> = claims
        .iter()
        .map(|c| {
            Ok((
                OutPoint::from_str(&c.outpoint)
                    .map_err(|e| e.to_string())?
                    .to_string(),
                PlanClaim::try_from(c)?,
            ))
        })
        .collect::<Result<_, String>>()?;
    let plan_coins: Vec<PlanCoin> = coins
        .iter()
        .map(|c| PlanCoin {
            outpoint: c.outpoint,
            value_sat: c.value_sat,
        })
        .collect();
    let placeholder = payout_tx::p2tr_placeholder_script();
    let p = match groups {
        Some(gs) => {
            let groups: Vec<Vec<PlanClaim>> = gs
                .iter()
                .map(|g| {
                    g.iter()
                        .map(|o| {
                            let key = OutPoint::from_str(o)
                                .map_err(|e| e.to_string())?
                                .to_string();
                            by_outpoint
                                .get(&key)
                                .copied()
                                .ok_or_else(|| format!("{o} is not one of the claims"))
                        })
                        .collect::<Result<Vec<_>, String>>()
                })
                .collect::<Result<_, _>>()?;
            planner::plan_groups(dep, &groups, &plan_coins, rate, &placeholder)
        }
        None => {
            let all: Vec<PlanClaim> = claims
                .iter()
                .map(PlanClaim::try_from)
                .collect::<Result<_, _>>()?;
            planner::plan(dep, &all, &plan_coins, rate, &placeholder)
        }
    };
    Ok(p)
}

fn plan_dto(p: &planner::Plan, coins: &[WalletCoin]) -> PlanDto {
    PlanDto {
        total_fee_sat: p.total_fee_sat(),
        ok: p.is_ok(),
        fee_rate_label: fees::format_fee_rate(p.fee_rate_tenths),
        available_sat: coins.iter().map(|c| c.value_sat).sum(),
        txs: p.txs.clone(),
    }
}

pub(super) fn child_key(xpub: &Xpub, branch: u32, index: u32) -> Result<XOnlyPublicKey, String> {
    let secp = Secp256k1::verification_only();
    let k = xpub
        .derive_pub(
            &secp,
            &[
                ChildNumber::from_normal_idx(branch).map_err(|e| e.to_string())?,
                ChildNumber::from_normal_idx(index).map_err(|e| e.to_string())?,
            ],
        )
        .map_err(|e| e.to_string())?;
    Ok(k.public_key.x_only_public_key().0)
}

pub(super) fn full_path(
    dep: &Deployment,
    branch: u32,
    index: u32,
) -> Result<DerivationPath, String> {
    Ok(account_path(dep.network).extend([
        ChildNumber::from_normal_idx(branch).map_err(|e| e.to_string())?,
        ChildNumber::from_normal_idx(index).map_err(|e| e.to_string())?,
    ]))
}

pub(super) fn fee_input(
    dep: &Deployment,
    me: &AdminSigner,
    coin: &WalletCoin,
) -> Result<FeeInput, String> {
    let internal = child_key(&me.account_xpub, coin.branch, coin.index)?;
    let script = ScriptBuf::new_p2tr(&Secp256k1::verification_only(), internal, None);
    if script != coin.script_pubkey {
        return Err(format!(
            "Admin Wallet coin {} is not at the key the wallet says ({}/{})",
            coin.outpoint, coin.branch, coin.index
        ));
    }
    Ok(FeeInput {
        outpoint: coin.outpoint,
        txout: TxOut {
            value: Amount::from_sat(coin.value_sat),
            script_pubkey: coin.script_pubkey.clone(),
        },
        internal_key: internal,
        fingerprint: me.fingerprint,
        path: full_path(dep, coin.branch, coin.index)?,
    })
}

pub(super) fn change_output(
    dep: &Deployment,
    me: &AdminSigner,
    index: u32,
    value_sat: u64,
) -> Result<ChangeOutput, String> {
    Ok(ChangeOutput {
        value: Amount::from_sat(value_sat),
        internal_key: child_key(&me.account_xpub, 1, index)?,
        fingerprint: me.fingerprint,
        path: full_path(dep, 1, index)?,
    })
}

pub(super) fn change_script(me: &AdminSigner, index: u32) -> Result<ScriptBuf, String> {
    change_or_receive_script(me, 1, index)
}

/// The change index to use: the first change address the wallet has not used and no pending
/// transaction pays to.
pub(super) fn pick_change_index(
    me: &AdminSigner,
    is_used: impl Fn(u32) -> bool,
    taken: &HashSet<ScriptBuf>,
) -> Result<u32, String> {
    // Never beyond what the wallet scan covers: a coin at a higher index would not be found again.
    if let Some(i) =
        wallet_tx::first_unused_change(SCAN_RANGE, |i| change_script(me, i), is_used, taken)?
    {
        return Ok(i);
    }
    Err(format!(
        "The first {SCAN_RANGE} change addresses of your Admin Wallet are all used; this app does \
         not look further."
    ))
}

/// Output scripts the Admin Wallet has already used, as far as Bitcoin Core and the coordinator
/// show: current coins, and the outputs of pending and past payout transactions.
pub(super) fn used_scripts(
    coins: &[WalletCoin],
    pending: &[CoordinatorPending],
    past: &[CoordinatorPast],
) -> HashSet<ScriptBuf> {
    let mut used: HashSet<ScriptBuf> = coins.iter().map(|c| c.script_pubkey.clone()).collect();
    for p in pending {
        if let Ok(psbt) = Psbt::from_str(&p.psbt) {
            used.extend(
                psbt.unsigned_tx
                    .output
                    .iter()
                    .map(|o| o.script_pubkey.clone()),
            );
        }
    }
    for p in past {
        if let Ok(tx) =
            bitcoin::consensus::encode::deserialize_hex::<bitcoin::Transaction>(&p.raw_tx)
        {
            used.extend(tx.output.iter().map(|o| o.script_pubkey.clone()));
        }
    }
    used
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRequest {
    pub claims: Vec<ClaimInput>,
    /// Exact transactions (outpoints per transaction) once the admin edited them.
    pub groups: Option<Vec<Vec<String>>>,
    pub fee_rate: String,
}

/// Admin Wallet coins usable for a new plan, and the scripts already used.
pub(super) async fn plan_inputs(
    ctx: &PayoutContext,
    node: &dyn PayoutNode,
    fresh: bool,
) -> Result<(Vec<WalletCoin>, HashSet<ScriptBuf>), String> {
    let pending = ctx.coordinator.pending().await.map_err(|e| e.to_string())?;
    let past = ctx.coordinator.past().await.map_err(|e| e.to_string())?;
    let coins = {
        let mut cache = ctx.coins.lock().await;
        match cache.as_ref() {
            Some((at, who, c))
                if !fresh && *who == ctx.me.index && at.elapsed() < COIN_CACHE_TTL =>
            {
                c.clone()
            }
            _ => {
                let c = admin_wallet_coins(&ctx.dep, &ctx.me, node).await?;
                *cache = Some((std::time::Instant::now(), ctx.me.index, c.clone()));
                c
            }
        }
    };
    let used = used_scripts(&coins, &pending, &past);
    let (_, reserved, _) = pending_spends(&ctx.dep, &pending);
    Ok((usable_coins(&coins, &reserved), used))
}

pub async fn plan_for(
    ctx: &PayoutContext,
    req: &PlanRequest,
    node: &dyn PayoutNode,
    fresh: bool,
) -> Result<(PlanDto, Vec<WalletCoin>, HashSet<ScriptBuf>), String> {
    let (plan, coins, used) = typed_plan_for(ctx, req, node, fresh).await?;
    Ok((plan_dto(&plan, &coins), coins, used))
}

pub(super) async fn typed_plan_for(
    ctx: &PayoutContext,
    req: &PlanRequest,
    node: &dyn PayoutNode,
    fresh: bool,
) -> Result<(planner::Plan, Vec<WalletCoin>, HashSet<ScriptBuf>), String> {
    let (coins, used) = plan_inputs(ctx, node, fresh).await?;
    let plan = typed_plan(
        &ctx.dep,
        &req.claims,
        req.groups.as_deref(),
        &req.fee_rate,
        &coins,
    )?;
    Ok((plan, coins, used))
}

/// Builds the transactions of `plan`, but never one the coordinator has
/// let expire. The same claims, fee coin and change address give the same transaction again, and
/// an expired one cannot be submitted a second time; its change address then counts as used, so
/// the next address gives a new transaction. The checklist and Confirm both build through here.
pub(super) async fn build_new_psbts(
    ctx: &PayoutContext,
    plan: &planner::Plan,
    coins: &[WalletCoin],
    used: &HashSet<ScriptBuf>,
) -> Result<Vec<Psbt>, String> {
    // A coordinator that cannot list them is no reason to stop: it refuses an expired one itself.
    let expired: HashSet<String> = ctx
        .coordinator
        .expired()
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();
    let (dep, me) = (&*ctx.dep, &ctx.me);
    let mut used = used.clone();
    let floors = [0, 0];
    loop {
        if !plan.is_ok() {
            return Err(plan
                .txs
                .iter()
                .flat_map(|t| t.errors.iter())
                .next()
                .map(|e| e.message().to_string())
                .unwrap_or_else(|| "The plan has errors.".into()));
        }
        let coin_by: HashMap<OutPoint, &WalletCoin> =
            coins.iter().map(|c| (c.outpoint, c)).collect();
        let mut taken = used.clone();
        let mut psbts = Vec::new();
        for t in &plan.txs {
            let connectors = t.connectors();
            let fee_inputs: Vec<FeeInput> = t
                .fee_coins
                .iter()
                .map(|coin| {
                    let o = coin.outpoint;
                    let c = coin_by
                        .get(&o)
                        .ok_or_else(|| format!("fee coin {o} is no longer in the Admin Wallet"))?;
                    fee_input(dep, me, c)
                })
                .collect::<Result<_, _>>()?;
            let idx = pick_change_index(me, |i| i < floors[1], &taken)?;
            let change = change_output(dep, me, idx, t.change_sat)?;
            taken.insert(change.script_pubkey());
            let psbt = payout_tx::build_psbt(dep, &connectors, &fee_inputs, &change)
                .map_err(|e| e.to_string())?;
            // Exact numbers the device will show must be the ones previewed.
            let a = payout_tx::analyze(dep, &psbt).map_err(|e| e.to_string())?;
            if a.fee_sat != t.fee_sat {
                return Err("The transaction changed since the preview; review it again.".into());
            }
            psbts.push(psbt);
        }
        let stale = psbts
            .iter()
            .find(|p| expired.contains(&p.unsigned_tx.compute_txid().to_string()));
        match stale.and_then(|p| p.unsigned_tx.output.first()) {
            // Ends: each round uses up one more of the finitely many change addresses.
            Some(change) => used.insert(change.script_pubkey.clone()),
            None => return Ok(psbts),
        };
    }
}

/// Confirm without a reviewed checklist to compare with (tests and tools). The app's Confirm
/// button uses [`confirm_reviewed`].
pub async fn confirm_request(
    ctx: &PayoutContext,
    req: &PlanRequest,
    expected_total_fee_sat: u64,
    store: &PolicyStore,
    node: &dyn PayoutNode,
) -> Result<Vec<SubmitResultDto>, String> {
    confirm_reviewed(ctx, req, expected_total_fee_sat, None, store, node).await
}

/// Confirm: re-plans from the same request, re-checks the claims on the node, builds the
/// transactions and signs and submits each. With `expected_txids` (the ids the checklist was
/// shown for, see [`LedgerReviewDto::txids`]) nothing is signed unless the transactions built now
/// are exactly those: a transaction id commits to every coin spent, the change address and the
/// amounts, so what the device shows is what the checklist said.
pub async fn confirm_reviewed(
    ctx: &PayoutContext,
    req: &PlanRequest,
    expected_total_fee_sat: u64,
    expected_txids: Option<&[String]>,
    store: &PolicyStore,
    node: &dyn PayoutNode,
) -> Result<Vec<SubmitResultDto>, String> {
    check_app_unchanged(ctx).await?;
    let (plan, coins, used) = typed_plan_for(ctx, req, node, true).await?;
    if plan.total_fee_sat() != expected_total_fee_sat {
        return Err("The fee changed since the preview; review it again.".into());
    }
    check_claims_on_node(&ctx.dep, &req.claims, node).await?;
    // Whatever happens, the next preview rescans (coins may now be spent).
    *ctx.coins.lock().await = None;
    let psbts = build_new_psbts(ctx, &plan, &coins, &used).await?;
    if let Some(expected) = expected_txids {
        let built: Vec<String> = psbts
            .iter()
            .map(|p| p.unsigned_tx.compute_txid().to_string())
            .collect();
        if built != expected {
            return Err(
                "The transaction changed since you reviewed it (another coin or change address \
                 is used now). Review it again; nothing was signed."
                    .into(),
            );
        }
    }
    sign_and_submit(ctx, psbts, store, node).await
}

/// Review of Confirm: builds the very transactions Confirm will sign.
pub async fn review_confirm(
    ctx: &PayoutContext,
    req: &PlanRequest,
    store: &PolicyStore,
    node: &dyn PayoutNode,
) -> Result<LedgerReviewDto, String> {
    check_app_unchanged(ctx).await?;
    let (plan, coins, used) = typed_plan_for(ctx, req, node, true).await?;
    let psbts = build_new_psbts(ctx, &plan, &coins, &used).await?;
    ledger_review(&ctx.dep, &ctx.me, &ctx.app, &psbts, true, store)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::super::*;
    use super::*;
    use bitcoin::hashes::Hash;

    #[tokio::test]
    async fn a_payout_that_expired_is_not_built_again_as_the_same_transaction() {
        let (fx, dep, me, _) = setup();
        let coin = WalletCoin {
            outpoint: OutPoint::new(Txid::from_byte_array([200; 32]), 0),
            value_sat: 50_000,
            script_pubkey: change_or_receive_script(&me, 0, 0).unwrap(),
            branch: 0,
            index: 0,
            confirmations: 6,
        };
        let claims = [ClaimInput {
            outpoint: OutPoint::new(Txid::from_byte_array([7; 32]), 1).to_string(),
            covenant_index: 0,
            payout_height: None,
        }];
        let plan = typed_plan(&dep, &claims, None, "2", std::slice::from_ref(&coin)).unwrap();
        let ctx = |coordinator| signed_in(&dep, &me, coordinator);

        let build = |ctx: PayoutContext| {
            let (plan, coin) = (plan.clone(), coin.clone());
            async move {
                let psbts = build_new_psbts(&ctx, &plan, &[coin], &HashSet::new())
                    .await
                    .unwrap();
                psbts[0].clone()
            }
        };
        // Nothing expired: the first unused change address.
        let first = build(ctx(coordinator_answering("[]".into()).await)).await;
        assert_eq!(
            first.unsigned_tx.output[0].script_pubkey,
            change_script(&me, 0).unwrap()
        );
        // That very transaction expired on the coordinator, which refuses it for good. The same
        // claims and coin now give another transaction: the change goes to the next address.
        let expired = format!("[\"{}\"]", first.unsigned_tx.compute_txid());
        let second = build(ctx(coordinator_answering(expired).await)).await;
        assert_ne!(
            second.unsigned_tx.compute_txid(),
            first.unsigned_tx.compute_txid()
        );
        assert_eq!(
            second.unsigned_tx.output[0].script_pubkey,
            change_script(&me, 1).unwrap()
        );
        let _ = fx;
    }

    #[test]
    fn change_addresses_stay_within_the_range_the_wallet_scan_covers() {
        let (_fx, _dep, me, _) = setup();
        let none = HashSet::new();
        assert_eq!(pick_change_index(&me, |_| false, &none), Ok(0));
        // Addresses up to the last scanned one are used: that one is still picked.
        assert_eq!(
            pick_change_index(&me, |i| i < SCAN_RANGE, &none),
            Ok(SCAN_RANGE)
        );
        // All used: an error, not an address the scan would never find a coin at.
        let e = pick_change_index(&me, |i| i <= SCAN_RANGE, &none).unwrap_err();
        assert!(e.contains("all used"), "{e}");
    }
}
