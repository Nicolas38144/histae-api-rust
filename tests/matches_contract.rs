use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use chrono::{DateTime, NaiveDate, TimeZone as _, Utc};
use histae_api_rust::config::{Environment, JwtConfig, LimitPolicy, SecretString, TrustProxy};
use histae_api_rust::http::health::{DependencyProbe, ProbeFuture, Readiness};
use histae_api_rust::http::rate_limit::RateLimiter;
use histae_api_rust::http::router::{HttpState, build_router};
use histae_api_rust::identity::mobile::domain::{
    AccountRole, ActiveAccount, MobileSessionIdentity, MobileSessionRow, RotationOutcome,
    SessionCursor,
};
use histae_api_rust::identity::mobile::http::MobileAuthState;
use histae_api_rust::identity::mobile::pg::{MobileSessionStore, SessionStoreFuture};
use histae_api_rust::identity::mobile::service::MobileAuthService;
use histae_api_rust::identity::mobile::tokens::{NewRefreshToken, TokenService};
use histae_api_rust::infra::postgres::DatabaseError;
use histae_api_rust::matches::domain::{
    ContinuationResult, EffectivePlan, LastMessageRow, MatchCommandResult, MatchRecord,
    MatchStatus, PageCursor, UserMatchRow,
};
use histae_api_rust::matches::http::{MatchHttpState, routes};
use histae_api_rust::matches::pg::{MatchStore, MatchStoreError, MatchStoreFuture};
use histae_api_rust::matches::service::{MatchService, NoopMatchEventPublisher};
use histae_api_rust::profiles::domain::Sex;
use histae_api_rust::profiles::service::{ProfilePhotoUrlFuture, ProfilePhotoUrlProvider};
use histae_api_rust::shared::clock::Clock;
use serde_json::{Value, json};
use tower::ServiceExt as _;
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
    account: ActiveAccount,
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
        let account = self.account.clone();
        Box::pin(async move { Ok(Some(account)) })
    }
}

#[derive(Clone)]
struct FakeMatchStore {
    state: Arc<Mutex<MatchState>>,
}

#[derive(Clone)]
struct MatchState {
    rows: Vec<UserMatchRow>,
    reveal: MatchCommandResult<bool>,
    continuation: ContinuationResult,
    plan: EffectivePlan,
    usage: i32,
    calls: usize,
}

impl FakeMatchStore {
    fn new(row: UserMatchRow) -> Self {
        Self {
            state: Arc::new(Mutex::new(MatchState {
                rows: vec![row],
                reveal: MatchCommandResult::Available(false),
                continuation: ContinuationResult::Pending,
                plan: EffectivePlan {
                    plan: "free".to_owned(),
                    weekly_limit: Some(2),
                },
                usage: 1,
                calls: 0,
            })),
        }
    }

    fn calls(&self) -> usize {
        self.state.lock().expect("match state").calls
    }
}

impl MatchStore for FakeMatchStore {
    fn create(&self, _record: MatchRecord) -> MatchStoreFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn find_by_pair(
        &self,
        _user1_id: Uuid,
        _user2_id: Uuid,
    ) -> MatchStoreFuture<'_, Option<MatchRecord>> {
        Box::pin(async { Ok(None) })
    }

    fn list_for_user(
        &self,
        _user_id: Uuid,
        _limit: u32,
        _offset: u32,
        _cursor: Option<PageCursor>,
    ) -> MatchStoreFuture<'_, Vec<UserMatchRow>> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .map_err(|_| MatchStoreError::Database(DatabaseError::QueryFailed))?;
            state.calls += 1;
            Ok(state.rows.clone())
        })
    }

    fn record_reveal(
        &self,
        _match_id: Uuid,
        _user_id: Uuid,
    ) -> MatchStoreFuture<'_, MatchCommandResult<bool>> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .map_err(|_| MatchStoreError::Database(DatabaseError::QueryFailed))?;
            state.calls += 1;
            Ok(state.reveal.clone())
        })
    }

    fn participant_ids(
        &self,
        _match_id: Uuid,
        _user_id: Uuid,
    ) -> MatchStoreFuture<'_, Option<[Uuid; 2]>> {
        Box::pin(async { Ok(None) })
    }

    fn effective_plan(
        &self,
        _user_id: Uuid,
        _now: DateTime<Utc>,
    ) -> MatchStoreFuture<'_, EffectivePlan> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .map_err(|_| MatchStoreError::Database(DatabaseError::QueryFailed))?;
            state.calls += 1;
            Ok(state.plan.clone())
        })
    }

    fn continuation_usage(
        &self,
        _user_id: Uuid,
        _week_start: NaiveDate,
    ) -> MatchStoreFuture<'_, i32> {
        Box::pin(async move {
            let state = self
                .state
                .lock()
                .map_err(|_| MatchStoreError::Database(DatabaseError::QueryFailed))?;
            Ok(state.usage)
        })
    }

    fn record_continuation(
        &self,
        _match_id: Uuid,
        _user_id: Uuid,
    ) -> MatchStoreFuture<'_, ContinuationResult> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .map_err(|_| MatchStoreError::Database(DatabaseError::QueryFailed))?;
            state.calls += 1;
            Ok(state.continuation)
        })
    }
}

#[derive(Clone)]
struct PhotoUrls;

impl ProfilePhotoUrlProvider for PhotoUrls {
    fn url_for_key(&self, object_key: Option<String>) -> ProfilePhotoUrlFuture<'_> {
        Box::pin(async move { Ok(object_key.map(|key| format!("https://photos.invalid/{key}"))) })
    }
}

#[derive(Clone, Copy)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

fn row() -> UserMatchRow {
    let match_id = Uuid::new_v4();
    let other_user_id = Uuid::new_v4();
    let message_id = Uuid::new_v4();
    let created_at = Utc
        .with_ymd_and_hms(2030, 1, 1, 12, 0, 0)
        .single()
        .expect("date");
    UserMatchRow {
        record: MatchRecord {
            id: match_id,
            user1_id: user_id().min(other_user_id),
            user2_id: user_id().max(other_user_id),
            status: MatchStatus::Active,
            expires_at: Utc
                .with_ymd_and_hms(2030, 1, 2, 12, 0, 0)
                .single()
                .expect("date"),
            purge_after: None,
            continuation_initiator_id: None,
            created_at,
            last_message_at: Some(created_at),
        },
        cursor_at: "2030-01-01T12:00:00.000000Z".to_owned(),
        other_user_id,
        other_firstname: "Ariane".to_owned(),
        other_age: 29,
        other_sex: Some(Sex::Female),
        other_bio: Some("Bio approved".to_owned()),
        other_photo: Some(format!(
            "profile-photos/{other_user_id}/{}.webp",
            Uuid::new_v4()
        )),
        other_traits: vec!["calm".to_owned()],
        other_profile_answers: vec![json!({
            "question_id": Uuid::new_v4(),
            "question": "A question?",
            "answer": "An approved answer",
            "position": 1
        })],
        my_revealed: true,
        photos_revealed: true,
        my_continued: false,
        unread_count: 1,
        last_message: Some(LastMessageRow {
            id: message_id,
            sender_id: other_user_id,
            content: "hello".to_owned(),
            created_at,
            read_at: None,
        }),
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

fn app(store: FakeMatchStore, onboarded: bool) -> (axum::Router, String) {
    let account = ActiveAccount {
        user_id: user_id(),
        role: AccountRole::User,
        is_banned: false,
        onboarding_complete: onboarded,
    };
    let tokens = TokenService::new(jwt_config());
    let auth_service = MobileAuthService::new(
        tokens.clone(),
        Arc::new(AuthStore { account }),
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
    let access_token = tokens
        .access_token(user_id(), session_id())
        .expect("access token");
    let clock = Utc
        .with_ymd_and_hms(2030, 1, 6, 23, 59, 59)
        .single()
        .expect("date");
    let service = MatchService::new(
        Arc::new(store),
        Arc::new(PhotoUrls),
        Arc::new(NoopMatchEventPublisher),
        Arc::new(FixedClock(clock)),
    );
    (
        build_router(routes(MatchHttpState::new(service), auth_state), http_state),
        access_token,
    )
}

fn request(method: &str, uri: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("request")
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    serde_json::from_slice(&bytes).expect("JSON response")
}

#[tokio::test]
async fn lists_the_exact_public_projection_and_signs_only_the_projected_photo() {
    let expected = row();
    let store = FakeMatchStore::new(expected.clone());
    let (app, token) = app(store, true);
    let response = app
        .oneshot(request("GET", "/api/matches/me", Some(&token)))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let object_key = expected.other_photo.as_ref().expect("photo key");
    assert_eq!(
        json_body(response).await,
        json!({
            "matches": [{
                "id": expected.record.id,
                "status": "active",
                "expires_at": "2030-01-02T12:00:00.000Z",
                "created_at": "2030-01-01T12:00:00.000Z",
                "last_message_at": "2030-01-01T12:00:00.000Z",
                "other_user": {
                    "user_id": expected.other_user_id,
                    "firstname": "Ariane",
                    "age": 29,
                    "sex": "female",
                    "bio": "Bio approved",
                    "traits": ["calm"],
                    "photo": format!("https://photos.invalid/{object_key}"),
                    "profile_answers": expected.other_profile_answers
                },
                "my_revealed": true,
                "photos_revealed": true,
                "my_continued": false,
                "unread_count": 1,
                "last_message": {
                    "id": expected.last_message.as_ref().expect("message").id,
                    "sender_id": expected.other_user_id,
                    "content": "hello",
                    "created_at": "2030-01-01T12:00:00.000Z",
                    "read_at": null
                }
            }],
            "next_cursor": null
        })
    );
}

#[tokio::test]
async fn preserves_reveal_continuation_and_quota_responses() {
    let store = FakeMatchStore::new(row());
    let match_id = store.state.lock().expect("state").rows[0].record.id;
    store.state.lock().expect("state").reveal = MatchCommandResult::Available(true);
    store.state.lock().expect("state").continuation = ContinuationResult::Confirmed;
    let (app, token) = app(store, true);

    let response = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/matches/{match_id}/reveal"),
            Some(&token),
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json_body(response).await,
        json!({
            "message": "Both participants agreed to reveal their profile photos.",
            "photos_revealed": true
        })
    );

    let response = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/matches/{match_id}/continue"),
            Some(&token),
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json_body(response).await,
        json!({
            "message": "Both participants agreed to continue the match.",
            "match_confirmed": true
        })
    );

    let response = app
        .oneshot(request(
            "GET",
            "/api/users/me/continuation-quota",
            Some(&token),
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json_body(response).await,
        json!({"plan": "free", "used": 1, "weekly_limit": 2, "remaining": 1})
    );
}

#[tokio::test]
async fn rejects_unauthenticated_invalid_and_not_onboarded_requests_before_storage() {
    let store = FakeMatchStore::new(row());
    let match_id = store.state.lock().expect("state").rows[0].record.id;
    let (router, token) = app(store.clone(), true);

    let unauthenticated = router
        .clone()
        .oneshot(request("GET", "/api/matches/me", None))
        .await
        .expect("response");
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let invalid_query = router
        .clone()
        .oneshot(request("GET", "/api/matches/me?limit=101", Some(&token)))
        .await
        .expect("response");
    assert_eq!(invalid_query.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(invalid_query).await,
        json!({"error": {"code": "invalid_pagination", "message": "Pagination parameters are invalid."}})
    );

    let invalid_path = router
        .oneshot(request(
            "PATCH",
            "/api/matches/not-an-id/reveal",
            Some(&token),
        ))
        .await
        .expect("response");
    assert_eq!(invalid_path.status(), StatusCode::BAD_REQUEST);
    assert_eq!(store.calls(), 0);

    let (not_onboarded, token) = app(store.clone(), false);
    let response = not_onboarded
        .oneshot(request(
            "PATCH",
            &format!("/api/matches/{match_id}/continue"),
            Some(&token),
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(store.calls(), 0);
}

#[tokio::test]
async fn maps_expiration_and_quota_failures_to_the_nest_contract() {
    let store = FakeMatchStore::new(row());
    let match_id = store.state.lock().expect("state").rows[0].record.id;
    store.state.lock().expect("state").reveal = MatchCommandResult::Unavailable(
        histae_api_rust::matches::domain::MatchAvailabilityFailure::Expired,
    );
    store.state.lock().expect("state").continuation = ContinuationResult::QuotaReached;
    let (app, token) = app(store, true);

    let expired = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/matches/{match_id}/reveal"),
            Some(&token),
        ))
        .await
        .expect("response");
    assert_eq!(expired.status(), StatusCode::GONE);
    assert_eq!(
        json_body(expired).await,
        json!({"error": {"code": "match_expired", "message": "This match has expired."}})
    );

    let quota = app
        .oneshot(request(
            "PATCH",
            &format!("/api/matches/{match_id}/continue"),
            Some(&token),
        ))
        .await
        .expect("response");
    assert_eq!(quota.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(quota).await,
        json!({"error": {
            "code": "continuation_quota_reached",
            "message": "The weekly continuation quota has been reached. Upgrade to Premium for unlimited continuations."
        }})
    );
}
