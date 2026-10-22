use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{StoreAccount, StoreFuture, StoreProvider, StoreSnapshot};

pub type PurchaseReference = (StoreProvider, String, Option<Uuid>);

/// Persistence required by native billing use cases. Provider calls never run
/// while a database transaction or an ownership lock is held.
pub trait StorePersistence: Send + Sync {
    fn observed_at(&self) -> StoreFuture<'_, DateTime<Utc>>;
    fn account(&self, user: Uuid) -> StoreFuture<'_, StoreAccount>;
    fn purchase_allowed(&self, user: Uuid) -> StoreFuture<'_, bool>;
    fn apply(
        &self,
        snapshot: StoreSnapshot,
        reference: String,
        expected_user: Option<Uuid>,
        observed_at: DateTime<Utc>,
    ) -> StoreFuture<'_, Option<Uuid>>;
    fn receive(
        &self,
        provider: StoreProvider,
        event_id: String,
        reference: Option<String>,
    ) -> StoreFuture<'_, ()>;
    fn reference(&self, id: Uuid, notification: bool)
    -> StoreFuture<'_, Option<PurchaseReference>>;
    fn complete_notification(&self, id: Uuid) -> StoreFuture<'_, ()>;
    fn acknowledged(&self, id: Uuid) -> StoreFuture<'_, ()>;
}
