use super::auth_session::AuthenticatedSession;
use crate::{
    application::payouts::PayoutService, domain::authority::Authority, error::AppError,
    state::AppState,
};
use axum::{extract::FromRequestParts, http::request::Parts};
use std::sync::Arc;

/// A session for the ASM governance routes (`/proposals/*`). A Payout Administrator session is
/// refused with the same 401 as no session: payout admins sign Bitcoin transactions, never ASM
/// actions.
#[derive(Debug, Clone)]
pub struct GovernanceSession(pub AuthenticatedSession);

impl std::ops::Deref for GovernanceSession {
    type Target = AuthenticatedSession;
    fn deref(&self) -> &AuthenticatedSession {
        &self.0
    }
}

#[axum::async_trait]
impl FromRequestParts<AppState> for GovernanceSession {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let s = AuthenticatedSession::from_request_parts(parts, state).await?;
        if s.authority == Authority::PayoutAdmin {
            return Err(AppError::Unauthorized);
        }
        Ok(Self(s))
    }
}

/// A Payout Administrator session on a coordinator configured for payouts. The admin is checked
/// against `params.toml` again on every request, so a session outlives no admin set change.
#[derive(Clone)]
pub struct PayoutAdminSession {
    pub service: Arc<PayoutService>,
}

#[axum::async_trait]
impl FromRequestParts<AppState> for PayoutAdminSession {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let s = AuthenticatedSession::from_request_parts(parts, state).await?;
        let service = state.payouts.clone().ok_or(AppError::Unauthorized)?;
        if s.authority != Authority::PayoutAdmin || !service.is_admin_pubkey(&s.signer_pubkey) {
            return Err(AppError::Unauthorized);
        }
        Ok(Self { service })
    }
}
