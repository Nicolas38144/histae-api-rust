#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone as _, Utc};
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::identity::admin_role::AdminRole;
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::infra::postgres_locks::MATCH_MAINTENANCE_LOCK;
use histae_api_rust::matches::maintenance::MatchMaintenanceRepository;
use histae_api_rust::outbox::admin::{
    OutboxAdminError, OutboxAdminService, OutboxOperator, PgOutboxAdminRepository,
};
use histae_api_rust::privacy::maintenance::PrivacyMaintenanceRepository;
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PostgreSQL S26 fixture error ({})", self.0)
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
        application_name: "histae-rust-s26-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
    })
}

async fn insert_account(
    database: &Database,
    user_id: Uuid,
    role: &str,
) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO user_account
           (user_id, role, phone_number_hash, phone_number_encrypted)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(role)
    .bind(format!("s26-{user_id}"))
    .bind(Vec::<u8>::new())
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

async fn insert_dead_letter(
    database: &Database,
    event_id: Uuid,
    event_type: &str,
    aggregate_id: Uuid,
) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO outbox_event
           (id, event_type, aggregate_id, status, attempts, last_error_code,
            created_at, dead_lettered_at)
         VALUES ($1, $2, $3, 'dead_letter', 10, 'handler_failed',
                 '2099-12-31T00:00:00Z', '2100-01-01T00:00:00Z')",
    )
    .bind(event_id)
    .bind(event_type)
    .bind(aggregate_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

async fn cleanup(
    database: &Database,
    users: &[Uuid],
    events: &[Uuid],
    match_id: Uuid,
) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM outbox_operator_action WHERE outbox_event_id = ANY($1::uuid[])")
        .bind(events)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM outbox_event WHERE id = ANY($1::uuid[])")
        .bind(events)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM user_report WHERE match_id = $1 OR reporter_id = ANY($2::uuid[])")
        .bind(match_id)
        .bind(users)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM chat_message WHERE match_id = $1")
        .bind(match_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM match_init WHERE id = $1")
        .bind(match_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM user_account WHERE user_id = ANY($1::uuid[])")
        .bind(users)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

#[tokio::test]
async fn preserves_operator_safety_leadership_and_bounded_cleanup()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?)
        .await
        .map_err(|error| FixtureError(error.safe_code()))?;
    let mut users = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    users[..2].sort_unstable();
    let [user1, user2, administrator] = users;
    let match_id = Uuid::new_v4();
    let photo_id = Uuid::new_v4();
    let retry_event = Uuid::new_v4();
    let discard_event = Uuid::new_v4();
    let account_event = Uuid::new_v4();
    let billing_event = Uuid::new_v4();
    let existing_photo_event = Uuid::new_v4();
    let absent_photo_event = Uuid::new_v4();
    let pending_event = Uuid::new_v4();
    let events = [
        retry_event,
        discard_event,
        account_event,
        billing_event,
        existing_photo_event,
        absent_photo_event,
        pending_event,
    ];
    cleanup(&database, &users, &events, match_id).await?;

    let test_result: Result<(), Box<dyn std::error::Error>> = async {
        insert_account(&database, user1, "user").await?;
        insert_account(&database, user2, "user").await?;
        insert_account(&database, administrator, "admin").await?;

        let mut connection = database.acquire().await?;
        sqlx::query(
            "INSERT INTO user_profile (user_id, firstname, birthdate)
             VALUES ($1, 'Maintenance', '1990-01-01')",
        )
        .bind(user1)
        .execute(&mut *connection)
        .await?;
        let object_key = format!("profile-photos/{user1}/{photo_id}.webp");
        sqlx::query(
            "INSERT INTO user_photo (id, user_id, object_key, status)
             VALUES ($1, $2, $3, 'deleting')",
        )
        .bind(photo_id)
        .bind(user1)
        .bind(object_key)
        .execute(&mut *connection)
        .await?;
        drop(connection);

        insert_dead_letter(&database, retry_event, "notification.push", Uuid::new_v4()).await?;
        insert_dead_letter(
            &database,
            discard_event,
            "notification.push",
            Uuid::new_v4(),
        )
        .await?;
        insert_dead_letter(&database, account_event, "account.erase", Uuid::new_v4()).await?;
        insert_dead_letter(
            &database,
            billing_event,
            "billing.subscription.reconcile",
            Uuid::new_v4(),
        )
        .await?;
        insert_dead_letter(&database, existing_photo_event, "photo.delete", photo_id).await?;
        insert_dead_letter(
            &database,
            absent_photo_event,
            "photo.delete",
            Uuid::new_v4(),
        )
        .await?;

        let mut connection = database.acquire().await?;
        sqlx::query(
            "INSERT INTO outbox_event (id, event_type, aggregate_id, status)
             VALUES ($1, 'notification.push', $2, 'pending')",
        )
        .bind(pending_event)
        .bind(Uuid::new_v4())
        .execute(&mut *connection)
        .await?;
        drop(connection);

        let service =
            OutboxAdminService::new(Arc::new(PgOutboxAdminRepository::new(database.clone())));
        let operator = OutboxOperator {
            user_id: administrator,
            role: AdminRole::Admin,
        };
        let page = service.dead_letters(100, None).await?;
        let listed = page
            .events
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>();
        for event_id in events.iter().take(6) {
            assert!(listed.contains(event_id));
        }
        assert!(!listed.contains(&pending_event));

        service
            .retry(retry_event, operator, "  Dépendance rétablie  ")
            .await?;
        service
            .discard(discard_event, operator, "Notification obsolète")
            .await?;
        assert_eq!(
            service
                .discard(account_event, operator, "Demande opérateur")
                .await,
            Err(OutboxAdminError::DiscardNotAllowed)
        );
        assert_eq!(
            service
                .discard(billing_event, operator, "Projection inspectée")
                .await,
            Err(OutboxAdminError::DiscardNotAllowed)
        );
        assert_eq!(
            service
                .discard(existing_photo_event, operator, "Objet vérifié")
                .await,
            Err(OutboxAdminError::DiscardNotAllowed)
        );
        service
            .discard(absent_photo_event, operator, "Objet déjà absent")
            .await?;
        assert_eq!(
            service
                .retry(pending_event, operator, "Relance impossible")
                .await,
            Err(OutboxAdminError::EventNotDeadLetter)
        );
        assert_eq!(
            service
                .retry(Uuid::new_v4(), operator, "Événement inconnu")
                .await,
            Err(OutboxAdminError::EventNotFound)
        );

        let mut connection = database.acquire().await?;
        let retry_state: (String, i16, Option<String>) = sqlx::query_as(
            "SELECT status, attempts, last_error_code FROM outbox_event WHERE id = $1",
        )
        .bind(retry_event)
        .fetch_one(&mut *connection)
        .await?;
        assert_eq!(retry_state, ("pending".to_owned(), 0, None));
        let discard_state: (String, Option<Uuid>, Option<String>) = sqlx::query_as(
            "SELECT status, resolved_by, resolution_reason
             FROM outbox_event WHERE id = $1",
        )
        .bind(discard_event)
        .fetch_one(&mut *connection)
        .await?;
        assert_eq!(discard_state.0, "discarded");
        assert_eq!(discard_state.1, Some(administrator));
        assert_eq!(discard_state.2.as_deref(), Some("Notification obsolète"));
        let audits: Vec<(String, String)> = sqlx::query_as(
            "SELECT action, reason FROM outbox_operator_action
             WHERE outbox_event_id = ANY($1::uuid[]) ORDER BY action, reason",
        )
        .bind(events)
        .fetch_all(&mut *connection)
        .await?;
        assert_eq!(audits.len(), 3);
        assert!(audits.contains(&("retry".to_owned(), "Dépendance rétablie".to_owned())));
        drop(connection);

        let maintenance_now = Utc
            .with_ymd_and_hms(1961, 1, 2, 0, 0, 0)
            .single()
            .ok_or(FixtureError("maintenance_now"))?;
        let old = Utc
            .with_ymd_and_hms(1960, 1, 1, 0, 0, 0)
            .single()
            .ok_or(FixtureError("old_timestamp"))?;
        let mut connection = database.acquire().await?;
        sqlx::query(
            "INSERT INTO match_init
               (id, user1_id, user2_id, status, expires_at, purge_after)
             VALUES ($1, $2, $3, 'ended', $4, $4)",
        )
        .bind(match_id)
        .bind(user1)
        .bind(user2)
        .bind(old)
        .execute(&mut *connection)
        .await?;
        sqlx::query(
            "INSERT INTO chat_message (id, match_id, sender_id, content, created_at)
             VALUES ($1, $2, $3, 'fixture', $4)",
        )
        .bind(Uuid::new_v4())
        .bind(match_id)
        .bind(user1)
        .bind(old)
        .execute(&mut *connection)
        .await?;
        let report_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO user_report
               (id, reporter_id, reported_id, match_id, reason, created_at)
             VALUES ($1, $2, $3, $4, 'other', $5)",
        )
        .bind(report_id)
        .bind(user1)
        .bind(user2)
        .bind(match_id)
        .bind(old)
        .execute(&mut *connection)
        .await?;
        sqlx::query(
            "INSERT INTO user_presence
               (user_id, latitude, longitude, is_location_fresh, updated_at)
             VALUES ($1, 48.856600, 2.352200, true, $2)",
        )
        .bind(user2)
        .bind(old)
        .execute(&mut *connection)
        .await?;
        let deletion_token_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO account_deletion_token (id, user_id, token_hash, expires_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(deletion_token_id)
        .bind(user2)
        .bind(format!("s26-token-{deletion_token_id}"))
        .bind(old)
        .execute(&mut *connection)
        .await?;

        let lock_acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(MATCH_MAINTENANCE_LOCK)
            .fetch_one(&mut *connection)
            .await?;
        assert!(lock_acquired);
        let matches = MatchMaintenanceRepository::new(database.clone());
        assert_eq!(matches.run_as_leader(maintenance_now, 1, 4).await?, None);
        let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
            .bind(MATCH_MAINTENANCE_LOCK)
            .fetch_one(&mut *connection)
            .await?;
        assert!(unlocked);
        drop(connection);

        let result = matches
            .run_as_leader(maintenance_now, 1, 4)
            .await?
            .ok_or(FixtureError("match_leader"))?;
        assert_eq!(result.deleted_messages, 1);
        assert_eq!(result.detached_reports, 1);
        assert_eq!(result.purged, 1);
        assert_eq!(result.batches, 2);
        assert!(!result.work_remaining);

        let privacy = PrivacyMaintenanceRepository::new(database.clone())
            .run_as_leader(maintenance_now, 10)
            .await?
            .ok_or(FixtureError("privacy_leader"))?;
        assert_eq!(privacy.stale_presences, 1);
        assert_eq!(privacy.expired_presences, 1);
        assert_eq!(privacy.expired_account_deletion_tokens, 1);

        let mut connection = database.acquire().await?;
        let report_match: Option<Uuid> =
            sqlx::query_scalar("SELECT match_id FROM user_report WHERE id = $1")
                .bind(report_id)
                .fetch_one(&mut *connection)
                .await?;
        assert_eq!(report_match, None);
        let match_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM match_init WHERE id = $1)")
                .bind(match_id)
                .fetch_one(&mut *connection)
                .await?;
        assert!(!match_exists);
        let presence_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_presence WHERE user_id = $1)")
                .bind(user2)
                .fetch_one(&mut *connection)
                .await?;
        assert!(!presence_exists);
        let token_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM account_deletion_token WHERE id = $1)")
                .bind(deletion_token_id)
                .fetch_one(&mut *connection)
                .await?;
        assert!(!token_exists);
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, &users, &events, match_id).await;
    database.close().await;
    cleanup_result?;
    test_result
}
