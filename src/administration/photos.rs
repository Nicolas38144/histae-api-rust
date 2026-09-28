use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::moderation::domain::{PageCursor, wire_timestamp};
use crate::outbox::pg::PgOutboxRepository;
use crate::outbox::types::{NewOutboxEvent, OutboxEventType};
use crate::shared::clock::Clock;
use crate::shared::text::{javascript_trim, validator_js_length};

pub const PHOTO_PROCESSING_STALE_MINUTES: i64 = 30;
pub const OUTBOX_LOCK_STALE_MINUTES: i64 = 5;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PhotoReconciliationFilter {
    #[default]
    All,
    StaleProcessing,
    Deleting,
    DeadLetter,
}

impl PhotoReconciliationFilter {
    const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::StaleProcessing => "stale_processing",
            Self::Deleting => "deleting",
            Self::DeadLetter => "dead_letter",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AdminPhotoReconciliation {
    pub photo_id: Uuid,
    pub user_id: Uuid,
    pub status: String,
    pub size_bytes: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub created_at: String,
    pub updated_at: String,
    pub outbox_status: Option<String>,
    pub outbox_attempts: Option<i16>,
    pub outbox_available_at: Option<String>,
    pub outbox_locked_at: Option<String>,
    pub outbox_last_error_code: Option<String>,
    pub issue: String,
}

#[derive(Clone, Debug)]
pub struct AdminPhotoRow {
    pub item: AdminPhotoReconciliation,
    pub cursor_at: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconciliationResult {
    Queued,
    NotFound,
    NotActionable,
    AlreadyProcessing,
}

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

#[derive(Clone)]
pub struct PgAdminPhotoRepository {
    database: Database,
    outbox: PgOutboxRepository,
}

impl PgAdminPhotoRepository {
    pub fn new(database: Database, outbox: PgOutboxRepository) -> Self {
        Self { database, outbox }
    }
}

impl AdminPhotoStore for PgAdminPhotoRepository {
    fn list<'a>(
        &'a self,
        filter: PhotoReconciliationFilter,
        stale_before: DateTime<Utc>,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> AdminPhotoStoreFuture<'a, Vec<AdminPhotoRow>> {
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT photo.id, photo.user_id, photo.status, photo.size_bytes,
                        photo.width, photo.height, photo.created_at, photo.updated_at,
                        event.status AS outbox_status, event.attempts AS outbox_attempts,
                        event.available_at AS outbox_available_at,
                        event.locked_at AS outbox_locked_at,
                        event.last_error_code AS outbox_last_error_code,
                        CASE
                          WHEN photo.status IN ('pending', 'processing') THEN 'stale_processing'
                          WHEN event.status = 'dead_letter' THEN 'deletion_dead_letter'
                          WHEN event.status = 'processing' THEN 'deletion_processing'
                          WHEN event.status = 'pending' AND event.attempts > 0 THEN 'deletion_retry_scheduled'
                          WHEN event.status = 'pending' THEN 'deletion_queued'
                          WHEN event.status = 'completed' THEN 'deletion_event_completed'
                          WHEN event.status = 'discarded' THEN 'deletion_event_discarded'
                          ELSE 'deletion_event_missing'
                        END AS issue,
                        to_char(photo.updated_at AT TIME ZONE 'UTC',
                          'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at
                 FROM user_photo AS photo
                 LEFT JOIN outbox_event AS event
                   ON event.event_type = 'photo.delete' AND event.aggregate_id = photo.id
                 WHERE ((photo.status IN ('pending', 'processing') AND photo.updated_at <= $1)
                         OR photo.status = 'deleting')
                   AND ($2 = 'all'
                     OR ($2 = 'stale_processing' AND photo.status IN ('pending', 'processing'))
                     OR ($2 = 'deleting' AND photo.status = 'deleting')
                     OR ($2 = 'dead_letter' AND photo.status = 'deleting'
                                             AND event.status = 'dead_letter'))
                   AND ($5::timestamptz IS NULL
                     OR (photo.updated_at, photo.id) < ($5::timestamptz, $6::uuid))
                 ORDER BY photo.updated_at DESC, photo.id DESC
                 LIMIT $3 OFFSET $4",
            )
            .bind(stale_before)
            .bind(filter.as_str())
            .bind(i64::from(limit))
            .bind(i64::from(offset))
            .bind(cursor.as_ref().map(|value| value.at))
            .bind(cursor.as_ref().map(|value| value.id))
            .fetch_all(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            rows.iter().map(map_admin_photo_row).collect()
        })
    }

    fn reconcile(
        &self,
        photo_id: Uuid,
        photo_stale_before: DateTime<Utc>,
        outbox_stale_before: DateTime<Utc>,
        admin_id: Uuid,
        admin_role: AdminRole,
        reason: String,
    ) -> AdminPhotoStoreFuture<'_, ReconciliationResult> {
        let outbox = self.outbox.clone();
        Box::pin(async move {
            self.database
                .transaction(|connection| {
                    Box::pin(async move {
                        type Photo = (Uuid, String, DateTime<Utc>);
                        let photo = sqlx::query_as::<_, Photo>(
                            "SELECT user_id, status, updated_at
                             FROM user_photo WHERE id = $1 FOR UPDATE",
                        )
                        .bind(photo_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        let Some((user_id, status, updated_at)) = photo else {
                            return Ok(ReconciliationResult::NotFound);
                        };
                        if status == "ready"
                            || (status != "deleting" && updated_at > photo_stale_before)
                        {
                            return Ok(ReconciliationResult::NotActionable);
                        }
                        let event = sqlx::query_as::<_, (String, Option<DateTime<Utc>>)>(
                            "SELECT status, locked_at FROM outbox_event
                             WHERE event_type = 'photo.delete' AND aggregate_id = $1
                             FOR UPDATE",
                        )
                        .bind(photo_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        if event.is_some_and(|(status, locked_at)| {
                            status == "processing"
                                && locked_at.is_some_and(|at| at > outbox_stale_before)
                        }) {
                            return Ok(ReconciliationResult::AlreadyProcessing);
                        }
                        sqlx::query(
                            "UPDATE user_photo SET status = 'deleting',
                             updated_at = clock_timestamp() WHERE id = $1",
                        )
                        .bind(photo_id)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        outbox
                            .requeue(
                                connection,
                                &NewOutboxEvent::empty(OutboxEventType::PhotoDelete, photo_id),
                            )
                            .await?;
                        sqlx::query(
                            "INSERT INTO data_access_log
                             (accessed_user_id, accessor_id, accessor_role, action, reason)
                             VALUES ($1, $2, $3, 'admin_reconcile_photo', $4)",
                        )
                        .bind(user_id)
                        .bind(admin_id)
                        .bind(admin_role.as_str())
                        .bind(reason)
                        .execute(&mut *connection)
                        .await
                        .map_err(map_sqlx_error)?;
                        Ok(ReconciliationResult::Queued)
                    })
                })
                .await
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdminPhotoError {
    InvalidRequest,
    InvalidCursor,
    NotFound,
    NotActionable,
    AlreadyProcessing,
    Database(DatabaseError),
}

impl From<DatabaseError> for AdminPhotoError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AdminPhotoPage {
    pub photos: Vec<AdminPhotoReconciliation>,
    pub next_cursor: Option<String>,
}

#[derive(Clone)]
pub struct AdminPhotoService {
    store: Arc<dyn AdminPhotoStore>,
    clock: Arc<dyn Clock>,
}

impl AdminPhotoService {
    pub fn new(store: Arc<dyn AdminPhotoStore>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }

    pub async fn list(
        &self,
        filter: PhotoReconciliationFilter,
        limit: u32,
        offset: u32,
        raw_cursor: Option<&str>,
    ) -> Result<AdminPhotoPage, AdminPhotoError> {
        if !(1..=100).contains(&limit)
            || (raw_cursor.is_some_and(|value| !value.is_empty()) && offset != 0)
        {
            return Err(AdminPhotoError::InvalidRequest);
        }
        let cursor = decode_cursor(raw_cursor)?;
        let stale_before = self.clock.now() - TimeDelta::minutes(PHOTO_PROCESSING_STALE_MINUTES);
        let rows = self
            .store
            .list(filter, stale_before, limit + 1, offset, cursor)
            .await?;
        let has_more = rows.len() > limit as usize;
        let photos = rows
            .iter()
            .take(limit as usize)
            .map(|row| row.item.clone())
            .collect();
        let next_cursor = if has_more {
            rows.get(limit as usize - 1)
                .map(|row| encode_cursor(&row.cursor_at, row.item.photo_id))
                .transpose()?
        } else {
            None
        };
        Ok(AdminPhotoPage {
            photos,
            next_cursor,
        })
    }

    pub async fn reconcile(
        &self,
        photo_id: Uuid,
        raw_reason: &str,
        admin_id: Uuid,
        admin_role: AdminRole,
    ) -> Result<(), AdminPhotoError> {
        let reason = normalize_admin_reason(raw_reason)?;
        let now = self.clock.now();
        match self
            .store
            .reconcile(
                photo_id,
                now - TimeDelta::minutes(PHOTO_PROCESSING_STALE_MINUTES),
                now - TimeDelta::minutes(OUTBOX_LOCK_STALE_MINUTES),
                admin_id,
                admin_role,
                reason,
            )
            .await?
        {
            ReconciliationResult::Queued => Ok(()),
            ReconciliationResult::NotFound => Err(AdminPhotoError::NotFound),
            ReconciliationResult::NotActionable => Err(AdminPhotoError::NotActionable),
            ReconciliationResult::AlreadyProcessing => Err(AdminPhotoError::AlreadyProcessing),
        }
    }
}

fn map_admin_photo_row(row: &sqlx::postgres::PgRow) -> Result<AdminPhotoRow, DatabaseError> {
    let created_at: DateTime<Utc> = row.try_get("created_at").map_err(map_sqlx_error)?;
    let updated_at: DateTime<Utc> = row.try_get("updated_at").map_err(map_sqlx_error)?;
    let available_at: Option<DateTime<Utc>> =
        row.try_get("outbox_available_at").map_err(map_sqlx_error)?;
    let locked_at: Option<DateTime<Utc>> =
        row.try_get("outbox_locked_at").map_err(map_sqlx_error)?;
    Ok(AdminPhotoRow {
        item: AdminPhotoReconciliation {
            photo_id: row.try_get("id").map_err(map_sqlx_error)?,
            user_id: row.try_get("user_id").map_err(map_sqlx_error)?,
            status: row.try_get("status").map_err(map_sqlx_error)?,
            size_bytes: row.try_get("size_bytes").map_err(map_sqlx_error)?,
            width: row.try_get("width").map_err(map_sqlx_error)?,
            height: row.try_get("height").map_err(map_sqlx_error)?,
            created_at: wire_timestamp(created_at),
            updated_at: wire_timestamp(updated_at),
            outbox_status: row.try_get("outbox_status").map_err(map_sqlx_error)?,
            outbox_attempts: row.try_get("outbox_attempts").map_err(map_sqlx_error)?,
            outbox_available_at: available_at.map(wire_timestamp),
            outbox_locked_at: locked_at.map(wire_timestamp),
            outbox_last_error_code: row
                .try_get("outbox_last_error_code")
                .map_err(map_sqlx_error)?,
            issue: row.try_get("issue").map_err(map_sqlx_error)?,
        },
        cursor_at: row.try_get("cursor_at").map_err(map_sqlx_error)?,
    })
}

fn normalize_admin_reason(value: &str) -> Result<String, AdminPhotoError> {
    let reason = javascript_trim(value).to_owned();
    if !(3..=500).contains(&validator_js_length(&reason)) {
        return Err(AdminPhotoError::InvalidRequest);
    }
    Ok(reason)
}

#[derive(Deserialize, Serialize)]
struct CursorWire {
    at: String,
    id: String,
}

fn decode_cursor(value: Option<&str>) -> Result<Option<PageCursor>, AdminPhotoError> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.encode_utf16().count() > 512 {
        return Err(AdminPhotoError::InvalidRequest);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AdminPhotoError::InvalidCursor)?;
    let cursor: CursorWire =
        serde_json::from_slice(&bytes).map_err(|_| AdminPhotoError::InvalidCursor)?;
    let id = Uuid::parse_str(&cursor.id).map_err(|_| AdminPhotoError::InvalidCursor)?;
    if !valid_cursor_timestamp(&cursor.at)
        || !canonical_uuid(&cursor.id, id)
        || !(1..=8).contains(&id.get_version_num())
        || id.get_variant() != uuid::Variant::RFC4122
    {
        return Err(AdminPhotoError::InvalidCursor);
    }
    let at = DateTime::parse_from_rfc3339(&cursor.at)
        .map_err(|_| AdminPhotoError::InvalidCursor)?
        .with_timezone(&Utc);
    Ok(Some(PageCursor { at, id }))
}

fn encode_cursor(at: &str, id: Uuid) -> Result<String, AdminPhotoError> {
    serde_json::to_vec(&CursorWire {
        at: at.to_owned(),
        id: id.hyphenated().to_string(),
    })
    .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
    .map_err(|_| AdminPhotoError::InvalidCursor)
}

fn valid_cursor_timestamp(value: &str) -> bool {
    let Some((date, fraction)) = value
        .strip_suffix('Z')
        .and_then(|value| value.rsplit_once('.'))
    else {
        return false;
    };
    (3..=6).contains(&fraction.len())
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
        && date.len() == 19
        && DateTime::parse_from_rfc3339(value).is_ok()
}

fn canonical_uuid(value: &str, parsed: Uuid) -> bool {
    value.len() == 36
        && [8, 13, 18, 23]
            .iter()
            .all(|index| value.as_bytes()[*index] == b'-')
        && parsed.hyphenated().to_string().eq_ignore_ascii_case(value)
}

#[cfg(feature = "webauthn-probe")]
mod http {
    use axum::extract::Extension;
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use uuid::{Uuid, Variant};

    use super::*;
    use crate::http::error::ApiError;
    use crate::http::extract::{ApiDto, ValidatedJson, ValidatedPath, ValidatedQuery};
    use crate::http::router::HttpState;
    use crate::identity::admin::http::{AdminAuthHttpState, AdminIdentity, RecentAdminIdentity};

    #[derive(Clone)]
    pub struct AdminPhotoHttpState {
        service: AdminPhotoService,
    }

    impl AdminPhotoHttpState {
        pub fn new(service: AdminPhotoService) -> Self {
            Self { service }
        }
    }

    pub fn routes(state: AdminPhotoHttpState, auth: AdminAuthHttpState) -> Router<HttpState> {
        Router::new()
            .route("/api/admin/photo-reconciliation", get(list))
            .route(
                "/api/admin/photo-reconciliation/{id}/retry",
                post(reconcile),
            )
            .layer(Extension(state))
            .layer(Extension(auth))
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ListQuery {
        #[serde(default)]
        status: PhotoReconciliationFilter,
        #[serde(default = "default_limit")]
        limit: u32,
        #[serde(default)]
        offset: u32,
        cursor: Option<String>,
    }

    impl ApiDto for ListQuery {
        const ERROR_CODE: &'static str = "invalid_admin_request";
        const ERROR_MESSAGE: &'static str = "The administrator request is invalid.";

        fn is_valid(&self) -> bool {
            (1..=100).contains(&self.limit)
                && self
                    .cursor
                    .as_ref()
                    .is_none_or(|value| validator_js_length(value) <= 512)
        }
    }

    const fn default_limit() -> u32 {
        20
    }

    async fn list(
        AdminIdentity(_identity): AdminIdentity,
        Extension(state): Extension<AdminPhotoHttpState>,
        ValidatedQuery(query): ValidatedQuery<ListQuery>,
    ) -> Result<Json<AdminPhotoPage>, ApiError> {
        state
            .service
            .list(
                query.status,
                query.limit,
                query.offset,
                query.cursor.as_deref(),
            )
            .await
            .map(Json)
            .map_err(admin_photo_error)
    }

    #[derive(Deserialize)]
    struct PhotoPath {
        id: Uuid,
    }

    impl ApiDto for PhotoPath {
        const ERROR_CODE: &'static str = "invalid_photo_id";
        const ERROR_MESSAGE: &'static str = "The photo ID must be a valid UUID.";

        fn is_valid(&self) -> bool {
            (1..=8).contains(&self.id.get_version_num())
                && self.id.get_variant() == Variant::RFC4122
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ReconcileBody {
        reason: String,
    }

    impl ApiDto for ReconcileBody {
        const ERROR_CODE: &'static str = "invalid_admin_request";
        const ERROR_MESSAGE: &'static str = "The administrator request is invalid.";

        fn is_valid(&self) -> bool {
            (3..=500).contains(&validator_js_length(&self.reason))
        }
    }

    #[derive(Serialize)]
    struct MessageResponse {
        message: &'static str,
    }

    async fn reconcile(
        RecentAdminIdentity(identity): RecentAdminIdentity,
        Extension(state): Extension<AdminPhotoHttpState>,
        ValidatedPath(path): ValidatedPath<PhotoPath>,
        ValidatedJson(body): ValidatedJson<ReconcileBody>,
    ) -> Result<(StatusCode, Json<MessageResponse>), ApiError> {
        state
            .service
            .reconcile(path.id, &body.reason, identity.user_id, identity.role)
            .await
            .map_err(admin_photo_error)?;
        Ok((
            StatusCode::ACCEPTED,
            Json(MessageResponse {
                message: "photo reconciliation queued",
            }),
        ))
    }

    fn admin_photo_error(error: AdminPhotoError) -> ApiError {
        match error {
            AdminPhotoError::InvalidRequest => ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_admin_request",
                "The administrator request is invalid.",
            ),
            AdminPhotoError::InvalidCursor => ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_cursor",
                "The pagination cursor is invalid.",
            ),
            AdminPhotoError::NotFound => ApiError::new(
                StatusCode::NOT_FOUND,
                "photo_not_found",
                "The profile photo could not be found.",
            ),
            AdminPhotoError::NotActionable => ApiError::new(
                StatusCode::CONFLICT,
                "photo_reconciliation_not_allowed",
                "This profile photo does not require reconciliation.",
            ),
            AdminPhotoError::AlreadyProcessing => ApiError::new(
                StatusCode::CONFLICT,
                "photo_reconciliation_in_progress",
                "This profile photo is already being processed.",
            ),
            AdminPhotoError::Database(_) => ApiError::internal(),
        }
    }
}

#[cfg(feature = "webauthn-probe")]
pub use http::{AdminPhotoHttpState, routes};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_and_bounds_operator_reasons() {
        assert_eq!(
            normalize_admin_reason("  Incident confirmé  "),
            Ok("Incident confirmé".to_owned())
        );
        assert_eq!(
            normalize_admin_reason(" x "),
            Err(AdminPhotoError::InvalidRequest)
        );
    }
}
