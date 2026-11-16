//! Block Payouts (Payout Administrators). Thin wrappers over `application::payouts`.

use desktop_app::application::orchestrator_url::validate_orchestrator_base_url;
use desktop_app::application::payouts::{
    self, BroadcastDto, DeploymentDto, LedgerReviewDto, PasteResultDto, PayoutSessionDto,
    PayoutState, PayoutsOverviewDto, PlanDto, PlanRequest, SubmitResultDto, VerifyReportsDto,
};
use desktop_app::infrastructure::bitcoin_rpc::HttpBitcoinRpcClient;
use desktop_app::infrastructure::node_config_store::NodeConfigState;
use serde::Deserialize;

fn node(cfg: &tauri::State<'_, NodeConfigState>) -> Result<HttpBitcoinRpcClient, String> {
    let c = cfg.0.read().map_err(|e| format!("lock error: {e}"))?;
    Ok(HttpBitcoinRpcClient::new(
        c.btc_rpc_url(),
        c.btc_rpc_user(),
        c.btc_rpc_pass(),
    ))
}

/// Loads params.toml (and the signer roster) from the paths in Node settings.
#[tauri::command]
pub async fn payout_load_deployment(
    cfg: tauri::State<'_, NodeConfigState>,
    state: tauri::State<'_, PayoutState>,
) -> Result<DeploymentDto, String> {
    let (params, roster) = {
        let c = cfg.0.read().map_err(|e| format!("lock error: {e}"))?;
        (c.payout_params_path.clone(), c.payout_roster_path.clone())
    };
    let params = params
        .filter(|p| !p.trim().is_empty())
        .ok_or("Set the bridge parameters file (params.toml) in Node settings.")?;
    let dep = payouts::load_deployment(&params, roster.as_deref())?;
    Ok(state.load(dep).await)
}

/// Connects the Ledger and returns what it will show for the sign-in message.
#[tauri::command]
pub async fn payout_sign_in_review(
    base_url: String,
    state: tauri::State<'_, PayoutState>,
) -> Result<LedgerReviewDto, String> {
    validate_orchestrator_base_url(&base_url)?;
    payouts::sign_in_review(&state, &base_url).await
}

#[tauri::command]
pub async fn payout_sign_in(
    base_url: String,
    state: tauri::State<'_, PayoutState>,
) -> Result<PayoutSessionDto, String> {
    validate_orchestrator_base_url(&base_url)?;
    payouts::sign_in(&state, &base_url).await
}

/// The Admin Wallet coins and next receive address the payouts use.

/// Shows an Admin Wallet receive address on the Ledger.

#[tauri::command]
pub async fn payout_sign_out(state: tauri::State<'_, PayoutState>) -> Result<(), String> {
    state.sign_out().await;
    Ok(())
}

#[tauri::command]
pub async fn payout_overview(
    cfg: tauri::State<'_, NodeConfigState>,
    state: tauri::State<'_, PayoutState>,
) -> Result<PayoutsOverviewDto, String> {
    let ctx = state.ctx().await?;
    let node = node(&cfg)?;
    payouts::overview_for(&ctx, &node).await
}

#[tauri::command]
pub async fn payout_verify_reports(
    text: String,
    cfg: tauri::State<'_, NodeConfigState>,
    state: tauri::State<'_, PayoutState>,
) -> Result<VerifyReportsDto, String> {
    let ctx = state.ctx().await?;
    let node = node(&cfg)?;
    let pending = ctx.coordinator.pending().await.map_err(|e| e.to_string())?;
    let (in_pending, _, _) = payouts::pending_spends(&ctx.dep, &pending);
    payouts::verify_reports(&ctx.dep, &text, &node, &in_pending).await
}

#[tauri::command]
pub async fn payout_plan(
    request: PlanRequest,
    cfg: tauri::State<'_, NodeConfigState>,
    state: tauri::State<'_, PayoutState>,
) -> Result<PlanDto, String> {
    let ctx = state.ctx().await?;
    let node = node(&cfg)?;
    payouts::plan_for(&ctx, &request, &node, false)
        .await
        .map(|(p, _, _)| p)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmInput {
    pub request: PlanRequest,
    pub expected_total_fee_sat: u64,
    /// The transaction ids of the checklist the admin reviewed (`LedgerReviewDto::txids`).
    pub expected_txids: Vec<String>,
}

#[tauri::command]
pub async fn payout_confirm(
    input: ConfirmInput,
    cfg: tauri::State<'_, NodeConfigState>,
    state: tauri::State<'_, PayoutState>,
) -> Result<Vec<SubmitResultDto>, String> {
    let ctx = state.ctx().await?;
    let node = node(&cfg)?;
    payouts::confirm_reviewed(
        &ctx,
        &input.request,
        input.expected_total_fee_sat,
        Some(&input.expected_txids),
        &state.policies,
        &node,
    )
    .await
}

#[tauri::command]
pub async fn payout_sign(
    txid: String,
    cfg: tauri::State<'_, NodeConfigState>,
    state: tauri::State<'_, PayoutState>,
) -> Result<SubmitResultDto, String> {
    let ctx = state.ctx().await?;
    let node = node(&cfg)?;
    payouts::check_app_unchanged(&ctx).await?;
    payouts::sign_pending(
        &ctx.dep,
        &ctx.me,
        &txid,
        ctx.device.clone(),
        &state.policies,
        &ctx.coordinator,
        &node,
    )
    .await
}

/// What the Ledger will show when signing a pending transaction.
#[tauri::command]
pub async fn payout_review_sign(
    txid: String,
    cfg: tauri::State<'_, NodeConfigState>,
    state: tauri::State<'_, PayoutState>,
) -> Result<LedgerReviewDto, String> {
    let ctx = state.ctx().await?;
    let node = node(&cfg)?;
    payouts::review_sign(&ctx, &txid, &state.policies, &node).await
}

/// What the Ledger will show when Confirm signs the planned transactions.
#[tauri::command]
pub async fn payout_review_confirm(
    request: PlanRequest,
    cfg: tauri::State<'_, NodeConfigState>,
    state: tauri::State<'_, PayoutState>,
) -> Result<LedgerReviewDto, String> {
    let ctx = state.ctx().await?;
    let node = node(&cfg)?;
    payouts::review_confirm(&ctx, &request, &state.policies, &node).await
}

#[tauri::command]
pub async fn payout_paste_signatures(
    txid: String,
    text: String,
    cfg: tauri::State<'_, NodeConfigState>,
    state: tauri::State<'_, PayoutState>,
) -> Result<PasteResultDto, String> {
    let ctx = state.ctx().await?;
    let node = node(&cfg)?;
    payouts::paste_signatures(&ctx.dep, &txid, &text, &ctx.coordinator, &node).await
}

#[tauri::command]
pub async fn payout_import(
    text: String,
    cfg: tauri::State<'_, NodeConfigState>,
    state: tauri::State<'_, PayoutState>,
) -> Result<SubmitResultDto, String> {
    let ctx = state.ctx().await?;
    let node = node(&cfg)?;
    payouts::import(&ctx.dep, &text, &ctx.coordinator, &node).await
}

/// Copy all signatures of a pending transaction (one line each).
#[tauri::command]
pub async fn payout_signatures_text(
    txid: String,
    state: tauri::State<'_, PayoutState>,
) -> Result<String, String> {
    let ctx = state.ctx().await?;
    let p = ctx
        .coordinator
        .get_pending(&txid)
        .await
        .map_err(|e| e.to_string())?;
    payouts::signatures_text(&ctx.dep, &p.psbt)
}

#[tauri::command]
pub async fn payout_retry_broadcast(
    txid: String,
    cfg: tauri::State<'_, NodeConfigState>,
    state: tauri::State<'_, PayoutState>,
) -> Result<BroadcastDto, String> {
    let ctx = state.ctx().await?;
    let node = node(&cfg)?;
    payouts::retry_broadcast(&ctx.dep, &txid, &ctx.coordinator, &node).await
}

/// Rebroadcast a past (still unconfirmed) transaction.
#[tauri::command]
pub async fn payout_rebroadcast(
    txid: String,
    cfg: tauri::State<'_, NodeConfigState>,
    state: tauri::State<'_, PayoutState>,
) -> Result<String, String> {
    let ctx = state.ctx().await?;
    let node = node(&cfg)?;
    let past = ctx.coordinator.past().await.map_err(|e| e.to_string())?;
    let p = past
        .iter()
        .find(|p| p.txid == txid)
        .ok_or("That transaction is not in Past.")?;
    payouts::rebroadcast(&ctx.dep, &txid, &p.psbt, &p.raw_tx, &node).await
}
