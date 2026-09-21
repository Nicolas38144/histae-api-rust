use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use chrono::{DateTime, NaiveDate, TimeZone as _, Utc};
use histae_api_rust::config::{
    Environment, JwtConfig, LegalConfig, LimitPolicy, SecretString, TrustProxy,
};
use histae_api_rust::http::health::{DependencyProbe, ProbeFuture, Readiness};
use histae_api_rust::http::rate_limit::RateLimiter;
use histae_api_rust::http::router::{HttpState, build_router};
use histae_api_rust::identity::mobile::domain::{
    AccountRole, ActiveAccount, MobileSessionIdentity, MobileSessionRow, RotationOutcome,
    SessionCursor,
};
use histae_api_rust::identity::mobile::http::MobileAuthState;
use histae_api_rust::identity::mobile::pg::{MobileSessionStore, SessionStoreFuture};
use histae_api_rust::identity::mobile::service::{MobileAuthService, TokenPair};
use histae_api_rust::identity::mobile::tokens::{NewRefreshToken, TokenService};
use histae_api_rust::infra::postgres::DatabaseError;
use histae_api_rust::profiles::domain::{
    ConsentRecord, ConsentType, ModerationReason, ModerationStatus, Preferences, PreferencesInput,
    PresenceInput, ProfileInput, ProfileRecord, Sex, VersionedConsentChange, WriteOutcome,
};
use histae_api_rust::profiles::http::{ProfileHttpState, routes};
use histae_api_rust::profiles::pg::{ProfileStore, ProfileStoreFuture};
use histae_api_rust::profiles::service::{
    ProfilePhotoUrlFuture, ProfilePhotoUrlProvider, ProfileService,
};
use histae_api_rust::shared::clock::Clock;
use serde_json::{Value, json};
use tower::ServiceExt as _;
use url::Url;
use uuid::Uuid;

fn user_id() -> Uuid {
    static ID: OnceLock<Uuid> = OnceLock::new();
    *ID.get_or_init(Uuid::new_v4)
}

fn session_id() -> Uuid {
    static ID: OnceLock<Uuid> = OnceLock::new();
    *ID.get_or_init(Uuid::new_v4)
}

#[derive(Clone)]
struct Probe;

impl DependencyProbe for Probe {
    fn check(&self) -> ProbeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone)]
struct AuthStore {
    account: Arc<Mutex<Option<ActiveAccount>>>,
}

impl MobileSessionStore for AuthStore {
    fn create(
        &self,
        user_id: Uuid,
        _token: NewRefreshToken,
    ) -> SessionStoreFuture<'_, Option<MobileSessionIdentity>> {
        Box::pin(async move {
            Ok(Some(MobileSessionIdentity {
                user_id,
                session_id: session_id(),
            }))
        })
    }

    fn rotate(
        &self,
        _jti: Uuid,
        _hash: String,
        _next: NewRefreshToken,
    ) -> SessionStoreFuture<'_, RotationOutcome> {
        Box::pin(async { Ok(RotationOutcome::Invalid) })
    }

    fn logout(
        &self,
        _user_id: Uuid,
        _session_id: Uuid,
        _jti: Uuid,
        _hash: String,
        _device_id: Option<Uuid>,
    ) -> SessionStoreFuture<'_, bool> {
        Box::pin(async { Ok(false) })
    }

    fn list(
        &self,
        _user_id: Uuid,
        _limit: u32,
        _cursor: Option<SessionCursor>,
    ) -> SessionStoreFuture<'_, Vec<MobileSessionRow>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn revoke(
        &self,
        _user_id: Uuid,
        _current_session_id: Uuid,
        _target_id: Option<Uuid>,
    ) -> SessionStoreFuture<'_, Option<u64>> {
        Box::pin(async { Ok(Some(0)) })
    }

    fn active_account(
        &self,
        _user_id: Uuid,
        _session_id: Uuid,
        _terms_version: Arc<str>,
        _privacy_version: Arc<str>,
    ) -> SessionStoreFuture<'_, Option<ActiveAccount>> {
        Box::pin(async move {
            self.account
                .lock()
                .map(|account| account.clone())
                .map_err(|_| DatabaseError::QueryFailed)
        })
    }
}

#[derive(Clone)]
struct FakeProfileStore {
    state: Arc<Mutex<ProfileState>>,
}

#[derive(Clone)]
struct ProfileState {
    profile: Option<ProfileRecord>,
    preferences: Option<Preferences>,
    consents: Vec<ConsentRecord>,
    write_outcome: WriteOutcome,
    record_consents: bool,
    database_error: Option<DatabaseError>,
    last_profile: Option<ProfileInput>,
    last_preferences: Option<PreferencesInput>,
    last_presence: Option<PresenceInput>,
    last_consent_changes: Vec<VersionedConsentChange>,
}

impl FakeProfileStore {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ProfileState {
                profile: None,
                preferences: None,
                consents: Vec::new(),
                write_outcome: WriteOutcome::Updated,
                record_consents: true,
                database_error: None,
                last_profile: None,
                last_preferences: None,
                last_presence: None,
                last_consent_changes: Vec::new(),
            })),
        }
    }

    fn result<T>(state: &ProfileState, value: T) -> Result<T, DatabaseError> {
        state.database_error.map_or(Ok(value), Err)
    }
}

impl ProfileStore for FakeProfileStore {
    fn find_profile(&self, _user_id: Uuid) -> ProfileStoreFuture<'_, Option<ProfileRecord>> {
        Box::pin(async move {
            let state = self.state.lock().map_err(|_| DatabaseError::QueryFailed)?;
            Self::result(&state, state.profile.clone())
        })
    }

    fn upsert_profile(
        &self,
        _user_id: Uuid,
        input: ProfileInput,
        _legal: LegalConfig,
    ) -> ProfileStoreFuture<'_, WriteOutcome> {
        Box::pin(async move {
            let mut state = self.state.lock().map_err(|_| DatabaseError::QueryFailed)?;
            state.last_profile = Some(input);
            Self::result(&state, state.write_outcome)
        })
    }

    fn find_preferences(&self, _user_id: Uuid) -> ProfileStoreFuture<'_, Option<Preferences>> {
        Box::pin(async move {
            let state = self.state.lock().map_err(|_| DatabaseError::QueryFailed)?;
            Self::result(&state, state.preferences.clone())
        })
    }

    fn upsert_preferences(
        &self,
        _user_id: Uuid,
        input: PreferencesInput,
        _legal: LegalConfig,
    ) -> ProfileStoreFuture<'_, WriteOutcome> {
        Box::pin(async move {
            let mut state = self.state.lock().map_err(|_| DatabaseError::QueryFailed)?;
            state.last_preferences = Some(input);
            Self::result(&state, state.write_outcome)
        })
    }

    fn upsert_presence(
        &self,
        _user_id: Uuid,
        input: PresenceInput,
        _legal: LegalConfig,
    ) -> ProfileStoreFuture<'_, WriteOutcome> {
        Box::pin(async move {
            let mut state = self.state.lock().map_err(|_| DatabaseError::QueryFailed)?;
            state.last_presence = Some(input);
            Self::result(&state, state.write_outcome)
        })
    }

    fn current_consents(&self, _user_id: Uuid) -> ProfileStoreFuture<'_, Vec<ConsentRecord>> {
        Box::pin(async move {
            let state = self.state.lock().map_err(|_| DatabaseError::QueryFailed)?;
            Self::result(&state, state.consents.clone())
        })
    }

    fn record_consents(
        &self,
        _user_id: Uuid,
        changes: Vec<VersionedConsentChange>,
        _ip_address: String,
        _user_agent: String,
    ) -> ProfileStoreFuture<'_, bool> {
        Box::pin(async move {
            let mut state = self.state.lock().map_err(|_| DatabaseError::QueryFailed)?;
            state.last_consent_changes = changes;
            Self::result(&state, state.record_consents)
        })
    }
}

#[derive(Clone)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

#[derive(Clone)]
struct PhotoUrls;

impl ProfilePhotoUrlProvider for PhotoUrls {
    fn url_for_key(&self, object_key: Option<String>) -> ProfilePhotoUrlFuture<'_> {
        Box::pin(async move {
            Ok(object_key.map(|key| format!("https://storage.example.test/{key}?signed=true")))
        })
    }
}

fn legal() -> LegalConfig {
    LegalConfig {
        terms_version: "terms-v1".to_owned(),
        privacy_version: "privacy-v1".to_owned(),
        sensitive_data_consent_version: "sensitive-v1".to_owned(),
        location_consent_version: "location-v1".to_owned(),
        terms_url: Url::parse("https://histae.test/legal/terms").expect("terms URL"),
        privacy_url: Url::parse("https://histae.test/legal/privacy").expect("privacy URL"),
        sensitive_data_consent_url: Url::parse("https://histae.test/legal/sensitive-data")
            .expect("sensitive URL"),
        location_consent_url: Url::parse("https://histae.test/legal/location")
            .expect("location URL"),
        review_reference: "test-review".to_owned(),
    }
}

fn jwt_config() -> JwtConfig {
    let secret = SecretString::new("jwt-signing-secret-0123456789abcdef".to_owned());
    JwtConfig {
        secret: secret.clone(),
        active_kid: "primary".to_owned(),
        verification_keys: BTreeMap::from([("primary".to_owned(), secret)]),
        access_ttl: Duration::from_secs(900),
        refresh_ttl: Duration::from_secs(3_600),
    }
}

fn app(store: FakeProfileStore, onboarded: bool) -> (axum::Router, TokenPair) {
    let account = ActiveAccount {
        user_id: user_id(),
        role: AccountRole::User,
        is_banned: false,
        onboarding_complete: onboarded,
    };
    let auth_store = AuthStore {
        account: Arc::new(Mutex::new(Some(account))),
    };
    let tokens = TokenService::new(jwt_config());
    let auth_service = MobileAuthService::new(
        tokens.clone(),
        Arc::new(auth_store),
        "terms-v1".to_owned(),
        "privacy-v1".to_owned(),
    );
    let limiter = RateLimiter::memory(&SecretString::new(
        "hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh".to_owned(),
    ));
    let auth_state = MobileAuthState::new(
        auth_service,
        limiter.clone(),
        LimitPolicy {
            max: 30,
            window: Duration::from_secs(900),
        },
    );
    let service = ProfileService::new(
        Arc::new(store),
        legal(),
        Arc::new(FixedClock(
            Utc.with_ymd_and_hms(2026, 9, 21, 12, 0, 0)
                .single()
                .expect("clock"),
        )),
        Arc::new(PhotoUrls),
    );
    let readiness = Readiness::new(Arc::new(Probe), Arc::new(Probe), Arc::new(Probe));
    let http_state = HttpState::new(
        readiness,
        Environment::Test,
        &TrustProxy::Disabled,
        &[],
        limiter,
        LimitPolicy {
            max: 100,
            window: Duration::from_secs(60),
        },
    )
    .expect("HTTP state");
    let pair = TokenPair {
        access_token: tokens
            .access_token(user_id(), session_id())
            .expect("access token"),
        refresh_token: String::new(),
    };
    (
        build_router(
            routes(ProfileHttpState::new(service), auth_state),
            http_state,
        ),
        pair,
    )
}

fn request(method: &str, uri: &str, token: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if !body.is_empty() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder.body(Body::from(body.to_owned())).expect("request")
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    serde_json::from_slice(&bytes).expect("JSON response")
}

#[tokio::test]
async fn consents_are_available_during_onboarding_and_keep_the_public_shape() {
    let store = FakeProfileStore::new();
    store.state.lock().expect("state").consents = vec![ConsentRecord {
        consent_type: ConsentType::TermsOfServiceAcceptance,
        granted: true,
        document_version: "terms-v1".to_owned(),
        granted_at: Utc
            .with_ymd_and_hms(2030, 1, 1, 0, 0, 0)
            .single()
            .expect("date"),
    }];
    let (app, tokens) = app(store, false);
    let response = app
        .oneshot(request(
            "GET",
            "/api/users/me/consents",
            Some(&tokens.access_token),
            "",
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json_body(response).await,
        json!({
            "consents": [
                {
                    "consent_type": "terms_of_service_acceptance",
                    "granted": true,
                    "document_version": "terms-v1",
                    "required_document_version": "terms-v1",
                    "document_url": "https://histae.test/legal/terms",
                    "updated_at": "2030-01-01T00:00:00.000Z"
                },
                {
                    "consent_type": "privacy_notice_acknowledgement",
                    "granted": false,
                    "required_document_version": "privacy-v1",
                    "document_url": "https://histae.test/legal/privacy"
                },
                {
                    "consent_type": "sensitive_data_consent",
                    "granted": false,
                    "required_document_version": "sensitive-v1",
                    "document_url": "https://histae.test/legal/sensitive-data"
                },
                {
                    "consent_type": "location_consent",
                    "granted": false,
                    "required_document_version": "location-v1",
                    "document_url": "https://histae.test/legal/location"
                }
            ],
            "onboarding_complete": false,
            "required_actions": ["privacy_notice_acknowledgement"]
        })
    );
}

#[tokio::test]
async fn authentication_and_onboarding_precede_profile_body_validation() {
    let store = FakeProfileStore::new();
    let (incomplete, tokens) = app(store.clone(), false);
    let missing = incomplete
        .clone()
        .oneshot(request("GET", "/api/users/me", None, ""))
        .await
        .expect("response");
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    let incomplete_response = incomplete
        .oneshot(request(
            "PATCH",
            "/api/users/me/profile",
            Some(&tokens.access_token),
            r#"{"unknown":true}"#,
        ))
        .await
        .expect("response");
    assert_eq!(incomplete_response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(incomplete_response).await["error"]["code"],
        "onboarding_incomplete"
    );
}

#[tokio::test]
async fn profile_projection_and_omitted_nullable_fields_match_nest() {
    let store = FakeProfileStore::new();
    store.state.lock().expect("state").profile = Some(ProfileRecord {
        user_id: user_id(),
        firstname: "Alice".to_owned(),
        birthdate: NaiveDate::from_ymd_opt(1995, 4, 12).expect("date"),
        sex: Some(Sex::Female),
        bio: Some("Contacte-moi".to_owned()),
        photo_object_key: Some(format!(
            "profile-photos/{}/{}.webp",
            user_id(),
            Uuid::new_v4()
        )),
        profile_answers: vec![json!({ "position": 1, "answer": "Une réponse" })],
        bio_moderation_status: Some(ModerationStatus::Pending),
        bio_moderation_reasons: vec![ModerationReason::PersonalContact],
        photo_moderation_status: Some(ModerationStatus::Approved),
        photo_moderation_reasons: Vec::new(),
    });
    let (app, tokens) = app(store.clone(), true);
    let profile = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/users/me",
            Some(&tokens.access_token),
            "",
        ))
        .await
        .expect("response");
    let payload = json_body(profile).await;
    assert_eq!(payload["firstname"], "Alice");
    assert_eq!(payload["birthdate"], "1995-04-12");
    assert_eq!(payload["bio"], "Contacte-moi");
    assert_eq!(payload["moderation"]["bio"]["status"], "pending");
    assert_eq!(
        payload["moderation"]["bio"]["reasons"][0],
        "personal_contact"
    );
    assert!(
        payload["photo"]
            .as_str()
            .is_some_and(|url| url.contains("signed=true"))
    );

    let updated = app
        .oneshot(request(
            "PATCH",
            "/api/users/me/profile",
            Some(&tokens.access_token),
            r#"{"firstname":"  Alice  ","birthdate":"1995-04-12"}"#,
        ))
        .await
        .expect("response");
    assert_eq!(updated.status(), StatusCode::OK);
    assert_eq!(
        json_body(updated).await,
        json!({ "message": "profile updated" })
    );
    let state = store.state.lock().expect("state");
    let saved = state.last_profile.as_ref().expect("profile write");
    assert_eq!(saved.firstname, "Alice");
    assert_eq!(saved.sex, None);
    assert_eq!(saved.bio, None);
}

#[tokio::test]
async fn validation_keeps_dto_and_business_error_codes_distinct() {
    let store = FakeProfileStore::new();
    let (app, tokens) = app(store, true);
    let cases = [
        (
            r#"{"firstname":"Alice","birthdate":"1995-04-12","role":"admin"}"#,
            "invalid_profile_payload",
        ),
        (
            r#"{"firstname":"Alice","birthdate":"2000-02-30"}"#,
            "invalid_profile",
        ),
        (
            r#"{"firstname":"Alice","birthdate":"2010-01-01"}"#,
            "invalid_profile",
        ),
    ];
    for (body, code) in cases {
        let response = app
            .clone()
            .oneshot(request(
                "PATCH",
                "/api/users/me/profile",
                Some(&tokens.access_token),
                body,
            ))
            .await
            .expect("response");
        assert_eq!(json_body(response).await["error"]["code"], code);
    }

    let invalid_preferences = app
        .clone()
        .oneshot(request(
            "PATCH",
            "/api/users/me/preferences",
            Some(&tokens.access_token),
            r#"{"min_age":25.5,"max_age":40,"max_distance_km":30,"looking_for":"both"}"#,
        ))
        .await
        .expect("response");
    assert_eq!(
        json_body(invalid_preferences).await["error"]["code"],
        "invalid_preferences"
    );
    let invalid_presence = app
        .oneshot(request(
            "PATCH",
            "/api/users/me/presence",
            Some(&tokens.access_token),
            r#"{"latitude":91,"longitude":2}"#,
        ))
        .await
        .expect("response");
    assert_eq!(
        json_body(invalid_presence).await["error"]["code"],
        "invalid_presence"
    );
}

#[tokio::test]
async fn missing_resources_consent_failures_and_late_write_errors_are_stable() {
    let store = FakeProfileStore::new();
    let (app, tokens) = app(store.clone(), true);
    let profile = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/users/me",
            Some(&tokens.access_token),
            "",
        ))
        .await
        .expect("response");
    assert_eq!(
        json_body(profile).await["error"]["code"],
        "profile_not_found"
    );
    let preferences = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/users/me/preferences",
            Some(&tokens.access_token),
            "",
        ))
        .await
        .expect("response");
    assert_eq!(
        json_body(preferences).await["error"]["code"],
        "preferences_not_found"
    );

    store.state.lock().expect("state").write_outcome = WriteOutcome::RequiredConsentMissing;
    let consent = app
        .clone()
        .oneshot(request(
            "PATCH",
            "/api/users/me/presence",
            Some(&tokens.access_token),
            r#"{"latitude":48,"longitude":2}"#,
        ))
        .await
        .expect("response");
    assert_eq!(consent.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(consent).await["error"]["code"],
        "required_consent_missing"
    );

    {
        let mut state = store.state.lock().expect("state");
        state.write_outcome = WriteOutcome::Updated;
        state.database_error = Some(DatabaseError::AccountUnavailable);
    }
    let unavailable = app
        .oneshot(request(
            "PATCH",
            "/api/users/me/presence",
            Some(&tokens.access_token),
            r#"{"latitude":48,"longitude":2}"#,
        ))
        .await
        .expect("response");
    assert_eq!(unavailable.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(unavailable).await,
        json!({ "error": { "code": "account_unavailable", "message": "An account is no longer available." } })
    );
}

#[tokio::test]
async fn consent_payload_rules_and_version_assignment_are_preserved() {
    let store = FakeProfileStore::new();
    let (app, tokens) = app(store.clone(), false);
    for body in [
        r#"{"consents":[]}"#,
        r#"{"consents":[{"consent_type":"advertising","granted":true}]}"#,
        r#"{"consents":[{"consent_type":"terms_of_service_acceptance","granted":false}]}"#,
        r#"{"consents":[{"consent_type":"location_consent","granted":true},{"consent_type":"location_consent","granted":false}]}"#,
    ] {
        let response = app
            .clone()
            .oneshot(request(
                "PUT",
                "/api/users/me/consents",
                Some(&tokens.access_token),
                body,
            ))
            .await
            .expect("response");
        assert_eq!(
            json_body(response).await["error"]["code"],
            "invalid_consent_payload"
        );
    }
    let accepted = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/users/me/consents")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", tokens.access_token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::USER_AGENT, "Histae/1.0")
                .body(Body::from(
                    r#"{"consents":[{"consent_type":"sensitive_data_consent","granted":true}]}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(accepted.status(), StatusCode::OK);
    let state = store.state.lock().expect("state");
    assert_eq!(state.last_consent_changes.len(), 1);
    assert_eq!(
        state.last_consent_changes[0].document_version,
        "sensitive-v1"
    );
}
