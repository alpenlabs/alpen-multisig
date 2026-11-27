use super::*;
use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use bitcoin::{
    hashes::Hash,
    secp256k1::{Message, Secp256k1},
    sign_message::{signed_msg_hash, MessageSignature},
    ScriptBuf, Transaction,
};
use http_body_util::BodyExt;
use payout_admin_core::test_support::*;
use tower::ServiceExt;

#[derive(Default)]
struct FakeBlockchain {
    history_calls: std::sync::atomic::AtomicUsize,
    history: tokio::sync::Mutex<blockchain::History>,
    fail: std::sync::atomic::AtomicBool,
    broadcasts: tokio::sync::Mutex<Vec<Transaction>>,
}

#[async_trait]
impl blockchain::Blockchain for FakeBlockchain {
    async fn history(&self, _: ScriptBuf) -> Result<blockchain::History> {
        self.history_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Ok(self.history.lock().await.clone())
    }

    async fn block_time(&self, _: usize) -> Result<SystemTime> {
        Ok(SystemTime::UNIX_EPOCH)
    }

    async fn broadcast(&self, transaction: Transaction) -> Result<()> {
        if self.fail.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(bad("offline"));
        }

        self.broadcasts.lock().await.push(transaction);

        Ok(())
    }
}

fn setup() -> (Fixture, Arc<Service>, Arc<FakeBlockchain>, Psbt) {
    let fixture = Fixture::new(3, 2);
    let deployment = fixture.deployment();
    let blockchain = Arc::new(FakeBlockchain::default());
    let coin = fee_coin(&fixture, 0, 0, 100_000);
    let mut psbt = block_payouts_tx::build_psbt(
        &deployment,
        &[block_payouts_tx::ConnectorRef::new(
            fixture.game(&deployment, 1).claim_txid(),
        )],
        std::slice::from_ref(&coin),
        &change(&fixture, 90_000),
    )
    .unwrap();
    sign_fee(&fixture, &mut psbt, &coin, 0);
    let service = Arc::new(Service {
        public_origin: "https://test.example".into(),
        submissions: Default::default(),
        deployment: tokio::sync::watch::channel(Arc::new(deployment)).0,
        database: Arc::new(database::InMemory::default()),
        blockchain: blockchain.clone(),
        auth: Default::default(),
        challenge_ttl: Duration::from_secs(120),
        session_ttl: Duration::from_secs(86400),
    });

    (fixture, service, blockchain, psbt)
}

fn signature_text(fixture: &Fixture, service: &Service, psbt: &Psbt, admin: usize) -> String {
    let mut psbt = psbt.clone();
    sign_as(fixture, &service.deployment(), &mut psbt, admin);

    block_payouts_tx::signatures_text(
        &block_payouts_tx::export_signatures(&service.deployment(), &psbt).unwrap(),
    )
}

#[tokio::test]
async fn lifecycle_signatures_expiry_retry_and_confirmation() {
    let (fixture, service, blockchain, psbt) = setup();
    let time = deadline(SystemTime::now(), Duration::ZERO).unwrap();
    let pending_view = service.submit(&psbt.to_string(), time).await.unwrap();
    assert_eq!(pending_view.admins_signed, 0);
    assert_eq!(pending_view.expires_at, deadline(time, EXPIRY).unwrap());
    let pasted = service
        .paste(
            &pending_view.txid.to_string(),
            &format!("{}\ninvalid", signature_text(&fixture, &service, &psbt, 0)),
            deadline(time, Duration::from_secs(1)).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pasted.invalid.len(), 1);
    let pending = pasted.pending.unwrap();
    assert_eq!(
        pending.expires_at,
        deadline(deadline(time, Duration::from_secs(1)).unwrap(), EXPIRY).unwrap()
    );
    assert_eq!(pending.signed_by, vec![fixture.admin_xonly(0)]);
    blockchain
        .fail
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let pasted = service
        .paste(
            &pending_view.txid.to_string(),
            &signature_text(&fixture, &service, &psbt, 1),
            deadline(time, Duration::from_secs(2)).unwrap(),
        )
        .await
        .unwrap();
    assert!(pasted
        .pending
        .unwrap()
        .broadcast_error
        .unwrap()
        .contains("offline"));
    blockchain
        .fail
        .store(false, std::sync::atomic::Ordering::Relaxed);
    service
        .tick(deadline(time, Duration::from_secs(3)).unwrap())
        .await
        .unwrap();
    assert!(service.list_pending().await.unwrap().is_empty());
    let transaction = blockchain.broadcasts.lock().await[0].clone();
    blockchain.history.lock().await.push((transaction, Some(1)));
    service
        .tick(deadline(time, Duration::from_secs(4)).unwrap())
        .await
        .unwrap();
    let past = service
        .get(&pending_view.txid.to_string())
        .await
        .unwrap()
        .past
        .unwrap();
    assert_eq!(past.status, PastStatus::Confirmed);
    assert_eq!(past.block_time, Some(SystemTime::UNIX_EPOCH));
    blockchain.history.lock().await.clear();
    service
        .tick(deadline(time, Duration::from_secs(5)).unwrap())
        .await
        .unwrap();
    assert_eq!(
        service
            .get(&pending_view.txid.to_string())
            .await
            .unwrap()
            .past
            .unwrap()
            .block_time,
        None
    );
}

#[tokio::test]
async fn unsigned_expiry_conflicts_and_finalized_refusal() {
    let (_, service, blockchain, psbt) = setup();
    let time = deadline(SystemTime::now(), Duration::ZERO).unwrap();
    let pending_view = service.submit(&psbt.to_string(), time).await.unwrap();
    let mut conflict = psbt.unsigned_tx.clone();
    conflict.output[0].value = bitcoin::Amount::from_sat(80_000);
    blockchain
        .history
        .lock()
        .await
        .push((conflict.clone(), None));
    service
        .tick(deadline(time, Duration::from_secs(1)).unwrap())
        .await
        .unwrap();
    assert!(service.get(&pending_view.txid.to_string()).await.is_ok());
    *blockchain.history.lock().await = vec![(conflict, Some(1))];
    service
        .tick(deadline(time, Duration::from_secs(1)).unwrap())
        .await
        .unwrap();
    assert!(service.get(&pending_view.txid.to_string()).await.is_err());
    blockchain.history.lock().await.clear();
    service.submit(&psbt.to_string(), time).await.unwrap();
    service.tick(deadline(time, EXPIRY).unwrap()).await.unwrap();
    assert!(service.get(&pending_view.txid.to_string()).await.is_err());
    let mut finalized = psbt;
    finalized.inputs[0].final_script_witness = Some(bitcoin::Witness::new());
    assert!(service.submit(&finalized.to_string(), time).await.is_err());
}

#[tokio::test]
async fn shared_claim_is_accepted_and_signatures_merge_concurrently() {
    let (fixture, service, _, psbt) = setup();
    let time = deadline(SystemTime::now(), Duration::ZERO).unwrap();
    let id = service
        .submit(&psbt.to_string(), time)
        .await
        .unwrap()
        .txid
        .to_string();
    let mut other = psbt.clone();
    other.unsigned_tx.output[0].value = bitcoin::Amount::from_sat(80_000);
    // Sharing only the claim is allowed and remains visible.
    other.unsigned_tx.input[1].previous_output.vout += 1;
    service.submit(&other.to_string(), time).await.unwrap();
    assert!(service
        .list_pending()
        .await
        .unwrap()
        .iter()
        .all(|pending_view| pending_view.duplicate_outpoints
            == vec![psbt.unsigned_tx.input[0].previous_output]));
    let first_signature = signature_text(&fixture, &service, &psbt, 0);
    let second_signature = signature_text(&fixture, &service, &psbt, 1);
    let (first_result, second_result) = tokio::join!(
        service.paste(&id, &first_signature, time),
        service.paste(&id, &second_signature, time)
    );
    first_result.unwrap();
    second_result.unwrap();
    assert!(service.get(&id).await.unwrap().past.is_some());
}

#[tokio::test]
async fn shared_fee_coin_is_refused() {
    let (_, service, _, psbt) = setup();
    let time = SystemTime::now();
    let mut other = psbt.clone();
    other.unsigned_tx.output[0].value = bitcoin::Amount::from_sat(80_000);
    let first = psbt.to_string();
    let second = other.to_string();
    let (first, second) = tokio::join!(service.submit(&first, time), service.submit(&second, time));

    assert!(matches!(
        (first, second),
        (Ok(_), Err(AppError::Conflict(_))) | (Err(AppError::Conflict(_)), Ok(_))
    ));
}

#[tokio::test]
async fn same_transaction_can_be_uploaded_again() {
    let (_, service, _, psbt) = setup();
    let time = SystemTime::now();
    let first = service.submit(&psbt.to_string(), time).await.unwrap();
    let again = service.submit(&psbt.to_string(), time).await.unwrap();

    assert_eq!(first.txid, again.txid);
    assert_eq!(again.added, 0);
}

async fn request(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: serde_json::Value,
    token: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");

    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }

    let result = app
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = result.status();
    let bytes = result.into_body().collect().await.unwrap().to_bytes();

    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn malformed_bearer_returns_unauthorized() {
    let (_, service, _, _) = setup();
    let app = handlers::router(service);
    let (status, _) = request(app, "GET", PENDING, serde_json::Value::Null, Some("zz")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn handlers_auth_replay_unauthorized_and_exchange() {
    let (fixture, service, _, psbt) = setup();
    let app = handlers::router(service.clone());
    let (status, _) = request(app.clone(), "GET", PENDING, serde_json::Value::Null, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (_, challenge) = request(
        app.clone(),
        "POST",
        CHALLENGE,
        serde_json::Value::Null,
        None,
    )
    .await;
    let nonce = challenge["nonce"].as_str().unwrap();
    let message = Message::from_digest_slice(
        signed_msg_hash(&sign_in_message("https://test.example", nonce)).as_ref(),
    )
    .unwrap();
    let signature = MessageSignature::new(
        Secp256k1::new().sign_ecdsa_recoverable(&message, &fixture.admin_keypair(0).secret_key()),
        true,
    )
    .to_base64();
    let body = serde_json::json!({"nonce":nonce,"signature":signature});
    let (status, verified) = request(app.clone(), "POST", VERIFY, body.clone(), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(app.clone(), "POST", VERIFY, body, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let token = verified["sessionToken"].as_str().unwrap();
    let (status, pending_view) = request(
        app.clone(),
        "POST",
        PENDING,
        serde_json::json!({"psbt":psbt.to_string()}),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, copy) = request(
        app.clone(),
        "GET",
        &TRANSACTIONS.replace(":txid", pending_view["txid"].as_str().unwrap()),
        serde_json::Value::Null,
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(copy["format"], "psbt");
    for admin in 0..2 {
        let text = signature_text(&fixture, &service, &psbt, admin);
        let (status, result) = request(
            app.clone(),
            "POST",
            &SIGNATURES.replace(":txid", pending_view["txid"].as_str().unwrap()),
            serde_json::json!({"signatures":text}),
            Some(token),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result["pending"].is_null(), admin == 1);
    }
    let (status, past) = request(
        app.clone(),
        "GET",
        PAST,
        serde_json::Value::Null,
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(past[0]["status"], "unconfirmed");
    assert_eq!(
        past[0].get("blockTimeUnixSeconds"),
        Some(&serde_json::Value::Null)
    );
    let (status, _) = request(
        app.clone(),
        "POST",
        &BROADCAST.replace(":txid", pending_view["txid"].as_str().unwrap()),
        serde_json::Value::Null,
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    service
        .auth
        .lock()
        .await
        .expire(&hex::decode(token).unwrap().try_into().unwrap());
    let (status, _) = request(app, "GET", PENDING, serde_json::Value::Null, Some(token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn partial_admin_signatures_do_not_count_and_external_broadcast_is_found() {
    let (fixture, service, blockchain, _) = setup();
    let coin = fee_coin(&fixture, 0, 0, 100_000);
    let connectors: Vec<_> = (1..=2)
        .map(|i| {
            block_payouts_tx::ConnectorRef::new(fixture.game(&service.deployment(), i).claim_txid())
        })
        .collect();
    let mut psbt = block_payouts_tx::build_psbt(
        &service.deployment(),
        &connectors,
        std::slice::from_ref(&coin),
        &change(&fixture, 90_000),
    )
    .unwrap();
    sign_fee(&fixture, &mut psbt, &coin, 0);
    let mut signed = psbt.clone();
    sign_as(&fixture, &service.deployment(), &mut signed, 0);
    psbt.inputs[0] = signed.inputs[0].clone();
    let pending_view = service
        .submit(&psbt.to_string(), SystemTime::now())
        .await
        .unwrap();
    assert_eq!(pending_view.claim_input_count, 2);
    assert_eq!(pending_view.admins_signed, 0);
    assert!(pending_view.signed_by.is_empty());
    sign_as(&fixture, &service.deployment(), &mut signed, 1);
    let transaction = block_payouts_tx::finalize(&service.deployment(), &signed).unwrap();
    blockchain.history.lock().await.push((transaction, None));
    service.tick(SystemTime::now()).await.unwrap();
    assert_eq!(
        service
            .get(&pending_view.txid.to_string())
            .await
            .unwrap()
            .past
            .unwrap()
            .status,
        PastStatus::Unconfirmed
    );
    assert!(blockchain.broadcasts.lock().await.is_empty());
}

#[tokio::test]
async fn database_rejects_stale_signature_and_expiry_writes() {
    let (_, service, _, psbt) = setup();
    let id = service
        .submit(&psbt.to_string(), SystemTime::now())
        .await
        .unwrap()
        .txid
        .to_string();
    let snapshot = service.get(&id).await.unwrap();
    let mut updated = snapshot.clone();
    updated.version += 1;
    updated.expires_at = deadline(updated.expires_at, Duration::from_secs(1)).unwrap();
    assert!(service
        .database
        .save(&updated, Some(snapshot.version))
        .await
        .unwrap());
    assert!(!service
        .database
        .save(&snapshot, Some(snapshot.version))
        .await
        .unwrap());
    service.database.delete(&snapshot).await.unwrap();
    assert_eq!(
        service.get(&id).await.unwrap().expires_at,
        updated.expires_at
    );
}

#[tokio::test]
async fn foreign_key_is_rejected() {
    let (_, service, _, _) = setup();
    let foreign = Fixture::new(1, 1);
    // Fixture key zero is shared: choose a secret not belonging to any fixture admin.
    let secret = bitcoin::secp256k1::SecretKey::from_slice(&[42; 32]).unwrap();
    assert_ne!(foreign.admin_keypair(0).secret_key(), secret);
    let app = handlers::router(service);
    let (_, challenge) = request(
        app.clone(),
        "POST",
        CHALLENGE,
        serde_json::Value::Null,
        None,
    )
    .await;
    let nonce = challenge["nonce"].as_str().unwrap();
    let message = Message::from_digest_slice(
        signed_msg_hash(&sign_in_message("https://test.example", nonce)).as_ref(),
    )
    .unwrap();
    let signature = MessageSignature::new(
        Secp256k1::new().sign_ecdsa_recoverable(&message, &secret),
        true,
    )
    .to_base64();
    let (status, _) = request(
        app,
        "POST",
        VERIFY,
        serde_json::json!({"nonce":nonce,"signature":signature}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn refresh_fetches_shared_connector_history_once() {
    let (fixture, service, blockchain, _) = setup();
    let psbt = block_payouts_tx::build_psbt(
        &service.deployment(),
        &[
            block_payouts_tx::ConnectorRef::new(bitcoin::Txid::from_byte_array([41; 32])),
            block_payouts_tx::ConnectorRef::new(bitcoin::Txid::from_byte_array([42; 32])),
        ],
        &[fee_coin(&fixture, 0, 0, 100_000)],
        &change(&fixture, 90_000),
    )
    .unwrap();
    service
        .submit(&psbt.to_string(), SystemTime::now())
        .await
        .unwrap();
    assert_eq!(
        blockchain
            .history_calls
            .load(std::sync::atomic::Ordering::Relaxed),
        2
    );
}

#[tokio::test]
async fn failed_asm_refresh_keeps_the_previous_configuration() {
    let (fixture, service, _, _) = setup();
    let before = service.deployment.borrow().clone();
    let roster = payout_admin_core::SignerRoster::from_toml(&fixture.roster_toml()).unwrap();
    // Port 1 refuses connections, so the ASM fetch fails.
    service
        .refresh_deployment("http://127.0.0.1:1", &roster)
        .await;
    assert!(Arc::ptr_eq(&before, &service.deployment.borrow()));
}
