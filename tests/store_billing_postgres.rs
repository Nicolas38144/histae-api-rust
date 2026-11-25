#![cfg(feature = "postgres-integration")]

#[path = "support/postgres_job_role.rs"]
mod job_role;

use chrono::{TimeDelta, Utc};
use histae_api_rust::identity::web::WebAccountRepository;
use histae_api_rust::{
    billing::{
        domain::{BillingPeriod, SubscriptionProvider},
        pg::PgBillingRepository,
        store::BillingStore,
        stores::{
            StoreBillingService, StoreError, StoreProvider, StoreSnapshot, StoreVerifier,
            pg::StoreRepository,
        },
    },
    config::{LegalConfig, PostgresConfig, SecretString, StoreBillingConfig},
    infra::postgres::{Database, DatabaseError, map_sqlx_error},
};
use std::{env, fmt, path::PathBuf, time::Duration};
use url::Url;
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

fn legal() -> LegalConfig {
    LegalConfig {
        document_dates: Default::default(),
        terms_version: "terms-v1".to_owned(),
        privacy_version: "privacy-v1".to_owned(),
        sensitive_data_consent_version: "sensitive-v1".to_owned(),
        location_consent_version: "location-v1".to_owned(),
        terms_url: Url::parse("https://histae.test/legal/terms").expect("terms URL"),
        privacy_url: Url::parse("https://histae.test/legal/privacy").expect("privacy URL"),
        sensitive_data_consent_url: Url::parse("https://histae.test/legal/sensitive")
            .expect("sensitive URL"),
        location_consent_url: Url::parse("https://histae.test/legal/location")
            .expect("location URL"),
        review_reference: "test-review".to_owned(),
    }
}

fn snapshot(provider: StoreProvider, binding: String, original: String) -> StoreSnapshot {
    StoreSnapshot {
        provider,
        original_id: original,
        transaction_id: Uuid::new_v4().to_string(),
        product_id: "premium".into(),
        base_plan_id: None,
        account_binding: binding,
        environment: "sandbox".into(),
        billing_period: BillingPeriod::Monthly,
        status: "active".into(),
        starts_at: Utc::now() - TimeDelta::hours(1),
        expires_at: Utc::now() + TimeDelta::days(30),
        auto_renew: true,
        acknowledgement_required: provider == StoreProvider::GooglePlay,
        linked_reference_hash: None,
    }
}

#[tokio::test]
async fn authenticated_notification_is_retained_and_retried_on_its_reserved_verifier_route()
-> Result<(), Box<dyn std::error::Error>> {
    use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    let postgres = postgres_config()?;
    let db = Database::connect(&postgres).await?;
    let worker_database = job_role::connect(&postgres, "outbox").await?;
    let maintenance = job_role::connect(&postgres, "maintenance").await?;
    let user_id = Uuid::new_v4();
    insert_user(&db, user_id).await?;
    let stores = StoreRepository::new(db.clone(), SecretString::new("01".repeat(32)));
    let provider = StoreProvider::AppStore;
    let event = Uuid::new_v4();
    let snapshot = snapshot(
        provider,
        stores.account(user_id).await?.id.to_string(),
        "100".into(),
    );
    let attempts = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/notification/app_store",
            post(move || async move {
                Json(serde_json::json!({ "event_id": event.to_string(), "reference": "100" }))
            }),
        )
        .route(
            "/verify-notification/app_store",
            post({
                let attempts = attempts.clone();
                move || async move {
                    if attempts.fetch_add(1, Ordering::Relaxed) == 0 {
                        StatusCode::SERVICE_UNAVAILABLE.into_response()
                    } else {
                        Json(snapshot).into_response()
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let config = StoreBillingConfig {
        apple_enabled: true,
        google_enabled: false,
        apple_monthly_product: "premium".into(),
        apple_annual_product: "premium.annual".into(),
        google_product: String::new(),
        google_monthly_plan: String::new(),
        google_annual_plan: String::new(),
        verifier_url: Url::parse(&format!("http://{}/", listener.local_addr()?))?,
        verifier_ca_file: None,
        verifier_token: SecretString::new("synthetic-verifier-token".into()),
        production: false,
    };
    let service = StoreBillingService::new(
        stores.clone(),
        Arc::new(StoreVerifier::new(config.clone())?),
        config.clone(),
    );
    let worker = StoreBillingService::new(
        StoreRepository::new(worker_database, SecretString::new("01".repeat(32))),
        Arc::new(StoreVerifier::new(config.clone())?),
        config,
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let result: Result<(), Box<dyn std::error::Error>> = async {
        service.notification(provider, serde_json::json!({}), None).await?;
        let mut connection = db.acquire().await?;
        let id: Uuid = sqlx::query_scalar(
            "SELECT n.id FROM billing_store_notification n JOIN outbox_event o
             ON o.event_type='billing.store.notification' AND o.aggregate_id=n.id
             WHERE n.provider=$1 AND n.provider_event_id=$2",
        ).bind(provider.as_str()).bind(event.to_string()).fetch_one(&mut *connection).await?;
        let delivery_kind = "billing.store.notification";
        assert_eq!(worker.process(delivery_kind, id).await, Err(StoreError::Unavailable));
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert!(stores.reference(id, true).await?.is_some());

        let retry_started = std::time::Instant::now();
        worker.process(delivery_kind, id).await?;
        let retry_elapsed = retry_started.elapsed();
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
        assert!(stores.reference(id, true).await?.is_none());
        let subscription_id: Uuid = sqlx::query_scalar(
            "UPDATE billing_store_subscription SET next_check_at = clock_timestamp()
             WHERE user_id = $1 RETURNING id",
        )
        .bind(user_id)
        .fetch_one(&mut *connection)
        .await?;
        assert!(StoreRepository::schedule(&maintenance).await? >= 1);
        let scheduled: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM outbox_event WHERE event_type = 'billing.store.reconcile'
             AND aggregate_id = $1 AND status = 'pending')",
        )
        .bind(subscription_id)
        .fetch_one(&mut *connection)
        .await?;
        assert!(scheduled);
        sqlx::query("DELETE FROM outbox_event WHERE event_type = 'billing.store.reconcile' AND aggregate_id = $1")
            .bind(subscription_id)
            .execute(&mut *connection)
            .await?;

        let elapsed_seconds: f64 = sqlx::query_scalar(
            "SELECT extract(epoch FROM processed_at - received_at)::float8
             FROM billing_store_notification WHERE id=$1",
        ).bind(id).fetch_one(&mut *connection).await?;
        println!("Synthetic Apple notification: durable receipt to processing {elapsed_seconds:.3} s, retry {:.3} s", retry_elapsed.as_secs_f64());
        Ok(())
    }.await;
    server.abort();
    let mut connection = db.acquire().await?;
    sqlx::query(
        "DELETE FROM outbox_event o USING billing_store_notification n
         WHERE o.event_type='billing.store.notification' AND o.aggregate_id=n.id
           AND n.provider=$1 AND n.provider_event_id=$2",
    )
    .bind(provider.as_str())
    .bind(event.to_string())
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "DELETE FROM billing_store_notification WHERE provider=$1 AND provider_event_id=$2",
    )
    .bind(provider.as_str())
    .bind(event.to_string())
    .execute(&mut *connection)
    .await?;
    drop(connection);
    cleanup(&db, &[user_id]).await?;
    result
}

#[tokio::test]
async fn providers_ownership_ordering_ack_and_erasure_are_independent()
-> Result<(), Box<dyn std::error::Error>> {
    let db = Database::connect(&postgres_config()?).await?;
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    insert_user(&db, first).await?;
    insert_user(&db, second).await?;
    let stores = StoreRepository::new(db.clone(), SecretString::new("01".repeat(32)));
    let account = stores.account(first).await?;
    let original = Uuid::new_v4().to_string();
    let apple = snapshot(
        StoreProvider::AppStore,
        account.id.to_string(),
        original.clone(),
    );
    let observed = stores.observed_at().await?;
    assert_eq!(
        stores
            .apply(
                apple.clone(),
                "apple-reference".into(),
                Some(second),
                observed
            )
            .await,
        Err(StoreError::Ownership)
    );
    assert_eq!(
        stores
            .apply(
                apple.clone(),
                "apple-reference".into(),
                Some(first),
                observed
            )
            .await?,
        Some(first)
    );
    let billing = PgBillingRepository::new(db.clone());
    let subscription = billing
        .effective_subscription_for_user(first, Utc::now())
        .await?
        .unwrap();
    assert!(subscription.access_granted);
    assert_eq!(subscription.provider, Some(SubscriptionProvider::AppStore));
    assert_eq!(subscription.billing_period, Some(BillingPeriod::Monthly));
    assert_eq!(
        subscription.management_url.as_deref(),
        Some("https://apps.apple.com/account/subscriptions")
    );
    // A non-commercial free row must not override native access.
    let mut c = db.acquire().await?;
    sqlx::query("INSERT INTO user_subscription(user_id,plan) VALUES($1,'free')")
        .bind(first)
        .execute(&mut *c)
        .await?;
    drop(c);
    assert!(
        billing
            .effective_subscription_for_user(first, Utc::now())
            .await?
            .unwrap()
            .access_granted
    );
    let mut revoked = apple.clone();
    revoked.status = "revoked".into();
    let newer = observed + TimeDelta::seconds(2);
    stores
        .apply(revoked, "apple-reference".into(), Some(first), newer)
        .await?;
    assert_eq!(
        stores
            .apply(apple, "apple-reference".into(), Some(first), observed)
            .await?,
        None
    );
    assert!(
        !billing
            .effective_subscription_for_user(first, Utc::now())
            .await?
            .unwrap()
            .access_granted
    );
    let google = snapshot(
        StoreProvider::GooglePlay,
        account.google_id.clone(),
        Uuid::new_v4().to_string(),
    );
    stores
        .apply(
            google.clone(),
            "synthetic-play-token".into(),
            Some(first),
            newer,
        )
        .await?;
    let subscription = billing
        .effective_subscription_for_user(first, Utc::now())
        .await?
        .unwrap();
    assert!(subscription.access_granted);
    assert_eq!(
        subscription.provider,
        Some(SubscriptionProvider::GooglePlay)
    );
    assert_eq!(
        subscription.management_url.as_deref(),
        Some("https://play.google.com/store/account/subscriptions")
    );
    let mut c = db.acquire().await?;
    let (id,encrypted):(Uuid,Vec<u8>)=sqlx::query_as("SELECT id,reference_encrypted FROM billing_store_subscription WHERE provider='google_play' AND user_id=$1").bind(first).fetch_one(&mut *c).await?;
    assert_ne!(encrypted, b"synthetic-play-token");
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM outbox_event WHERE event_type='billing.store.acknowledge' AND aggregate_id=$1").bind(id).fetch_one(&mut *c).await?;
    assert_eq!(count, 1);
    sqlx::query("UPDATE outbox_event SET status='completed',processed_at=clock_timestamp() WHERE event_type='billing.store.acknowledge' AND aggregate_id=$1").bind(id).execute(&mut *c).await?;
    drop(c);
    stores
        .apply(
            google.clone(),
            "synthetic-play-token".into(),
            Some(first),
            newer + TimeDelta::seconds(1),
        )
        .await?;
    let mut c = db.acquire().await?;
    let status:String=sqlx::query_scalar("SELECT status FROM outbox_event WHERE event_type='billing.store.acknowledge' AND aggregate_id=$1").bind(id).fetch_one(&mut *c).await?;
    assert_eq!(status, "pending");
    drop(c);
    // Replaying a replaced Play token can never restore its old entitlement,
    // even when the successor is revoked and the old verification starts later.
    let mut successor = snapshot(
        StoreProvider::GooglePlay,
        account.google_id.clone(),
        Uuid::new_v4().to_string(),
    );
    successor.linked_reference_hash = Some(google.original_id.clone());
    successor.status = "revoked".into();
    stores
        .apply(
            successor,
            "successor-play-token".into(),
            Some(first),
            newer + TimeDelta::seconds(3),
        )
        .await?;
    assert_eq!(
        stores
            .apply(
                google,
                "synthetic-play-token".into(),
                Some(first),
                newer + TimeDelta::seconds(5)
            )
            .await?,
        None
    );
    assert!(
        !billing
            .effective_subscription_for_user(first, Utc::now())
            .await?
            .unwrap()
            .access_granted
    );
    let mut c = db.acquire().await?;
    // Anonymization removes session links and encrypted tokens, keeps a replay tombstone.
    sqlx::query("UPDATE user_account SET deleted_at=clock_timestamp() WHERE user_id=$1")
        .bind(first)
        .execute(&mut *c)
        .await?;
    sqlx::query("SELECT fct_anonymize_user($1)")
        .bind(first)
        .execute(&mut *c)
        .await?;
    let detached:bool=sqlx::query_scalar("SELECT user_id IS NULL AND octet_length(reference_encrypted)=0 FROM billing_store_subscription WHERE id=$1").bind(id).fetch_one(&mut *c).await?;
    assert!(detached);
    drop(c);
    assert!(stores.reference(id, false).await?.is_none());
    cleanup(&db, &[first, second]).await?;
    Ok(())
}
#[tokio::test]
async fn web_challenge_does_not_create_accounts_and_limits_parallel_attempts()
-> Result<(), Box<dyn std::error::Error>> {
    let db = Database::connect(&postgres_config()?).await?;
    let user = Uuid::new_v4();
    insert_user(&db, user).await?;
    let repo = WebAccountRepository::new(db.clone(), legal());
    let phone = format!("s20-{user}");
    let key = Uuid::new_v4();
    let challenge = repo.challenge(key, user, &phone, "fr").await?.unwrap();
    let replay = repo.challenge(key, user, &phone, "fr").await?.unwrap();
    assert_eq!(replay.id, challenge.id);
    assert!(repo.challenge(key, user, "other", "fr").await?.is_none());
    let absent = repo
        .challenge(Uuid::new_v4(), Uuid::new_v4(), &phone, "fr")
        .await?
        .unwrap();
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_event WHERE event_type='identity.web_otp.send'
         AND aggregate_id IN ($1,$2) AND payload='{}'::jsonb",
    )
    .bind(challenge.id)
    .bind(absent.id)
    .fetch_one(db.acquire().await?.as_mut())
    .await?;
    assert_eq!(queued, 2);
    assert!(repo.begin_verify(absent.id, &phone).await?.is_none());
    let futures = (0..12).map(|_| repo.begin_verify(challenge.id, &phone));
    let results = futures_util::future::join_all(futures).await;
    assert_eq!(
        results
            .into_iter()
            .filter(|result| matches!(result, Ok(Some(_))))
            .count(),
        5
    );
    let valid = repo
        .challenge(Uuid::new_v4(), user, &phone, "en")
        .await?
        .unwrap();
    let token = "synthetic-random-session-token-not-stored-plaintext";
    assert!(repo.open_session(valid.id, user, token).await?);
    assert!(!repo.open_session(valid.id, user, token).await?);
    assert_eq!(repo.identity(token).await?.unwrap().user_id, user);
    let mut c = db.acquire().await?;
    sqlx::query("UPDATE user_account SET is_banned=true WHERE user_id=$1")
        .bind(user)
        .execute(&mut *c)
        .await?;
    drop(c);
    assert!(repo.identity(token).await?.is_none());
    repo.logout(token).await?;
    cleanup(&db, &[user]).await?;
    Ok(())
}

#[derive(Default)]
struct SyntheticSms(std::sync::Mutex<Option<String>>);
impl histae_api_rust::identity::mobile::sweego::SmsDelivery for SyntheticSms {
    fn send_otp(
        &self,
        message: histae_api_rust::identity::mobile::sweego::SmsMessage,
    ) -> histae_api_rust::identity::mobile::sweego::SmsDeliveryFuture<'_> {
        *self.0.lock().unwrap() = Some(message.code);
        Box::pin(async {
            Ok(
                histae_api_rust::identity::mobile::sweego::SmsDeliveryReceipt {
                    transaction_id: Uuid::new_v4().to_string(),
                    message_id: Uuid::new_v4().to_string(),
                },
            )
        })
    }
}
#[tokio::test]
async fn website_otp_cannot_authenticate_mobile_or_another_website_challenge()
-> Result<(), Box<dyn std::error::Error>> {
    use histae_api_rust::identity::mobile::otp::{OtpRepository, OtpService};
    use std::sync::Arc;
    let db = Database::connect(&postgres_config()?).await?;
    let sms = Arc::new(SyntheticSms::default());
    let otp = OtpService::new(
        Arc::new(OtpRepository::new(db.clone())),
        sms.clone(),
        SecretString::new(histae_api_rust::infra::crypto::sha256_hex(
            Uuid::new_v4().as_bytes(),
        )),
        "fr".into(),
        Duration::from_secs(2),
        Duration::from_secs(300),
    );
    let phone = "+33600000001";
    let challenge = Uuid::new_v4();
    otp.for_web_challenge(challenge)
        .send(phone, Some(&challenge.to_string()))
        .await?;
    let code = sms.0.lock().unwrap().clone().unwrap();
    assert!(otp.consume(phone, &code).await.is_err());
    assert!(
        otp.for_web_challenge(Uuid::new_v4())
            .consume(phone, &code)
            .await
            .is_err()
    );
    assert!(
        otp.for_web_challenge(challenge)
            .consume(phone, &code)
            .await
            .is_ok()
    );
    assert!(
        otp.for_web_challenge(challenge)
            .consume(phone, &code)
            .await
            .is_err()
    );
    let hash = otp.rate_limit_key(
        phone,
        histae_api_rust::identity::mobile::otp::OtpError::InvalidPhone,
    )?;
    let mut c = db.acquire().await?;
    sqlx::query("DELETE FROM otp_verification WHERE phone_number_hash=$1")
        .bind(hash)
        .execute(&mut *c)
        .await?;
    Ok(())
}

#[tokio::test]
async fn financial_import_checks_current_role_and_rolls_back_conflicting_reports()
-> Result<(), Box<dyn std::error::Error>> {
    use histae_api_rust::billing::stores::administration::{
        FinanceImportError, StoreFinanceImport, import_report,
    };
    let db = Database::connect(&postgres_config()?).await?;
    let actor = Uuid::new_v4();
    let ordinary = Uuid::new_v4();
    insert_user(&db, actor).await?;
    insert_user(&db, ordinary).await?;
    let mut c = db.acquire().await?;
    sqlx::query("UPDATE user_account SET role='superadmin' WHERE user_id=$1")
        .bind(actor)
        .execute(&mut *c)
        .await?;
    drop(c);
    let repo = StoreRepository::new(db.clone(), SecretString::new("01".repeat(32)));
    let report = Uuid::new_v4().to_string();
    let value = serde_json::json!({"provider":"google_play","report_id":report,"rows":[{"row_id":"one","paid_at":"2026-10-01T00:00:00Z","currency":"EUR","gross_cents":500,"net_cents":null}]});
    let parse =
        |value: serde_json::Value| serde_json::from_value::<StoreFinanceImport>(value).unwrap();
    assert!(matches!(
        import_report(&repo, ordinary, parse(value.clone())).await,
        Err(FinanceImportError::Forbidden)
    ));
    import_report(&repo, actor, parse(value.clone()))
        .await
        .map_err(|_| FixtureError("finance_import"))?;
    import_report(&repo, actor, parse(value.clone()))
        .await
        .map_err(|_| FixtureError("finance_replay"))?;
    let mut conflict = value.clone();
    let mut new = conflict["rows"][0].clone();
    new["row_id"] = serde_json::json!("two");
    conflict["rows"][0]["gross_cents"] = serde_json::json!(600);
    conflict["rows"].as_array_mut().unwrap().insert(0, new);
    assert!(matches!(
        import_report(&repo, actor, parse(conflict)).await,
        Err(FinanceImportError::Conflict)
    ));
    let mut c = db.acquire().await?;
    let (rows,gross):(i64,i64)=sqlx::query_as("SELECT count(*),sum(gross_cents)::bigint FROM billing_store_finance WHERE provider='google_play' AND report_id=$1").bind(&report).fetch_one(&mut *c).await?;
    assert_eq!((rows, gross), (1, 500));
    sqlx::query("UPDATE user_account SET role='admin' WHERE user_id=$1")
        .bind(actor)
        .execute(&mut *c)
        .await?;
    drop(c);
    assert!(matches!(
        import_report(&repo, actor, parse(value)).await,
        Err(FinanceImportError::Forbidden)
    ));
    let mut c = db.acquire().await?;
    sqlx::query("DELETE FROM billing_store_finance WHERE report_id=$1")
        .bind(report)
        .execute(&mut *c)
        .await?;
    drop(c);
    cleanup(&db, &[actor, ordinary]).await?;
    Ok(())
}

#[tokio::test]
async fn native_entitlement_does_not_start_before_the_paid_period()
-> Result<(), Box<dyn std::error::Error>> {
    let db = Database::connect(&postgres_config()?).await?;
    let user = Uuid::new_v4();
    insert_user(&db, user).await?;
    let repo = StoreRepository::new(db.clone(), SecretString::new("01".repeat(32)));
    let account = repo.account(user).await?;
    let now = repo.observed_at().await?;
    let mut future = snapshot(
        StoreProvider::AppStore,
        account.id.to_string(),
        Uuid::new_v4().to_string(),
    );
    future.starts_at = now + TimeDelta::days(1);
    future.expires_at = now + TimeDelta::days(30);
    repo.apply(future, "synthetic-future-reference".into(), Some(user), now)
        .await?;
    let billing = PgBillingRepository::new(db.clone());
    assert!(
        !billing
            .effective_subscription_for_user(user, now)
            .await?
            .unwrap()
            .access_granted
    );
    assert!(
        billing
            .effective_subscription_for_user(user, now + TimeDelta::days(1))
            .await?
            .unwrap()
            .access_granted
    );
    assert!(
        !billing
            .effective_subscription_for_user(user, now + TimeDelta::days(30))
            .await?
            .unwrap()
            .access_granted
    );
    cleanup(&db, &[user]).await?;
    Ok(())
}

#[tokio::test]
async fn website_service_keeps_identity_otp_and_session_authorization_together()
-> Result<(), Box<dyn std::error::Error>> {
    use histae_api_rust::{
        identity::mobile::otp::{OtpRepository, OtpService},
        identity::web::{WebAccountError, WebAccountService, WebOtpHandler},
        infra::crypto::{encrypt_phone, sha256_hex},
        localization::Language,
        outbox::{
            types::{DispatchOutcome, OutboxEvent, OutboxEventType, OutboxStatus},
            worker::OutboxHandler,
        },
    };
    use std::sync::Arc;
    let config = postgres_config()?;
    let db = Database::connect(&config).await?;
    let worker_database = job_role::connect(&config, "outbox").await?;
    let sms = Arc::new(SyntheticSms::default());
    let phone_key = SecretString::new(sha256_hex(Uuid::new_v4().as_bytes()));
    let otp = OtpService::new(
        Arc::new(OtpRepository::new(db.clone())),
        sms.clone(),
        phone_key.clone(),
        "fr".into(),
        Duration::from_secs(2),
        Duration::from_secs(300),
    );
    let delivery_otp = OtpService::new(
        Arc::new(OtpRepository::new(worker_database.clone())),
        sms.clone(),
        phone_key.clone(),
        "fr".into(),
        Duration::from_secs(2),
        Duration::from_secs(300),
    );
    let legal = LegalConfig {
        document_dates: Default::default(),
        terms_version: "web-refactor-test".into(),
        privacy_version: "web-refactor-test".into(),
        ..legal()
    };
    let repository = WebAccountRepository::new(db.clone(), legal.clone());
    let worker = WebOtpHandler::new(
        WebAccountRepository::new(worker_database, legal),
        delivery_otp,
        phone_key.clone(),
    );
    let service = WebAccountService::new(repository, otp.clone());
    let phone = "+33600000002";
    let hash = service.phone_key(phone).unwrap();
    let user = Uuid::new_v4();
    insert_user(&db, user).await?;
    sqlx::query(
        "UPDATE user_account SET phone_number_hash=$2,phone_number_encrypted=$3 WHERE user_id=$1",
    )
    .bind(user)
    .bind(&hash)
    .bind(encrypt_phone(phone, &phone_key)?)
    .execute(&mut *db.acquire().await?)
    .await?;
    let unknown = service
        .start(Uuid::new_v4(), Uuid::new_v4(), phone, Language::Fr)
        .await
        .unwrap();
    assert!(sms.0.lock().unwrap().is_none());
    assert!(matches!(
        service.verify(unknown.challenge_id, phone, "123456").await,
        Err(WebAccountError::Authentication)
    ));
    let key = Uuid::new_v4();
    let challenge = service.start(key, user, phone, Language::Fr).await.unwrap();
    assert_eq!(
        service
            .start(key, user, phone, Language::Fr)
            .await
            .unwrap()
            .challenge_id,
        challenge.challenge_id
    );
    assert!(sms.0.lock().unwrap().is_none());
    let event = OutboxEvent {
        id: Uuid::new_v4(),
        event_type: OutboxEventType::WebOtpSend,
        aggregate_id: challenge.challenge_id,
        payload: Default::default(),
        status: OutboxStatus::Processing,
        attempts: 1,
    };
    assert_eq!(
        worker.handle(&event, Uuid::new_v4()).await,
        Ok(DispatchOutcome::Completed)
    );
    let code = sms.0.lock().unwrap().clone().unwrap();
    assert!(otp.consume(phone, &code).await.is_err());
    let (first, second) = tokio::join!(
        service.verify(challenge.challenge_id, phone, &code),
        service.verify(challenge.challenge_id, phone, &code)
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let token = first.or(second).unwrap();
    assert_eq!(token.expose_secret().len(), 43);
    assert_eq!(
        service
            .identity(token.expose_secret())
            .await
            .unwrap()
            .user_id,
        user
    );
    sqlx::query("UPDATE user_account SET is_banned=true WHERE user_id=$1")
        .bind(user)
        .execute(&mut *db.acquire().await?)
        .await?;
    assert!(matches!(
        service.identity(token.expose_secret()).await,
        Err(WebAccountError::Authentication)
    ));
    service.logout(token.expose_secret()).await.unwrap();
    cleanup(&db, &[user]).await?;
    sqlx::query("DELETE FROM web_account_challenge WHERE id=$1")
        .bind(unknown.challenge_id)
        .execute(&mut *db.acquire().await?)
        .await?;
    sqlx::query("DELETE FROM otp_verification WHERE phone_number_hash=$1")
        .bind(hash)
        .execute(&mut *db.acquire().await?)
        .await?;
    Ok(())
}
