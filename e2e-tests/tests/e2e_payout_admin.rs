//! E2E: Admin Block Payouts on regtest.
//!
//! Real Bitcoin Core (txindex), the real coordinator binary (`PAYOUT_ADMIN_PARAMS_PATH` set, in-
//! memory store) and the real desktop application layer (`application::payouts`). Only the Ledger
//! is replaced by a software signer that holds the fixture admins' keys and signs the way the
//! device does (claim leaf with the params.toml key, fee inputs by key path).
//!
//! Flow (2-of-3):
//! 1. Two claims' admin payout connectors (330 sat at the covenant connector script, vout 1) and
//!    Admin Wallet coins are mined.
//! 2. Admin 0 signs in (BIP-137 on the "device"), plans and confirms: one pending tx, 1/2.
//! 3. Admin 1 signs in and signs it: quorum, broadcast by the app, accepted by Core (TRUC v3,
//!    multisig leaf + key-path fee input), moved to Past as unconfirmed; mined → confirmed.
//! 4. A second payout is completed by pasting admin 2's signature lines (plus a forged line,
//!    which is reported and ignored).
//! 5. Reports that are invalid against the node are ignored, and a non-admin cannot sign in.

use std::str::FromStr;
use std::sync::Arc;

use bitcoin::psbt::Psbt;
use bitcoin::{OutPoint, Txid};
use desktop_app::application::payouts::{self, ClaimInput, PayoutState, PlanRequest};
use desktop_app::infrastructure::payout_policy_store::PolicyStore;
use payout_admin_core::policy;
use payout_admin_core::test_support::{sign_as, Fixture};
use serde_json::json;

mod payout_common;
use payout_common::*;

// ─── Test ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn e2e_block_payouts_regtest() {
    // Bitcoin Core with txindex.
    let PayoutEnv {
        fx,
        dep,
        params,
        roster,
        dir,
        node,
        coordinator,
        bitcoind: _bitcoind,
    } = payout_env("payout-e2e").await;
    let (params, roster) = (params.as_str(), roster.as_str());

    // On-chain: 3 claims at covenant 0's connector, Admin Wallet coins for admin 0.
    let connector = policy::connector_address(&dep, &dep.covenants[0]);
    let mut claims = Vec::new();
    for _ in 0..3 {
        claims.push(fund_claim(&node, &connector).await);
    }
    for i in 0..2 {
        call(
            &node,
            "sendtoaddress",
            json!([wallet_address(&fx, 0, i).to_string(), 0.0005]),
        )
        .await;
    }
    mine(&node, 1).await;

    // Non-admin cannot sign in.
    let outsider = Fixture::new(4, 2);
    let stranger = PayoutState::new(
        Arc::new(SoftDevice {
            fx: Arc::new(outsider),
            dep: dep.clone(),
            admin: 3,
        }),
        PolicyStore::new(None),
    );
    stranger
        .load(payouts::load_deployment(params, Some(roster)).unwrap())
        .await;
    let err = payouts::sign_in(&stranger, &coordinator.base_url)
        .await
        .unwrap_err();
    assert_eq!(err, payouts::MSG_NOT_A_PAYOUT_ADMIN);

    // ── Admin 0 plans and confirms the first two claims.
    let a0 = admin_state(&fx, &dep, 0, params, roster, &coordinator).await;
    let ctx0 = a0.ctx().await.unwrap();
    // The Admin Wallet: both coins (receive 0 and 1), next receive address 2, shown on the device.
    let fw = payouts::admin_wallet(&ctx0, &node).await.unwrap();
    assert_eq!(fw.coins.len(), 2);
    assert_eq!(fw.total_sat, 100_000);
    assert_eq!(fw.available_sat, 100_000);
    assert_eq!(fw.receive_index, 2);
    assert_eq!(fw.receive_address, wallet_address(&fx, 0, 2).to_string());
    let shown = payouts::show_receive_address(&ctx0, 2).await.unwrap();
    assert_eq!(Some(shown), fw.receive_on_device);
    let claim_input = |t: Txid| ClaimInput {
        outpoint: OutPoint::new(t, 1).to_string(),
        covenant_index: 0,
        payout_height: None,
    };
    let req = PlanRequest {
        claims: vec![claim_input(claims[0]), claim_input(claims[1])],
        groups: None,
        fee_rate: "2.5".into(),
    };
    let (plan, coins, _) = payouts::plan_for(&ctx0, &req, &node, true).await.unwrap();
    assert_eq!(
        coins.len(),
        2,
        "both Admin Wallet coins found via scantxoutset"
    );
    assert!(plan.ok, "{:?}", plan.txs[0].errors);
    assert_eq!(plan.txs.len(), 1);
    assert_eq!(plan.txs[0].fee_coins.len(), 1);
    // The checklist names the transaction it describes. Confirm signs only if it builds exactly
    // that one again: not when the checklist was for something else.
    let review = payouts::review_confirm(&ctx0, &req, &a0.policies, &node)
        .await
        .expect("review");
    assert_eq!(review.txids.len(), 1);
    let stale = vec!["11".repeat(32)];
    let err = payouts::confirm_reviewed(
        &ctx0,
        &req,
        plan.total_fee_sat,
        Some(&stale),
        &a0.policies,
        &node,
    )
    .await
    .unwrap_err();
    assert!(err.contains("changed since you reviewed it"), "{err}");
    let res = payouts::confirm_reviewed(
        &ctx0,
        &req,
        plan.total_fee_sat,
        Some(&review.txids),
        &a0.policies,
        &node,
    )
    .await
    .expect("confirm");
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].txid, review.txids[0]);
    assert_eq!(stored_signature_counts(&ctx0, &res[0].txid).await, (1, 2));
    assert!(res[0].broadcast.is_none());
    let txid1 = res[0].txid.clone();

    // A changed fee since the preview is refused.
    let err = payouts::confirm_request(&ctx0, &req, plan.total_fee_sat + 1, &a0.policies, &node)
        .await
        .unwrap_err();
    assert!(
        err.contains("fee changed") || err.contains("already"),
        "{err}"
    );

    // Pending as admin 1 sees it.
    let a1 = admin_state(&fx, &dep, 1, params, roster, &coordinator).await;
    let ctx1 = a1.ctx().await.unwrap();
    let ov = payouts::overview(&ctx1.dep, Some(1), &ctx1.coordinator, &node)
        .await
        .unwrap();
    assert_eq!(ov.pending.len(), 1);
    let p = &ov.pending[0];
    assert_eq!(p.txid, txid1);
    assert_eq!(p.connector_count, 2);
    assert!(!p.signed_by_me);
    assert!(!p.has_duplicates);
    let stored = ctx0.coordinator.get_pending(&p.txid).await.unwrap();
    assert!(p.expires_at > stored.created_at);
    // No change address is used yet: the first payout must use internal index 0.
    let change = p.change_address.as_deref().expect("change address");
    assert_eq!(change, change_address_of(&fx, 0, 0));
    assert!(p.change_sat > 0);

    // ── Admin 1 signs: quorum → broadcast → Past (unconfirmed) → mined → confirmed.
    let r = payouts::sign_pending(
        &ctx1.dep,
        &ctx1.me,
        &txid1,
        ctx1.device.clone(),
        &a1.policies,
        &ctx1.coordinator,
        &node,
    )
    .await
    .expect("sign");
    let b = r.broadcast.expect("broadcast at quorum");
    assert!(b.broadcast, "Core rejected the payout: {:?}", b.error);
    let mempool = call(&node, "getrawmempool", json!([])).await;
    assert!(mempool
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t == &json!(txid1)));
    let ov = payouts::overview(&ctx1.dep, Some(1), &ctx1.coordinator, &node)
        .await
        .unwrap();
    assert!(ov.pending.is_empty());
    assert_eq!(ov.past.len(), 1);
    assert_eq!(ov.past[0].status, "unconfirmed");
    // Rebroadcast of an unconfirmed past tx is accepted.
    assert_eq!(
        payouts::rebroadcast(
            &dep,
            &txid1,
            &ctx0.coordinator.past().await.unwrap()[0].psbt,
            &ov.past[0].raw_tx,
            &node
        )
        .await
        .unwrap(),
        txid1
    );
    mine(&node, 1).await;
    let ov = payouts::overview(&ctx1.dep, Some(1), &ctx1.coordinator, &node)
        .await
        .unwrap();
    assert_eq!(ov.past[0].status, "confirmed");
    assert!(ov.past[0].block_timestamp.is_some());
    // Both connectors are now spent on chain.
    for c in &claims[..2] {
        let out = call(&node, "gettxout", json!([c.to_string(), 1, false])).await;
        assert!(out.is_null());
    }

    // ── Second payout, completed by pasting admin 2's signatures.
    let req2 = PlanRequest {
        claims: vec![claim_input(claims[2])],
        groups: None,
        fee_rate: "1".into(),
    };
    let (plan2, _, _) = payouts::plan_for(&ctx0, &req2, &node, true).await.unwrap();
    assert!(plan2.ok, "{:?}", plan2.txs[0].errors);
    let res2 = payouts::confirm_request(&ctx0, &req2, plan2.total_fee_sat, &a0.policies, &node)
        .await
        .expect("confirm 2");
    let txid2 = res2[0].txid.clone();
    let pending2 = ctx0.coordinator.get_pending(&txid2).await.unwrap();
    let mut psbt2 = Psbt::from_str(&pending2.psbt).unwrap();
    // The first payout's confirmed change/Past record occupies index 0, even if its
    // coin is selected as a fee input now. The next unused change address is index 1.
    let ov2 = payouts::overview(&dep, Some(0), &ctx0.coordinator, &node)
        .await
        .unwrap();
    assert_eq!(ov2.pending.len(), 1);
    assert_eq!(ov2.pending[0].txid, txid2);
    assert_eq!(
        ov2.pending[0].change_address.as_deref(),
        Some(change_address_of(&fx, 0, 1).as_str())
    );
    // Admin 2 signs offline and copies their signature lines.
    sign_as(&fx, &dep, &mut psbt2, 2);
    let lines = payouts::signatures_text(&dep, &psbt2.to_string()).unwrap();
    let a2_lines: Vec<&str> = lines
        .lines()
        .filter(|l| l.contains(&fx.admin_xonly(2).to_string()))
        .collect();
    assert_eq!(a2_lines.len(), 1);
    // A forged line for admin 1 (admin 2's signature under admin 1's key).
    let forged = a2_lines[0].replace(
        &fx.admin_xonly(2).to_string(),
        &fx.admin_xonly(1).to_string(),
    );
    let paste = format!("{}\n{forged}\n", a2_lines[0]);
    let r = payouts::paste_signatures(&dep, &txid2, &paste, &ctx0.coordinator, &node)
        .await
        .unwrap();
    assert_eq!(r.added, 1);
    assert_eq!(
        payout_admin_core::payout_tx::paste_signatures(&dep, &mut psbt2.clone(), &paste)
            .invalid
            .len(),
        1
    );
    assert!(r.message.is_some());
    let b = r.broadcast.expect("broadcast after paste");
    assert!(b.broadcast, "Core rejected payout 2: {:?}", b.error);
    mine(&node, 1).await;
    let ov = payouts::overview(&ctx0.dep, Some(0), &ctx0.coordinator, &node)
        .await
        .unwrap();
    assert!(ov.pending.is_empty());
    assert_eq!(ov.past.len(), 2);
    assert!(ov.past.iter().all(|p| p.status == "confirmed"));

    // ── Reports: valid JSON whose transactions are not this deployment's game are invalid.
    let fake = json!({
        "operatorPubkey": fx.operator_xonly(0).to_string(),
        "newClaimTxid": claims[2].to_string(),
        "newContestTxid": claims[1].to_string(),
        "ackedClaimTxid": claims[0].to_string(),
        "ackedContestTxid": claims[0].to_string(),
        "counterproofAckTxid": claims[0].to_string(),
    });
    let v = payouts::verify_reports(
        &dep,
        &format!("{fake}\n{{\"foo\": 1}}"),
        &node,
        &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(v.malformed.len(), 1);
    assert_eq!(v.reports.len(), 1);
    assert_ne!(
        v.reports[0].verified.verdict,
        payout_admin_core::report::ReportVerdict::Valid
    );

    // Spent connectors cannot be confirmed again.
    let err = payouts::check_claims_on_node(&dep, &req2.claims, &node)
        .await
        .unwrap_err();
    assert!(err.contains("spent"), "{err}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn valid_mined_report_reaches_a_confirmed_payout() {
    use payout_admin_core::report::{FalseClaimReport, ReportVerdict, REPORT_TYPE, REPORT_VERSION};

    let PayoutEnv {
        fx,
        dep,
        params,
        roster,
        dir,
        node,
        coordinator,
        bitcoind: _bitcoind,
    } = payout_env("payout-report-e2e").await;
    let (params, roster) = (params.as_str(), roster.as_str());

    let nn = n_of_n_keypair(&fx, &dep);
    let (old, ack) = acked_game(&node, &fx, &dep, &nn).await;
    let new = claim_and_contest(&node, &fx, &dep, &nn).await;
    mine(&node, 1).await;
    let claim_height = call(&node, "getblockcount", json!([]))
        .await
        .as_u64()
        .unwrap();
    let report = FalseClaimReport {
        report_type: Some(REPORT_TYPE.into()),
        version: Some(REPORT_VERSION),
        network: Some("regtest".into()),
        created_at: None,
        operator_pubkey: fx.operator_xonly(0),
        new_claim_txid: new.claim.compute_txid(),
        new_contest_txid: new.contest.compute_txid(),
        acked_claim_txid: old.claim.compute_txid(),
        acked_contest_txid: old.contest.compute_txid(),
        counterproof_ack_txid: ack.compute_txid(),
    };
    let verified = payouts::verify_reports(
        &dep,
        &serde_json::to_string(&report).unwrap(),
        &node,
        &Default::default(),
    )
    .await
    .unwrap();
    assert!(verified.malformed.is_empty());
    assert_eq!(verified.reports.len(), 1);
    let v = &verified.reports[0].verified;
    assert_eq!(v.verdict, ReportVerdict::Valid, "{:?}", v.reasons);
    assert_eq!(v.outpoint, OutPoint::new(new.claim.compute_txid(), 1));
    assert_eq!(v.covenant_index, Some(0));
    assert_eq!(v.operator_index, Some(0));
    assert_eq!(v.new_claim_height, Some(claim_height));
    assert_eq!(
        v.payout_height,
        Some(claim_height + u64::from(dep.contest_timelock))
    );

    call(
        &node,
        "sendtoaddress",
        json!([wallet_address(&fx, 0, 0).to_string(), 0.001]),
    )
    .await;
    mine(&node, 1).await;
    let a0 = admin_state(&fx, &dep, 0, params, roster, &coordinator).await;
    let ctx0 = a0.ctx().await.unwrap();
    // Plan from the node-verified result, not a separately constructed claim input.
    let req = PlanRequest {
        claims: vec![ClaimInput {
            outpoint: v.outpoint.to_string(),
            covenant_index: v.covenant_index.unwrap(),
            payout_height: v.payout_height,
        }],
        groups: None,
        fee_rate: "2".into(),
    };
    let (plan, _, _) = payouts::plan_for(&ctx0, &req, &node, true).await.unwrap();
    assert!(plan.ok, "{:?}", plan.txs);
    assert_eq!(plan.txs.len(), 1);
    let confirmed = payouts::confirm_request(&ctx0, &req, plan.total_fee_sat, &a0.policies, &node)
        .await
        .unwrap();
    assert_eq!(confirmed.len(), 1);
    assert_eq!(
        stored_signature_counts(&ctx0, &confirmed[0].txid).await,
        (1, 2)
    );
    assert!(confirmed[0].broadcast.is_none());
    let txid = &confirmed[0].txid;
    let pending = ctx0.coordinator.get_pending(txid).await.unwrap();
    let psbt = Psbt::from_str(&pending.psbt).unwrap();
    let claim_txids = [new.claim.compute_txid(), old.claim.compute_txid()];
    let claim_inputs: Vec<_> = psbt
        .unsigned_tx
        .input
        .iter()
        .map(|i| i.previous_output)
        .filter(|op| claim_txids.contains(&op.txid))
        .collect();
    assert_eq!(claim_inputs, vec![v.outpoint]);
    let ov = payouts::overview(&dep, Some(0), &ctx0.coordinator, &node)
        .await
        .unwrap();
    assert_eq!(ov.pending.len(), 1);
    assert_eq!(ov.pending[0].connector_count, 1);

    let a1 = admin_state(&fx, &dep, 1, params, roster, &coordinator).await;
    let ctx1 = a1.ctx().await.unwrap();
    let signed = payouts::sign_pending(
        &dep,
        &ctx1.me,
        txid,
        ctx1.device.clone(),
        &a1.policies,
        &ctx1.coordinator,
        &node,
    )
    .await
    .unwrap();
    let broadcast = signed.broadcast.expect("broadcast at quorum");
    assert!(
        broadcast.broadcast,
        "Core rejected the payout: {:?}",
        broadcast.error
    );
    let mempool = call(&node, "getrawmempool", json!([])).await;
    assert!(mempool.as_array().unwrap().contains(&json!(txid)));
    mine(&node, 1).await;
    let tx = call(&node, "getrawtransaction", json!([txid, true])).await;
    assert!(tx["confirmations"].as_u64().unwrap() >= 1);
    assert!(call(
        &node,
        "gettxout",
        json!([v.outpoint.txid.to_string(), 1, false])
    )
    .await
    .is_null());
    let ov = payouts::overview(&dep, Some(0), &ctx0.coordinator, &node)
        .await
        .unwrap();
    assert!(ov.pending.is_empty());
    assert_eq!(ov.past.len(), 1);
    assert_eq!(ov.past[0].txid, *txid);
    assert_eq!(ov.past[0].status, "confirmed");
    let _ = std::fs::remove_dir_all(&dir);
}
