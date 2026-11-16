//! E2E: Admin Block Payouts with the real Ledger Bitcoin app (Speculos) as admin 0.
//!
//! Skipped unless `LEDGER_SPECULOS_URL` is set (e.g. `http://127.0.0.1:5001`), with Speculos running
//! `app-bitcoin-new` (Bitcoin Testnet) on its default test seed. Admins 1 and 2 are software signers.
//! A "device user" task presses through each review while the app talks to the device.
//!
//! Covers, on the device: identity, BIP-137 sign-in with the params.toml key, policy registration
//! (and the policy address equal to the connector), signing claim inputs and Admin Wallet fee
//! inputs in Confirm, and signing someone else's pending transaction (fee inputs external).

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bitcoin::bip32::{DerivationPath, Xpriv};
use bitcoin::psbt::Psbt;
use bitcoin::{Network, OutPoint, Txid};
use desktop_app::application::payouts::{
    self, ClaimInput, LedgerPayoutDevice, PayoutDevice, PlanRequest,
};
use desktop_app::infrastructure::signing::SignatureResult;
use payout_admin_core::deployment::account_path;
use payout_admin_core::policy::{self, PolicySpec};
use payout_admin_core::test_support::Fixture;
use serde_json::{json, Value};

mod payout_common;
use payout_common::*;

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

#[tokio::test(flavor = "multi_thread")]
async fn e2e_block_payouts_with_speculos_ledger() {
    let Ok(speculos) = std::env::var("LEDGER_SPECULOS_URL") else {
        eprintln!("skip: LEDGER_SPECULOS_URL not set");
        return;
    };
    // Our own presser drives the screens; no automation rules.
    std::env::set_var("LEDGER_SPECULOS_AUTO_APPROVE", "0");

    // Admin 0 = the Speculos seed.
    let mnemonic = bip39::Mnemonic::parse(SPECULOS_SEED).unwrap();
    let seed = mnemonic.to_seed("");
    let mut fx = Fixture::new(3, 2);
    fx.admin_masters[0] = Xpriv::new_master(Network::Regtest, &seed).unwrap();
    let fx = Arc::new(fx);

    let active = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    spawn_device_user(speculos.clone(), active.clone(), seen.clone());
    let ledger: Arc<dyn PayoutDevice> = Arc::new(Watched {
        inner: LedgerPayoutDevice,
        active: active.clone(),
    });

    // The device is the fixture's admin 0.
    let (fp, xpub) = tokio::task::spawn_blocking({
        let l = ledger.clone();
        move || l.identity(&account_path(Network::Regtest))
    })
    .await
    .unwrap()
    .expect("Speculos identity");
    assert_eq!(
        fp,
        payouts::fingerprint_u32(fx.fingerprint(0)),
        "Speculos is not on its default seed"
    );
    let dev_xpub = bitcoin::bip32::Xpub::from_str(&xpub).unwrap();
    assert_eq!(dev_xpub.public_key, fx.account_xpub(0).public_key);

    // Chain + files + coordinator.
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
    let dir = std::env::temp_dir().join(format!("payout-speculos-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (pp, rp) = (dir.join("params.toml"), dir.join("roster.toml"));
    std::fs::write(&pp, fx.params_toml()).unwrap();
    std::fs::write(&rp, fx.roster_toml()).unwrap();
    let (params, roster) = (pp.to_str().unwrap(), rp.to_str().unwrap());
    let dep = payouts::load_deployment(params, Some(roster)).unwrap();
    let coordinator = start_coordinator(params, (&url, &cookie.user, &cookie.password)).await;

    let connector = policy::connector_address(&dep, &dep.covenants[0]);
    let mut claims = Vec::new();
    for _ in 0..3 {
        claims.push(fund_claim(&node, &connector).await);
    }
    for admin in [0, 1] {
        call(
            &node,
            "sendtoaddress",
            json!([wallet_address(&fx, admin, 0).to_string(), 0.0005]),
        )
        .await;
    }
    mine(&node, 1).await;
    let claim = |t: Txid| ClaimInput {
        outpoint: OutPoint::new(t, 1).to_string(),
        covenant_index: 0,
        payout_height: None,
    };

    // ── Admin 0 on the Ledger: sign in, then Confirm (register policy, sign claims and fees).
    let a0 = signed_in_state(ledger.clone(), 0, params, roster, &coordinator).await;
    let ctx0 = a0.ctx().await.unwrap();
    let req = PlanRequest {
        claims: vec![claim(claims[0]), claim(claims[1])],
        groups: None,
        fee_rate: "3".into(),
    };
    let (plan, _, _) = payouts::plan_for(&ctx0, &req, &node, true).await.unwrap();
    assert!(plan.ok, "{:?}", plan.txs[0].errors);
    // What the app tells the admin to expect, taken before the device is touched.
    let review = payouts::review_confirm(&ctx0, &req, &a0.policies, &node)
        .await
        .expect("review of Confirm");
    let res = payouts::confirm_request(&ctx0, &req, plan.total_fee_sat, &a0.policies, &node)
        .await
        .expect("confirm on the Ledger");
    assert_eq!(stored_signature_counts(&ctx0, &res[0].txid).await, (1, 2));
    let txid1 = res[0].txid.clone();

    // Admin 1 (software) completes it: Core accepts the Ledger's leaf and key-path signatures.
    let a1 = admin_state(&fx, &dep, 1, params, roster, &coordinator).await;
    let ctx1 = a1.ctx().await.unwrap();
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
    .unwrap();
    let b = r.broadcast.expect("broadcast");
    assert!(b.broadcast, "Core rejected: {:?}", b.error);

    // ── Admin 1 creates a payout; the Ledger signs it as second signer (fee inputs are not its own).
    let req2 = PlanRequest {
        claims: vec![claim(claims[2])],
        groups: None,
        fee_rate: "1".into(),
    };
    let (plan2, _, _) = payouts::plan_for(&ctx1, &req2, &node, true).await.unwrap();
    let res2 = payouts::confirm_request(&ctx1, &req2, plan2.total_fee_sat, &a1.policies, &node)
        .await
        .unwrap();
    let r = payouts::sign_pending(
        &ctx0.dep,
        &ctx0.me,
        &res2[0].txid,
        ctx0.device.clone(),
        &a0.policies,
        &ctx0.coordinator,
        &node,
    )
    .await
    .expect("Ledger signs a pending payout");
    let b = r.broadcast.expect("broadcast");
    assert!(b.broadcast, "Core rejected: {:?}", b.error);
    mine(&node, 1).await;
    let ov = payouts::overview(&ctx0.dep, Some(0), &ctx0.coordinator, &node)
        .await
        .unwrap();
    assert_eq!(ov.past.len(), 2);
    assert!(ov.past.iter().all(|p| p.status == "confirmed"));

    eprintln!("screens the device showed:");
    for s in seen.lock().unwrap().iter() {
        eprintln!("  {s}");
    }

    // The checklist must match the device: every value it lists is on a screen the device showed
    // (registration, then the claim and fee passes of Confirm), as the device words and wraps it.
    let flat = |t: &str| -> String {
        t.chars()
            .filter(|c| !c.is_whitespace() && *c != '|')
            .collect()
    };
    // Long values span pages ("Our key@1 (2/3) | …"): drop each page label so the value reads on.
    let unpage = |line: &String| -> String {
        let paged = |head: &str| {
            head.strip_suffix(')')
                .and_then(|h| h.rsplit_once('('))
                .is_some_and(|(_, n)| {
                    n.split_once('/').is_some_and(|(a, b)| {
                        !a.is_empty()
                            && !b.is_empty()
                            && (a.chars().chain(b.chars())).all(|c| c.is_ascii_digit())
                    })
                })
        };
        match line.split_once(" | ") {
            Some((head, rest)) if paged(head.trim_end()) => rest.to_string(),
            _ => line.clone(),
        }
    };
    let screens = flat(
        &seen
            .lock()
            .unwrap()
            .iter()
            .map(unpage)
            .collect::<Vec<_>>()
            .join(" "),
    );
    let device_amount = |sat: u64| {
        let s = format!("{:.8}", sat as f64 / 1e8);
        format!("{}TEST", s.trim_end_matches('0').trim_end_matches('.'))
    };
    // The emulator renders some 2.4.x values blank (the real device shows them): a screen that is
    // only the label then counts for that label.
    let blank: Vec<String> = seen.lock().unwrap().clone();
    assert_eq!(review.steps.len(), 3, "registration, claim pass, fee pass");
    for step in &review.steps {
        for d in &step.details {
            let expected = match d.sat {
                _ if blank.iter().any(|s| s == &d.label) => flat(&d.label),
                Some(sat) => flat(&format!("{}{}", d.label, device_amount(sat))),
                None => flat(&d.value),
            };
            assert!(
                screens.contains(&expected),
                "the checklist says `{}` ({}) but the device never showed it",
                d.label,
                step.title
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A plain send from the Admin Wallet and its fee bump, signed on the device: the checklist the
/// dialog shows must name what the device shows.
#[tokio::test(flavor = "multi_thread")]
async fn e2e_admin_wallet_send_with_speculos_ledger() {
    use desktop_app::application::payouts::{WalletBumpRequest, WalletSendRequest};
    let Ok(speculos) = std::env::var("LEDGER_SPECULOS_URL") else {
        eprintln!("skip: LEDGER_SPECULOS_URL not set");
        return;
    };
    std::env::set_var("LEDGER_SPECULOS_AUTO_APPROVE", "0");
    let mnemonic = bip39::Mnemonic::parse(SPECULOS_SEED).unwrap();
    let mut fx = Fixture::new(3, 2);
    fx.admin_masters[0] = Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();
    let fx = Arc::new(fx);

    let active = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    spawn_device_user(speculos.clone(), active.clone(), seen.clone());
    let ledger: Arc<dyn PayoutDevice> = Arc::new(Watched {
        inner: LedgerPayoutDevice,
        active: active.clone(),
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
    let dir = std::env::temp_dir().join(format!("payout-speculos-send-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (pp, rp) = (dir.join("params.toml"), dir.join("roster.toml"));
    std::fs::write(&pp, fx.params_toml()).unwrap();
    std::fs::write(&rp, fx.roster_toml()).unwrap();
    let (params, roster) = (pp.to_str().unwrap(), rp.to_str().unwrap());
    let coordinator = start_coordinator(params, (&url, &cookie.user, &cookie.password)).await;
    call(
        &node,
        "sendtoaddress",
        json!([wallet_address(&fx, 0, 0).to_string(), 0.001]),
    )
    .await;
    mine(&node, 1).await;

    let a0 = signed_in_state(ledger.clone(), 0, params, roster, &coordinator).await;
    let ctx = a0.ctx().await.unwrap();
    seen.lock().unwrap().clear();

    let flat = |t: &str| -> String {
        t.chars()
            .filter(|c| !c.is_whitespace() && *c != '|')
            .collect()
    };
    let device_amount = |sat: u64| {
        let s = format!("{:.8}", sat as f64 / 1e8);
        format!("{}TEST", s.trim_end_matches('0').trim_end_matches('.'))
    };
    // Every value of the checklist is on a screen the device showed since the last check.
    let check = |what: &str, review: &payouts::LedgerReviewDto| {
        let shown: Vec<String> = std::mem::take(&mut *seen.lock().unwrap());
        eprintln!("{what}: the checklist");
        for step in &review.steps {
            eprintln!("  step: {} (warning: {:?})", step.title, step.warning);
            for d in &step.details {
                eprintln!("    {} = {} ({:?})", d.label, d.value, d.sat);
            }
        }
        eprintln!("{what}: the device showed");
        for s in &shown {
            eprintln!("  {s}");
        }
        // Long values span pages ("To (1/2) | …"): drop each page label so the value reads on.
        let unpage = |line: &String| match line.split_once(" | ") {
            Some((head, rest)) if head.ends_with("/2)") => rest.to_string(),
            _ => line.clone(),
        };
        let screens = flat(&shown.iter().map(unpage).collect::<Vec<_>>().join(" "));
        // The emulator renders 2.4.x values blank (the real device shows them): labels only then.
        let labels_only = shown.iter().any(|s| s == "Amount");
        for step in &review.steps {
            for d in &step.details {
                let expected = match d.sat {
                    _ if labels_only => flat(&d.label),
                    Some(sat) => flat(&format!("{}{}", d.label, device_amount(sat))),
                    None => flat(&d.value),
                };
                assert!(
                    screens.contains(&expected),
                    "{what}: the checklist says `{}` = `{}` but the device never showed it",
                    d.label,
                    d.value
                );
            }
        }
        let titles: Vec<&str> = shown
            .iter()
            .filter_map(|s| s.split(" | ").next())
            .map(|t| t.split(" (").next().unwrap_or(t))
            .filter(|t| ["Amount", "To", "Fees", "You spend", "From"].contains(t))
            .collect();
        let mut expected: Vec<&str> = review.steps[0]
            .details
            .iter()
            .map(|d| d.label.as_str())
            .collect();
        expected.dedup();
        let mut titles_once = titles.clone();
        titles_once.dedup();
        assert_eq!(titles_once, expected, "{what}: the device's value screens");
        assert!(
            !screens.to_lowercase().contains("securityrisk")
                && !screens.to_lowercase().contains("register"),
            "{what}: a plain send shows no warning and no registration"
        );
    };

    let mut req = WalletSendRequest {
        address: wallet_address(&fx, 1, 0).to_string(),
        amount_sat: Some(20_000),
        max: false,
        fee_rate: "2.0".into(),
        expected_txid: None,
    };
    let review = payouts::wallet_prepare(&ctx, &payouts::WalletIntent::Send(req.clone()), &node)
        .await
        .expect("review of the send");
    seen.lock().unwrap().clear();
    req.expected_txid = Some(review.txid.clone());
    let sent = payouts::wallet_execute(
        &ctx,
        &payouts::WalletIntent::Send(req.clone()),
        req.expected_txid.as_deref().unwrap_or(""),
        &node,
    )
    .await
    .expect("send signed on the Ledger");
    check("send", &review.review);

    let mut bump = WalletBumpRequest {
        txid: sent.txid,
        fee_rate: "5.0".into(),
        expected_txid: None,
    };
    let replacement =
        payouts::wallet_prepare(&ctx, &payouts::WalletIntent::Bump(bump.clone()), &node)
            .await
            .expect("review of the bump");
    seen.lock().unwrap().clear();
    bump.expected_txid = Some(replacement.txid.clone());
    payouts::wallet_execute(
        &ctx,
        &payouts::WalletIntent::Bump(bump.clone()),
        bump.expected_txid.as_deref().unwrap_or(""),
        &node,
    )
    .await
    .expect("bump signed on the Ledger");
    check("bump", &replacement.review);

    mine(&node, 1).await;
    let recipient = call(&node, "gettxout", json!([replacement.txid, 0])).await;
    assert_eq!(recipient["value"].as_f64().unwrap(), 0.0002);
    let _ = std::fs::remove_dir_all(&dir);
}
