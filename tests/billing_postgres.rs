#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use histae_api_rust::billing::domain::{
    BeginCheckoutResult, BillingPeriod, PersistedCheckoutSession,
};
use histae_api_rust::billing::pg::BeginCheckoutInput;
use histae_api_rust::billing::pg::{BillingStore, PgBillingRepository};
use histae_api_rust::billing::service::{BillingError, BillingService};
use histae_api_rust::billing::stripe::{
    CheckoutInput, StripeCheckoutSession, StripeCustomer, StripeError, StripeFuture, StripeGateway,
    StripePortalSession,
};
use histae_api_rust::config::{BillingConfig, BillingProvider, PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::infra::postgres_locks::AccountActivityPool;
use histae_api_rust::shared::clock::SystemClock;
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "S20 integration fixture error ({})", self.0)
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
        application_name: "histae-rust-s20-integration",
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
    .bind(format!("s20-{user_id}"))
    .bind(Vec::<u8>::new())
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

async fn cleanup(database: &Database, users: &[Uuid]) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM user_account WHERE user_id = ANY($1::uuid[])")
        .bind(users)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

fn checkout_window() -> (DateTime<Utc>, DateTime<Utc>, DateTime<Utc>) {
    let now = DateTime::from_timestamp_micros(Utc::now().timestamp_micros())
        .unwrap_or_else(|| unreachable!());
    (
        now,
        now + TimeDelta::minutes(30),
        now - TimeDelta::seconds(60),
    )
}

#[derive(Clone, Default)]
struct CountingStripe {
    create_customer_calls: Arc<AtomicUsize>,
}

impl StripeGateway for CountingStripe {
    fn create_customer(
        &self,
        _user_id: Uuid,
        _attempt_id: Uuid,
        _idempotency_key: String,
    ) -> StripeFuture<'_, StripeCustomer> {
        self.create_customer_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(StripeError::Rejected) })
    }

    fn create_checkout_session(
        &self,
        _input: CheckoutInput,
    ) -> StripeFuture<'_, StripeCheckoutSession> {
        Box::pin(async { Err(StripeError::Rejected) })
    }

    fn expire_checkout_session(&self, _session_id: &str) -> StripeFuture<'_, ()> {
        Box::pin(async { Err(StripeError::Rejected) })
    }

    fn create_portal_session(
        &self,
        _customer_id: &str,
        _idempotency_key: String,
    ) -> StripeFuture<'_, StripePortalSession> {
        Box::pin(async { Err(StripeError::Rejected) })
    }

    fn delete_customer(
        &self,
        _customer_id: &str,
        _idempotency_key: String,
    ) -> StripeFuture<'_, StripeCustomer> {
        Box::pin(async { Err(StripeError::Rejected) })
    }

    fn retrieve_customer(&self, _customer_id: &str) -> StripeFuture<'_, StripeCustomer> {
        Box::pin(async { Err(StripeError::Rejected) })
    }
}

fn billing_config() -> BillingConfig {
    BillingConfig {
        provider: BillingProvider::Stripe,
        stripe_secret_key: SecretString::new("test-secret".to_owned()),
        stripe_webhook_secret: SecretString::new("test-webhook-secret".to_owned()),
        premium_product_id: "prod_contract".to_owned(),
        premium_monthly_price_id: "price_monthly_contract".to_owned(),
        premium_annual_price_id: "price_annual_contract".to_owned(),
        checkout_success_url: Some("https://app.histae.test/success".to_owned()),
        checkout_cancel_url: Some("https://app.histae.test/cancel".to_owned()),
        portal_return_url: Some("https://app.histae.test/portal".to_owned()),
        automatic_tax: false,
        allow_promotion_codes: false,
        timeout: Duration::from_secs(2),
        max_network_retries: 0,
        reconciliation_interval: Duration::from_secs(300),
        reconciliation_freshness: Duration::from_secs(3_600),
        reconciliation_batch_size: 25,
    }
}

#[tokio::test]
async fn customer_intent_and_watchdog_are_durable_before_the_provider_call()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let repository = PgBillingRepository::new(database.clone());
    let user_id = Uuid::new_v4();
    cleanup(&database, &[user_id]).await?;
    insert_user(&database, user_id).await?;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let key = Uuid::new_v4();
        let attempt_id = Uuid::new_v4();
        let (now, expires_at, stale_before) = checkout_window();
        let attempt = repository
            .begin_checkout(BeginCheckoutInput {
                user_id,
                idempotency_key: key,
                billing_period: BillingPeriod::Monthly,
                attempt_id,
                now,
                expires_at,
                stale_before,
            })
            .await?;
        let context = match attempt {
            BeginCheckoutResult::Created(context) => context,
            other => panic!("unexpected checkout outcome: {other:?}"),
        };
        assert_eq!(context.attempt_id, attempt_id);
        assert!(context.stripe_customer_id.is_none());

        let creation = repository.begin_customer_creation(attempt_id).await?;
        assert_eq!(creation.id, attempt_id);
        assert_eq!(creation.user_id, user_id);
        let mut connection = database.acquire().await?;
        let watchdog: (String, DateTime<Utc>) = sqlx::query_as(
            "SELECT status, available_at FROM outbox_event
             WHERE event_type = 'billing.customer.reconcile' AND aggregate_id = $1",
        )
        .bind(attempt_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
        assert_eq!(watchdog.0, "pending");
        let delay = watchdog.1 - creation.customer_creation_started_at;
        assert_eq!(delay.num_hours(), 23);
        drop(connection);

        let customer_id = format!("cus_{}", Uuid::new_v4().simple());
        repository
            .record_created_customer(attempt_id, customer_id.clone())
            .await?;
        assert!(
            repository
                .save_customer(user_id, customer_id.clone())
                .await?
        );
        assert_eq!(
            repository.customer_for_user(user_id).await?,
            Some(customer_id)
        );
        let mut connection = database.acquire().await?;
        let watchdog_count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM outbox_event
             WHERE event_type = 'billing.customer.reconcile' AND aggregate_id = $1",
        )
        .bind(attempt_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
        assert_eq!(watchdog_count, 0);
        Ok(())
    }
    .await;

    cleanup(&database, &[user_id]).await?;
    database.close().await;
    result
}

#[tokio::test]
async fn checkout_replay_conflicts_and_unresolved_customer_rules_match_nest()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let repository = PgBillingRepository::new(database.clone());
    let replay_user = Uuid::new_v4();
    let unresolved_user = Uuid::new_v4();
    let missing_user = Uuid::new_v4();
    let users = [replay_user, unresolved_user, missing_user];
    cleanup(&database, &users).await?;
    insert_user(&database, replay_user).await?;
    insert_user(&database, unresolved_user).await?;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let replay_key = Uuid::new_v4();
        let replay_attempt = Uuid::new_v4();
        let (now, expires_at, stale_before) = checkout_window();
        assert!(matches!(
            repository
                .begin_checkout(BeginCheckoutInput {
                    user_id: replay_user,
                    idempotency_key: replay_key,
                    billing_period: BillingPeriod::Annual,
                    attempt_id: replay_attempt,
                    now,
                    expires_at,
                    stale_before,
                })
                .await?,
            BeginCheckoutResult::Created(_)
        ));
        let session = PersistedCheckoutSession {
            session_id: format!("cs_test_{}", Uuid::new_v4().simple()),
            url: "https://checkout.stripe.test/session".to_owned(),
            expires_at,
        };
        assert!(
            repository
                .mark_checkout_open(replay_attempt, session.clone())
                .await?
        );
        assert_eq!(
            repository
                .begin_checkout(BeginCheckoutInput {
                    user_id: replay_user,
                    idempotency_key: replay_key,
                    billing_period: BillingPeriod::Annual,
                    attempt_id: Uuid::new_v4(),
                    now,
                    expires_at,
                    stale_before,
                })
                .await?,
            BeginCheckoutResult::Replay(session)
        );
        assert_eq!(
            repository
                .begin_checkout(BeginCheckoutInput {
                    user_id: replay_user,
                    idempotency_key: replay_key,
                    billing_period: BillingPeriod::Monthly,
                    attempt_id: Uuid::new_v4(),
                    now,
                    expires_at,
                    stale_before,
                })
                .await?,
            BeginCheckoutResult::IdempotencyConflict
        );

        let unresolved_key = Uuid::new_v4();
        let unresolved_attempt = Uuid::new_v4();
        assert!(matches!(
            repository
                .begin_checkout(BeginCheckoutInput {
                    user_id: unresolved_user,
                    idempotency_key: unresolved_key,
                    billing_period: BillingPeriod::Monthly,
                    attempt_id: unresolved_attempt,
                    now,
                    expires_at,
                    stale_before,
                })
                .await?,
            BeginCheckoutResult::Created(_)
        ));
        repository
            .begin_customer_creation(unresolved_attempt)
            .await?;
        repository.mark_checkout_failed(unresolved_attempt).await?;
        assert_eq!(
            repository
                .begin_checkout(BeginCheckoutInput {
                    user_id: unresolved_user,
                    idempotency_key: Uuid::new_v4(),
                    billing_period: BillingPeriod::Monthly,
                    attempt_id: Uuid::new_v4(),
                    now,
                    expires_at,
                    stale_before,
                })
                .await?,
            BeginCheckoutResult::CustomerReconciliationRequired
        );
        assert_eq!(
            repository
                .begin_checkout(BeginCheckoutInput {
                    user_id: missing_user,
                    idempotency_key: Uuid::new_v4(),
                    billing_period: BillingPeriod::Monthly,
                    attempt_id: Uuid::new_v4(),
                    now,
                    expires_at,
                    stale_before,
                })
                .await?,
            BeginCheckoutResult::NotFound
        );
        Ok(())
    }
    .await;

    cleanup(&database, &users).await?;
    database.close().await;
    result
}

#[tokio::test]
async fn old_uncertain_customer_creation_never_replays_the_provider_post()
-> Result<(), Box<dyn std::error::Error>> {
    let config = postgres_config()?;
    let database = Database::connect(&config).await?;
    let activity = AccountActivityPool::connect(&config).await?;
    let repository = PgBillingRepository::new(database.clone());
    let user_id = Uuid::new_v4();
    cleanup(&database, &[user_id]).await?;
    insert_user(&database, user_id).await?;

    let stripe = CountingStripe::default();
    let calls = Arc::clone(&stripe.create_customer_calls);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let attempt_id = Uuid::new_v4();
        let (now, expires_at, stale_before) = checkout_window();
        assert!(matches!(
            repository
                .begin_checkout(BeginCheckoutInput {
                    user_id,
                    idempotency_key: Uuid::new_v4(),
                    billing_period: BillingPeriod::Monthly,
                    attempt_id,
                    now,
                    expires_at,
                    stale_before,
                })
                .await?,
            BeginCheckoutResult::Created(_)
        ));
        repository.begin_customer_creation(attempt_id).await?;
        let mut connection = database.acquire().await?;
        sqlx::query(
            "UPDATE billing_checkout_session
             SET customer_creation_started_at = clock_timestamp() - interval '24 hours'
             WHERE id = $1",
        )
        .bind(attempt_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
        drop(connection);

        let service = BillingService::new(
            Arc::new(repository.clone()),
            Arc::new(stripe),
            billing_config(),
            activity.clone(),
            Arc::new(SystemClock),
        );
        assert_eq!(
            service.delete_customer_for_account(user_id).await,
            Err(BillingError::ErasureStripeReconciliationRequired)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        Ok(())
    }
    .await;

    cleanup(&database, &[user_id]).await?;
    activity.close().await;
    database.close().await;
    result
}
