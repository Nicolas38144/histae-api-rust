use axum::extract::{Extension, FromRequest, Multipart, Request};
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::put;
use axum::{Json, Router};
use serde::Serialize;
use uuid::{Uuid, Variant, Version};

use super::domain::{UploadPhoto, UploadResult};
use super::service::{PhotoError, PhotoService};
use crate::config::LimitPolicy;
use crate::http::error::ApiError;
use crate::http::rate_limit::RateLimiter;
use crate::http::router::HttpState;
use crate::identity::mobile::http::{MobileAuthState, OnboardedMobile};
use crate::photo_codec_probe::MAX_PHOTO_UPLOAD_BYTES;

#[derive(Clone)]
pub struct PhotoHttpState {
    service: PhotoService,
    limiter: RateLimiter,
    policy: LimitPolicy,
}

impl PhotoHttpState {
    pub fn new(service: PhotoService, limiter: RateLimiter, policy: LimitPolicy) -> Self {
        Self {
            service,
            limiter,
            policy,
        }
    }
}

pub fn routes(state: PhotoHttpState, auth: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route("/api/users/me/photo", put(upload).delete(remove))
        .layer(Extension(state))
        .layer(Extension(auth))
}

#[derive(Serialize)]
struct UploadResponse {
    message: &'static str,
    #[serde(flatten)]
    result: UploadResult,
}

async fn upload(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<PhotoHttpState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Json<UploadResponse>, ApiError> {
    let idempotency_key = normalize_idempotency_key(&headers)?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    if !content_type.is_some_and(|value| {
        value
            .to_ascii_lowercase()
            .starts_with("multipart/form-data;")
    }) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_photo",
            "A multipart photo file is required.",
        ));
    }
    let user_id = identity.account.user_id;
    state
        .limiter
        .enforce(
            "photo",
            &user_id.to_string(),
            &state.policy,
            "photo_rate_limit_exceeded",
        )
        .await?;

    let mut multipart = Multipart::from_request(request, &())
        .await
        .map_err(|error| {
            if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                photo_too_large_input()
            } else {
                single_photo_error()
            }
        })?;
    let field = multipart
        .next_field()
        .await
        .map_err(multipart_error)?
        .ok_or_else(single_photo_error)?;
    let field_name = field.name().map(str::to_owned);
    let filename = field.file_name().map(str::to_owned);
    let mime_type = field
        .content_type()
        .map(str::to_owned)
        .unwrap_or_else(|| "application/octet-stream".to_owned());
    if field_name.as_deref() != Some("photo") || filename.as_deref().is_none_or(str::is_empty) {
        return Err(single_photo_error());
    }
    let body = field.bytes().await.map_err(multipart_error)?;
    if body.len() > MAX_PHOTO_UPLOAD_BYTES {
        return Err(photo_too_large_input());
    }
    if multipart
        .next_field()
        .await
        .map_err(multipart_error)?
        .is_some()
    {
        return Err(single_photo_error());
    }
    let result = state
        .service
        .upload(
            user_id,
            UploadPhoto {
                filename: filename.unwrap_or_default(),
                mime_type,
                body: body.to_vec(),
            },
            idempotency_key,
        )
        .await
        .map_err(photo_error)?;
    Ok(Json(UploadResponse {
        message: "photo updated",
        result,
    }))
}

async fn remove(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<PhotoHttpState>,
) -> Result<StatusCode, ApiError> {
    state
        .service
        .delete(identity.account.user_id)
        .await
        .map_err(photo_error)?;
    Ok(StatusCode::NO_CONTENT)
}

fn normalize_idempotency_key(headers: &HeaderMap) -> Result<Uuid, ApiError> {
    let values = headers.get_all("idempotency-key");
    let mut values = values.iter();
    let value = values
        .next()
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if values.next().is_some() {
        return Err(invalid_idempotency_key());
    }
    let normalized = value.trim().to_lowercase();
    let uuid = Uuid::parse_str(&normalized).map_err(|_| invalid_idempotency_key())?;
    if uuid.get_version() != Some(Version::Random)
        || uuid.get_variant() != Variant::RFC4122
        || uuid.to_string() != normalized
    {
        return Err(invalid_idempotency_key());
    }
    Ok(uuid)
}

fn invalid_idempotency_key() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_idempotency_key",
        "The Idempotency-Key header must contain a UUID v4.",
    )
}

fn single_photo_error() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_photo",
        "A single photo file in the photo field is required.",
    )
}

fn multipart_error(error: axum::extract::multipart::MultipartError) -> ApiError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        photo_too_large_input()
    } else {
        single_photo_error()
    }
}

fn photo_too_large_input() -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "photo_too_large",
        "The photo exceeds the allowed size.",
    )
}

fn photo_error(error: PhotoError) -> ApiError {
    match error {
        PhotoError::ProfileNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "profile_not_found",
            "Profile not found",
        ),
        PhotoError::UpdateInProgress => ApiError::new(
            StatusCode::CONFLICT,
            "photo_update_in_progress",
            "A profile photo update is already in progress",
        ),
        PhotoError::IdempotencyConflict => ApiError::new(
            StatusCode::CONFLICT,
            "idempotency_key_conflict",
            "The Idempotency-Key has already been used for another photo",
        ),
        PhotoError::IdempotencyConsumed => ApiError::new(
            StatusCode::CONFLICT,
            "idempotency_key_consumed",
            "The result associated with this Idempotency-Key is no longer current",
        ),
        PhotoError::InvalidPhoto(reason) => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_photo",
            reason.public_message(),
        ),
        PhotoError::PhotoTooLarge => ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "photo_too_large",
            "The processed photo exceeds 500 kB",
        ),
        PhotoError::UpdateConflict => ApiError::new(
            StatusCode::CONFLICT,
            "photo_update_conflict",
            "The profile photo update could not be completed",
        ),
        PhotoError::StorageUnavailable => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "photo_storage_unavailable",
            "Photo storage is temporarily unavailable",
        ),
        PhotoError::AccountActivity(error) => ApiError::from(error),
        PhotoError::Database(_) => ApiError::internal(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_accepts_canonical_uuid_v4_headers() {
        let id = Uuid::new_v4();
        let mut headers = HeaderMap::new();
        headers.insert(
            "idempotency-key",
            format!("  {}  ", id.to_string().to_uppercase())
                .parse()
                .unwrap_or_else(|_| unreachable!()),
        );
        assert_eq!(normalize_idempotency_key(&headers), Ok(id));
        headers.insert(
            "idempotency-key",
            Uuid::new_v4()
                .simple()
                .to_string()
                .parse()
                .unwrap_or_else(|_| unreachable!()),
        );
        assert_eq!(
            normalize_idempotency_key(&headers).map_err(ApiError::code),
            Err("invalid_idempotency_key")
        );
    }
}
