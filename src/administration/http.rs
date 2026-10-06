use axum::extract::Extension;
use axum::http::StatusCode;
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::domain::{AdminUser, AdminUserDetail, AdminUserRole, AdminUserStatus};
use super::metrics::{AdminMetrics, AdminMetricsService, AdminRevenue, RevenuePeriod};
use super::service::{AdministrationError, AdministrationService, AuditedPageRequest, Page};
use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedJson, ValidatedPath, ValidatedQuery};
use crate::http::router::HttpState;
use crate::identity::admin::http::{AdminAuthHttpState, AdminIdentity};
use crate::identity::admin_role::AdminRole;
use crate::matches::domain::{PublicMatch, PublicMessage};
use crate::shared::text::validator_js_length;

#[derive(Clone)]
pub struct AdministrationHttpState {
    service: AdministrationService,
    metrics: AdminMetricsService,
}

impl AdministrationHttpState {
    pub fn new(service: AdministrationService, metrics: AdminMetricsService) -> Self {
        Self { service, metrics }
    }
}

pub fn routes(state: AdministrationHttpState, auth: AdminAuthHttpState) -> Router<HttpState> {
    Router::new()
        .route("/api/admin/me", get(me))
        .route("/api/admin/metrics", get(metrics))
        .route("/api/admin/revenue", get(revenue))
        .route("/api/admin/users", get(users))
        .route("/api/admin/users/{id}", get(user_detail))
        .route("/api/admin/users/{id}/status", patch(update_status))
        .route("/api/admin/users/{id}/role", patch(update_role))
        .route("/api/matches/{userId}", get(matches))
        .route("/api/admin/matches/{id}/messages", get(messages))
        .layer(Extension(state))
        .layer(Extension(auth))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevenueQuery {
    revenue_period: Option<RevenuePeriod>,
}

impl RevenueQuery {
    fn period(&self) -> RevenuePeriod {
        self.revenue_period.unwrap_or(RevenuePeriod::MonthToDate)
    }
}

impl ApiDto for RevenueQuery {
    const ERROR_CODE: &'static str = "invalid_admin_request";
    const ERROR_MESSAGE: &'static str = "The administrator request is invalid.";
    fn is_valid(&self) -> bool {
        true
    }
}

async fn metrics(
    AdminIdentity(_identity): AdminIdentity,
    Extension(state): Extension<AdministrationHttpState>,
    ValidatedQuery(query): ValidatedQuery<RevenueQuery>,
) -> Result<Json<AdminMetrics>, ApiError> {
    state
        .metrics
        .metrics(query.period())
        .await
        .map(Json)
        .map_err(Into::into)
}

async fn revenue(
    AdminIdentity(_identity): AdminIdentity,
    Extension(state): Extension<AdministrationHttpState>,
    ValidatedQuery(query): ValidatedQuery<RevenueQuery>,
) -> Result<Json<AdminRevenue>, ApiError> {
    state
        .metrics
        .revenue(query.period())
        .await
        .map(Json)
        .map_err(Into::into)
}

#[derive(Serialize)]
struct MeResponse {
    user_id: Uuid,
    role: AdminRole,
}

async fn me(AdminIdentity(identity): AdminIdentity) -> Json<MeResponse> {
    Json(MeResponse {
        user_id: identity.user_id,
        role: identity.role,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UsersQuery {
    status: Option<AdminUserStatus>,
    role: Option<AdminUserRole>,
    search: Option<String>,
    limit: Option<String>,
    offset: Option<String>,
    cursor: Option<String>,
}

impl UsersQuery {
    fn limit(&self) -> Option<u32> {
        javascript_number(&self.limit, 20)
    }

    fn offset(&self) -> Option<u32> {
        javascript_number(&self.offset, 0)
    }
}

impl ApiDto for UsersQuery {
    const ERROR_CODE: &'static str = "invalid_admin_request";
    const ERROR_MESSAGE: &'static str = "The administrator request is invalid.";

    fn is_valid(&self) -> bool {
        self.limit().is_some_and(|limit| (1..=100).contains(&limit))
            && self.offset().is_some()
            && self
                .search
                .as_ref()
                .is_none_or(|value| validator_js_length(value) <= 100)
            && self
                .cursor
                .as_ref()
                .is_none_or(|value| validator_js_length(value) <= 512)
    }
}

#[derive(Serialize)]
struct UsersResponse {
    users: Vec<AdminUser>,
    next_cursor: Option<String>,
}

async fn users(
    AdminIdentity(_identity): AdminIdentity,
    Extension(state): Extension<AdministrationHttpState>,
    ValidatedQuery(query): ValidatedQuery<UsersQuery>,
) -> Result<Json<UsersResponse>, ApiError> {
    let Page { items, next_cursor } = state
        .service
        .users(
            query.status,
            query.role,
            query.search.as_deref(),
            query.limit().ok_or_else(invalid_admin_request)?,
            query.offset().ok_or_else(invalid_admin_request)?,
            query.cursor.as_deref(),
        )
        .await
        .map_err(administration_error)?;
    Ok(Json(UsersResponse {
        users: items,
        next_cursor,
    }))
}

#[derive(Deserialize)]
struct IdPath {
    id: String,
}

impl IdPath {
    fn id(&self) -> Option<Uuid> {
        canonical_uuid(&self.id)
    }
}

impl ApiDto for IdPath {
    const ERROR_CODE: &'static str = "invalid_user_id";
    const ERROR_MESSAGE: &'static str = "The user ID must be a valid UUID.";

    fn is_valid(&self) -> bool {
        self.id().is_some()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessQuery {
    reason: String,
}

impl ApiDto for AccessQuery {
    const ERROR_CODE: &'static str = "invalid_admin_request";
    const ERROR_MESSAGE: &'static str = "The administrator request is invalid.";

    fn is_valid(&self) -> bool {
        (3..=500).contains(&validator_js_length(&self.reason))
    }
}

async fn user_detail(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<AdministrationHttpState>,
    ValidatedPath(path): ValidatedPath<IdPath>,
    ValidatedQuery(query): ValidatedQuery<AccessQuery>,
) -> Result<Json<AdminUserDetail>, ApiError> {
    state
        .service
        .user_detail(
            path.id().ok_or_else(invalid_user_id)?,
            identity.user_id,
            identity.role,
            &query.reason,
        )
        .await
        .map(Json)
        .map_err(administration_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateStatusBody {
    is_banned: bool,
    reason: Option<String>,
}

impl ApiDto for UpdateStatusBody {
    const ERROR_CODE: &'static str = "invalid_admin_request";
    const ERROR_MESSAGE: &'static str = "The administrator request is invalid.";

    fn is_valid(&self) -> bool {
        self.reason
            .as_ref()
            .is_none_or(|value| validator_js_length(value) <= 500)
    }
}

#[derive(Serialize)]
struct MessageResponse {
    message: &'static str,
}

async fn update_status(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<AdministrationHttpState>,
    ValidatedPath(path): ValidatedPath<IdPath>,
    ValidatedJson(body): ValidatedJson<UpdateStatusBody>,
) -> Result<Json<MessageResponse>, ApiError> {
    state
        .service
        .update_ban(
            path.id().ok_or_else(invalid_user_id)?,
            body.is_banned,
            body.reason.as_deref(),
            identity.user_id,
            identity.role,
        )
        .await
        .map_err(administration_error)?;
    Ok(Json(MessageResponse {
        message: if body.is_banned {
            "account banned"
        } else {
            "account unbanned"
        },
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateRoleBody {
    role: AdminUserRole,
    reason: String,
}

impl ApiDto for UpdateRoleBody {
    const ERROR_CODE: &'static str = "invalid_admin_request";
    const ERROR_MESSAGE: &'static str = "The administrator request is invalid.";

    fn is_valid(&self) -> bool {
        self.role != AdminUserRole::Superadmin && validator_js_length(&self.reason) <= 500
    }
}

async fn update_role(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<AdministrationHttpState>,
    ValidatedPath(path): ValidatedPath<IdPath>,
    ValidatedJson(body): ValidatedJson<UpdateRoleBody>,
) -> Result<Json<MessageResponse>, ApiError> {
    state
        .service
        .update_role(
            path.id().ok_or_else(invalid_user_id)?,
            body.role,
            &body.reason,
            identity.user_id,
            identity.role,
        )
        .await
        .map_err(administration_error)?;
    Ok(Json(MessageResponse {
        message: if body.role == AdminUserRole::Admin {
            "account is administrator"
        } else {
            "administrator access removed"
        },
    }))
}

#[derive(Deserialize)]
struct UserPath {
    #[serde(rename = "userId")]
    user_id: String,
}

impl UserPath {
    fn id(&self) -> Option<Uuid> {
        canonical_uuid(&self.user_id)
    }
}

impl ApiDto for UserPath {
    const ERROR_CODE: &'static str = "invalid_user_id";
    const ERROR_MESSAGE: &'static str = "The user ID must be a valid UUID.";

    fn is_valid(&self) -> bool {
        self.id().is_some()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessPageQuery {
    reason: String,
    limit: Option<String>,
    offset: Option<String>,
    cursor: Option<String>,
}

impl AccessPageQuery {
    fn limit(&self) -> Option<u32> {
        javascript_number(&self.limit, 20)
    }

    fn offset(&self) -> Option<u32> {
        javascript_number(&self.offset, 0)
    }
}

impl ApiDto for AccessPageQuery {
    const ERROR_CODE: &'static str = "invalid_pagination";
    const ERROR_MESSAGE: &'static str = "Pagination parameters are invalid.";

    fn is_valid(&self) -> bool {
        (3..=500).contains(&validator_js_length(&self.reason))
            && self.limit().is_some_and(|limit| (1..=100).contains(&limit))
            && self.offset().is_some()
            && self
                .cursor
                .as_ref()
                .is_none_or(|value| validator_js_length(value) <= 512)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminMessageQuery {
    reason: String,
    limit: Option<String>,
    offset: Option<String>,
    cursor: Option<String>,
}

impl AdminMessageQuery {
    fn limit(&self) -> Option<u32> {
        javascript_number(&self.limit, 20)
    }

    fn offset(&self) -> Option<u32> {
        javascript_number(&self.offset, 0)
    }
}

impl ApiDto for AdminMessageQuery {
    const ERROR_CODE: &'static str = "invalid_admin_request";
    const ERROR_MESSAGE: &'static str = "The administrator request is invalid.";

    fn is_valid(&self) -> bool {
        (3..=500).contains(&validator_js_length(&self.reason))
            && self.limit().is_some_and(|limit| (1..=100).contains(&limit))
            && self.offset().is_some()
            && self
                .cursor
                .as_ref()
                .is_none_or(|value| validator_js_length(value) <= 512)
    }
}

#[derive(Serialize)]
struct MatchesResponse {
    matches: Vec<PublicMatch>,
    next_cursor: Option<String>,
}

async fn matches(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<AdministrationHttpState>,
    ValidatedPath(path): ValidatedPath<UserPath>,
    ValidatedQuery(query): ValidatedQuery<AccessPageQuery>,
) -> Result<Json<MatchesResponse>, ApiError> {
    let Page { items, next_cursor } = state
        .service
        .matches(AuditedPageRequest {
            resource_id: path.id().ok_or_else(invalid_user_id)?,
            admin_id: identity.user_id,
            admin_role: identity.role,
            reason: &query.reason,
            limit: query.limit().ok_or_else(invalid_admin_request)?,
            offset: query.offset().ok_or_else(invalid_admin_request)?,
            cursor: query.cursor.as_deref(),
        })
        .await
        .map_err(administration_error)?;
    Ok(Json(MatchesResponse {
        matches: items,
        next_cursor,
    }))
}

#[derive(Deserialize)]
struct MatchPath {
    id: String,
}

impl MatchPath {
    fn id(&self) -> Option<Uuid> {
        canonical_uuid(&self.id)
    }
}

impl ApiDto for MatchPath {
    const ERROR_CODE: &'static str = "invalid_match_id";
    const ERROR_MESSAGE: &'static str = "The match ID must be a valid UUID.";

    fn is_valid(&self) -> bool {
        self.id().is_some()
    }
}

#[derive(Serialize)]
struct MessagesResponse {
    messages: Vec<PublicMessage>,
    next_cursor: Option<String>,
}

async fn messages(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<AdministrationHttpState>,
    ValidatedPath(path): ValidatedPath<MatchPath>,
    ValidatedQuery(query): ValidatedQuery<AdminMessageQuery>,
) -> Result<Json<MessagesResponse>, ApiError> {
    let Page { items, next_cursor } = state
        .service
        .messages(AuditedPageRequest {
            resource_id: path.id().ok_or_else(invalid_match_id)?,
            admin_id: identity.user_id,
            admin_role: identity.role,
            reason: &query.reason,
            limit: query.limit().ok_or_else(invalid_admin_request)?,
            offset: query.offset().ok_or_else(invalid_admin_request)?,
            cursor: query.cursor.as_deref(),
        })
        .await
        .map_err(administration_error)?;
    Ok(Json(MessagesResponse {
        messages: items,
        next_cursor,
    }))
}

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

fn invalid_admin_request() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_admin_request",
        "The administrator request is invalid.",
    )
}

fn invalid_user_id() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_user_id",
        "The user ID must be a valid UUID.",
    )
}

fn invalid_match_id() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_match_id",
        "The match ID must be a valid UUID.",
    )
}

fn administration_error(error: AdministrationError) -> ApiError {
    match error {
        AdministrationError::InvalidRequest => invalid_admin_request(),
        AdministrationError::InvalidCursor => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_cursor",
            "The pagination cursor is invalid.",
        ),
        AdministrationError::AccountNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "account_not_found",
            "The account could not be found or has been deleted.",
        ),
        AdministrationError::MatchNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "match_not_found",
            "The match could not be found.",
        ),
        AdministrationError::ActionForbidden => ApiError::new(
            StatusCode::FORBIDDEN,
            "admin_action_forbidden",
            "The administrator cannot change this account.",
        ),
        AdministrationError::PhotoStorageUnavailable => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "photo_storage_unavailable",
            "Photo storage is temporarily unavailable",
        ),
        AdministrationError::Database(error) => error.into(),
    }
}

#[cfg(test)]
mod metrics_query_tests {
    use super::*;

    #[test]
    fn role_request_never_accepts_a_second_superadmin() {
        for role in [AdminUserRole::User, AdminUserRole::Admin] {
            assert!(
                UpdateRoleBody {
                    role,
                    reason: "Motif valide".to_owned(),
                }
                .is_valid()
            );
        }
        assert!(
            !UpdateRoleBody {
                role: AdminUserRole::Superadmin,
                reason: "Motif valide".to_owned(),
            }
            .is_valid()
        );
    }

    #[test]
    fn revenue_period_defaults_to_month_to_date() {
        assert_eq!(
            RevenueQuery {
                revenue_period: None,
            }
            .period(),
            RevenuePeriod::MonthToDate
        );
    }
}
