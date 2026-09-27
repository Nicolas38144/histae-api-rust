#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::notifications::devices::DeviceStore;
use histae_api_rust::notifications::domain::{
    DevicePlatform, DeviceRegistration, NotificationIntent,
};
use histae_api_rust::notifications::enqueue_notification;
use histae_api_rust::notifications::pg::PgNotificationRepository;
use serde_json::Value;
use sqlx::Acquire as _;
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PostgreSQL S14 fixture error ({})", self.0)
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
        application_name: "histae-rust-s14-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
    })
}

async fn fixture(
    database: &Database,
    user_id: Uuid,
    sessions: &[Uuid],
) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO user_account
           (user_id, role, phone_number_hash, phone_number_encrypted)
         VALUES ($1, 'user', $2, $3)",
    )
    .bind(user_id)
    .bind(format!("s14-{user_id}"))
    .bind(Vec::<u8>::new())
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    for session_id in sessions {
        sqlx::query(
            "INSERT INTO refresh_token_family
               (id, user_id, created_at, last_refreshed_at, expires_at)
             VALUES ($1, $2, clock_timestamp(), clock_timestamp(), $3)",
        )
        .bind(session_id)
        .bind(user_id)
        .bind(Utc::now() + TimeDelta::days(1))
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    }
    Ok(())
}

async fn cleanup(
    database: &Database,
    user_id: Uuid,
    known_outbox_ids: &[Uuid],
) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query(
        "DELETE FROM outbox_event
         WHERE id IN (
           SELECT delivery.id FROM notification_push_delivery delivery
           JOIN notification notification ON notification.id = delivery.notification_id
           WHERE notification.user_id = $1
         )",
    )
    .bind(user_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM outbox_event WHERE id = ANY($1::uuid[])")
        .bind(known_outbox_ids)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM user_account WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

#[tokio::test]
async fn devices_and_notification_jobs_preserve_transactions_deduplication_and_eligibility()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?)
        .await
        .map_err(|error| FixtureError(error.safe_code()))?;
    let repository = PgNotificationRepository::new(database.clone());
    let user_id = Uuid::new_v4();
    let sessions = [Uuid::new_v4(), Uuid::new_v4()];
    cleanup(&database, user_id, &[]).await?;
    fixture(&database, user_id, &sessions).await?;

    let mut created_outbox_ids = Vec::new();
    let test_result: Result<(), Box<dyn std::error::Error>> = async {
        let first = repository
            .register(
                user_id,
                sessions[0],
                DeviceRegistration {
                    token: format!("provider-token-{user_id}"),
                    platform: DevicePlatform::Ios,
                    app_version: Some("1.0.0".to_owned()),
                },
            )
            .await?
            .ok_or(FixtureError("first_device"))?;
        let reassigned = repository
            .register(
                user_id,
                sessions[0],
                DeviceRegistration {
                    token: format!("provider-token-{user_id}"),
                    platform: DevicePlatform::Android,
                    app_version: Some("2.0.0".to_owned()),
                },
            )
            .await?
            .ok_or(FixtureError("reassigned_device"))?;
        assert_eq!(reassigned.id, first.id);
        assert_eq!(reassigned.created_at, first.created_at);
        assert_eq!(reassigned.platform, DevicePlatform::Android);
        assert_eq!(reassigned.app_version.as_deref(), Some("2.0.0"));

        let revoked_device = repository
            .register(
                user_id,
                sessions[1],
                DeviceRegistration {
                    token: format!("provider-revoked-{user_id}"),
                    platform: DevicePlatform::Ios,
                    app_version: None,
                },
            )
            .await?
            .ok_or(FixtureError("revoked_device"))?;
        let legacy_device = Uuid::new_v4();
        let mut connection = database.acquire().await?;
        sqlx::query(
            "INSERT INTO device_token (id, user_id, token, platform, session_id)
             VALUES ($1, $2, $3, 'ios', NULL)",
        )
        .bind(legacy_device)
        .bind(user_id)
        .bind(format!("provider-legacy-{user_id}"))
        .execute(&mut *connection)
        .await?;
        sqlx::query(
            "UPDATE refresh_token_family
             SET revoked_at = clock_timestamp(), revocation_reason = 'logout'
             WHERE id = $1",
        )
        .bind(sessions[1])
        .execute(&mut *connection)
        .await?;
        drop(connection);

        assert_eq!(repository.list(user_id).await?.len(), 3);
        assert!(
            repository
                .register(
                    user_id,
                    sessions[1],
                    DeviceRegistration {
                        token: format!("provider-rejected-{user_id}"),
                        platform: DevicePlatform::Ios,
                        app_version: None,
                    },
                )
                .await?
                .is_none()
        );
        assert!(!repository.remove(Uuid::new_v4(), first.id).await?);
        assert!(!repository.remove(user_id, Uuid::new_v4()).await?);

        let source_id = Uuid::new_v4().hyphenated().to_string();
        let intent = NotificationIntent::NewMessage {
            match_id: Uuid::new_v4(),
            message_id: Uuid::new_v4(),
            sender_id: Uuid::new_v4(),
        };
        let mut connection = database.acquire().await?;
        let mut transaction = connection.begin().await?;
        enqueue_notification(&mut transaction, user_id, &source_id, &intent).await?;
        let inside_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM notification WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&mut *transaction)
                .await?;
        assert_eq!(inside_count, 1);
        transaction.rollback().await?;
        let rolled_back_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM notification WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&mut *connection)
                .await?;
        assert_eq!(rolled_back_count, 0);
        drop(connection);

        let committed_intent = intent.clone();
        database
            .transaction(|connection| {
                Box::pin(async move {
                    enqueue_notification(connection, user_id, &source_id, &committed_intent).await
                })
            })
            .await?;
        let mut query_connection = database.acquire().await?;
        let payload: Value =
            sqlx::query_scalar("SELECT payload FROM notification WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&mut *query_connection)
                .await?;
        assert_eq!(payload, intent.payload());
        assert!(!payload.to_string().contains("content"));
        let targeted_devices: Vec<Uuid> = sqlx::query_scalar(
            "SELECT delivery.device_id
             FROM notification_push_delivery delivery
             JOIN notification notification ON notification.id = delivery.notification_id
             WHERE notification.user_id = $1 ORDER BY delivery.device_id",
        )
        .bind(user_id)
        .fetch_all(&mut *query_connection)
        .await?;
        let mut expected = vec![first.id, legacy_device];
        expected.sort_unstable();
        assert_eq!(targeted_devices, expected);
        assert!(!targeted_devices.contains(&revoked_device.id));
        drop(query_connection);

        let concurrent_source = Uuid::new_v4().hyphenated().to_string();
        let left_database = database.clone();
        let right_database = database.clone();
        let left_intent = intent.clone();
        let right_intent = intent.clone();
        let left_source = concurrent_source.clone();
        let right_source = concurrent_source.clone();
        let (left, right) = tokio::join!(
            left_database.transaction(|connection| Box::pin(async move {
                enqueue_notification(connection, user_id, &left_source, &left_intent).await
            })),
            right_database.transaction(|connection| Box::pin(async move {
                enqueue_notification(connection, user_id, &right_source, &right_intent).await
            }))
        );
        left?;
        right?;
        let mut query_connection = database.acquire().await?;
        let counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT
               (SELECT count(*) FROM notification WHERE user_id = $1),
               (SELECT count(*) FROM notification_push_delivery delivery
                JOIN notification notification ON notification.id = delivery.notification_id
                WHERE notification.user_id = $1),
               (SELECT count(*) FROM outbox_event outbox
                JOIN notification_push_delivery delivery ON delivery.id = outbox.id
                JOIN notification notification ON notification.id = delivery.notification_id
                WHERE notification.user_id = $1)",
        )
        .bind(user_id)
        .fetch_one(&mut *query_connection)
        .await?;
        assert_eq!(counts, (2, 4, 4));
        drop(query_connection);

        let missing_invoice = format!("in_{}", Uuid::new_v4().simple());
        let missing_billing_source = Uuid::new_v4().hyphenated().to_string();
        let missing_billing_intent = NotificationIntent::BillingPaymentFailed {
            invoice_id: missing_invoice,
        };
        database
            .transaction(|connection| {
                Box::pin(async move {
                    enqueue_notification(
                        connection,
                        user_id,
                        &missing_billing_source,
                        &missing_billing_intent,
                    )
                    .await
                })
            })
            .await?;

        let invoice_id = format!("in_{}", Uuid::new_v4().simple());
        let customer_id = format!("cus_{}", Uuid::new_v4().simple());
        let subscription_id = format!("sub_{}", Uuid::new_v4().simple());
        let trial_ends_at = Utc::now() + TimeDelta::days(2);
        let mut billing_connection = database.acquire().await?;
        sqlx::query(
            "INSERT INTO billing_invoice
               (stripe_invoice_id, user_id, stripe_customer_id, status, currency,
                amount_due, amount_paid, amount_remaining, period_starts_at,
                period_ends_at, created_at)
             VALUES ($1, $2, $3, 'open', 'EUR', 1000, 0, 1000,
                     clock_timestamp(), clock_timestamp() + interval '1 month', clock_timestamp())",
        )
        .bind(&invoice_id)
        .bind(user_id)
        .bind(customer_id)
        .execute(&mut *billing_connection)
        .await?;
        sqlx::query(
            "INSERT INTO user_subscription
               (user_id, plan, provider, provider_subscription_id, status, trial_ends_at)
             VALUES ($1, 'free', 'stripe', $2, 'trialing', $3)",
        )
        .bind(user_id)
        .bind(&subscription_id)
        .bind(trial_ends_at)
        .execute(&mut *billing_connection)
        .await?;
        drop(billing_connection);

        let invoice_source = Uuid::new_v4().hyphenated().to_string();
        let invoice_intent = NotificationIntent::BillingPaymentFailed { invoice_id };
        database
            .transaction(|connection| {
                Box::pin(async move {
                    enqueue_notification(connection, user_id, &invoice_source, &invoice_intent)
                        .await
                })
            })
            .await?;
        let trial_source = Uuid::new_v4().hyphenated().to_string();
        let trial_intent = NotificationIntent::SubscriptionTrialEnding {
            subscription_id,
            trial_ends_at,
        };
        database
            .transaction(|connection| {
                Box::pin(async move {
                    enqueue_notification(connection, user_id, &trial_source, &trial_intent).await
                })
            })
            .await?;

        let mut query_connection = database.acquire().await?;
        let billing_counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT
               (SELECT count(*) FROM notification WHERE user_id = $1),
               (SELECT count(*) FROM notification_push_delivery delivery
                JOIN notification notification ON notification.id = delivery.notification_id
                WHERE notification.user_id = $1),
               (SELECT count(*) FROM outbox_event outbox
                JOIN notification_push_delivery delivery ON delivery.id = outbox.id
                JOIN notification notification ON notification.id = delivery.notification_id
                WHERE notification.user_id = $1)",
        )
        .bind(user_id)
        .fetch_one(&mut *query_connection)
        .await?;
        assert_eq!(billing_counts, (4, 8, 8));

        created_outbox_ids = sqlx::query_scalar(
            "SELECT outbox.id FROM outbox_event outbox
             JOIN notification_push_delivery delivery ON delivery.id = outbox.id
             JOIN notification notification ON notification.id = delivery.notification_id
             WHERE notification.user_id = $1 ORDER BY outbox.id",
        )
        .bind(user_id)
        .fetch_all(&mut *query_connection)
        .await?;
        assert_eq!(created_outbox_ids.len(), 8);
        drop(query_connection);

        assert!(repository.remove(user_id, first.id).await?);
        let mut query_connection = database.acquire().await?;
        let remaining_deliveries: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM notification_push_delivery delivery
             JOIN notification notification ON notification.id = delivery.notification_id
             WHERE notification.user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&mut *query_connection)
        .await?;
        assert_eq!(remaining_deliveries, 4);
        let remaining_outbox: i64 =
            sqlx::query_scalar("SELECT count(*) FROM outbox_event WHERE id = ANY($1::uuid[])")
                .bind(&created_outbox_ids)
                .fetch_one(&mut *query_connection)
                .await?;
        assert_eq!(remaining_outbox, 8);
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, user_id, &created_outbox_ids).await;
    database.close().await;
    cleanup_result?;
    test_result
}
