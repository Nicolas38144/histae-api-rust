use std::future::Future;
use std::pin::Pin;

use uuid::Uuid;

use super::domain::{
    ModerationContentType, ModerationReviewInput, ModerationReviewResult, ModerationRow, PageCursor,
};
use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::DatabaseError;
use crate::profiles::domain::ModerationStatus;

pub type ModerationStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait ModerationStore: Send + Sync {
    fn list<'a>(
        &'a self,
        status: Option<ModerationStatus>,
        content_type: Option<ModerationContentType>,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> ModerationStoreFuture<'a, Vec<ModerationRow>>;

    fn detail(
        &self,
        case_id: Uuid,
        admin_id: Uuid,
        admin_role: AdminRole,
        reason: String,
    ) -> ModerationStoreFuture<'_, Option<ModerationRow>>;

    fn review(
        &self,
        case_id: Uuid,
        input: ModerationReviewInput,
        admin_id: Uuid,
        admin_role: AdminRole,
    ) -> ModerationStoreFuture<'_, ModerationReviewResult>;
}
