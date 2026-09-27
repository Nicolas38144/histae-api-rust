use axum::extract::Extension;
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant};

use super::devices::{DeviceError, DeviceService};
use super::domain::{DevicePlatform, PublicDevice};
use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedJson, ValidatedPath};
use crate::http::router::HttpState;
use crate::identity::mobile::http::{AuthenticatedMobile, MobileAuthState};
use crate::shared::text::validator_js_length;

#[derive(Clone)]
pub struct NotificationHttpState {
    service: DeviceService,
}

impl NotificationHttpState {
    pub fn new(service: DeviceService) -> Self {
        Self { service }
    }
}

pub fn routes(state: NotificationHttpState, auth: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route(
            "/api/users/me/devices",
            get(list_devices).post(register_device),
        )
        .route("/api/users/me/devices/{id}", delete(remove_device))
        .layer(Extension(state))
        .layer(Extension(auth))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterDeviceBody {
    push_token: String,
    platform: DevicePlatform,
    app_version: Option<String>,
}

impl ApiDto for RegisterDeviceBody {
    const ERROR_CODE: &'static str = "invalid_device_payload";
    const ERROR_MESSAGE: &'static str = "The device registration request body is invalid.";

    fn is_valid(&self) -> bool {
        (20..=4096).contains(&validator_js_length(&self.push_token))
            && self
                .app_version
                .as_deref()
                .is_none_or(|value| validator_js_length(value) <= 50)
    }
}

#[derive(Serialize)]
struct DevicesResponse {
    devices: Vec<PublicDevice>,
}

async fn list_devices(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<NotificationHttpState>,
) -> Result<Json<DevicesResponse>, ApiError> {
    state
        .service
        .list(identity.account.user_id)
        .await
        .map(|devices| Json(DevicesResponse { devices }))
        .map_err(device_error)
}

async fn register_device(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<NotificationHttpState>,
    ValidatedJson(body): ValidatedJson<RegisterDeviceBody>,
) -> Result<(StatusCode, Json<PublicDevice>), ApiError> {
    state
        .service
        .register(
            identity.account.user_id,
            identity.session_id,
            body.push_token,
            body.platform,
            body.app_version,
        )
        .await
        .map(|device| (StatusCode::CREATED, Json(device)))
        .map_err(device_error)
}

#[derive(Deserialize)]
struct DevicePath {
    id: Uuid,
}

impl ApiDto for DevicePath {
    const ERROR_CODE: &'static str = "invalid_device_id";
    const ERROR_MESSAGE: &'static str = "The device ID must be a valid UUID.";

    fn is_valid(&self) -> bool {
        (1..=8).contains(&self.id.get_version_num()) && self.id.get_variant() == Variant::RFC4122
    }
}

async fn remove_device(
    AuthenticatedMobile(identity): AuthenticatedMobile,
    Extension(state): Extension<NotificationHttpState>,
    ValidatedPath(path): ValidatedPath<DevicePath>,
) -> Result<StatusCode, ApiError> {
    state
        .service
        .remove(identity.account.user_id, path.id)
        .await
        .map_err(device_error)?;
    Ok(StatusCode::NO_CONTENT)
}

fn device_error(error: DeviceError) -> ApiError {
    match error {
        DeviceError::AuthenticationRequired => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "A valid mobile session is required.",
        ),
        DeviceError::NotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "device_not_found",
            "The device registration could not be found.",
        ),
        DeviceError::Database(error) => error.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_before_service_trimming_like_class_validator() {
        let valid_with_whitespace = RegisterDeviceBody {
            push_token: "                    ".to_owned(),
            platform: DevicePlatform::Ios,
            app_version: Some(String::new()),
        };
        assert!(valid_with_whitespace.is_valid());
        assert!(
            !RegisterDeviceBody {
                push_token: "short".to_owned(),
                platform: DevicePlatform::Android,
                app_version: None,
            }
            .is_valid()
        );
        assert!(
            !RegisterDeviceBody {
                push_token: "x".repeat(20),
                platform: DevicePlatform::Android,
                app_version: Some("x".repeat(51)),
            }
            .is_valid()
        );
    }
}
