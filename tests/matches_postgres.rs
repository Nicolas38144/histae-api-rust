#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::matches::domain::{
    ContinuationResult, MatchCommandResult, MatchRecord, MatchStatus,
};
use histae_api_rust::matches::pg::{MatchStore, PgMatchRepository};
use sqlx::{Acquire as _, Row as _};
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "S17 integration fixture error ({})", self.0)
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
        max_connections: 6,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        statement_timeout: Duration::from_secs(15),
        idle_transaction_timeout: Duration::from_secs(30),
        application_name: "histae-rust-s17-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS").ok().map(PathBuf::from),
    })
}

async fn account(database: &Database, user_id: Uuid, firstname: &str) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO user_account
         (user_id, role, phone_number_hash, phone_number_encrypted)
         VALUES ($1, 'user', $2, $3)",
    )
    .bind(user_id)
    .bind(format!("s17-{user_id}"))
    .bind(Vec::<u8>::new())
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        "INSERT INTO user_profile (user_id, firstname, birthdate, sex, bio)
         VALUES ($1, $2, DATE '2000-01-01', 'female', $3)",
    )
    .bind(user_id)
    .bind(firstname)
    .bind(format!("Approved biography for {firstname}"))
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

async fn approved_photo(database: &Database, user_id: Uuid) -> Result<String, DatabaseError> {
    let photo_id = Uuid::new_v4();
    let object_key = format!("profile-photos/{user_id}/{photo_id}.webp");
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO user_photo
           (id, user_id, object_key, status, mime_type, size_bytes, width, height, sha256)
         VALUES ($1, $2, $3, 'ready', 'image/webp', 128, 32, 32, $4)",
    )
    .bind(photo_id)
    .bind(user_id)
    .bind(&object_key)
    .bind(vec![7_u8; 32])
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        "INSERT INTO content_moderation_case
           (id, user_id, content_type, photo_id, status, policy_version,
            face_detectable, sharp_enough, content_allowed)
         VALUES ($1, $2, 'photo', $3, 'approved', 's17-test', true, true, true)",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(photo_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        "INSERT INTO content_moderation_case
           (id, user_id, content_type, bio_user_id, status, policy_version)
         VALUES ($1, $2, 'bio', $2, 'approved', 's17-test')",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(object_key)
}

fn record(first: Uuid, second: Uuid, expires_at: chrono::DateTime<Utc>) -> MatchRecord {
    let [user1_id, user2_id] = if first < second {
        [first, second]
    } else {
        [second, first]
    };
    MatchRecord {
        id: Uuid::new_v4(),
        user1_id,
        user2_id,
        status: MatchStatus::Active,
        expires_at,
        purge_after: None,
        continuation_initiator_id: None,
        created_at: Utc::now(),
        last_message_at: None,
    }
}

async fn custom_plan(
    database: &Database,
    user_id: Uuid,
    weekly_limit: i16,
) -> Result<String, DatabaseError> {
    let code = format!("s17{}", &Uuid::new_v4().simple().to_string()[..12]);
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO subscription_plan
           (code, display_name, monthly_price_cents, annual_price_cents,
            weekly_continuation_limit)
         VALUES ($1, 'S17 test', 0, 0, $2)",
    )
    .bind(&code)
    .bind(weekly_limit)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query("INSERT INTO user_subscription (user_id, plan) VALUES ($1, $2)")
        .bind(user_id)
        .bind(&code)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(code)
}

async fn cleanup(
    database: &Database,
    users: &[Uuid],
    plans: &[String],
) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM user_account WHERE user_id = ANY($1::uuid[])")
        .bind(users)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM subscription_plan WHERE code = ANY($1::text[])")
        .bind(plans)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

#[tokio::test]
async fn creation_listing_and_reveal_keep_notifications_and_private_photo_rules_atomic()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let repository = PgMatchRepository::new(database.clone());
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    cleanup(&database, &[first, second], &[]).await?;
    account(&database, first, "First").await?;
    account(&database, second, "Second").await?;
    let second_photo = approved_photo(&database, second).await?;

    let test_result: Result<(), Box<dyn std::error::Error>> = async {
        let match_record = record(first, second, Utc::now() + TimeDelta::hours(24));
        repository
            .create(match_record.clone())
            .await
            .map_err(|_| FixtureError("create"))?;

        let notification_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM notification
             WHERE type = 'new_match' AND payload ->> 'match_id' = $1",
        )
        .bind(match_record.id.hyphenated().to_string())
        .fetch_one(database.acquire().await?.as_mut())
        .await?;
        assert_eq!(notification_count, 2);

        let before = repository
            .list_for_user(first, 20, 0, None)
            .await
            .map_err(|_| FixtureError("list_before_reveal"))?;
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].other_photo, None);
        assert_eq!(
            before[0].other_bio.as_deref(),
            Some("Approved biography for Second")
        );

        assert_eq!(
            repository
                .record_reveal(match_record.id, first)
                .await
                .map_err(|_| FixtureError("first_reveal"))?,
            MatchCommandResult::Available(false)
        );
        assert_eq!(
            repository
                .record_reveal(match_record.id, second)
                .await
                .map_err(|_| FixtureError("second_reveal"))?,
            MatchCommandResult::Available(true)
        );
        let after = repository
            .list_for_user(first, 20, 0, None)
            .await
            .map_err(|_| FixtureError("list_after_reveal"))?;
        assert_eq!(after[0].other_photo.as_deref(), Some(second_photo.as_str()));
        assert!(after[0].photos_revealed);
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, &[first, second], &[]).await;
    test_result?;
    cleanup_result?;
    Ok(())
}

#[tokio::test]
async fn continuation_quota_is_atomic_and_a_zero_limit_does_not_allocate_usage()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let repository = PgMatchRepository::new(database.clone());
    let initiator = Uuid::new_v4();
    let peers = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    let mut users = vec![initiator];
    users.extend(peers);
    cleanup(&database, &users, &[]).await?;
    account(&database, initiator, "Initiator").await?;
    for (index, peer) in peers.iter().enumerate() {
        account(&database, *peer, &format!("Peer{index}")).await?;
    }
    let limited_plan = custom_plan(&database, initiator, 1).await?;
    let zero_plan = custom_plan(&database, peers[2], 0).await?;
    let plans = vec![limited_plan, zero_plan];

    let test_result: Result<(), Box<dyn std::error::Error>> = async {
        let first_match = record(initiator, peers[0], Utc::now() - TimeDelta::seconds(1));
        let second_match = record(initiator, peers[1], Utc::now() - TimeDelta::seconds(1));
        repository
            .create(first_match.clone())
            .await
            .map_err(|_| FixtureError("create_first_limited"))?;
        repository
            .create(second_match.clone())
            .await
            .map_err(|_| FixtureError("create_second_limited"))?;
        assert_eq!(
            repository
                .record_continuation(first_match.id, initiator)
                .await
                .map_err(|_| FixtureError("initiate_first"))?,
            ContinuationResult::Pending
        );
        assert_eq!(
            repository
                .record_continuation(second_match.id, initiator)
                .await
                .map_err(|_| FixtureError("initiate_second"))?,
            ContinuationResult::Pending
        );

        let first_repository = repository.clone();
        let second_repository = repository.clone();
        let (first_result, second_result) = tokio::join!(
            first_repository.record_continuation(first_match.id, peers[0]),
            second_repository.record_continuation(second_match.id, peers[1])
        );
        let results = [
            first_result.map_err(|_| FixtureError("confirm_first"))?,
            second_result.map_err(|_| FixtureError("confirm_second"))?,
        ];
        assert!(results.contains(&ContinuationResult::Confirmed));
        assert!(results.contains(&ContinuationResult::QuotaReached));
        let usage: i16 = sqlx::query_scalar(
            "SELECT COALESCE(sum(used_count), 0)::smallint
             FROM continuation_usage WHERE user_id = $1",
        )
        .bind(initiator)
        .fetch_one(database.acquire().await?.as_mut())
        .await?;
        assert_eq!(usage, 1);

        let zero_match = record(peers[2], peers[0], Utc::now() - TimeDelta::seconds(1));
        repository
            .create(zero_match.clone())
            .await
            .map_err(|_| FixtureError("create_zero"))?;
        assert_eq!(
            repository
                .record_continuation(zero_match.id, peers[2])
                .await
                .map_err(|_| FixtureError("initiate_zero"))?,
            ContinuationResult::Pending
        );
        assert_eq!(
            repository
                .record_continuation(zero_match.id, peers[0])
                .await
                .map_err(|_| FixtureError("confirm_zero"))?,
            ContinuationResult::QuotaReached
        );
        let zero_usage: i64 =
            sqlx::query_scalar("SELECT count(*) FROM continuation_usage WHERE user_id = $1")
                .bind(peers[2])
                .fetch_one(database.acquire().await?.as_mut())
                .await?;
        assert_eq!(zero_usage, 0);
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, &users, &plans).await;
    test_result?;
    cleanup_result?;
    Ok(())
}

#[tokio::test]
async fn expiration_clock_is_read_after_waiting_for_the_match_lock()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let repository = PgMatchRepository::new(database.clone());
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    cleanup(&database, &[first, second], &[]).await?;
    account(&database, first, "LockFirst").await?;
    account(&database, second, "LockSecond").await?;

    let test_result: Result<(), Box<dyn std::error::Error>> = async {
        let match_record = record(first, second, Utc::now() + TimeDelta::milliseconds(500));
        repository
            .create(match_record.clone())
            .await
            .map_err(|_| FixtureError("create_lock_match"))?;

        let mut connection = database.acquire().await?;
        let mut transaction = connection.begin().await?;
        sqlx::query("SELECT id FROM match_init WHERE id = $1 FOR UPDATE")
            .bind(match_record.id)
            .fetch_one(&mut *transaction)
            .await?;

        let waiting_repository = repository.clone();
        let waiting = tokio::spawn(async move {
            waiting_repository
                .record_continuation(match_record.id, first)
                .await
        });
        tokio::time::sleep(Duration::from_millis(700)).await;
        transaction.commit().await?;
        let outcome = waiting
            .await
            .map_err(|_| FixtureError("continuation_task"))?
            .map_err(|_| FixtureError("continuation_after_lock"))?;
        assert_eq!(outcome, ContinuationResult::Pending);

        let row = sqlx::query(
            "SELECT status, expires_at > clock_timestamp() AS future FROM match_init WHERE id = $1",
        )
        .bind(match_record.id)
        .fetch_one(database.acquire().await?.as_mut())
        .await?;
        assert_eq!(row.try_get::<String, _>("status")?, "awaiting_continuation");
        assert!(row.try_get::<bool, _>("future")?);
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, &[first, second], &[]).await;
    test_result?;
    cleanup_result?;
    Ok(())
}
