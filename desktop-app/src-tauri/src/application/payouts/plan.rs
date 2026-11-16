use super::{
    admin_wallet_coins, ChildNumber, Deployment, Deserialize, FromStr, HashMap, HashSet, OutPoint,
    PayoutContext, PayoutNode, ScriptBuf, Secp256k1, Serialize, WalletCoin, XOnlyPublicKey, Xpub,
    COIN_CACHE_TTL,
};
pub use payout_admin_core::planner::{ClaimInput, PlannedTx as PlannedTxDto};
use payout_admin_core::{
    fees, payout_tx,
    planner::{self, PlanClaim, PlanCoin},
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDto {
    pub txs: Vec<PlannedTxDto>,
    pub fee_rate_label: String,
    pub total_fee_sat: u64,
    pub ok: bool,
    pub available_sat: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRequest {
    pub claims: Vec<ClaimInput>,
    /// Exact transactions (outpoints per transaction) once the admin edited them.
    pub groups: Option<Vec<Vec<String>>>,
    pub fee_rate: String,
}

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
pub(super) async fn plan_inputs(
    ctx: &PayoutContext,
    node: &dyn PayoutNode,
    fresh: bool,
) -> Result<(Vec<WalletCoin>, HashSet<ScriptBuf>), String> {
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
    let used = coins.iter().map(|c| c.script_pubkey.clone()).collect();
    Ok((usable_coins(&coins, &HashSet::new()), used))
}
