use std::future::Future;
use std::pin::Pin;

use uuid::Uuid;

use super::domain::{
    AdminUserDetailRow, AdminUserRole, AdminUserRow, AdminUserStatus, BanResult, CursorMatchRow,
    CursorMessageRow, PageCursor, RoleChangeResult,
};
use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::DatabaseError;

pub type AdministrationStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

#[allow(clippy::too_many_arguments)]
pub trait AdministrationStore: Send + Sync {
    fn user_names(
        &self,
        ids: Vec<Uuid>,
    ) -> AdministrationStoreFuture<'_, Vec<(Uuid, Option<String>)>>;

    fn list_users(
        &self,
        status: Option<AdminUserStatus>,
        role: Option<AdminUserRole>,
        search: String,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
        terms_version: String,
        privacy_version: String,
    ) -> AdministrationStoreFuture<'_, Vec<AdminUserRow>>;

    fn user_detail(
        &self,
        target_id: Uuid,
        admin_id: Uuid,
        admin_role: AdminRole,
        reason: String,
        terms_version: String,
        privacy_version: String,
    ) -> AdministrationStoreFuture<'_, Option<AdminUserDetailRow>>;

    fn set_ban(
        &self,
        target_id: Uuid,
        is_banned: bool,
        reason: String,
        admin_id: Uuid,
        admin_role: AdminRole,
    ) -> AdministrationStoreFuture<'_, BanResult>;

    fn set_role(
        &self,
        target_id: Uuid,
        role: AdminUserRole,
        reason: String,
        actor_id: Uuid,
    ) -> AdministrationStoreFuture<'_, RoleChangeResult>;

    fn matches(
        &self,
        user_id: Uuid,
        admin_id: Uuid,
        admin_role: AdminRole,
        reason: String,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> AdministrationStoreFuture<'_, Option<Vec<CursorMatchRow>>>;

    fn messages(
        &self,
        match_id: Uuid,
        admin_id: Uuid,
        admin_role: AdminRole,
        reason: String,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> AdministrationStoreFuture<'_, Option<Vec<CursorMessageRow>>>;
}
