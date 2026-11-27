//! Speculos-only device contract test, adapted from the reference e2e_payout_admin_speculos.
//! Run alone with LEDGER_SPECULOS_URL=http://127.0.0.1:5001 and SPECULOS_CAPTURE_DIR.
//! Requires SPECULOS_COORDINATOR_BIN pointing to a built orchestrator-be.
//! Synthetic prevouts: verifies signatures and real coordinator auth, not mining/broadcast.
#[path = "../../../e2e-tests/tests/payout_admin_asm/mod.rs"]
mod payout_admin_asm;
use bitcoin::{
    bip32::Xpriv,
    hashes::Hash,
    secp256k1::{Message, Secp256k1},
    Network,
};
use desktop_app::infrastructure::hw_wallet::ledger::payout_admin as device;
use payout_admin_core::{
    block_payouts_tx, deployment, policy,
    test_support::{self, Fixture},
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

const SPECULOS_SEED: &str = "glory promote mansion idle axis finger extra february uncover one trip resource lawn turtle enact monster seven myth punch hobby comfort wild raise skin";

struct CoordinatorProcess(std::process::Child);

impl Drop for CoordinatorProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// Adapted from the reference's device user: capture before each button press, never HID.
async fn device_user(url: String, root: PathBuf, step: Arc<Mutex<String>>) {
    let http = reqwest::Client::new();
    let mut last = String::new();
    let mut counts = std::collections::HashMap::<String, usize>::new();
    loop {
        tokio::time::sleep(Duration::from_millis(350)).await;
        let current = step.lock().unwrap().clone();
        let response: Value = http
            .get(format!("{url}/events?currentscreenonly=true"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let texts: Vec<_> = response["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|event| event["text"].as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .collect();

        if texts.is_empty() {
            continue;
        }

        let line = texts.join(" | ");
        let key = format!("{current}:{line}");

        if key != last {
            let directory = root.join(&current);
            std::fs::create_dir_all(&directory).unwrap();
            let count = counts.entry(current.clone()).or_default();
            *count += 1;
            let name = format!("{count:03}.png");
            let png = http
                .get(format!("{url}/screenshot"))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .bytes()
                .await
                .unwrap();
            assert!(png.starts_with(b"\x89PNG"));
            std::fs::write(directory.join(&name), &png).unwrap();
            use std::io::Write;
            let mut index = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(directory.join("index.md"))
                .unwrap();
            writeln!(index, "{count}. [{name}]({name}) — {line}").unwrap();
            eprintln!("{current}: {line}");
            last = key;
        }

        if texts.contains(&"app is ready") {
            continue;
        }

        let title = texts[0].to_ascii_lowercase();
        let terminal = if current == "reject" {
            title.starts_with("reject")
        } else {
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
            .any(|word| title == *word || title.starts_with(&format!("{word} ")))
        };
        let button = if terminal && texts.len() <= 2 && !texts[0].starts_with("Review") {
            "both"
        } else {
            "right"
        };
        http.post(format!("{url}/button/{button}"))
            .json(&json!({"action":"press-and-release"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn payout_admin_speculos() {
    let Ok(url) = std::env::var("LEDGER_SPECULOS_URL") else {
        eprintln!("skip: emulator URL absent; physical Ledger is never used");

        return;
    };
    assert!(["http://localhost:5001", "http://127.0.0.1:5001"].contains(&url.as_str()));
    // The product's transport selects Speculos exclusively whenever this variable is present.
    std::env::set_var("LEDGER_SPECULOS_APDU_ADDR", "127.0.0.1:9999");
    std::env::set_var("LEDGER_SPECULOS_AUTO_APPROVE", "0");
    let root =
        PathBuf::from(std::env::var("SPECULOS_CAPTURE_DIR").expect("capture directory required"));
    let step = Arc::new(Mutex::new("identity".to_string()));
    let driver = tokio::spawn(device_user(url, root.clone(), step.clone()));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let asm_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        payout_admin_asm::serve(listener, &Fixture::new(3, 2))
            .await
            .unwrap()
    });
    let flow = tokio::task::spawn_blocking(move || {
        let mut fixture = Fixture::new(3, 2);
        let seed = bip39::Mnemonic::parse(SPECULOS_SEED).unwrap().to_seed("");
        fixture.replace_admin_master(0, Xpriv::new_master(Network::Regtest, &seed).unwrap());
        let deployment = fixture.deployment();
        std::fs::create_dir_all(&root).unwrap();
        let signers = root.join("signers.toml");
        std::fs::write(&signers, fixture.roster_toml()).unwrap();
        // Authentication needs no chain calls. Hold an inert TCP endpoint for the
        // coordinator's Electrum connection; never submit synthetic prevouts to it.
        let electrum = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let electrum_address = electrum.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{BufRead, Write};
            let (mut stream, _) = electrum.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(
                    request["method"], "server.version",
                    "auth must not access chain data"
                );
                writeln!(
                    stream,
                    "{}",
                    json!({"id":request["id"],"result":["Speculos auth fixture","1.4"]})
                )
                .unwrap();
                line.clear();
            }
        });
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let _coordinator = CoordinatorProcess(
            std::process::Command::new(
                std::env::var("SPECULOS_COORDINATOR_BIN")
                    .expect("built coordinator binary required"),
            )
            .env_remove("DATABASE_URL")
            .env("SERVER_HOST", "127.0.0.1")
            .env("SERVER_PORT", port.to_string())
            .env(
                "PAYOUT_ADMIN_PUBLIC_URL",
                format!("http://127.0.0.1:{port}"),
            )
            .env("PAYOUT_SIGNERS_PATH", &signers)
            .env("STRATA_ADMIN_STATE_RPC_URL", asm_url)
            .env("PAYOUT_ELECTRUM_URL", format!("tcp://{electrum_address}"))
            .stdout(std::fs::File::create(root.join("coordinator-stdout.log")).unwrap())
            .stderr(std::fs::File::create(root.join("coordinator.log")).unwrap())
            .spawn()
            .unwrap(),
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let http = reqwest::Client::new();
        let base = format!("http://127.0.0.1:{port}");
        runtime.block_on(async {
            for _ in 0..100 {
                if http.get(format!("{base}/health")).send().await.is_ok() {
                    return;
                }

                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            panic!("coordinator did not start");
        });
        let policy = policy::admin_multisig_policy(&deployment, deployment.covenant());
        let wallet = policy::admin_wallet_policy(&deployment.signers()[0], deployment.network());
        let app = device::open_app().unwrap();
        assert!(matches!(app.version, (2, 4, 6) | (2, 5, 1)));
        eprintln!("APP: {app:?}");
        let (fingerprint, xpub) =
            device::read_identity(&deployment::account_path(deployment.network())).unwrap();
        assert_eq!(fingerprint, fixture.fingerprint(0));
        assert_eq!(xpub.public_key, fixture.account_xpub(0).public_key);
        assert_eq!(
            deployment::xonly_at_0_0(&xpub).unwrap(),
            fixture.admin_xonly(0)
        );
        let address = device::policy_address(&wallet, None, 0, true).unwrap();
        assert_eq!(
            address.script_pubkey(),
            bitcoin::ScriptBuf::new_p2tr(&Secp256k1::new(), fixture.admin_xonly(0), None)
        );
        *step.lock().unwrap() = "sign-in".into();
        let challenge: payout_admin_core::wire::ChallengeResponse = runtime.block_on(async {
            http.post(format!("{base}{}", payout_admin_core::wire::CHALLENGE))
                .json(&json!({}))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap()
        });
        let message = payout_admin_core::wire::sign_in_message(&base, &challenge.nonce);
        let signature = device::sign_message(
            &message,
            &deployment::admin_key_path(deployment.network()),
            fingerprint,
        )
        .unwrap();
        let bytes = hex::decode(&signature.signature_hex).unwrap();
        let signature = bitcoin::secp256k1::ecdsa::Signature::from_compact(&bytes[..64]).unwrap();
        Secp256k1::new()
            .verify_ecdsa(
                &Message::from_digest(
                    bitcoin::sign_message::signed_msg_hash(&message).to_byte_array(),
                ),
                &signature,
                &fixture.admin_keypair(0).public_key(),
            )
            .unwrap();
        let mut compact = vec![31 + bytes[64]];
        compact.extend_from_slice(&bytes[..64]);
        let signature = bitcoin::sign_message::MessageSignature::from_slice(&compact)
            .unwrap()
            .to_base64();
        let session: payout_admin_core::wire::VerifyResponse = runtime.block_on(async {
            http.post(format!("{base}{}", payout_admin_core::wire::VERIFY))
                .json(&json!({"nonce":challenge.nonce,"signature":signature}))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap()
        });
        assert_eq!(session.admin_key, fixture.admin_xonly(0));
        assert_ne!(session.session_token, [0; 32]);
        *step.lock().unwrap() = "registration".into();
        let hmac = device::register_policy(&policy, fingerprint).unwrap();
        let address = device::policy_address(&policy, Some(hmac), 0, false).unwrap();
        assert_eq!(
            address.script_pubkey(),
            policy::connector_address(&deployment).script_pubkey()
        );
        for count in [1, 4, 20] {
            *step.lock().unwrap() = format!("payout-{count}");
            let claims: Vec<_> = (0..count)
                .map(|i| {
                    block_payouts_tx::ConnectorRef::new(fixture.game(&deployment, i).claim_txid())
                })
                .collect();
            let coin = test_support::fee_coin(&fixture, 0, 4, 100_000);
            let mut psbt = block_payouts_tx::build_psbt(
                &deployment,
                &claims,
                &[coin],
                &test_support::change(&fixture, 90_000),
            )
            .unwrap();
            let unsigned = psbt.unsigned_tx.clone();
            assert_eq!(
                device::sign_psbt(&mut psbt, &policy, Some(hmac), fingerprint).unwrap(),
                count as usize
            );
            assert_eq!(
                device::sign_psbt(&mut psbt, &wallet, None, fingerprint).unwrap(),
                1
            );
            let analysis = block_payouts_tx::analyze(&deployment, &psbt).unwrap();
            assert_eq!(analysis.signers(), vec![0]);
            assert!(analysis
                .fee_inputs()
                .iter()
                .all(|fee_input| fee_input.signed()));
            test_support::sign_as(&fixture, &deployment, &mut psbt, 1);
            assert!(block_payouts_tx::analyze(&deployment, &psbt)
                .unwrap()
                .is_complete());
            let transaction = block_payouts_tx::finalize(&deployment, &psbt).unwrap();
            assert_eq!(transaction.compute_txid(), unsigned.compute_txid());
            assert_eq!(transaction.input.len(), count as usize + 1);
        }
        *step.lock().unwrap() = "reject".into();
        let error = device::sign_message(
            &message,
            &deployment::admin_key_path(deployment.network()),
            fingerprint,
        )
        .unwrap_err();
        assert_eq!(
            error,
            "Request rejected on device — confirm the operation on your Ledger"
        );
        eprintln!("PASS: identity, nonce signature, registration, payouts 1/4/20, clean rejection");
    });
    let result = tokio::time::timeout(Duration::from_secs(900), flow).await;
    driver.abort();
    result.expect("Speculos flow timeout").unwrap();
}
