#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use histae_api_rust::administration::photos::{
    AdminPhotoStore, PgAdminPhotoRepository, ReconciliationResult,
};
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::identity::admin_role::AdminRole;
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::moderation::domain::{
    ModerationDecision, ModerationReviewInput, ModerationReviewResult, PhotoReviewChecks,
};
use histae_api_rust::moderation::pg::PgModerationRepository;
use histae_api_rust::moderation::store::ModerationStore;
use histae_api_rust::outbox::pg::PgOutboxRepository;
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "S16 integration fixture error ({})", self.0)
    }
}

impl std::error::Error for FixtureError {}

fn variable(name: &'static str) -> Result<String, FixtureError> {
    let _ = dotenvy::dotenv();
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(FixtureError(name))
}

fn postgres_config() -> Result<PostgresConfig, FixtureError> {
    if variable("ENV")? != "development" {
        return Err(FixtureError("ENV"));
    }
    let host = variable("POSTGRES_HOST")?;
    if !matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1") {
        return Err(FixtureError("POSTGRES_HOST"));
    }
    let database = variable("POSTGRES_DB")?;
    if database != "histae-dev" {
        return Err(FixtureError("POSTGRES_DB"));
    }
    Ok(PostgresConfig {
        host,
        port: variable("POSTGRES_PORT")?
            .parse()
            .map_err(|_| FixtureError("POSTGRES_PORT"))?,
        user: variable("POSTGRES_USER")?,
        password: SecretString::new(variable("POSTGRES_PASSWORD")?),
        database,
        tls: false,
        max_connections: 4,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        statement_timeout: Duration::from_secs(15),
        idle_transaction_timeout: Duration::from_secs(30),
        application_name: "histae-rust-s16-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS").ok().map(PathBuf::from),
    })
}

async fn account(database: &Database, user_id: Uuid, role: &str) -> Result<(), DatabaseError> {
    sqlx::query(
        "INSERT INTO user_account
         (user_id, role, phone_number_hash, phone_number_encrypted)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(role)
    .bind(format!("s16-{user_id}"))
    .bind(Vec::<u8>::new())
    .execute(database.acquire().await?.as_mut())
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

async fn cleanup(database: &Database, ids: &[Uuid]) -> Result<(), DatabaseError> {
    sqlx::query("DELETE FROM outbox_event WHERE aggregate_id = ANY($1::uuid[])")
        .bind(ids)
        .execute(database.acquire().await?.as_mut())
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query(
        "DELETE FROM data_access_log
         WHERE accessed_user_id = ANY($1::uuid[]) OR accessor_id = ANY($1::uuid[])",
    )
    .bind(ids)
    .execute(database.acquire().await?.as_mut())
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM user_account WHERE user_id = ANY($1::uuid[])")
        .bind(ids)
        .execute(database.acquire().await?.as_mut())
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

#[cfg(feature = "webauthn-probe")]
#[tokio::test]
async fn moderation_and_photo_routes_accept_older_sessions_but_keep_origin_and_cookie_checks()
-> Result<(), Box<dyn std::error::Error>> {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use histae_api_rust::{
        administration::photos::{
            AdminPhotoService,
            http::{self as photos_http, AdminPhotoHttpState},
        },
        config::{AdminAuthConfig, LimitPolicy},
        http::rate_limit::RateLimiter,
        identity::admin::{
            http::{self as auth_http, AdminAuthHttpState},
            pg::AdminAuthRepository,
            service::AdminAuthService,
        },
        moderation::{
            http::{self as moderation_http, ModerationHttpState},
            service::{ModerationPhotoUrlProvider, ModerationService, PhotoUrlFuture},
        },
        shared::clock::SystemClock,
    };
    use sha2::{Digest as _, Sha256};
    use std::sync::Arc;
    use tower::ServiceExt as _;
    struct NoPhoto;
    impl ModerationPhotoUrlProvider for NoPhoto {
        fn url_for_key(&self, _: Option<String>) -> PhotoUrlFuture<'_> {
            Box::pin(async { Ok(None) })
        }
    }
    let database = Database::connect(&postgres_config()?).await?;
    let admin_id = Uuid::new_v4();
    account(&database, admin_id, "admin").await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let credential_id = Uuid::new_v4();
        sqlx::query("INSERT INTO admin_webauthn_credential (id,user_id,credential_id,public_key,device_type,backed_up,name) VALUES ($1,$2,$3,$4,'singleDevice',false,'Fixture')")
            .bind(credential_id).bind(admin_id).bind(Uuid::new_v4().simple().to_string()).bind(vec![1_u8])
            .execute(database.acquire().await?.as_mut()).await?;
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|_| FixtureError("random token"))?;
        let token = URL_SAFE_NO_PAD.encode(secret);
        sqlx::query("INSERT INTO admin_session (user_id,credential_id,token_hash,authenticated_at,idle_expires_at,absolute_expires_at) VALUES ($1,$2,$3,now()-interval '20 minutes',now()+interval '30 minutes',now()+interval '8 hours')")
            .bind(admin_id).bind(credential_id).bind(Sha256::digest(token.as_bytes()).to_vec())
            .execute(database.acquire().await?.as_mut()).await?;
        let config = AdminAuthConfig {
            rp_id: "localhost".into(), origin: "http://localhost:5173".into(), rp_name: "Fixture".into(),
            challenge_ttl: Duration::from_secs(300), bootstrap_ttl: Duration::from_secs(900),
            session_idle_ttl: Duration::from_secs(1800), session_absolute_ttl: Duration::from_secs(28800),
            recent_authentication_ttl: Duration::from_secs(600), cookie_name: "histae_admin_session", secure_cookie: false,
        };
        let service = AdminAuthService::new(Arc::new(AdminAuthRepository::new(database.clone())), config.clone())?;
        let auth = AdminAuthHttpState::new(service, RateLimiter::memory(&SecretString::new("test-only-rate-limit-key".into())), LimitPolicy { max: 100, window: Duration::from_secs(60) }, config);
        let outbox = PgOutboxRepository::new(database.clone());
        let routes = moderation_http::routes(ModerationHttpState::new(ModerationService::new(Arc::new(PgModerationRepository::new(database.clone(), outbox.clone())), Arc::new(NoPhoto))), auth.clone())
            .merge(photos_http::routes(AdminPhotoHttpState::new(AdminPhotoService::new(Arc::new(PgAdminPhotoRepository::new(database.clone(), outbox)), Arc::new(SystemClock))), auth.clone()))
            .merge(auth_http::routes(auth));
        // Use inert health probes to focus on the real guards and session lookup.
        let routes: axum::Router = routes.with_state::<()>(http_fixture_state()?);
        let cases = [
            ("PATCH", format!("/api/admin/content-moderation/{}", Uuid::new_v4()), r#"{"version":1,"decision":"approved","reason":"Manual verification"}"#, 404, "moderation_case_not_found"),
            ("POST", format!("/api/admin/photo-reconciliation/{}/retry", Uuid::new_v4()), r#"{"reason":"Manual verification"}"#, 404, "photo_not_found"),
            ("POST", "/api/admin/auth/credentials/options".into(), "", 401, "admin_reauthentication_required"),
        ];
        for (method, path, body, status, code) in cases {
            let request = |origin: &str, cookie: bool| {
                let mut builder = Request::builder().method(method).uri(&path).header("origin", origin);
                if cookie { builder = builder.header("cookie", format!("histae_admin_session={token}")); }
                if !body.is_empty() { builder = builder.header("content-type", "application/json"); }
                builder.body(Body::from(body)).expect("request")
            };
            let response = routes.clone().oneshot(request("http://localhost:5173", true)).await?;
            assert_eq!(response.status().as_u16(), status);
            let json: serde_json::Value = serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
            assert_eq!(json["error"]["code"], code);
            assert_eq!(routes.clone().oneshot(request("http://127.0.0.1:5173", true)).await?.status().as_u16(), 403);
            assert_eq!(routes.clone().oneshot(request("http://localhost:5173", false)).await?.status().as_u16(), 401);
        }
        Ok(())
    }.await;
    cleanup(&database, &[admin_id]).await?;
    database.close().await;
    result
}

#[cfg(feature = "webauthn-probe")]
fn http_fixture_state() -> Result<histae_api_rust::http::HttpState, FixtureError> {
    use histae_api_rust::{
        config::{Environment, LimitPolicy, TrustProxy},
        http::{
            HttpState,
            health::{DependencyProbe, ProbeFuture, Readiness},
            rate_limit::RateLimiter,
        },
    };
    use std::sync::Arc;
    struct Probe;
    impl DependencyProbe for Probe {
        fn check(&self) -> ProbeFuture<'_> {
            Box::pin(async { Ok(()) })
        }
    }
    HttpState::new(
        Readiness::new(Arc::new(Probe), Arc::new(Probe), Arc::new(Probe)),
        Environment::Test,
        &TrustProxy::Disabled,
        &[],
        RateLimiter::memory(&SecretString::new("test-only-rate-limit-key".into())),
        LimitPolicy {
            max: 100,
            window: Duration::from_secs(60),
        },
    )
    .map_err(|_| FixtureError("http state"))
}

#[tokio::test]
async fn review_and_reconciliation_preserve_locks_audits_and_outbox_atomicity()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let outbox = PgOutboxRepository::new(database.clone());
    let moderation = PgModerationRepository::new(database.clone(), outbox.clone());
    let admin_photos = PgAdminPhotoRepository::new(database.clone(), outbox);
    let user_id = Uuid::new_v4();
    let admin_id = Uuid::new_v4();
    let ready_photo = Uuid::new_v4();
    let case_id = Uuid::new_v4();
    let stale_photo = Uuid::new_v4();
    let active_photo = Uuid::new_v4();
    let ids = [
        user_id,
        admin_id,
        ready_photo,
        case_id,
        stale_photo,
        active_photo,
    ];
    cleanup(&database, &ids).await?;
    account(&database, user_id, "user").await?;
    account(&database, admin_id, "admin").await?;
    sqlx::query(
        "INSERT INTO user_profile (user_id, firstname, birthdate)
         VALUES ($1, 'Alice', '1990-01-01')",
    )
    .bind(user_id)
    .execute(database.acquire().await?.as_mut())
    .await?;
    sqlx::query(
        "INSERT INTO user_photo
         (id, user_id, object_key, status, mime_type, size_bytes, width, height, sha256)
         VALUES ($1, $2, $3, 'ready', 'image/webp', 128, 64, 64, $4)",
    )
    .bind(ready_photo)
    .bind(user_id)
    .bind(format!("profile-photos/{user_id}/{ready_photo}.webp"))
    .bind(vec![7_u8; 32])
    .execute(database.acquire().await?.as_mut())
    .await?;
    sqlx::query(
        "INSERT INTO content_moderation_case
         (id, user_id, content_type, photo_id, status, reason_codes, policy_version,
          face_count, sharpness_score, nsfw_score)
         VALUES ($1, $2, 'photo', $3, 'pending', ARRAY['blurry']::text[],
                 'local_vision_v1', 1, 40, 0.1)",
    )
    .bind(case_id)
    .bind(user_id)
    .bind(ready_photo)
    .execute(database.acquire().await?.as_mut())
    .await?;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        assert!(
            moderation
                .detail(
                    case_id,
                    admin_id,
                    AdminRole::Admin,
                    "Contrôle manuel".to_owned(),
                )
                .await
                .map_err(|_| FixtureError("moderation_detail"))?
                .is_some()
        );
        let view_audit: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM data_access_log
             WHERE accessed_user_id = $1 AND accessor_id = $2
               AND action = 'view_moderation_content'",
        )
        .bind(user_id)
        .bind(admin_id)
        .fetch_one(database.acquire().await?.as_mut())
        .await?;
        assert_eq!(view_audit, 1);

        let stale = moderation
            .review(
                case_id,
                ModerationReviewInput {
                    version: 2,
                    decision: ModerationDecision::Rejected,
                    reason: "Photo floue".to_owned(),
                    photo_checks: Some(PhotoReviewChecks {
                        face_detectable: true,
                        sharp_enough: false,
                        content_allowed: true,
                    }),
                },
                admin_id,
                AdminRole::Admin,
            )
            .await
            .map_err(|_| FixtureError("stale_review"))?;
        assert_eq!(stale, ModerationReviewResult::Stale);

        let reviewed = moderation
            .review(
                case_id,
                ModerationReviewInput {
                    version: 1,
                    decision: ModerationDecision::Rejected,
                    reason: "Photo floue".to_owned(),
                    photo_checks: Some(PhotoReviewChecks {
                        face_detectable: true,
                        sharp_enough: false,
                        content_allowed: true,
                    }),
                },
                admin_id,
                AdminRole::Admin,
            )
            .await
            .map_err(|_| FixtureError("photo_review"))?;
        assert_eq!(reviewed, ModerationReviewResult::Updated);
        let state: (String, i32, String, String) = sqlx::query_as(
            "SELECT moderation.status, moderation.version, photo.status, event.status
             FROM content_moderation_case AS moderation
             JOIN user_photo AS photo ON photo.id = moderation.photo_id
             JOIN outbox_event AS event ON event.aggregate_id = photo.id
                AND event.event_type = 'photo.delete'
             WHERE moderation.id = $1",
        )
        .bind(case_id)
        .fetch_one(database.acquire().await?.as_mut())
        .await?;
        assert_eq!(
            state,
            (
                "rejected".to_owned(),
                2,
                "deleting".to_owned(),
                "pending".to_owned()
            )
        );

        let old = Utc::now() - TimeDelta::hours(1);
        sqlx::query(
            "INSERT INTO user_photo (id, user_id, object_key, status, updated_at)
             VALUES ($1, $2, $3, 'processing', $4)",
        )
        .bind(stale_photo)
        .bind(user_id)
        .bind(format!("profile-photos/{user_id}/{stale_photo}.webp"))
        .bind(old)
        .execute(database.acquire().await?.as_mut())
        .await?;
        assert_eq!(
            admin_photos
                .reconcile(
                    stale_photo,
                    Utc::now() - TimeDelta::minutes(30),
                    Utc::now() - TimeDelta::minutes(5),
                    admin_id,
                    AdminRole::Admin,
                    "Traitement ancien".to_owned(),
                )
                .await
                .map_err(|_| FixtureError("stale_reconcile"))?,
            ReconciliationResult::Queued
        );
        sqlx::query(
            "INSERT INTO user_photo (id, user_id, object_key, status, updated_at)
             VALUES ($1, $2, $3, 'processing', $4)",
        )
        .bind(active_photo)
        .bind(user_id)
        .bind(format!("profile-photos/{user_id}/{active_photo}.webp"))
        .bind(old)
        .execute(database.acquire().await?.as_mut())
        .await?;
        sqlx::query(
            "INSERT INTO outbox_event
             (id, event_type, aggregate_id, status, locked_at, locked_by)
             VALUES ($1, 'photo.delete', $2, 'processing', clock_timestamp(), $3)",
        )
        .bind(Uuid::new_v4())
        .bind(active_photo)
        .bind(Uuid::new_v4())
        .execute(database.acquire().await?.as_mut())
        .await?;
        sqlx::query("UPDATE user_photo SET status = 'deleting' WHERE id = $1")
            .bind(active_photo)
            .execute(database.acquire().await?.as_mut())
            .await?;
        assert_eq!(
            admin_photos
                .reconcile(
                    active_photo,
                    Utc::now() - TimeDelta::minutes(30),
                    Utc::now() - TimeDelta::minutes(5),
                    admin_id,
                    AdminRole::Admin,
                    "Worker actif".to_owned(),
                )
                .await
                .map_err(|_| FixtureError("active_reconcile"))?,
            ReconciliationResult::AlreadyProcessing
        );
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, &ids).await;
    database.close().await;
    result?;
    cleanup_result?;
    Ok(())
}
