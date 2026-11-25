#![cfg(feature = "postgres-integration")]

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use futures_util::FutureExt as _;
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::infra::{migrations, postgres::Database};
use histae_api_rust::outbox::pg::PgOutboxRepository;
use histae_api_rust::outbox::store::OutboxStore;
use histae_api_rust::outbox::types::{ClaimWindow, NewOutboxEvent, OutboxEventType, RetryResult};
use sqlx::Acquire;
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PostgreSQL S11 fixture error ({})", self.0)
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

fn local_config() -> Result<PostgresConfig, FixtureError> {
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
        port: env::var("POSTGRES_PORT")
            .unwrap_or_else(|_| "5432".to_owned())
            .parse()
            .map_err(|_| FixtureError("POSTGRES_PORT"))?,
        user: variable("POSTGRES_USER")?,
        password: SecretString::new(variable("POSTGRES_PASSWORD")?),
        database,
        tls: env::var("POSTGRES_SSLMODE").is_ok_and(|value| value != "disable"),
        max_connections: 4,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        statement_timeout: Duration::from_secs(15),
        idle_transaction_timeout: Duration::from_secs(30),
        application_name: "histae-rust-s11-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
    })
}

#[tokio::test]
async fn claims_are_exclusive_stale_and_owned() -> Result<(), Box<dyn std::error::Error>> {
    // The claim query intentionally sees the whole queue. A private database prevents this
    // test from claiming or completing unrelated events left by another run or the local API.
    let mut config = local_config()?;
    let admin = Database::connect(&config)
        .await
        .map_err(|error| FixtureError(error.safe_code()))?;
    let name = format!("histae_outbox_test_{}", Uuid::new_v4().simple());
    let mut connection = admin.acquire().await?;
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&mut *connection)
        .await?;
    config.database = name.clone();
    let result = AssertUnwindSafe(async {
        // Use the real migrator, never fabricate schema history for a fixture.
        migrations::apply(&config).await?;
        let database = Database::connect(&config).await?;
        let result = AssertUnwindSafe(async {
            assert_repeated_initialization_preserves_schema(&config, &database).await?;
            assert_event_identifiers(&config, &database).await?;
            assert_index_repair(&config, &database).await?;
            assert_claim_protocol(&database).await
        })
        .catch_unwind()
        .await;
        database.close().await;
        match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
    .catch_unwind()
    .await;
    // This identifier can only be the database created above, on the verified local server.
    let cleanup = sqlx::query(&format!("DROP DATABASE {name}"))
        .execute(&mut *connection)
        .await;
    drop(connection);
    admin.close().await;
    cleanup?;
    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

async fn schema_snapshot(
    database: &Database,
) -> Result<BTreeMap<String, String>, Box<dyn std::error::Error>> {
    let mut connection = database.acquire().await?;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT relation.relname || '.column.' || attribute.attname,
                jsonb_build_array(format_type(attribute.atttypid,attribute.atttypmod),
                  attribute.attnotnull,pg_get_expr(value.adbin,value.adrelid),
                  attribute.attidentity,attribute.attgenerated)::text
         FROM pg_class relation
         JOIN pg_attribute attribute ON attribute.attrelid=relation.oid
         LEFT JOIN pg_attrdef value ON value.adrelid=relation.oid AND value.adnum=attribute.attnum
         WHERE relation.relnamespace='public'::regnamespace AND relation.relkind IN ('r','p')
           AND attribute.attnum>0 AND NOT attribute.attisdropped
         UNION ALL
         SELECT relation.relname || '.constraint.' || rule.conname,
                pg_get_constraintdef(rule.oid,true)
         FROM pg_constraint rule JOIN pg_class relation ON relation.oid=rule.conrelid
         WHERE relation.relnamespace='public'::regnamespace
         UNION ALL
         SELECT tablename || '.index.' || indexname, indexdef
         FROM pg_indexes WHERE schemaname='public'
         UNION ALL
         SELECT relation.relname || '.trigger.' || trigger.tgname,
                pg_get_triggerdef(trigger.oid,true)
         FROM pg_trigger trigger JOIN pg_class relation ON relation.oid=trigger.tgrelid
         WHERE relation.relnamespace='public'::regnamespace AND NOT trigger.tgisinternal",
    )
    .fetch_all(&mut *connection)
    .await?;
    Ok(rows.into_iter().collect())
}

async fn assert_repeated_initialization_preserves_schema(
    config: &PostgresConfig,
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let initial = schema_snapshot(database).await?;
    for _ in 0..2 {
        assert_eq!(migrations::apply(config).await?, 0);
        assert_eq!(schema_snapshot(database).await?, initial);
    }
    Ok(())
}

async fn assert_event_identifiers(
    config: &PostgresConfig,
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let explicit_id = Uuid::new_v4();
    let aggregate = Uuid::new_v4();
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO outbox_event(id,event_type,aggregate_id) VALUES($1,'photo.delete',$2)",
    )
    .bind(explicit_id)
    .bind(aggregate)
    .execute(&mut *connection)
    .await?;
    drop(connection);
    for _ in 0..2 {
        migrations::apply(config).await?;
    }
    let mut connection = database.acquire().await?;
    let retained: Uuid = sqlx::query_scalar("SELECT id FROM outbox_event WHERE aggregate_id=$1")
        .bind(aggregate)
        .fetch_one(&mut *connection)
        .await?;
    assert_eq!(retained, explicit_id);
    let defaults: Vec<String> = sqlx::query_scalar(
        "SELECT column_default FROM information_schema.columns
         WHERE table_schema='public' AND column_name='id'
           AND table_name IN ('chat_message','notification','outbox_event')",
    )
    .fetch_all(&mut *connection)
    .await?;
    assert_eq!(defaults.len(), 3);
    assert!(defaults.iter().all(|value| value == "uuidv7()"));
    let generated: Uuid = sqlx::query_scalar(
        "INSERT INTO outbox_event(event_type,aggregate_id) VALUES('photo.delete',$1) RETURNING id",
    )
    .bind(Uuid::new_v4())
    .fetch_one(&mut *connection)
    .await?;
    assert_eq!(generated.get_version_num(), 7);
    drop(connection);
    let repository = PgOutboxRepository::new(database.clone());
    let mut connection = database.acquire().await?;
    repository
        .requeue(
            &mut connection,
            &NewOutboxEvent::empty(OutboxEventType::PhotoDelete, aggregate),
        )
        .await?;
    let requeued: Uuid = sqlx::query_scalar("SELECT id FROM outbox_event WHERE aggregate_id=$1")
        .bind(aggregate)
        .fetch_one(&mut *connection)
        .await?;
    assert_eq!(requeued, explicit_id);
    sqlx::query("DELETE FROM outbox_event WHERE id=ANY($1)")
        .bind(&[explicit_id, generated][..])
        .execute(&mut *connection)
        .await?;
    Ok(())
}

async fn assert_index_repair(
    config: &PostgresConfig,
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let users = [Uuid::new_v4(), Uuid::new_v4()];
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO user_account(user_id, phone_number_hash, phone_number_encrypted)
        SELECT id, id::text, ''::bytea FROM unnest($1::uuid[]) id",
    )
    .bind(&users[..])
    .execute(&mut *connection)
    .await?;
    sqlx::query("DROP INDEX idx_user_account_role_created")
        .execute(&mut *connection)
        .await?;
    // A failed concurrent build leaves an invalid index with the expected name.
    assert!(
        sqlx::query(
            "CREATE UNIQUE INDEX CONCURRENTLY idx_user_account_role_created ON user_account(role)"
        )
        .execute(&mut *connection)
        .await
        .is_err()
    );
    let valid: bool = sqlx::query_scalar("SELECT indisvalid FROM pg_index WHERE indexrelid = 'idx_user_account_role_created'::regclass")
        .fetch_one(&mut *connection).await?;
    assert!(!valid);
    assert_eq!(migrations::apply(config).await?, 0);
    assert_eq!(migrations::apply(config).await?, 0);
    let repaired: bool = sqlx::query_scalar(
        "SELECT indisvalid AND NOT indisunique AND indnkeyatts = 3
        FROM pg_index WHERE indexrelid = 'idx_user_account_role_created'::regclass",
    )
    .fetch_one(&mut *connection)
    .await?;
    assert!(repaired);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM user_account WHERE user_id = ANY($1)")
            .bind(&users[..])
            .fetch_one(&mut *connection)
            .await?;
    assert_eq!(count, 2);
    sqlx::query("DELETE FROM user_account WHERE user_id = ANY($1)")
        .bind(&users[..])
        .execute(&mut *connection)
        .await?;
    Ok(())
}

async fn assert_claim_protocol(database: &Database) -> Result<(), Box<dyn std::error::Error>> {
    let repository = PgOutboxRepository::new(database.clone());
    let aggregates = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];

    let available_at = Utc::now() + TimeDelta::days(1);
    let mut connection = database.acquire().await?;
    let mut transaction = connection.begin().await?;
    for aggregate_id in aggregates {
        assert!(
            repository
                .enqueue(
                    &mut transaction,
                    &NewOutboxEvent::empty(OutboxEventType::PhotoDelete, aggregate_id),
                )
                .await?
        );
    }
    sqlx::query("UPDATE outbox_event SET available_at = $1 WHERE aggregate_id = ANY($2)")
        .bind(available_at)
        .bind(&aggregates[..])
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;

    let first_worker = Uuid::new_v4();
    let second_worker = Uuid::new_v4();
    let claim_time = available_at + TimeDelta::seconds(1);
    let window = ClaimWindow {
        now: claim_time,
        stale_before: claim_time - TimeDelta::minutes(5),
    };
    let (first, second) = tokio::join!(
        repository.claim_batch(first_worker, window, 1),
        repository.claim_batch(second_worker, window, 1),
    );
    let first = first?;
    let second = second?;
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_ne!(first[0].id, second[0].id);
    assert_eq!(first[0].id.get_version_num(), 7);
    assert_eq!(second[0].id.get_version_num(), 7);
    assert!(!repository.renew_claim(first[0].id, second_worker).await?);
    assert!(repository.renew_claim(first[0].id, first_worker).await?);
    assert!(
        repository
            .complete(first[0].id, first_worker, claim_time)
            .await?
    );
    assert_eq!(
        repository
            .reschedule(
                second[0].id,
                second_worker,
                claim_time + TimeDelta::days(1),
                "handler_failed",
                10,
            )
            .await?,
        RetryResult::Pending
    );

    let third = repository
        .claim_batch(first_worker, window, 1)
        .await?
        .into_iter()
        .next()
        .ok_or(FixtureError("third_claim"))?;
    let stale_time = claim_time + TimeDelta::minutes(5);
    let reclaimed = repository
        .claim_batch(
            second_worker,
            ClaimWindow {
                now: stale_time,
                stale_before: claim_time,
            },
            1,
        )
        .await?;
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].id, third.id);
    assert_eq!(reclaimed[0].attempts, 2);
    Ok(())
}
