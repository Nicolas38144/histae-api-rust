#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::outbox::pg::{OutboxStore, PgOutboxRepository};
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

async fn cleanup(database: &Database, aggregate_ids: &[Uuid]) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM outbox_event WHERE aggregate_id = ANY($1)")
        .bind(aggregate_ids)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

#[tokio::test]
async fn claims_are_exclusive_stale_and_owned() -> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?)
        .await
        .map_err(|error| FixtureError(error.safe_code()))?;
    let repository = PgOutboxRepository::new(database.clone());
    let aggregates = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    cleanup(&database, &aggregates).await?;

    let test_result: Result<(), Box<dyn std::error::Error>> = async {
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
    .await;

    let cleanup_result = cleanup(&database, &aggregates).await;
    database.close().await;
    cleanup_result?;
    test_result
}
