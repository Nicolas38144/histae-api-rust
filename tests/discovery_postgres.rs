#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use histae_api_rust::config::{LegalConfig, PostgresConfig, SecretString};
use histae_api_rust::discovery::domain::{SWIPE_RETENTION_DAYS, SwipeDecision};
use histae_api_rust::discovery::pg::{PgDiscoveryRepository, PgSwipeStore};
use histae_api_rust::discovery::service::DiscoveryService;
use histae_api_rust::discovery::store::SwipeStore;
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::infra::postgres_locks::AccountActivityPool;
use histae_api_rust::matches::pg::{PgMatchMessageRepository, PgMatchRepository};
use histae_api_rust::matches::service::{MatchService, NoopMatchEventPublisher};
use histae_api_rust::profiles::service::{ProfilePhotoUrlFuture, ProfilePhotoUrlProvider};
use histae_api_rust::shared::clock::SystemClock;
use url::Url;
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "S19 integration fixture error ({})", self.0)
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
        max_connections: 8,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        statement_timeout: Duration::from_secs(15),
        idle_transaction_timeout: Duration::from_secs(30),
        application_name: "histae-rust-s19-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS").ok().map(PathBuf::from),
    })
}

fn legal() -> LegalConfig {
    let url = || Url::parse("https://histae.test/legal").unwrap_or_else(|_| unreachable!());
    LegalConfig {
        terms_version: "terms-v1".to_owned(),
        privacy_version: "privacy-v1".to_owned(),
        sensitive_data_consent_version: "s19-v1".to_owned(),
        location_consent_version: "s19-v1".to_owned(),
        terms_url: url(),
        privacy_url: url(),
        sensitive_data_consent_url: url(),
        location_consent_url: url(),
        review_reference: "s19-test".to_owned(),
    }
}

#[derive(Clone, Copy)]
struct NoPhotoUrls;

impl ProfilePhotoUrlProvider for NoPhotoUrls {
    fn url_for_key(&self, _object_key: Option<String>) -> ProfilePhotoUrlFuture<'_> {
        Box::pin(async { Ok(None) })
    }
}

async fn ready_user(
    database: &Database,
    user_id: Uuid,
    ordinal: i32,
    latitude: f64,
) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO user_account
         (user_id, role, phone_number_hash, phone_number_encrypted)
         VALUES ($1, 'user', $2, $3)",
    )
    .bind(user_id)
    .bind(format!("s19-{user_id}"))
    .bind(Vec::<u8>::new())
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    let sex = if ordinal % 2 == 0 { "female" } else { "male" };
    sqlx::query(
        "INSERT INTO user_profile (user_id, firstname, birthdate, sex, bio)
         VALUES ($1, $2, DATE '1990-01-01', $3, 'Approved S19 biography')",
    )
    .bind(user_id)
    .bind(format!("S19 {ordinal}"))
    .bind(sex)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        "INSERT INTO user_preferences (user_id, min_age, max_age, max_distance_km, looking_for)
         VALUES ($1, 18, 99, 500, 'both')",
    )
    .bind(user_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        "INSERT INTO user_presence (user_id, latitude, longitude, is_location_fresh, updated_at)
         VALUES ($1, $2, 2.3522, true, clock_timestamp())",
    )
    .bind(user_id)
    .bind(latitude)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    for consent_type in ["sensitive_data_consent", "location_consent"] {
        sqlx::query(
            "INSERT INTO user_consent (user_id, consent_type, granted, document_version)
             VALUES ($1, $2, true, 's19-v1')",
        )
        .bind(user_id)
        .bind(consent_type)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    }
    Ok(())
}

async fn cleanup(database: &Database, users: &[Uuid]) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM user_account WHERE user_id = ANY($1::uuid[])")
        .bind(users)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

async fn services(
    config: &PostgresConfig,
) -> Result<
    (
        Database,
        AccountActivityPool,
        DiscoveryService,
        PgSwipeStore,
    ),
    Box<dyn std::error::Error>,
> {
    let database = Database::connect(config).await?;
    let activity = AccountActivityPool::connect(config).await?;
    let swipes = PgSwipeStore::new(database.clone(), activity.clone());
    let matches = MatchService::new(
        Arc::new(PgMatchRepository::new(database.clone())),
        Arc::new(PgMatchMessageRepository::new(database.clone())),
        Arc::new(NoPhotoUrls),
        Arc::new(NoopMatchEventPublisher),
        Arc::new(SystemClock),
    );
    let service = DiscoveryService::new(
        Arc::new(PgDiscoveryRepository::new(database.clone())),
        Arc::new(swipes.clone()),
        Arc::new(matches),
        legal(),
    );
    Ok((database, activity, service, swipes))
}

#[tokio::test]
async fn feed_excludes_current_swipes_and_expired_decisions_can_be_replaced()
-> Result<(), Box<dyn std::error::Error>> {
    let config = postgres_config()?;
    let (database, activity, service, swipes) = services(&config).await?;
    let viewer = Uuid::new_v4();
    let current = Uuid::new_v4();
    let expired = Uuid::new_v4();
    let distant = Uuid::new_v4();
    let missing_consent = Uuid::new_v4();
    let too_old = Uuid::new_v4();
    let users = [viewer, current, expired, distant, missing_consent, too_old];
    cleanup(&database, &users).await?;
    ready_user(&database, viewer, 1, 48.8566).await?;
    ready_user(&database, current, 2, 48.8576).await?;
    ready_user(&database, expired, 4, 48.8586).await?;
    ready_user(&database, distant, 6, 50.0).await?;
    ready_user(&database, missing_consent, 8, 48.8596).await?;
    ready_user(&database, too_old, 10, 48.8606).await?;
    let mut connection = database.acquire().await?;
    sqlx::query(
        "UPDATE user_preferences SET max_distance_km = 10, max_age = 40 WHERE user_id = $1",
    )
    .bind(viewer)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        "DELETE FROM user_consent WHERE user_id = $1 AND consent_type = 'location_consent'",
    )
    .bind(missing_consent)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query("UPDATE user_profile SET birthdate = DATE '1950-01-01' WHERE user_id = $1")
        .bind(too_old)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    drop(connection);

    let test_result: Result<(), Box<dyn std::error::Error>> = async {
        service.swipe(viewer, current, SwipeDecision::Pass).await?;
        let old = Utc::now() - TimeDelta::days(SWIPE_RETENTION_DAYS + 1);
        let mut connection = database.acquire().await?;
        sqlx::query(
            "INSERT INTO swipe_decision (actor_id, target_id, decision, swiped_at, expires_at)
             VALUES ($1, $2, 'pass', $3, $4)",
        )
        .bind(viewer)
        .bind(expired)
        .bind(old)
        .bind(old + TimeDelta::days(SWIPE_RETENTION_DAYS))
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
        drop(connection);

        let feed = service.feed(viewer, 100, None).await?;
        assert!(
            !feed
                .profiles
                .iter()
                .any(|candidate| candidate.user_id == current)
        );
        assert!(
            feed.profiles
                .iter()
                .any(|candidate| candidate.user_id == expired)
        );
        assert!(
            !feed
                .profiles
                .iter()
                .any(|candidate| candidate.user_id == distant)
        );
        assert!(
            !feed
                .profiles
                .iter()
                .any(|candidate| candidate.user_id == missing_consent)
        );
        assert!(
            !feed
                .profiles
                .iter()
                .any(|candidate| candidate.user_id == too_old)
        );

        let replaced = swipes.record(viewer, expired, SwipeDecision::Like).await?;
        assert!(replaced.created);
        assert_eq!(replaced.decision, SwipeDecision::Like);
        let mut connection = database.acquire().await?;
        let row: (String, i64) = sqlx::query_as(
            "SELECT decision,
               extract(epoch FROM (expires_at - swiped_at))::bigint AS retention_seconds
             FROM swipe_decision WHERE actor_id = $1 AND target_id = $2",
        )
        .bind(viewer)
        .bind(expired)
        .fetch_one(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
        assert_eq!(row.0, "like");
        assert_eq!(row.1, SWIPE_RETENTION_DAYS * 86_400);
        Ok(())
    }
    .await;

    cleanup(&database, &users).await?;
    activity.close().await;
    database.close().await;
    test_result
}

#[tokio::test]
async fn simultaneous_mutual_likes_create_one_match_and_keep_decisions_immutable()
-> Result<(), Box<dyn std::error::Error>> {
    let config = postgres_config()?;
    let (database, activity, service, _swipes) = services(&config).await?;
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let users = [first, second];
    cleanup(&database, &users).await?;
    ready_user(&database, first, 1, 48.8566).await?;
    ready_user(&database, second, 2, 48.8576).await?;

    let test_result: Result<(), Box<dyn std::error::Error>> = async {
        let (left, right) = tokio::join!(
            service.swipe(first, second, SwipeDecision::Like),
            service.swipe(second, first, SwipeDecision::Like),
        );
        let outcomes = [left?, right?];
        assert!(outcomes.iter().any(|outcome| outcome.matched));
        let mut connection = database.acquire().await?;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM match_init
             WHERE (user1_id = $1 AND user2_id = $2) OR (user1_id = $2 AND user2_id = $1)",
        )
        .bind(first)
        .bind(second)
        .fetch_one(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
        assert_eq!(count, 1);
        let decisions: Vec<String> = sqlx::query_scalar(
            "SELECT decision FROM swipe_decision
             WHERE (actor_id = $1 AND target_id = $2) OR (actor_id = $2 AND target_id = $1)
             ORDER BY actor_id",
        )
        .bind(first)
        .bind(second)
        .fetch_all(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
        assert_eq!(decisions, vec!["like", "like"]);
        Ok(())
    }
    .await;

    cleanup(&database, &users).await?;
    activity.close().await;
    database.close().await;
    test_result
}
