//! Plain Admin Wallet send and replacement against Bitcoin Core, with a software signer.
use desktop_app::application::payouts::{self, WalletBumpRequest, WalletSendRequest};
use serde_json::json;
mod payout_common;
use payout_common::*;

#[tokio::test(flavor = "multi_thread")]
async fn send_bump_and_confirm() {
    let PayoutEnv {
        fx,
        dep,
        params,
        roster,
        dir,
        node,
        coordinator,
        bitcoind: _bitcoind,
    } = payout_env("payout-wallet-e2e").await;
    let (params, roster) = (params.as_str(), roster.as_str());
    let state = admin_state(&fx, &dep, 0, params, roster, &coordinator).await;
    let ctx = state.ctx().await.unwrap();
    call(
        &node,
        "sendtoaddress",
        json!([wallet_address(&fx, 0, 0).to_string(), 0.001]),
    )
    .await;
    mine(&node, 1).await;
    let mut req = WalletSendRequest {
        address: wallet_address(&fx, 1, 0).to_string(),
        amount_sat: Some(20_000),
        max: false,
        fee_rate: "2.0".into(),
        expected_txid: None,
    };
    let review = payouts::wallet_prepare(&ctx, &payouts::WalletIntent::Send(req.clone()), &node)
        .await
        .unwrap();
    assert!(review.review.steps.iter().all(|s| s.warning.is_none()));
    req.expected_txid = Some(review.txid.clone());
    let sent = payouts::wallet_execute(
        &ctx,
        &payouts::WalletIntent::Send(req.clone()),
        req.expected_txid.as_deref().unwrap_or(""),
        &node,
    )
    .await
    .unwrap();
    assert_eq!(sent.txid, review.txid);
    let wallet = payouts::admin_wallet(&ctx, &node).await.unwrap();
    assert_eq!(wallet.sends.len(), 1);
    assert_eq!(wallet.sends[0].status, "mempool");
    assert_eq!(wallet.unconfirmed_sat, -(20_000 + review.fee_sat as i64));
    assert_eq!(wallet.total_sat, 100_000 - 20_000 - review.fee_sat);
    assert_eq!(wallet.available_sat, 0);
    let mut bump = WalletBumpRequest {
        txid: sent.txid,
        fee_rate: "5.0".into(),
        expected_txid: None,
    };
    let replacement =
        payouts::wallet_prepare(&ctx, &payouts::WalletIntent::Bump(bump.clone()), &node)
            .await
            .unwrap();
    assert_eq!(replacement.amount_sat, 20_000);
    bump.expected_txid = Some(replacement.txid.clone());
    payouts::wallet_execute(
        &ctx,
        &payouts::WalletIntent::Bump(bump.clone()),
        bump.expected_txid.as_deref().unwrap_or(""),
        &node,
    )
    .await
    .unwrap();
    let wallet = payouts::admin_wallet(&ctx, &node).await.unwrap();
    assert_eq!(wallet.sends.len(), 1);
    assert_eq!(wallet.sends[0].txid, replacement.txid);
    mine(&node, 1).await;
    let wallet = payouts::admin_wallet(&ctx, &node).await.unwrap();
    assert!(wallet.sends.is_empty());
    assert_eq!(wallet.unconfirmed_sat, 0);
    // Receive address 0 held the coin that was just spent: it is not offered a second time.
    assert_eq!(wallet.receive_index, 1);
    assert_eq!(wallet.total_sat, 80_000 - replacement.fee_sat);
    let recipient = call(&node, "gettxout", json!([replacement.txid, 0])).await;
    assert_eq!(recipient["value"].as_f64().unwrap(), 0.0002);
    std::fs::remove_dir_all(dir).unwrap();
}
