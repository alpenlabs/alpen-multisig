//! Docker regtest, Electrum and real coordinator (memory and Postgres), software test keys only.
//! Run from the repo root (Colima already running):
//! `export DOCKER_CONTEXT=colima; docker build -t payout-rt-electrs -f staging/Dockerfile.electrs . && cargo build -p orchestrator-be && PAYOUT_RT_SERVER="$(cargo metadata --format-version 1 --no-deps | python3 -c 'import sys,json; print(json.load(sys.stdin)["target_directory"]+"/debug/server")')" cargo test -p alpen-multisig-e2e-tests --test payout_admin_regtest_seed -- --ignored --nocapture`
//! Owns only payout-rt-* containers and ports 18400..18403; removes services on unwind.
//! Game construction is copied/cut from payout-admin-reference's seeder/common/tools.
mod payout_admin_asm;
mod payout_admin_common;
use payout_admin_core::test_support::NACK_TIMELOCK;
mod payout_admin_tools;
use bitcoin::{
    consensus::encode::{deserialize_hex, serialize_hex},
    hashes::Hash,
    secp256k1::{Message, Secp256k1},
    sign_message::{signed_msg_hash, MessageSignature},
    OutPoint,
};
use payout_admin_common::*;
use payout_admin_tools::*;

use payout_admin_core::{
    block_payouts_tx::{self, ConnectorRef},
    report::{self, MinedBlock, MinedTx, Report, ReportVerdict},
    test_support::{change, fee_coin, sign_as, sign_fee, Fixture},
    wire,
};
use serde_json::json;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

async fn indexed(report: &Report, confirmations: u64) -> HashMap<bitcoin::Txid, MinedTx> {
    for _ in 0..120 {
        let height = electrum("blockchain.headers.subscribe", json!([]))
            .ok()
            .and_then(|response| response["height"].as_u64());
        let mut map = HashMap::new();
        report
            .fetch_transactions(&mut map, |id| {
                if let Ok(response) =
                    electrum("blockchain.transaction.get", json!([id.to_string(), true]))
                {
                    if let (Some(hex), Some(confirmations), Some(tip_height)) = (
                        response["hex"].as_str(),
                        response["confirmations"].as_u64(),
                        height,
                    ) {
                        if 0 < confirmations && confirmations <= tip_height + 1 {
                            return Ok::<_, ()>(Some(MinedTx::new(
                                deserialize_hex(hex).unwrap(),
                                Some(MinedBlock::new(
                                    tip_height + 1 - confirmations,
                                    confirmations,
                                )),
                            )));
                        }
                    }
                }
                Ok(None)
            })
            .unwrap();

        if 5 <= map.len()
            && map[&report.counterproof_ack_txid()]
                .block()
                .is_some_and(|block| block.confirmations() == confirmations)
        {
            return map;
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("Electrum did not index report with {confirmations} ACK confirmations");
}

#[tokio::test]
async fn kit_asm_history_decodes_with_the_desktop_rpc_client() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let fixture = Fixture::new(3, 2);
    let expected = fixture.deployment().covenant().n_of_n();
    let server =
        tokio::spawn(async move { payout_admin_asm::serve(listener, &fixture).await.unwrap() });
    let bridge = desktop_app::infrastructure::asm_status_rpc::fetch_bridge_state(&url)
        .await
        .unwrap();
    assert_eq!(
        payout_admin_core::Deployment::history_keys(bridge.operators()).unwrap(),
        [expected]
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker/Colima, payout-rt-electrs image and PAYOUT_RT_SERVER; see header command"]
async fn payout_admin_regtest_seed() {
    let started = Instant::now();
    let stack = Stack::start();
    let node = Node;
    let mut ready = false;
    for _ in 0..100 {
        if node.rpc("getblockchaininfo", json!([])).await.is_ok() {
            ready = true;
            break;
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(ready);
    call(&node, "createwallet", json!(["payout-rt"])).await;
    mine(&node, 110).await;
    stack.electrs();
    let fixture = Fixture::new(3, 2);
    let deployment = fixture.deployment();
    let n_of_n = n_of_n_keypair(&fixture, &deployment);
    let directory = std::env::temp_dir().join(format!("payout-rt-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let signers = directory.join("signers.toml");
    std::fs::write(&signers, fixture.roster_toml()).unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let asm_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        payout_admin_asm::serve(listener, &Fixture::new(3, 2))
            .await
            .unwrap()
    });
    for postgres in [false, true] {
        let mut coins = Vec::new();
        for index in 0..2 {
            let coin = fee_coin(&fixture, 0, index, 100_000);
            let address =
                bitcoin::Address::from_script(&coin.txout().script_pubkey, deployment.network())
                    .unwrap();
            let id = call(&node, "sendtoaddress", json!([address.to_string(), 0.001])).await;
            let funded = call(&node, "getrawtransaction", json!([id, true])).await;
            let transaction: bitcoin::Transaction =
                deserialize_hex(funded["hex"].as_str().unwrap()).unwrap();
            let vout = transaction
                .output
                .iter()
                .position(|output| *output == coin.txout())
                .unwrap();
            let outpoint = OutPoint::new(transaction.compute_txid(), vout as u32);
            coins.push(payout_admin_core::test_support::fee_coin_at(
                &fixture, 0, index, 100_000, outpoint,
            ));
        }
        let coin = &coins[0];
        let (_, ack) = acked_game(&node, &fixture, &deployment, &n_of_n).await;
        let new = claim_and_contest(&node, &fixture, &deployment, &n_of_n).await;
        mine(&node, 1).await;
        let report = Report::from_value(
            json!({
                "version": 2, "network": "regtest", "contestTxids": [new.contest.compute_txid()],
                "counterproofAckTxid": ack.compute_txid(),
            }),
            bitcoin::Network::Regtest,
        )
        .unwrap();
        let young = report::verify_reports(
            &deployment,
            std::slice::from_ref(&report),
            &indexed(&report, 2).await,
            &HashMap::new(),
        )
        .remove(0);
        assert!(
            matches!(young.verdict(), ReportVerdict::Invalid(reason) if reason.contains("2 of the 6 confirmations"))
        );
        mine(&node, 4).await;
        let verified = report::verify_reports(
            &deployment,
            std::slice::from_ref(&report),
            &indexed(&report, 6).await,
            &HashMap::new(),
        )
        .remove(0);
        assert_eq!(
            verified.verdict(),
            &ReportVerdict::Valid,
            "{:?}",
            verified.verdict()
        );
        assert_eq!(
            verified.new_claim_admin_payout_outpoint().unwrap().txid,
            new.claim.compute_txid()
        );
        let _server = Coordinator::start(&signers, &asm_url, postgres).await;
        let mut tokens = Vec::new();
        for admin in 0..3 {
            let challenge = api(wire::CHALLENGE, "", Some(json!({}))).await;
            let nonce = challenge["nonce"].as_str().unwrap();
            let message = Message::from_digest(
                signed_msg_hash(&wire::sign_in_message("http://127.0.0.1:18403", nonce))
                    .to_byte_array(),
            );
            let signature = MessageSignature {
                signature: Secp256k1::new()
                    .sign_ecdsa_recoverable(&message, &fixture.admin_keypair(admin).secret_key()),
                compressed: true,
            }
            .to_base64();
            let response = api(
                wire::VERIFY,
                "",
                Some(json!({"nonce":nonce,"signature":signature})),
            )
            .await;
            assert_eq!(response["adminKey"], fixture.admin_xonly(admin).to_string());
            tokens.push(response["sessionToken"].as_str().unwrap().to_owned());
        }
        let mut psbt = block_payouts_tx::build_psbt(
            &deployment,
            &[ConnectorRef::new(
                verified.new_claim_admin_payout_outpoint().unwrap().txid,
            )],
            std::slice::from_ref(coin),
            &change(&fixture, 98_000),
        )
        .unwrap();
        // Only the claim input overlaps: expiry cannot pass just because a fee input was spent.
        let competing = block_payouts_tx::build_psbt(
            &deployment,
            &[ConnectorRef::new(
                verified.new_claim_admin_payout_outpoint().unwrap().txid,
            )],
            &coins[1..],
            &change(&fixture, 97_000),
        )
        .unwrap();
        assert_ne!(
            competing.unsigned_tx.input[1].previous_output,
            psbt.unsigned_tx.input[1].previous_output
        );
        let competing_id = competing.unsigned_tx.compute_txid().to_string();
        api(
            wire::PENDING,
            &tokens[2],
            Some(json!({"psbt":competing.to_string()})),
        )
        .await;
        sign_fee(&fixture, &mut psbt, coin, 0);
        sign_as(&fixture, &deployment, &mut psbt, 0);
        let id = psbt.unsigned_tx.compute_txid().to_string();
        let first = api(
            wire::PENDING,
            &tokens[0],
            Some(
                json!({"psbt":psbt.to_string(), "report":serde_json::to_string(&report).unwrap()}),
            ),
        )
        .await;
        assert_eq!(first["adminsSigned"], 1);
        assert_eq!(first["signaturesNeeded"], 2);
        sign_as(&fixture, &deployment, &mut psbt, 1);
        api(
            wire::PENDING,
            &tokens[1],
            Some(json!({"psbt":psbt.to_string()})),
        )
        .await;
        let past = api(wire::PAST, &tokens[2], None).await;
        let entry = past
            .as_array()
            .unwrap()
            .iter()
            .find(|payout| payout["txid"] == id)
            .expect("quorum broadcast");
        assert_eq!(entry["status"], "unconfirmed");
        assert!(entry["blockTimeUnixSeconds"].is_null());
        assert_eq!(
            entry["rawTx"],
            serialize_hex(&block_payouts_tx::finalize(&deployment, &psbt).unwrap())
        );
        assert!(call(&node, "getrawmempool", json!([]))
            .await
            .as_array()
            .unwrap()
            .contains(&json!(id)));
        // Ensure Electrum exposes the conflict before giving the worker a full polling interval.
        let mut script_hash = bitcoin::hashes::sha256::Hash::hash(
            deployment.covenant().connector_script().as_bytes(),
        )
        .to_byte_array();
        script_hash.reverse();
        let mut seen = false;
        for _ in 0..60 {
            let history = electrum(
                "blockchain.scripthash.get_history",
                json!([hex::encode(script_hash)]),
            )
            .unwrap();

            if history
                .as_array()
                .unwrap()
                .iter()
                .any(|history_entry| history_entry["tx_hash"] == id && history_entry["height"] == 0)
            {
                seen = true;
                break;
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        assert!(seen, "Electrum must expose mempool conflict");
        // Let the background worker observe the mempool conflict; it must retain the competitor.
        tokio::time::sleep(Duration::from_secs(35)).await;
        assert!(api(wire::PENDING, &tokens[2], None)
            .await
            .as_array()
            .unwrap()
            .iter()
            .any(|payout| payout["txid"] == competing_id));
        let block = call(
            &node,
            "generatetoaddress",
            json!([1, new_addr(&node).await]),
        )
        .await;
        let header = call(&node, "getblockheader", json!([block[0]])).await;
        let expected_time = header["time"].as_i64().unwrap();
        let mut confirmed = false;
        for _ in 0..90 {
            let past = api(wire::PAST, &tokens[2], None).await;
            let pending = api(wire::PENDING, &tokens[2], None).await;

            if past.as_array().unwrap().iter().any(|payout| {
                payout["txid"] == id
                    && payout["status"] == "confirmed"
                    && payout["blockTimeUnixSeconds"] == expected_time
            }) && !pending
                .as_array()
                .unwrap()
                .iter()
                .any(|payout| payout["txid"] == competing_id)
            {
                confirmed = true;
                break;
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        assert!(
            confirmed,
            "worker must confirm payout and expire mined conflict"
        );

        if postgres {
            let pool =
                sqlx::PgPool::connect("postgres://postgres:payout-test@127.0.0.1:18402/postgres")
                    .await
                    .unwrap();
            let rows: Vec<String> = sqlx::query_scalar("SELECT txid FROM payout_admin_records")
                .fetch_all(&pool)
                .await
                .unwrap();
            assert!(rows.contains(&id));
            assert!(
                !rows.contains(&competing_id),
                "expired record must be deleted from Postgres"
            );
            pool.close().await;
        }

        eprintln!(
            "{} lifecycle passed",
            if postgres { "Postgres" } else { "memory" }
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
    drop(stack);
    eprintln!("regtest runtime: {:?}", started.elapsed());
}
