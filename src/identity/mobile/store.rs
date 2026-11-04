use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use uuid::Uuid;

use crate::infra::postgres::DatabaseError;

use super::domain::{
    ActiveAccount, MobileSessionIdentity, MobileSessionRow, RotationOutcome, SessionCursor,
};
use super::play_review::NewReviewAccount;
use super::refresh_recovery::{RefreshRecovery, RefreshRotation};
use super::tokens::NewRefreshToken;

pub type SessionStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait MobileSessionStore: Send + Sync {
    /// Fails closed for stores that do not implement dedicated review accounts.
    fn create_review_session(
        &self,
        _account: NewReviewAccount,
        _token: NewRefreshToken,
    ) -> SessionStoreFuture<'_, Option<MobileSessionIdentity>> {
        Box::pin(async { Ok(None) })
    }

    fn create(
        &self,
        user_id: Uuid,
        token: NewRefreshToken,
    ) -> SessionStoreFuture<'_, Option<MobileSessionIdentity>>;

    fn rotate(
        &self,
        jti: Uuid,
        hash: String,
        next: NewRefreshToken,
    ) -> SessionStoreFuture<'_, RotationOutcome>;

    // Stores must explicitly implement atomic recovery, never silently downgrade
    // a recoverable request to legacy rotation.
    fn rotate_recoverable(
        &self,
        _jti: Uuid,
        _hash: String,
        _next: NewRefreshToken,
        _recovery: RefreshRecovery,
    ) -> SessionStoreFuture<'_, RefreshRotation> {
        Box::pin(async { Err(DatabaseError::QueryFailed) })
    }

    fn logout(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        jti: Uuid,
        hash: String,
        device_id: Option<Uuid>,
    ) -> SessionStoreFuture<'_, bool>;

    fn list(
        &self,
        user_id: Uuid,
        limit: u32,
        cursor: Option<SessionCursor>,
    ) -> SessionStoreFuture<'_, Vec<MobileSessionRow>>;

    fn revoke(
        &self,
        user_id: Uuid,
        current_session_id: Uuid,
        target_id: Option<Uuid>,
    ) -> SessionStoreFuture<'_, Option<u64>>;

    fn active_account(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        terms_version: Arc<str>,
        privacy_version: Arc<str>,
    ) -> SessionStoreFuture<'_, Option<ActiveAccount>>;
}
