use axum::extract::Extension;
use axum::http::StatusCode;
use axum::routing::post;
#[cfg(feature = "webauthn-probe")]
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
#[cfg(feature = "webauthn-probe")]
use uuid::Uuid;

use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedJson};
#[cfg(feature = "webauthn-probe")]
use crate::http::extract::{ValidatedPath, ValidatedQuery};
use crate::http::router::HttpState;
#[cfg(feature = "webauthn-probe")]
use crate::identity::admin::http::{AdminAuthHttpState, AdminIdentity, RecentAdminIdentity};
use crate::identity::mobile::http::{AuthenticatedMobile, MobileAuthState};
#[cfg(feature = "webauthn-probe")]
use crate::privacy::rights::{
    AdminDataRequest, DataAccessLog, DataRequestStatus, DataRequestTransition, Page,
    UpdateRequestInput, UpdateRequestResult,
};
use crate::privacy::rights::{DataRequest, DataRequestType, DataRightsError, DataRightsService};
#[cfg(feature = "webauthn-probe")]
use crate::shared::text::validator_js_length;

#[derive(Clone)]
pub struct DataRightsHttpState {
    service: DataRightsService,
}

impl DataRightsHttpState {
    pub fn new(service: DataRightsService) -> Self {
        Self { service }
    }
}

pub fn mobile_routes(state: DataRightsHttpState, auth: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route(
            "/api/users/me/data-subject-requests",
            post(create_request).get(user_requests),
        )
        .layer(Extension(state))
        .layer(Extension(auth))
}

#[cfg(feature = "webauthn-probe")]
pub fn admin_routes(state: DataRightsHttpState, auth: AdminAuthHttpState) -> Router<HttpState> {
    Router::new()
        .route("/api/admin/data-subject-requests", get(admin_requests))
        .route(
            "/api/admin/data-subject-requests/{id}",
            patch(update_request),
        )
        .route("/api/admin/data-access-logs", get(access_logs))
        .layer(Extension(state))
        .layer(Extension(auth))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRequestBody {
    #[serde(rename = "type")]
    request_type: DataRequestType,
}

impl ApiDto for CreateRequestBody {
    const ERROR_CODE: &'static str = "invalid_data_request";
    const ERROR_MESSAGE: &'static str = "The data subject request is invalid.";

    fn is_valid(&self) -> bool {
        true
    }
}

async fn create_request(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<DataRightsHttpState>,
    ValidatedJson(body): ValidatedJson<CreateRequestBody>,
) -> Result<(StatusCode, Json<DataRequest>), ApiError> {
    state
        .service
        .create_request(identity.account.user_id, body.request_type)
        .await
        .map(|request| (StatusCode::CREATED, Json(request)))
        .map_err(data_rights_error)
}

#[derive(Serialize)]
struct UserRequestsResponse {
    requests: Vec<DataRequest>,
}

async fn user_requests(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<DataRightsHttpState>,
) -> Result<Json<UserRequestsResponse>, ApiError> {
    state
        .service
        .requests_for_user(identity.account.user_id)
        .await
        .map(|requests| Json(UserRequestsResponse { requests }))
        .map_err(data_rights_error)
}

#[cfg(feature = "webauthn-probe")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestsQuery {
    status: Option<DataRequestStatus>,
    limit: Option<String>,
    offset: Option<String>,
    cursor: Option<String>,
}

#[cfg(feature = "webauthn-probe")]
impl RequestsQuery {
    fn limit(&self) -> Option<u32> {
        javascript_number(&self.limit, 20)
    }

    fn offset(&self) -> Option<u32> {
        javascript_number(&self.offset, 0)
    }
}

#[cfg(feature = "webauthn-probe")]
impl ApiDto for RequestsQuery {
    const ERROR_CODE: &'static str = "invalid_data_request_query";
    const ERROR_MESSAGE: &'static str = "The data subject request query is invalid.";

    fn is_valid(&self) -> bool {
        self.limit().is_some_and(|limit| (1..=100).contains(&limit))
            && self.offset().is_some()
            && self
                .cursor
                .as_ref()
                .is_none_or(|value| validator_js_length(value) <= 512)
    }
}

#[cfg(feature = "webauthn-probe")]
#[derive(Serialize)]
struct AdminRequestsResponse {
    requests: Vec<AdminDataRequest>,
    next_cursor: Option<String>,
}

#[cfg(feature = "webauthn-probe")]
async fn admin_requests(
    AdminIdentity(_identity): AdminIdentity,
    Extension(state): Extension<DataRightsHttpState>,
    ValidatedQuery(query): ValidatedQuery<RequestsQuery>,
) -> Result<Json<AdminRequestsResponse>, ApiError> {
    let Page { items, next_cursor } = state
        .service
        .requests_for_admin(
            query.status,
            query.limit().ok_or_else(invalid_data_request_query)?,
            query.offset().ok_or_else(invalid_data_request_query)?,
            query.cursor.as_deref(),
        )
        .await
        .map_err(data_rights_error)?;
    Ok(Json(AdminRequestsResponse {
        requests: items,
        next_cursor,
    }))
}

#[cfg(feature = "webauthn-probe")]
#[derive(Deserialize)]
struct RequestPath {
    id: String,
}

#[cfg(feature = "webauthn-probe")]
impl RequestPath {
    fn id(&self) -> Option<Uuid> {
        canonical_uuid(&self.id)
    }
}

#[cfg(feature = "webauthn-probe")]
impl ApiDto for RequestPath {
    const ERROR_CODE: &'static str = "invalid_data_request_id";
    const ERROR_MESSAGE: &'static str = "The data subject request ID is invalid.";

    fn is_valid(&self) -> bool {
        self.id().is_some()
    }
}

#[cfg(feature = "webauthn-probe")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateRequestBody {
    status: DataRequestTransition,
    notes: Option<String>,
}

#[cfg(feature = "webauthn-probe")]
impl ApiDto for UpdateRequestBody {
    const ERROR_CODE: &'static str = "invalid_data_request";
    const ERROR_MESSAGE: &'static str = "The data subject request is invalid.";

    fn is_valid(&self) -> bool {
        self.notes
            .as_ref()
            .is_none_or(|value| validator_js_length(value) <= 2_000)
    }
}

#[cfg(feature = "webauthn-probe")]
#[derive(Serialize)]
struct MessageResponse {
    message: &'static str,
}

#[cfg(feature = "webauthn-probe")]
async fn update_request(
    RecentAdminIdentity(identity): RecentAdminIdentity,
    Extension(state): Extension<DataRightsHttpState>,
    ValidatedPath(path): ValidatedPath<RequestPath>,
    ValidatedJson(body): ValidatedJson<UpdateRequestBody>,
) -> Result<Json<MessageResponse>, ApiError> {
    let result = state
        .service
        .update_request(UpdateRequestInput {
            request_id: path.id().ok_or_else(invalid_data_request_id)?,
            status: body.status,
            admin_id: identity.user_id,
            admin_role: identity.role,
            notes: body.notes,
        })
        .await
        .map_err(data_rights_error)?;
    Ok(Json(MessageResponse {
        message: if result == UpdateRequestResult::ErasureScheduled {
            "account erasure scheduled"
        } else {
            "data subject request updated"
        },
    }))
}

#[cfg(feature = "webauthn-probe")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessLogsQuery {
    user_id: String,
    limit: Option<String>,
    offset: Option<String>,
    cursor: Option<String>,
}

#[cfg(feature = "webauthn-probe")]
impl AccessLogsQuery {
    fn user_id(&self) -> Option<Uuid> {
        canonical_uuid(&self.user_id)
    }

    fn limit(&self) -> Option<u32> {
        javascript_number(&self.limit, 20)
    }

    fn offset(&self) -> Option<u32> {
        javascript_number(&self.offset, 0)
    }
}

#[cfg(feature = "webauthn-probe")]
impl ApiDto for AccessLogsQuery {
    const ERROR_CODE: &'static str = "invalid_data_access_query";
    const ERROR_MESSAGE: &'static str = "The data access query is invalid.";

    fn is_valid(&self) -> bool {
        self.user_id().is_some()
            && self.limit().is_some_and(|limit| (1..=100).contains(&limit))
            && self.offset().is_some()
            && self
                .cursor
                .as_ref()
                .is_none_or(|value| validator_js_length(value) <= 512)
    }
}

#[cfg(feature = "webauthn-probe")]
#[derive(Serialize)]
struct AccessLogsResponse {
    logs: Vec<DataAccessLog>,
    next_cursor: Option<String>,
}

#[cfg(feature = "webauthn-probe")]
async fn access_logs(
    AdminIdentity(_identity): AdminIdentity,
    Extension(state): Extension<DataRightsHttpState>,
    ValidatedQuery(query): ValidatedQuery<AccessLogsQuery>,
) -> Result<Json<AccessLogsResponse>, ApiError> {
    let Page { items, next_cursor } = state
        .service
        .access_logs(
            query.user_id().ok_or_else(invalid_data_access_query)?,
            query.limit().ok_or_else(invalid_data_access_query)?,
            query.offset().ok_or_else(invalid_data_access_query)?,
            query.cursor.as_deref(),
        )
        .await
        .map_err(data_rights_error)?;
    Ok(Json(AccessLogsResponse {
        logs: items,
        next_cursor,
    }))
}

fn data_rights_error(error: DataRightsError) -> ApiError {
    match error {
        DataRightsError::AlreadyOpen => ApiError::new(
            StatusCode::CONFLICT,
            "data_request_already_open",
            "An open request of this type already exists.",
        ),
        DataRightsError::InvalidCursor | DataRightsError::InvalidPagination => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_cursor",
            "The pagination cursor is invalid.",
        ),
        DataRightsError::RequestNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "data_request_not_found",
            "The data subject request was not found.",
        ),
        DataRightsError::InvalidTransition => ApiError::new(
            StatusCode::CONFLICT,
            "invalid_data_request_transition",
            "This data subject request transition is not allowed.",
        ),
        DataRightsError::Database(error) => error.into(),
    }
}

#[cfg(feature = "webauthn-probe")]
fn javascript_number(value: &Option<String>, default: u32) -> Option<u32> {
    let Some(value) = value else {
        return Some(default);
    };
    let value = crate::shared::text::javascript_trim(value);
    if value.is_empty() {
        return Some(0);
    }
    let number = crate::shared::validation::javascript_number(value)?;
    if !number.is_finite()
        || number.fract() != 0.0
        || !(0.0..=f64::from(u32::MAX)).contains(&number)
    {
        return None;
    }
    Some(number as u32)
}

#[cfg(feature = "webauthn-probe")]
fn canonical_uuid(value: &str) -> Option<Uuid> {
    let parsed = Uuid::parse_str(value).ok()?;
    (value.len() == 36
        && [8, 13, 18, 23]
            .iter()
            .all(|index| value.as_bytes()[*index] == b'-')
        && parsed.hyphenated().to_string().eq_ignore_ascii_case(value)
        && crate::shared::validation::uuid_all(parsed))
    .then_some(parsed)
}

#[cfg(feature = "webauthn-probe")]
fn invalid_data_request_query() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_data_request_query",
        "The data subject request query is invalid.",
    )
}

#[cfg(feature = "webauthn-probe")]
fn invalid_data_access_query() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_data_access_query",
        "The data access query is invalid.",
    )
}

#[cfg(feature = "webauthn-probe")]
fn invalid_data_request_id() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_data_request_id",
        "The data subject request ID is invalid.",
    )
}
