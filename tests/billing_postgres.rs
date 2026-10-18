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
use histae_api_rust::billing::pg::PgBillingRepository;
use histae_api_rust::billing::service::{BillingError, BillingService};
use histae_api_rust::billing::store::BeginCheckoutInput;
use histae_api_rust::billing::store::BillingStore;
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
                .mark_checkout_open(replay_attempt, 1, session.clone())
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
        repository
            .mark_checkout_failed(unresolved_attempt, 1, None)
            .await?;
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

#[derive(Clone)]
struct MutableClock(Arc<std::sync::Mutex<DateTime<Utc>>>);
impl histae_api_rust::shared::clock::Clock for MutableClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().expect("clock")
    }
}

#[derive(Clone)]
struct RecordingStripe {
    inputs: Arc<std::sync::Mutex<Vec<CheckoutInput>>>,
    current_subscription: Arc<std::sync::atomic::AtomicBool>,
    clock: MutableClock,
}
impl StripeGateway for RecordingStripe {
    fn has_current_subscription(&self, _: &str) -> StripeFuture<'_, bool> {
        let value = self.current_subscription.load(Ordering::SeqCst);
        Box::pin(async move { Ok(value) })
    }
    fn create_customer(&self, _: Uuid, _: Uuid, _: String) -> StripeFuture<'_, StripeCustomer> {
        *self.clock.0.lock().expect("clock") += TimeDelta::minutes(2);
        Box::pin(async {
            Ok(StripeCustomer {
                id: format!("cus_{}", Uuid::new_v4().simple()),
                deleted: false,
            })
        })
    }
    fn create_checkout_session(
        &self,
        input: CheckoutInput,
    ) -> StripeFuture<'_, StripeCheckoutSession> {
        let mut inputs = self.inputs.lock().expect("requests");
        let result = if inputs.is_empty() {
            Err(StripeError::Network)
        } else if inputs[0] != input {
            Err(StripeError::Rejected)
        } else {
            Ok(StripeCheckoutSession {
                id: format!("cs_test_{}", Uuid::new_v4().simple()),
                url: Some("https://checkout.stripe.test/session".into()),
                expires_at: input.expires_at,
            })
        };
        inputs.push(input);
        Box::pin(async move { result })
    }
    fn expire_checkout_session(&self, _: &str) -> StripeFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn create_portal_session(&self, _: &str, _: String) -> StripeFuture<'_, StripePortalSession> {
        Box::pin(async { Err(StripeError::Rejected) })
    }
    fn delete_customer(&self, _: &str, _: String) -> StripeFuture<'_, StripeCustomer> {
        Box::pin(async { Err(StripeError::Rejected) })
    }
    fn retrieve_customer(&self, _: &str) -> StripeFuture<'_, StripeCustomer> {
        Box::pin(async { Err(StripeError::Rejected) })
    }
}

#[tokio::test]
async fn uncertain_checkout_keeps_the_exact_request_and_blocks_parallel_attempts()
-> Result<(), Box<dyn std::error::Error>> {
    use histae_api_rust::shared::clock::Clock;
    let config = postgres_config()?;
    let database = Database::connect(&config).await?;
    let activity = AccountActivityPool::connect(&config).await?;
    let repository = PgBillingRepository::new(database.clone());
    let user_id = Uuid::new_v4();
    insert_user(&database, user_id).await?;
    let clock = MutableClock(Arc::new(std::sync::Mutex::new(
        DateTime::from_timestamp(Utc::now().timestamp(), 0).expect("clock"),
    )));
    let provider = RecordingStripe {
        inputs: Default::default(),
        current_subscription: Default::default(),
        clock: clock.clone(),
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let service = BillingService::new(
            Arc::new(repository.clone()),
            Arc::new(provider.clone()),
            billing_config(),
            activity.clone(),
            Arc::new(clock.clone()),
        );
        let key = Uuid::new_v4().to_string();
        assert_eq!(
            service
                .create_checkout(user_id, BillingPeriod::Monthly, Some(&key))
                .await,
            Err(BillingError::StripeRequestFailed)
        );
        let original = provider.inputs.lock().expect("request")[0].clone();
        assert_eq!(original.trial_days, 0);
        assert_eq!(original.expires_at - clock.now(), TimeDelta::minutes(35));
        assert_eq!(
            service
                .create_checkout(
                    user_id,
                    BillingPeriod::Monthly,
                    Some(&Uuid::new_v4().to_string())
                )
                .await,
            Err(BillingError::CheckoutAlreadyInProgress)
        );
        let mut connection = database.acquire().await?;
        sqlx::query("UPDATE billing_customer SET trial_used_at=clock_timestamp() WHERE user_id=$1")
            .bind(user_id)
            .execute(&mut *connection)
            .await?;
        drop(connection);
        *clock.0.lock().expect("clock") += TimeDelta::minutes(2);
        let mut changed = billing_config();
        changed.premium_monthly_price_id = "price_Replaced".into();
        changed.checkout_success_url = Some("https://changed.histae.test/success".into());
        changed.automatic_tax = true;
        changed.allow_promotion_codes = true;
        let restarted = BillingService::new(
            Arc::new(repository.clone()),
            Arc::new(provider.clone()),
            changed,
            activity.clone(),
            Arc::new(clock.clone()),
        );
        let session = restarted
            .create_checkout(user_id, BillingPeriod::Monthly, Some(&key))
            .await?;
        assert_eq!(
            provider.inputs.lock().expect("requests").as_slice(),
            &[original.clone(), original.clone()]
        );
        assert_eq!(
            restarted
                .create_checkout(user_id, BillingPeriod::Monthly, Some(&key))
                .await?,
            session
        );
        assert_eq!(provider.inputs.lock().expect("requests").len(), 2);
        *clock.0.lock().expect("clock") = original.expires_at;
        assert_eq!(
            restarted
                .create_checkout(user_id, BillingPeriod::Monthly, Some(&key))
                .await,
            Err(BillingError::IdempotencyKeyConsumed)
        );
        Ok(())
    }
    .await;
    cleanup(&database, &[user_id]).await?;
    activity.close().await;
    database.close().await;
    result
}

#[tokio::test]
async fn nonterminal_stripe_subscriptions_block_checkout_even_without_premium_access()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let repository = PgBillingRepository::new(database.clone());
    let mut users = Vec::new();
    let result: Result<(), Box<dyn std::error::Error>> = async {
        for (status,expected) in [("active",true),("trialing",true),("past_due",true),("unpaid",true),("paused",true),("incomplete",true),("canceled",false),("incomplete_expired",false)] {
            let user_id = Uuid::new_v4();users.push(user_id);insert_user(&database,user_id).await?;
            let mut connection = database.acquire().await?;
            sqlx::query("INSERT INTO user_subscription(user_id,plan,provider,provider_subscription_id,status,current_period_ends_at) VALUES($1,'premium','stripe',$2,$3,clock_timestamp()-interval '1 day')")
                .bind(user_id).bind(format!("sub_{}",Uuid::new_v4().simple())).bind(status).execute(&mut *connection).await?;
            drop(connection);
            let (now,expires_at,stale_before) = checkout_window();
            let outcome = repository.begin_checkout(BeginCheckoutInput { user_id,idempotency_key:Uuid::new_v4(),billing_period:BillingPeriod::Monthly,attempt_id:Uuid::new_v4(),now,expires_at,stale_before }).await?;
            assert_eq!(matches!(outcome,BeginCheckoutResult::AlreadySubscribed),expected,"{status}");
        }
        Ok(())
    }.await;
    cleanup(&database, &users).await?;
    database.close().await;
    result
}

#[tokio::test]
async fn missing_webhooks_do_not_allow_a_second_provider_subscription()
-> Result<(), Box<dyn std::error::Error>> {
    let config = postgres_config()?;
    let database = Database::connect(&config).await?;
    let activity = AccountActivityPool::connect(&config).await?;
    let repository = PgBillingRepository::new(database.clone());
    let user_id = Uuid::new_v4();
    insert_user(&database, user_id).await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        assert!(
            repository
                .save_customer(user_id, format!("cus_{}", Uuid::new_v4().simple()))
                .await?
        );
        let provider = RecordingStripe {
            inputs: Default::default(),
            current_subscription: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            clock: MutableClock(Arc::new(std::sync::Mutex::new(Utc::now()))),
        };
        let service = BillingService::new(
            Arc::new(repository),
            Arc::new(provider.clone()),
            billing_config(),
            activity.clone(),
            Arc::new(SystemClock),
        );
        assert_eq!(
            service
                .create_checkout(
                    user_id,
                    BillingPeriod::Monthly,
                    Some(&Uuid::new_v4().to_string())
                )
                .await,
            Err(BillingError::SubscriptionAlreadyActive)
        );
        assert!(provider.inputs.lock().expect("requests").is_empty());
        Ok(())
    }
    .await;
    cleanup(&database, &[user_id]).await?;
    activity.close().await;
    database.close().await;
    result
}

#[tokio::test]
async fn live_checkout_owner_cannot_be_replaced_by_a_stale_timestamp()
-> Result<(), Box<dyn std::error::Error>> {
    let config = postgres_config()?;
    let database = Database::connect(&config).await?;
    let activity = AccountActivityPool::connect(&config).await?;
    let repository = PgBillingRepository::new(database.clone());
    let user_id = Uuid::new_v4();
    insert_user(&database, user_id).await?;
    let key = Uuid::new_v4();
    let attempt = Uuid::new_v4();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (resume, paused) = tokio::sync::oneshot::channel();
    let owner_repository = repository.clone();
    let owner_activity = activity.clone();
    let owner = tokio::spawn(async move {
        owner_activity
            .try_serialized(user_id, "billing.checkout", move |_| {
                Box::pin(async move {
                    let (now, expires_at, stale_before) = checkout_window();
                    owner_repository
                        .begin_checkout(BeginCheckoutInput {
                            user_id,
                            idempotency_key: key,
                            billing_period: BillingPeriod::Monthly,
                            attempt_id: attempt,
                            now,
                            expires_at,
                            stale_before,
                        })
                        .await
                        .map_err(|error| {
                            BillingError::Database(match error {
                                histae_api_rust::billing::store::BillingStoreError::Database(
                                    db,
                                ) => db,
                                _ => DatabaseError::QueryFailed,
                            })
                        })?;
                    started.send(()).expect("ready");
                    paused.await.expect("resume");
                    owner_repository
                        .mark_checkout_failed(attempt, 1, None)
                        .await
                        .map_err(|_| BillingError::Database(DatabaseError::QueryFailed))?;
                    Ok::<_, BillingError>(())
                })
            })
            .await
    });
    ready.await?;
    let result: Result<(), Box<dyn std::error::Error>>=async {
        let mut connection=database.acquire().await?;
        sqlx::query("UPDATE billing_checkout_session SET updated_at=clock_timestamp()-interval '2 minutes' WHERE id=$1").bind(attempt).execute(&mut *connection).await?;
        drop(connection);
        let provider=RecordingStripe {inputs:Default::default(),current_subscription:Default::default(),clock:MutableClock(Arc::new(std::sync::Mutex::new(Utc::now())))};
        let service=BillingService::new(Arc::new(repository),Arc::new(provider.clone()),billing_config(),activity.clone(),Arc::new(SystemClock));
        assert_eq!(service.create_checkout(user_id,BillingPeriod::Monthly,Some(&key.to_string())).await,Err(BillingError::CheckoutAlreadyInProgress));
        assert!(provider.inputs.lock().expect("requests").is_empty());
        activity.run(&[user_id],|lease|Box::pin(async move {lease.assert_held()?;Ok::<_,BillingError>(())})).await?;
        Ok(())
    }.await;
    let _ = resume.send(());
    owner.await??;
    cleanup(&database, &[user_id]).await?;
    activity.close().await;
    database.close().await;
    result
}

#[tokio::test]
async fn admin_subscription_badges_follow_effective_mobile_access()
-> Result<(), Box<dyn std::error::Error>> {
    use histae_api_rust::administration::{pg::PgAdministrationStore, store::AdministrationStore};
    let database = Database::connect(&postgres_config()?).await?;
    let admin = Uuid::new_v4();
    let user = Uuid::new_v4();
    insert_user(&database, admin).await?;
    insert_user(&database, user).await?;
    let result: Result<(),Box<dyn std::error::Error>>=async {
        let mut connection=database.acquire().await?;
        sqlx::query("UPDATE user_account SET role='admin' WHERE user_id=$1").bind(admin).execute(&mut *connection).await?;
        sqlx::query("INSERT INTO user_subscription(user_id,plan,provider,provider_subscription_id,status,current_period_ends_at) VALUES($1,'premium','stripe',$2,'canceled',clock_timestamp()+interval '30 days')")
            .bind(user).bind(format!("sub_{}",Uuid::new_v4().simple())).execute(&mut *connection).await?;
        drop(connection);
        let store=PgAdministrationStore::new(database.clone());
        for (status,expected) in [("canceled","free"),("active","premium"),("unpaid","free")] {
            let mut connection=database.acquire().await?;
            sqlx::query("UPDATE user_subscription SET status=$2 WHERE user_id=$1").bind(user).bind(status).execute(&mut *connection).await?;
            drop(connection);
            let rows=store.list_users(None,None,user.to_string(),1,0,None,"terms-v1".into(),"privacy-v1".into()).await?;
            assert_eq!(rows[0].plan,expected);
            let detail=store.user_detail(user,admin,histae_api_rust::identity::admin_role::AdminRole::Admin,"Billing consistency verification".into(),"terms-v1".into(),"privacy-v1".into()).await?.expect("detail");
            assert_eq!(detail.user.plan,expected);
        }
        Ok(())
    }.await;
    cleanup(&database, &[admin, user]).await?;
    database.close().await;
    result
}

#[tokio::test]
async fn replaced_checkout_owners_cannot_fail_or_expire_the_new_attempt()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let repository = PgBillingRepository::new(database.clone());
    let user_id = Uuid::new_v4();
    insert_user(&database, user_id).await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let (now, expires_at, stale_before) = checkout_window();
        let input = BeginCheckoutInput {
            user_id,
            idempotency_key: Uuid::new_v4(),
            billing_period: BillingPeriod::Monthly,
            attempt_id: Uuid::new_v4(),
            now,
            expires_at,
            stale_before,
        };
        let BeginCheckoutResult::Created(first) = repository.begin_checkout(input).await? else {
            panic!("created")
        };
        assert!(
            repository
                .mark_checkout_failed(first.attempt_id, first.creation_generation, None)
                .await?
        );
        let BeginCheckoutResult::Retry(second) = repository.begin_checkout(input).await? else {
            panic!("retry")
        };
        assert!(second.creation_generation > first.creation_generation);
        let session = PersistedCheckoutSession {
            session_id: format!("cs_test_{}", Uuid::new_v4().simple()),
            url: "https://checkout.stripe.test/session".into(),
            expires_at,
        };
        assert!(
            !repository
                .mark_checkout_failed(
                    first.attempt_id,
                    first.creation_generation,
                    Some(session.session_id.clone())
                )
                .await?
        );
        assert!(
            !repository
                .mark_checkout_open(first.attempt_id, first.creation_generation, session.clone())
                .await?
        );
        assert!(
            repository
                .mark_checkout_open(
                    second.attempt_id,
                    second.creation_generation,
                    session.clone()
                )
                .await?
        );
        assert_eq!(
            repository.begin_checkout(input).await?,
            BeginCheckoutResult::Replay(session)
        );
        Ok(())
    }
    .await;
    cleanup(&database, &[user_id]).await?;
    database.close().await;
    result
}
