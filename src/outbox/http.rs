use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant, Version};

use super::admin::{
    DeadLetter, DeadLetterPage, OutboxAdminError, OutboxAdminService, OutboxOperator,
};
use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedJson, ValidatedPath, ValidatedQuery};
use crate::http::router::HttpState;
use crate::identity::admin::http::{AdminAuthHttpState, AdminIdentity, RecentAdminIdentity};
use crate::shared::text::validator_js_length;

#[derive(Clone)]
pub struct OutboxAdminHttpState {
    service: OutboxAdminService,
}

impl OutboxAdminHttpState {
    pub fn new(service: OutboxAdminService) -> Self {
        Self { service }
    }
}

pub fn routes(state: OutboxAdminHttpState, auth: AdminAuthHttpState) -> Router<HttpState> {
    Router::new()
        .route("/api/admin/outbox/dead-letters", get(dead_letters))
        .route("/api/admin/outbox/{id}/retry", post(retry))
        .route("/api/admin/outbox/{id}/discard", post(discard))
        .layer(Extension(state))
        .layer(Extension(auth))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    limit: Option<String>,
    cursor: Option<String>,
}

impl ListQuery {
    fn limit(&self) -> Option<u32> {
        javascript_number(&self.limit, 20)
    }
}

impl ApiDto for ListQuery {
    const ERROR_CODE: &'static str = "invalid_outbox_request";
    const ERROR_MESSAGE: &'static str = "The outbox administrator request is invalid.";

    fn is_valid(&self) -> bool {
        self.limit().is_some_and(|limit| (1..=100).contains(&limit))
            && self
                .cursor
                .as_ref()
                .is_none_or(|value| validator_js_length(value) <= 512)
    }
}

#[derive(Serialize)]
struct ListResponse {
    events: Vec<DeadLetter>,
    next_cursor: Option<String>,
}

async fn dead_letters(
    AdminIdentity(_identity): AdminIdentity,
    Extension(state): Extension<OutboxAdminHttpState>,
    ValidatedQuery(query): ValidatedQuery<ListQuery>,
) -> Result<Json<ListResponse>, ApiError> {
    let DeadLetterPage {
        events,
        next_cursor,
    } = state
        .service
        .dead_letters(
            query.limit().ok_or_else(invalid_request)?,
            query.cursor.as_deref(),
        )
        .await
        .map_err(map_error)?;
    Ok(Json(ListResponse {
        events,
        next_cursor,
    }))
}

#[derive(Deserialize)]
struct EventPath {
    id: String,
}

impl EventPath {
    fn id(&self) -> Option<Uuid> {
        let parsed = Uuid::parse_str(&self.id).ok()?;
        (parsed.get_version() == Some(Version::Random)
            && parsed.get_variant() == Variant::RFC4122
            && self.id.len() == 36
            && parsed
                .hyphenated()
                .to_string()
                .eq_ignore_ascii_case(&self.id))
        .then_some(parsed)
    }
}

impl ApiDto for EventPath {
    const ERROR_CODE: &'static str = "invalid_outbox_request";
    const ERROR_MESSAGE: &'static str = "The outbox administrator request is invalid.";

    fn is_valid(&self) -> bool {
        self.id().is_some()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveBody {
    reason: String,
}

impl ApiDto for ResolveBody {
    const ERROR_CODE: &'static str = "invalid_outbox_request";
    const ERROR_MESSAGE: &'static str = "The outbox administrator request is invalid.";

    fn is_valid(&self) -> bool {
        (3..=500).contains(&validator_js_length(&self.reason))
    }
}

#[derive(Serialize)]
struct RetryResponse {
    message: &'static str,
}

async fn retry(
    RecentAdminIdentity(identity): RecentAdminIdentity,
    Extension(state): Extension<OutboxAdminHttpState>,
    ValidatedPath(path): ValidatedPath<EventPath>,
    ValidatedJson(body): ValidatedJson<ResolveBody>,
) -> Result<impl IntoResponse, ApiError> {
    state
        .service
        .retry(
            path.id().ok_or_else(invalid_request)?,
            OutboxOperator {
                user_id: identity.user_id,
                role: identity.role,
            },
            &body.reason,
        )
        .await
        .map_err(map_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(RetryResponse {
            message: "outbox event queued",
        }),
    ))
}

async fn discard(
    RecentAdminIdentity(identity): RecentAdminIdentity,
    Extension(state): Extension<OutboxAdminHttpState>,
    ValidatedPath(path): ValidatedPath<EventPath>,
    ValidatedJson(body): ValidatedJson<ResolveBody>,
) -> Result<StatusCode, ApiError> {
    state
        .service
        .discard(
            path.id().ok_or_else(invalid_request)?,
            OutboxOperator {
                user_id: identity.user_id,
                role: identity.role,
            },
            &body.reason,
        )
        .await
        .map_err(map_error)?;
    Ok(StatusCode::NO_CONTENT)
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

fn invalid_request() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_outbox_request",
        "The outbox administrator request is invalid.",
    )
}

fn map_error(error: OutboxAdminError) -> ApiError {
    match error {
        OutboxAdminError::InvalidRequest => invalid_request(),
        OutboxAdminError::InvalidCursor => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_cursor",
            "The pagination cursor is invalid.",
        ),
        OutboxAdminError::EventNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "outbox_event_not_found",
            "The outbox event could not be found.",
        ),
        OutboxAdminError::EventNotDeadLetter => ApiError::new(
            StatusCode::CONFLICT,
            "outbox_event_not_dead_letter",
            "The outbox event is no longer a dead letter.",
        ),
        OutboxAdminError::DiscardNotAllowed => ApiError::new(
            StatusCode::CONFLICT,
            "outbox_discard_not_allowed",
            "The outbox event cannot be safely discarded.",
        ),
        OutboxAdminError::Database(error) => error.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_uuid_v4_limits_unknown_fields_and_public_errors() {
        let id = Uuid::new_v4();
        let path = EventPath {
            id: id.hyphenated().to_string(),
        };
        assert!(path.is_valid());
        assert!(
            !EventPath {
                id: Uuid::nil().to_string()
            }
            .is_valid()
        );
        assert!(serde_json::from_str::<ResolveBody>(r#"{"reason":"Relance"}"#).is_ok());
        assert!(
            serde_json::from_str::<ResolveBody>(r#"{"reason":"Relance","force":true}"#).is_err()
        );
        assert_eq!(
            map_error(OutboxAdminError::DiscardNotAllowed).code(),
            "outbox_discard_not_allowed"
        );
    }

    #[test]
    fn preserves_query_defaults_and_every_public_operator_status() {
        assert_eq!(
            ListQuery {
                limit: None,
                cursor: None,
            }
            .limit(),
            Some(20)
        );
        assert_eq!(
            ListQuery {
                limit: Some("1e2".to_owned()),
                cursor: None,
            }
            .limit(),
            Some(100)
        );
        assert_eq!(
            ListQuery {
                limit: Some("1.5".to_owned()),
                cursor: None,
            }
            .limit(),
            None
        );

        let cases = [
            (
                OutboxAdminError::InvalidRequest,
                StatusCode::BAD_REQUEST,
                "invalid_outbox_request",
            ),
            (
                OutboxAdminError::InvalidCursor,
                StatusCode::BAD_REQUEST,
                "invalid_cursor",
            ),
            (
                OutboxAdminError::EventNotFound,
                StatusCode::NOT_FOUND,
                "outbox_event_not_found",
            ),
            (
                OutboxAdminError::EventNotDeadLetter,
                StatusCode::CONFLICT,
                "outbox_event_not_dead_letter",
            ),
            (
                OutboxAdminError::DiscardNotAllowed,
                StatusCode::CONFLICT,
                "outbox_discard_not_allowed",
            ),
        ];
        for (error, status, code) in cases {
            let api_error = map_error(error);
            assert_eq!(api_error.code(), code);
            assert_eq!(api_error.into_response().status(), status);
        }
    }
}
