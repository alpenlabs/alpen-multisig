//! E2E: claims of several covenants in one payout, completed by a second signer and accepted by
//! Bitcoin Core, plus the transaction size boundary (TRUC, 10 000 vB) and the split into several
//! transactions. The Ledger is replaced by the software signer, so no Docker is needed.
//!
//! The operator schedule in `params.toml` creates one covenant per distinct operator set (each
//! with its own N/N key, connector script and wallet policy).

use std::str::FromStr;

use bitcoin::OutPoint;
use desktop_app::application::payouts::{
    self, ClaimInput, PayoutState, PlanRequest, SubmitResultDto,
};
use payout_admin_core::payout_tx::TRUC_MAX_VSIZE;
use payout_admin_core::planner;
use payout_admin_core::policy;
use serde_json::json;

mod payout_common;
use payout_common::*;

use payout_common::PayoutEnv as Env;

/// Three covenants: operator sets {0,1}, {0,1,2} and {0,2}.
async fn env(tag: &str) -> Env {
    env_with_asm(tag, "mock://asm-membership").await
}

async fn env_with_asm(tag: &str, asm_url: &str) -> Env {
    let e = payout_env_with_params(&format!("payout-multicov-{tag}"), asm_url, |fx| {
        fx.params_toml_with_schedule(&[(0, 101, None), (1, 101, Some(300)), (2, 200, None)])
    })
    .await;
    assert_eq!(e.dep.covenants.len(), 3);
    e
}

/// `n` claims spread over the covenants round-robin; mined in batches so the wallet's unconfirmed
/// chain stays short.
async fn fund_claims(e: &Env, n: usize) -> Vec<ClaimInput> {
    let mut out = Vec::new();
    for i in 0..n {
        let cov = i % e.dep.covenants.len();
        let connector = policy::connector_address(&e.dep, &e.dep.covenants[cov]);
        let txid = fund_claim(&e.node, &connector).await;
        out.push(ClaimInput {
            outpoint: OutPoint::new(txid, 1).to_string(),
            covenant_index: cov,
            payout_height: None,
        });
        if i % 15 == 14 {
            mine(&e.node, 1).await;
        }
    }
    mine(&e.node, 1).await;
    out
}

async fn fee_coins(e: &Env, n: u32, btc: f64) {
    fee_coins_for(e, 0, n, btc).await
}

async fn fee_coins_for(e: &Env, admin: usize, n: u32, btc: f64) {
    for i in 0..n {
        call(
            &e.node,
            "sendtoaddress",
            json!([wallet_address(&e.fx, admin, i).to_string(), btc]),
        )
        .await;
    }
    mine(&e.node, 1).await;
}

/// Admin 1 signs every pending tx that admin 0 created; returns the txids Core accepted.
async fn second_signer(e: &Env, results: &[SubmitResultDto]) -> Vec<String> {
    let a1 = admin_state(&e.fx, &e.dep, 1, &e.params, &e.roster, &e.coordinator).await;
    let ctx1 = a1.ctx().await.unwrap();
    let mut accepted = Vec::new();
    for r in results {
        assert_eq!(stored_signature_counts(&ctx1, &r.txid).await, (1, 2));
        let s = payouts::sign_pending(
            &ctx1.dep,
            &ctx1.me,
            &r.txid,
            ctx1.device.clone(),
            &a1.policies,
            &ctx1.coordinator,
            &e.node,
        )
        .await
        .expect("second signer");
        let b = s.broadcast.expect("broadcast at quorum");
        assert!(b.broadcast, "Core rejected {}: {:?}", r.txid, b.error);
        accepted.push(r.txid.clone());
    }
    accepted
}

async fn plan_and_confirm(
    e: &Env,
    a0: &PayoutState,
    req: &PlanRequest,
) -> (payouts::PlanDto, Vec<SubmitResultDto>) {
    let ctx0 = a0.ctx().await.unwrap();
    let (plan, _, _) = payouts::plan_for(&ctx0, req, &e.node, true).await.unwrap();
    assert!(
        plan.ok,
        "plan: {:?}",
        plan.txs.iter().flat_map(|t| &t.errors).collect::<Vec<_>>()
    );
    let res = payouts::confirm_request(&ctx0, req, plan.total_fee_sat, &a0.policies, &e.node)
        .await
        .expect("confirm");
    (plan, res)
}

/// Claims of all three covenants in one transaction, signed by two admins, accepted by Core.
#[tokio::test(flavor = "multi_thread")]
async fn claims_of_three_covenants_in_one_tx_are_accepted_by_core() {
    let e = env("mixed").await;
    let claims = fund_claims(&e, 6).await; // two per covenant
    fee_coins(&e, 1, 0.0005).await;
    let a0 = admin_state(&e.fx, &e.dep, 0, &e.params, &e.roster, &e.coordinator).await;
    let req = PlanRequest {
        claims: claims.clone(),
        groups: None,
        fee_rate: "2".into(),
    };
    let (plan, res) = plan_and_confirm(&e, &a0, &req).await;
    assert_eq!(plan.txs.len(), 1, "one transaction for all covenants");
    assert_eq!(res.len(), 1);

    let accepted = second_signer(&e, &res).await;
    let entry = call(&e.node, "getmempoolentry", json!([accepted[0]])).await;
    let core_vsize = entry["vsize"].as_u64().unwrap();
    println!(
        "mixed: app vsize {} / Core vsize {core_vsize}",
        plan.txs[0].vsize
    );
    assert!(
        core_vsize <= plan.txs[0].vsize,
        "app estimate is an upper bound"
    );
    mine(&e.node, 1).await;
    for c in &claims {
        let op: OutPoint = c.outpoint.parse().unwrap();
        let out = call(
            &e.node,
            "gettxout",
            json!([op.txid.to_string(), op.vout, false]),
        )
        .await;
        assert!(out.is_null(), "{} still unspent", c.outpoint);
    }
}

/// The biggest transaction the planner builds fits Core's TRUC limit; one claim more is split off
/// into a second transaction; a hand-made group over the limit is refused by the plan.
#[tokio::test(flavor = "multi_thread")]
async fn size_boundary_and_split_are_accepted_by_core() {
    let e = env("boundary").await;
    let change = payout_admin_core::payout_tx::p2tr_placeholder_script();
    let per_tx = planner::max_claims_per_tx(&e.dep, 2, &change);
    let absolute = planner::max_claims_per_tx(&e.dep, 1, &change);
    println!("claims per tx with 2 fee inputs: {per_tx}, with 1: {absolute}");
    assert!(
        per_tx > 50,
        "expected dozens of claims per tx, got {per_tx}"
    );

    // One claim over the planner's limit: per_tx + 1 claims are split in two.
    let claims = fund_claims(&e, per_tx + 1).await;
    fee_coins(&e, 4, 0.001).await;
    let a0 = admin_state(&e.fx, &e.dep, 0, &e.params, &e.roster, &e.coordinator).await;
    let req = PlanRequest {
        claims: claims.clone(),
        groups: None,
        fee_rate: "2".into(),
    };
    let (plan, res) = plan_and_confirm(&e, &a0, &req).await;
    let sizes: Vec<usize> = plan.txs.iter().map(|t| t.claims.len()).collect();
    println!(
        "split: {sizes:?}, vsizes {:?}",
        plan.txs.iter().map(|t| t.vsize).collect::<Vec<_>>()
    );
    assert_eq!(sizes, vec![per_tx, 1]);
    assert!(plan.txs.iter().all(|t| t.vsize <= TRUC_MAX_VSIZE));
    assert_eq!(res.len(), 2);

    let accepted = second_signer(&e, &res).await;
    for (txid, planned) in accepted.iter().zip(&plan.txs) {
        let entry = call(&e.node, "getmempoolentry", json!([txid])).await;
        let core_vsize = entry["vsize"].as_u64().unwrap();
        println!("tx {txid}: app {} / Core {core_vsize}", planned.vsize);
        assert!(core_vsize <= TRUC_MAX_VSIZE);
        assert!(core_vsize <= planned.vsize, "estimate must not undershoot");
    }
    mine(&e.node, 1).await;
    for c in &claims {
        let op: OutPoint = c.outpoint.parse().unwrap();
        assert!(call(
            &e.node,
            "gettxout",
            json!([op.txid.to_string(), op.vout, false])
        )
        .await
        .is_null());
    }

    // An edited group over the hard limit (even with a single fee input) is refused by the plan.
    let n = absolute + 1;
    let fresh = fund_claims(&e, n).await;
    fee_coins(&e, 2, 0.001).await;
    let over = PlanRequest {
        claims: fresh.clone(),
        groups: Some(vec![fresh.iter().map(|c| c.outpoint.clone()).collect()]),
        fee_rate: "2".into(),
    };
    let ctx0 = a0.ctx().await.unwrap();
    let (plan, _, _) = payouts::plan_for(&ctx0, &over, &e.node, true)
        .await
        .unwrap();
    println!(
        "over-limit group of {n}: ok={} errors={:?}",
        plan.ok, plan.txs[0].errors
    );
    assert!(!plan.ok, "a group over the TRUC size must not plan ok");
    assert!(
        plan.txs[0].errors.contains(&planner::PlanError::SizeLimit),
        "{:?}",
        plan.txs[0].errors
    );
}

/// Two admins' pending payouts share a claim: both mark it. A third admin signs one offline and it
/// is imported as a PSBT: quorum, broadcast, Core accepts. Once mined, the other payout, whose
/// input is now spent in a block, is dropped.
#[tokio::test(flavor = "multi_thread")]
async fn shared_inputs_are_marked_and_an_imported_psbt_completes_a_payout() {
    let e = env("import").await;
    let claims = fund_claims(&e, 2).await;
    fee_coins_for(&e, 0, 1, 0.0005).await;
    fee_coins_for(&e, 1, 1, 0.0005).await;
    let a0 = admin_state(&e.fx, &e.dep, 0, &e.params, &e.roster, &e.coordinator).await;
    let a1 = admin_state(&e.fx, &e.dep, 1, &e.params, &e.roster, &e.coordinator).await;
    let both = PlanRequest {
        claims: claims.clone(),
        groups: None,
        fee_rate: "2".into(),
    };
    let (_, r0) = plan_and_confirm(&e, &a0, &both).await;
    let only_second = PlanRequest {
        claims: vec![claims[1].clone()],
        groups: None,
        fee_rate: "2".into(),
    };
    let (_, r1) = plan_and_confirm(&e, &a1, &only_second).await;
    let (p0, p1) = (r0[0].txid.clone(), r1[0].txid.clone());

    let ctx0 = a0.ctx().await.unwrap();
    let ov = payouts::overview(&ctx0.dep, Some(0), &ctx0.coordinator, &e.node)
        .await
        .unwrap();
    assert_eq!(ov.pending.len(), 2);
    for p in &ov.pending {
        assert!(p.has_duplicates, "{} should be marked", p.txid);
        for i in &p.inputs {
            assert_eq!(
                i.duplicate,
                i.outpoint == claims[1].outpoint,
                "{}",
                i.outpoint
            );
        }
    }

    // Admin 2 exports payout p0, signs it offline, and anyone imports the result.
    let pending = ctx0.coordinator.get_pending(&p0).await.unwrap();
    let mut psbt = bitcoin::psbt::Psbt::from_str(&pending.psbt).unwrap();
    payout_admin_core::test_support::sign_as(&e.fx, &e.dep, &mut psbt, 2);
    let r = payouts::import(&ctx0.dep, &psbt.to_string(), &ctx0.coordinator, &e.node)
        .await
        .expect("import");
    assert_eq!(stored_signature_counts(&ctx0, &r.txid).await, (2, 2));
    let b = r.broadcast.expect("broadcast at quorum");
    assert!(
        b.broadcast,
        "Core rejected the imported payout: {:?}",
        b.error
    );

    // Mempool only: p1 stays pending (decision: only a spend in a block expires it).
    let ov = payouts::overview(&ctx0.dep, Some(0), &ctx0.coordinator, &e.node)
        .await
        .unwrap();
    assert_eq!(
        ov.pending.iter().map(|p| &p.txid).collect::<Vec<_>>(),
        [&p1]
    );
    mine(&e.node, 1).await;
    let ov = payouts::overview(&ctx0.dep, Some(0), &ctx0.coordinator, &e.node)
        .await
        .unwrap();
    assert!(
        ov.pending.is_empty(),
        "p1's input is spent in a block: {:?}",
        ov.pending.iter().map(|p| &p.txid).collect::<Vec<_>>()
    );
    assert_eq!(ov.past.len(), 1);
    assert_eq!(ov.past[0].txid, p0);
}

/// Block Payouts work while the ASM endpoint is unreachable (the design doc: no ASM needed).
#[tokio::test(flavor = "multi_thread")]
async fn payouts_work_with_the_asm_unreachable() {
    // Port 9 (discard) on loopback: nothing listens.
    let e = env_with_asm("noasm", "http://127.0.0.1:9").await;
    let claims = fund_claims(&e, 1).await;
    fee_coins(&e, 1, 0.0005).await;
    let a0 = admin_state(&e.fx, &e.dep, 0, &e.params, &e.roster, &e.coordinator).await;
    let req = PlanRequest {
        claims,
        groups: None,
        fee_rate: "2".into(),
    };
    let (_, res) = plan_and_confirm(&e, &a0, &req).await;
    let accepted = second_signer(&e, &res).await;
    assert_eq!(accepted.len(), 1);
}
