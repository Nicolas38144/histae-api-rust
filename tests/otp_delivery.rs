#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::identity::mobile::account::{
    AccountStoreError, MobileAccountRepository, MobileAccountStore, NewMobileAccount,
};
use histae_api_rust::identity::mobile::otp::{
    BeginOtpDelivery, OtpDeliveryStart, OtpDeliveryState, OtpRepository, OtpStore,
    SmsDeliveryEvent, SmsEventKind, SmsEventOutcome,
};
use histae_api_rust::infra::postgres::{ConstraintKind, Database, DatabaseError, map_sqlx_error};
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PostgreSQL S09 fixture error ({})", self.0)
    }
}

impl std::error::Error for FixtureError {}

fn variable(name: &'static str) -> Result<String, FixtureError> {
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
        max_connections: 8,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        statement_timeout: Duration::from_secs(15),
        idle_transaction_timeout: Duration::from_secs(30),
        application_name: "histae-rust-s09-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
    })
}

fn begin(phone_hash: &str, otp_hash: &str, idempotency_key: Uuid) -> BeginOtpDelivery {
    BeginOtpDelivery {
        id: Uuid::new_v4(),
        phone_hash: phone_hash.to_owned(),
        otp_hash: otp_hash.to_owned(),
        idempotency_key,
        ttl: Duration::from_secs(600),
        settlement: Duration::from_secs(15),
    }
}

async fn cleanup_otp(database: &Database, phone_hashes: &[&str]) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM otp_verification WHERE phone_number_hash = ANY($1)")
        .bind(phone_hashes)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

#[tokio::test]
async fn otp_repository_serializes_idempotency_callbacks_and_consumption()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?)
        .await
        .map_err(|_| FixtureError("connect"))?;
    let repository = OtpRepository::new(database.clone());
    let suffix = Uuid::new_v4().simple().to_string();
    let phone_hash = format!("s09-phone-{suffix}");
    let other_phone_hash = format!("s09-other-{suffix}");
    let key = Uuid::new_v4();
    let first = begin(&phone_hash, "otp-hash", key);
    let second = BeginOtpDelivery {
        id: Uuid::new_v4(),
        ..first.clone()
    };
    let (left, right) = tokio::join!(repository.begin(first.clone()), repository.begin(second));
    let outcomes = [left?, right?];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, OtpDeliveryStart::Created(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| {
                matches!(
                    outcome,
                    OtpDeliveryStart::Existing(OtpDeliveryState::Pending, _)
                )
            })
            .count(),
        1
    );
    let conflict = repository
        .begin(BeginOtpDelivery {
            id: Uuid::new_v4(),
            phone_hash: other_phone_hash.clone(),
            ..first.clone()
        })
        .await?;
    assert_eq!(conflict, OtpDeliveryStart::Conflict);

    assert!(
        !repository
            .consume(phone_hash.clone(), "otp-hash".to_owned())
            .await?
    );
    assert!(
        repository
            .mark_accepted(
                first.id,
                phone_hash.clone(),
                "transaction-1".to_owned(),
                "message-1".to_owned(),
            )
            .await?
    );
    assert_eq!(
        repository
            .apply_sms_event(SmsDeliveryEvent {
                delivery_id: first.id,
                message_id: "message-1".to_owned(),
                transaction_id: Some("transaction-1".to_owned()),
                kind: SmsEventKind::Sent,
            })
            .await?,
        SmsEventOutcome::Applied
    );
    assert_eq!(
        repository
            .apply_sms_event(SmsDeliveryEvent {
                delivery_id: first.id,
                message_id: "message-1".to_owned(),
                transaction_id: Some("transaction-1".to_owned()),
                kind: SmsEventKind::Sent,
            })
            .await?,
        SmsEventOutcome::Ignored
    );
    let snapshot = repository.snapshot().await?;
    assert!(snapshot.states.sent >= 1);
    assert_eq!(snapshot.retention, "otp_expiry");
    assert_eq!(snapshot.handset_delivery, "not_confirmed");
    let serialized = serde_json::to_string(&snapshot)?;
    assert!(!serialized.contains(&phone_hash));
    assert!(!serialized.contains(&first.id.to_string()));

    let attempts = (0..8)
        .map(|_| repository.consume(phone_hash.clone(), "otp-hash".to_owned()))
        .collect::<Vec<_>>();
    let results = futures_util::future::join_all(attempts).await;
    assert_eq!(
        results
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|consumed| *consumed)
            .count(),
        1
    );

    cleanup_otp(&database, &[&phone_hash, &other_phone_hash]).await?;
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn newer_delivery_supersedes_ancestors_and_undelivered_is_absorbing()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?)
        .await
        .map_err(|_| FixtureError("connect"))?;
    let repository = OtpRepository::new(database.clone());
    let phone_hash = format!("s09-order-{}", Uuid::new_v4().simple());
    let old = begin(&phone_hash, "old-hash", Uuid::new_v4());
    let current = begin(&phone_hash, "current-hash", Uuid::new_v4());
    assert!(matches!(
        repository.begin(old.clone()).await?,
        OtpDeliveryStart::Created(_)
    ));
    assert!(matches!(
        repository.begin(current.clone()).await?,
        OtpDeliveryStart::Created(_)
    ));
    assert!(
        repository
            .mark_accepted(
                current.id,
                phone_hash.clone(),
                "current-tx".to_owned(),
                "current-message".to_owned(),
            )
            .await?
    );
    assert!(
        repository
            .consume(phone_hash.clone(), "current-hash".to_owned())
            .await?
    );
    assert_eq!(
        repository
            .apply_sms_event(SmsDeliveryEvent {
                delivery_id: old.id,
                message_id: "old-message".to_owned(),
                transaction_id: None,
                kind: SmsEventKind::Sent,
            })
            .await?,
        SmsEventOutcome::Applied
    );
    assert!(
        !repository
            .consume(phone_hash.clone(), "old-hash".to_owned())
            .await?
    );

    assert_eq!(
        repository
            .apply_sms_event(SmsDeliveryEvent {
                delivery_id: current.id,
                message_id: "current-message".to_owned(),
                transaction_id: Some("current-tx".to_owned()),
                kind: SmsEventKind::Undelivered,
            })
            .await?,
        SmsEventOutcome::Applied
    );
    assert_eq!(
        repository
            .apply_sms_event(SmsDeliveryEvent {
                delivery_id: current.id,
                message_id: "current-message".to_owned(),
                transaction_id: Some("current-tx".to_owned()),
                kind: SmsEventKind::Sent,
            })
            .await?,
        SmsEventOutcome::Ignored
    );
    let mut connection = database.acquire().await?;
    let state: String =
        sqlx::query_scalar("SELECT delivery_status FROM otp_verification WHERE id = $1")
            .bind(current.id)
            .fetch_one(&mut *connection)
            .await?;
    assert_eq!(state, "failed");
    drop(connection);

    cleanup_otp(&database, &[&phone_hash]).await?;
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn account_repository_preserves_tombstones_and_unique_creation_conflicts()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?)
        .await
        .map_err(|_| FixtureError("connect"))?;
    let repository = MobileAccountRepository::new(database.clone());
    let suffix = Uuid::new_v4().simple().to_string();
    let phone_hash = format!("s09-account-{suffix}");
    let tombstoned_hash = format!("s09-tombstone-{suffix}");
    let first_user = Uuid::new_v4();
    let created = repository
        .create(NewMobileAccount {
            user_id: first_user,
            phone_hash: phone_hash.clone(),
            encrypted_phone: vec![1_u8, 2, 3],
        })
        .await?;
    assert_eq!(created.user_id, first_user);
    assert_eq!(
        repository.find_by_phone_hash(phone_hash.clone()).await?,
        Some(created)
    );
    assert_eq!(
        repository
            .create(NewMobileAccount {
                user_id: Uuid::new_v4(),
                phone_hash: phone_hash.clone(),
                encrypted_phone: vec![4_u8, 5, 6],
            })
            .await,
        Err(AccountStoreError::Database(DatabaseError::Constraint(
            ConstraintKind::Unique
        )))
    );

    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO account_tombstone(phone_number_hash, reason, expires_at)
         VALUES ($1, 'banned_account', clock_timestamp() + INTERVAL '1 hour')",
    )
    .bind(&tombstoned_hash)
    .execute(&mut *connection)
    .await?;
    drop(connection);
    assert_eq!(
        repository
            .create(NewMobileAccount {
                user_id: Uuid::new_v4(),
                phone_hash: tombstoned_hash.clone(),
                encrypted_phone: vec![7_u8, 8, 9],
            })
            .await,
        Err(AccountStoreError::Tombstone)
    );

    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM user_account WHERE phone_number_hash = $1")
        .bind(&phone_hash)
        .execute(&mut *connection)
        .await?;
    sqlx::query("DELETE FROM account_tombstone WHERE phone_number_hash = $1")
        .bind(&tombstoned_hash)
        .execute(&mut *connection)
        .await?;
    drop(connection);
    database.close().await;
    Ok(())
}
