#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use histae_api_rust::billing::domain::{BillingPeriod, StripeSubscriptionStatus};
use histae_api_rust::billing::reconcile::{
    BillingReconciliationStore, CustomerRecoveryResult, PgBillingReconciliationStore,
    ReconciliationApplyState, ReconciliationKind, SubscriptionContext,
};
use histae_api_rust::billing::webhook::SubscriptionProjection;
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PostgreSQL S21 fixture error ({})", self.0)
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
        max_connections: 4,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        statement_timeout: Duration::from_secs(15),
        idle_transaction_timeout: Duration::from_secs(30),
        application_name: "histae-rust-s21-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS").ok().map(PathBuf::from),
    })
}

async fn insert_user(database: &Database, user_id: Uuid) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO user_account
           (user_id, role, phone_number_hash, phone_number_encrypted)
         VALUES ($1, 'user', $2, $3)",
    )
    .bind(user_id)
    .bind(format!("s21-{user_id}"))
    .bind(Vec::<u8>::new())
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

async fn cleanup(
    database: &Database,
    user_id: Uuid,
    event_ids: &[Uuid],
) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM outbox_event WHERE id = ANY($1::uuid[])")
        .bind(event_ids)
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
async fn stale_provider_snapshots_do_not_overwrite_newer_projection()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let store = PgBillingReconciliationStore::new(database.clone());
    let user_id = Uuid::new_v4();
    cleanup(&database, user_id, &[]).await?;
    insert_user(&database, user_id).await?;
    let now = Utc::now();
    let customer_id = format!("cus_{}", Uuid::new_v4().simple());
    let subscription_id = format!("sub_{}", Uuid::new_v4().simple());
    let price_id = format!("price_{}", Uuid::new_v4().simple());
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let mut connection = database.acquire().await?;
        sqlx::query("INSERT INTO billing_customer (user_id, stripe_customer_id) VALUES ($1, $2)")
            .bind(user_id)
            .bind(&customer_id)
            .execute(&mut *connection)
            .await?;
        sqlx::query(
            "INSERT INTO user_subscription
               (user_id, plan, provider, provider_subscription_id, provider_price_id,
                billing_period, status, current_period_starts_at, current_period_ends_at,
                projection_version, provider_snapshot_at)
             VALUES ($1, 'premium', 'stripe', $2, $3, 'monthly', 'active',
                     $4, $5, 2, $6)",
        )
        .bind(user_id)
        .bind(&subscription_id)
        .bind(&price_id)
        .bind(now - TimeDelta::days(1))
        .bind(now + TimeDelta::days(30))
        .bind(now + TimeDelta::seconds(10))
        .execute(&mut *connection)
        .await?;
        drop(connection);

        let attempted = SubscriptionProjection {
            metadata_user_id: Some(user_id),
            stripe_customer_id: customer_id.clone(),
            stripe_subscription_id: subscription_id.clone(),
            stripe_price_id: price_id.clone(),
            billing_period: BillingPeriod::Monthly,
            status: StripeSubscriptionStatus::Canceled,
            cancel_at_period_end: false,
            current_period_starts_at: now - TimeDelta::days(1),
            current_period_ends_at: now + TimeDelta::days(30),
            trial_starts_at: None,
            trial_ends_at: None,
            canceled_at: Some(now),
            event_created_at: now,
        };
        let applied = store
            .apply_subscription(
                SubscriptionContext {
                    user_id,
                    stripe_customer_id: customer_id.clone(),
                    projection_version: Some(1),
                },
                Some(attempted),
                now,
                now + TimeDelta::hours(1),
                false,
            )
            .await?;
        assert_eq!(applied.state, ReconciliationApplyState::Stale);
        assert_eq!(applied.status, Some(StripeSubscriptionStatus::Active));
        let mut connection = database.acquire().await?;
        let stored: (String, i64) = sqlx::query_as(
            "SELECT status, projection_version FROM user_subscription WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&mut *connection)
        .await?;
        assert_eq!(stored, ("active".to_owned(), 2));
        Ok(())
    }
    .await;
    cleanup(&database, user_id, &[]).await?;
    database.close().await;
    result
}

#[tokio::test]
async fn dead_letter_listing_exposes_only_operational_metadata_and_customer_watchdog_clears()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let store = PgBillingReconciliationStore::new(database.clone());
    let user_id = Uuid::new_v4();
    let attempt_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();
    cleanup(&database, user_id, &[event_id]).await?;
    insert_user(&database, user_id).await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let mut connection = database.acquire().await?;
        sqlx::query(
            "INSERT INTO billing_checkout_session
               (id, user_id, idempotency_key, billing_period, status, expires_at,
                customer_creation_started_at)
             VALUES ($1, $2, $3, 'annual', 'creating', $4, $5)",
        )
        .bind(attempt_id)
        .bind(user_id)
        .bind(Uuid::new_v4())
        .bind(Utc::now() + TimeDelta::minutes(30))
        .bind(Utc::now() - TimeDelta::hours(24))
        .execute(&mut *connection)
        .await?;
        sqlx::query(
            "INSERT INTO outbox_event
               (id, event_type, aggregate_id, payload, status, attempts,
                last_error_code, dead_lettered_at)
             VALUES ($1, 'billing.customer.reconcile', $2,
                     jsonb_build_object('provider_customer_id', $3::text),
                     'dead_letter', 10, 'billing_provider_unavailable', clock_timestamp())",
        )
        .bind(event_id)
        .bind(attempt_id)
        .bind(format!("cus_{}", Uuid::new_v4().simple()))
        .execute(&mut *connection)
        .await?;
        drop(connection);

        let rows = store
            .list(Some(ReconciliationKind::CustomerCreation), 20, None)
            .await?;
        let row = rows
            .iter()
            .find(|row| row.event_id == event_id)
            .ok_or(FixtureError("dead_letter_row"))?;
        assert_eq!(row.user_id, user_id);
        assert_eq!(row.kind, ReconciliationKind::CustomerCreation);
        assert_eq!(
            row.last_error_code.as_deref(),
            Some("billing_provider_unavailable")
        );

        assert_eq!(
            store.recover_customer_creation(attempt_id, None).await?,
            CustomerRecoveryResult::Cleared
        );
        let mut connection = database.acquire().await?;
        let state: (String, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
            "SELECT status, customer_creation_started_at
             FROM billing_checkout_session WHERE id = $1",
        )
        .bind(attempt_id)
        .fetch_one(&mut *connection)
        .await?;
        assert_eq!(state, ("failed".to_owned(), None));
        Ok(())
    }
    .await;
    cleanup(&database, user_id, &[event_id]).await?;
    database.close().await;
    result
}
