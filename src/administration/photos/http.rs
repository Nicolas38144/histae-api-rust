use axum::extract::Extension;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use uuid::{Uuid, Variant};

use super::{AdminPhotoError, AdminPhotoPage, AdminPhotoService, PhotoReconciliationFilter};
use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedJson, ValidatedPath, ValidatedQuery};
use crate::http::router::HttpState;
use crate::identity::admin::http::{AdminAuthHttpState, AdminIdentity, RecentAdminIdentity};
use crate::shared::text::validator_js_length;
use serde::{Deserialize, Serialize};

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
        (1..=8).contains(&self.id.get_version_num()) && self.id.get_variant() == Variant::RFC4122
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
