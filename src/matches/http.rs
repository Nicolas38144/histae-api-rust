use axum::extract::Extension;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant};

use super::domain::{ContinuationQuota, PublicMessage, PublicUserMatch};
use super::service::{MatchError, MatchPage, MatchService, MessagePage};
use crate::config::LimitPolicy;
use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedPath, ValidatedQuery};
use crate::http::rate_limit::RateLimiter;
use crate::http::router::HttpState;
use crate::identity::mobile::http::{MobileAuthState, OnboardedMobile};
use crate::shared::text::validator_js_length;

#[derive(Clone)]
pub struct MatchHttpState {
    service: MatchService,
    limiter: RateLimiter,
    message_policy: LimitPolicy,
}

impl MatchHttpState {
    pub fn new(service: MatchService, limiter: RateLimiter, message_policy: LimitPolicy) -> Self {
        Self {
            service,
            limiter,
            message_policy,
        }
    }
}

pub fn routes(state: MatchHttpState, auth: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route("/api/matches/me", get(list))
        .route("/api/matches/{id}/reveal", patch(reveal))
        .route("/api/matches/{id}/continue", patch(continue_match))
        .route(
            "/api/matches/{id}/messages",
            get(list_messages).post(send_message),
        )
        .route(
            "/api/matches/{id}/messages/read",
            patch(mark_messages_read_through),
        )
        .route(
            "/api/matches/{id}/messages/{msgId}/read",
            patch(mark_message_read),
        )
        .route("/api/users/me/continuation-quota", get(continuation_quota))
        .layer(Extension(state))
        .layer(Extension(auth))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PaginationQuery {
    limit: Option<String>,
    offset: Option<String>,
    cursor: Option<String>,
}

impl PaginationQuery {
    fn limit(&self) -> Option<u32> {
        javascript_number(&self.limit, 20)
    }

    fn offset(&self) -> Option<u32> {
        javascript_number(&self.offset, 0)
    }
}

impl ApiDto for PaginationQuery {
    const ERROR_CODE: &'static str = "invalid_pagination";
    const ERROR_MESSAGE: &'static str = "Pagination parameters are invalid.";

    fn is_valid(&self) -> bool {
        self.limit().is_some_and(|limit| (1..=100).contains(&limit))
            && self.offset().is_some()
            && self
                .cursor
                .as_ref()
                .is_none_or(|value| validator_js_length(value) <= 512)
    }
}

fn javascript_number(value: &Option<String>, default: u32) -> Option<u32> {
    let Some(value) = value else {
        return Some(default);
    };
    let value = value.trim();
    if value.is_empty() {
        return Some(0);
    }
    let number = value.parse::<f64>().ok()?;
    if !number.is_finite() || number.fract() != 0.0 || number < 0.0 || number > f64::from(u32::MAX)
    {
        return None;
    }
    Some(number as u32)
}

#[derive(Serialize)]
struct ListResponse {
    matches: Vec<PublicUserMatch>,
    next_cursor: Option<String>,
}

async fn list(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<MatchHttpState>,
    ValidatedQuery(query): ValidatedQuery<PaginationQuery>,
) -> Result<Json<ListResponse>, ApiError> {
    let limit = query.limit().ok_or_else(invalid_pagination)?;
    let offset = query.offset().ok_or_else(invalid_pagination)?;
    let MatchPage { items, next_cursor } = state
        .service
        .list(
            identity.account.user_id,
            limit,
            offset,
            query.cursor.as_deref(),
        )
        .await
        .map_err(match_error)?;
    Ok(Json(ListResponse {
        matches: items,
        next_cursor,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchPath {
    id: String,
}

impl MatchPath {
    fn id(&self) -> Option<Uuid> {
        let parsed = Uuid::parse_str(&self.id).ok()?;
        (self.id.len() == 36
            && [8, 13, 18, 23]
                .iter()
                .all(|index| self.id.as_bytes()[*index] == b'-')
            && parsed
                .hyphenated()
                .to_string()
                .eq_ignore_ascii_case(&self.id)
            && (1..=8).contains(&parsed.get_version_num())
            && parsed.get_variant() == Variant::RFC4122)
            .then_some(parsed)
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
struct RevealResponse {
    message: &'static str,
    photos_revealed: bool,
}

async fn reveal(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<MatchHttpState>,
    ValidatedPath(path): ValidatedPath<MatchPath>,
) -> Result<Json<RevealResponse>, ApiError> {
    let match_id = path.id().ok_or_else(invalid_match_id)?;
    let photos_revealed = state
        .service
        .reveal(match_id, identity.account.user_id)
        .await
        .map_err(match_error)?;
    Ok(Json(RevealResponse {
        message: if photos_revealed {
            "Both participants agreed to reveal their profile photos."
        } else {
            "Photo reveal consent recorded."
        },
        photos_revealed,
    }))
}

#[derive(Serialize)]
struct ContinueResponse {
    message: &'static str,
    match_confirmed: bool,
}

async fn continue_match(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<MatchHttpState>,
    ValidatedPath(path): ValidatedPath<MatchPath>,
) -> Result<Json<ContinueResponse>, ApiError> {
    let match_id = path.id().ok_or_else(invalid_match_id)?;
    let match_confirmed = state
        .service
        .continue_match(match_id, identity.account.user_id)
        .await
        .map_err(match_error)?;
    Ok(Json(ContinueResponse {
        message: if match_confirmed {
            "Both participants agreed to continue the match."
        } else {
            "Match continuation consent recorded."
        },
        match_confirmed,
    }))
}

async fn continuation_quota(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<MatchHttpState>,
) -> Result<Json<ContinuationQuota>, ApiError> {
    state
        .service
        .continuation_quota(identity.account.user_id)
        .await
        .map(Json)
        .map_err(match_error)
}

#[derive(Serialize)]
struct MessageListResponse {
    messages: Vec<PublicMessage>,
    next_cursor: Option<String>,
}

async fn list_messages(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<MatchHttpState>,
    ValidatedPath(path): ValidatedPath<MatchPath>,
    ValidatedQuery(query): ValidatedQuery<PaginationQuery>,
) -> Result<Json<MessageListResponse>, ApiError> {
    let match_id = path.id().ok_or_else(invalid_match_id)?;
    let limit = query.limit().ok_or_else(invalid_pagination)?;
    let offset = query.offset().ok_or_else(invalid_pagination)?;
    let MessagePage { items, next_cursor } = state
        .service
        .messages(
            match_id,
            identity.account.user_id,
            limit,
            offset,
            query.cursor.as_deref(),
        )
        .await
        .map_err(match_error)?;
    Ok(Json(MessageListResponse {
        messages: items,
        next_cursor,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendMessageBody {
    content: String,
}

impl ApiDto for SendMessageBody {
    const ERROR_CODE: &'static str = "invalid_message_payload";
    const ERROR_MESSAGE: &'static str = "The message request body is invalid.";

    fn is_valid(&self) -> bool {
        true
    }
}

async fn send_message(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<MatchHttpState>,
    ValidatedPath(path): ValidatedPath<MatchPath>,
    headers: HeaderMap,
    crate::http::extract::ValidatedJson(body): crate::http::extract::ValidatedJson<SendMessageBody>,
) -> Result<(StatusCode, Json<PublicMessage>), ApiError> {
    let match_id = path.id().ok_or_else(invalid_match_id)?;
    let user_id = identity.account.user_id;
    state
        .limiter
        .enforce(
            "messages",
            &user_id.hyphenated().to_string(),
            &state.message_policy,
            "message_rate_limit_exceeded",
        )
        .await?;
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok());
    let message = state
        .service
        .send_message(match_id, user_id, &body.content, idempotency_key)
        .await
        .map_err(match_error)?;
    Ok((StatusCode::CREATED, Json(message)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadMessagesBody {
    read_through_message_id: String,
}

impl ReadMessagesBody {
    fn message_id(&self) -> Option<Uuid> {
        canonical_uuid(&self.read_through_message_id)
    }
}

impl ApiDto for ReadMessagesBody {
    const ERROR_CODE: &'static str = "invalid_read_payload";
    const ERROR_MESSAGE: &'static str = "The read request body is invalid.";

    fn is_valid(&self) -> bool {
        self.message_id().is_some()
    }
}

#[derive(Serialize)]
struct ReadMessagesResponse {
    updated_count: i32,
    read_through_message_id: Uuid,
}

async fn mark_messages_read_through(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<MatchHttpState>,
    ValidatedPath(path): ValidatedPath<MatchPath>,
    crate::http::extract::ValidatedJson(body): crate::http::extract::ValidatedJson<
        ReadMessagesBody,
    >,
) -> Result<Json<ReadMessagesResponse>, ApiError> {
    let match_id = path.id().ok_or_else(invalid_match_id)?;
    let message_id = body.message_id().ok_or_else(invalid_read_payload)?;
    let updated_count = state
        .service
        .mark_messages_read_through(match_id, message_id, identity.account.user_id)
        .await
        .map_err(match_error)?;
    Ok(Json(ReadMessagesResponse {
        updated_count,
        read_through_message_id: message_id,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchMessagePath {
    id: String,
    #[serde(rename = "msgId")]
    msg_id: String,
}

impl MatchMessagePath {
    fn ids(&self) -> Option<(Uuid, Uuid)> {
        Some((canonical_uuid(&self.id)?, canonical_uuid(&self.msg_id)?))
    }
}

impl ApiDto for MatchMessagePath {
    const ERROR_CODE: &'static str = "invalid_message_id";
    const ERROR_MESSAGE: &'static str = "The message ID must be a valid UUID.";

    fn is_valid(&self) -> bool {
        self.ids().is_some()
    }
}

#[derive(Serialize)]
struct MarkMessageReadResponse {
    message: &'static str,
}

async fn mark_message_read(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<MatchHttpState>,
    ValidatedPath(path): ValidatedPath<MatchMessagePath>,
) -> Result<Json<MarkMessageReadResponse>, ApiError> {
    let (match_id, message_id) = path.ids().ok_or_else(invalid_message_id)?;
    state
        .service
        .mark_message_read(match_id, message_id, identity.account.user_id)
        .await
        .map_err(match_error)?;
    Ok(Json(MarkMessageReadResponse {
        message: "message marked as read",
    }))
}

fn invalid_pagination() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_pagination",
        "Pagination parameters are invalid.",
    )
}

fn invalid_match_id() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_match_id",
        "The match ID must be a valid UUID.",
    )
}

fn invalid_read_payload() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_read_payload",
        "The read request body is invalid.",
    )
}

fn invalid_message_id() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_message_id",
        "The message ID must be a valid UUID.",
    )
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

fn match_error(error: MatchError) -> ApiError {
    match error {
        MatchError::InvalidRequest => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_match_request",
            "The match request is invalid.",
        ),
        MatchError::InvalidMessageRequest => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_message_request",
            "The message request is invalid.",
        ),
        MatchError::InvalidIdempotencyKey => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_idempotency_key",
            "The Idempotency-Key header must contain a UUID v4.",
        ),
        MatchError::InvalidCursor => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_cursor",
            "The pagination cursor is invalid.",
        ),
        MatchError::NotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "match_not_found",
            "The match could not be found.",
        ),
        MatchError::Blocked => ApiError::new(
            StatusCode::CONFLICT,
            "match_blocked",
            "A match cannot be created between blocked users.",
        ),
        MatchError::CandidateNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "discovery_candidate_not_found",
            "The discovery candidate is no longer available.",
        ),
        MatchError::InvalidState => ApiError::new(
            StatusCode::CONFLICT,
            "invalid_match_state",
            "This action is not available in the match's current state.",
        ),
        MatchError::ContinuationNotAvailableYet => ApiError::new(
            StatusCode::CONFLICT,
            "continuation_not_available_yet",
            "Continuation becomes available after the initial 24-hour match period.",
        ),
        MatchError::Expired => {
            ApiError::new(StatusCode::GONE, "match_expired", "This match has expired.")
        }
        MatchError::QuotaReached => ApiError::new(
            StatusCode::FORBIDDEN,
            "continuation_quota_reached",
            "The weekly continuation quota has been reached. Upgrade to Premium for unlimited continuations.",
        ),
        MatchError::MessagingUnavailable => ApiError::new(
            StatusCode::CONFLICT,
            "messaging_not_available",
            "Messaging is not available for this match.",
        ),
        MatchError::MessageNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "message_not_found",
            "The message could not be found.",
        ),
        MatchError::IdempotencyConflict => ApiError::new(
            StatusCode::CONFLICT,
            "idempotency_key_conflict",
            "The idempotency key was already used for another request.",
        ),
        MatchError::PhotoStorageUnavailable => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "photo_storage_unavailable",
            "Photo storage is temporarily unavailable",
        ),
        MatchError::Database(error) => ApiError::from(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_numbers_follow_the_common_class_transformer_cases() {
        assert_eq!(javascript_number(&Some("12".to_owned()), 20), Some(12));
        assert_eq!(javascript_number(&Some("1e2".to_owned()), 20), Some(100));
        assert_eq!(javascript_number(&Some(String::new()), 20), Some(0));
        assert_eq!(javascript_number(&Some("1.5".to_owned()), 20), None);
    }

    #[test]
    fn match_paths_require_canonical_rfc4122_ids() {
        let id = Uuid::new_v4();
        assert_eq!(
            MatchPath {
                id: id.hyphenated().to_string(),
            }
            .id(),
            Some(id)
        );
        assert!(
            MatchPath {
                id: id.simple().to_string()
            }
            .id()
            .is_none()
        );
    }
}
