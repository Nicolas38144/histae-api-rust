use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::infra::postgres::DatabaseError;
use crate::infra::postgres_locks::AccountActivityError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    retry_after_seconds: Option<u64>,
}

impl ApiError {
    pub const fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
            retry_after_seconds: None,
        }
    }

    pub const fn with_retry_after(mut self, seconds: u64) -> Self {
        self.retry_after_seconds = Some(seconds);
        self
    }

    pub const fn status(self) -> StatusCode {
        self.status
    }

    pub const fn code(self) -> &'static str {
        self.code
    }

    pub const fn message(self) -> &'static str {
        self.message
    }

    pub const fn invalid_body() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_body",
            "The request body is invalid.",
        )
    }

    pub const fn invalid_body_with_status(status: StatusCode) -> Self {
        Self::new(
            status,
            "invalid_request_body",
            "The request body is invalid.",
        )
    }

    pub const fn route_not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "route_not_found",
            "This route is not available.",
        )
    }

    pub const fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "The request could not be completed.",
        )
    }

    pub const fn dependency_unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "request_failed",
            "A required dependency is unavailable.",
        )
    }
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response();
        if let Some(seconds) = self.retry_after_seconds
            && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }
}

impl From<DatabaseError> for ApiError {
    fn from(error: DatabaseError) -> Self {
        if error == DatabaseError::AccountUnavailable {
            Self::new(
                StatusCode::CONFLICT,
                "account_unavailable",
                "An account is no longer available.",
            )
        } else {
            Self::internal()
        }
    }
}

impl From<AccountActivityError> for ApiError {
    fn from(error: AccountActivityError) -> Self {
        match error {
            AccountActivityError::AccountUnavailable
            | AccountActivityError::Database(DatabaseError::AccountUnavailable) => Self::new(
                StatusCode::CONFLICT,
                "account_unavailable",
                "An account is no longer available.",
            ),
            AccountActivityError::ActivityUnavailable | AccountActivityError::Database(_) => {
                Self::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "account_activity_unavailable",
                    "Account activity is temporarily unavailable.",
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_postgres_account_guard_without_exposing_driver_details() {
        let unavailable = ApiError::from(DatabaseError::AccountUnavailable);
        assert_eq!(unavailable.status(), StatusCode::CONFLICT);
        assert_eq!(unavailable.code(), "account_unavailable");

        let internal = ApiError::from(DatabaseError::QueryFailed);
        assert_eq!(internal.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(internal.code(), "internal_error");
    }
}
