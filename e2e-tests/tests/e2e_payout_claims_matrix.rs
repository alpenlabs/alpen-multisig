//! Claim-count matrix on Speculos: one Confirm with `CLAIM_COUNT` claims in a single payout,
//! signed by the real Ledger Bitcoin app (admin 0, the Speculos default seed). It never fails on a
//! device error: it prints one `MATRIX_RESULT {json}` line with the outcome, the error text, the
//! plan shape and the last screens the device showed, for `.local/bin/claims-matrix` to collect
//! per device model and app version. A multi-claim signing once failed on a physical Nano X
//! (Bitcoin Test 2.4.6: `0x6a80`, the app exited before any review screen).
//!
//! Skipped unless `CLAIM_COUNT` and either `LEDGER_SPECULOS_URL` or `LEDGER_PHYSICAL=1` are set (a
//! physical Ledger is approved by hand, and no screens are recorded; `LEDGER_REVIEW_FILE` then gets
//! the app's checklists, each written before the device shows its screens). Not a CI test.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bitcoin::bip32::{DerivationPath, Xpriv};
use bitcoin::psbt::Psbt;
use bitcoin::{Network, OutPoint};
use desktop_app::application::payouts::{
    self, ClaimInput, LedgerPayoutDevice, PayoutDevice, PlanRequest,
};
use desktop_app::infrastructure::signing::SignatureResult;
use payout_admin_core::policy::{self, PolicySpec};
use payout_admin_core::test_support::Fixture;
use serde_json::{json, Value};

mod payout_common;
mod payout_tools;
use payout_common::*;
use payout_tools::*;

const SPECULOS_SEED: &str =
    "glory promote mansion idle axis finger extra february uncover one trip resource lawn \
                             turtle enact monster seven myth punch hobby comfort wild raise skin";

/// The Ledger, with a flag that tells the device user a request is in flight.
struct Watched {
    inner: LedgerPayoutDevice,
    active: Arc<AtomicBool>,
}

impl Watched {
    fn run<T>(&self, f: impl FnOnce(&LedgerPayoutDevice) -> T) -> T {
        self.active.store(true, Ordering::SeqCst);
        let r = f(&self.inner);
        self.active.store(false, Ordering::SeqCst);
        r
    }
}

impl PayoutDevice for Watched {
    fn app(&self) -> Result<(String, String), String> {
        self.run(|d| d.app())
    }
    fn identity(&self, a: &DerivationPath) -> Result<(u32, String), String> {
        self.run(|d| d.identity(a))
    }
    fn show_address(&self, p: &PolicySpec, index: u32) -> Result<String, String> {
        self.run(|d| d.show_address(p, index))
    }
    fn register_policy(&self, p: &PolicySpec, fp: u32) -> Result<[u8; 32], String> {
        self.run(|d| d.register_policy(p, fp))
    }
    fn policy_address(&self, p: &PolicySpec, h: [u8; 32]) -> Result<String, String> {
        self.run(|d| d.policy_address(p, h))
    }
    fn sign_psbt(
        &self,
        psbt: &mut Psbt,
        p: &PolicySpec,
        h: Option<[u8; 32]>,
        fp: u32,
    ) -> Result<usize, String> {
        self.run(|d| d.sign_psbt(psbt, p, h, fp))
    }
    fn sign_message(
        &self,
        m: &str,
        path: &DerivationPath,
        fp: u32,
    ) -> Result<SignatureResult, String> {
        self.run(|d| d.sign_message(m, path, fp))
    }
}

/// Presses through every review screen while a request is in flight.
fn spawn_device_user(
    url: String,
    active: Arc<AtomicBool>,
    seen: Arc<std::sync::Mutex<Vec<String>>>,
) {
    tokio::spawn(async move {
        let http = reqwest::Client::new();
        let approve = |t: &str| {
            let t = t.to_ascii_lowercase();
            [
                "approve",
                "accept",
                "sign",
                "confirm",
                "continue",
                "register",
                "hold to sign",
            ]
            .iter()
            .any(|w| t == *w || t.starts_with(&format!("{w} ")))
        };
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if !active.load(Ordering::SeqCst) {
                continue;
            }
            let Ok(r) = http
                .get(format!("{url}/events?currentscreenonly=true"))
                .send()
                .await
            else {
                continue;
            };
            let Ok(v) = r.json::<Value>().await else {
                continue;
            };
            let texts: Vec<String> = v["events"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|e| e["text"].as_str().map(|s| s.trim().to_string()))
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            if texts.is_empty() || texts.iter().any(|t| t == "app is ready") {
                continue;
            }
            let line = texts.join(" | ");
            {
                let mut s = seen.lock().unwrap();
                if s.last() != Some(&line) {
                    s.push(line);
                }
            }
            // Single-line screen titles that end a review: both buttons.
            let button = if texts.iter().any(|t| approve(t))
                && texts.len() <= 2
                && !texts[0].starts_with("Review")
            {
                "both"
            } else {
                "right"
            };
            let _ = http
                .post(format!("{url}/button/{button}"))
                .json(&json!({ "action": "press-and-release" }))
                .send()
                .await;
        }
    });
}

/// The checklist as plain text: one numbered step per device review, one line per screen.
fn review_text(review: &payouts::LedgerReviewDto) -> String {
    let mut out = String::new();
    if let Some(note) = &review.note {
        out.push_str(&format!("Note: {note}\n\n"));
    }
    for (i, step) in review.steps.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", i + 1, step.title));
        if let Some(w) = &step.warning {
            out.push_str(&format!("   Warning: {w}\n"));
        }
        for d in &step.details {
            // Amounts as the device prints them: bitcoin, without trailing zeros.
            let value = match d.sat {
                Some(sat) => {
                    let btc = format!("{}.{:08}", sat / 100_000_000, sat % 100_000_000);
                    let btc = btc.trim_end_matches('0').trim_end_matches('.');
                    format!("{btc} TEST ({sat} sat)")
                }
                None => d.value.clone(),
            };
            out.push_str(&format!("   {}: {value}", d.label));
            if let Some(n) = &d.note {
                out.push_str(&format!("  ({n})"));
            }
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn claims_matrix_run() {
    // `LEDGER_PHYSICAL=1` instead of `LEDGER_SPECULOS_URL`: the Ledger on USB, approved by hand. It
    // keeps its own keys; the deployment is built around its public identity.
    let speculos = std::env::var("LEDGER_SPECULOS_URL").ok();
    let physical = std::env::var("LEDGER_PHYSICAL").is_ok_and(|v| v == "1");
    let Ok(count) = std::env::var("CLAIM_COUNT") else {
        eprintln!("skip: CLAIM_COUNT not set");
        return;
    };
    if speculos.is_none() && !physical {
        eprintln!("skip: set LEDGER_SPECULOS_URL or LEDGER_PHYSICAL=1");
        return;
    }
    let count: usize = count.parse().expect("CLAIM_COUNT is a number");
    // 1 (default), 2 or 3 covenants: the operator schedule changes the covenant set, and the
    // claims are spread over the covenants round-robin.
    let n_cov: usize = std::env::var("COVENANTS")
        .ok()
        .map_or(1, |v| v.parse().expect("COVENANTS is a number"));
    // Our own presser drives the screens; no automation rules.
    std::env::set_var("LEDGER_SPECULOS_AUTO_APPROVE", "0");

    let mnemonic = bip39::Mnemonic::parse(SPECULOS_SEED).unwrap();
    let mut fx = Fixture::new(3, 2);
    fx.admin_masters[0] = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
    let fx = Arc::new(fx);

    let active = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    if let Some(url) = speculos {
        spawn_device_user(url, active.clone(), seen.clone());
    }
    let ledger: Arc<dyn PayoutDevice> = Arc::new(Watched {
        inner: LedgerPayoutDevice,
        active,
    });

    let mut conf = corepc_node::Conf::default();
    conf.args.push("-txindex");
    let bitcoind = corepc_node::Node::with_conf("bitcoind", &conf).expect("bitcoind on PATH");
    let cookie = bitcoind.params.get_cookie_values().unwrap().unwrap();
    let url = bitcoind.rpc_url();
    let node = desktop_app::infrastructure::bitcoin_rpc::HttpBitcoinRpcClient::new(
        &url,
        &cookie.user,
        &cookie.password,
    );
    mine(&node, 110).await;
    let dir = std::env::temp_dir().join(format!("payout-matrix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (pp, rp) = (dir.join("params.toml"), dir.join("roster.toml"));
    let params_toml = match n_cov {
        1 => fx.params_toml(),
        2 => fx.params_toml_with_schedule(&[(0, 101, None), (1, 101, None), (2, 200, None)]),
        3 => fx.params_toml_with_schedule(&[(0, 101, None), (1, 101, Some(300)), (2, 200, None)]),
        n => panic!("COVENANTS must be 1, 2 or 3, not {n}"),
    };
    // A physical Ledger keeps its own keys: admin 0 becomes that device.
    let device = physical.then(physical_ledger_identity);
    let (params_toml, roster_toml) = match &device {
        Some((fp, xpub)) => with_device_as_admin0(&fx, &params_toml, &fx.roster_toml(), *fp, xpub),
        None => (params_toml, fx.roster_toml()),
    };
    std::fs::write(&pp, params_toml).unwrap();
    std::fs::write(&rp, roster_toml).unwrap();
    let (params, roster) = (pp.to_str().unwrap(), rp.to_str().unwrap());
    let dep = payouts::load_deployment(params, Some(roster)).unwrap();
    let coordinator = start_coordinator(params, (&url, &cookie.user, &cookie.password)).await;

    assert_eq!(dep.covenants.len(), n_cov, "covenants from the schedule");
    let mut claims = Vec::new();
    for i in 0..count {
        let connector = policy::connector_address(&dep, &dep.covenants[i % n_cov]);
        claims.push(fund_claim(&node, &connector).await);
        // Core refuses more than 25 unconfirmed ancestors in the funding wallet's chain.
        if i % 15 == 14 {
            mine(&node, 1).await;
        }
    }
    // One fee coin is plenty for a single payout.
    let fee_address = match &device {
        Some((_, xpub)) => xpub_address(xpub, 0, 0),
        None => wallet_address(&fx, 0, 0),
    };
    call(
        &node,
        "sendtoaddress",
        json!([fee_address.to_string(), 0.01]),
    )
    .await;
    mine(&node, 1).await;

    // By hand, the device user needs the app's checklists to compare the screens with:
    // `LEDGER_REVIEW_FILE` gets the sign-in one before the sign-in, then the payout one.
    let review_file = std::env::var("LEDGER_REVIEW_FILE").ok();
    let a0 = match &review_file {
        Some(file) => {
            let state = payouts::PayoutState::new(
                ledger.clone(),
                desktop_app::infrastructure::payout_policy_store::PolicyStore::new(None),
            );
            state
                .load(payouts::load_deployment(params, Some(roster)).unwrap())
                .await;
            let review = payouts::sign_in_review(&state, &coordinator.base_url)
                .await
                .expect("sign-in review");
            std::fs::write(file, format!("SIGN IN\n\n{}", review_text(&review))).unwrap();
            payouts::sign_in(&state, &coordinator.base_url)
                .await
                .expect("sign in");
            state
        }
        None => signed_in_state(ledger.clone(), 0, params, roster, &coordinator).await,
    };
    let ctx0 = a0.ctx().await.unwrap();
    let req = PlanRequest {
        claims: claims
            .iter()
            .enumerate()
            .map(|(i, t)| ClaimInput {
                outpoint: OutPoint::new(*t, 1).to_string(),
                covenant_index: i % n_cov,
                payout_height: None,
            })
            .collect(),
        groups: None,
        fee_rate: "2".into(),
    };
    let (plan, _, _) = payouts::plan_for(&ctx0, &req, &node, true).await.unwrap();
    let shape: Vec<Value> = plan
        .txs
        .iter()
        .map(|t| json!({ "claims": t.claims.len(), "vsize": t.vsize, "feeSat": t.fee_sat }))
        .collect();
    if let Some(file) = &review_file {
        let review = payouts::review_confirm(&ctx0, &req, &a0.policies, &node)
            .await
            .expect("review");
        let sign_in = std::fs::read_to_string(file).unwrap();
        std::fs::write(file, format!("{sign_in}PAYOUT\n\n{}", review_text(&review))).unwrap();
    }
    // Seen before the signing, so only the signing screens are counted below.
    let before = seen.lock().unwrap().len();
    let outcome = if plan.ok {
        payouts::confirm_request(&ctx0, &req, plan.total_fee_sat, &a0.policies, &node)
            .await
            .map(|r| r.len())
    } else {
        Err(format!(
            "plan not ok: {:?}",
            plan.txs.iter().flat_map(|t| &t.errors).collect::<Vec<_>>()
        ))
    };
    let screens = seen.lock().unwrap().clone();
    // `MATRIX_SCREENS_FILE`: every screen the emulator showed while signing, one per line, to
    // compare with the checklist in `LEDGER_REVIEW_FILE`.
    if let Ok(file) = std::env::var("MATRIX_SCREENS_FILE") {
        std::fs::write(&file, screens[before..].join("\n")).unwrap();
    }
    let tail: Vec<&String> = screens.iter().skip(before).rev().take(6).rev().collect();
    println!(
        "MATRIX_RESULT {}",
        json!({
            "claims": count,
            "covenants": n_cov,
            "ok": outcome.is_ok(),
            "transactions": outcome.as_ref().ok(),
            "error": outcome.as_ref().err(),
            "plan": shape,
            "screensDuringSigning": screens.len() - before,
            "lastScreens": tail,
        })
    );
    let _ = std::fs::remove_dir_all(&dir);
}
