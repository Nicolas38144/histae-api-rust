use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use uuid::Uuid;

use crate::infra::postgres::DatabaseError;

use super::domain::{
    ActiveAccount, MobileSessionIdentity, MobileSessionRow, RotationOutcome, SessionCursor,
};
use super::tokens::NewRefreshToken;

pub type SessionStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait MobileSessionStore: Send + Sync {
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
