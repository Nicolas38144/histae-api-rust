#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::matches::domain::{
    ContinuationResult, MatchCommandResult, MatchRecord, MatchStatus, MessageCreationResult,
    PageCursor,
};
use histae_api_rust::matches::pg::{
    MatchMessageStore, MatchStore, PgMatchMessageRepository, PgMatchRepository,
};
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
