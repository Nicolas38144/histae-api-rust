//! Parse and bound request bodies before invoking handlers, including handlers
//! which intentionally do not extract a body. Preserve raw bytes for signatures.
use super::ApiError;
use axum::{
    RequestExt,
    body::{Body, Bytes},
    extract::{FromRequest, Request},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

pub async fn middleware(request: Request, next: Next) -> Response {
    match validate(request).await {
        Ok(request) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

async fn validate(request: Request) -> Result<Request, ApiError> {
    // Fastify does not parse GET/HEAD bodies.
    if matches!(*request.method(), Method::GET | Method::HEAD) {
        return Ok(request);
    }
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
    let bytes = Bytes::from_request(Request::new(body), &())
        .await
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
