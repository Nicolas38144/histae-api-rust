#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use histae_api_rust::config::{LegalConfig, PostgresConfig, SecretString};
use histae_api_rust::discovery::cursor::FeedCursorCodec;
use histae_api_rust::discovery::domain::{DiscoveryCursor, SWIPE_RETENTION_DAYS, SwipeDecision};
use histae_api_rust::discovery::pg::{PgDiscoveryRepository, PgSwipeStore};
use histae_api_rust::discovery::service::DiscoveryService;
use histae_api_rust::discovery::store::{DiscoveryRepository, SwipeStore};
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::infra::postgres_locks::AccountActivityPool;
use histae_api_rust::matches::pg::{PgMatchMessageRepository, PgMatchRepository};
use histae_api_rust::matches::service::{MatchService, NoopMatchEventPublisher};
use histae_api_rust::privacy::maintenance::PrivacyMaintenanceRepository;
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
        document_dates: Default::default(),
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
    let photo_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO user_photo
         (id, user_id, object_key, status, mime_type, size_bytes, width, height, sha256)
         VALUES ($1,$2,$3,'ready','image/webp',128,32,32,$4)",
    )
    .bind(photo_id)
    .bind(user_id)
    .bind(format!("profile-photos/{user_id}/{photo_id}.webp"))
    .bind(vec![7_u8; 32])
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        "INSERT INTO content_moderation_case
         (id,user_id,content_type,photo_id,status,policy_version)
         VALUES ($1,$2,'photo',$3,'approved','photo-discovery-test')",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(photo_id)
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

async fn swipe_timestamps(
    database: &Database,
    actor_id: Uuid,
    target_id: Uuid,
) -> Result<(DateTime<Utc>, DateTime<Utc>), DatabaseError> {
    sqlx::query_as(
        "SELECT swiped_at, expires_at FROM swipe_decision
         WHERE actor_id=$1 AND target_id=$2",
    )
    .bind(actor_id)
    .bind(target_id)
    .fetch_one(database.acquire().await?.as_mut())
    .await
    .map_err(map_sqlx_error)
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
        FeedCursorCodec::new(
            &SecretString::new("test-cursor-secret-0123456789abcdef".to_owned()),
            Arc::new(SystemClock),
        )?,
    );
    Ok((database, activity, service, swipes))
}

#[tokio::test]
async fn a_like_replay_returns_the_existing_live_match_without_reopening_an_ended_one()
-> Result<(), Box<dyn std::error::Error>> {
    use histae_api_rust::discovery::service::DiscoveryError;
    use histae_api_rust::matches::store::MatchStore;

    let (database, activity, service, _) = services(&postgres_config()?).await?;
    let users = [Uuid::new_v4(), Uuid::new_v4()];
    ready_user(&database, users[0], 1, 48.8566).await?;
    ready_user(&database, users[1], 2, 48.8576).await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        assert!(
            !service
                .swipe(users[0], users[1], SwipeDecision::Like)
                .await?
                .matched
        );
        let accepted = service
            .swipe(users[1], users[0], SwipeDecision::Like)
            .await?;
        let match_id = accepted.r#match.ok_or(FixtureError("mutual_match"))?.id;
        let replay = service
            .swipe(users[1], users[0], SwipeDecision::Like)
            .await?;
        assert!(replay.matched);
        assert_eq!(
            replay.r#match.ok_or(FixtureError("replayed_match"))?.id,
            match_id
        );
        assert_eq!(
            service
                .swipe(users[1], users[0], SwipeDecision::Pass)
                .await
                .unwrap_err(),
            DiscoveryError::SwipeAlreadyRecorded
        );
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM notification WHERE payload->>'match_id'=$1")
                .bind(match_id.to_string())
                .fetch_one(database.acquire().await?.as_mut())
                .await?;
        assert_eq!(count, 2, "replays must not duplicate notifications");
        assert!(service.feed(users[0], 100, None).await?.profiles.is_empty());
        // A replay does not bypass the current discovery filters.
        sqlx::query("UPDATE user_presence SET latitude=-48.8566 WHERE user_id=$1")
            .bind(users[0])
            .execute(database.acquire().await?.as_mut())
            .await?;
        assert_eq!(
            service
                .swipe(users[1], users[0], SwipeDecision::Like)
                .await
                .unwrap_err(),
            DiscoveryError::CandidateNotFound
        );
        sqlx::query("UPDATE user_presence SET latitude=48.8566 WHERE user_id=$1")
            .bind(users[0])
            .execute(database.acquire().await?.as_mut())
            .await?;
        PgMatchRepository::new(database.clone())
            .record_end(match_id, users[0])
            .await?;
        assert_eq!(
            service
                .swipe(users[1], users[0], SwipeDecision::Like)
                .await
                .unwrap_err(),
            DiscoveryError::CandidateNotFound
        );
        Ok(())
    }
    .await;
    let cleaned = cleanup(&database, &users).await;
    activity.close().await;
    database.close().await;
    result?;
    cleaned?;
    Ok(())
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
async fn thirty_day_swipes_preserve_replays_and_require_fresh_mutual_likes()
-> Result<(), Box<dyn std::error::Error>> {
    use histae_api_rust::discovery::service::DiscoveryError;

    let (database, activity, service, swipes) = services(&postgres_config()?).await?;
    let [viewer, old_like, old_pass, recent_pass] = [
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    ];
    let users = [viewer, old_like, old_pass, recent_pass];
    for (user, ordinal) in [(viewer, 1), (old_like, 2), (old_pass, 4), (recent_pass, 6)] {
        ready_user(&database, user, ordinal, 48.8566).await?;
    }
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let mut connection = database.acquire().await?;
        for (target, decision) in [(old_like, "like"), (old_pass, "pass")] {
            sqlx::query(
                "INSERT INTO swipe_decision(actor_id,target_id,decision,swiped_at,expires_at)
                 VALUES ($1,$2,$3,now()-INTERVAL '31 days',now()-INTERVAL '1 day')",
            )
            .bind(viewer)
            .bind(target)
            .bind(decision)
            .execute(&mut *connection)
            .await?;
        }
        drop(connection);
        service
            .swipe(viewer, recent_pass, SwipeDecision::Pass)
            .await?;
        let original = swipe_timestamps(&database, viewer, recent_pass).await?;
        assert_eq!(original.1 - original.0, TimeDelta::days(30));
        service
            .swipe(viewer, recent_pass, SwipeDecision::Pass)
            .await?;
        assert_eq!(
            service
                .swipe(viewer, recent_pass, SwipeDecision::Like)
                .await
                .unwrap_err(),
            DiscoveryError::SwipeAlreadyRecorded,
        );
        let replayed = swipe_timestamps(&database, viewer, recent_pass).await?;
        assert_eq!(replayed, original);
        for target in [old_like, old_pass] {
            assert!(swipes.find(viewer, target).await?.is_none());
        }
        let feed = service.feed(viewer, 100, None).await?;
        let visible = |id| feed.profiles.iter().any(|profile| profile.user_id == id);
        assert!(visible(old_like) && visible(old_pass));
        assert!(!visible(recent_pass));

        // The expired reciprocal like must not create a match.
        let unmatched = service.swipe(old_like, viewer, SwipeDecision::Like).await?;
        assert!(!unmatched.matched && unmatched.r#match.is_none());
        let matched = service.swipe(viewer, old_like, SwipeDecision::Like).await?;
        assert!(matched.matched);
        let match_id = matched
            .r#match
            .ok_or(FixtureError("renewed_mutual_like"))?
            .id;

        let purged = PrivacyMaintenanceRepository::new(database.clone())
            .run_as_leader(Utc::now(), 10)
            .await?
            .ok_or(FixtureError("privacy_leader"))?;
        assert!(purged.expired_swipes >= 1);
        let mut connection = database.acquire().await?;
        let choices: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT target_id,extract(epoch FROM expires_at-swiped_at)::bigint
             FROM swipe_decision WHERE actor_id=$1 ORDER BY target_id",
        )
        .bind(viewer)
        .fetch_all(&mut *connection)
        .await?;
        assert_eq!(choices.len(), 2);
        assert!(choices.contains(&(old_like, 30 * 86_400)));
        assert!(choices.contains(&(recent_pass, 30 * 86_400)));
        let status: String = sqlx::query_scalar("SELECT status FROM match_init WHERE id=$1")
            .bind(match_id)
            .fetch_one(&mut *connection)
            .await?;
        assert_eq!(status, "active");
        drop(connection);
        let feed = service.feed(viewer, 100, None).await?;
        assert_eq!(
            feed.profiles
                .iter()
                .map(|profile| profile.user_id)
                .collect::<Vec<_>>(),
            vec![old_pass]
        );
        Ok(())
    }
    .await;
    let cleaned = cleanup(&database, &users).await;
    activity.close().await;
    database.close().await;
    result?;
    cleaned?;
    Ok(())
}

#[tokio::test]
async fn dense_feed_scans_past_rejections_and_preserves_distance_uuid_pagination()
-> Result<(), Box<dyn std::error::Error>> {
    let config = postgres_config()?;
    let database = Database::connect(&config).await?;
    let repository = PgDiscoveryRepository::new(database.clone());
    let viewer = Uuid::new_v4();
    let rejected: Vec<_> = (0..128).map(|_| Uuid::new_v4()).collect();
    let mut eligible: Vec<_> = (0..6).map(|_| Uuid::new_v4()).collect();
    eligible.sort();
    let users: Vec<_> = std::iter::once(viewer)
        .chain(rejected.iter().copied())
        .chain(eligible.iter().copied())
        .collect();

    let result: Result<(), Box<dyn std::error::Error>> = async {
        // A separate latitude isolates this fixture from existing Paris seed data.
        ready_user(&database, viewer, 1, -40.0).await?;
        let mut connection = database.acquire().await?;
        sqlx::query("UPDATE user_preferences SET max_distance_km = 10, max_age = 40, looking_for = 'female' WHERE user_id = $1")
            .bind(viewer).execute(&mut *connection).await?;
        drop(connection);
        for (index, &user) in rejected.iter().enumerate() {
            ready_user(&database, user, 2, -40.0 + (index + 1) as f64 * 0.0001).await?;
            let mut connection = database.acquire().await?;
            let statement = match index % 17 {
                0 => "UPDATE user_consent SET withdrawn_at = now() WHERE user_id = $1 AND consent_type = 'location_consent'",
                1 => "UPDATE user_consent SET granted = false WHERE user_id = $1 AND consent_type = 'sensitive_data_consent'",
                2 => "UPDATE user_consent SET document_version = 'old' WHERE user_id = $1 AND consent_type = 'sensitive_data_consent'",
                3 => "DELETE FROM user_consent WHERE user_id = $1 AND consent_type = 'location_consent'",
                4 => "UPDATE user_account SET is_banned = true WHERE user_id = $1",
                5 => "UPDATE user_account SET deleted_at = now() WHERE user_id = $1",
                6 => "UPDATE user_presence SET is_location_fresh = false WHERE user_id = $1",
                7 => "UPDATE user_presence SET updated_at = now() - INTERVAL '2 hours' WHERE user_id = $1",
                8 => "UPDATE user_profile SET birthdate = DATE '1950-01-01' WHERE user_id = $1",
                9 => "UPDATE user_preferences SET looking_for = 'female' WHERE user_id = $1",
                10 => {
                    sqlx::query("UPDATE user_presence SET latitude = -39.975 WHERE user_id = $1")
                        .bind(user).execute(&mut *connection).await?;
                    "UPDATE user_preferences SET max_distance_km = 1 WHERE user_id = $1"
                }
                11 => "INSERT INTO user_block (blocker_id, blocked_id) VALUES ($2, $1)",
                12 => "INSERT INTO user_block (blocker_id, blocked_id) VALUES ($1, $2)",
                13 => "INSERT INTO match_init (user1_id, user2_id, status) VALUES (least($1::uuid, $2::uuid), greatest($1::uuid, $2::uuid), 'ended')",
                14 => "INSERT INTO swipe_decision (actor_id, target_id, decision, swiped_at, expires_at) VALUES ($2, $1, 'pass', now(), now() + INTERVAL '30 days')",
                15 => "UPDATE user_profile SET sex = 'male' WHERE user_id = $1",
                _ => "UPDATE user_preferences SET max_age = 20 WHERE user_id = $1",
            };
            let query = sqlx::query(statement).bind(user);
            let query = if (11..=14).contains(&(index % 17)) { query.bind(viewer) } else { query };
            query.execute(&mut *connection).await?;
        }
        for (index, &user) in eligible.iter().enumerate() {
            // Identical locations exercise the UUID tie breaker across pages.
            ready_user(&database, user, 2, if index < 3 { -39.97 } else { -39.96 }).await?;
        }
        let mut connection = database.acquire().await?;
        sqlx::query("INSERT INTO swipe_decision (actor_id, target_id, decision, swiped_at, expires_at) VALUES ($1, $2, 'pass', now() - INTERVAL '31 days', now() - INTERVAL '1 day')")
            .bind(viewer).bind(eligible[0]).execute(&mut *connection).await?;
        drop(connection);

        let mut cursor = None;
        let mut actual = Vec::new();
        for _ in 0..4 {
            let page = repository.candidate_batch(viewer, "s19-v1", "s19-v1", 2, cursor, None).await?;
            if let Some(last) = page.last() {
                assert_eq!(page.len(), 2);
                cursor = Some(DiscoveryCursor { distance_km: last.distance_km, id: last.user_id });
            } else {
                break;
            }
            actual.extend(page.iter().map(|row| row.user_id));
        }
        assert_eq!(actual, eligible);
        assert!(repository.candidate_batch(viewer, "s19-v1", "s19-v1", 2, cursor, None).await?.is_empty());
        assert!(repository.candidate_batch(viewer, "wrong-version", "s19-v1", 2, None, None).await?.is_empty());

        // The same numeric distance expression must also work across ±180°.
        let mut connection = database.acquire().await?;
        sqlx::query("UPDATE user_presence SET latitude = 0, longitude = 179.999 WHERE user_id = $1")
            .bind(viewer).execute(&mut *connection).await?;
        sqlx::query("UPDATE user_presence SET latitude = 0, longitude = -179.999 WHERE user_id = ANY($1::uuid[])")
            .bind(&eligible[..2]).execute(&mut *connection).await?;
        drop(connection);
        let dateline = repository.candidate_batch(viewer, "s19-v1", "s19-v1", 2, None, None).await?;
        assert_eq!(dateline.iter().map(|row| row.user_id).collect::<Vec<_>>(), eligible[..2]);
        assert!(dateline.iter().all(|row| row.distance_km > 0.2 && row.distance_km < 0.3));
        let target = repository.candidate_batch(viewer, "s19-v1", "s19-v1", 2, None, Some(eligible[0])).await?;
        assert_eq!(target, dateline[..1]);
        let mut connection = database.acquire().await?;
        sqlx::query("UPDATE user_consent SET withdrawn_at = now() WHERE user_id = $1 AND consent_type = 'location_consent'")
            .bind(viewer).execute(&mut *connection).await?;
        drop(connection);
        assert!(repository.candidate_batch(viewer, "s19-v1", "s19-v1", 2, None, None).await?.is_empty());
        Ok(())
    }.await;

    cleanup(&database, &users).await?;
    database.close().await;
    result
}

#[tokio::test]
async fn polar_feed_preserves_exact_radius_and_live_eligibility_checks()
-> Result<(), Box<dyn std::error::Error>> {
    let config = postgres_config()?;
    let database = Database::connect(&config).await?;
    let repository = PgDiscoveryRepository::new(database.clone());
    let viewer = Uuid::new_v4();
    let nearby = Uuid::new_v4();
    let beyond_target_radius = Uuid::new_v4();
    let users = [viewer, nearby, beyond_target_radius];

    let result: Result<(), Box<dyn std::error::Error>> = async {
        ready_user(&database, viewer, 1, 89.999).await?;
        ready_user(&database, nearby, 2, 89.999).await?;
        ready_user(&database, beyond_target_radius, 4, 89.99).await?;
        let mut connection = database.acquire().await?;
        sqlx::query("UPDATE user_presence SET longitude = CASE WHEN user_id = $1 THEN 0 ELSE 90 END WHERE user_id = ANY($2::uuid[])")
            .bind(viewer).bind(&users[..]).execute(&mut *connection).await?;
        sqlx::query("UPDATE user_preferences SET max_distance_km = CASE WHEN user_id = $1 THEN 5 ELSE 1 END WHERE user_id = ANY($2::uuid[])")
            .bind(viewer).bind(&users[..]).execute(&mut *connection).await?;
        drop(connection);

        let feed = repository.candidate_batch(viewer, "s19-v1", "s19-v1", 8, None, None).await?;
        assert_eq!(feed.len(), 1);
        assert_eq!(feed[0].user_id, nearby);
        assert!(feed[0].distance_km > 0.15 && feed[0].distance_km < 0.16);
        assert_eq!(repository.candidate_batch(viewer, "s19-v1", "s19-v1", 1, None, Some(nearby)).await?, feed);
        assert!(repository.candidate_batch(viewer, "s19-v1", "s19-v1", 1, None, Some(beyond_target_radius)).await?.is_empty());

        for (reject, restore) in [
            ("UPDATE user_account SET is_banned = true WHERE user_id = $1", "UPDATE user_account SET is_banned = false WHERE user_id = $1"),
            ("UPDATE user_consent SET document_version = 'old' WHERE user_id = $1 AND consent_type = 'sensitive_data_consent'", "UPDATE user_consent SET document_version = 's19-v1' WHERE user_id = $1 AND consent_type = 'sensitive_data_consent'"),
            ("UPDATE user_consent SET withdrawn_at = clock_timestamp() WHERE user_id = $1 AND consent_type = 'location_consent'", "UPDATE user_consent SET withdrawn_at = NULL WHERE user_id = $1 AND consent_type = 'location_consent'"),
        ] {
            let mut connection = database.acquire().await?;
            sqlx::query(reject).bind(nearby).execute(&mut *connection).await?;
            drop(connection);
            assert!(repository.candidate_batch(viewer, "s19-v1", "s19-v1", 8, None, None).await?.is_empty());
            assert!(repository.candidate_batch(viewer, "s19-v1", "s19-v1", 1, None, Some(nearby)).await?.is_empty());
            let mut connection = database.acquire().await?;
            sqlx::query(restore).bind(nearby).execute(&mut *connection).await?;
        }
        Ok(())
    }.await;

    cleanup(&database, &users).await?;
    database.close().await;
    result
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

#[tokio::test]
async fn paused_profiles_are_filtered_and_atomic_writes_preserve_existing_matches()
-> Result<(), Box<dyn std::error::Error>> {
    use histae_api_rust::discovery::service::DiscoveryError;
    use histae_api_rust::discovery::store::DiscoveryStoreError;
    use histae_api_rust::matches::domain::MessageCreationResult;
    use histae_api_rust::matches::store::{MatchMessageStore, MatchStore};

    let (database, activity, service, swipes) = services(&postgres_config()?).await?;
    let users = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    for (index, user) in users.iter().enumerate() {
        ready_user(
            &database,
            *user,
            index as i32 + 1,
            48.8566 + index as f64 * 0.001,
        )
        .await?;
    }
    let result: Result<(), Box<dyn std::error::Error>> = async {
        service
            .swipe(users[0], users[1], SwipeDecision::Like)
            .await?;
        let existing = service
            .swipe(users[1], users[0], SwipeDecision::Like)
            .await?;
        assert!(existing.matched);
        let match_id = existing.r#match.ok_or(FixtureError("existing_match"))?.id;
        assert!(
            service
                .feed(users[2], 20, None)
                .await?
                .profiles
                .iter()
                .any(|p| p.user_id == users[0])
        );
        service.set_paused(users[0], true).await?;
        let second_reader = PgDiscoveryRepository::new(database.clone());
        assert!(second_reader.paused(users[0]).await?);
        assert!(
            !service
                .feed(users[2], 20, None)
                .await?
                .profiles
                .iter()
                .any(|p| p.user_id == users[0])
        );
        assert_eq!(
            service.feed(users[0], 20, None).await,
            Err(DiscoveryError::NotReady)
        );
        assert_eq!(
            service.swipe(users[2], users[0], SwipeDecision::Like).await,
            Err(DiscoveryError::CandidateNotFound)
        );
        assert_eq!(
            swipes.record(users[2], users[0], SwipeDecision::Like).await,
            Err(DiscoveryStoreError::CandidateNotFound)
        );
        assert_eq!(
            swipes.record(users[0], users[2], SwipeDecision::Like).await,
            Err(DiscoveryStoreError::NotReady)
        );
        let matches = PgMatchRepository::new(database.clone());
        assert_eq!(matches.list_for_user(users[0], 20, 0, None).await?.len(), 1);
        let messages = PgMatchMessageRepository::new(database.clone());
        for user in &users[..2] {
            assert!(matches!(
                messages
                    .create_message(
                        Uuid::new_v4(),
                        match_id,
                        *user,
                        "Existing conversation stays open".to_owned(),
                        Uuid::new_v4(),
                    )
                    .await?,
                MessageCreationResult::Available(_)
            ));
        }
        let creator = MatchService::new(
            Arc::new(matches),
            Arc::new(PgMatchMessageRepository::new(database.clone())),
            Arc::new(NoPhotoUrls),
            Arc::new(NoopMatchEventPublisher),
            Arc::new(SystemClock),
        );
        assert!(
            creator
                .create_from_mutual_like(users[0], users[2])
                .await
                .is_err()
        );
        service.set_paused(users[0], false).await?;
        assert!(
            service
                .feed(users[2], 20, None)
                .await?
                .profiles
                .iter()
                .any(|p| p.user_id == users[0])
        );
        Ok(())
    }
    .await;
    cleanup(&database, &users).await?;
    activity.close().await;
    database.close().await;
    result
}

#[tokio::test]
async fn writes_wait_for_a_concurrent_pause_and_recheck_both_participants()
-> Result<(), Box<dyn std::error::Error>> {
    use histae_api_rust::discovery::store::DiscoveryStoreError;
    use histae_api_rust::matches::service::MatchError;

    let (database, activity, service, swipes) = services(&postgres_config()?).await?;
    let users = [Uuid::new_v4(), Uuid::new_v4()];
    for (index, user) in users.iter().enumerate() {
        ready_user(&database, *user, index as i32 + 1, 48.8566).await?;
    }
    let result: Result<(), Box<dyn std::error::Error>> = async {
        for paused_user in users {
            for create_match in [false, true] {
                let mut connection = database.acquire().await?;
                let mut pausing = sqlx::Connection::begin(&mut *connection).await?;
                sqlx::query("UPDATE user_account SET discovery_paused=true WHERE user_id=$1")
                    .bind(paused_user)
                    .execute(&mut *pausing)
                    .await?;
                let pending = if create_match {
                    let creator = MatchService::new(
                        Arc::new(PgMatchRepository::new(database.clone())),
                        Arc::new(PgMatchMessageRepository::new(database.clone())),
                        Arc::new(NoPhotoUrls),
                        Arc::new(NoopMatchEventPublisher),
                        Arc::new(SystemClock),
                    );
                    tokio::spawn(async move {
                        matches!(
                            creator.create_from_mutual_like(users[0], users[1]).await,
                            Err(MatchError::CandidateNotFound)
                        )
                    })
                } else {
                    let swipes = swipes.clone();
                    tokio::spawn(async move {
                        let expected = if paused_user == users[0] {
                            DiscoveryStoreError::NotReady
                        } else {
                            DiscoveryStoreError::CandidateNotFound
                        };
                        swipes.record(users[0], users[1], SwipeDecision::Like).await
                            == Err(expected)
                    })
                };
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                loop {
                    let waiting: bool = sqlx::query_scalar(
                        "SELECT EXISTS (
                           SELECT 1 FROM pg_locks AS lock
                           JOIN pg_stat_activity AS activity ON activity.pid=lock.pid
                           WHERE NOT lock.granted AND lock.locktype='transactionid'
                             AND activity.datname=current_database()
                             AND activity.usename=current_user
                         )",
                    )
                    .fetch_one(database.acquire().await?.as_mut())
                    .await?;
                    if waiting {
                        break;
                    }
                    assert!(!pending.is_finished(), "the write must wait for the pause");
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "the write must reach the account lock"
                    );
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                pausing.commit().await?;
                drop(connection);
                assert!(pending.await?, "the committed pause must reject the write");
                service.set_paused(paused_user, false).await?;
            }
        }
        let writes: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM swipe_decision WHERE actor_id=ANY($1))
                  + (SELECT count(*) FROM match_init WHERE user1_id=ANY($1) OR user2_id=ANY($1))",
        )
        .bind(&users[..])
        .fetch_one(database.acquire().await?.as_mut())
        .await?;
        assert_eq!(writes, 0, "no discovery write may survive the pause race");
        Ok(())
    }
    .await;
    let cleaned = cleanup(&database, &users).await;
    activity.close().await;
    database.close().await;
    result?;
    cleaned?;
    Ok(())
}

#[tokio::test]
async fn discovery_requires_approved_photos_for_viewer_and_candidate()
-> Result<(), Box<dyn std::error::Error>> {
    use histae_api_rust::discovery::domain::DiscoveryRequiredAction;
    use histae_api_rust::discovery::service::DiscoveryError;
    let (database, activity, service, _) = services(&postgres_config()?).await?;
    let users = [Uuid::new_v4(), Uuid::new_v4()];
    let result: Result<(), Box<dyn std::error::Error>> = async {
        ready_user(&database, users[0], 1, 48.8566).await?;
        ready_user(&database, users[1], 2, 48.8576).await?;
        assert_eq!(service.feed(users[0], 20, None).await?.profiles.len(), 1);
        sqlx::query("UPDATE content_moderation_case SET status='pending' WHERE user_id=$1 AND content_type='photo'")
            .bind(users[1]).execute(database.acquire().await?.as_mut()).await?;
        assert!(service.feed(users[0], 20, None).await?.profiles.is_empty());
        assert!(service.status(users[1]).await?.required_actions.contains(&DiscoveryRequiredAction::Photo));
        assert_eq!(service.feed(users[1], 20, None).await.unwrap_err(), DiscoveryError::NotReady);
        assert_eq!(service.swipe(users[0], users[1], SwipeDecision::Like).await.unwrap_err(), DiscoveryError::CandidateNotFound);
        sqlx::query("DELETE FROM user_photo WHERE user_id=$1")
            .bind(users[0]).execute(database.acquire().await?.as_mut()).await?;
        assert!(service.status(users[0]).await?.required_actions.contains(&DiscoveryRequiredAction::Photo));
        Ok(())
    }.await;
    let cleaned = cleanup(&database, &users).await;
    activity.close().await;
    database.close().await;
    result?;
    cleaned?;
    Ok(())
}
