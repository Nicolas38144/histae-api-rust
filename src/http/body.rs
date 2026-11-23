//! Parse and bound request bodies before invoking handlers, including handlers
//! which intentionally do not extract a body. Preserve raw bytes for signatures.
use super::ApiError;
use axum::{
    RequestExt,
    body::{Body, Bytes},
    extract::{FromRequest, Request, State},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{sync::Arc, time::Duration};
use tokio::{sync::Semaphore, time::timeout};

const BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const MAX_ACTIVE_BODIES: usize = 64;

pub async fn middleware(
    State(slots): State<Arc<Semaphore>>,
    request: Request,
    next: Next,
) -> Response {
    if matches!(*request.method(), Method::GET | Method::HEAD) {
        return next.run(request).await;
    }
    let Ok(_slot) = slots.try_acquire() else {
        return ApiError::busy("request_body_capacity_reached").into_response();
    };
    match validate(request).await {
        Ok(request) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

async fn validate(request: Request) -> Result<Request, ApiError> {
    let (parts, body) = request.with_limited_body().into_parts();
    let content_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !content_type.is_empty()
        && !matches!(
            content_type.as_str(),
            "application/json" | "text/plain" | "multipart/form-data"
        )
    {
        return Err(ApiError::invalid_body_with_status(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ));
    }
    let bytes = timeout(
        BODY_READ_TIMEOUT,
        Bytes::from_request(Request::new(body), &()),
    )
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::REQUEST_TIMEOUT,
            "request_body_timeout",
            "The request body took too long to arrive.",
        )
    })?
    .map_err(|error| ApiError::invalid_body_with_status(error.status()))?;
    if content_type == "application/json" {
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .map_err(|_| ApiError::invalid_body())?;
    } else if (!bytes.is_empty() || !content_type.is_empty())
        && content_type != "text/plain"
        && content_type != "multipart/form-data"
    {
        return Err(ApiError::invalid_body_with_status(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ));
    }
    Ok(Request::from_parts(parts, Body::from(bytes)))
}
