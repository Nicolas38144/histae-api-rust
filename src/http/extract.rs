use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, FromRequestParts, Json, Path, Query, Request};
use axum::http::StatusCode;
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

use super::error::ApiError;

pub trait ApiDto {
    const ERROR_CODE: &'static str = "invalid_request_body";
    const ERROR_MESSAGE: &'static str = "The request body is invalid.";
}

#[derive(Debug)]
pub struct ValidatedJson<T>(pub T);

#[derive(Debug)]
pub struct ValidatedQuery<T>(pub T);

#[derive(Debug)]
pub struct ValidatedPath<T>(pub T);

impl<S, T> FromRequest<S> for ValidatedJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned + ApiDto + Send,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(json_error::<T>)
    }
}

impl<S, T> FromRequestParts<S> for ValidatedQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned + ApiDto + Send,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|_| dto_error::<T>())
    }
}

impl<S, T> FromRequestParts<S> for ValidatedPath<T>
where
    S: Send + Sync,
    T: DeserializeOwned + ApiDto + Send,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(value)| Self(value))
            .map_err(|_| dto_error::<T>())
    }
}

fn json_error<T: ApiDto>(rejection: JsonRejection) -> ApiError {
    match rejection {
        JsonRejection::JsonDataError(_) => dto_error::<T>(),
        other => ApiError::invalid_body_with_status(other.status()),
    }
}

fn dto_error<T: ApiDto>() -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, T::ERROR_CODE, T::ERROR_MESSAGE)
}
