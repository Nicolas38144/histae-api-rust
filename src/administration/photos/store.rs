use super::{AdminPhotoRow, PhotoReconciliationFilter, ReconciliationResult};
use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::DatabaseError;
use crate::moderation::domain::PageCursor;
use chrono::{DateTime, Utc};
use std::{future::Future, pin::Pin};
use uuid::Uuid;
pub type AdminPhotoStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait AdminPhotoStore: Send + Sync {
    fn list<'a>(
        &'a self,
        filter: PhotoReconciliationFilter,
        stale_before: DateTime<Utc>,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> AdminPhotoStoreFuture<'a, Vec<AdminPhotoRow>>;

    fn reconcile(
        &self,
        photo_id: Uuid,
        photo_stale_before: DateTime<Utc>,
        outbox_stale_before: DateTime<Utc>,
        admin_id: Uuid,
        admin_role: AdminRole,
        reason: String,
    ) -> AdminPhotoStoreFuture<'_, ReconciliationResult>;
}
