use std::future::Future;
use std::pin::Pin;

use uuid::Uuid;

use super::domain::BlockedUserRow;
use crate::infra::postgres::DatabaseError;

pub type PrivacyStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait PrivacyStore: Send + Sync {
    fn block(&self, blocker_id: Uuid, blocked_id: Uuid) -> PrivacyStoreFuture<'_, bool>;
    fn unblock(&self, blocker_id: Uuid, blocked_id: Uuid) -> PrivacyStoreFuture<'_, ()>;
    fn blocked_users(&self, blocker_id: Uuid) -> PrivacyStoreFuture<'_, Vec<BlockedUserRow>>;
}
