use desktop_app::application::orchestrator_url::validate_orchestrator_base_url;
use desktop_app::application::payouts::{
    self, DeploymentDto, LedgerReviewDto, PayoutSessionDto, PayoutState,
};
use desktop_app::infrastructure::node_config_store::NodeConfigState;
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
#[tauri::command]
pub async fn payout_sign_out(state: tauri::State<'_, PayoutState>) -> Result<(), String> {
    state.sign_out().await;
    Ok(())
}

use desktop_app::application::payouts::VerifyReportsDto;
use desktop_app::infrastructure::bitcoin_rpc::HttpBitcoinRpcClient;
fn node(cfg: &tauri::State<'_, NodeConfigState>) -> Result<HttpBitcoinRpcClient, String> {
    let c = cfg.0.read().map_err(|e| format!("lock error: {e}"))?;
    Ok(HttpBitcoinRpcClient::new(
        c.btc_rpc_url(),
        c.btc_rpc_user(),
        c.btc_rpc_pass(),
    ))
}

#[tauri::command]
pub async fn payout_verify_reports(
    text: String,
    cfg: tauri::State<'_, NodeConfigState>,
    state: tauri::State<'_, PayoutState>,
) -> Result<VerifyReportsDto, String> {
    let ctx = state.ctx().await?;
    let node = node(&cfg)?;
    let in_pending = std::collections::HashSet::new();
    payouts::verify_reports(&ctx.dep, &text, &node, &in_pending).await
}
