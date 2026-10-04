use std::future::Future;
use std::pin::Pin;

use uuid::Uuid;

use super::domain::{CursorReportRow, PageCursor, ReportRecord, ReportStatus};
use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::DatabaseError;

pub type ReportStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait ReportStore: Send + Sync {
    fn account_exists(&self, user_id: Uuid) -> ReportStoreFuture<'_, bool>;
    fn match_participants(&self, match_id: Uuid) -> ReportStoreFuture<'_, Option<[Uuid; 2]>>;
    fn create(&self, report: ReportRecord) -> ReportStoreFuture<'_, ()>;
    fn list(
        &self,
        status: Option<ReportStatus>,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> ReportStoreFuture<'_, Vec<CursorReportRow>>;
    fn update_status(
        &self,
        id: Uuid,
        status: ReportStatus,
        admin_id: Uuid,
        admin_role: AdminRole,
    ) -> ReportStoreFuture<'_, bool>;
}
