//! Persistent regtest session; NEVER accesses HID. Run from the repository root.
//! Preparation (Colima must already be running):
//! `export DOCKER_CONTEXT=colima; docker build -t payout-rt-electrs -f staging/Dockerfile.electrs .`
//! `cargo build -p orchestrator-be`
//! `export PAYOUT_RT_SERVER="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import sys,json; print(json.load(sys.stdin)["target_directory"]+"/debug/server")')"`
//! `export PAYOUT_PHYS_XPUB='<account xpub from identity panel>' PAYOUT_PHYS_FINGERPRINT='<8 hex digits>'`
//! Optional: PAYOUT_PHYS_DIR (default .local/physical-session), PAYOUT_PHYS_SETS (default 1,4,20).
//! Every action is one command; substitute up, cosign, signatures, mine, conflict, down for ACTION:
//! `PAYOUT_PHYS_ACTION=ACTION cargo test -p alpen-multisig-e2e-tests --test payout_admin_physical_session -- --ignored --nocapture`
//! `mine` reads PAYOUT_PHYS_BLOCKS (default 1); `conflict` optionally selects PAYOUT_PHYS_TXID.
//! `signatures` prints only admin 2's signature lines (no submission). `conflict` uses admins 2+3
//! and an independent fee coin, then waits for the coordinator to expire the pending payout.
//! Mining does NOT advance the four-day wall-clock expiry; use conflict for mined-input expiry.
//! `up` leaves services running; `down` removes owned services, retaining reports/logs.
//! Verification only: `speculos-identity` requires LEDGER_SPECULOS_URL and prints the two exports;
//! `PAYOUT_PHYS_SOFTWARE=1 ... up` uses test admin 1; `... exercise` submits software admin-1
//! payouts, checks paste/cosign/broadcast/confirmation/conflict. Never set SOFTWARE for a human session.
mod payout_admin_common;
#[allow(dead_code)]
mod payout_admin_tools;
use bitcoin::{
    bip32::{Fingerprint, Xpub},
    consensus::encode::{deserialize_hex, serialize_hex},
    hashes::Hash,
    psbt::Psbt,
    secp256k1::{Message, Secp256k1},
    sign_message::{signed_msg_hash, MessageSignature},
    Address, OutPoint,
};
use payout_admin_common::*;
use payout_admin_core::{
    block_payouts_tx::{self, ConnectorRef},
    deployment::xonly_at_0_0,
    report::Report,
    test_support::{change, fee_coin, sign_as, sign_fee, Fixture},
    wire, Deployment,
};
use payout_admin_tools::*;
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    str::FromStr,
    time::Duration,
};

fn directory() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();

    root.join(std::env::var("PAYOUT_PHYS_DIR").unwrap_or_else(|_| ".local/physical-session".into()))
}

fn deployment(directory: &Path) -> Deployment {
    Deployment::load(
        &fs::read_to_string(directory.join("params.toml")).unwrap(),
        Some(&fs::read_to_string(directory.join("signers.toml")).unwrap()),
    )
    .unwrap()
}

fn process_identity(pid: &str) -> String {
    let output = Command::new("ps")
        .args(["-p", pid, "-o", "lstart=,command="])
        .output()
        .unwrap();

    String::from_utf8(output.stdout).unwrap()
}

fn down(directory: &Path) {
    if let Ok(text) = fs::read_to_string(directory.join("server.json")) {
        let record: Value = serde_json::from_str(&text).unwrap();
        let pid = record["pid"].as_str().unwrap();
        let identity = process_identity(pid);

        if !identity.trim().is_empty() {
            assert_eq!(
                identity,
                record["identity"].as_str().unwrap(),
                "PID reused; refusing to kill"
            );
            assert!(Command::new("kill")
                .args(["-TERM", pid])
                .status()
                .unwrap()
                .success());
        }

        fs::remove_file(directory.join("server.json")).unwrap();
    }

    for name in ["payout-phys-electrs", "payout-phys-bitcoin"] {
        let _ = Command::new("docker")
            .args(["rm", "-fv", name])
            .output()
            .unwrap();
    }
    let _ = Command::new("docker")
        .args(["network", "rm", "payout-phys-net"])
        .output()
        .unwrap();
}

async fn up(directory: &Path, fixture: &Fixture) {
    assert!(
        !directory.join("server.json").exists(),
        "run down before up"
    );
    let software = std::env::var("PAYOUT_PHYS_SOFTWARE").as_deref() == Ok("1");
    let (xpub, fingerprint) = if software {
        (fixture.account_xpub(0), fixture.fingerprint(0))
    } else {
        (
            Xpub::from_str(&std::env::var("PAYOUT_PHYS_XPUB").expect("set PAYOUT_PHYS_XPUB"))
                .unwrap(),
            Fingerprint::from_str(
                &std::env::var("PAYOUT_PHYS_FINGERPRINT").expect("set PAYOUT_PHYS_FINGERPRINT"),
            )
            .unwrap(),
        )
    };
    let sets: Vec<usize> = std::env::var("PAYOUT_PHYS_SETS")
        .unwrap_or_else(|_| "1,4,20".into())
        .split(',')
        .map(|count| count.parse().unwrap())
        .collect();
    assert!(sets.iter().all(|count| *count > 0 && *count <= 100));
    let params = fixture.params_toml().replacen(
        &fixture.admin_xonly(0).to_string(),
        &xonly_at_0_0(&xpub).unwrap().to_string(),
        1,
    );
    let roster = fixture
        .roster_toml()
        .replacen(&fixture.account_xpub(0).to_string(), &xpub.to_string(), 1)
        .replacen(
            &fixture.fingerprint(0).to_string(),
            &fingerprint.to_string(),
            1,
        );
    let deployment = Deployment::load(&params, Some(&roster)).unwrap();
    let server =
        fs::canonicalize(std::env::var("PAYOUT_RT_SERVER").expect("set PAYOUT_RT_SERVER")).unwrap();
    fs::create_dir_all(directory).unwrap();
    fs::write(directory.join("params.toml"), params).unwrap();
    fs::write(directory.join("signers.toml"), roster).unwrap();
    docker(&["network", "create", "payout-phys-net"]);
    docker(&[
        "run",
        "-d",
        "--name",
        "payout-phys-bitcoin",
        "--network",
        "payout-phys-net",
        "--network-alias",
        "bitcoin",
        "-p",
        "127.0.0.1:18500:18443",
        "bitcoin/bitcoin:30",
        "-regtest",
        "-server",
        "-rpcbind=0.0.0.0",
        "-rpcallowip=0.0.0.0/0",
        "-rpcuser=user",
        "-rpcpassword=password",
        "-fallbackfee=0.00002",
    ]);
    for _ in 0..100 {
        if Node.rpc("getblockchaininfo", json!([])).await.is_ok() {
            break;
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    call(&Node, "createwallet", json!(["payout-phys"])).await;
    mine(&Node, 110).await;
    let address = Address::p2tr(
        &Secp256k1::new(),
        deployment.admin_keys()[0],
        None,
        deployment.network(),
    );
    // Several separate coins allow the report sets to be tested while earlier payouts remain pending.
    for _ in 0..6 {
        call(&Node, "sendtoaddress", json!([address.to_string(), 0.01])).await;
    }
    let n_of_n = n_of_n_keypair(fixture, &deployment);
    for (set, count) in sets.iter().enumerate() {
        let folder = directory.join(format!("set-{}-{count}", set + 1));
        fs::create_dir_all(&folder).unwrap();
        for i in 0..*count {
            let report = game_report(fixture, &deployment, &n_of_n).await;
            fs::write(
                folder.join(format!("report-{:02}.json", i + 1)),
                serde_json::to_string_pretty(&report).unwrap(),
            )
            .unwrap();
        }
    }
    mine(&Node, 4).await;
    // Always last: no more blocks after the young ACK reaches two confirmations.
    let young = game_report(fixture, &deployment, &n_of_n).await;
    fs::write(
        directory.join("young-ack-2.json"),
        serde_json::to_string_pretty(&young).unwrap(),
    )
    .unwrap();
    docker(&[
        "run",
        "-d",
        "--name",
        "payout-phys-electrs",
        "--network",
        "payout-phys-net",
        "-p",
        "127.0.0.1:18501:60401",
        "payout-rt-electrs",
    ]);
    let mut indexed = false;
    for _ in 0..120 {
        if electrum(
            "blockchain.transaction.get",
            json!([young.counterproof_ack_txid().to_string(), true]),
        )
        .is_ok_and(|response| response["confirmations"] == 2)
        {
            indexed = true;
            break;
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(indexed, "electrs did not index young ACK");
    for (set, count) in sets.iter().enumerate() {
        for i in 0..*count {
            let report = Report::from_value(
                serde_json::from_str(
                    &fs::read_to_string(directory.join(format!(
                        "set-{}-{count}/report-{:02}.json",
                        set + 1,
                        i + 1
                    )))
                    .unwrap(),
                )
                .unwrap(),
                bitcoin::Network::Regtest,
            )
            .unwrap();
            let ack = electrum(
                "blockchain.transaction.get",
                json!([report.counterproof_ack_txid().to_string(), true]),
            )
            .unwrap();
            assert!(ack["confirmations"].as_u64().unwrap() >= 6);
        }
    }
    let log = fs::File::create(directory.join("coordinator.log")).unwrap();
    let mut child = Command::new(server)
        .current_dir(directory)
        .env_remove("DATABASE_URL")
        .env("SERVER_HOST", "127.0.0.1")
        .env("SERVER_PORT", "18503")
        .env("PAYOUT_ADMIN_PUBLIC_URL", "http://127.0.0.1:18503")
        .env("PAYOUT_PARAMS_PATH", directory.join("params.toml"))
        .env("PAYOUT_ELECTRUM_URL", "tcp://127.0.0.1:18501")
        .env("STRATA_ADMIN_STATE_RPC_URL", "http://127.0.0.1:1")
        .env("BITCOIN_RPC_URL", "http://127.0.0.1:1")
        .env("BITCOIN_NETWORK", "regtest")
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    let pid = child.id().to_string();
    fs::write(
        directory.join("server.json"),
        json!({"pid":pid,"identity":process_identity(&pid)}).to_string(),
    )
    .unwrap();
    let mut ready = false;
    for _ in 0..100 {
        assert!(
            child.try_wait().unwrap().is_none(),
            "server exited; see coordinator.log"
        );

        if reqwest::get("http://127.0.0.1:18503/api/v1/health")
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            ready = true;
            break;
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(ready);
    println!("Network: regtest\nElectrum: Custom → tcp://127.0.0.1:18501\nCoordinator: http://127.0.0.1:18503\nParams: {}\nSigners: {}\nAdmin Wallet receive 0/0: {address}\nReports: {}", directory.join("params.toml").display(), directory.join("signers.toml").display(), directory.display());
}

async fn game_report(
    fixture: &Fixture,
    deployment: &Deployment,
    n_of_n: &bitcoin::secp256k1::Keypair,
) -> Report {
    let (old, ack) = acked_game(&Node, fixture, deployment, n_of_n).await;
    let new = claim_and_contest(&Node, fixture, deployment, n_of_n).await;
    mine(&Node, 1).await;

    Report::from_value(json!({
        "network": "regtest", "operatorPubkey": fixture.operator_xonly(0),
        "newClaimTxid": new.claim.compute_txid(), "newContestTxid": new.contest.compute_txid(),
        "ackedClaimTxid": old.claim.compute_txid(), "ackedContestTxid": old.contest.compute_txid(),
        "counterproofAckTxid": ack.compute_txid(),
    }), bitcoin::Network::Regtest).unwrap()
}

async fn login(fixture: &Fixture, admin: usize) -> String {
    let challenge = api(wire::CHALLENGE, "", Some(json!({}))).await;
    let nonce = challenge["nonce"].as_str().unwrap();
    let signature = MessageSignature {
        signature: Secp256k1::new().sign_ecdsa_recoverable(
            &Message::from_digest(
                signed_msg_hash(&wire::sign_in_message("http://127.0.0.1:18503", nonce))
                    .to_byte_array(),
            ),
            &fixture.admin_keypair(admin).secret_key(),
        ),
        compressed: true,
    }
    .to_base64();

    api(
        wire::VERIFY,
        "",
        Some(json!({"nonce":nonce,"signature":signature})),
    )
    .await["sessionToken"]
        .as_str()
        .unwrap()
        .into()
}

async fn cosign(fixture: &Fixture, deployment: &Deployment, print_only: bool) {
    let token = login(fixture, 1).await;
    for item in api(wire::PENDING, &token, None).await.as_array().unwrap() {
        let mut psbt = Psbt::from_str(item["psbt"].as_str().unwrap()).unwrap();
        sign_as(fixture, deployment, &mut psbt, 1);
        let lines = block_payouts_tx::export_signatures(deployment, &psbt)
            .unwrap()
            .into_iter()
            .filter(|signature_line| signature_line.admin_key() == fixture.admin_xonly(1))
            .collect::<Vec<_>>();
        let text = block_payouts_tx::signatures_text(&lines);

        if print_only {
            println!("{text}");
        } else {
            let result = api(
                &wire::SIGNATURES.replace(":txid", item["txid"].as_str().unwrap()),
                &token,
                Some(json!({"signatures":text})),
            )
            .await;
            assert!(result["invalid"].as_array().unwrap().is_empty());
            println!("{}: {result}", item["txid"]);
        }
    }
}

async fn funded_coin(fixture: &Fixture) -> block_payouts_tx::FeeInput {
    let coin = fee_coin(fixture, 0, 9, 100_000);
    let address =
        Address::from_script(&coin.txout().script_pubkey, bitcoin::Network::Regtest).unwrap();
    let id = call(&Node, "sendtoaddress", json!([address.to_string(), 0.001])).await;
    let raw = call(&Node, "getrawtransaction", json!([id, true])).await;
    let transaction: bitcoin::Transaction = deserialize_hex(raw["hex"].as_str().unwrap()).unwrap();
    let outpoint = OutPoint::new(
        transaction.compute_txid(),
        transaction
            .output
            .iter()
            .position(|output| *output == coin.txout())
            .unwrap() as u32,
    );
    mine(&Node, 1).await;

    payout_admin_core::test_support::fee_coin_at(fixture, 0, 9, 100_000, outpoint)
}

async fn conflict(fixture: &Fixture, deployment: &Deployment) {
    let token = login(fixture, 1).await;
    let pending = api(wire::PENDING, &token, None).await;
    let chosen = std::env::var("PAYOUT_PHYS_TXID").ok();
    let item = pending
        .as_array()
        .unwrap()
        .iter()
        .find(|payout| chosen.as_ref().is_none_or(|id| payout["txid"] == *id))
        .expect("no matching pending payout");
    let original = Psbt::from_str(item["psbt"].as_str().unwrap()).unwrap();
    let analysis = block_payouts_tx::analyze(deployment, &original).unwrap();
    let claim = original.unsigned_tx.input[analysis.connector_inputs()[0].input_index()]
        .previous_output
        .txid;
    let coin = funded_coin(fixture).await;
    assert!(!original
        .unsigned_tx
        .input
        .iter()
        .any(|i| i.previous_output == coin.outpoint()));
    let mut psbt = block_payouts_tx::build_psbt(
        deployment,
        &[ConnectorRef::new(claim)],
        std::slice::from_ref(&coin),
        &change(fixture, 97_000),
    )
    .unwrap();
    sign_fee(fixture, &mut psbt, &coin, 9);
    sign_as(fixture, deployment, &mut psbt, 1);
    sign_as(fixture, deployment, &mut psbt, 2);
    call(
        &Node,
        "sendrawtransaction",
        json!([serialize_hex(
            &block_payouts_tx::finalize(deployment, &psbt).unwrap()
        )]),
    )
    .await;
    mine(&Node, 1).await;
    for _ in 0..90 {
        if !api(wire::PENDING, &token, None)
            .await
            .as_array()
            .unwrap()
            .iter()
            .any(|payout| payout["txid"] == item["txid"])
        {
            println!("Expired {} by mined claim-connector conflict", item["txid"]);

            return;
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("coordinator did not expire mined conflict");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "manual persistent regtest tooling; see header"]
async fn session() {
    std::env::set_var("DOCKER_CONTEXT", "colima");
    std::env::set_var("PAYOUT_TEST_RPC", "http://127.0.0.1:18500");
    std::env::set_var("PAYOUT_TEST_ELECTRUM", "127.0.0.1:18501");
    std::env::set_var("PAYOUT_TEST_API", "http://127.0.0.1:18503");
    let directory = directory();
    let fixture = Fixture::new(3, 2);
    match std::env::var("PAYOUT_PHYS_ACTION")
        .expect("set PAYOUT_PHYS_ACTION")
        .as_str()
    {
        "up" => up(&directory, &fixture).await,
        "down" => down(&directory),
        "mine" => {
            mine(
                &Node,
                std::env::var("PAYOUT_PHYS_BLOCKS")
                    .unwrap_or_else(|_| "1".into())
                    .parse()
                    .unwrap(),
            )
            .await
        }
        "cosign" => cosign(&fixture, &deployment(&directory), false).await,
        "signatures" => cosign(&fixture, &deployment(&directory), true).await,
        "conflict" => conflict(&fixture, &deployment(&directory)).await,
        "exercise" => exercise(&directory, &fixture).await,
        "speculos-identity" => {
            assert_eq!(
                std::env::var("LEDGER_SPECULOS_URL").as_deref(),
                Ok("http://127.0.0.1:5001"),
                "Speculos only; HID forbidden"
            );
            let (xpub, fingerprint) = tokio::task::spawn_blocking(|| {
                use desktop_app::infrastructure::hw_wallet::ledger;

                (
                    ledger::get_account_xpub("m/86'/1'/73'").unwrap(),
                    ledger::get_master_fingerprint().unwrap(),
                )
            })
            .await
            .unwrap();
            println!(
                "export PAYOUT_PHYS_XPUB='{xpub}' PAYOUT_PHYS_FINGERPRINT='{}'",
                Fingerprint::from(fingerprint.to_le_bytes())
            );
        }
        action => panic!("unknown action {action}"),
    }
}

async fn exercise(directory: &Path, fixture: &Fixture) {
    let deployment = deployment(directory);
    assert_eq!(
        deployment.admin_keys()[0],
        fixture.admin_xonly(0),
        "exercise requires software up"
    );
    let token = login(fixture, 0).await;
    let mut reports = Vec::new();
    for folder in fs::read_dir(directory)
        .unwrap()
        .flatten()
        .filter(|path| path.file_name().to_string_lossy().starts_with("set-"))
    {
        for file in fs::read_dir(folder.path()).unwrap().flatten() {
            reports.push(
                Report::from_value(
                    serde_json::from_str(&fs::read_to_string(file.path()).unwrap()).unwrap(),
                    bitcoin::Network::Regtest,
                )
                .unwrap(),
            );
        }
    }
    assert!(reports.len() >= 2);
    for report in reports.iter().take(2) {
        let coin = funded_coin(fixture).await;
        let mut psbt = block_payouts_tx::build_psbt(
            &deployment,
            &[ConnectorRef::new(report.new_claim_txid())],
            std::slice::from_ref(&coin),
            &change(fixture, 98_000),
        )
        .unwrap();
        sign_fee(fixture, &mut psbt, &coin, 9);
        sign_as(fixture, &deployment, &mut psbt, 0);
        api(
            wire::PENDING,
            &token,
            Some(json!({"psbt":psbt.to_string()})),
        )
        .await;
    }
    conflict(fixture, &deployment).await;
    cosign(fixture, &deployment, true).await;
    cosign(fixture, &deployment, false).await;
    assert!(api(wire::PENDING, &token, None)
        .await
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        api(wire::PAST, &token, None)
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    mine(&Node, 1).await;
    for _ in 0..90 {
        if api(wire::PAST, &token, None).await[0]["status"] == "confirmed" {
            return;
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("payout not confirmed");
}
