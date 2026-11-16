//! `/payouts/*`: Admin Block Payouts coordination for Payout Administrator sessions.
//!
//! Every route needs a Payout Administrator session; any other session, or a coordinator started
//! without `PAYOUT_ADMIN_PARAMS_PATH`, gets the same 401 so nothing about pending payouts leaks.

use axum::{
    extract::Path,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

use crate::{
    application::payouts::{
        PastPayoutView, PasteSignaturesView, PayoutConfigView, PendingPayoutView, RefreshOutcome,
    },
    error::Result,
    handlers::auth_session::PayoutAdminSession,
    state::AppState,
};

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        // Admin Block Payouts (Payout Administrators; no ASM involved)
        .route("/payouts/config", get(get_config))
        .route("/payouts/pending", get(list_pending))
        .route("/payouts/pending", post(submit_pending))
        .route("/payouts/pending/:txid", get(get_pending))
        .route("/payouts/pending/:txid/signatures", post(paste_signatures))
        .route("/payouts/pending/:txid/broadcast", post(report_broadcast))
        .route("/payouts/pending/:txid/refresh", post(refresh_pending))
        .route("/payouts/past", get(list_past))
        .route("/payouts/expired", get(list_expired))
}

#[derive(Debug, Deserialize)]
pub struct SubmitPayoutRequest {
    /// Base64 PSBT: a new transaction, or a copy of a pending one carrying more signatures.
    pub psbt: String,
}

#[derive(Debug, Deserialize)]
pub struct PasteSignaturesRequest {
    pub signatures: String,
}

#[derive(Debug, Deserialize)]
pub struct BroadcastResultRequest {
    /// `None` when the node accepted the transaction.
    pub error: Option<String>,
}

pub async fn get_config(auth: PayoutAdminSession) -> Result<Json<PayoutConfigView>> {
    Ok(Json(auth.service.config()))
}

pub async fn list_pending(auth: PayoutAdminSession) -> Result<Json<Vec<PendingPayoutView>>> {
    Ok(Json(auth.service.list_pending(chrono::Utc::now()).await?))
}

pub async fn get_pending(
    auth: PayoutAdminSession,
    Path(txid): Path<String>,
) -> Result<Json<PendingPayoutView>> {
    Ok(Json(
        auth.service.get_pending(&txid, chrono::Utc::now()).await?,
    ))
}

pub async fn submit_pending(
    auth: PayoutAdminSession,
    Json(body): Json<SubmitPayoutRequest>,
) -> Result<(StatusCode, Json<PendingPayoutView>)> {
    let view = auth
        .service
        .submit(&body.psbt, &auth.admin_xonly, chrono::Utc::now())
        .await?;
    Ok((StatusCode::OK, Json(view)))
}

pub async fn paste_signatures(
    auth: PayoutAdminSession,
    Path(txid): Path<String>,
    Json(body): Json<PasteSignaturesRequest>,
) -> Result<Json<PasteSignaturesView>> {
    Ok(Json(
        auth.service
            .paste_signatures(&txid, &body.signatures, chrono::Utc::now())
            .await?,
    ))
}

pub async fn report_broadcast(
    auth: PayoutAdminSession,
    Path(txid): Path<String>,
    Json(body): Json<BroadcastResultRequest>,
) -> Result<Json<RefreshOutcome>> {
    Ok(Json(
        auth.service
            .report_broadcast(&txid, body.error, chrono::Utc::now())
            .await?,
    ))
}

pub async fn refresh_pending(
    auth: PayoutAdminSession,
    Path(txid): Path<String>,
) -> Result<Json<RefreshOutcome>> {
    Ok(Json(auth.service.refresh(&txid, chrono::Utc::now()).await?))
}

pub async fn list_past(auth: PayoutAdminSession) -> Result<Json<Vec<PastPayoutView>>> {
    Ok(Json(auth.service.list_past().await?))
}

pub async fn list_expired(auth: PayoutAdminSession) -> Result<Json<Vec<String>>> {
    Ok(Json(auth.service.list_expired().await?))
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use bitcoin::hashes::Hash;
    use bitcoin::secp256k1::{Message, Secp256k1};
    use bitcoin::sign_message::signed_msg_hash;
    use http_body_util::BodyExt;
    use payout_admin_core::{payout_tx, test_support as ts, Deployment};
    use serde_json::{json, Value};
    use tower::util::ServiceExt;

    use crate::application::payouts::PayoutService;
    use crate::error::AppError;
    use crate::infrastructure::bitcoin_rpc::BitcoinRpcClient;
    use crate::infrastructure::memory_repo::InMemoryProposalRepository;
    use crate::infrastructure::payout_repo::InMemoryPayoutRepository;
    use crate::state::AppState;

    /// A node whose view the test sets.
    #[derive(Default)]
    struct Node {
        known: Mutex<HashSet<String>>,
        transactions: Mutex<HashMap<String, bitcoin::Transaction>>,
        spent: Mutex<HashSet<String>>,
    }

    #[async_trait]
    impl BitcoinRpcClient for Node {
        async fn estimate_fee_rate_sats_per_vb(&self, _t: u16) -> Result<u64, AppError> {
            Ok(1)
        }
        async fn get_block_height_for_txid(&self, _txid: &str) -> Result<u64, AppError> {
            Err(AppError::NotFound)
        }
        async fn get_chain_tip(&self) -> Result<u64, AppError> {
            Ok(1)
        }
        async fn tx_out_unspent(&self, txid: &str, vout: u32, _m: bool) -> Result<bool, AppError> {
            Ok(!self
                .spent
                .lock()
                .unwrap()
                .contains(&format!("{txid}:{vout}")))
        }
        async fn payout_transaction(
            &self,
            txid: &str,
        ) -> Result<Option<bitcoin::Transaction>, AppError> {
            Ok(self.transactions.lock().unwrap().get(txid).cloned())
        }
        async fn tx_known(&self, txid: &str) -> Result<bool, AppError> {
            Ok(self.known.lock().unwrap().contains(txid))
        }
    }

    use crate::application::payouts::PayoutRepository;

    struct Harness {
        app: Router,
        fx: ts::Fixture,
        dep: Deployment,
        node: Arc<Node>,
        repo: Arc<InMemoryPayoutRepository>,
    }

    fn harness_with(expiry: chrono::Duration) -> Harness {
        let fx = ts::Fixture::new(3, 2);
        let dep = fx.deployment();
        let node = Arc::new(Node::default());
        let repo = Arc::new(InMemoryPayoutRepository::new());
        let service = PayoutService::new(Arc::new(dep.clone()), repo.clone(), node.clone())
            .with_expiry(expiry);
        let state = AppState::new(
            Arc::new(InMemoryProposalRepository::new()),
            "mock://asm-membership".to_string(),
            120_000,
            240_000,
            node.clone(),
            7,
            bitcoin::Network::Regtest,
        )
        .with_payouts(Some(Arc::new(service)));
        Harness {
            repo,
            app: super::super::router(state),
            fx,
            dep,
            node,
        }
    }

    fn harness() -> Harness {
        harness_with(chrono::Duration::days(4))
    }

    fn req(method: &str, uri: &str, body: Option<Value>, bearer: Option<&str>) -> Request<Body> {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(t) = bearer {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        b.body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
            .unwrap()
    }

    async fn call(app: &Router, r: Request<Body>) -> (StatusCode, Value) {
        let resp = app.clone().oneshot(r).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Signs in the way the desktop does with a Ledger: BIP-137 message signature by the admin key
    /// at account/0/0.
    async fn login_with(
        h: &Harness,
        authority: &str,
        key: &bitcoin::secp256k1::Keypair,
    ) -> (StatusCode, Value) {
        let (s, ch) = call(
            &h.app,
            req(
                "POST",
                "/auth/challenge",
                Some(json!({ "authority": authority })),
                None,
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let message = ch["challenge_message"].as_str().unwrap();
        let digest = signed_msg_hash(message);
        let msg = Message::from_digest(digest.to_byte_array());
        let sig = Secp256k1::new().sign_ecdsa_recoverable(&msg, &key.secret_key());
        let (recid, compact) = sig.serialize_compact();
        let mut sig_bytes = compact.to_vec();
        sig_bytes.push(recid.to_i32() as u8);
        call(
            &h.app,
            req(
                "POST",
                "/auth/verify",
                Some(json!({
                    "challenge_id": ch["challenge_id"],
                    "signer_pubkey": key.public_key().to_string(),
                    "signature_hex": hex::encode(sig_bytes),
                    "signature_format": "bitcoin-message",
                })),
                None,
            ),
        )
        .await
    }

    async fn login(h: &Harness, admin: usize) -> String {
        let (s, body) = login_with(h, "payout_admin", &h.fx.admin_keypair(admin)).await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
        body["token"].as_str().unwrap().to_string()
    }

    fn unsigned_psbt(h: &Harness, variants: &[u32]) -> (bitcoin::psbt::Psbt, payout_tx::FeeInput) {
        let coin = ts::fee_coin(&h.fx, 0, 3, 40_000);
        let claims: Vec<_> = variants
            .iter()
            .map(|&v| payout_tx::ConnectorRef {
                claim_txid: h.fx.game(&h.dep, v).report.new_claim_txid,
                covenant_index: 0,
            })
            .collect();
        let psbt = payout_tx::build_psbt(
            &h.dep,
            &claims,
            std::slice::from_ref(&coin),
            &ts::change(&h.fx, 39_000),
        )
        .unwrap();
        (psbt, coin)
    }

    #[tokio::test]
    async fn only_params_admins_sign_in() {
        let h = harness();
        login(&h, 2).await;
        let stranger =
            bitcoin::secp256k1::Keypair::from_seckey_slice(&Secp256k1::new(), &[9u8; 32]).unwrap();
        let (s, _) = login_with(&h, "payout_admin", &stranger).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn sessions_do_not_cross_between_governance_and_payouts() {
        let h = harness();
        let payout = login(&h, 0).await;
        let (s, _) = call(&h.app, req("GET", "/proposals", None, Some(&payout))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        // A governance (ASM mock) session cannot read payouts.
        let sk = bitcoin::secp256k1::Keypair::from_seckey_slice(&Secp256k1::new(), &{
            let mut b = [0u8; 32];
            b[31] = 1;
            b
        })
        .unwrap();
        let (s, body) = login_with(&h, "strata_admin", &sk).await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
        let gov = body["token"].as_str().unwrap();
        for uri in ["/payouts/pending", "/payouts/past", "/payouts/config"] {
            let (s, _) = call(&h.app, req("GET", uri, None, Some(gov))).await;
            assert_eq!(s, StatusCode::UNAUTHORIZED, "{uri}");
        }
        let (s, _) = call(&h.app, req("GET", "/payouts/pending", None, None)).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn config_names_the_deployment() {
        let h = harness();
        let t = login(&h, 0).await;
        let (s, c) = call(&h.app, req("GET", "/payouts/config", None, Some(&t))).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(h.dep.threshold, 2);
        assert_eq!(c["deployment_fingerprint"], h.dep.fingerprint());
        assert_eq!(c["expiry_days"], 4);
    }

    #[tokio::test]
    async fn pending_lifecycle_merge_paste_broadcast() {
        let h = harness();
        let a0 = login(&h, 0).await;
        let a1 = login(&h, 1).await;
        let (mut psbt, coin) = unsigned_psbt(&h, &[0, 1]);
        let txid = psbt.unsigned_tx.compute_txid().to_string();
        ts::sign_as(&h.fx, &h.dep, &mut psbt, 0);
        ts::sign_fee(&h.fx, &mut psbt, &coin, 3);
        let (s, v) = call(
            &h.app,
            req(
                "POST",
                "/payouts/pending",
                Some(json!({ "psbt": psbt.to_string() })),
                Some(&a0),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["txid"], txid);
        let analysis =
            payout_tx::analyze(&h.dep, &v["psbt"].as_str().unwrap().parse().unwrap()).unwrap();
        assert_eq!(analysis.connector_inputs.len(), 2);
        assert_eq!(v["signatures_received"], 1);
        assert_eq!(v["signatures_needed"], 2);
        assert!(!analysis.has_quorum);
        assert!(h
            .repo
            .get_pending(&txid)
            .await
            .unwrap()
            .unwrap()
            .first_signature_at
            .is_some());
        let created_by = h.fx.admin_xonly(0).to_string();
        assert_eq!(v["created_by"], created_by);

        // Another admin sees it.
        let (_, list) = call(&h.app, req("GET", "/payouts/pending", None, Some(&a1))).await;
        assert_eq!(list.as_array().unwrap().len(), 1);

        // Admin 2 pastes signatures exported by admin 2's device, plus one invalid.
        let mut other = bitcoin::psbt::Psbt::from_str(v["psbt"].as_str().unwrap()).unwrap();
        ts::sign_as(&h.fx, &h.dep, &mut other, 2);
        let lines = payout_tx::export_signatures(&h.dep, &other).unwrap();
        let mine: Vec<_> = lines
            .iter()
            .filter(|l| l.admin_key == h.fx.admin_xonly(2).to_string())
            .cloned()
            .collect();
        let bad = format!("{}:0:{}:{}", txid, h.fx.admin_xonly(1), "11".repeat(64));
        let text = format!("{}\n{bad}", payout_tx::signatures_text(&mine));
        let (s, p) = call(
            &h.app,
            req(
                "POST",
                &format!("/payouts/pending/{txid}/signatures"),
                Some(json!({ "signatures": text })),
                Some(&a1),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{p}");
        assert_eq!(p["added"], 2);
        assert_eq!(p["invalid"].as_array().unwrap().len(), 1);
        assert!(p["message"]
            .as_str()
            .unwrap()
            .starts_with("Warning: Invalid signature."));
        let analysis = payout_tx::analyze(
            &h.dep,
            &p["pending"]["psbt"].as_str().unwrap().parse().unwrap(),
        )
        .unwrap();
        assert!(analysis.has_quorum);
        assert!(analysis.complete);

        // Broadcast failed: kept, error shown.
        let (s, _) = call(
            &h.app,
            req(
                "POST",
                &format!("/payouts/pending/{txid}/broadcast"),
                Some(json!({ "error": "min relay fee not met" })),
                Some(&a1),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let (_, v) = call(
            &h.app,
            req("GET", &format!("/payouts/pending/{txid}"), None, Some(&a0)),
        )
        .await;
        assert_eq!(v["broadcast_error"], "min relay fee not met");

        let error = format!("{}€", "a".repeat(999));
        let (s, _) = call(
            &h.app,
            req(
                "POST",
                &format!("/payouts/pending/{txid}/broadcast"),
                Some(json!({ "error": error })),
                Some(&a0),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let (_, v) = call(
            &h.app,
            req("GET", &format!("/payouts/pending/{txid}"), None, Some(&a0)),
        )
        .await;
        assert_eq!(v["broadcast_error"], "a".repeat(999));

        // A success report alone is not evidence of node acceptance.
        let (s, _) = call(
            &h.app,
            req(
                "POST",
                &format!("/payouts/pending/{txid}/broadcast"),
                Some(json!({})),
                Some(&a0),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (_, pending) = call(&h.app, req("GET", "/payouts/pending", None, Some(&a0))).await;
        assert_eq!(pending.as_array().unwrap().len(), 1);
        let (_, past) = call(&h.app, req("GET", "/payouts/past", None, Some(&a0))).await;
        assert!(past.as_array().unwrap().is_empty());
        h.node.known.lock().unwrap().insert(txid.clone());

        // Retry succeeded: moved to past with the finalized transaction.
        let (s, o) = call(
            &h.app,
            req(
                "POST",
                &format!("/payouts/pending/{txid}/broadcast"),
                Some(json!({})),
                Some(&a0),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(o["outcome"], "broadcast");
        let (_, list) = call(&h.app, req("GET", "/payouts/pending", None, Some(&a0))).await;
        assert!(list.as_array().unwrap().is_empty());
        let (_, past) = call(&h.app, req("GET", "/payouts/past", None, Some(&a0))).await;
        let past = past.as_array().unwrap();
        assert_eq!(past.len(), 1);
        let raw: bitcoin::Transaction =
            bitcoin::consensus::encode::deserialize_hex(past[0]["raw_tx"].as_str().unwrap())
                .unwrap();
        assert_eq!(raw.compute_txid().to_string(), txid);
        assert!(raw.input.iter().all(|i| !i.witness.is_empty()));

        // Re-submitting a broadcast transaction is refused.
        let (s, _) = call(
            &h.app,
            req(
                "POST",
                "/payouts/pending",
                Some(json!({ "psbt": psbt.to_string() })),
                Some(&a0),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn submitting_a_signed_copy_merges() {
        let h = harness();
        let a0 = login(&h, 0).await;
        let (base, _) = unsigned_psbt(&h, &[0]);
        let mut p0 = base.clone();
        ts::sign_as(&h.fx, &h.dep, &mut p0, 0);
        let mut p1 = base.clone();
        ts::sign_as(&h.fx, &h.dep, &mut p1, 1);
        for p in [&p0, &p1] {
            let (s, _) = call(
                &h.app,
                req(
                    "POST",
                    "/payouts/pending",
                    Some(json!({ "psbt": p.to_string() })),
                    Some(&a0),
                ),
            )
            .await;
            assert_eq!(s, StatusCode::OK);
        }
        let (_, list) = call(&h.app, req("GET", "/payouts/pending", None, Some(&a0))).await;
        let analysis =
            payout_tx::analyze(&h.dep, &list[0]["psbt"].as_str().unwrap().parse().unwrap())
                .unwrap();
        assert_eq!(analysis.signers, vec![0, 1]);
        // Not complete: the fee input is unsigned, so broadcast is refused.
        assert!(!analysis.complete);
        let txid = list[0]["txid"].as_str().unwrap();
        let (s, _) = call(
            &h.app,
            req(
                "POST",
                &format!("/payouts/pending/{txid}/broadcast"),
                Some(json!({})),
                Some(&a0),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn duplicates_are_marked() {
        let h = harness();
        let a0 = login(&h, 0).await;
        let (p1, _) = unsigned_psbt(&h, &[0, 1]);
        let coin = ts::fee_coin(&h.fx, 0, 5, 30_000);
        let claim = payout_tx::ConnectorRef {
            claim_txid: h.fx.game(&h.dep, 1).report.new_claim_txid,
            covenant_index: 0,
        };
        let p2 =
            payout_tx::build_psbt(&h.dep, &[claim], &[coin], &ts::change(&h.fx, 29_000)).unwrap();
        for p in [&p1, &p2] {
            call(
                &h.app,
                req(
                    "POST",
                    "/payouts/pending",
                    Some(json!({ "psbt": p.to_string() })),
                    Some(&a0),
                ),
            )
            .await;
        }
        let (_, list) = call(&h.app, req("GET", "/payouts/pending", None, Some(&a0))).await;
        let list = list.as_array().unwrap();
        assert_eq!(list.len(), 2);
        let dup = claim.outpoint().to_string();
        for v in list {
            assert_eq!(v["duplicate_outpoints"], json!([dup]));
        }
    }

    #[tokio::test]
    async fn rejects_foreign_transactions() {
        let h = harness();
        let a0 = login(&h, 0).await;
        let (mut p, _) = unsigned_psbt(&h, &[0]);
        p.unsigned_tx.version = bitcoin::transaction::Version::TWO;
        let (s, _) = call(
            &h.app,
            req(
                "POST",
                "/payouts/pending",
                Some(json!({ "psbt": p.to_string() })),
                Some(&a0),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _) = call(
            &h.app,
            req(
                "POST",
                "/payouts/pending",
                Some(json!({ "psbt": "not a psbt" })),
                Some(&a0),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn expires_after_the_window() {
        let h = harness_with(chrono::Duration::milliseconds(-1));
        let a0 = login(&h, 0).await;
        let (mut p, _) = unsigned_psbt(&h, &[0]);
        ts::sign_as(&h.fx, &h.dep, &mut p, 0);
        let (s, v) = call(
            &h.app,
            req(
                "POST",
                "/payouts/pending",
                Some(json!({ "psbt": p.to_string() })),
                Some(&a0),
            ),
        )
        .await;
        // Saved, but already past its (test) window: gone on the next read.
        assert_eq!(s, StatusCode::OK, "{v}");
        let (_, list) = call(&h.app, req("GET", "/payouts/pending", None, Some(&a0))).await;
        assert!(list.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn refresh_removes_when_an_input_is_spent_elsewhere() {
        let h = harness();
        let a0 = login(&h, 0).await;
        let (p, _) = unsigned_psbt(&h, &[0, 1]);
        let txid = p.unsigned_tx.compute_txid().to_string();
        call(
            &h.app,
            req(
                "POST",
                "/payouts/pending",
                Some(json!({ "psbt": p.to_string() })),
                Some(&a0),
            ),
        )
        .await;
        let uri = format!("/payouts/pending/{txid}/refresh");
        let (_, o) = call(&h.app, req("POST", &uri, None, Some(&a0))).await;
        assert_eq!(o["outcome"], "unchanged");
        let spent = p.unsigned_tx.input[1].previous_output.to_string();
        h.node.spent.lock().unwrap().insert(spent.clone());
        let (_, o) = call(&h.app, req("POST", &uri, None, Some(&a0))).await;
        assert_eq!(o["outcome"], "expired");
        assert_eq!(o["spent_outpoint"], spent);
        let (s, _) = call(
            &h.app,
            req("GET", &format!("/payouts/pending/{txid}"), None, Some(&a0)),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn refresh_moves_a_broadcast_transaction_to_past() {
        let h = harness();
        let a0 = login(&h, 0).await;
        let (mut p, coin) = unsigned_psbt(&h, &[0]);
        ts::sign_as(&h.fx, &h.dep, &mut p, 0);
        ts::sign_as(&h.fx, &h.dep, &mut p, 1);
        ts::sign_fee(&h.fx, &mut p, &coin, 3);
        let txid = p.unsigned_tx.compute_txid().to_string();
        call(
            &h.app,
            req(
                "POST",
                "/payouts/pending",
                Some(json!({ "psbt": p.to_string() })),
                Some(&a0),
            ),
        )
        .await;
        h.node
            .transactions
            .lock()
            .unwrap()
            .insert(txid.clone(), payout_tx::finalize(&h.dep, &p).unwrap());
        h.node.known.lock().unwrap().insert(txid.clone());
        // Its inputs are spent too — by itself, which must not count as "elsewhere".
        for i in &p.unsigned_tx.input {
            h.node
                .spent
                .lock()
                .unwrap()
                .insert(i.previous_output.to_string());
        }
        let (_, o) = call(
            &h.app,
            req(
                "POST",
                &format!("/payouts/pending/{txid}/refresh"),
                None,
                Some(&a0),
            ),
        )
        .await;
        assert_eq!(o["outcome"], "broadcast");
        let (_, past) = call(&h.app, req("GET", "/payouts/past", None, Some(&a0))).await;
        assert_eq!(past[0]["txid"], txid);
    }

    #[tokio::test]
    async fn refresh_recovers_offline_completion() {
        let h = harness();
        let a0 = login(&h, 0).await;
        let (mut p, coin) = unsigned_psbt(&h, &[0]);
        ts::sign_as(&h.fx, &h.dep, &mut p, 0);
        ts::sign_fee(&h.fx, &mut p, &coin, 3);
        let txid = p.unsigned_tx.compute_txid().to_string();
        call(
            &h.app,
            req(
                "POST",
                "/payouts/pending",
                Some(json!({ "psbt": p.to_string() })),
                Some(&a0),
            ),
        )
        .await;
        ts::sign_as(&h.fx, &h.dep, &mut p, 1);
        h.node.known.lock().unwrap().insert(txid.clone());
        h.node
            .transactions
            .lock()
            .unwrap()
            .insert(txid.clone(), payout_tx::finalize(&h.dep, &p).unwrap());
        // Its inputs are spent too — by itself, which must not count as "elsewhere".
        for i in &p.unsigned_tx.input {
            h.node
                .spent
                .lock()
                .unwrap()
                .insert(i.previous_output.to_string());
        }
        let (_, o) = call(
            &h.app,
            req(
                "POST",
                &format!("/payouts/pending/{txid}/refresh"),
                None,
                Some(&a0),
            ),
        )
        .await;
        assert_eq!(o["outcome"], "broadcast");
        let (_, past) = call(&h.app, req("GET", "/payouts/past", None, Some(&a0))).await;
        assert_eq!(past[0]["txid"], txid);
    }

    #[tokio::test]
    async fn expired_signed_psbt_cannot_restart_clock() {
        for sweep in [false, true] {
            let h = harness();
            let repo = Arc::new(InMemoryPayoutRepository::new());
            let service = PayoutService::new(Arc::new(h.dep.clone()), repo.clone(), h.node.clone());
            let (mut p, _) = unsigned_psbt(&h, &[0]);
            ts::sign_as(&h.fx, &h.dep, &mut p, 0);
            let now = chrono::Utc::now();
            service.submit(&p.to_string(), "admin", now).await.unwrap();
            let later = now + chrono::Duration::days(5);
            if sweep {
                assert!(service.list_pending(later).await.unwrap().is_empty());
            }
            assert!(matches!(
                service.submit(&p.to_string(), "admin", later).await,
                Err(AppError::Conflict(_))
            ));
            assert!(service.list_pending(later).await.unwrap().is_empty());
            // The app reads which ids these are, to build a different transaction next time.
            assert_eq!(
                service.list_expired().await.unwrap(),
                vec![p.unsigned_tx.compute_txid().to_string()]
            );
        }
    }

    #[tokio::test]
    async fn missing_txindex_preserves_confirmed_payout() {
        use crate::application::payouts::PayoutRepository;
        use crate::infrastructure::bitcoin_rpc::HttpBitcoinRpcClient;
        for index in [json!({}), json!({"txindex": {"synced": false}})] {
            let rpc = Router::new().route("/", axum::routing::post(
                move |axum::Json(request): axum::Json<Value>| {
                    let index = index.clone();
                    async move {
                        axum::Json(match request["method"].as_str().unwrap() {
                            "getindexinfo" => json!({"result": index, "error": null}),
                            "getrawtransaction" => json!({"result": null,
                                "error": {"code": -5, "message": "No such mempool or blockchain transaction. Use -txindex"}}),
                            "gettxout" => json!({"result": null, "error": null}),
                            method => panic!("unexpected RPC {method}"),
                        })
                    }
                }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                axum::serve(listener, rpc).await.unwrap();
            });
            let h = harness();
            let repo = Arc::new(InMemoryPayoutRepository::new());
            let service = PayoutService::new(
                Arc::new(h.dep.clone()),
                repo.clone(),
                Arc::new(HttpBitcoinRpcClient::new(&url, "", "")),
            );
            let (mut p, coin) = unsigned_psbt(&h, &[0]);
            ts::sign_as(&h.fx, &h.dep, &mut p, 0);
            ts::sign_as(&h.fx, &h.dep, &mut p, 1);
            ts::sign_fee(&h.fx, &mut p, &coin, 3);
            let now = chrono::Utc::now();
            let txid = p.unsigned_tx.compute_txid().to_string();
            service.submit(&p.to_string(), "admin", now).await.unwrap();
            assert!(service.refresh(&txid, now).await.is_err());
            assert!(repo.get_pending(&txid).await.unwrap().is_some());
            // Past its deadline it can be neither expired nor recorded as long as the node cannot
            // say what happened to it. It is kept and still listed, so that one such payout does
            // not hide all the others, and using it directly is refused.
            let later = now + chrono::Duration::days(5);
            let listed = service.list_pending(later).await.unwrap();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].txid, txid);
            assert!(service.get_pending(&txid, later).await.is_err());
            assert!(repo.get_pending(&txid).await.unwrap().is_some());
            assert!(service.list_expired().await.unwrap().is_empty());
            server.abort();
        }
    }

    use std::str::FromStr;
}
