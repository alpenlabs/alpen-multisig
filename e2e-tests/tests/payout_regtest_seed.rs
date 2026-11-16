//! Manual walletless regtest seeder. See PAYOUT_SEED_* in the tester kit brief.
//! No services or devices are started. Only tool-held bridge/mining keys are stored.
use std::{path::PathBuf, str::FromStr};

use bitcoin::{
    bip32::Xpub,
    secp256k1::{Keypair, Secp256k1, SecretKey},
    Address, Amount, Network, TxOut,
};
use desktop_app::{application::payouts, infrastructure::bitcoin_rpc::HttpBitcoinRpcClient};
use payout_admin_core::{
    deployment::{xonly_at_0_0, SignerRoster},
    report::{FalseClaimReport, ReportVerdict, REPORT_TYPE, REPORT_VERSION},
    test_support::Fixture,
    Deployment,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

mod payout_common;
mod payout_tools;
use payout_common::{acked_game_funded, call, claim_and_contest_funded, n_of_n_keypair};
use payout_tools::*;

#[derive(Serialize, Deserialize)]
struct SeedKeys {
    mining: String,
    operators: Vec<String>,
}

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("Set {name}"))
}
fn number(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .map(|v| {
            v.parse()
                .unwrap_or_else(|_| panic!("{name} must be an unsigned integer"))
        })
        .unwrap_or(default)
}

#[tokio::test(flavor = "multi_thread")]
async fn payout_regtest_seed() {
    let Ok(dir) = std::env::var("PAYOUT_SEED_DIR") else {
        eprintln!("skip: PAYOUT_SEED_DIR not set");
        return;
    };
    let roster_text = std::fs::read_to_string(required("PAYOUT_SEED_ROSTER")).expect("read roster");
    let roster = SignerRoster::from_toml(&roster_text).expect("parse roster");
    assert_eq!(
        roster.network.as_deref(),
        Some("regtest"),
        "roster network must explicitly be regtest"
    );
    let threshold = number(
        "PAYOUT_SEED_THRESHOLD",
        (roster.admins.len() / 2 + 1) as u64,
    ) as usize;
    assert!(
        !roster.admins.is_empty() && roster.admins.len() <= 16,
        "roster needs 1..16 admins"
    );
    assert!(
        threshold > 0 && threshold <= roster.admins.len(),
        "threshold must be 1..admin count"
    );
    let claims = number("PAYOUT_SEED_CLAIMS", 5);
    assert!(claims > 0, "PAYOUT_SEED_CLAIMS must be positive");
    let fund = number("PAYOUT_SEED_FUND_SATS", 100_000_000);
    assert!(
        (990..=4_999_990_000).contains(&fund),
        "funding must cover three non-dust coins and fit one coinbase"
    );
    let url = required("PAYOUT_SEED_RPC_URL");
    let user = required("PAYOUT_SEED_RPC_USER");
    let pass = required("PAYOUT_SEED_RPC_PASS");
    let node = HttpBitcoinRpcClient::new(&url, &user, &pass);
    let chain = call(&node, "getblockchaininfo", json!([])).await;
    assert_eq!(
        chain["chain"], "regtest",
        "seeder refuses a node that is not regtest"
    );
    let indexes = call(&node, "getindexinfo", json!([])).await;
    assert_eq!(
        indexes["txindex"]["synced"], true,
        "Bitcoin Core needs -txindex=1 and a fully synced transaction index"
    );

    let out = PathBuf::from(dir);
    std::fs::create_dir_all(&out).unwrap();
    let out = out.canonicalize().unwrap();
    let key_path = out.join("seed-keys.json");
    let keys: SeedKeys = if key_path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&key_path).unwrap())
            .expect("read seed-keys.json")
    } else {
        assert!(
            !out.join("params.toml").exists(),
            "params.toml exists without seed-keys.json; restore the keys or use a new directory"
        );
        let random_key = || {
            SecretKey::new(&mut bitcoin::secp256k1::rand::thread_rng())
                .display_secret()
                .to_string()
        };
        let keys = SeedKeys {
            mining: random_key(),
            operators: (0..3).map(|_| random_key()).collect(),
        };
        // These are test-only keys, but still avoid a world-readable key file.
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&key_path).unwrap();
        file.write_all(serde_json::to_string_pretty(&keys).unwrap().as_bytes())
            .unwrap();
        keys
    };
    assert_eq!(
        keys.operators.len(),
        3,
        "seed-keys.json must contain three operators"
    );
    // No admin masters are generated or used. Fixture supplies only the bridge params template.
    let fx = Fixture {
        network: Network::Regtest,
        threshold,
        admin_masters: vec![],
        operator_keys: keys
            .operators
            .iter()
            .map(|k| SecretKey::from_str(k).expect("operator key"))
            .collect(),
    };
    let admin_keys = roster
        .admins
        .iter()
        .map(|a| {
            let xpub = Xpub::from_str(&a.xpub).expect("roster account xpub");
            format!("\"{}\"", xonly_at_0_0(&xpub).unwrap())
        })
        .collect::<Vec<_>>()
        .join(", ");
    let params = fx
        .params_toml()
        .replace("pubkeys = []", &format!("pubkeys = [{admin_keys}]"));
    let dep =
        Deployment::load(&params, Some(&roster_text)).expect("validate roster against params");
    let pp = out.join("params.toml");
    if pp.exists() {
        assert_eq!(
            std::fs::read_to_string(&pp).unwrap(),
            params,
            "deployment changed: use the original roster/threshold/keys or a new directory"
        );
    }
    std::fs::write(&pp, &params).unwrap();
    std::fs::write(out.join("roster.toml"), &roster_text).unwrap();
    let mining = Keypair::from_secret_key(
        &Secp256k1::new(),
        &SecretKey::from_str(&keys.mining).expect("mining key"),
    );
    let funding = GameFunding {
        node: &node,
        key: Some(&mining),
    };
    let nn = n_of_n_keypair(&fx, &dep);
    let (old, ack) = acked_game_funded(&funding, &fx, &dep, &nn).await;
    let mut reports = Vec::new();
    for _ in 0..claims {
        let game = claim_and_contest_funded(&funding, &fx, &dep, &nn).await;
        funding.mine(1).await;
        reports.push(FalseClaimReport {
            report_type: Some(REPORT_TYPE.into()),
            version: Some(REPORT_VERSION),
            network: Some("regtest".into()),
            created_at: None,
            operator_pubkey: fx.operator_xonly(0),
            new_claim_txid: game.claim.compute_txid(),
            new_contest_txid: game.contest.compute_txid(),
            acked_claim_txid: old.claim.compute_txid(),
            acked_contest_txid: old.contest.compute_txid(),
            counterproof_ack_txid: ack.compute_txid(),
        });
    }
    let mut funded = Vec::new();
    for admin in dep.signers().unwrap() {
        let address = Address::p2tr(&Secp256k1::new(), admin.xonly, None, Network::Regtest);
        let tx = funding
            .fund(
                (0..3)
                    .map(|i| TxOut {
                        value: Amount::from_sat(fund / 3 + if i == 2 { fund % 3 } else { 0 }),
                        script_pubkey: address.script_pubkey(),
                    })
                    .collect(),
            )
            .await;
        funded.push(json!({"label": admin.label, "address": address.to_string(), "sats": fund, "txid": tx.compute_txid(), "vouts": [0,1,2]}));
    }
    funding.mine(6).await;
    let text = serde_json::to_string_pretty(&reports).unwrap();
    let verified = payouts::verify_reports(&dep, &text, &node, &Default::default())
        .await
        .unwrap();
    assert_eq!(verified.reports.len(), claims as usize);
    for report in &verified.reports {
        assert_eq!(
            report.verified.verdict,
            ReportVerdict::Valid,
            "{:?}",
            report.verified.reasons
        );
    }
    let height = call(&node, "getblockcount", json!([])).await;
    // Keep each successful round as well as the latest reports, so reruns preserve imports.
    std::fs::write(out.join(format!("reports-{height}.json")), &text).unwrap();
    std::fs::write(out.join("reports.json"), text).unwrap();
    let mut transactions = Vec::new();
    for txid in reports
        .iter()
        .flat_map(|r| [r.new_claim_txid, r.new_contest_txid])
        .chain([
            old.claim.compute_txid(),
            old.contest.compute_txid(),
            ack.compute_txid(),
        ])
    {
        let tx = call(&node, "getrawtransaction", json!([txid.to_string(), true])).await;
        let header = call(&node, "getblockheader", json!([tx["blockhash"]])).await;
        transactions.push(
            json!({"txid": txid, "height": header["height"], "confirmations": tx["confirmations"]}),
        );
    }
    let summary = json!({"height": height, "transactions": transactions, "fundedAddresses": funded,
        "paramsPath": pp, "rosterPath": out.join("roster.toml"), "reportsPath": out.join("reports.json"),
        "coordinatorEnv": {"PAYOUT_ADMIN_PARAMS_PATH": pp, "BITCOIN_NETWORK": "regtest", "BITCOIN_RPC_URL": url, "BITCOIN_RPC_USER": user, "BITCOIN_RPC_PASS": pass, "STRATA_ADMIN_STATE_RPC_URL": "http://127.0.0.1:1"}});
    std::fs::write(
        out.join("seed.json"),
        serde_json::to_string_pretty(&summary).unwrap(),
    )
    .unwrap();
    eprintln!(
        "Seeded {claims} valid reports at height {height}; {}",
        out.display()
    );
}
