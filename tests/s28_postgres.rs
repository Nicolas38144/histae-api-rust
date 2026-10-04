#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use histae_api_rust::administration::metrics::{
    AdminMetricsStore, PgAdminMetricsRepository, RevenuePeriod,
};
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "S28 PostgreSQL fixture error ({})", self.0)
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
        port: variable("POSTGRES_PORT")?
            .parse()
            .map_err(|_| FixtureError("POSTGRES_PORT"))?,
        user: variable("POSTGRES_USER")?,
        password: SecretString::new(variable("POSTGRES_PASSWORD")?),
        database,
        tls: false,
        max_connections: 4,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        statement_timeout: Duration::from_secs(15),
        idle_transaction_timeout: Duration::from_secs(30),
        application_name: "histae-rust-s28-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
    })
}

async fn insert_premium_account(database: &Database, user_id: Uuid) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO user_account
         (user_id, role, phone_number_hash, phone_number_encrypted)
         VALUES ($1, 'user', $2, $3)",
    )
    .bind(user_id)
    .bind(format!("s28-{user_id}"))
    .bind(Vec::<u8>::new())
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query("INSERT INTO user_subscription (user_id, plan) VALUES ($1, 'premium')")
        .bind(user_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

async fn cleanup(database: &Database, user_id: Uuid) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM user_account WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

#[tokio::test]
async fn admin_metrics_and_revenue_execute_against_the_real_schema()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?)
        .await
        .map_err(|error| FixtureError(error.safe_code()))?;
    let user_id = Uuid::new_v4();
    cleanup(&database, user_id).await?;
    insert_premium_account(&database, user_id).await?;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let repository = PgAdminMetricsRepository::new(
            database.clone(),
            env::var("TERMS_OF_SERVICE_VERSION")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "development-unversioned".to_owned()),
            env::var("PRIVACY_POLICY_VERSION")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "development-unversioned".to_owned()),
        );
        let revenue = repository.revenue(RevenuePeriod::Last7Days).await?;
        assert_eq!(revenue.period, RevenuePeriod::Last7Days);
        assert!(revenue.period_start.is_some());
        assert!(revenue.premium_subscriptions >= 1);
        assert_eq!(revenue.price_per_subscription_cents, 500);
        assert_eq!(
            revenue.estimated_revenue_cents,
            revenue.premium_subscriptions * i64::from(revenue.price_per_subscription_cents)
        );
        assert_eq!(revenue.currency, "EUR");

        let metrics = repository.metrics(RevenuePeriod::MonthToDate).await?;
        assert!(metrics.users.total >= 1);
        assert!(metrics.users.active >= 1);
        assert!(
            metrics
                .subscriptions
                .iter()
                .any(|entry| entry.plan == "premium" && entry.users >= 1)
        );
        assert_eq!(metrics.revenue.period, RevenuePeriod::MonthToDate);
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, user_id).await;
    result?;
    cleanup_result?;
    Ok(())
}
