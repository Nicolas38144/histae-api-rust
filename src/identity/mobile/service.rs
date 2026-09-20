use std::fmt;
use std::sync::Arc;

use serde::Serialize;
use uuid::Uuid;

use super::domain::{ActiveAccount, RotationOutcome, decode_cursor, encode_cursor, wire_timestamp};
use super::pg::MobileSessionStore;
use super::tokens::{TokenError, TokenService};
use crate::infra::postgres::DatabaseError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MobileAuthError {
    InvalidRefreshToken,
    InvalidAccessToken,
    AuthenticationRequired,
    AccountUnavailable,
    AccountCheckFailed,
    SessionNotFound,
    InvalidCursor,
    Internal,
}

impl fmt::Display for MobileAuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRefreshToken => "invalid_or_expired_refresh_token",
            Self::InvalidAccessToken => "invalid_or_expired_access_token",
            Self::AuthenticationRequired => "authentication_required",
            Self::AccountUnavailable => "account_unavailable",
            Self::AccountCheckFailed => "account_check_failed",
            Self::SessionNotFound => "session_not_found",
            Self::InvalidCursor => "invalid_cursor",
            Self::Internal => "internal_error",
        })
    }
}

impl std::error::Error for MobileAuthError {}

impl From<DatabaseError> for MobileAuthError {
    fn from(_: DatabaseError) -> Self {
        Self::Internal
    }
}

impl From<TokenError> for MobileAuthError {
    fn from(_: TokenError) -> Self {
        Self::Internal
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedMobile {
    pub account: ActiveAccount,
    pub session_id: Uuid,
    pub access_expires_at_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MobileSessionView {
    pub id: Uuid,
    pub created_at: String,
    pub last_refreshed_at: String,
    pub expires_at: String,
    pub current: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MobileSessionPage {
    pub sessions: Vec<MobileSessionView>,
    pub next_cursor: Option<String>,
}

#[derive(Clone)]
pub struct MobileAuthService {
    tokens: TokenService,
    sessions: Arc<dyn MobileSessionStore>,
    terms_version: Arc<str>,
    privacy_version: Arc<str>,
}

impl MobileAuthService {
    pub fn new(
        tokens: TokenService,
        sessions: Arc<dyn MobileSessionStore>,
        terms_version: String,
        privacy_version: String,
    ) -> Self {
        Self {
            tokens,
            sessions,
            terms_version: terms_version.into(),
            privacy_version: privacy_version.into(),
        }
    }

    pub async fn authenticate(
        &self,
        bearer_token: &str,
    ) -> Result<AuthenticatedMobile, MobileAuthError> {
        let verified = self
            .tokens
            .verify_access_token(bearer_token)
            .map_err(|_| MobileAuthError::InvalidAccessToken)?;
        let account = self
            .sessions
            .active_account(
                verified.user_id,
                verified.session_id,
                Arc::clone(&self.terms_version),
                Arc::clone(&self.privacy_version),
            )
            .await
            .map_err(|_| MobileAuthError::AccountCheckFailed)?
            .ok_or(MobileAuthError::AuthenticationRequired)?;
        if account.is_banned {
            return Err(MobileAuthError::AccountUnavailable);
        }
        Ok(AuthenticatedMobile {
            account,
            session_id: verified.session_id,
            access_expires_at_seconds: verified.expires_at_seconds,
        })
    }

    pub async fn issue_token_pair(&self, user_id: Uuid) -> Result<TokenPair, MobileAuthError> {
        let refresh = self.tokens.new_refresh_token()?;
        let refresh_token = refresh.plain.clone();
        let session = self
            .sessions
            .create(user_id, refresh)
            .await?
            .ok_or(MobileAuthError::AccountUnavailable)?;
        Ok(TokenPair {
            access_token: self.tokens.access_token(user_id, session.session_id)?,
            refresh_token,
        })
    }

    pub async fn refresh(&self, raw_token: &str) -> Result<TokenPair, MobileAuthError> {
        let parsed = TokenService::parse_refresh_token(raw_token)
            .ok_or(MobileAuthError::InvalidRefreshToken)?;
        let next = self.tokens.new_refresh_token()?;
        let refresh_token = next.plain.clone();
        let session = match self.sessions.rotate(parsed.jti, parsed.hash, next).await? {
            RotationOutcome::Rotated(session) => session,
            RotationOutcome::Invalid | RotationOutcome::ReplayRevoked => {
                return Err(MobileAuthError::InvalidRefreshToken);
            }
        };
        Ok(TokenPair {
            access_token: self
                .tokens
                .access_token(session.user_id, session.session_id)?,
            refresh_token,
        })
    }

    pub async fn logout(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        raw_token: &str,
        device_id: Option<Uuid>,
    ) -> Result<(), MobileAuthError> {
        let parsed = TokenService::parse_refresh_token(raw_token)
            .ok_or(MobileAuthError::InvalidRefreshToken)?;
        if !self
            .sessions
            .logout(user_id, session_id, parsed.jti, parsed.hash, device_id)
            .await?
        {
            return Err(MobileAuthError::InvalidRefreshToken);
        }
        Ok(())
    }

    pub async fn list_sessions(
        &self,
        user_id: Uuid,
        current_session_id: Uuid,
        limit: u32,
        raw_cursor: Option<&str>,
    ) -> Result<MobileSessionPage, MobileAuthError> {
        let cursor = decode_cursor(raw_cursor).map_err(|_| MobileAuthError::InvalidCursor)?;
        let mut rows = self.sessions.list(user_id, limit + 1, cursor).await?;
        let has_more = rows.len() > limit as usize;
        if has_more {
            rows.truncate(limit as usize);
        }
        let next_cursor = if has_more {
            rows.last()
                .map(|row| {
                    encode_cursor(&super::domain::SessionCursor {
                        at: row.cursor_at.clone(),
                        id: row.id,
                    })
                })
                .transpose()
                .map_err(|_| MobileAuthError::Internal)?
        } else {
            None
        };
        let sessions = rows
            .into_iter()
            .map(|row| MobileSessionView {
                id: row.id,
                created_at: wire_timestamp(row.created_at),
                last_refreshed_at: wire_timestamp(row.last_refreshed_at),
                expires_at: wire_timestamp(row.expires_at),
                current: row.id == current_session_id,
            })
            .collect();
        Ok(MobileSessionPage {
            sessions,
            next_cursor,
        })
    }

    pub async fn revoke_session(
        &self,
        user_id: Uuid,
        current_session_id: Uuid,
        target_id: Uuid,
    ) -> Result<(), MobileAuthError> {
        match self
            .sessions
            .revoke(user_id, current_session_id, Some(target_id))
            .await?
        {
            None => Err(MobileAuthError::InvalidRefreshToken),
            Some(0) => Err(MobileAuthError::SessionNotFound),
            Some(_) => Ok(()),
        }
    }

    pub async fn logout_all(
        &self,
        user_id: Uuid,
        current_session_id: Uuid,
    ) -> Result<u64, MobileAuthError> {
        self.sessions
            .revoke(user_id, current_session_id, None)
            .await?
            .ok_or(MobileAuthError::InvalidRefreshToken)
    }
}
