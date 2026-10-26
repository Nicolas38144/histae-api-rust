#![cfg(feature = "postgres-integration")]
use chrono::{TimeDelta, Utc};
use histae_api_rust::{
    billing::{
        domain::BillingPeriod,
        pg::PgBillingRepository,
        store::BillingStore,
        stores::{StoreError, StoreProvider, StoreSnapshot, pg::StoreRepository},
        web::WebBillingRepository,
    },
    config::{LegalConfig, PostgresConfig, SecretString},
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
    assert!(
        billing
            .effective_subscription_for_user(first, Utc::now())
            .await?
            .unwrap()
            .access_granted
    );
    // A later Stripe projection with no access must not override native access.
    let mut c = db.acquire().await?;
    sqlx::query("INSERT INTO user_subscription(user_id,plan,provider) VALUES($1,'premium','stripe') ON CONFLICT(user_id) DO UPDATE SET plan='premium',provider='stripe',status=NULL,current_period_ends_at=NULL").bind(first).execute(&mut *c).await?;
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
    let repo = WebBillingRepository::new(db.clone(), legal());
    let phone = format!("s20-{user}");
    let key = Uuid::new_v4();
    let challenge = repo.challenge(key, user, &phone, "fr").await?.unwrap();
    assert!(challenge.created && challenge.matched);
    let replay = repo.challenge(key, user, &phone, "fr").await?.unwrap();
    assert_eq!(replay.id, challenge.id);
    assert!(!replay.created);
    assert!(repo.challenge(key, user, "other", "fr").await?.is_none());
    let absent = repo
        .challenge(Uuid::new_v4(), Uuid::new_v4(), &phone, "fr")
        .await?
        .unwrap();
    assert!(!absent.matched);
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
        billing::{
            service::BillingService,
            stripe::StripeClient,
            web::{WebBillingError, WebBillingService},
        },
        config::{BillingConfig, BillingProvider},
        identity::mobile::otp::{OtpRepository, OtpService},
        infra::postgres_locks::AccountActivityPool,
        localization::Language,
        shared::clock::SystemClock,
    };
    use std::sync::Arc;
    let config = postgres_config()?;
    let db = Database::connect(&config).await?;
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
    let billing_config = BillingConfig {
        provider: BillingProvider::Disabled,
        stripe_secret_key: SecretString::new(String::new()),
        stripe_webhook_secret: SecretString::new(String::new()),
        premium_monthly_price_id: String::new(),
        premium_annual_price_id: String::new(),
        checkout_success_url: None,
        checkout_cancel_url: None,
        portal_return_url: None,
        automatic_tax: false,
        allow_promotion_codes: false,
        timeout: Duration::from_secs(1),
        max_network_retries: 0,
        reconciliation_interval: Duration::from_secs(300),
        reconciliation_freshness: Duration::from_secs(3600),
        reconciliation_batch_size: 25,
    };
    // Billing is disabled and even accidental provider calls can only target loopback.
    let stripe =
        StripeClient::with_endpoint(billing_config.clone(), Url::parse("http://127.0.0.1:9/")?)?;
    let activity = AccountActivityPool::connect(&config).await?;
    let billing = Arc::new(BillingService::new(
        Arc::new(PgBillingRepository::new(db.clone())),
        Arc::new(stripe),
        billing_config,
        activity.clone(),
        Arc::new(SystemClock),
    ));
    let legal = LegalConfig {
        document_dates: Default::default(),
        terms_version: "web-refactor-test".into(),
        privacy_version: "web-refactor-test".into(),
        ..legal()
    };
    let service = WebBillingService::new(
        WebBillingRepository::new(db.clone(), legal),
        otp.clone(),
        billing,
    );
    let phone = "+33600000002";
    let hash = service.phone_key(phone).unwrap();
    let user = Uuid::new_v4();
    insert_user(&db, user).await?;
    sqlx::query("UPDATE user_account SET phone_number_hash=$2 WHERE user_id=$1")
        .bind(user)
        .bind(&hash)
        .execute(&mut *db.acquire().await?)
        .await?;
    let unknown = service
        .start(Uuid::new_v4(), Uuid::new_v4(), phone, Language::Fr)
        .await
        .unwrap();
    assert!(sms.0.lock().unwrap().is_none());
    assert!(matches!(
        service.verify(unknown.challenge_id, phone, "123456").await,
        Err(WebBillingError::Authentication)
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
            .session(token.expose_secret())
            .await
            .unwrap()
            .user_id,
        user
    );
    assert!(matches!(
        service.payer(token.expose_secret()).await,
        Err(WebBillingError::OnboardingIncomplete)
    ));
    sqlx::query("UPDATE user_account SET is_banned=true WHERE user_id=$1")
        .bind(user)
        .execute(&mut *db.acquire().await?)
        .await?;
    assert!(matches!(
        service.session(token.expose_secret()).await,
        Err(WebBillingError::Authentication)
    ));
    service.logout(token.expose_secret()).await.unwrap();
    cleanup(&db, &[user]).await?;
    sqlx::query("DELETE FROM web_billing_challenge WHERE id=$1")
        .bind(unknown.challenge_id)
        .execute(&mut *db.acquire().await?)
        .await?;
    sqlx::query("DELETE FROM otp_verification WHERE phone_number_hash=$1")
        .bind(hash)
        .execute(&mut *db.acquire().await?)
        .await?;
    activity.close().await;
    Ok(())
}
