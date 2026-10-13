#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::matches::domain::{
    ContinuationResult, MatchAvailabilityFailure, MatchCommandResult, MatchRecord, MatchStatus,
    MessageCreationResult, PageCursor,
};
use histae_api_rust::matches::pg::{PgMatchMessageRepository, PgMatchRepository};
use histae_api_rust::matches::store::{MatchMessageStore, MatchStore};
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
                .record_continuation(first_match.id, initiator, 1)
                .await
                .map_err(|_| FixtureError("initiate_first"))?,
            ContinuationResult::Pending
        );
        assert_eq!(
            repository
                .record_continuation(second_match.id, initiator, 1)
                .await
                .map_err(|_| FixtureError("initiate_second"))?,
            ContinuationResult::QuotaReached
        );

        let first_repository = repository.clone();
        let second_repository = repository.clone();
        let (first_result, second_result) = tokio::join!(
            first_repository.record_continuation(first_match.id, peers[0], 1),
            second_repository.record_continuation(second_match.id, peers[1], 1)
        );
        let results = [
            first_result.map_err(|_| FixtureError("confirm_first"))?,
            second_result.map_err(|_| FixtureError("confirm_second"))?,
        ];
        assert!(results.contains(&ContinuationResult::Confirmed));
        assert!(results.contains(&ContinuationResult::Pending));
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
                .record_continuation(zero_match.id, peers[2], 1)
                .await
                .map_err(|_| FixtureError("initiate_zero"))?,
            ContinuationResult::QuotaReached
        );
        assert_eq!(
            repository
                .record_continuation(zero_match.id, peers[0], 1)
                .await
                .map_err(|_| FixtureError("confirm_zero"))?,
            ContinuationResult::Pending
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
                .record_continuation(match_record.id, first, 1)
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

#[tokio::test]
async fn message_creation_is_concurrently_idempotent_and_notifies_without_private_text()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let matches = PgMatchRepository::new(database.clone());
    let messages = PgMatchMessageRepository::new(database.clone());
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    cleanup(&database, &[first, second], &[]).await?;
    account(&database, first, "MessageFirst").await?;
    account(&database, second, "MessageSecond").await?;

    let test_result: Result<(), Box<dyn std::error::Error>> = async {
        let match_record = record(first, second, Utc::now() + TimeDelta::hours(24));
        matches
            .create(match_record.clone())
            .await
            .map_err(|_| FixtureError("create_message_match"))?;
        let key = Uuid::new_v4();
        let first_messages = messages.clone();
        let second_messages = messages.clone();
        let (left, right) = tokio::join!(
            first_messages.create_message(
                Uuid::new_v4(),
                match_record.id,
                first,
                "private hello".to_owned(),
                key,
            ),
            second_messages.create_message(
                Uuid::new_v4(),
                match_record.id,
                first,
                "private hello".to_owned(),
                key,
            )
        );
        let results = [
            left.map_err(|_| FixtureError("concurrent_message_left"))?,
            right.map_err(|_| FixtureError("concurrent_message_right"))?,
        ];
        let created = results
            .iter()
            .filter(|result| {
                matches!(
                    result,
                    MessageCreationResult::Available(creation) if creation.created
                )
            })
            .count();
        let replayed = results
            .iter()
            .filter(|result| {
                matches!(
                    result,
                    MessageCreationResult::Available(creation) if !creation.created
                )
            })
            .count();
        assert_eq!((created, replayed), (1, 1));
        let persisted_id = match &results[0] {
            MessageCreationResult::Available(creation) => creation.message.id,
            _ => return Err(FixtureError("unexpected_message_result").into()),
        };
        assert!(results.iter().all(|result| {
            matches!(result, MessageCreationResult::Available(creation) if creation.message.id == persisted_id)
        }));

        assert_eq!(
            messages
                .create_message(
                    Uuid::new_v4(),
                    match_record.id,
                    first,
                    "different".to_owned(),
                    key,
                )
                .await
                .map_err(|_| FixtureError("message_conflict"))?,
            MessageCreationResult::IdempotencyConflict
        );
        let row = sqlx::query(
            "SELECT
               (SELECT count(*)::integer FROM chat_message WHERE match_id = $1) AS messages,
               (SELECT count(*)::integer FROM notification
                WHERE type = 'new_message' AND payload ->> 'message_id' = $2) AS notifications,
               (SELECT bool_and(NOT payload ? 'content') FROM notification
                WHERE type = 'new_message' AND payload ->> 'message_id' = $2) AS content_absent,
               (SELECT last_message_at IS NOT NULL FROM match_init WHERE id = $1) AS active",
        )
        .bind(match_record.id)
        .bind(persisted_id.hyphenated().to_string())
        .fetch_one(database.acquire().await?.as_mut())
        .await?;
        assert_eq!(row.try_get::<i32, _>("messages")?, 1);
        assert_eq!(row.try_get::<i32, _>("notifications")?, 1);
        assert!(row.try_get::<bool, _>("content_absent")?);
        assert!(row.try_get::<bool, _>("active")?);
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, &[first, second], &[]).await;
    test_result?;
    cleanup_result?;
    Ok(())
}

#[tokio::test]
async fn message_pagination_keeps_microseconds_and_read_through_skips_own_messages()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let matches = PgMatchRepository::new(database.clone());
    let messages = PgMatchMessageRepository::new(database.clone());
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    cleanup(&database, &[first, second], &[]).await?;
    account(&database, first, "PageFirst").await?;
    account(&database, second, "PageSecond").await?;

    let test_result: Result<(), Box<dyn std::error::Error>> = async {
        let match_record = record(first, second, Utc::now() + TimeDelta::hours(24));
        matches
            .create(match_record.clone())
            .await
            .map_err(|_| FixtureError("create_page_match"))?;
        let ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        sqlx::query(
            "INSERT INTO chat_message (id, match_id, sender_id, content, created_at) VALUES
               ($1, $4, $5, 'newest', '2030-08-16T12:00:00.123900Z'),
               ($2, $4, $6, 'middle', '2030-08-16T12:00:00.123800Z'),
               ($3, $4, $5, 'oldest', '2030-08-16T12:00:00.123700Z')",
        )
        .bind(ids[0])
        .bind(ids[1])
        .bind(ids[2])
        .bind(match_record.id)
        .bind(second)
        .bind(first)
        .execute(database.acquire().await?.as_mut())
        .await?;

        let first_page = match messages
            .messages_for_user(match_record.id, first, 2, 0, None)
            .await
            .map_err(|_| FixtureError("first_message_page"))?
        {
            MatchCommandResult::Available(rows) => rows,
            _ => return Err(FixtureError("first_page_unavailable").into()),
        };
        assert_eq!(
            first_page
                .iter()
                .map(|row| row.message.content.as_str())
                .collect::<Vec<_>>(),
            ["newest", "middle"]
        );
        assert_eq!(first_page[0].cursor_at, "2030-08-16T12:00:00.123900Z");
        let cursor = PageCursor {
            at: chrono::DateTime::parse_from_rfc3339(&first_page[0].cursor_at)?.with_timezone(&Utc),
            id: first_page[0].message.id,
        };
        let second_page = match messages
            .messages_for_user(match_record.id, first, 2, 0, Some(cursor))
            .await
            .map_err(|_| FixtureError("second_message_page"))?
        {
            MatchCommandResult::Available(rows) => rows,
            _ => return Err(FixtureError("second_page_unavailable").into()),
        };
        assert_eq!(
            second_page
                .iter()
                .map(|row| row.message.content.as_str())
                .collect::<Vec<_>>(),
            ["middle", "oldest"]
        );

        let read = messages
            .mark_messages_read_through(match_record.id, ids[0], first)
            .await
            .map_err(|_| FixtureError("read_through"))?;
        assert!(matches!(
            read,
            MatchCommandResult::Available(Some(ref update)) if update.updated_count == 2
        ));
        let rows = sqlx::query(
            "SELECT sender_id, read_at IS NOT NULL AS is_read
             FROM chat_message WHERE match_id = $1 ORDER BY created_at DESC",
        )
        .bind(match_record.id)
        .fetch_all(database.acquire().await?.as_mut())
        .await?;
        assert!(rows.iter().all(|row| {
            let sender: Result<Uuid, _> = row.try_get("sender_id");
            let is_read: Result<bool, _> = row.try_get("is_read");
            matches!((sender, is_read), (Ok(id), Ok(read)) if read == (id == second))
        }));
        assert_eq!(
            messages
                .mark_message_read(match_record.id, ids[1], first)
                .await
                .map_err(|_| FixtureError("read_own_message"))?,
            MatchCommandResult::Available(None)
        );
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, &[first, second], &[]).await;
    test_result?;
    cleanup_result?;
    Ok(())
}

#[tokio::test]
async fn expired_match_transition_commits_while_the_new_message_is_refused()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let matches = PgMatchRepository::new(database.clone());
    let messages = PgMatchMessageRepository::new(database.clone());
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    cleanup(&database, &[first, second], &[]).await?;
    account(&database, first, "ExpiredFirst").await?;
    account(&database, second, "ExpiredSecond").await?;

    let test_result: Result<(), Box<dyn std::error::Error>> = async {
        let mut match_record = record(first, second, Utc::now() - TimeDelta::seconds(1));
        match_record.status = MatchStatus::AwaitingContinuation;
        matches
            .create(match_record.clone())
            .await
            .map_err(|_| FixtureError("create_expired_message_match"))?;
        let result = messages
            .create_message(
                Uuid::new_v4(),
                match_record.id,
                first,
                "too late".to_owned(),
                Uuid::new_v4(),
            )
            .await
            .map_err(|_| FixtureError("expired_message"))?;
        assert_eq!(
            result,
            MessageCreationResult::Unavailable(
                histae_api_rust::matches::domain::MatchAvailabilityFailure::Expired
            )
        );
        let row = sqlx::query(
            "SELECT status, purge_after IS NOT NULL AS purge_scheduled,
               (SELECT count(*)::integer FROM chat_message WHERE match_id = $1) AS messages
             FROM match_init WHERE id = $1",
        )
        .bind(match_record.id)
        .fetch_one(database.acquire().await?.as_mut())
        .await?;
        assert_eq!(row.try_get::<String, _>("status")?, "expired");
        assert!(row.try_get::<bool, _>("purge_scheduled")?);
        assert_eq!(row.try_get::<i32, _>("messages")?, 0);
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, &[first, second], &[]).await;
    test_result?;
    cleanup_result?;
    Ok(())
}

#[tokio::test]
async fn ending_a_match_is_atomic_idempotent_and_revokes_all_mobile_conversation_access()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let matches = PgMatchRepository::new(database.clone());
    let messages = PgMatchMessageRepository::new(database.clone());
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    account(&database, first, "EndFirst").await?;
    account(&database, second, "EndSecond").await?;
    let test_result: Result<(), Box<dyn std::error::Error>> = async {
        let item = record(first, second, Utc::now() + TimeDelta::hours(24));
        matches.create(item.clone()).await.map_err(|_| FixtureError("create_end_match"))?;
        let key = Uuid::new_v4();
        let created = messages.create_message(Uuid::new_v4(), item.id, first, "Before closure".to_owned(), key)
            .await.map_err(|_| FixtureError("message_before_end"))?;
        let MessageCreationResult::Available(created) = created else { panic!("message must be created") };
        assert_eq!(matches.record_end(item.id, Uuid::new_v4()).await.map_err(|_| FixtureError("foreign_end"))?, None);
        assert_eq!(matches.record_end(Uuid::new_v4(), first).await.map_err(|_| FixtureError("missing_end"))?, None);
        let (left, right) = tokio::join!(matches.record_end(item.id, first), matches.record_end(item.id, second));
        let left = left.map_err(|_| FixtureError("end_first"))?.expect("match");
        let right = right.map_err(|_| FixtureError("end_second"))?.expect("match");
        assert_ne!(left.changed, right.changed, "only one concurrent caller changes the match");
        assert_eq!(left.participant_ids, [item.user1_id, item.user2_id]);
        assert_eq!(right.participant_ids, left.participant_ids);
        let deadline: chrono::DateTime<Utc> = sqlx::query_scalar("SELECT purge_after FROM match_init WHERE id = $1")
            .bind(item.id).fetch_one(database.acquire().await?.as_mut()).await?;
        assert!((deadline - Utc::now()).num_days() >= 29);
        for participant in [first, second] {
            assert!(!matches.record_end(item.id, participant).await.map_err(|_| FixtureError("end_retry"))?.expect("match").changed);
            assert!(matches.list_for_user(participant, 20, 0, None).await.map_err(|_| FixtureError("ended_list"))?.is_empty());
            assert_eq!(messages.messages_for_user(item.id, participant, 20, 0, None).await.map_err(|_| FixtureError("ended_messages"))?,
                MatchCommandResult::Unavailable(MatchAvailabilityFailure::InvalidState));
            assert_eq!(matches.record_reveal(item.id, participant).await.map_err(|_| FixtureError("ended_reveal"))?,
                MatchCommandResult::Unavailable(MatchAvailabilityFailure::InvalidState));
            assert_eq!(matches.record_continuation(item.id, participant, 1).await.map_err(|_| FixtureError("ended_continue"))?, ContinuationResult::InvalidState);
            assert_eq!(messages.create_message(Uuid::new_v4(), item.id, participant, "After closure".to_owned(), Uuid::new_v4())
                .await.map_err(|_| FixtureError("ended_send"))?, MessageCreationResult::Unavailable(MatchAvailabilityFailure::InvalidState));
        }
        assert_eq!(messages.create_message(Uuid::new_v4(), item.id, first, "Before closure".to_owned(), key)
            .await.map_err(|_| FixtureError("ended_replay"))?, MessageCreationResult::Unavailable(MatchAvailabilityFailure::InvalidState));
        assert_eq!(messages.mark_message_read(item.id, created.message.id, second).await.map_err(|_| FixtureError("ended_read"))?,
            MatchCommandResult::Unavailable(MatchAvailabilityFailure::InvalidState));
        assert_eq!(messages.mark_messages_read_through(item.id, created.message.id, second).await.map_err(|_| FixtureError("ended_read_through"))?,
            MatchCommandResult::Unavailable(MatchAvailabilityFailure::InvalidState));
        let row = sqlx::query("SELECT status, purge_after, (SELECT count(*) FROM chat_message WHERE match_id = $1) AS message_count,
            (SELECT count(*) FROM user_block WHERE blocker_id = ANY($2) OR blocked_id = ANY($2)) AS block_count FROM match_init WHERE id = $1")
            .bind(item.id).bind(vec![first, second]).fetch_one(database.acquire().await?.as_mut()).await?;
        assert_eq!(row.try_get::<String, _>("status")?, "ended");
        assert_eq!(row.try_get::<chrono::DateTime<Utc>, _>("purge_after")?, deadline);
        assert_eq!(row.try_get::<i64, _>("message_count")?, 1);
        assert_eq!(row.try_get::<i64, _>("block_count")?, 0);
        Ok(())
    }.await;
    let cleanup_result = cleanup(&database, &[first, second], &[]).await;
    test_result?;
    cleanup_result?;
    Ok(())
}

#[tokio::test]
async fn ending_pending_confirmed_and_expired_matches_preserves_retention_and_usage()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let matches = PgMatchRepository::new(database.clone());
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    account(&database, first, "EndStatesFirst").await?;
    account(&database, second, "EndStatesSecond").await?;
    let test_result: Result<(), Box<dyn std::error::Error>> = async {
        sqlx::query("INSERT INTO continuation_usage (user_id, week_start, used_count) VALUES ($1, date_trunc('week', CURRENT_DATE)::date, 1)")
            .bind(first).execute(database.acquire().await?.as_mut()).await?;
        for status in [MatchStatus::AwaitingContinuation, MatchStatus::Confirmed, MatchStatus::Expired] {
            let mut item = record(first, second, Utc::now() - TimeDelta::seconds(1));
            item.status = status;
            item.purge_after = (status == MatchStatus::Expired).then(|| Utc::now() + TimeDelta::days(1));
            matches.create(item.clone()).await.map_err(|_| FixtureError("create_end_state"))?;
            assert!(matches.record_end(item.id, second).await.map_err(|_| FixtureError("end_state"))?.expect("match").changed);
            let deadline: chrono::DateTime<Utc> = sqlx::query_scalar("SELECT purge_after FROM match_init WHERE id = $1")
                .bind(item.id).fetch_one(database.acquire().await?.as_mut()).await?;
            if let Some(expected) = item.purge_after { assert_eq!(deadline.timestamp_micros(), expected.timestamp_micros()); }
            let used: i16 = sqlx::query_scalar("SELECT used_count FROM continuation_usage WHERE user_id = $1").bind(first).fetch_one(database.acquire().await?.as_mut()).await?;
            assert_eq!(used, 1);
            sqlx::query("DELETE FROM match_init WHERE id = $1").bind(item.id).execute(database.acquire().await?.as_mut()).await?;
        }
        Ok(())
    }.await;
    let cleanup_result = cleanup(&database, &[first, second], &[]).await;
    test_result?;
    cleanup_result?;
    Ok(())
}

#[tokio::test]
async fn messages_waiting_behind_a_closure_cannot_reopen_the_match()
-> Result<(), Box<dyn std::error::Error>> {
    let mut config = postgres_config()?;
    config.application_name = "histae-rust-end-race";
    let database = Database::connect(&config).await?;
    let matches = PgMatchRepository::new(database.clone());
    let messages = PgMatchMessageRepository::new(database.clone());
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    account(&database, first, "EndRaceFirst").await?;
    account(&database, second, "EndRaceSecond").await?;
    let test_result: Result<(), Box<dyn std::error::Error>> = async {
        let item = record(first, second, Utc::now() + TimeDelta::hours(24));
        matches.create(item.clone()).await.map_err(|_| FixtureError("create_end_race"))?;
        let mut connection = database.acquire().await?;
        let mut transaction = connection.begin().await?;
        sqlx::query("SELECT id FROM match_init WHERE id = $1 FOR UPDATE").bind(item.id).fetch_one(&mut *transaction).await?;
        let end = matches.record_end(item.id, first);
        tokio::pin!(end);
        // Drive the real closure until it is blocked on our transaction.
        let wait = async {
            loop {
                let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE application_name = 'histae-rust-end-race'
                    AND wait_event_type = 'Lock' AND query LIKE 'SELECT user1_id, user2_id, status FROM match_init%')")
                    .fetch_one(database.acquire().await?.as_mut()).await?;
                if waiting { return Ok::<_, Box<dyn std::error::Error>>(()); }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::select! {
            result = &mut end => panic!("closure completed before lock release: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(3), wait) => { result??; }
        }
        let send = messages.create_message(Uuid::new_v4(), item.id, second, "Queued message".to_owned(), Uuid::new_v4());
        tokio::pin!(send);
        tokio::select! {
            result = &mut send => panic!("message completed before lock release: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(30)) => {}
        }
        transaction.commit().await?;
        let (ended, sent) = tokio::join!(end, send);
        assert!(ended.map_err(|_| FixtureError("queued_end"))?.expect("match").changed);
        assert_eq!(sent.map_err(|_| FixtureError("queued_send"))?, MessageCreationResult::Unavailable(MatchAvailabilityFailure::InvalidState));
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM chat_message WHERE match_id = $1").bind(item.id).fetch_one(database.acquire().await?.as_mut()).await?;
        assert_eq!(count, 0);
        Ok(())
    }.await;
    let cleanup_result = cleanup(&database, &[first, second], &[]).await;
    test_result?;
    cleanup_result?;
    Ok(())
}

#[tokio::test]
async fn free_credits_reserve_atomically_and_both_users_pay_only_at_confirmation()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let repository = PgMatchRepository::new(database.clone());
    let owner = Uuid::new_v4();
    let peers = (0..4).map(|_| Uuid::new_v4()).collect::<Vec<_>>();
    let mut users = peers.clone();
    users.push(owner);
    for user in &users {
        account(&database, *user, "Credits").await?;
    }
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let mut matches = Vec::new();
        for peer in &peers {
            let m = record(owner, *peer, Utc::now() - TimeDelta::seconds(1));
            repository
                .create(m.clone())
                .await
                .map_err(|_| FixtureError("credit_match"))?;
            matches.push(m);
        }
        for m in &matches[..2] {
            assert_eq!(
                repository.record_continuation(m.id, owner, 1).await?,
                ContinuationResult::Pending
            );
        }
        let (a, b) = tokio::join!(
            repository.record_continuation(matches[2].id, owner, 1),
            repository.record_continuation(matches[3].id, owner, 1)
        );
        let results = [a?, b?];
        assert!(results.contains(&ContinuationResult::Pending));
        assert!(results.contains(&ContinuationResult::QuotaReached));
        let before = repository.quota_snapshot(owner, Utc::now()).await?;
        assert_eq!(
            (before.used, before.reserved, before.remaining),
            (0, 3, Some(0))
        );
        assert_eq!(
            repository
                .record_continuation(matches[0].id, peers[0], 1)
                .await?,
            ContinuationResult::Confirmed
        );
        assert_eq!(
            repository
                .record_continuation(matches[0].id, owner, 1)
                .await?,
            ContinuationResult::AlreadyConfirmed
        );
        for user in [owner, peers[0]] {
            assert_eq!(repository.quota_snapshot(user, Utc::now()).await?.used, 1);
        }
        repository.record_end(matches[1].id, peers[1]).await?;
        let after = repository.quota_snapshot(owner, Utc::now()).await?;
        assert_eq!(
            (after.used, after.reserved, after.remaining),
            (1, 1, Some(1))
        );
        Ok(())
    }
    .await;
    let cleaned = cleanup(&database, &users, &[]).await;
    result?;
    cleaned?;
    Ok(())
}

#[tokio::test]
async fn decision_window_pauses_sends_keeps_history_and_never_reopens_old_matches()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let repository = PgMatchRepository::new(database.clone());
    let messages = PgMatchMessageRepository::new(database.clone());
    let users = [Uuid::new_v4(), Uuid::new_v4()];
    for user in users {
        account(&database, user, "Paused").await?;
    }
    let result: Result<(),Box<dyn std::error::Error>>=async {
        let m=record(users[0],users[1],Utc::now()+TimeDelta::hours(24)); repository.create(m.clone()).await?;
        let key=Uuid::new_v4();
        assert!(matches!(messages.create_message(Uuid::new_v4(),m.id,users[0],"Before pause".into(),key).await?,MessageCreationResult::Available(_)));
        sqlx::query("UPDATE match_init SET expires_at=clock_timestamp()-INTERVAL '1 second' WHERE id=$1").bind(m.id).execute(database.acquire().await?.as_mut()).await?;
        let replay=messages.create_message(Uuid::new_v4(),m.id,users[0],"Before pause".into(),key).await?;
        assert_eq!(replay,MessageCreationResult::Unavailable(MatchAvailabilityFailure::InvalidState));
        assert!(matches!(messages.messages_for_user(m.id,users[1],20,0,None).await?,MatchCommandResult::Available(rows) if rows.len()==1));
        assert_eq!(repository.record_continuation(m.id,users[0],0).await?,ContinuationResult::PlanChanged);
        assert_eq!(repository.record_continuation(m.id,users[0],1).await?,ContinuationResult::Pending);
        assert_eq!(repository.record_continuation(m.id,users[1],1).await?,ContinuationResult::Confirmed);
        assert!(matches!(messages.create_message(Uuid::new_v4(),m.id,users[0],"After agreement".into(),Uuid::new_v4()).await?,MessageCreationResult::Available(_)));
        sqlx::query("UPDATE match_init SET status='active',expires_at=clock_timestamp()-INTERVAL '25 hours' WHERE id=$1").bind(m.id).execute(database.acquire().await?.as_mut()).await?;
        assert_eq!(repository.record_continuation(m.id,users[0], 1).await?,ContinuationResult::Expired);
        Ok(())
    }.await;
    let cleaned = cleanup(&database, &users, &[]).await;
    result?;
    cleaned?;
    Ok(())
}

#[tokio::test]
async fn advertising_stock_survives_week_reset_and_free_redemptions_stop_at_six()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let repository = PgMatchRepository::new(database.clone());
    let users = [Uuid::new_v4(), Uuid::new_v4()];
    for user in users {
        account(&database, user, "Advertising").await?;
    }
    let result: Result<(),Box<dyn std::error::Error>>=async {
        for user in users {
            sqlx::query("INSERT INTO continuation_wallet(user_id,advertising_credits) VALUES($1,3)").bind(user).execute(database.acquire().await?.as_mut()).await?;
            sqlx::query("INSERT INTO continuation_usage(user_id,week_start,used_count,free_used_count) VALUES($1,date_trunc('week',clock_timestamp() AT TIME ZONE 'UTC')::date,3,3)").bind(user).execute(database.acquire().await?.as_mut()).await?;
        }
        let m=record(users[0],users[1],Utc::now()-TimeDelta::seconds(1)); repository.create(m.clone()).await?;
        assert_eq!(repository.record_continuation(m.id,users[0], 1).await?,ContinuationResult::Pending);
        let reserved=repository.quota_snapshot(users[0],Utc::now()).await?;
        assert_eq!((reserved.advertising_balance,reserved.advertising_remaining),(3,2));
        assert_eq!(repository.record_continuation(m.id,users[1], 1).await?,ContinuationResult::Confirmed);
        assert_eq!(repository.quota_snapshot(users[0],Utc::now()).await?.advertising_balance,2);
        // Six successful continuations are a separate ceiling, even with stock.
        sqlx::query("UPDATE continuation_usage SET used_count=6 WHERE user_id=$1").bind(users[0]).execute(database.acquire().await?.as_mut()).await?;
        let now=Utc::now();let quota=repository.quota_snapshot(users[0],now).await?;
        assert_eq!(quota.remaining,Some(0));assert_eq!(quota.advertising_balance,2);
        let next=repository.quota_snapshot(users[0],now+TimeDelta::days(7)).await?;
        assert_eq!((next.used,next.free_remaining,next.advertising_balance,next.remaining),(0,3,2,Some(5)));
        Ok(())
    }.await;
    let cleaned = cleanup(&database, &users, &[]).await;
    result?;
    cleaned?;
    Ok(())
}

async fn legal_consents(
    database: &Database,
    user_id: Uuid,
    legal: &histae_api_rust::config::LegalConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    for (kind, version) in [
        ("terms_of_service_acceptance", &legal.terms_version),
        ("privacy_notice_acknowledgement", &legal.privacy_version),
    ] {
        sqlx::query("INSERT INTO user_consent(user_id,consent_type,granted,document_version) VALUES($1,$2,true,$3)")
            .bind(user_id).bind(kind).bind(version).execute(database.acquire().await?.as_mut()).await?;
    }
    Ok(())
}

#[tokio::test]
async fn advertising_rewards_are_bound_unique_capped_and_cancellable()
-> Result<(), Box<dyn std::error::Error>> {
    use histae_api_rust::advertising::{
        domain::{RewardPlatform, RewardStatus},
        error::RewardError,
        pg::RewardRepository,
        ssv::Callback,
    };
    let database = Database::connect(&postgres_config()?).await?;
    let mut config = histae_api_rust::config::AppConfig::from_env()?;
    config.advertising.enabled = true;
    config.advertising.android_ad_unit = "ca-app-pub-3940256099942544/5224354917".into();
    let rewards = RewardRepository::new(
        database.clone(),
        config.advertising.clone(),
        config.legal.clone(),
    );
    let users = [Uuid::new_v4(), Uuid::new_v4()];
    for user in users {
        account(&database, user, "Reward").await?;
        legal_consents(&database, user, &config.legal).await?;
    }
    let prefix = Uuid::new_v4().simple().to_string();
    let proof = |id, transaction: &str| Callback {
        intent_id: id,
        ad_unit: "5224354917".into(),
        transaction_id: format!("{prefix}{transaction}"),
        timestamp_ms: Utc::now().timestamp_millis(),
        reward_amount: 1,
        reward_item: "continuation_credit".into(),
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let intent = rewards.create(users[0], RewardPlatform::Android).await?;
        assert_eq!(
            rewards.create(users[0], RewardPlatform::Android).await?.id,
            intent.id
        );
        assert_eq!(
            rewards.status(users[1], intent.id).await.unwrap_err(),
            RewardError::NotFound
        );
        let mut bad = proof(intent.id, "abcd00");
        bad.reward_amount = 2;
        assert_eq!(
            rewards.complete(bad).await.unwrap_err(),
            RewardError::InvalidProof
        );
        let mut bad = proof(intent.id, "abcd00");
        bad.ad_unit = "9999999999".into();
        assert_eq!(
            rewards.complete(bad).await.unwrap_err(),
            RewardError::InvalidProof
        );
        let (a, b) = tokio::join!(
            rewards.complete(proof(intent.id, "abcd00")),
            rewards.complete(proof(intent.id, "abcd00"))
        );
        a?;
        b?;
        assert_eq!(
            rewards.status(users[0], intent.id).await?.status,
            RewardStatus::Credited
        );
        let repo = PgMatchRepository::new(database.clone());
        assert_eq!(
            repo.quota_snapshot(users[0], Utc::now())
                .await?
                .advertising_balance,
            1
        );
        let other = rewards.create(users[1], RewardPlatform::Android).await?;
        assert_eq!(
            rewards
                .complete(proof(other.id, "abcd00"))
                .await
                .unwrap_err(),
            RewardError::InvalidProof
        );
        assert_eq!(
            rewards.status(users[1], other.id).await?.status,
            RewardStatus::Pending
        );
        rewards.cancel(users[1], other.id).await?;
        assert_eq!(
            rewards
                .complete(proof(other.id, "abcd01"))
                .await
                .unwrap_err(),
            RewardError::InvalidProof
        );
        assert_eq!(
            rewards.status(users[1], other.id).await?.status,
            RewardStatus::Cancelled
        );
        for transaction in ["abcd02", "abcd03"] {
            let intent = rewards.create(users[0], RewardPlatform::Android).await?;
            rewards.complete(proof(intent.id, transaction)).await?;
        }
        assert_eq!(
            rewards
                .create(users[0], RewardPlatform::Android)
                .await
                .unwrap_err(),
            RewardError::StockFull
        );
        sqlx::query("INSERT INTO user_subscription(user_id,plan) VALUES($1,'premium')")
            .bind(users[1])
            .execute(database.acquire().await?.as_mut())
            .await?;
        assert_eq!(
            rewards
                .create(users[1], RewardPlatform::Android)
                .await
                .unwrap_err(),
            RewardError::NotEligible
        );
        Ok(())
    }
    .await;
    let cleaned = cleanup(&database, &users, &[]).await;
    result?;
    cleaned?;
    Ok(())
}

#[tokio::test]
async fn final_agreement_rechecks_plans_and_consents_without_partial_debits()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let legal = histae_api_rust::config::AppConfig::from_env()?.legal;
    let repository = PgMatchRepository::new(database.clone()).with_legal(legal.clone());
    let users = [Uuid::new_v4(), Uuid::new_v4()];
    for user in users {
        account(&database, user, "Consent").await?;
        legal_consents(&database, user, &legal).await?;
    }
    let result: Result<(),Box<dyn std::error::Error>>=async {
        sqlx::query("INSERT INTO user_subscription(user_id,plan) VALUES($1,'premium')").bind(users[0]).execute(database.acquire().await?.as_mut()).await?;
        let m=record(users[0],users[1],Utc::now()-TimeDelta::seconds(1));repository.create(m.clone()).await?;
        assert_eq!(repository.record_continuation(m.id,users[0],0).await?,ContinuationResult::Pending);
        sqlx::query("DELETE FROM user_subscription WHERE user_id=$1").bind(users[0]).execute(database.acquire().await?.as_mut()).await?;
        assert_eq!(repository.record_continuation(m.id,users[1],1).await?,ContinuationResult::Pending);
        assert_eq!(repository.quota_snapshot(users[1],Utc::now()).await?.used,0);
        let continued: bool=sqlx::query_scalar("SELECT continued FROM match_state WHERE match_id=$1 AND user_id=$2").bind(m.id).bind(users[0]).fetch_one(database.acquire().await?.as_mut()).await?;
        assert!(!continued);
        sqlx::query("UPDATE user_consent SET withdrawn_at=clock_timestamp() WHERE user_id=$1 AND consent_type='privacy_notice_acknowledgement'").bind(users[1]).execute(database.acquire().await?.as_mut()).await?;
        assert!(repository.record_continuation(m.id,users[0],1).await.is_err());
        assert_eq!(repository.quota_snapshot(users[0],Utc::now()).await?.used,0);
        assert_eq!(repository.quota_snapshot(users[1],Utc::now()).await?.used,0);
        Ok(())
    }.await;
    let cleaned = cleanup(&database, &users, &[]).await;
    result?;
    cleaned?;
    Ok(())
}

#[tokio::test]
async fn credits_export_and_erasure_include_own_data_and_release_partner_reservations()
-> Result<(), Box<dyn std::error::Error>> {
    use histae_api_rust::{
        advertising::{
            domain::{RewardPlatform, RewardStatus},
            error::RewardError,
            pg::RewardRepository,
            ssv::Callback,
        },
        privacy::export::{DataExportStore, JsonExportWriter, pg::PgDataExportStore},
    };
    use std::os::unix::fs::OpenOptionsExt;
    let database = Database::connect(&postgres_config()?).await?;
    let mut config = histae_api_rust::config::AppConfig::from_env()?;
    config.advertising.enabled = true;
    config.advertising.android_ad_unit = "ca-app-pub-3940256099942544/5224354917".into();
    let repository = PgMatchRepository::new(database.clone());
    let rewards = RewardRepository::new(
        database.clone(),
        config.advertising.clone(),
        config.legal.clone(),
    );
    let users = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    for user in users {
        account(&database, user, "Credit privacy").await?;
        legal_consents(&database, user, &config.legal).await?;
    }
    let export_path =
        std::env::temp_dir().join(format!("histae-credit-export-{}.json", Uuid::new_v4()));
    let result: Result<(),Box<dyn std::error::Error>>=async {
        sqlx::query("INSERT INTO continuation_wallet(user_id,advertising_credits) VALUES($1,2),($2,1)").bind(users[0]).bind(users[1]).execute(database.acquire().await?.as_mut()).await?;
        let partner_match=record(users[0],users[1],Utc::now()-TimeDelta::seconds(1));repository.create(partner_match.clone()).await?;
        let own_match=record(users[0],users[2],Utc::now()-TimeDelta::seconds(1));repository.create(own_match.clone()).await?;
        assert_eq!(repository.record_continuation(partner_match.id,users[1],1).await?,ContinuationResult::Pending);
        assert_eq!(repository.record_continuation(own_match.id,users[0],1).await?,ContinuationResult::Pending);
        let own_reward=rewards.create(users[0], RewardPlatform::Android).await?;
        let partner_reward=rewards.create(users[1], RewardPlatform::Android).await?;
        let transaction=Uuid::new_v4().simple().to_string();
        rewards.complete(Callback { intent_id:own_reward.id,ad_unit:"5224354917".into(),transaction_id:transaction.clone(),timestamp_ms:Utc::now().timestamp_millis(),reward_amount:1,reward_item:"continuation_credit".into() }).await?;
        let file=std::fs::OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(&export_path)?;
        let mut writer=JsonExportWriter::new(tokio::fs::File::from_std(file),1024*1024);
        writer.start_object(None).await.map_err(|_|FixtureError("export_start"))?;
        PgDataExportStore::new(database.clone()).write_snapshot(users[0],&mut writer,1).await.map_err(|_|FixtureError("credit_export"))?;
        writer.end_object().await.map_err(|_|FixtureError("export_end"))?;
        let (file,_)=writer.finish().await.map_err(|_|FixtureError("export_finish"))?;file.sync_all().await?;drop(file);
        let export: serde_json::Value=serde_json::from_slice(&tokio::fs::read(&export_path).await?)?;
        assert_eq!(export["continuation_wallet"]["advertising_credits"],3);
        assert_eq!(export["continuation_reservations"].as_array().expect("reservations").len(),1);
        assert_eq!(export["continuation_reservations"][0]["match_id"],own_match.id.to_string());
        assert_eq!(export["advertising_rewards"].as_array().expect("rewards").len(),1);
        assert_eq!(export["advertising_rewards"][0]["id"],own_reward.id.to_string());
        assert!(export["advertising_rewards"][0].get("transaction_hash").is_none());
        sqlx::query("SELECT fct_anonymize_user($1)").bind(users[0]).execute(database.acquire().await?.as_mut()).await?;
        let remaining: i64=sqlx::query_scalar("SELECT (SELECT count(*) FROM continuation_wallet WHERE user_id=$1)+(SELECT count(*) FROM advertising_reward_intent WHERE user_id=$1)+(SELECT count(*) FROM continuation_reservation WHERE match_id=ANY($2))")
            .bind(users[0]).bind(vec![own_match.id,partner_match.id]).fetch_one(database.acquire().await?.as_mut()).await?;
        assert_eq!(remaining,0);
        let peer=repository.quota_snapshot(users[1],Utc::now()).await?;
        assert_eq!((peer.free_remaining,peer.advertising_balance,peer.reserved),(3,1,0));
        assert_eq!(rewards.status(users[1],partner_reward.id).await?.status, RewardStatus::Pending);
        assert_eq!(rewards.complete(Callback { intent_id:own_reward.id,ad_unit:"5224354917".into(),transaction_id:transaction,timestamp_ms:Utc::now().timestamp_millis(),reward_amount:1,reward_item:"continuation_credit".into() }).await.unwrap_err(),RewardError::NotFound);
        Ok(())
    }.await;
    let _ = tokio::fs::remove_file(export_path).await;
    let cleaned = cleanup(&database, &users, &[]).await;
    result?;
    cleaned?;
    Ok(())
}

#[tokio::test]
async fn subscription_changes_wait_with_confirmation_and_require_fresh_paid_agreement()
-> Result<(), Box<dyn std::error::Error>> {
    let mut configuration = postgres_config()?;
    configuration.application_name = "histae-continuation-subscription-race";
    let database = Database::connect(&configuration).await?;
    let repository = PgMatchRepository::new(database.clone());
    let users = [Uuid::new_v4(), Uuid::new_v4()];
    for user in users {
        account(&database, user, "Subscription race").await?;
    }
    let result: Result<(),Box<dyn std::error::Error>>=async {
        sqlx::query("INSERT INTO user_subscription(user_id,plan) VALUES($1,'premium')").bind(users[0]).execute(database.acquire().await?.as_mut()).await?;
        let m=record(users[0],users[1],Utc::now()-TimeDelta::seconds(1));repository.create(m.clone()).await?;
        assert_eq!(repository.record_continuation(m.id,users[0],0).await?,ContinuationResult::Pending);
        let mut connection=database.acquire().await?;let mut downgrade=connection.begin().await?;
        sqlx::query("DELETE FROM user_subscription WHERE user_id=$1").bind(users[0]).execute(&mut *downgrade).await?;
        let other=repository.clone();let task=tokio::spawn(async move { other.record_continuation(m.id,users[1],1).await });
        let deadline=tokio::time::Instant::now()+Duration::from_secs(5);
        loop {
            let waiting: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name='histae-continuation-subscription-race' AND wait_event_type='Lock')")
                .fetch_one(database.acquire().await?.as_mut()).await?;
            if waiting { break; }
            assert!(tokio::time::Instant::now()<deadline,"confirmation should wait for the subscription transaction");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        downgrade.commit().await?;
        assert_eq!(task.await??,ContinuationResult::Pending);
        for user in users { assert_eq!(repository.quota_snapshot(user,Utc::now()).await?.used,0); }
        assert_eq!(repository.record_continuation(m.id,users[0],1).await?,ContinuationResult::Confirmed);
        for user in users { assert_eq!(repository.quota_snapshot(user,Utc::now()).await?.used,1); }
        Ok(())
    }.await;
    let cleaned = cleanup(&database, &users, &[]).await;
    result?;
    cleaned?;
    Ok(())
}
