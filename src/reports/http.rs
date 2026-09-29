use axum::extract::Extension;
use axum::http::StatusCode;
use axum::routing::post;
#[cfg(feature = "webauthn-probe")]
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::Deserialize;
#[cfg(feature = "webauthn-probe")]
use serde::Serialize;
use uuid::{Uuid, Variant};

#[cfg(feature = "webauthn-probe")]
use super::domain::ReportStatus;
use super::domain::{PublicReport, ReportReason};
use super::service::{CreateReportInput, ReportError, ReportService};
use crate::config::LimitPolicy;
use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedJson};
#[cfg(feature = "webauthn-probe")]
use crate::http::extract::{ValidatedPath, ValidatedQuery};
use crate::http::rate_limit::RateLimiter;
use crate::http::router::HttpState;
#[cfg(feature = "webauthn-probe")]
use crate::identity::admin::http::{AdminAuthHttpState, AdminIdentity};
use crate::identity::mobile::http::{MobileAuthState, OnboardedMobile};
#[cfg(feature = "webauthn-probe")]
use crate::shared::text::validator_js_length;

#[derive(Clone)]
pub struct ReportHttpState {
    service: ReportService,
    limiter: RateLimiter,
    policy: LimitPolicy,
}

impl ReportHttpState {
    pub fn new(service: ReportService, limiter: RateLimiter, policy: LimitPolicy) -> Self {
        Self {
            service,
            limiter,
            policy,
        }
    }
}

pub fn mobile_routes(state: ReportHttpState, auth: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route("/api/reports", post(create))
        .layer(Extension(state))
        .layer(Extension(auth))
}

#[cfg(feature = "webauthn-probe")]
pub fn admin_routes(state: ReportHttpState, auth: AdminAuthHttpState) -> Router<HttpState> {
    Router::new()
        .route("/api/admin/reports", get(list))
        .route("/api/admin/reports/{id}", patch(update))
        .layer(Extension(state))
        .layer(Extension(auth))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBody {
    reported_user_id: String,
    match_id: Option<String>,
    reason: ReportReason,
    description: Option<String>,
}

impl ApiDto for CreateBody {
    const ERROR_CODE: &'static str = "invalid_report_payload";
    const ERROR_MESSAGE: &'static str = "The report request body is invalid.";

    fn is_valid(&self) -> bool {
        self.reported_user_id().is_some() && self.match_id().is_some()
    }
}

impl CreateBody {
    fn reported_user_id(&self) -> Option<Uuid> {
        canonical_uuid(&self.reported_user_id)
    }

    fn match_id(&self) -> Option<Option<Uuid>> {
        match self.match_id.as_deref() {
            None => Some(None),
            Some(value) => canonical_uuid(value).map(Some),
        }
    }
}

async fn create(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<ReportHttpState>,
    ValidatedJson(body): ValidatedJson<CreateBody>,
) -> Result<(StatusCode, Json<PublicReport>), ApiError> {
    state
        .limiter
        .enforce(
            "reports",
            &identity.account.user_id.to_string(),
            &state.policy,
            "report_rate_limit_exceeded",
        )
        .await?;
    state
        .service
        .create(
            identity.account.user_id,
            CreateReportInput {
                reported_user_id: body.reported_user_id().ok_or_else(invalid_report_payload)?,
                match_id: body.match_id().ok_or_else(invalid_report_payload)?,
                reason: body.reason,
                description: body.description,
            },
        )
        .await
        .map(|report| (StatusCode::CREATED, Json(report)))
        .map_err(report_error)
}

#[cfg(feature = "webauthn-probe")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    status: Option<ReportStatus>,
    limit: Option<String>,
    offset: Option<String>,
    cursor: Option<String>,
}

#[cfg(feature = "webauthn-probe")]
impl ListQuery {
    fn limit(&self) -> Option<u32> {
        javascript_number(&self.limit, 20)
    }

    fn offset(&self) -> Option<u32> {
        javascript_number(&self.offset, 0)
    }
}

#[cfg(feature = "webauthn-probe")]
impl ApiDto for ListQuery {
    const ERROR_CODE: &'static str = "invalid_report_request";
    const ERROR_MESSAGE: &'static str = "The report request is invalid.";

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
struct ListResponse {
    reports: Vec<PublicReport>,
    next_cursor: Option<String>,
}

#[cfg(feature = "webauthn-probe")]
async fn list(
    AdminIdentity(_identity): AdminIdentity,
    Extension(state): Extension<ReportHttpState>,
    ValidatedQuery(query): ValidatedQuery<ListQuery>,
) -> Result<Json<ListResponse>, ApiError> {
    let super::service::ReportPage { items, next_cursor } = state
        .service
        .list(
            query.status,
            query.limit().ok_or_else(invalid_report_request)?,
            query.offset().ok_or_else(invalid_report_request)?,
            query.cursor.as_deref(),
        )
        .await
        .map_err(report_error)?;
    Ok(Json(ListResponse {
        reports: items,
        next_cursor,
    }))
}

#[cfg(feature = "webauthn-probe")]
#[derive(Deserialize)]
struct ReportPath {
    id: String,
}

#[cfg(feature = "webauthn-probe")]
impl ReportPath {
    fn id(&self) -> Option<Uuid> {
        canonical_uuid(&self.id)
    }
}

#[cfg(feature = "webauthn-probe")]
impl ApiDto for ReportPath {
    const ERROR_CODE: &'static str = "invalid_report_id";
    const ERROR_MESSAGE: &'static str = "The report ID must be a valid UUID.";

    fn is_valid(&self) -> bool {
        self.id().is_some()
    }
}

#[cfg(feature = "webauthn-probe")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateBody {
    status: ReportStatus,
}

#[cfg(feature = "webauthn-probe")]
impl ApiDto for UpdateBody {
    const ERROR_CODE: &'static str = "invalid_report_payload";
    const ERROR_MESSAGE: &'static str = "The report request body is invalid.";
}

#[cfg(feature = "webauthn-probe")]
#[derive(Serialize)]
struct MessageResponse {
    message: &'static str,
}

#[cfg(feature = "webauthn-probe")]
async fn update(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<ReportHttpState>,
    ValidatedPath(path): ValidatedPath<ReportPath>,
    ValidatedJson(body): ValidatedJson<UpdateBody>,
) -> Result<Json<MessageResponse>, ApiError> {
    state
        .service
        .update_status(
            path.id().ok_or_else(invalid_report_id)?,
            body.status,
            identity.user_id,
            identity.role,
        )
        .await
        .map_err(report_error)?;
    Ok(Json(MessageResponse {
        message: "report updated",
    }))
}

#[cfg(feature = "webauthn-probe")]
fn javascript_number(value: &Option<String>, default: u32) -> Option<u32> {
    let Some(value) = value else {
        return Some(default);
    };
    let value = value.trim();
    if value.is_empty() {
        return Some(0);
    }
    let number = value.parse::<f64>().ok()?;
    if !number.is_finite()
        || number.fract() != 0.0
        || !(0.0..=f64::from(u32::MAX)).contains(&number)
    {
        return None;
    }
    Some(number as u32)
}

fn canonical_uuid(value: &str) -> Option<Uuid> {
    let parsed = Uuid::parse_str(value).ok()?;
    (value.len() == 36
        && [8, 13, 18, 23]
            .iter()
            .all(|index| value.as_bytes()[*index] == b'-')
        && parsed.hyphenated().to_string().eq_ignore_ascii_case(value)
        && (1..=8).contains(&parsed.get_version_num())
        && parsed.get_variant() == Variant::RFC4122)
        .then_some(parsed)
}

fn invalid_report_payload() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_report_payload",
        "The report request body is invalid.",
    )
}

#[cfg(feature = "webauthn-probe")]
fn invalid_report_id() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_report_id",
        "The report ID must be a valid UUID.",
    )
}

fn invalid_report_request() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_report_request",
        "The report request is invalid.",
    )
}

fn report_error(error: ReportError) -> ApiError {
    match error {
        ReportError::InvalidRequest => invalid_report_request(),
        ReportError::InvalidCursor => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_cursor",
            "The pagination cursor is invalid.",
        ),
        ReportError::AccountNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "account_not_found",
            "The account could not be found or has been deleted.",
        ),
        ReportError::MatchNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "match_not_found",
            "The match could not be found.",
        ),
        ReportError::AlreadyPending => ApiError::new(
            StatusCode::CONFLICT,
            "report_already_pending",
            "A pending report already exists for this user.",
        ),
        ReportError::ReportNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "report_not_found",
            "The report could not be found.",
        ),
        ReportError::Database(error) => error.into(),
    }
}
