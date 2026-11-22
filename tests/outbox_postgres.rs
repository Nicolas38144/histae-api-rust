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
            assert_initial_and_upgraded_schema_match(&config, &database).await?;
            assert_event_identifier_upgrade(&config, &database).await?;
            assert_payment_notification_retirement(&config, &database).await?;
            assert_index_upgrade(&config, &database).await?;
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

async fn assert_initial_and_upgraded_schema_match(
    config: &PostgresConfig,
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let initial = schema_snapshot(database).await?;
    // Simulate older installations without rewriting any stored identifiers.
    // Column order is deliberately excluded: ADD COLUMN appends on existing tables.
    let mut connection = database.acquire().await?;
    sqlx::raw_sql(
        "ALTER TABLE user_account DROP COLUMN is_review_account CASCADE;
         ALTER TABLE match_init DROP COLUMN photo_reveal_version CASCADE;
         ALTER TABLE billing_store_subscription DROP COLUMN superseded CASCADE;
         ALTER TABLE user_report DROP COLUMN public_response;
         ALTER TABLE user_profile_answer DROP CONSTRAINT chk_user_profile_answer_text;
         ALTER TABLE user_profile_answer ADD CONSTRAINT chk_user_profile_answer_text
           CHECK (answer=btrim(answer) AND char_length(answer) BETWEEN 10 AND 300 AND octet_length(answer)<=1000);
         DROP INDEX uq_profile_question_prompt_ci;
         CREATE UNIQUE INDEX uq_profile_question_prompt_ci ON profile_question(lower(prompt));
         DROP TRIGGER trg_save_data_request_reply ON data_subject_request;
         DROP TABLE data_request_message, assisted_data_export, erasure_receipt, support_case;
         ALTER TABLE notification DROP CONSTRAINT notification_type_check;
         ALTER TABLE notification ADD CONSTRAINT notification_type_check
           CHECK (type IN ('new_match','new_message'));
         ALTER TABLE chat_message ALTER COLUMN id SET DEFAULT uuid_generate_v4();
         ALTER TABLE notification ALTER COLUMN id SET DEFAULT uuid_generate_v4();
         ALTER TABLE outbox_event ALTER COLUMN id DROP DEFAULT;",
    )
    .execute(&mut *connection)
    .await?;
    let owner = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO user_account(user_id,phone_number_hash,phone_number_encrypted)
         VALUES ($1,$1::text,''::bytea)",
    )
    .bind(owner)
    .execute(&mut *connection)
    .await?;
    let legacy_reply = "é".repeat(1_500);
    let legacy_request: Uuid = sqlx::query_scalar(
        "INSERT INTO data_subject_request(user_id,type,response) VALUES ($1,'access',$2)
         RETURNING id",
    )
    .bind(owner)
    .bind(&legacy_reply)
    .fetch_one(&mut *connection)
    .await?;
    drop(connection);
    for _ in 0..2 {
        migrations::apply(config).await?;
        let upgraded = schema_snapshot(database).await?;
        assert_eq!(initial.len(), upgraded.len(), "schema object count changed");
        for (name, definition) in &initial {
            assert_eq!(upgraded.get(name), Some(definition), "schema drift: {name}");
        }
        let replies: Vec<(String, String)> =
            sqlx::query_as("SELECT author,body FROM data_request_message WHERE request_id=$1")
                .bind(legacy_request)
                .fetch_all(&mut *database.acquire().await?)
                .await?;
        assert_eq!(replies, [("team".to_owned(), legacy_reply.clone())]);
    }
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM user_account WHERE user_id=$1")
        .bind(owner)
        .execute(&mut *connection)
        .await?;
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT count(*),count(*) FILTER (WHERE NOT EXISTS (
           SELECT 1 FROM pg_depend dependency WHERE dependency.classid='pg_class'::regclass
             AND dependency.objid=relation.oid AND dependency.deptype='e'))
         FROM pg_class relation WHERE relation.relnamespace='public'::regnamespace
           AND relation.relkind IN ('r','p')",
    )
    .fetch_one(&mut *connection)
    .await?;
    println!(
        "Initial and upgraded schemas match: {} application tables, {} public tables including extensions.",
        counts.1, counts.0
    );
    Ok(())
}

async fn assert_event_identifier_upgrade(
    config: &PostgresConfig,
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let legacy_id = Uuid::new_v4();
    let aggregate = Uuid::new_v4();
    let mut connection = database.acquire().await?;
    sqlx::raw_sql(
        "ALTER TABLE chat_message ALTER COLUMN id SET DEFAULT uuid_generate_v4();
         ALTER TABLE notification ALTER COLUMN id SET DEFAULT uuid_generate_v4();
         ALTER TABLE outbox_event ALTER COLUMN id DROP DEFAULT;",
    )
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "INSERT INTO outbox_event(id,event_type,aggregate_id) VALUES($1,'photo.delete',$2)",
    )
    .bind(legacy_id)
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
    assert_eq!(retained, legacy_id);
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
    assert_eq!(requeued, legacy_id);
    sqlx::query("DELETE FROM outbox_event WHERE id=ANY($1)")
        .bind(&[legacy_id, generated][..])
        .execute(&mut *connection)
        .await?;
    Ok(())
}

async fn assert_payment_notification_retirement(
    config: &PostgresConfig,
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let user = Uuid::new_v4();
    let device = Uuid::new_v4();
    let notifications = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    let jobs = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    let mut connection = database.acquire().await?;
    // Recreate the previous notification schema and a pending delivery of each kind.
    sqlx::raw_sql(
        "ALTER TABLE notification
           ADD COLUMN billing_reference text, ADD COLUMN billing_trial_ends_at timestamptz,
           DROP CONSTRAINT notification_type_check,
           DROP CONSTRAINT IF EXISTS chk_notification_billing_context;
         ALTER TABLE notification
           ADD CONSTRAINT notification_type_check CHECK (
             type IN ('new_match', 'new_message', 'profile_liked', 'match_expiring',
                      'continuation_requested', 'billing_payment_failed', 'subscription_trial_ending')),
           ADD CONSTRAINT chk_notification_billing_context CHECK (
             (billing_reference IS NULL AND billing_trial_ends_at IS NULL) OR
             (type = 'billing_payment_failed' AND billing_reference ~ '^in_[A-Za-z0-9]+$'
               AND billing_trial_ends_at IS NULL) OR
             (type = 'subscription_trial_ending' AND billing_reference ~ '^sub_[A-Za-z0-9]+$'
               AND billing_trial_ends_at IS NOT NULL));",
    )
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "INSERT INTO user_account(user_id, phone_number_hash, phone_number_encrypted)
         VALUES ($1, $1::text, ''::bytea)",
    )
    .bind(user)
    .execute(&mut *connection)
    .await?;
    sqlx::query("INSERT INTO device_token(id, user_id, token, platform) VALUES ($1, $2, $1::text, 'android')")
        .bind(device).bind(user).execute(&mut *connection).await?;
    sqlx::query("INSERT INTO user_subscription(user_id,plan) VALUES($1,'free')")
        .bind(user)
        .execute(&mut *connection)
        .await?;
    sqlx::query(
        "INSERT INTO notification(id, user_id, type, billing_reference, billing_trial_ends_at, payload)
         VALUES ($1, $3, 'subscription_trial_ending', 'sub_RetiredTrial', clock_timestamp() + interval '1 day', NULL),
                ($2, $3, 'billing_payment_failed', 'in_StillUnpaid', NULL, NULL),
                ($4, $3, 'continuation_requested', NULL, NULL, '{}'::jsonb)",
    )
    .bind(notifications[0]).bind(notifications[1]).bind(user).bind(notifications[2])
    .execute(&mut *connection).await?;
    for (notification, job) in notifications.into_iter().zip(jobs) {
        sqlx::query("INSERT INTO outbox_event(id, event_type, aggregate_id) VALUES ($1, 'notification.push', $1)")
            .bind(job).execute(&mut *connection).await?;
        sqlx::query("INSERT INTO notification_push_delivery(id, notification_id, device_id) VALUES ($1, $2, $3)")
            .bind(job).bind(notification).bind(device).execute(&mut *connection).await?;
    }
    let snapshot_sql = "SELECT jsonb_build_array(id,type,payload,created_at,expires_at)
                        FROM notification WHERE id=$1";
    let retained_notice: serde_json::Value = sqlx::query_scalar(snapshot_sql)
        .bind(notifications[2])
        .fetch_one(&mut *connection)
        .await?;
    drop(connection);

    // Both the first upgrade and a repeated deployment must preserve unrelated work.
    for _ in 0..2 {
        assert_eq!(migrations::apply(config).await?, 0);
        let mut connection = database.acquire().await?;
        let remaining: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM notification WHERE user_id = $1")
                .bind(user)
                .fetch_all(&mut *connection)
                .await?;
        assert_eq!(remaining, vec![notifications[2]]);
        let actual: serde_json::Value = sqlx::query_scalar(snapshot_sql)
            .bind(notifications[2])
            .fetch_one(&mut *connection)
            .await?;
        assert_eq!(actual, retained_notice);
        let deliveries: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM notification_push_delivery WHERE device_id = $1")
                .bind(device)
                .fetch_all(&mut *connection)
                .await?;
        assert_eq!(deliveries, vec![jobs[2]]);
        let pending: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM outbox_event WHERE id = ANY($1::uuid[])")
                .bind(&jobs[..])
                .fetch_all(&mut *connection)
                .await?;
        assert_eq!(pending, vec![jobs[2]]);
        let old_column: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'notification' AND column_name IN ('billing_trial_ends_at','billing_reference'))")
            .fetch_one(&mut *connection).await?;
        assert!(!old_column);
        let current_types_restored: bool = sqlx::query_scalar(
            "SELECT pg_get_constraintdef(oid) LIKE '%data_request_replied%'
                AND pg_get_constraintdef(oid) LIKE '%first_period_ending%'
             FROM pg_constraint WHERE conrelid='notification'::regclass
               AND conname='notification_type_check'",
        )
        .fetch_one(&mut *connection)
        .await?;
        assert!(current_types_restored);
        assert!(
            sqlx::query(
                "INSERT INTO notification(user_id, type) VALUES ($1, 'subscription_trial_ending')"
            )
            .bind(user)
            .execute(&mut *connection)
            .await
            .is_err()
        );
    }
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM outbox_event WHERE id = ANY($1::uuid[])")
        .bind(&jobs[..])
        .execute(&mut *connection)
        .await?;
    sqlx::query("DELETE FROM user_account WHERE user_id = $1")
        .bind(user)
        .execute(&mut *connection)
        .await?;
    Ok(())
}

async fn assert_index_upgrade(
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
