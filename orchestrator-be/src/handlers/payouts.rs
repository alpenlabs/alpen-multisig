use crate::{
    application::payouts::PayoutConfigView, error::Result,
    handlers::auth_session::PayoutAdminSession, state::AppState,
};
use axum::{routing::get, Json, Router};
pub(crate) fn router() -> Router<AppState> {
    Router::new().route("/payouts/config", get(get_config))
}
pub async fn get_config(auth: PayoutAdminSession) -> Result<Json<PayoutConfigView>> {
    Ok(Json(auth.service.config()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use bitcoin::hashes::Hash;
    use bitcoin::secp256k1::{Message, Secp256k1};
    use bitcoin::sign_message::signed_msg_hash;
    use http_body_util::BodyExt;
    use payout_admin_core::{test_support as ts, Deployment};
    use serde_json::{json, Value};
    use tower::util::ServiceExt;

    use crate::application::payouts::PayoutService;
    use crate::error::AppError;
    use crate::infrastructure::bitcoin_rpc::BitcoinRpcClient;
    use crate::infrastructure::memory_repo::InMemoryProposalRepository;
    use crate::state::AppState;

    struct Node;
    impl Default for Node {
        fn default() -> Self {
            Self
        }
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
    }
    struct Harness {
        app: Router,
        fx: ts::Fixture,
        dep: Deployment,
    }
    fn harness() -> Harness {
        let fx = ts::Fixture::new(3, 2);
        let dep = fx.deployment();
        let node = Arc::new(Node::default());
        let service = PayoutService::new(Arc::new(dep.clone()));
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
            app: super::super::router(state),
            fx,
            dep,
        }
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
        for uri in ["/payouts/config"] {
            let (s, _) = call(&h.app, req("GET", uri, None, Some(gov))).await;
            assert_eq!(s, StatusCode::UNAUTHORIZED, "{uri}");
        }
        let (s, _) = call(&h.app, req("GET", "/payouts/config", None, None)).await;
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
}
