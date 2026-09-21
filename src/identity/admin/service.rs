use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use getrandom::fill as random_fill;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization as _;
use uuid::{Uuid, Variant, Version};

use crate::config::AdminAuthConfig;
use crate::infra::postgres::{ConstraintKind, DatabaseError};
use crate::webauthn_probe::{
    CeremonyState, DeviceType, ExistingCredential, ProbeError, StoredCredential, WebauthnProbe,
    validate_authentication_payload,
};

use super::domain::{
    ActiveSessionRow, AdminRole, AuthEventRow, ChallengePurpose, CredentialRevocation,
    CredentialRow, EventCursor, NewCredential, NewSession, decode_cursor, encode_cursor,
    wire_timestamp,
};
use super::pg::{AdminAuthStore, NewChallenge};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminAuthError {
    InvalidBootstrap,
    InvalidChallenge,
    InvalidRegistration,
    InvalidAuthentication,
    InvalidWebauthnPayload,
    CredentialConflict,
    SessionInvalid,
    CredentialNameInvalid,
    CredentialNotFound,
    SessionNotFound,
    CurrentCredential,
    LastCredential,
    CurrentSession,
    InvalidCursor,
    Internal,
}

impl fmt::Display for AdminAuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidBootstrap => "invalid_or_expired_admin_bootstrap",
            Self::InvalidChallenge => "invalid_or_expired_webauthn_challenge",
            Self::InvalidRegistration => "webauthn_registration_failed",
            Self::InvalidAuthentication => "webauthn_authentication_failed",
            Self::InvalidWebauthnPayload => "invalid_webauthn_payload",
            Self::CredentialConflict => "webauthn_credential_already_registered",
            Self::SessionInvalid => "admin_session_invalid",
            Self::CredentialNameInvalid => "invalid_admin_credential_name",
            Self::CredentialNotFound => "admin_credential_not_found",
            Self::SessionNotFound => "admin_session_not_found",
            Self::CurrentCredential => "current_admin_credential",
            Self::LastCredential => "last_admin_credential",
            Self::CurrentSession => "current_admin_session",
            Self::InvalidCursor => "invalid_cursor",
            Self::Internal => "internal_error",
        })
    }
}

impl std::error::Error for AdminAuthError {}

#[derive(Clone, Debug, Serialize)]
pub struct AdminSessionView {
    pub user_id: Uuid,
    pub role: AdminRole,
    pub authenticated_at: String,
    pub expires_at: String,
}

#[derive(Clone, Debug)]
pub struct AuthenticatedAdmin {
    pub session_id: Uuid,
    pub credential_id: Uuid,
    pub user_id: Uuid,
    pub role: AdminRole,
    pub authenticated_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct SessionCreation {
    pub token: String,
    pub session: AdminSessionView,
}

#[derive(Clone, Debug, Serialize)]
pub struct WebauthnOptions {
    pub challenge_id: Uuid,
    pub options: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct CredentialView {
    pub id: Uuid,
    pub name: String,
    pub device_type: String,
    pub backed_up: bool,
    pub transports: Vec<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub current: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionSummaryView {
    pub id: Uuid,
    pub credential_id: Uuid,
    pub credential_name: String,
    pub authenticated_at: String,
    pub last_seen_at: String,
    pub expires_at: String,
    pub current: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct AuthEventView {
    pub id: Uuid,
    pub event_type: String,
    pub credential_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AuthEventPage {
    pub events: Vec<AuthEventView>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug)]
pub struct IssuedBootstrap {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct AdminAuthService {
    store: Arc<dyn AdminAuthStore>,
    webauthn: Arc<WebauthnProbe>,
    config: AdminAuthConfig,
}

impl AdminAuthService {
    pub fn new(
        store: Arc<dyn AdminAuthStore>,
        config: AdminAuthConfig,
    ) -> Result<Self, AdminAuthError> {
        let timeout = u64::try_from(config.challenge_ttl.as_millis())
            .map_err(|_| AdminAuthError::Internal)?;
        let webauthn = WebauthnProbe::new(&config.rp_name, &config.rp_id, &config.origin, timeout)
            .map_err(|_| AdminAuthError::Internal)?;
        Ok(Self {
            store,
            webauthn: Arc::new(webauthn),
            config,
        })
    }

    pub async fn authentication_options(&self) -> Result<WebauthnOptions, AdminAuthError> {
        let issued = self
            .webauthn
            .start_authentication()
            .map_err(|_| AdminAuthError::Internal)?;
        self.persist_challenge(issued, ChallengePurpose::Authentication, None, None)
            .await
    }

    pub async fn bootstrap_registration_options(
        &self,
        token: &str,
    ) -> Result<WebauthnOptions, AdminAuthError> {
        let (id, secret) = parse_bootstrap_token(token)?;
        let bootstrap = self
            .store
            .bootstrap(id, digest(secret.as_bytes()))
            .await
            .map_err(internal)?
            .ok_or(AdminAuthError::InvalidBootstrap)?;
        self.registration_options(bootstrap.user_id, Some(bootstrap.id))
            .await
    }

    pub async fn complete_bootstrap_registration(
        &self,
        token: &str,
        challenge_id: Uuid,
        credential: Value,
        name: &str,
    ) -> Result<SessionCreation, AdminAuthError> {
        let (bootstrap_id, secret) = parse_bootstrap_token(token)?;
        let secret_hash = digest(secret.as_bytes());
        let bootstrap = self
            .store
            .bootstrap(bootstrap_id, secret_hash)
            .await
            .map_err(internal)?
            .ok_or(AdminAuthError::InvalidBootstrap)?;
        let challenge = self
            .store
            .consume_challenge(
                challenge_id,
                ChallengePurpose::BootstrapRegistration,
                Some(bootstrap.user_id),
                Some(bootstrap.id),
            )
            .await
            .map_err(internal)?
            .ok_or(AdminAuthError::InvalidChallenge)?;
        let stored = self.finish_registration(credential, challenge.ceremony_state, name)?;
        let secrets = self.new_session()?;
        let row = self
            .store
            .complete_bootstrap(bootstrap.id, secret_hash, stored, secrets.persisted)
            .await
            .map_err(map_credential_conflict)?
            .ok_or(AdminAuthError::InvalidBootstrap)?;
        Ok(SessionCreation {
            token: secrets.token,
            session: session_view(&row),
        })
    }

    pub async fn authenticate(
        &self,
        challenge_id: Uuid,
        credential: Value,
    ) -> Result<SessionCreation, AdminAuthError> {
        validate_authentication_payload(&credential).map_err(probe_payload_error)?;
        let external_id = credential
            .get("id")
            .and_then(Value::as_str)
            .ok_or(AdminAuthError::InvalidWebauthnPayload)?
            .to_owned();
        let challenge = self
            .store
            .consume_challenge(challenge_id, ChallengePurpose::Authentication, None, None)
            .await
            .map_err(internal)?
            .ok_or(AdminAuthError::InvalidAuthentication)?;
        let stored = self
            .store
            .active_credential_by_external_id(external_id)
            .await
            .map_err(internal)?
            .ok_or(AdminAuthError::InvalidAuthentication)?;
        let mut state = CeremonyState::from_persisted(challenge.ceremony_state)
            .map_err(|_| AdminAuthError::InvalidAuthentication)?;
        let update = self
            .webauthn
            .finish_authentication(credential, &probe_credential(&stored)?, &mut state)
            .map_err(|error| match error {
                ProbeError::InvalidPayload => AdminAuthError::InvalidWebauthnPayload,
                _ => AdminAuthError::InvalidAuthentication,
            })?;
        let secrets = self.new_session()?;
        let row = self
            .store
            .complete_authentication(
                stored.id,
                stored.counter,
                update.counter,
                device_type(update.device_type).to_owned(),
                update.backed_up,
                secrets.persisted,
            )
            .await
            .map_err(internal)?
            .ok_or(AdminAuthError::InvalidAuthentication)?;
        Ok(SessionCreation {
            token: secrets.token,
            session: session_view(&row),
        })
    }

    pub async fn authenticate_session(
        &self,
        token: &str,
    ) -> Result<Option<AuthenticatedAdmin>, AdminAuthError> {
        let idle_millis = i64::try_from(self.config.session_idle_ttl.as_millis())
            .map_err(|_| AdminAuthError::Internal)?;
        self.store
            .active_session(digest(token.as_bytes()), idle_millis)
            .await
            .map_err(internal)
            .map(|row| row.map(authenticated_admin))
    }

    pub async fn additional_registration_options(
        &self,
        user_id: Uuid,
    ) -> Result<WebauthnOptions, AdminAuthError> {
        self.registration_options(user_id, None).await
    }

    pub async fn add_credential(
        &self,
        user_id: Uuid,
        challenge_id: Uuid,
        credential: Value,
        name: &str,
    ) -> Result<(), AdminAuthError> {
        let challenge = self
            .store
            .consume_challenge(
                challenge_id,
                ChallengePurpose::AdditionalRegistration,
                Some(user_id),
                None,
            )
            .await
            .map_err(internal)?
            .ok_or(AdminAuthError::InvalidChallenge)?;
        let credential = self.finish_registration(credential, challenge.ceremony_state, name)?;
        let added = self
            .store
            .add_credential(user_id, credential)
            .await
            .map_err(map_credential_conflict)?;
        if !added {
            return Err(AdminAuthError::SessionInvalid);
        }
        Ok(())
    }

    pub async fn credentials(
        &self,
        user_id: Uuid,
        current_credential_id: Uuid,
    ) -> Result<Vec<CredentialView>, AdminAuthError> {
        self.store
            .active_credentials(user_id)
            .await
            .map_err(internal)
            .map(|rows| {
                rows.into_iter()
                    .map(|row| CredentialView {
                        id: row.id,
                        name: row.name,
                        device_type: row.device_type,
                        backed_up: row.backed_up,
                        transports: row.transports,
                        created_at: wire_timestamp(row.created_at),
                        last_used_at: row.last_used_at.map(wire_timestamp),
                        current: row.id == current_credential_id,
                    })
                    .collect()
            })
    }

    pub async fn sessions(
        &self,
        user_id: Uuid,
        current_session_id: Uuid,
    ) -> Result<Vec<SessionSummaryView>, AdminAuthError> {
        self.store
            .active_sessions(user_id)
            .await
            .map_err(internal)
            .map(|rows| {
                rows.into_iter()
                    .map(|row| SessionSummaryView {
                        id: row.id,
                        credential_id: row.credential_id,
                        credential_name: row.credential_name,
                        authenticated_at: wire_timestamp(row.authenticated_at),
                        last_seen_at: wire_timestamp(row.last_seen_at),
                        expires_at: wire_timestamp(row.expires_at),
                        current: row.id == current_session_id,
                    })
                    .collect()
            })
    }

    pub async fn revoke_selected_session(
        &self,
        user_id: Uuid,
        target: Uuid,
        current: Uuid,
    ) -> Result<(), AdminAuthError> {
        if target == current {
            return Err(AdminAuthError::CurrentSession);
        }
        if !self
            .store
            .revoke_selected_session(user_id, target, current)
            .await
            .map_err(internal)?
        {
            return Err(AdminAuthError::SessionNotFound);
        }
        Ok(())
    }

    pub async fn rename_credential(
        &self,
        user_id: Uuid,
        credential_id: Uuid,
        current_session_id: Uuid,
        name: &str,
    ) -> Result<(), AdminAuthError> {
        let name = normalized_name(name)?;
        if !self
            .store
            .rename_credential(user_id, credential_id, current_session_id, name)
            .await
            .map_err(internal)?
        {
            return Err(AdminAuthError::CredentialNotFound);
        }
        Ok(())
    }

    pub async fn revoke_credential(
        &self,
        user_id: Uuid,
        credential_id: Uuid,
        current_session_id: Uuid,
        current_credential_id: Uuid,
    ) -> Result<(), AdminAuthError> {
        if credential_id == current_credential_id {
            return Err(AdminAuthError::CurrentCredential);
        }
        match self
            .store
            .revoke_credential(user_id, credential_id, current_session_id)
            .await
            .map_err(internal)?
        {
            CredentialRevocation::Revoked => Ok(()),
            CredentialRevocation::NotFound => Err(AdminAuthError::CredentialNotFound),
            CredentialRevocation::LastCredential => Err(AdminAuthError::LastCredential),
        }
    }

    pub async fn revoke_other_sessions(
        &self,
        user_id: Uuid,
        current_session_id: Uuid,
    ) -> Result<u64, AdminAuthError> {
        self.store
            .revoke_other_sessions(user_id, current_session_id)
            .await
            .map_err(internal)
    }

    pub async fn revoke_session(
        &self,
        user_id: Uuid,
        session_id: Uuid,
    ) -> Result<(), AdminAuthError> {
        self.store
            .revoke_session(user_id, session_id)
            .await
            .map_err(internal)
    }

    pub async fn auth_events(
        &self,
        user_id: Uuid,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<AuthEventPage, AdminAuthError> {
        if !(1..=100).contains(&limit) {
            return Err(AdminAuthError::InvalidCursor);
        }
        let cursor = decode_cursor(cursor).map_err(|_| AdminAuthError::InvalidCursor)?;
        let mut rows = self
            .store
            .auth_events(user_id, limit + 1, cursor)
            .await
            .map_err(internal)?;
        let has_more = rows.len() > limit as usize;
        if has_more {
            rows.truncate(limit as usize);
        }
        let next_cursor = if has_more {
            rows.last()
                .map(|row| {
                    encode_cursor(&EventCursor {
                        at: row.created_at,
                        id: row.id,
                    })
                })
                .transpose()
                .map_err(|_| AdminAuthError::Internal)?
        } else {
            None
        };
        Ok(AuthEventPage {
            events: rows.into_iter().map(event_view).collect(),
            next_cursor,
        })
    }

    pub async fn issue_bootstrap(&self, user_id: Uuid) -> Result<IssuedBootstrap, AdminAuthError> {
        let mut secret = [0_u8; 32];
        random_fill(&mut secret).map_err(|_| AdminAuthError::Internal)?;
        let encoded = URL_SAFE_NO_PAD.encode(secret);
        let bootstrap_id = Uuid::new_v4();
        let expires_at = add_duration(Utc::now(), self.config.bootstrap_ttl)?;
        if !self
            .store
            .issue_bootstrap(
                user_id,
                bootstrap_id,
                digest(encoded.as_bytes()),
                expires_at,
            )
            .await
            .map_err(internal)?
        {
            return Err(AdminAuthError::SessionInvalid);
        }
        Ok(IssuedBootstrap {
            token: format!("{bootstrap_id}:{encoded}"),
            expires_at,
        })
    }

    async fn registration_options(
        &self,
        user_id: Uuid,
        bootstrap_id: Option<Uuid>,
    ) -> Result<WebauthnOptions, AdminAuthError> {
        let existing = self
            .store
            .active_credentials(user_id)
            .await
            .map_err(internal)?
            .into_iter()
            .map(|credential| ExistingCredential {
                credential_id: credential.credential_id,
                transports: credential.transports,
            })
            .collect::<Vec<_>>();
        let issued = self
            .webauthn
            .start_registration(
                *user_id.as_bytes(),
                &user_id.hyphenated().to_string(),
                &existing,
            )
            .map_err(|_| AdminAuthError::Internal)?;
        self.persist_challenge(
            issued,
            if bootstrap_id.is_some() {
                ChallengePurpose::BootstrapRegistration
            } else {
                ChallengePurpose::AdditionalRegistration
            },
            Some(user_id),
            bootstrap_id,
        )
        .await
    }

    async fn persist_challenge(
        &self,
        issued: crate::webauthn_probe::IssuedOptions,
        purpose: ChallengePurpose,
        user_id: Option<Uuid>,
        bootstrap_id: Option<Uuid>,
    ) -> Result<WebauthnOptions, AdminAuthError> {
        let state = issued
            .state
            .persisted()
            .map_err(|_| AdminAuthError::Internal)?
            .to_vec();
        let expires_at = add_duration(Utc::now(), self.config.challenge_ttl)?;
        let challenge_id = self
            .store
            .create_challenge(NewChallenge {
                purpose,
                challenge_hash: issued.challenge_hash,
                ceremony_state: state,
                user_id,
                bootstrap_id,
                expires_at,
            })
            .await
            .map_err(internal)?;
        Ok(WebauthnOptions {
            challenge_id,
            options: issued.options,
        })
    }

    fn finish_registration(
        &self,
        credential: Value,
        state: Vec<u8>,
        name: &str,
    ) -> Result<NewCredential, AdminAuthError> {
        let mut state = CeremonyState::from_persisted(state)
            .map_err(|_| AdminAuthError::InvalidRegistration)?;
        let verified = self
            .webauthn
            .finish_registration(credential, &mut state)
            .map_err(|error| match error {
                ProbeError::InvalidPayload => AdminAuthError::InvalidWebauthnPayload,
                _ => AdminAuthError::InvalidRegistration,
            })?;
        Ok(NewCredential {
            credential_id: verified.credential_id,
            public_key: verified.public_key,
            counter: verified.counter,
            device_type: device_type(verified.device_type).to_owned(),
            backed_up: verified.backed_up,
            transports: verified.transports,
            aaguid: verified.aaguid.map(Uuid::from_bytes),
            name: normalized_name(name)?,
        })
    }

    fn new_session(&self) -> Result<SessionSecrets, AdminAuthError> {
        let mut secret = [0_u8; 32];
        random_fill(&mut secret).map_err(|_| AdminAuthError::Internal)?;
        let token = URL_SAFE_NO_PAD.encode(secret);
        let now = Utc::now();
        Ok(SessionSecrets {
            persisted: NewSession {
                token_hash: digest(token.as_bytes()),
                idle_expires_at: add_duration(now, self.config.session_idle_ttl)?,
                absolute_expires_at: add_duration(now, self.config.session_absolute_ttl)?,
            },
            token,
        })
    }
}

struct SessionSecrets {
    token: String,
    persisted: NewSession,
}

fn parse_bootstrap_token(value: &str) -> Result<(Uuid, &str), AdminAuthError> {
    let mut parts = value.split(':');
    let id = parts.next().ok_or(AdminAuthError::InvalidBootstrap)?;
    let secret = parts.next().ok_or(AdminAuthError::InvalidBootstrap)?;
    if parts.next().is_some()
        || secret.len() != 43
        || !secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(AdminAuthError::InvalidBootstrap);
    }
    let id = Uuid::parse_str(id).map_err(|_| AdminAuthError::InvalidBootstrap)?;
    if id.get_version() != Some(Version::Random) || id.get_variant() != Variant::RFC4122 {
        return Err(AdminAuthError::InvalidBootstrap);
    }
    Ok((id, secret))
}

fn normalized_name(value: &str) -> Result<String, AdminAuthError> {
    let normalized = value.nfkc().collect::<String>();
    let normalized = normalized.trim();
    let utf16_len = normalized.encode_utf16().count();
    if !(1..=100).contains(&utf16_len)
        || normalized.len() > 200
        || normalized
            .chars()
            .any(|character| character <= '\u{1f}' || character == '\u{7f}')
    {
        return Err(AdminAuthError::CredentialNameInvalid);
    }
    Ok(normalized.to_owned())
}

fn probe_credential(value: &CredentialRow) -> Result<StoredCredential, AdminAuthError> {
    let device_type = match value.device_type.as_str() {
        "singleDevice" => DeviceType::SingleDevice,
        "multiDevice" => DeviceType::MultiDevice,
        _ => return Err(AdminAuthError::Internal),
    };
    Ok(StoredCredential {
        credential_id: value.credential_id.clone(),
        public_key: value.public_key.clone(),
        counter: value.counter,
        device_type,
        backed_up: value.backed_up,
        transports: value.transports.clone(),
        aaguid: value.aaguid.map(|id| *id.as_bytes()),
    })
}

const fn device_type(value: DeviceType) -> &'static str {
    match value {
        DeviceType::SingleDevice => "singleDevice",
        DeviceType::MultiDevice => "multiDevice",
    }
}

fn authenticated_admin(row: ActiveSessionRow) -> AuthenticatedAdmin {
    AuthenticatedAdmin {
        session_id: row.id,
        credential_id: row.credential_id,
        user_id: row.user_id,
        role: row.role,
        authenticated_at: row.authenticated_at,
        expires_at: row.expires_at,
    }
}

fn session_view(row: &ActiveSessionRow) -> AdminSessionView {
    AdminSessionView {
        user_id: row.user_id,
        role: row.role,
        authenticated_at: wire_timestamp(row.authenticated_at),
        expires_at: wire_timestamp(row.expires_at),
    }
}

fn event_view(row: AuthEventRow) -> AuthEventView {
    AuthEventView {
        id: row.id,
        event_type: row.event_type,
        credential_id: row.credential_id,
        session_id: row.session_id,
        created_at: wire_timestamp(row.created_at),
    }
}

fn digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

fn add_duration(now: DateTime<Utc>, duration: Duration) -> Result<DateTime<Utc>, AdminAuthError> {
    chrono::Duration::from_std(duration)
        .ok()
        .and_then(|duration| now.checked_add_signed(duration))
        .ok_or(AdminAuthError::Internal)
}

fn internal(_: DatabaseError) -> AdminAuthError {
    AdminAuthError::Internal
}

fn map_credential_conflict(error: DatabaseError) -> AdminAuthError {
    if error == DatabaseError::Constraint(ConstraintKind::Unique) {
        AdminAuthError::CredentialConflict
    } else {
        AdminAuthError::Internal
    }
}

fn probe_payload_error(error: ProbeError) -> AdminAuthError {
    if error == ProbeError::InvalidPayload {
        AdminAuthError::InvalidWebauthnPayload
    } else {
        AdminAuthError::InvalidAuthentication
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_bootstrap_tokens_without_fixed_uuid_fixtures() {
        let id = Uuid::new_v4();
        let secret = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        let token = format!("{id}:{secret}");
        assert_eq!(parse_bootstrap_token(&token), Ok((id, secret.as_str())));
        assert_eq!(
            parse_bootstrap_token(&format!("{id}:short")),
            Err(AdminAuthError::InvalidBootstrap)
        );
    }

    #[test]
    fn normalizes_names_and_rejects_control_or_oversized_values() {
        assert_eq!(
            normalized_name("  Clé principale  "),
            Ok("Clé principale".to_owned())
        );
        assert_eq!(
            normalized_name("bad\nname"),
            Err(AdminAuthError::CredentialNameInvalid)
        );
        assert_eq!(
            normalized_name(&"a".repeat(201)),
            Err(AdminAuthError::CredentialNameInvalid)
        );
    }
}
