#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::notifications::delivery::{
    NotificationDeliveryStore, PgNotificationDeliveryStore,
};
use histae_api_rust::notifications::devices::{DeviceError, DeviceStore};
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
                    language: None,
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
                    language: None,
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
                    language: None,
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
                        language: None,
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
        let notification_id: Uuid =
            sqlx::query_scalar("SELECT id FROM notification WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&mut *query_connection)
                .await?;
        assert_eq!(notification_id.get_version_num(), 7);
        let job_ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT delivery.id FROM notification_push_delivery delivery
             JOIN outbox_event job ON job.id = delivery.id
             WHERE delivery.notification_id = $1",
        )
        .bind(notification_id)
        .fetch_all(&mut *query_connection)
        .await?;
        assert!(!job_ids.is_empty());
        assert!(job_ids.iter().all(|id| id.get_version_num() == 7));
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

        let mut query_connection = database.acquire().await?;
        created_outbox_ids = sqlx::query_scalar(
            "SELECT outbox.id FROM outbox_event outbox
             JOIN notification_push_delivery delivery ON delivery.id = outbox.id
             JOIN notification notification ON notification.id = delivery.notification_id
             WHERE notification.user_id = $1 ORDER BY outbox.id",
        )
        .bind(user_id)
        .fetch_all(&mut *query_connection)
        .await?;
        assert_eq!(created_outbox_ids.len(), 4);
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
        assert_eq!(remaining_deliveries, 2);
        let remaining_outbox: i64 =
            sqlx::query_scalar("SELECT count(*) FROM outbox_event WHERE id = ANY($1::uuid[])")
                .bind(&created_outbox_ids)
                .fetch_one(&mut *query_connection)
                .await?;
        assert_eq!(remaining_outbox, 4);
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, user_id, &created_outbox_ids).await;
    database.close().await;
    cleanup_result?;
    test_result
}

#[tokio::test]
async fn push_device_limits_and_binding_changes_apply_to_queued_deliveries()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?).await?;
    let user_id = Uuid::new_v4();
    let other_user = Uuid::new_v4();
    let sessions: [Uuid; 4] = std::array::from_fn(|_| Uuid::new_v4());
    let other_session = Uuid::new_v4();
    fixture(&database, user_id, &sessions).await?;
    fixture(&database, other_user, &[other_session]).await?;
    let devices = PgNotificationRepository::new(database.clone());
    let delivery = PgNotificationDeliveryStore::new(database.clone());
    let registration = |number| DeviceRegistration {
        token: format!("fixture-device-{user_id}-{number}"),
        platform: DevicePlatform::Ios,
        app_version: None,
        language: None,
    };
    let mut jobs = Vec::new();
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let mut initial = Vec::new();
        for (index, session_id) in sessions[..2].iter().copied().enumerate() {
            initial.push(devices.register(user_id, session_id, registration(index)).await?
                .ok_or(FixtureError("initial_device"))?);
        }
        let (left, right) = tokio::join!(
            devices.register(user_id, sessions[2], registration(2)),
            devices.register(user_id, sessions[3], registration(3)),
        );
        let (empty_slot, third_device) = match (left, right) {
            (Ok(Some(device)), Err(DeviceError::AccountLimitReached)) => (3, device),
            (Err(DeviceError::AccountLimitReached), Ok(Some(device))) => (2, device),
            _ => return Err(FixtureError("concurrent_device_limit").into()),
        };

        // Verify the independent list and fan-out limits with historical data above the cap.
        let mut connection = database.acquire().await?;
        let overflow: Vec<Uuid> = sqlx::query_scalar(
            "INSERT INTO device_token (user_id, token, platform)
             SELECT $1, $2 || suffix, 'ios' FROM generate_series(1, 2) AS suffix RETURNING id",
        )
        .bind(user_id)
        .bind(format!("legacy-device-{user_id}-"))
        .fetch_all(&mut *connection)
        .await?;
        assert_eq!(devices.list(user_id).await?.len(), 3);
        let opened_session = sessions[empty_slot];
        let intent = NotificationIntent::NewSession { session_id: opened_session };
        database.transaction(|connection| Box::pin(async move {
            enqueue_notification(connection, user_id, &opened_session.to_string(), &intent).await
        })).await?;
        let queued: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT d.device_id, d.id FROM notification_push_delivery d
             JOIN notification n ON n.id = d.notification_id
             JOIN outbox_event o ON o.id = d.id WHERE n.user_id = $1",
        ).bind(user_id).fetch_all(&mut *connection).await?;
        jobs.extend(queued.iter().map(|(_, id)| *id));
        assert_eq!(jobs.len(), 3);
        assert!(queued.iter().all(|(id, _)| !overflow.contains(id)));
        sqlx::query("DELETE FROM device_token WHERE id = ANY($1::uuid[])")
            .bind(&overflow).execute(&mut *connection).await?;
        drop(connection);
        let job_for = |device_id: Uuid| queued.iter().find_map(|(device, job)|
            (*device == device_id).then_some(*job)).ok_or(FixtureError("device_job"));

        let replaced_job = job_for(initial[1].id)?;
        let replacement = devices.register(user_id, sessions[1], registration(4)).await?
            .ok_or(FixtureError("replacement_device"))?;
        assert_ne!(replacement.id, initial[1].id);
        assert_eq!(devices.list(user_id).await?.len(), 3);
        assert!(delivery.find_deliverable(replaced_job).await?.is_none());

        let transferred_job = job_for(initial[0].id)?;
        let previous = delivery.find_deliverable(transferred_job).await?
            .ok_or(FixtureError("original_delivery"))?;
        let other_device = devices.register(other_user, other_session, registration(5)).await?
            .ok_or(FixtureError("other_device"))?;
        assert_eq!(devices.register(other_user, other_session, registration(0)).await,
            Err(DeviceError::TokenConflict));
        assert_eq!(devices.list(other_user).await?[0].id, other_device.id);
        assert!(delivery.find_deliverable(transferred_job).await?.is_some());
        assert!(!devices.remove(other_user, previous.device_id).await?);

        let moved = devices.register(user_id, opened_session, registration(0)).await?
            .ok_or(FixtureError("same_account_device"))?;
        assert_ne!(moved.id, previous.device_id);
        assert!(delivery.find_deliverable(transferred_job).await?.is_none());
        assert!(devices.remove(user_id, moved.id).await?);
        let transferred = devices.register(other_user, other_session, registration(0)).await?
            .ok_or(FixtureError("transferred_device"))?;
        assert_ne!(transferred.id, previous.device_id);
        assert!(delivery.find_deliverable(transferred_job).await?.is_none());
        delivery.remove_registration(previous.device_id, &previous.token).await?;
        assert_eq!(devices.list(other_user).await?[0].id, transferred.id);
        assert!(devices.remove(other_user, transferred.id).await?);
        let returned = devices.register(user_id, sessions[0], registration(0)).await?
            .ok_or(FixtureError("returned_device"))?;
        assert_ne!(returned.id, previous.device_id);
        assert!(delivery.find_deliverable(transferred_job).await?.is_none());

        let revoked_job = job_for(third_device.id)?;
        assert!(delivery.find_deliverable(revoked_job).await?.is_some());
        let mut connection = database.acquire().await?;
        sqlx::query("UPDATE refresh_token_family SET revoked_at = clock_timestamp(), revocation_reason = 'logout' WHERE id = $1")
            .bind(third_device.session_id).execute(&mut *connection).await?;
        drop(connection);
        assert!(delivery.find_deliverable(revoked_job).await?.is_none());
        assert!(devices.register(user_id, opened_session, registration(empty_slot)).await?.is_some());
        let active_devices = devices.list(user_id).await?;
        assert_eq!(active_devices.len(), 3);
        assert!(active_devices.iter().all(|device| device.id != third_device.id));

        let (first_claim, other_claim) = tokio::join!(
            devices.register(user_id, sessions[0], registration(6)),
            devices.register(other_user, other_session, registration(6)),
        );
        assert!(matches!((first_claim, other_claim),
            (Ok(Some(_)), Err(DeviceError::TokenConflict)) |
            (Err(DeviceError::TokenConflict), Ok(Some(_)))));
        Ok(())
    }.await;

    let first_cleanup = cleanup(&database, user_id, &jobs).await;
    let second_cleanup = cleanup(&database, other_user, &[]).await;
    database.close().await;
    first_cleanup?;
    second_cleanup?;
    result
}

#[path = "notifications_postgres/lifecycle.rs"]
mod lifecycle;

#[path = "notifications_postgres/preferences.rs"]
mod preferences;
