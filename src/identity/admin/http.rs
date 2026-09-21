use std::sync::Arc;

use axum::extract::{Extension, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use uuid::{Uuid, Variant, Version};

use crate::config::{AdminAuthConfig, LimitPolicy};
use crate::http::error::ApiError;
use crate::http::extract::{ApiDto, ValidatedJson, ValidatedPath, ValidatedQuery};
use crate::http::lifecycle::ClientIp;
use crate::http::rate_limit::RateLimiter;
use crate::http::router::HttpState;

use super::service::{
    AdminAuthError, AdminAuthService, AdminSessionView, AuthEventPage, AuthenticatedAdmin,
    CredentialView, SessionSummaryView, WebauthnOptions,
};

#[derive(Clone)]
pub struct AdminAuthHttpState {
    service: AdminAuthService,
    limiter: RateLimiter,
    policy: LimitPolicy,
    config: Arc<AdminAuthConfig>,
}

impl AdminAuthHttpState {
    pub fn new(
        service: AdminAuthService,
        limiter: RateLimiter,
        policy: LimitPolicy,
        config: AdminAuthConfig,
    ) -> Self {
        Self {
            service,
            limiter,
            policy,
            config: Arc::new(config),
        }
    }
}

pub fn routes(state: AdminAuthHttpState) -> Router<HttpState> {
    Router::new()
        .route("/api/admin/auth/login/options", post(login_options))
        .route("/api/admin/auth/login/verify", post(login_verify))
        .route("/api/admin/auth/bootstrap/options", post(bootstrap_options))
        .route("/api/admin/auth/bootstrap/verify", post(bootstrap_verify))
        .route("/api/admin/auth/session", get(session))
        .route("/api/admin/auth/logout", post(logout))
        .route("/api/admin/auth/credentials", get(credentials))
        .route(
            "/api/admin/auth/credentials/options",
            post(credential_options),
        )
        .route(
            "/api/admin/auth/credentials/verify",
            post(credential_verify),
        )
        .route(
            "/api/admin/auth/credentials/{id}",
            patch(rename_credential).delete(revoke_credential),
        )
        .route("/api/admin/auth/sessions", get(sessions))
        .route(
            "/api/admin/auth/sessions/revoke-others",
            post(revoke_other_sessions),
        )
        .route(
            "/api/admin/auth/sessions/{id}",
            delete(revoke_selected_session),
        )
        .route("/api/admin/auth/events", get(events))
        .layer(Extension(state))
}

async fn login_options(
    Extension(state): Extension<AdminAuthHttpState>,
    Extension(ClientIp(client_ip)): Extension<ClientIp>,
) -> Result<Json<WebauthnOptions>, ApiError> {
    public_limit(&state, &client_ip.to_string()).await?;
    state
        .service
        .authentication_options()
        .await
        .map(Json)
        .map_err(admin_auth_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthenticationVerifyBody {
    challenge_id: UuidV4,
    credential: Value,
}

impl ApiDto for AuthenticationVerifyBody {
    const ERROR_CODE: &'static str = "invalid_admin_auth_request";
    const ERROR_MESSAGE: &'static str = "The administrator authentication request is invalid.";

    fn is_valid(&self) -> bool {
        self.credential.is_object()
    }
}

async fn login_verify(
    Extension(state): Extension<AdminAuthHttpState>,
    Extension(ClientIp(client_ip)): Extension<ClientIp>,
    ValidatedJson(body): ValidatedJson<AuthenticationVerifyBody>,
) -> Result<Response, ApiError> {
    public_limit(&state, &client_ip.to_string()).await?;
    let created = state
        .service
        .authenticate(body.challenge_id.0, body.credential)
        .await
        .map_err(admin_auth_error)?;
    session_response(
        StatusCode::OK,
        created.session,
        &created.token,
        &state.config,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapOptionsBody {
    bootstrap_token: String,
}

impl ApiDto for BootstrapOptionsBody {
    const ERROR_CODE: &'static str = "invalid_admin_auth_request";
    const ERROR_MESSAGE: &'static str = "The administrator authentication request is invalid.";

    fn is_valid(&self) -> bool {
        valid_bootstrap_token(&self.bootstrap_token)
    }
}

async fn bootstrap_options(
    Extension(state): Extension<AdminAuthHttpState>,
    Extension(ClientIp(client_ip)): Extension<ClientIp>,
    ValidatedJson(body): ValidatedJson<BootstrapOptionsBody>,
) -> Result<Json<WebauthnOptions>, ApiError> {
    public_limit(&state, &client_ip.to_string()).await?;
    state
        .service
        .bootstrap_registration_options(&body.bootstrap_token)
        .await
        .map(Json)
        .map_err(admin_auth_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapVerifyBody {
    bootstrap_token: String,
    challenge_id: UuidV4,
    credential: Value,
    name: String,
}

impl ApiDto for BootstrapVerifyBody {
    const ERROR_CODE: &'static str = "invalid_admin_auth_request";
    const ERROR_MESSAGE: &'static str = "The administrator authentication request is invalid.";

    fn is_valid(&self) -> bool {
        valid_bootstrap_token(&self.bootstrap_token)
            && self.credential.is_object()
            && valid_dto_name(&self.name)
    }
}

async fn bootstrap_verify(
    Extension(state): Extension<AdminAuthHttpState>,
    Extension(ClientIp(client_ip)): Extension<ClientIp>,
    ValidatedJson(body): ValidatedJson<BootstrapVerifyBody>,
) -> Result<Response, ApiError> {
    public_limit(&state, &client_ip.to_string()).await?;
    let created = state
        .service
        .complete_bootstrap_registration(
            &body.bootstrap_token,
            body.challenge_id.0,
            body.credential,
            &body.name,
        )
        .await
        .map_err(admin_auth_error)?;
    session_response(
        StatusCode::CREATED,
        created.session,
        &created.token,
        &state.config,
    )
}

async fn session(AdminIdentity(identity): AdminIdentity) -> Json<AdminSessionView> {
    Json(identity_view(&identity))
}

async fn logout(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<AdminAuthHttpState>,
) -> Result<Response, ApiError> {
    state
        .service
        .revoke_session(identity.user_id, identity.session_id)
        .await
        .map_err(admin_auth_error)?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    insert_cookie(
        response.headers_mut(),
        expired_session_cookie(&state.config),
    )?;
    Ok(response)
}

async fn credentials(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<AdminAuthHttpState>,
) -> Result<Json<Vec<CredentialView>>, ApiError> {
    state
        .service
        .credentials(identity.user_id, identity.credential_id)
        .await
        .map(Json)
        .map_err(admin_auth_error)
}

async fn credential_options(
    RecentAdminIdentity(identity): RecentAdminIdentity,
    Extension(state): Extension<AdminAuthHttpState>,
) -> Result<(StatusCode, Json<WebauthnOptions>), ApiError> {
    let options = state
        .service
        .additional_registration_options(identity.user_id)
        .await
        .map_err(admin_auth_error)?;
    Ok((StatusCode::CREATED, Json(options)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdditionalCredentialBody {
    challenge_id: UuidV4,
    credential: Value,
    name: String,
}

impl ApiDto for AdditionalCredentialBody {
    const ERROR_CODE: &'static str = "invalid_admin_auth_request";
    const ERROR_MESSAGE: &'static str = "The administrator authentication request is invalid.";

    fn is_valid(&self) -> bool {
        self.credential.is_object() && valid_dto_name(&self.name)
    }
}

#[derive(Serialize)]
struct MessageResponse {
    message: &'static str,
}

async fn credential_verify(
    RecentAdminIdentity(identity): RecentAdminIdentity,
    Extension(state): Extension<AdminAuthHttpState>,
    ValidatedJson(body): ValidatedJson<AdditionalCredentialBody>,
) -> Result<(StatusCode, Json<MessageResponse>), ApiError> {
    state
        .service
        .add_credential(
            identity.user_id,
            body.challenge_id.0,
            body.credential,
            &body.name,
        )
        .await
        .map_err(admin_auth_error)?;
    Ok((
        StatusCode::CREATED,
        Json(MessageResponse {
            message: "administrator credential registered",
        }),
    ))
}

#[derive(Deserialize)]
struct CredentialPath {
    id: UuidV4,
}

impl ApiDto for CredentialPath {
    const ERROR_CODE: &'static str = "invalid_admin_credential_id";
    const ERROR_MESSAGE: &'static str = "The administrator credential ID is invalid.";

    fn is_valid(&self) -> bool {
        true
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameCredentialBody {
    name: String,
}

impl ApiDto for RenameCredentialBody {
    const ERROR_CODE: &'static str = "invalid_admin_auth_request";
    const ERROR_MESSAGE: &'static str = "The administrator authentication request is invalid.";

    fn is_valid(&self) -> bool {
        valid_dto_name(&self.name)
    }
}

async fn rename_credential(
    RecentAdminIdentity(identity): RecentAdminIdentity,
    Extension(state): Extension<AdminAuthHttpState>,
    ValidatedPath(path): ValidatedPath<CredentialPath>,
    ValidatedJson(body): ValidatedJson<RenameCredentialBody>,
) -> Result<Json<MessageResponse>, ApiError> {
    state
        .service
        .rename_credential(identity.user_id, path.id.0, identity.session_id, &body.name)
        .await
        .map_err(admin_auth_error)?;
    Ok(Json(MessageResponse {
        message: "administrator credential renamed",
    }))
}

async fn revoke_credential(
    RecentAdminIdentity(identity): RecentAdminIdentity,
    Extension(state): Extension<AdminAuthHttpState>,
    ValidatedPath(path): ValidatedPath<CredentialPath>,
) -> Result<StatusCode, ApiError> {
    state
        .service
        .revoke_credential(
            identity.user_id,
            path.id.0,
            identity.session_id,
            identity.credential_id,
        )
        .await
        .map_err(admin_auth_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn sessions(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<AdminAuthHttpState>,
) -> Result<Json<Vec<SessionSummaryView>>, ApiError> {
    state
        .service
        .sessions(identity.user_id, identity.session_id)
        .await
        .map(Json)
        .map_err(admin_auth_error)
}

#[derive(Deserialize)]
struct SessionPath {
    id: UuidV4,
}

impl ApiDto for SessionPath {
    const ERROR_CODE: &'static str = "invalid_admin_session_id";
    const ERROR_MESSAGE: &'static str = "The administrator session ID is invalid.";

    fn is_valid(&self) -> bool {
        true
    }
}

async fn revoke_selected_session(
    RecentAdminIdentity(identity): RecentAdminIdentity,
    Extension(state): Extension<AdminAuthHttpState>,
    ValidatedPath(path): ValidatedPath<SessionPath>,
) -> Result<StatusCode, ApiError> {
    state
        .service
        .revoke_selected_session(identity.user_id, path.id.0, identity.session_id)
        .await
        .map_err(admin_auth_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct RevokeOthersResponse {
    revoked_sessions: u64,
}

async fn revoke_other_sessions(
    RecentAdminIdentity(identity): RecentAdminIdentity,
    Extension(state): Extension<AdminAuthHttpState>,
) -> Result<(StatusCode, Json<RevokeOthersResponse>), ApiError> {
    let revoked_sessions = state
        .service
        .revoke_other_sessions(identity.user_id, identity.session_id)
        .await
        .map_err(admin_auth_error)?;
    Ok((
        StatusCode::CREATED,
        Json(RevokeOthersResponse { revoked_sessions }),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventsQuery {
    #[serde(default = "default_limit")]
    limit: u32,
    cursor: Option<String>,
}

impl ApiDto for EventsQuery {
    const ERROR_CODE: &'static str = "invalid_admin_auth_request";
    const ERROR_MESSAGE: &'static str = "The administrator authentication request is invalid.";

    fn is_valid(&self) -> bool {
        (1..=100).contains(&self.limit)
            && self
                .cursor
                .as_ref()
                .is_none_or(|value| value.encode_utf16().count() <= 512)
    }
}

const fn default_limit() -> u32 {
    20
}

async fn events(
    AdminIdentity(identity): AdminIdentity,
    Extension(state): Extension<AdminAuthHttpState>,
    ValidatedQuery(query): ValidatedQuery<EventsQuery>,
) -> Result<Json<AuthEventPage>, ApiError> {
    state
        .service
        .auth_events(identity.user_id, query.limit, query.cursor.as_deref())
        .await
        .map(Json)
        .map_err(admin_auth_error)
}

#[derive(Clone, Debug)]
pub struct AdminIdentity(pub AuthenticatedAdmin);

#[derive(Clone, Debug)]
pub struct RecentAdminIdentity(pub AuthenticatedAdmin);

#[derive(Debug)]
pub struct AdminGuardRejection {
    error: ApiError,
    expired_cookie: Option<String>,
}

impl IntoResponse for AdminGuardRejection {
    fn into_response(self) -> Response {
        let mut response = self.error.into_response();
        if let Some(cookie) = self.expired_cookie {
            let _ = insert_cookie(response.headers_mut(), cookie);
        }
        response
    }
}

impl<S> FromRequestParts<S> for AdminIdentity
where
    S: Send + Sync,
{
    type Rejection = AdminGuardRejection;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Extension(auth) = Extension::<AdminAuthHttpState>::from_request_parts(parts, state)
            .await
            .map_err(|_| guard_error(ApiError::internal(), None))?;
        verify_mutation_origin(parts, &auth.config)?;
        let token = read_session_cookie(&parts.headers, auth.config.cookie_name);
        let session = match token {
            Some(token) => auth
                .service
                .authenticate_session(&token)
                .await
                .map_err(|error| guard_error(admin_auth_error(error), None))?,
            None => None,
        };
        session.map(Self).ok_or_else(|| {
            guard_error(
                ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "admin_session_invalid",
                    "The administrator session is invalid or expired.",
                ),
                Some(expired_session_cookie(&auth.config)),
            )
        })
    }
}

impl<S> FromRequestParts<S> for RecentAdminIdentity
where
    S: Send + Sync,
{
    type Rejection = AdminGuardRejection;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let AdminIdentity(identity) = AdminIdentity::from_request_parts(parts, state).await?;
        let Extension(auth) = Extension::<AdminAuthHttpState>::from_request_parts(parts, state)
            .await
            .map_err(|_| guard_error(ApiError::internal(), None))?;
        let age = Utc::now().signed_duration_since(identity.authenticated_at);
        let maximum = chrono::Duration::from_std(auth.config.recent_authentication_ttl)
            .map_err(|_| guard_error(ApiError::internal(), None))?;
        if age > maximum {
            return Err(guard_error(
                ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "admin_reauthentication_required",
                    "A recent WebAuthn authentication is required.",
                ),
                None,
            ));
        }
        Ok(Self(identity))
    }
}

fn verify_mutation_origin(
    parts: &Parts,
    config: &AdminAuthConfig,
) -> Result<(), AdminGuardRejection> {
    if matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return Ok(());
    }
    let valid = parts
        .headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        == Some(config.origin.as_str());
    if valid {
        Ok(())
    } else {
        Err(guard_error(
            ApiError::new(
                StatusCode::FORBIDDEN,
                "invalid_admin_request_origin",
                "The administrator request origin is not allowed.",
            ),
            None,
        ))
    }
}

fn read_session_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let header = headers.get(header::COOKIE)?.to_str().ok()?;
    let prefix = format!("{name}=");
    let mut matches = header
        .split(';')
        .map(str::trim)
        .filter(|part| part.starts_with(&prefix));
    let value = matches.next()?.strip_prefix(&prefix)?;
    if matches.next().is_some()
        || value.len() != 43
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return None;
    }
    Some(value.to_owned())
}

pub fn session_cookie(token: &str, config: &AdminAuthConfig) -> String {
    let max_age = config.session_absolute_ttl.as_secs();
    format!(
        "{}={token}; Path=/; Max-Age={max_age}; HttpOnly; SameSite=Strict{}",
        config.cookie_name,
        if config.secure_cookie { "; Secure" } else { "" }
    )
}

pub fn expired_session_cookie(config: &AdminAuthConfig) -> String {
    format!(
        "{}=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict{}",
        config.cookie_name,
        if config.secure_cookie { "; Secure" } else { "" }
    )
}

fn session_response(
    status: StatusCode,
    session: AdminSessionView,
    token: &str,
    config: &AdminAuthConfig,
) -> Result<Response, ApiError> {
    let mut response = (status, Json(session)).into_response();
    insert_cookie(response.headers_mut(), session_cookie(token, config))?;
    Ok(response)
}

fn insert_cookie(headers: &mut HeaderMap, value: String) -> Result<(), ApiError> {
    let value = HeaderValue::from_str(&value).map_err(|_| ApiError::internal())?;
    headers.insert(header::SET_COOKIE, value);
    Ok(())
}

async fn public_limit(state: &AdminAuthHttpState, client_ip: &str) -> Result<(), ApiError> {
    state
        .limiter
        .enforce(
            "admin-auth",
            client_ip,
            &state.policy,
            "admin_auth_rate_limit_exceeded",
        )
        .await
}

fn valid_uuid_v4(value: Uuid) -> bool {
    value.get_version() == Some(Version::Random) && value.get_variant() == Variant::RFC4122
}

#[derive(Clone, Copy, Debug)]
struct UuidV4(Uuid);

impl<'de> Deserialize<'de> for UuidV4 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if !canonical_uuid_text(&value) {
            return Err(serde::de::Error::custom("invalid UUID v4"));
        }
        let parsed =
            Uuid::parse_str(&value).map_err(|_| serde::de::Error::custom("invalid UUID v4"))?;
        if !valid_uuid_v4(parsed) {
            return Err(serde::de::Error::custom("invalid UUID v4"));
        }
        Ok(Self(parsed))
    }
}

fn canonical_uuid_text(value: &str) -> bool {
    value.len() == 36
        && value.as_bytes().get(8) == Some(&b'-')
        && value.as_bytes().get(13) == Some(&b'-')
        && value.as_bytes().get(18) == Some(&b'-')
        && value.as_bytes().get(23) == Some(&b'-')
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 8 | 13 | 18 | 23) || byte.is_ascii_hexdigit())
}

fn valid_bootstrap_token(value: &str) -> bool {
    let mut parts = value.split(':');
    let Some(id) = parts.next() else { return false };
    let Some(secret) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && canonical_uuid_text(id)
        && Uuid::parse_str(id).is_ok_and(valid_uuid_v4)
        && secret.len() == 43
        && secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn valid_dto_name(value: &str) -> bool {
    (1..=100).contains(&value.encode_utf16().count())
}

fn identity_view(identity: &AuthenticatedAdmin) -> AdminSessionView {
    AdminSessionView {
        user_id: identity.user_id,
        role: identity.role,
        authenticated_at: super::domain::wire_timestamp(identity.authenticated_at),
        expires_at: super::domain::wire_timestamp(identity.expires_at),
    }
}

fn guard_error(error: ApiError, expired_cookie: Option<String>) -> AdminGuardRejection {
    AdminGuardRejection {
        error,
        expired_cookie,
    }
}

fn admin_auth_error(error: AdminAuthError) -> ApiError {
    match error {
        AdminAuthError::InvalidBootstrap => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_or_expired_admin_bootstrap",
            "The administrator enrollment token is invalid or expired.",
        ),
        AdminAuthError::InvalidChallenge => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_or_expired_webauthn_challenge",
            "The WebAuthn challenge is invalid or expired.",
        ),
        AdminAuthError::InvalidRegistration => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "webauthn_registration_failed",
            "The WebAuthn registration could not be verified.",
        ),
        AdminAuthError::InvalidAuthentication => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "webauthn_authentication_failed",
            "The WebAuthn authentication could not be verified.",
        ),
        AdminAuthError::InvalidWebauthnPayload => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_webauthn_payload",
            "The WebAuthn response is invalid.",
        ),
        AdminAuthError::CredentialConflict => ApiError::new(
            StatusCode::CONFLICT,
            "webauthn_credential_already_registered",
            "This WebAuthn credential is already registered.",
        ),
        AdminAuthError::SessionInvalid => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "admin_session_invalid",
            "The administrator session is invalid or expired.",
        ),
        AdminAuthError::CredentialNameInvalid => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_admin_credential_name",
            "The administrator credential name is invalid.",
        ),
        AdminAuthError::CredentialNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "admin_credential_not_found",
            "The administrator credential was not found.",
        ),
        AdminAuthError::SessionNotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "admin_session_not_found",
            "The administrator session was not found.",
        ),
        AdminAuthError::CurrentCredential => ApiError::new(
            StatusCode::CONFLICT,
            "current_admin_credential",
            "Sign in with another credential before revoking this one.",
        ),
        AdminAuthError::LastCredential => ApiError::new(
            StatusCode::CONFLICT,
            "last_admin_credential",
            "The last active administrator credential cannot be revoked.",
        ),
        AdminAuthError::CurrentSession => ApiError::new(
            StatusCode::CONFLICT,
            "current_admin_session",
            "The current administrator session cannot be revoked here.",
        ),
        AdminAuthError::InvalidCursor => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_cursor",
            "The pagination cursor is invalid.",
        ),
        AdminAuthError::Internal => ApiError::internal(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::body::to_bytes;
    use axum::http::Request;
    use base64::Engine as _;

    use super::*;

    fn config(secure: bool) -> AdminAuthConfig {
        AdminAuthConfig {
            rp_id: "localhost".to_owned(),
            origin: "http://localhost:5173".to_owned(),
            rp_name: "Histae Administration".to_owned(),
            challenge_ttl: Duration::from_secs(300),
            bootstrap_ttl: Duration::from_secs(900),
            session_idle_ttl: Duration::from_secs(1_800),
            session_absolute_ttl: Duration::from_secs(28_800),
            recent_authentication_ttl: Duration::from_secs(600),
            cookie_name: if secure {
                "__Host-histae_admin_session"
            } else {
                "histae_admin_session"
            },
            secure_cookie: secure,
        }
    }

    #[test]
    fn cookie_is_host_only_http_only_and_strict() {
        let token = "a".repeat(43);
        assert_eq!(
            session_cookie(&token, &config(false)),
            format!(
                "histae_admin_session={token}; Path=/; Max-Age=28800; HttpOnly; SameSite=Strict"
            )
        );
        let production = session_cookie(&token, &config(true));
        assert!(production.starts_with("__Host-histae_admin_session="));
        assert!(production.ends_with("; HttpOnly; SameSite=Strict; Secure"));
        assert!(!production.contains("Domain="));
    }

    #[test]
    fn cookie_reader_rejects_duplicates_and_malformed_values() {
        let token = "a".repeat(43);
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("other=x; histae_admin_session={token}"))
                .expect("cookie"),
        );
        assert_eq!(
            read_session_cookie(&headers, "histae_admin_session"),
            Some(token.clone())
        );
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!(
                "histae_admin_session={token}; histae_admin_session={}",
                "b".repeat(43)
            ))
            .expect("cookie"),
        );
        assert_eq!(read_session_cookie(&headers, "histae_admin_session"), None);
    }

    #[test]
    fn validates_dynamic_v4_bootstrap_tokens() {
        let token = format!(
            "{}:{}",
            Uuid::new_v4(),
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([9_u8; 32])
        );
        assert!(valid_bootstrap_token(&token));
        assert!(!valid_bootstrap_token("invalid"));
    }

    #[test]
    fn mutations_require_the_exact_admin_origin_before_authentication() {
        let valid = Request::builder()
            .method(Method::POST)
            .uri("/api/admin/auth/logout")
            .header(header::ORIGIN, "http://localhost:5173")
            .body(())
            .expect("request")
            .into_parts()
            .0;
        assert!(verify_mutation_origin(&valid, &config(false)).is_ok());

        let invalid = Request::builder()
            .method(Method::POST)
            .uri("/api/admin/auth/logout")
            .header(header::ORIGIN, "http://127.0.0.1:5173")
            .body(())
            .expect("request")
            .into_parts()
            .0;
        let response = verify_mutation_origin(&invalid, &config(false))
            .expect_err("origin must be rejected")
            .into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let get_without_origin = Request::builder()
            .method(Method::GET)
            .uri("/api/admin/auth/session")
            .body(())
            .expect("request")
            .into_parts()
            .0;
        assert!(verify_mutation_origin(&get_without_origin, &config(false)).is_ok());
    }

    #[test]
    fn authentication_dto_rejects_unknown_fields_and_non_v4_ids() {
        let challenge_id = Uuid::new_v4().to_string();
        let valid = serde_json::json!({
            "challenge_id": challenge_id,
            "credential": {}
        });
        assert!(serde_json::from_value::<AuthenticationVerifyBody>(valid).is_ok());

        let unknown = serde_json::json!({
            "challenge_id": Uuid::new_v4(),
            "credential": {},
            "role": "superadmin"
        });
        assert!(serde_json::from_value::<AuthenticationVerifyBody>(unknown).is_err());

        let mut non_v4 = Uuid::new_v4().to_string();
        non_v4.replace_range(14..15, "1");
        let wrong_version = serde_json::json!({
            "challenge_id": non_v4,
            "credential": {}
        });
        assert!(serde_json::from_value::<AuthenticationVerifyBody>(wrong_version).is_err());
    }

    #[tokio::test]
    async fn public_errors_preserve_auth_not_found_and_conflict_contracts() {
        let cases = [
            (
                AdminAuthError::SessionInvalid,
                StatusCode::UNAUTHORIZED,
                "admin_session_invalid",
            ),
            (
                AdminAuthError::CredentialNotFound,
                StatusCode::NOT_FOUND,
                "admin_credential_not_found",
            ),
            (
                AdminAuthError::SessionNotFound,
                StatusCode::NOT_FOUND,
                "admin_session_not_found",
            ),
            (
                AdminAuthError::CurrentCredential,
                StatusCode::CONFLICT,
                "current_admin_credential",
            ),
            (
                AdminAuthError::LastCredential,
                StatusCode::CONFLICT,
                "last_admin_credential",
            ),
            (
                AdminAuthError::CurrentSession,
                StatusCode::CONFLICT,
                "current_admin_session",
            ),
        ];

        for (error, status, code) in cases {
            let response = admin_auth_error(error).into_response();
            assert_eq!(response.status(), status);
            let body = to_bytes(response.into_body(), 1_024)
                .await
                .expect("bounded error body");
            let json: Value = serde_json::from_slice(&body).expect("JSON error body");
            assert_eq!(json["error"]["code"], code);
        }
    }
}
