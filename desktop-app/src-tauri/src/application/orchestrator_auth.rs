use std::sync::{Mutex, OnceLock};

use crate::application::orchestrator_client::{
    CompleteOrchestratorAuthRequest, OrchestratorAuthChallenge, OrchestratorAuthSession,
    OrchestratorClient, StartOrchestratorAuthRequest,
};
use crate::application::orchestrator_url::normalize_orchestrator_base_url;
use crate::infrastructure::challenge_verifier;
use crate::infrastructure::orchestrator_client::HttpOrchestratorClient;
use serde::Serialize;

/// Non-secret session metadata exposed to the renderer. The opaque ID identifies the native
/// session without granting access to its bearer credential.
#[derive(Debug, Clone, Serialize)]
pub struct OrchestratorAuthSessionView {
    pub session_id: String,
    pub authority: String,
    pub signer_pubkey: String,
    pub expires_at_unix_ms: u64,
}

#[derive(Clone)]
struct StoredOrchestratorSession {
    session_id: String,
    base_url: String,
    credentials: OrchestratorAuthSession,
}

impl StoredOrchestratorSession {
    fn view(&self) -> OrchestratorAuthSessionView {
        OrchestratorAuthSessionView {
            session_id: self.session_id.clone(),
            authority: self.credentials.authority.clone(),
            signer_pubkey: self.credentials.signer_pubkey.clone(),
            expires_at_unix_ms: self.credentials.expires_at_unix_ms,
        }
    }

    fn client(&self) -> HttpOrchestratorClient {
        HttpOrchestratorClient::new(self.base_url.clone())
            .with_bearer_token(self.credentials.token.clone())
    }

    fn client_for_base_url(&self, base_url: &str) -> Result<HttpOrchestratorClient, String> {
        let requested_base_url = normalize_orchestrator_base_url(base_url)?;
        if requested_base_url != self.base_url {
            return Err(
                "orchestrator base_url does not match the authenticated session; re-authenticate"
                    .to_string(),
            );
        }
        Ok(self.client())
    }
}

#[derive(Default)]
struct OrchestratorAuthState {
    session: Option<StoredOrchestratorSession>,
}

impl OrchestratorAuthState {
    fn clear_expired(&mut self) {
        let expired = self
            .session
            .as_ref()
            .map(|session| now_unix_ms() >= session.credentials.expires_at_unix_ms)
            .unwrap_or(false);
        if expired {
            self.session = None;
        }
    }

    fn take_active_session(&mut self) -> Option<StoredOrchestratorSession> {
        self.clear_expired();
        self.session.take()
    }
}

fn state() -> &'static Mutex<OrchestratorAuthState> {
    static STATE: OnceLock<Mutex<OrchestratorAuthState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(OrchestratorAuthState::default()))
}

pub async fn start_challenge(
    base_url: &str,
    authority: &str,
) -> Result<OrchestratorAuthChallenge, String> {
    let base_url = normalize_orchestrator_base_url(base_url)?;
    let client = HttpOrchestratorClient::new(base_url);
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
) -> Result<OrchestratorAuthSessionView, String> {
    let base_url = normalize_orchestrator_base_url(base_url)?;
    let client = HttpOrchestratorClient::new(base_url.clone());
    let credentials = client
        .auth_verify(request)
        .await
        .map_err(|e| e.to_string())?;
    let session = StoredOrchestratorSession {
        session_id: uuid::Uuid::new_v4().to_string(),
        base_url,
        credentials,
    };
    let view = session.view();
    let mut lock = state()
        .lock()
        .map_err(|_| "orchestrator auth state lock poisoned".to_string())?;
    lock.session = Some(session);
    Ok(view)
}

/// A client for the orchestrator of the current, unexpired session — for background work that
/// has no request to take a base URL from (the settle loop, #516). `None` without a session.
pub fn authenticated_client() -> Option<HttpOrchestratorClient> {
    let mut lock = state().lock().ok()?;
    lock.clear_expired();
    lock.session.as_ref().map(StoredOrchestratorSession::client)
}

/// Build an authenticated client only when the renderer's configured URL is the exact canonical
/// URL that issued the current session. The client always uses the stored URL, never caller data.
pub fn authenticated_client_for(base_url: &str) -> Result<HttpOrchestratorClient, String> {
    let mut lock = state()
        .lock()
        .map_err(|_| "orchestrator auth state lock poisoned".to_string())?;
    lock.clear_expired();
    let session = lock
        .session
        .as_ref()
        .ok_or_else(|| "no orchestrator session; authenticate first".to_string())?;
    session.client_for_base_url(base_url)
}

pub fn get_session() -> Result<Option<OrchestratorAuthSessionView>, String> {
    let mut lock = state()
        .lock()
        .map_err(|_| "orchestrator auth state lock poisoned".to_string())?;
    lock.clear_expired();
    Ok(lock.session.as_ref().map(StoredOrchestratorSession::view))
}

fn now_unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub async fn logout() -> Result<(), String> {
    // Remove native credentials before awaiting the network so logout always revokes local access,
    // even if the issuing orchestrator is unavailable.
    let session = {
        let mut lock = state()
            .lock()
            .map_err(|_| "orchestrator auth state lock poisoned".to_string())?;
        lock.take_active_session()
            .ok_or_else(|| "no orchestrator session".to_string())?
    };
    session
        .client()
        .auth_logout()
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{validated_challenge_message, OrchestratorAuthState, StoredOrchestratorSession};
    use crate::application::orchestrator_client::{
        OrchestratorAuthChallenge, OrchestratorAuthSession,
    };
    use crate::domain::action::Action;
    use crate::infrastructure::{action_codec, signing};

    fn stored_session(base_url: &str) -> StoredOrchestratorSession {
        StoredOrchestratorSession {
            session_id: "opaque-session-id".to_string(),
            base_url: base_url.to_string(),
            credentials: OrchestratorAuthSession {
                token: "secret-token".to_string(),
                authority: "strata_admin".to_string(),
                signer_pubkey: "02aa".to_string(),
                expires_at_unix_ms: u64::MAX,
            },
        }
    }

    #[test]
    fn renderer_session_view_omits_bearer_token() {
        let serialized =
            serde_json::to_value(stored_session("https://orchestrator.example/api/v1").view())
                .unwrap();
        assert_eq!(serialized["session_id"].as_str(), Some("opaque-session-id"));
        assert!(serialized.get("token").is_none());
    }

    #[test]
    fn authenticated_client_rejects_another_origin() {
        let session = stored_session("https://orchestrator.example/api/v1");
        assert!(session
            .client_for_base_url("https://attacker.example/api/v1")
            .is_err());
        assert!(session
            .client_for_base_url("https://ORCHESTRATOR.EXAMPLE:443/api/v1/")
            .is_ok());
    }

    #[test]
    fn taking_session_for_logout_erases_native_credentials() {
        let mut state = OrchestratorAuthState {
            session: Some(stored_session("https://orchestrator.example/api/v1")),
        };
        assert!(state.take_active_session().is_some());
        assert!(state.session.is_none());
    }

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
