use axum::Router;
use axum::body::Body;
use axum::extract::Extension;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::get;

use super::export::{DataExportError, DataExportService, EXPORT_FILENAME};
use crate::config::LimitPolicy;
use crate::http::error::ApiError;
use crate::http::rate_limit::RateLimiter;
use crate::http::router::HttpState;
use crate::identity::mobile::http::{AuthenticatedMobile, MobileAuthState};

#[derive(Clone)]
pub struct DataExportHttpState {
    service: DataExportService,
    limiter: RateLimiter,
    policy: LimitPolicy,
}

impl DataExportHttpState {
    pub fn new(service: DataExportService, limiter: RateLimiter, policy: LimitPolicy) -> Self {
        Self {
            service,
            limiter,
            policy,
        }
    }
}

pub fn routes(state: DataExportHttpState, auth: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route("/api/users/me/data-export", get(export))
        .layer(Extension(state))
        .layer(Extension(auth))
}

async fn export(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<DataExportHttpState>,
) -> Result<Response, ApiError> {
    state
        .limiter
        .enforce(
            "data-export",
            &identity.account.user_id.to_string(),
            &state.policy,
            "data_export_rate_limit_exceeded",
        )
        .await?;
    let prepared = state
        .service
        .prepare(identity.account.user_id)
        .await
        .map_err(data_export_error)?;
    let length =
        HeaderValue::from_str(&prepared.bytes().to_string()).map_err(|_| ApiError::internal())?;
    let mut response = Response::new(Body::from_stream(prepared));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{EXPORT_FILENAME}\""))
        .map_err(|_| ApiError::internal())?;
    response
        .headers_mut()
        .insert(header::CONTENT_DISPOSITION, disposition);
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, length);
    Ok(response)
}

fn data_export_error(error: DataExportError) -> ApiError {
    match error {
        DataExportError::Busy => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "data_export_busy",
            "The data export service is busy. Try again later.",
        )
        .with_retry_after(30),
        DataExportError::TooLarge => ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "data_export_too_large",
            "The data export exceeds the online download limit. Contact support to exercise this right.",
        ),
        DataExportError::Unavailable => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "data_export_unavailable",
            "The complete data export is temporarily unavailable.",
        ),
    }
}
