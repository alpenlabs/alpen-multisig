use super::*;
use crate::infrastructure::auth_crypto::random_hex;
use axum::{
    extract::{FromRequestParts, State},
    http::request::Parts,
    Json,
};
use bitcoin::{
    base64::{engine::general_purpose::STANDARD, Engine},
    sign_message::{signed_msg_hash, MessageSignature},
};
use rand::RngCore;

/// In-memory expiration times for outstanding challenges and authenticated bearer tokens.
#[derive(Default)]
pub struct Sessions {
    challenges: HashMap<String, SystemTime>,
    tokens: HashMap<[u8; 32], SystemTime>,
}

#[cfg(test)]
impl Sessions {
    pub(super) fn expire(&mut self, token: &[u8; 32]) {
        *self.tokens.get_mut(token).unwrap() = SystemTime::UNIX_EPOCH;
    }
}

/// Issues a single-use nonce for an admin to sign before the challenge expires.
pub async fn challenge(State(service): State<Arc<Service>>) -> Result<Json<ChallengeResponse>> {
    let time = SystemTime::now();
    let mut auth = service.auth.lock().await;
    auth.challenges.retain(|_, expiry| time < *expiry);

    if auth.challenges.len() >= 10_000 {
        return Err(bad("too many outstanding challenges"));
    }

    let nonce = random_hex(32);
    let expires_at = deadline(time, service.challenge_ttl)?;
    auth.challenges.insert(nonce.clone(), expires_at);

    Ok(Json(ChallengeResponse { nonce, expires_at }))
}

/// Consumes the challenge and issues a session if the signature recovers a deployment admin key.
pub async fn verify(
    State(service): State<Arc<Service>>,
    Json(request): Json<VerifyRequest>,
) -> Result<Json<VerifyResponse>> {
    let time = SystemTime::now();
    let mut auth = service.auth.lock().await;

    if !auth
        .challenges
        .remove(&request.nonce)
        .is_some_and(|expiry| time < expiry)
    {
        return Err(AppError::Unauthorized);
    }

    let mut bytes = STANDARD
        .decode(&request.signature)
        .map_err(|_| AppError::Unauthorized)?;

    if bytes.len() != 65 || !(27..=42).contains(&bytes[0]) {
        return Err(AppError::Unauthorized);
    }

    // BIP-137 address classes share recovery IDs; authorization is by recovered x-only key.
    bytes[0] = 31 + (bytes[0] - 27) % 4;
    let key = MessageSignature::from_slice(&bytes)
        .and_then(|signature| {
            signature.recover_pubkey(
                &bitcoin::secp256k1::Secp256k1::new(),
                signed_msg_hash(&sign_in_message(&service.public_origin, &request.nonce)),
            )
        })
        .map_err(|_| AppError::Unauthorized)?
        .inner
        .x_only_public_key()
        .0;

    if service.deployment.admin_index(&key).is_none() {
        return Err(AppError::Unauthorized);
    }

    auth.tokens.retain(|_, expiry| time < *expiry);
    let mut session_token = [0; 32];
    rand::thread_rng().fill_bytes(&mut session_token);
    let expires_at = deadline(time, service.session_ttl)?;
    auth.tokens.insert(session_token, expires_at);

    Ok(Json(VerifyResponse {
        session_token,
        expires_at,
        admin_key: key,
    }))
}

/// Axum extractor that requires a live payout bearer token on a protected route.
pub struct Auth;

#[axum::async_trait]
impl FromRequestParts<Arc<Service>> for Auth {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, service: &Arc<Service>) -> Result<Self> {
        let token = parts
            .headers
            .get("authorization")
            .and_then(|header| header.to_str().ok())
            .and_then(|header| header.strip_prefix("Bearer "))
            .ok_or(AppError::Unauthorized)?;
        let mut credential = [0; 32];
        hex::decode_to_slice(token, &mut credential).map_err(|_| AppError::Unauthorized)?;

        if !service
            .auth
            .lock()
            .await
            .tokens
            .get(&credential)
            .is_some_and(|expiry| SystemTime::now() < *expiry)
        {
            return Err(AppError::Unauthorized);
        }

        Ok(Self)
    }
}
