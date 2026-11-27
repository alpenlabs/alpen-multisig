use desktop_app::application::payout_admin::PayoutAdminState;
use payout_admin_core::wire;
use serde_json::Value;

macro_rules! commands {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $response:ty => $method:ident($($value:expr),*);)*) => {
        $(#[tauri::command]
        /// Runs the corresponding payout session operation on behalf of the desktop UI.
        pub async fn $name($($arg: $ty,)* state: tauri::State<'_, PayoutAdminState>) -> Result<$response, String> {
            state.$method($($value),*).await
        })*
    };
}
commands! {
    payout_admin_identity() -> Value => ledger_identity(false);
    payout_admin_show_key() -> Value => ledger_identity(true);
    payout_admin_sign_in_review(coordinator_url: String) -> Value => sign_in_review(coordinator_url);
    payout_admin_sign_in(review_id: String) -> Value => sign_in(review_id);
    payout_admin_sign_out() -> () => sign_out();
    payout_admin_overview() -> Value => pending_and_past();
    payout_admin_verify_pending_with_report(txid: String, text: String) -> () => verify_pending_with_report(txid, text);
    payout_admin_review_pending(txid: String) -> Value => review_block_payouts_tx(None, Some(txid));
    payout_admin_sign_pending(review_id: String) -> wire::PasteSignaturesView => sign_block_payouts_tx(review_id, false);
    payout_admin_copy_signatures(txid: String) -> String => copy_transaction_text(txid, true);
    payout_admin_paste_signatures(txid: String, signatures: String) -> wire::PasteSignaturesView => paste_signatures(txid, signatures);
    payout_admin_import_psbt(psbt: String) -> wire::PendingBlockPayoutsView => import_psbt(psbt);
    payout_admin_export_psbt(txid: String) -> String => copy_transaction_text(txid, false);
    payout_admin_retry_broadcast(txid: String) -> wire::BroadcastResponse => broadcast_block_payouts_tx(txid);
    payout_admin_rebroadcast(txid: String) -> wire::BroadcastResponse => broadcast_block_payouts_tx(txid);
    payout_admin_verify_reports(text: String) -> Value => check_reports(text);
    payout_admin_bundle(verification_id: String, fee_rate_tenths: u64, removed_inputs: Vec<String>) -> Value => bundle_claims(verification_id, fee_rate_tenths, removed_inputs);
    payout_admin_review_transaction(bundle_id: String, transaction_id: String) -> Value => review_block_payouts_tx(Some((bundle_id, transaction_id)), None);
    payout_admin_confirm_transaction(review_id: String) -> wire::PasteSignaturesView => sign_block_payouts_tx(review_id, true);
}

/// Loads ASM history and the signer roster, then saves the roster path in the desktop node settings.
#[tauri::command]
pub async fn payout_admin_load_deployment(
    signers_path: std::path::PathBuf,
    state: tauri::State<'_, PayoutAdminState>,
    config: tauri::State<'_, desktop_app::infrastructure::node_config_store::NodeConfigState>,
    app: tauri::AppHandle,
) -> Result<Value, String> {
    let result = state.load_deployment(signers_path.clone()).await?;
    let mut settings = config.0.write().map_err(|error| error.to_string())?;
    let mut next = settings.clone();
    next.payout_signers_path = Some(signers_path);
    desktop_app::infrastructure::node_config_store::save_node_config(&app, &next)?;
    *settings = next;

    Ok(result)
}

#[tauri::command]
pub async fn payout_admin_check_asm(
    state: tauri::State<'_, PayoutAdminState>,
) -> Result<(), String> {
    state.check_asm().await
}
