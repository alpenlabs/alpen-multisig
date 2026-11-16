//! Manual walkthrough stack for Admin Block Payouts (not a CI test).
//!
//! Runs only when `PAYOUT_DEMO_DIR` is set. It starts Bitcoin Core (regtest, txindex) and the real
//! coordinator, then puts on chain everything the screen needs, with consensus-valid transactions:
//!
//! - one finished "acked" game: claim → contest (N/N + watchtower leaf) → counterproof → ACK, the
//!   ACK spending the counterproof through its N/N timeout leaf, as the bridge's does;
//! - five "new" claims (A–E), each contested, so five false claim reports verify as valid (a report
//!   names the new claim and contest and the acked claim, contest and ACK, not the counterproof);
//! - payout E is created and completed by admins 1 and 2 (software signers) and mined: Past, and
//!   report E shows as already spent;
//! - payout D is created by admin 1 (1 of 2): Pending, for the Ledger admin to sign;
//! - Admin Wallet coins for admin 0 (the Speculos seed) and admin 1;
//! - with `PAYOUT_DEMO_SETS=4,90`, 4 and 90 more claims with `reports-4.json` and `reports-90.json`.
//!
//! It writes `demo.json` (RPC, coordinator URL, file paths) and `reports.json` to the directory,
//! then keeps the stack up until a file named `stop` appears there. Admin 0 is the Speculos
//! default seed, so the desktop app with `LEDGER_SPECULOS_URL` set is that admin.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use bitcoin::bip32::Xpriv;
use bitcoin::secp256k1::Secp256k1;
use bitcoin::{Address, Network, OutPoint};
use desktop_app::application::payouts::{self, ClaimInput, PlanRequest};
use desktop_app::infrastructure::bitcoin_rpc::HttpBitcoinRpcClient;
use payout_admin_core::report::FalseClaimReport;
use payout_admin_core::test_support::Fixture;
use serde_json::{json, Value};

mod payout_common;
use payout_common::*;

const SPECULOS_SEED: &str =
    "glory promote mansion idle axis finger extra february uncover one trip resource lawn \
     turtle enact monster seven myth punch hobby comfort wild raise skin";

#[tokio::test(flavor = "multi_thread")]
async fn payout_demo_stack() {
    let Ok(out) = std::env::var("PAYOUT_DEMO_DIR") else {
        eprintln!("skip: PAYOUT_DEMO_DIR not set");
        return;
    };
    let out = PathBuf::from(out);
    std::fs::create_dir_all(&out).unwrap();
    let _ = std::fs::remove_file(out.join("stop"));

    let mnemonic = bip39::Mnemonic::parse(SPECULOS_SEED).unwrap();
    let mut fx = Fixture::new(3, 2);
    fx.admin_masters[0] = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
    let fx = Arc::new(fx);

    // With PAYOUT_DEMO_REAL_LEDGER set, admin 0 is the connected Ledger (Bitcoin Test app open)
    // instead of the Speculos seed. Admin 0 never signs during setup, so its public identity is
    // enough: it goes into params.toml, the roster and the Admin Wallet funding addresses.
    let real_ledger = std::env::var("PAYOUT_DEMO_REAL_LEDGER").is_ok().then(|| {
        use payouts::PayoutDevice as _;
        // The device call runs its own runtime, so it cannot run on this async test's thread.
        // Waits for a Ledger that is locked or has no app open, up to 15 minutes.
        let (fp, xpub) = std::thread::spawn(|| {
            let path = payout_admin_core::deployment::account_path(Network::Regtest);
            let mut last = String::new();
            for _ in 0..450 {
                match payouts::LedgerPayoutDevice.identity(&path) {
                    Ok(id) => return Ok(id),
                    Err(e) => {
                        if e != last {
                            eprintln!("waiting for the Ledger: {e}");
                        }
                        last = e;
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(last)
        })
        .join()
        .unwrap()
        .expect("Ledger connected and unlocked, with the Bitcoin Test app open");
        let fp = bitcoin::bip32::Fingerprint::from(fp.to_le_bytes());
        let xpub = bitcoin::bip32::Xpub::from_str(&xpub).unwrap();
        eprintln!("admin 0 is the Ledger {fp}, {xpub}");
        (fp, xpub)
    });
    let admin0_branch_address = |branch: u32, index: u32| -> Option<Address> {
        let (_, xpub) = real_ledger.as_ref()?;
        let secp = Secp256k1::new();
        let k = xpub
            .derive_pub(
                &secp,
                &[
                    bitcoin::bip32::ChildNumber::Normal { index: branch },
                    bitcoin::bip32::ChildNumber::Normal { index },
                ],
            )
            .unwrap();
        Some(Address::p2tr(
            &secp,
            k.public_key.x_only_public_key().0,
            None,
            Network::Regtest,
        ))
    };

    let mut conf = corepc_node::Conf::default();
    conf.args.push("-txindex");
    let bitcoind = corepc_node::Node::with_conf("bitcoind", &conf).expect("bitcoind on PATH");
    let cookie = bitcoind.params.get_cookie_values().unwrap().unwrap();
    let url = bitcoind.rpc_url();
    let node = HttpBitcoinRpcClient::new(&url, &cookie.user, &cookie.password);
    mine(&node, 110).await;

    let (pp, rp) = (out.join("params.toml"), out.join("signers.toml"));
    let (mut params_toml, mut roster_toml) = (fx.params_toml(), fx.roster_toml());
    if let Some((fp, xpub)) = &real_ledger {
        // The admin key is account/0/0, the same key as receive address 0.
        let ledger_xonly = xpub
            .derive_pub(
                &Secp256k1::new(),
                &[
                    bitcoin::bip32::ChildNumber::Normal { index: 0 },
                    bitcoin::bip32::ChildNumber::Normal { index: 0 },
                ],
            )
            .unwrap()
            .public_key
            .x_only_public_key()
            .0;
        params_toml =
            params_toml.replace(&fx.admin_xonly(0).to_string(), &ledger_xonly.to_string());
        roster_toml = roster_toml
            .replace(&fx.fingerprint(0).to_string(), &fp.to_string())
            .replace(&fx.account_xpub(0).to_string(), &xpub.to_string());
    }
    std::fs::write(&pp, params_toml).unwrap();
    let roster = roster_toml
        .replace("Admin 0", "Alice (Ledger)")
        .replace("Admin 1", "Bob")
        .replace("Admin 2", "Carol");
    std::fs::write(&rp, roster).unwrap();
    let (params, roster) = (pp.to_str().unwrap(), rp.to_str().unwrap());
    let dep = payouts::load_deployment(params, Some(roster)).unwrap();
    let coordinator = start_coordinator(params, (&url, &cookie.user, &cookie.password)).await;
    let nn = n_of_n_keypair(&fx, &dep);

    // The old game, finished with an ACK.
    let (old, ack) = acked_game(&node, &fx, &dep, &nn).await;
    let ct = old.contest.compute_txid();

    // Five new claims, contested.
    let mut games = Vec::new();
    for _ in 0..5 {
        games.push(claim_and_contest(&node, &fx, &dep, &nn).await);
        mine(&node, 1).await;
    }
    let report = |g: &Game, op: usize| FalseClaimReport {
        report_type: Some(payout_admin_core::report::REPORT_TYPE.into()),
        version: Some(payout_admin_core::report::REPORT_VERSION),
        network: Some("regtest".into()),
        created_at: Some("2026-11-11T12:00:00Z".into()),
        operator_pubkey: fx.operator_xonly(op),
        new_claim_txid: g.claim.compute_txid(),
        new_contest_txid: g.contest.compute_txid(),
        acked_claim_txid: old.claim.compute_txid(),
        acked_contest_txid: ct,
        counterproof_ack_txid: ack.compute_txid(),
    };

    // `PAYOUT_DEMO_SETS=4,90`: for each size, that many more contested claims, with their reports in
    // `reports-<size>.json`, to paste as one payout of that size; and one more fee coin each.
    let sets: Vec<usize> = std::env::var("PAYOUT_DEMO_SETS")
        .map(|v| {
            v.split(',')
                .map(|n| n.trim().parse().expect("PAYOUT_DEMO_SETS: sizes"))
                .collect()
        })
        .unwrap_or_default();
    for size in &sets {
        let mut set = Vec::new();
        for _ in 0..*size {
            let g = claim_and_contest(&node, &fx, &dep, &nn).await;
            mine(&node, 1).await;
            set.push(serde_json::to_value(report(&g, 0)).unwrap());
        }
        std::fs::write(
            out.join(format!("reports-{size}.json")),
            serde_json::to_string_pretty(&set).unwrap(),
        )
        .unwrap();
    }

    // Admin Wallet coins.
    let mut wallet_coins = vec![(0, 0, 0.0004), (0, 1, 0.0006), (1, 0, 0.001), (2, 0, 0.001)];
    wallet_coins.extend((0..sets.len() as u32).map(|i| (0, 2 + i, 0.001)));
    for (admin, idx, amt) in wallet_coins {
        call(
            &node,
            "sendtoaddress",
            json!([
                admin0_branch_address(0, idx)
                    .filter(|_| admin == 0)
                    .unwrap_or_else(|| wallet_address(&fx, admin, idx))
                    .to_string(),
                amt
            ]),
        )
        .await;
    }
    mine(&node, 1).await;

    let claim = |g: &Game| ClaimInput {
        outpoint: OutPoint::new(g.claim.compute_txid(), 1).to_string(),
        covenant_index: 0,
        payout_height: None,
    };
    let bob = admin_state(&fx, &dep, 1, params, roster, &coordinator).await;
    let carol = admin_state(&fx, &dep, 2, params, roster, &coordinator).await;
    let (cb, cc) = (bob.ctx().await.unwrap(), carol.ctx().await.unwrap());

    // E: created by Bob, completed by Carol, mined → Past, and its report shows as spent.
    let req = PlanRequest {
        claims: vec![claim(&games[4])],
        groups: None,
        fee_rate: "2".into(),
    };
    let (plan, _, _) = payouts::plan_for(&cb, &req, &node, true).await.unwrap();
    let r = payouts::confirm_request(&cb, &req, plan.total_fee_sat, &bob.policies, &node)
        .await
        .unwrap();
    let s = payouts::sign_pending(
        &cc.dep,
        &cc.me,
        &r[0].txid,
        cc.device.clone(),
        &carol.policies,
        &cc.coordinator,
        &node,
    )
    .await
    .unwrap();
    assert!(s.broadcast.unwrap().broadcast);
    mine(&node, 1).await;

    // D: created by Bob, 1 of 2: Pending for Alice (Ledger).
    let req = PlanRequest {
        claims: vec![claim(&games[3])],
        groups: None,
        fee_rate: "1.5".into(),
    };
    let (plan, _, _) = payouts::plan_for(&cb, &req, &node, true).await.unwrap();
    let pending_d = payouts::confirm_request(&cb, &req, plan.total_fee_sat, &bob.policies, &node)
        .await
        .unwrap();

    // Reports: A, B, C valid; D in Pending; E spent; one naming the wrong operator (invalid).
    let mut reports: Vec<Value> = games
        .iter()
        .map(|g| serde_json::to_value(report(g, 0)).unwrap())
        .collect();
    reports.push(serde_json::to_value(report(&games[0], 1)).unwrap());
    let v = payouts::verify_reports(
        &dep,
        &serde_json::to_string_pretty(&reports).unwrap(),
        &node,
        &Default::default(),
    )
    .await
    .unwrap();
    for r in &v.reports {
        eprintln!(
            "report {}: {:?} {:?}",
            r.verified.outpoint, r.verified.verdict, r.verified.reasons
        );
    }
    std::fs::write(
        out.join("reports.json"),
        serde_json::to_string_pretty(&reports).unwrap(),
    )
    .unwrap();
    std::fs::write(
        out.join("demo.json"),
        serde_json::to_string_pretty(&json!({
            "btcRpcUrl": url,
            "btcRpcUser": cookie.user,
            "btcRpcPass": cookie.password,
            "orchestratorUrl": coordinator.base_url,
            "paramsPath": params,
            "rosterPath": roster,
            "pendingD": pending_d[0].txid,
            "claims": games.iter().map(|g| g.claim.compute_txid().to_string()).collect::<Vec<_>>(),
            "bitcoindDatadir": bitcoind.workdir(),
        }))
        .unwrap(),
    )
    .unwrap();
    eprintln!("demo stack up; touch {}/stop to end", out.display());

    // Keep the stack up. Touch `mine` to mine one block.
    while !out.join("stop").exists() {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if out.join("mine").exists() {
            let _ = std::fs::remove_file(out.join("mine"));
            mine(&node, 1).await;
        }
    }
    drop(coordinator);
}
