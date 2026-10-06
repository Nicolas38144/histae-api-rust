#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::{
    Database, DatabaseError, duration_setting, map_sqlx_error, verify_schema_compatibility_on,
};
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::Acquire;
use sqlx::types::Json;
use uuid::Uuid;

type TypeProbeRow = (
    Uuid,
    NaiveDate,
    DateTime<Utc>,
    Decimal,
    i64,
    Json<serde_json::Value>,
    Vec<u8>,
    f64,
);

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "PostgreSQL integration fixture error ({})",
            self.0
        )
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
    let port = env::var("POSTGRES_PORT")
        .unwrap_or_else(|_| "5432".to_owned())
        .parse::<u16>()
        .map_err(|_| FixtureError("POSTGRES_PORT"))?;
    Ok(PostgresConfig {
        host,
        port,
        user: variable("POSTGRES_USER")?,
        password: SecretString::new(variable("POSTGRES_PASSWORD")?),
        database,
        tls: env::var("POSTGRES_SSLMODE").is_ok_and(|value| value != "disable"),
        max_connections: 1,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        statement_timeout: Duration::from_secs(15),
        idle_transaction_timeout: Duration::from_secs(30),
        application_name: "histae-rust-s05-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
    })
}

#[tokio::test]
async fn reads_the_existing_schema_types_and_transaction_protocol()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?)
        .await
        .map_err(|_| FixtureError("connect"))?;

    let mut connection = database
        .acquire()
        .await
        .map_err(|_| FixtureError("initial_acquire"))?;
    let statement_timeout: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&mut *connection)
        .await?;
    let idle_transaction_timeout: String =
        sqlx::query_scalar("SHOW idle_in_transaction_session_timeout")
            .fetch_one(&mut *connection)
            .await?;
    assert_eq!(
        duration_setting(&statement_timeout)?,
        Duration::from_secs(15)
    );
    assert_eq!(
        duration_setting(&idle_transaction_timeout)?,
        Duration::from_secs(30)
    );

    let schema = format!("schema_probe_{}", Uuid::new_v4().simple());
    let mut schema_probe = connection.begin().await?;
    let schema_result: Result<(), Box<dyn std::error::Error>> = async {
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&mut *schema_probe)
            .await?;
        sqlx::query(&format!("SET LOCAL search_path TO {schema}, public"))
            .execute(&mut *schema_probe)
            .await?;
        assert_eq!(
            verify_schema_compatibility_on(&mut schema_probe).await,
            Err(DatabaseError::SchemaObjectsMissing)
        );
        sqlx::query("CREATE TABLE user_account(id integer)")
            .execute(&mut *schema_probe)
            .await?;
        sqlx::query("CREATE TABLE swipe_decision(id integer)")
            .execute(&mut *schema_probe)
            .await?;
        sqlx::query("CREATE TABLE account_erasure(step text, scylla_partition smallint, CONSTRAINT account_erasure_step_check CHECK (step IN ('scylla', 'completed')))")
            .execute(&mut *schema_probe)
            .await?;
        sqlx::query("CREATE TABLE admin_webauthn_challenge(id integer)")
            .execute(&mut *schema_probe)
            .await?;
        assert_eq!(
            verify_schema_compatibility_on(&mut schema_probe).await,
            Err(DatabaseError::SchemaObjectsMissing)
        );
        sqlx::query("ALTER TABLE account_erasure DROP COLUMN scylla_partition")
            .execute(&mut *schema_probe)
            .await?;
        sqlx::query("ALTER TABLE account_erasure DROP CONSTRAINT account_erasure_step_check")
            .execute(&mut *schema_probe)
            .await?;
        sqlx::query("ALTER TABLE account_erasure ADD CONSTRAINT account_erasure_step_check CHECK (step IN ('swipes', 'completed'))")
            .execute(&mut *schema_probe)
            .await?;
        sqlx::query("ALTER TABLE admin_webauthn_challenge ADD COLUMN ceremony_state bytea")
            .execute(&mut *schema_probe)
            .await?;
        sqlx::query("ALTER TABLE admin_webauthn_challenge ADD CONSTRAINT chk_admin_webauthn_challenge_state CHECK (ceremony_state IS NULL OR octet_length(ceremony_state) BETWEEN 1 AND 65536)")
            .execute(&mut *schema_probe)
            .await?;
        assert_eq!(
            verify_schema_compatibility_on(&mut schema_probe).await,
            Ok(())
        );
        Ok(())
    }
    .await;
    schema_probe.rollback().await?;
    schema_result?;

    let identifier = Uuid::new_v4();
    let calendar_date = NaiveDate::parse_from_str("2000-02-29", "%Y-%m-%d")?;
    let instant = DateTime::parse_from_rfc3339("2026-09-20T12:34:56.123456Z")?.with_timezone(&Utc);
    let numeric = Decimal::from_str("48.856613")?;
    let counter = 9_007_199_254_740_991_i64;
    let payload = Json(json!({ "status": "pending", "attempt": 1 }));
    let bytes = vec![0_u8, 1, 127, 255];
    let score = 0.625_f64;
    let row: TypeProbeRow = sqlx::query_as(
        "SELECT $1::uuid, $2::date, $3::timestamptz, $4::numeric(9,6), $5::bigint,
            $6::jsonb, $7::bytea, $8::double precision",
    )
    .bind(identifier)
    .bind(calendar_date)
    .bind(instant)
    .bind(numeric)
    .bind(counter)
    .bind(payload.clone())
    .bind(&bytes)
    .bind(score)
    .fetch_one(&mut *connection)
    .await?;
    assert_eq!(
        row,
        (
            identifier,
            calendar_date,
            instant,
            numeric,
            counter,
            payload,
            bytes,
            score,
        )
    );
    drop(connection);

    let rolled_back: Result<(), DatabaseError> = database
        .transaction(|connection| {
            Box::pin(async move {
                sqlx::query("CREATE TEMP TABLE s05_rollback_probe(value integer)")
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                sqlx::query("INSERT INTO s05_rollback_probe(value) VALUES (1)")
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                Err(DatabaseError::QueryFailed)
            })
        })
        .await;
    assert_eq!(rolled_back, Err(DatabaseError::QueryFailed));

    let mut connection = database
        .acquire()
        .await
        .map_err(|_| FixtureError("post_rollback_acquire"))?;
    let rolled_back_table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('pg_temp.s05_rollback_probe')::text")
            .fetch_one(&mut *connection)
            .await?;
    assert_eq!(rolled_back_table, None);

    let unavailable = sqlx::query(
        "DO $probe$ BEGIN RAISE EXCEPTION USING ERRCODE = 'P0E01', MESSAGE = 'private'; END $probe$",
    )
    .execute(&mut *connection)
    .await
    .expect_err("the synthetic PostgreSQL error must be raised");
    let unavailable = map_sqlx_error(unavailable);
    assert_eq!(unavailable, DatabaseError::AccountUnavailable);
    assert_eq!(unavailable.sqlstate(), Some("P0E01"));

    drop(connection);
    assert_eq!(database.pool_stats().max, 1);
    database.close().await;
    Ok(())
}
