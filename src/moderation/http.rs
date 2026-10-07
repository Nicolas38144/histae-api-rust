use std::fmt;

use axum::extract::Extension;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use super::domain::{
    ModerationContentType, ModerationDecision, ModerationReviewInput, PhotoReviewChecks,
};
use super::service::{ModerationError, ModerationService, Page};
use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedJson, ValidatedPath, ValidatedQuery};
use crate::http::router::HttpState;
use crate::identity::admin::http::{AdminAuthHttpState, AdminIdentity};
use crate::profiles::domain::ModerationStatus;
use crate::shared::text::validator_js_length;

#[derive(Clone)]
pub struct ModerationHttpState {
    service: ModerationService,
}

impl ModerationHttpState {
    pub fn new(service: ModerationService) -> Self {
        Self { service }
    }
}

pub fn routes(state: ModerationHttpState, auth: AdminAuthHttpState) -> Router<HttpState> {
    Router::new()
        .route("/api/admin/content-moderation", get(list))
        .route(
            "/api/admin/content-moderation/{id}",
            get(detail).patch(review),
        )
        .layer(Extension(state))
        .layer(Extension(auth))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    status: Option<ModerationStatus>,
    content_type: Option<ModerationContentType>,
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
    const ERROR_CODE: &'static str = "invalid_moderation_request";
    const ERROR_MESSAGE: &'static str = "The moderation request is invalid.";

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

#[derive(Serialize)]
struct ListResponse {
    cases: Vec<super::domain::ModerationCase>,
    next_cursor: Option<String>,
}

async fn list(
    AdminIdentity(_identity): AdminIdentity,
    Extension(state): Extension<ModerationHttpState>,
    ValidatedQuery(query): ValidatedQuery<ListQuery>,
) -> Result<Json<ListResponse>, ApiError> {
    let Page { items, next_cursor } = state
        .service
        .list(
            query.status,
            query.content_type,
            query.limit,
            query.offset,
            query.cursor.as_deref(),
        )
        .await
        .map_err(moderation_error)?;
    Ok(Json(ListResponse {
        cases: items,
        next_cursor,
    }))
}

#[derive(Deserialize)]
struct CasePath {
    #[serde(deserialize_with = "crate::shared::validation::deserialize_uuid")]
    id: Uuid,
}

impl ApiDto for CasePath {
    const ERROR_CODE: &'static str = "invalid_moderation_case_id";
    const ERROR_MESSAGE: &'static str = "The moderation case ID must be a valid UUID.";

    fn is_valid(&self) -> bool {
        crate::shared::validation::uuid_all(self.id)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessQuery {
    reason: Option<String>,
}

impl ApiDto for AccessQuery {
    const ERROR_CODE: &'static str = "invalid_moderation_request";
    const ERROR_MESSAGE: &'static str = "The moderation request is invalid.";

    fn is_valid(&self) -> bool {
        self.reason
            .as_ref()
            .is_none_or(|reason| validator_js_length(reason) <= 500)
    }
}

async fn detail(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<ModerationHttpState>,
    ValidatedPath(path): ValidatedPath<CasePath>,
    ValidatedQuery(query): ValidatedQuery<AccessQuery>,
) -> Result<Json<super::domain::ModerationDetail>, ApiError> {
    state
        .service
        .detail(
            path.id,
            identity.user_id,
            identity.role,
            query.reason.as_deref().unwrap_or_default(),
        )
        .await
        .map(Json)
        .map_err(moderation_error)
}

#[derive(Clone, Copy, Debug)]
struct PositiveInteger(i32);

impl<'de> Deserialize<'de> for PositiveInteger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(IntegerVisitor)
    }
}

struct IntegerVisitor;

impl Visitor<'_> for IntegerVisitor {
    type Value = PositiveInteger;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JavaScript-coercible positive integer")
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        i32::try_from(value)
            .map(PositiveInteger)
            .map_err(|_| E::custom("integer is outside i32"))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        i32::try_from(value)
            .map(PositiveInteger)
            .map_err(|_| E::custom("integer is outside i32"))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value.is_finite()
            && value.fract() == 0.0
            && value >= i32::MIN as f64
            && value <= i32::MAX as f64
        {
            Ok(PositiveInteger(value as i32))
        } else {
            Err(E::custom("value must be an integer"))
        }
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        crate::shared::validation::javascript_number(value)
            .ok_or(())
            .map_err(|_| E::custom("value must be numeric"))
            .and_then(|value| self.visit_f64(value))
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(PositiveInteger(i32::from(value)))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewBody {
    version: PositiveInteger,
    decision: ModerationDecision,
    reason: Option<String>,
    photo_checks: Option<PhotoReviewChecks>,
}

impl ApiDto for ReviewBody {
    const ERROR_CODE: &'static str = "invalid_moderation_request";
    const ERROR_MESSAGE: &'static str = "The moderation request is invalid.";

    fn is_valid(&self) -> bool {
        self.version.0 >= 1
            && self
                .reason
                .as_ref()
                .is_none_or(|reason| validator_js_length(reason) <= 500)
    }
}

#[derive(Serialize)]
struct MessageResponse {
    message: &'static str,
}

async fn review(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<ModerationHttpState>,
    ValidatedPath(path): ValidatedPath<CasePath>,
    ValidatedJson(body): ValidatedJson<ReviewBody>,
) -> Result<Json<MessageResponse>, ApiError> {
    state
        .service
        .review(
            path.id,
            ModerationReviewInput {
                version: body.version.0,
                decision: body.decision,
                reason: body.reason.unwrap_or_default(),
                photo_checks: body.photo_checks,
            },
            identity.user_id,
            identity.role,
        )
        .await
        .map_err(moderation_error)?;
    Ok(Json(MessageResponse {
        message: "content moderation decision recorded",
    }))
}

fn moderation_error(error: ModerationError) -> ApiError {
    match error {
        ModerationError::InvalidRequest => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_moderation_request",
            "The moderation request is invalid.",
        ),
        ModerationError::InvalidCursor => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_cursor",
            "The pagination cursor is invalid.",
        ),
        ModerationError::NotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "moderation_case_not_found",
            "The moderation case could not be found.",
        ),
        ModerationError::Stale => ApiError::new(
            StatusCode::CONFLICT,
            "moderation_case_stale",
            "The moderation case has changed; refresh it before reviewing.",
        ),
        ModerationError::ReviewNotAllowed => ApiError::new(
            StatusCode::CONFLICT,
            "moderation_review_not_allowed",
            "This moderation decision is not valid for the current content.",
        ),
        ModerationError::PhotoStorageUnavailable => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "photo_storage_unavailable",
            "Photo storage is temporarily unavailable",
        ),
        ModerationError::Database(error) => error.into(),
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

    #[test]
    fn account_erasure_sql_guard_remains_a_conflict() {
        let error = moderation_error(ModerationError::Database(
            crate::infra::postgres::DatabaseError::AccountUnavailable,
        ));
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(error.code(), "account_unavailable");
    }

    #[test]
    fn coerces_the_version_like_class_transformer() {
        let body: ReviewBody = serde_json::from_str(
            r#"{"version":"2","decision":"approved","reason":"Validation humaine","photo_checks":{"face_detectable":true,"sharp_enough":true,"content_allowed":true}}"#,
        )
        .unwrap_or_else(|error| panic!("valid review: {error}"));
        assert_eq!(body.version.0, 2);
        assert!(body.is_valid());
    }

    #[test]
    fn rejects_unknown_review_fields() {
        let body = serde_json::from_str::<ReviewBody>(
            r#"{"version":1,"decision":"approved","reason":"Validation humaine","privilege":true}"#,
        );
        assert!(body.is_err());
    }
}
