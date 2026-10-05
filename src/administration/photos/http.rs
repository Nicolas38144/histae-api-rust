use axum::extract::Extension;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use uuid::Uuid;

use super::{AdminPhotoError, AdminPhotoPage, AdminPhotoService, PhotoReconciliationFilter};
use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedJson, ValidatedPath, ValidatedQuery};
use crate::http::router::HttpState;
use crate::identity::admin::http::{AdminAuthHttpState, AdminIdentity};
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
    #[serde(
        default = "default_limit",
        deserialize_with = "crate::shared::validation::deserialize_query_u32"
    )]
    limit: u32,
    #[serde(
        default,
        deserialize_with = "crate::shared::validation::deserialize_query_u32"
    )]
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
    #[serde(deserialize_with = "crate::shared::validation::deserialize_uuid")]
    id: Uuid,
}

impl ApiDto for PhotoPath {
    const ERROR_CODE: &'static str = "invalid_photo_id";
    const ERROR_MESSAGE: &'static str = "The photo ID must be a valid UUID.";

    fn is_valid(&self) -> bool {
        crate::shared::validation::uuid_all(self.id)
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
    AdminIdentity(identity): AdminIdentity,
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
        AdminPhotoError::Database(error) => error.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pagination_coercion_matches_class_transformer() {
        for value in ["20", "0x14", "2e1", "20.0", "0b10100", "0o24"] {
            let uri = format!("/?limit={value}").parse().expect("URI");
            let axum::extract::Query(query) =
                axum::extract::Query::<ListQuery>::try_from_uri(&uri).expect("query");
            assert!(query.is_valid());
            assert_eq!(query.limit, 20);
        }
        for value in ["0", "101", "1.5", "-1", "NaN", "Infinity"] {
            let uri = format!("/?limit={value}").parse().expect("URI");
            assert!(
                axum::extract::Query::<ListQuery>::try_from_uri(&uri)
                    .map_or(true, |query| !query.0.is_valid())
            );
        }
    }
}
