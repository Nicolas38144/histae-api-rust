use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use chrono::Utc;
use histae_api_rust::config::{
    Environment, JwtConfig, LegalConfig, LimitPolicy, SecretString, TrustProxy,
};
use histae_api_rust::discovery::cursor::FeedCursorCodec;
use histae_api_rust::discovery::domain::{
    DiscoveryCandidateRow, DiscoveryCursor, DiscoveryStatusRow, RecordedSwipe, SwipeDecision,
    SwipeRecord,
};
use histae_api_rust::discovery::http::{DiscoveryHttpState, routes};
use histae_api_rust::discovery::service::{
    DiscoveryError, DiscoveryService, MatchCreator, MatchCreatorFuture,
};
use histae_api_rust::discovery::store::{
    DiscoveryRepository, DiscoveryStoreError, DiscoveryStoreFuture, SwipeStore,
};
use histae_api_rust::http::health::{DependencyProbe, ProbeFuture, Readiness};
use histae_api_rust::http::rate_limit::RateLimiter;
use histae_api_rust::http::router::{HttpState, build_router};
use histae_api_rust::identity::mobile::domain::{
    AccountRole, ActiveAccount, MobileSessionIdentity, MobileSessionRow, RotationOutcome,
    SessionCursor,
};
use histae_api_rust::identity::mobile::http::MobileAuthState;
use histae_api_rust::identity::mobile::service::MobileAuthService;
use histae_api_rust::identity::mobile::store::{MobileSessionStore, SessionStoreFuture};
use histae_api_rust::identity::mobile::tokens::{NewRefreshToken, TokenService};
use histae_api_rust::infra::postgres::DatabaseError;
use histae_api_rust::matches::domain::{MatchStatus, PublicMatch};
use histae_api_rust::matches::service::MatchError;
use histae_api_rust::profiles::domain::Sex;
use histae_api_rust::shared::clock::SystemClock;
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
struct AuthStore;

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
        Box::pin(async {
            Ok(Some(ActiveAccount {
                user_id: user_id(),
                role: AccountRole::User,
                is_banned: false,
                onboarding_complete: true,
            }))
        })
    }
}

#[derive(Clone)]
struct FakeRepository {
    ready: bool,
    candidates: Vec<DiscoveryCandidateRow>,
}

impl DiscoveryRepository for FakeRepository {
    fn status(
        &self,
        _user_id: Uuid,
        _sensitive_version: &str,
        _location_version: &str,
    ) -> DiscoveryStoreFuture<'_, DiscoveryStatusRow> {
        let ready = self.ready;
        Box::pin(async move {
            Ok(DiscoveryStatusRow {
                has_profile: ready,
                has_sex: ready,
                has_preferences: ready,
                has_sensitive_consent: ready,
                has_location_consent: ready,
                has_fresh_presence: ready,
                presence_expires_at: None,
            })
        })
    }

    fn is_ready(
        &self,
        _user_id: Uuid,
        _sensitive_version: &str,
        _location_version: &str,
    ) -> DiscoveryStoreFuture<'_, bool> {
        let ready = self.ready;
        Box::pin(async move { Ok(ready) })
    }

    fn candidate_batch(
        &self,
        _user_id: Uuid,
        _sensitive_version: &str,
        _location_version: &str,
        limit: u32,
        cursor: Option<DiscoveryCursor>,
        target_id: Option<Uuid>,
    ) -> DiscoveryStoreFuture<'_, Vec<DiscoveryCandidateRow>> {
        let candidates = self.candidates.clone();
        Box::pin(async move {
            Ok(candidates
                .into_iter()
                .filter(|candidate| {
                    target_id.is_none_or(|target| candidate.user_id == target)
                        && cursor.is_none_or(|cursor| {
                            (candidate.distance_km, candidate.user_id)
                                > (cursor.distance_km, cursor.id)
                        })
                })
                .take(limit as usize)
                .collect())
        })
    }
}

#[derive(Clone, Default)]
struct FakeSwipes {
    rows: Arc<Mutex<HashMap<(Uuid, Uuid), SwipeDecision>>>,
}

impl SwipeStore for FakeSwipes {
    fn record(
        &self,
        actor_id: Uuid,
        target_id: Uuid,
        decision: SwipeDecision,
    ) -> DiscoveryStoreFuture<'_, RecordedSwipe> {
        Box::pin(async move {
            let mut rows = self
                .rows
                .lock()
                .map_err(|_| DiscoveryStoreError::InvalidStoredData)?;
            if let Some(existing) = rows.get(&(actor_id, target_id)) {
                return Ok(RecordedSwipe {
                    created: false,
                    decision: *existing,
                });
            }
            rows.insert((actor_id, target_id), decision);
            Ok(RecordedSwipe {
                created: true,
                decision,
            })
        })
    }

    fn find(
        &self,
        actor_id: Uuid,
        target_id: Uuid,
    ) -> DiscoveryStoreFuture<'_, Option<SwipeRecord>> {
        Box::pin(async move {
            let rows = self
                .rows
                .lock()
                .map_err(|_| DiscoveryStoreError::InvalidStoredData)?;
            Ok(rows
                .get(&(actor_id, target_id))
                .map(|decision| SwipeRecord {
                    actor_id,
                    target_id,
                    decision: *decision,
                    swiped_at: Utc::now(),
                }))
        })
    }

    fn swiped_target_ids(
        &self,
        actor_id: Uuid,
        target_ids: Vec<Uuid>,
    ) -> DiscoveryStoreFuture<'_, HashSet<Uuid>> {
        Box::pin(async move {
            let rows = self
                .rows
                .lock()
                .map_err(|_| DiscoveryStoreError::InvalidStoredData)?;
            Ok(target_ids
                .into_iter()
                .filter(|target| rows.contains_key(&(actor_id, *target)))
                .collect())
        })
    }
}

#[derive(Clone, Copy)]
struct FakeMatches;

impl MatchCreator for FakeMatches {
    fn create_from_mutual_like(
        &self,
        first_user_id: Uuid,
        second_user_id: Uuid,
    ) -> MatchCreatorFuture<'_> {
        Box::pin(async move {
            let now = Utc::now();
            Ok(PublicMatch {
                id: Uuid::new_v4(),
                user1_id: first_user_id.min(second_user_id),
                user2_id: first_user_id.max(second_user_id),
                status: MatchStatus::Active,
                expires_at: now.to_rfc3339(),
                purge_after: None,
                created_at: now.to_rfc3339(),
                last_message_at: None,
            })
        })
    }
}

#[derive(Clone, Default)]
struct FlakyMatches {
    calls: Arc<AtomicUsize>,
}

impl MatchCreator for FlakyMatches {
    fn create_from_mutual_like(
        &self,
        first_user_id: Uuid,
        second_user_id: Uuid,
    ) -> MatchCreatorFuture<'_> {
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if attempt == 0 {
                return Err(MatchError::Database(DatabaseError::QueryFailed));
            }
            let now = Utc::now();
            Ok(PublicMatch {
                id: Uuid::new_v4(),
                user1_id: first_user_id.min(second_user_id),
                user2_id: first_user_id.max(second_user_id),
                status: MatchStatus::Active,
                expires_at: now.to_rfc3339(),
                purge_after: None,
                created_at: now.to_rfc3339(),
                last_message_at: None,
            })
        })
    }
}

fn legal() -> LegalConfig {
    let url = || Url::parse("https://histae.test/legal").unwrap_or_else(|_| unreachable!());
    LegalConfig {
        terms_version: "terms-v1".to_owned(),
        privacy_version: "privacy-v1".to_owned(),
        sensitive_data_consent_version: "sensitive-v1".to_owned(),
        location_consent_version: "location-v1".to_owned(),
        terms_url: url(),
        privacy_url: url(),
        sensitive_data_consent_url: url(),
        location_consent_url: url(),
        review_reference: "test".to_owned(),
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

fn candidate(id: Uuid, distance_km: f64) -> DiscoveryCandidateRow {
    DiscoveryCandidateRow {
        user_id: id,
        firstname: "Ariane".to_owned(),
        age: 30,
        sex: Sex::Female,
        bio: Some("Bio approuvée".to_owned()),
        distance_km,
        traits: vec!["Curieuse".to_owned()],
        profile_answers: Vec::new(),
    }
}

fn app(
    ready: bool,
    candidates: Vec<DiscoveryCandidateRow>,
    swipes: FakeSwipes,
) -> (axum::Router, String) {
    let tokens = TokenService::new(jwt_config());
    let auth = MobileAuthService::new(
        tokens.clone(),
        Arc::new(AuthStore),
        "terms-v1".to_owned(),
        "privacy-v1".to_owned(),
    );
    let limiter = RateLimiter::memory(&SecretString::new(
        "rate-limit-secret-0123456789abcdef".to_owned(),
    ));
    let policy = LimitPolicy {
        max: 100,
        window: Duration::from_secs(60),
    };
    let auth_state = MobileAuthState::new(auth, limiter.clone(), policy.clone());
    let service = DiscoveryService::new(
        Arc::new(FakeRepository { ready, candidates }),
        Arc::new(swipes),
        Arc::new(FakeMatches),
        legal(),
        FeedCursorCodec::new(&jwt_config().secret, Arc::new(SystemClock)).expect("cursor codec"),
    );
    let discovery_state =
        DiscoveryHttpState::new(service, limiter.clone(), policy.clone(), policy.clone());
    let readiness = Readiness::new(Arc::new(Probe), Arc::new(Probe), Arc::new(Probe));
    let http_state = HttpState::new(
        readiness,
        Environment::Test,
        &TrustProxy::Disabled,
        &[],
        limiter,
        policy,
    )
    .unwrap_or_else(|_| unreachable!());
    let token = tokens
        .access_token(user_id(), session_id())
        .unwrap_or_else(|_| unreachable!());
    (
        build_router(routes(discovery_state, auth_state), http_state),
        token,
    )
}

async fn json_response(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap_or_else(|_| unreachable!());
    serde_json::from_slice(&bytes).unwrap_or_else(|_| unreachable!())
}

#[tokio::test]
async fn requires_mobile_authentication() {
    let (app, _) = app(true, Vec::new(), FakeSwipes::default());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/feed")
                .body(Body::empty())
                .unwrap_or_else(|_| unreachable!()),
        )
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_response(response).await,
        json!({"error":{"code":"authentication_required","message":"A valid Bearer Authorization header is required."}})
    );
}

#[tokio::test]
async fn exposes_status_feed_and_created_swipe_contracts() {
    let target = Uuid::new_v4();
    let (app, token) = app(
        true,
        vec![candidate(target, 1.23456)],
        FakeSwipes::default(),
    );
    let status_response = app
        .clone()
        .oneshot(authenticated(
            "/api/users/me/discovery-status",
            "GET",
            &token,
            None,
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(status_response.status(), StatusCode::OK);
    assert_eq!(json_response(status_response).await["ready"], true);

    let feed_response = app
        .clone()
        .oneshot(authenticated("/api/feed?limit=1", "GET", &token, None))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(feed_response.status(), StatusCode::OK);
    let feed = json_response(feed_response).await;
    assert_eq!(feed["profiles"][0]["user_id"], target.to_string());
    assert_eq!(feed["profiles"][0]["distance_km"], 1.2);

    let swipe_response = app
        .oneshot(authenticated(
            "/api/swipes",
            "POST",
            &token,
            Some(json!({"target_user_id":target,"decision":"like"})),
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(swipe_response.status(), StatusCode::CREATED);
    assert_eq!(
        json_response(swipe_response).await,
        json!({"decision":"like","matched":false})
    );
}

#[tokio::test]
async fn encrypted_feed_cursors_paginate_without_exposing_precise_distances() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let (app, token) = app(
        true,
        vec![candidate(first, 1.23456), candidate(second, 1.23459)],
        FakeSwipes::default(),
    );
    let response = app
        .clone()
        .oneshot(authenticated("/api/feed?limit=1", "GET", &token, None))
        .await
        .expect("first page");
    assert_eq!(response.status(), StatusCode::OK);
    let page = json_response(response).await;
    assert_eq!(page["profiles"][0]["user_id"], first.to_string());
    assert_eq!(page["profiles"][0]["distance_km"], 1.2);
    let cursor = page["next_cursor"].as_str().expect("next cursor");
    let decoded = URL_SAFE_NO_PAD.decode(cursor).expect("base64");
    assert!(serde_json::from_slice::<Value>(&decoded).is_err());

    let response = app
        .clone()
        .oneshot(authenticated(
            &format!("/api/feed?limit=1&cursor={cursor}"),
            "GET",
            &token,
            None,
        ))
        .await
        .expect("second page");
    assert_eq!(response.status(), StatusCode::OK);
    let next = json_response(response).await;
    assert_eq!(next["profiles"][0]["user_id"], second.to_string());
    assert_eq!(next["profiles"][0]["distance_km"], 1.2);
    assert!(next["next_cursor"].is_null());

    let mut modified = decoded;
    *modified.last_mut().expect("tag") ^= 1;
    let response = app
        .oneshot(authenticated(
            &format!("/api/feed?cursor={}", URL_SAFE_NO_PAD.encode(modified)),
            "GET",
            &token,
            None,
        ))
        .await
        .expect("tampered cursor");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_response(response).await["error"]["code"],
        "invalid_cursor"
    );
}

#[tokio::test]
async fn rejects_invalid_payload_missing_candidate_and_immutable_change() {
    let target = Uuid::new_v4();
    let swipes = FakeSwipes::default();
    swipes
        .rows
        .lock()
        .unwrap_or_else(|_| unreachable!())
        .insert((user_id(), target), SwipeDecision::Pass);
    let (app, token) = app(true, vec![candidate(target, 1.0)], swipes);

    let invalid = app
        .clone()
        .oneshot(authenticated(
            "/api/swipes",
            "POST",
            &token,
            Some(json!({"target_user_id":"bad","decision":"super-like"})),
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_response(invalid).await["error"]["code"],
        "invalid_swipe_payload"
    );

    let conflict = app
        .clone()
        .oneshot(authenticated(
            "/api/swipes",
            "POST",
            &token,
            Some(json!({"target_user_id":target,"decision":"like"})),
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_response(conflict).await["error"]["code"],
        "swipe_already_recorded"
    );

    let missing = Uuid::new_v4();
    let missing_response = app
        .oneshot(authenticated(
            "/api/swipes",
            "POST",
            &token,
            Some(json!({"target_user_id":missing,"decision":"like"})),
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(missing_response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_response(missing_response).await["error"]["code"],
        "discovery_candidate_not_found"
    );
}

#[tokio::test]
async fn rejects_discovery_when_prerequisites_are_missing() {
    let (app, token) = app(false, Vec::new(), FakeSwipes::default());
    let status_response = app
        .clone()
        .oneshot(authenticated(
            "/api/users/me/discovery-status",
            "GET",
            &token,
            None,
        ))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(status_response.status(), StatusCode::OK);
    assert_eq!(
        json_response(status_response).await["required_actions"],
        json!([
            "profile",
            "preferences",
            "sensitive_data_consent",
            "location_consent",
            "fresh_presence"
        ])
    );

    let response = app
        .oneshot(authenticated("/api/feed", "GET", &token, None))
        .await
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_response(response).await["error"]["code"],
        "discovery_not_ready"
    );
}

#[tokio::test]
async fn identical_like_replay_recovers_after_match_creation_failure() {
    let target = Uuid::new_v4();
    let swipes = FakeSwipes::default();
    swipes
        .rows
        .lock()
        .unwrap_or_else(|_| unreachable!())
        .insert((target, user_id()), SwipeDecision::Like);
    let matches = FlakyMatches::default();
    let service = DiscoveryService::new(
        Arc::new(FakeRepository {
            ready: true,
            candidates: vec![candidate(target, 1.0)],
        }),
        Arc::new(swipes.clone()),
        Arc::new(matches.clone()),
        legal(),
        FeedCursorCodec::new(&jwt_config().secret, Arc::new(SystemClock)).expect("cursor codec"),
    );

    let first = service.swipe(user_id(), target, SwipeDecision::Like).await;
    assert!(matches!(
        first,
        Err(DiscoveryError::Match(MatchError::Database(
            DatabaseError::QueryFailed
        )))
    ));
    assert_eq!(
        swipes
            .rows
            .lock()
            .unwrap_or_else(|_| unreachable!())
            .get(&(user_id(), target)),
        Some(&SwipeDecision::Like)
    );

    let replay = service
        .swipe(user_id(), target, SwipeDecision::Like)
        .await
        .unwrap_or_else(|_| unreachable!());
    assert!(replay.matched);
    assert!(replay.r#match.is_some());
    assert_eq!(matches.calls.load(Ordering::SeqCst), 2);
}

fn authenticated(uri: &str, method: &str, token: &str, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap_or_else(|_| unreachable!())
}
