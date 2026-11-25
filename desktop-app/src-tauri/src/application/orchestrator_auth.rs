use std::sync::{Mutex, OnceLock};

use crate::application::orchestrator_client::{
    CompleteOrchestratorAuthRequest, OrchestratorAuthChallenge, OrchestratorAuthSession,
    OrchestratorClient, StartOrchestratorAuthRequest,
};
use crate::infrastructure::challenge_verifier;
use crate::infrastructure::orchestrator_client::HttpOrchestratorClient;

#[derive(Default)]
struct OrchestratorAuthState {
    session: Option<OrchestratorAuthSession>,
    /// The orchestrator the session was obtained from.
    base_url: Option<String>,
}

fn state() -> &'static Mutex<OrchestratorAuthState> {
    static STATE: OnceLock<Mutex<OrchestratorAuthState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(OrchestratorAuthState::default()))
}

pub async fn start_challenge(
    base_url: &str,
    authority: &str,
) -> Result<OrchestratorAuthChallenge, String> {
    let client = HttpOrchestratorClient::new(base_url.to_string());
    let mut challenge = client
        .auth_challenge(StartOrchestratorAuthRequest {
            authority: authority.to_string(),
        })
        .await
        .map_err(|e| e.to_string())?;

    // The orchestrator is not trusted to choose bytes for the Admin ID to sign. Reconstruct the
    // versioned authentication transcript locally and require the response to match it exactly
    // before any frontend or wallet adapter can see the message.
    challenge.challenge_message = validated_challenge_message(authority, &challenge)?;
    Ok(challenge)
}

fn validated_challenge_message(
    authority: &str,
    challenge: &OrchestratorAuthChallenge,
) -> Result<String, String> {
    if challenge.challenge_hex.len() != 64
        || !challenge
            .challenge_hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(
            "orchestrator authentication challenge must be 32-byte lowercase hex".to_string(),
        );
    }

    let expected =
        challenge_verifier::render_challenge_message(authority, &challenge.challenge_hex);
    if challenge.challenge_message != expected {
        return Err(
            "orchestrator authentication challenge did not match the client-defined transcript"
                .to_string(),
        );
    }
    Ok(expected)
}

pub async fn complete_auth(
    base_url: &str,
    request: CompleteOrchestratorAuthRequest,
) -> Result<OrchestratorAuthSession, String> {
    let client = HttpOrchestratorClient::new(base_url.to_string());
    let session = client
        .auth_verify(request)
        .await
        .map_err(|e| e.to_string())?;
    let mut lock = state()
        .lock()
        .map_err(|_| "orchestrator auth state lock poisoned".to_string())?;
    lock.session = Some(session.clone());
    lock.base_url = Some(base_url.to_string());
    Ok(session)
}

/// A client for the orchestrator of the current, unexpired session — for background work that
/// has no request to take a base URL from (the settle loop, #516). `None` without a session.
pub fn authenticated_client() -> Option<HttpOrchestratorClient> {
    let session = get_session().ok()??;
    let base_url = state().lock().ok()?.base_url.clone()?;
    Some(HttpOrchestratorClient::new(base_url).with_bearer_token(session.token))
}

pub fn get_session() -> Result<Option<OrchestratorAuthSession>, String> {
    let mut lock = state()
        .lock()
        .map_err(|_| "orchestrator auth state lock poisoned".to_string())?;

    let expired = lock
        .session
        .as_ref()
        .map(|s| now_unix_ms() >= s.expires_at_unix_ms)
        .unwrap_or(false);

    if expired {
        lock.session = None;
    }

    Ok(lock.session.clone())
}

fn now_unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub async fn logout(base_url: &str) -> Result<(), String> {
    let token = {
        let lock = state()
            .lock()
            .map_err(|_| "orchestrator auth state lock poisoned".to_string())?;
        lock.session
            .as_ref()
            .map(|session| session.token.clone())
            .ok_or_else(|| "no orchestrator session".to_string())?
    };
    let client = HttpOrchestratorClient::new(base_url.to_string()).with_bearer_token(token);
    client.auth_logout().await.map_err(|e| e.to_string())?;
    state()
        .lock()
        .map_err(|_| "orchestrator auth state lock poisoned".to_string())?
        .session = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validated_challenge_message;
    use crate::application::orchestrator_client::OrchestratorAuthChallenge;
    use crate::domain::action::Action;
    use crate::infrastructure::{action_codec, signing};

    fn challenge(message: String) -> OrchestratorAuthChallenge {
        OrchestratorAuthChallenge {
            challenge_id: "challenge-id".to_string(),
            challenge_hex: "ab".repeat(32),
            challenge_message: message,
            issued_at_unix_ms: 1,
            expires_at_unix_ms: 2,
        }
    }

    #[test]
    fn accepts_canonical_authentication_transcript() {
        let message = format!(
            "Strata Session Authentication v1 | Role: strata_admin | Challenge: {}",
            "ab".repeat(32)
        );
        let challenge = challenge(message.clone());

        assert_eq!(
            validated_challenge_message("strata_admin", &challenge),
            Ok(message)
        );
    }

    #[test]
    fn rejects_governance_message_as_authentication_transcript() {
        let action_hex = action_codec::encode_hex(&Action::Defcon1).expect("encode action");
        let governance_message =
            signing::render_signing_message(1, &action_hex, bitcoin::Network::Regtest)
                .expect("render SPS-65 message");
        let challenge = challenge(governance_message);

        assert!(validated_challenge_message("strata_admin", &challenge).is_err());
    }

    #[test]
    fn rejects_delimiter_injection_in_challenge_hex() {
        let mut challenge = challenge(String::new());
        challenge.challenge_hex = format!("{}|Role: alpen_admin", "ab".repeat(32));
        challenge.challenge_message = format!(
            "Strata Session Authentication v1 | Role: strata_admin | Challenge: {}",
            challenge.challenge_hex
        );

        assert!(validated_challenge_message("strata_admin", &challenge).is_err());
    }
}
