//! Manual walkthrough helper (not a CI test): prints admin 1's ("Bob") signature lines for a
//! pending payout, plus one forged line, for the paste-signatures step. Needs `PAYOUT_DEMO_DIR`
//! (from `demo_payout_stack`) and `PAYOUT_DEMO_TXID`.

use std::str::FromStr;
use std::sync::Arc;

use bitcoin::psbt::Psbt;
use desktop_app::application::payouts;
use payout_admin_core::test_support::{sign_as, Fixture};
use serde_json::Value;

mod payout_common;
mod payout_tools;
use payout_common::*;

#[tokio::test(flavor = "multi_thread")]
async fn payout_demo_cosign() {
    let (Ok(dir), Ok(txid)) = (
        std::env::var("PAYOUT_DEMO_DIR"),
        std::env::var("PAYOUT_DEMO_TXID"),
    ) else {
        eprintln!("skip: PAYOUT_DEMO_DIR / PAYOUT_DEMO_TXID not set");
        return;
    };
    let demo: Value = serde_json::from_str(
        &std::fs::read_to_string(std::path::Path::new(&dir).join("demo.json")).unwrap(),
    )
    .unwrap();
    let (params, roster) = (
        demo["paramsPath"].as_str().unwrap(),
        demo["rosterPath"].as_str().unwrap(),
    );
    let fx = Arc::new(Fixture::new(3, 2));
    let dep = payouts::load_deployment(params, Some(roster)).unwrap();
    let coordinator = Coordinator::external(demo["orchestratorUrl"].as_str().unwrap());
    let bob = admin_state(&fx, &dep, 1, params, roster, &coordinator).await;
    let ctx = bob.ctx().await.unwrap();
    let p = ctx.coordinator.get_pending(&txid).await.unwrap();
    let mut psbt = Psbt::from_str(&p.psbt).unwrap();
    sign_as(&fx, &dep, &mut psbt, 1);
    let lines = payouts::signatures_text(&dep, &psbt.to_string()).unwrap();
    let bob_key = fx.admin_xonly(1).to_string();
    let mine: Vec<&str> = lines.lines().filter(|l| l.contains(&bob_key)).collect();
    let forged = mine[0].replace(&bob_key, &fx.admin_xonly(2).to_string());
    println!("BEGIN\n{}\n{forged}\nEND", mine.join("\n"));
}
