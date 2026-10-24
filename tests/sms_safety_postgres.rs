#![cfg(feature = "postgres-integration")]
use futures_util::{FutureExt, future::join_all};
use histae_api_rust::{
    config::{PostgresConfig, SecretString, SmsSafetyConfig},
    identity::mobile::{
        otp::{BeginOtpDelivery, OtpDeliveryStart, OtpDeliveryState, OtpRepository, OtpStore},
        sms_safety::*,
        sweego::SmsFailureReason,
    },
    infra::{migrations, postgres::Database},
    privacy::maintenance::PrivacyMaintenanceRepository,
};
use std::{env, fmt, panic::AssertUnwindSafe, path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;
#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PostgreSQL SMS fixture error ({})", self.0)
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
        max_connections: 8,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        statement_timeout: Duration::from_secs(15),
        idle_transaction_timeout: Duration::from_secs(30),
        application_name: "histae-sms-safety-test",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
    })
}

#[tokio::test]
async fn budgets_are_atomic_durable_and_independent_of_otp_retention()
-> Result<(), Box<dyn std::error::Error>> {
    let mut config = local_config()?;
    let admin = Database::connect(&config).await?;
    let name = format!("histae_sms_test_{}", Uuid::new_v4().simple());
    let mut connection = admin.acquire().await?;
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&mut *connection)
        .await?;
    config.database = name.clone();
    let result = AssertUnwindSafe(async {
        migrations::apply(&config).await?;
        let database = Database::connect(&config).await?;
        let result = AssertUnwindSafe(check_safety(&config, &database))
            .catch_unwind()
            .await;
        database.close().await;
        match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
    .catch_unwind()
    .await;
    let cleanup = sqlx::query(&format!("DROP DATABASE {name}"))
        .execute(&mut *connection)
        .await;
    drop(connection);
    admin.close().await;
    cleanup?;
    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

async fn check_safety(
    config: &PostgresConfig,
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    assert_legacy_budget_upgrade(config, database).await?;
    let repository = PgSmsSafetyStore::new(database.clone());
    let policy = SmsSafetyConfig::default();
    let phone = "synthetic-hmac-phone";
    // Independent repository instances compete for a shared budget.
    let results = join_all((0..30).map(|_| {
        let repository = PgSmsSafetyStore::new(database.clone());
        let policy = policy.clone();
        async move {
            repository
                .reserve(Uuid::new_v4(), phone.into(), policy)
                .await
        }
    }))
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(results.iter().filter(|value| value.is_ok()).count(), 5);
    assert_eq!(
        results
            .iter()
            .filter(|value| **value == Err(SmsSendRejection::HourlyLimit))
            .count(),
        25
    );
    let other_id = Uuid::new_v4();
    assert_eq!(
        repository
            .reserve(other_id, "other-phone".into(), policy.clone())
            .await?,
        Ok(())
    );
    assert_eq!(
        repository
            .reserve(other_id, "other-phone".into(), policy.clone())
            .await?,
        Err(SmsSendRejection::AlreadyReserved)
    );
    let mut connection = database.acquire().await?;
    sqlx::query("UPDATE sms_send_attempt SET reserved_at = clock_timestamp() - interval '61 minutes' WHERE phone_hash = $1")
        .bind(phone).execute(&mut *connection).await?;
    drop(connection);
    for _ in 0..5 {
        assert_eq!(
            repository
                .reserve(Uuid::new_v4(), phone.into(), policy.clone())
                .await?,
            Ok(())
        );
    }
    assert_eq!(
        repository
            .reserve(Uuid::new_v4(), phone.into(), policy.clone())
            .await?,
        Err(SmsSendRejection::DailyLimit)
    );

    // Public idempotent replays preserve a policy failure and never ask for another send.
    let otp = OtpRepository::new(database.clone());
    let attempt = BeginOtpDelivery {
        language: histae_api_rust::localization::Language::Fr,
        id: Uuid::new_v4(),
        phone_hash: phone.into(),
        otp_hash: "synthetic-code-hash".into(),
        idempotency_key: Uuid::new_v4(),
        ttl: Duration::from_secs(600),
        settlement: Duration::from_secs(35),
    };
    assert!(matches!(
        otp.begin(attempt.clone()).await?,
        OtpDeliveryStart::Created(_)
    ));
    otp.mark_outcome(
        attempt.id,
        phone.into(),
        OtpDeliveryState::Failed,
        SmsFailureReason::PhoneDailyLimit,
    )
    .await?;
    assert_eq!(
        otp.begin(attempt.clone()).await?,
        OtpDeliveryStart::PolicyRejected(SmsFailureReason::PhoneDailyLimit)
    );
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM otp_verification WHERE id=$1")
        .bind(attempt.id)
        .execute(&mut *connection)
        .await?;
    drop(connection);
    assert_eq!(
        repository
            .reserve(Uuid::new_v4(), phone.into(), policy.clone())
            .await?,
        Err(SmsSendRejection::DailyLimit)
    );

    let operator = Uuid::new_v4();
    let ordinary = Uuid::new_v4();
    let mut connection = database.acquire().await?;
    sqlx::query("INSERT INTO user_account(user_id,role,phone_number_hash,phone_number_encrypted) VALUES ($1,'superadmin',$1::text,''::bytea),($2,'admin',$2::text,''::bytea)")
        .bind(operator).bind(ordinary).execute(&mut *connection).await?;
    drop(connection);
    assert_eq!(
        repository.set_suspended(ordinary, true, 0).await,
        Err(SmsControlError::Forbidden)
    );
    repository.set_suspended(operator, true, 0).await?;
    assert_eq!(
        repository
            .reserve(Uuid::new_v4(), "new-phone".into(), policy.clone())
            .await?,
        Err(SmsSendRejection::Suspended)
    );
    assert_eq!(
        repository.set_suspended(operator, false, 0).await,
        Err(SmsControlError::Conflict)
    );
    // Migration and fresh service construction preserve a manual stop and all reservations.
    migrations::apply(config).await?;
    migrations::apply(config).await?;
    let service = SmsSafetyService::new(
        Arc::new(repository.clone()),
        policy.clone(),
        SecretString::new("h".repeat(32)),
        true,
    );
    let snapshot = service.snapshot().await?;
    assert!(snapshot.manual_suspended && snapshot.suspended);
    assert_eq!(snapshot.version, 1);
    assert_eq!(snapshot.sends_last_24_hours, 11);
    repository.set_suspended(operator, false, 1).await?;
    assert_eq!(
        repository
            .reserve(Uuid::new_v4(), phone.into(), policy.clone())
            .await?,
        Err(SmsSendRejection::DailyLimit)
    );
    let forced = SmsSafetyService::new(
        Arc::new(repository.clone()),
        SmsSafetyConfig {
            suspended: true,
            ..policy.clone()
        },
        SecretString::new("h".repeat(32)),
        true,
    );
    assert_eq!(
        forced.reserve(Uuid::new_v4(), "+33600000001").await,
        Err(SmsSendRejection::Suspended)
    );
    assert!(forced.snapshot().await?.environment_suspended);
    let mut connection = database.acquire().await?;
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM data_access_log WHERE accessor_id=$1 ORDER BY accessed_at,id",
    )
    .bind(operator)
    .fetch_all(&mut *connection)
    .await?;
    assert_eq!(actions, vec!["admin_suspend_sms", "admin_resume_sms"]);
    assert_otp_quota(database, &service).await?;
    sqlx::query("UPDATE sms_send_attempt SET reserved_at=clock_timestamp()-interval '26 hours' WHERE phone_hash=$1")
        .bind(phone).execute(&mut *connection).await?;
    drop(connection);
    let retention = PrivacyMaintenanceRepository::new(database.clone())
        .run_as_leader(chrono::Utc::now(), 1000)
        .await?
        .expect("maintenance leader");
    assert_eq!(retention.expired_sms_attempts, 10);
    assert_eq!(
        repository
            .reserve(Uuid::new_v4(), phone.into(), policy)
            .await?,
        Ok(())
    );
    assert_global_budget(database).await?;
    assert_mobile_queue(database).await?;
    // A missing control row fails closed rather than treating suspension as false.
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM sms_send_control")
        .execute(&mut *connection)
        .await?;
    drop(connection);
    assert_eq!(
        service.reserve(Uuid::new_v4(), "+33600000001").await,
        Err(SmsSendRejection::Unavailable)
    );
    Ok(())
}

async fn assert_global_budget(database: &Database) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query("DELETE FROM sms_send_attempt").execute(database.pool()).await?;
    let policy = SmsSafetyConfig { hourly_global: 2, daily_global: 3, ..Default::default() };
    let results = join_all((0..30).map(|index| {
        let repository = PgSmsSafetyStore::new(database.clone());
        let policy = policy.clone();
        async move { repository.reserve(Uuid::new_v4(), format!("different-phone-{index}"), policy).await }
    })).await.into_iter().collect::<Result<Vec<_>, _>>()?;
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 2);
    assert_eq!(results.iter().filter(|result| **result == Err(SmsSendRejection::GlobalHourlyLimit)).count(), 28);
    // Restarting another repository or changing phone cannot reset a global cap.
    sqlx::query("UPDATE sms_send_attempt SET reserved_at=clock_timestamp()-interval '61 minutes'")
        .execute(database.pool()).await?;
    let restarted = PgSmsSafetyStore::new(database.clone());
    assert_eq!(restarted.reserve(Uuid::new_v4(), "new-phone".into(), policy.clone()).await?, Ok(()));
    assert_eq!(restarted.reserve(Uuid::new_v4(), "another-phone".into(), policy).await?, Err(SmsSendRejection::GlobalDailyLimit));
    Ok(())
}

async fn assert_mobile_queue(database: &Database) -> Result<(), Box<dyn std::error::Error>> {
    use histae_api_rust::{
        identity::mobile::otp::{requests::{OtpRequestService, OtpRequestStore, RequestError}, requests_pg::PgOtpRequestStore},
        infra::crypto::hmac_sha256,
        localization::Language,
    };
    let phone = "+33600000008";
    let key = SecretString::new("h".repeat(32));
    let hash = hmac_sha256(phone, &key)?;
    let store = Arc::new(PgOtpRequestStore::new(database.clone()));
    let service = OtpRequestService::new(store.clone(), key.clone(), Duration::from_secs(600), Duration::from_secs(60));
    let results = join_all((0..30).map(|_| {
        let service = service.clone();
        let hash = hash.clone();
        async move {
            let id = Uuid::new_v4();
            (id, service.enqueue(phone, hash, id, true, Language::Fr).await)
        }
    })).await;
    let accepted: Vec<_> = results.iter().filter(|(_, result)| result.is_ok()).collect();
    assert_eq!(accepted.len(), 1);
    assert_eq!(results.iter().filter(|(_, result)| matches!(result, Err(RequestError::Cooldown(1..=60)))).count(), 29);
    let id = accepted[0].0;
    let pending = store.pending(id).await?.expect("encrypted request");
    assert!(!pending.encrypted_phone.windows(phone.len()).any(|window| window == phone.as_bytes()));
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM outbox_event WHERE event_type='identity.otp.send' AND aggregate_id=$1 AND payload='{}'::jsonb")
        .bind(id).fetch_one(database.pool()).await?;
    assert_eq!(events, 1);
    // Same UUID remains idempotent even after erasure of the dispatch payload.
    store.finish(id).await?;
    assert!(store.pending(id).await?.is_none());
    assert!(service.enqueue(phone, hash.clone(), id, true, Language::Fr).await? <= 60);
    assert_eq!(service.enqueue(phone, hash.clone(), id, false, Language::Fr).await, Err(RequestError::Conflict));
    // Cooldown is durable across new service and repository instances.
    let restarted = OtpRequestService::new(Arc::new(PgOtpRequestStore::new(database.clone())), key, Duration::from_secs(600), Duration::from_secs(60));
    assert!(matches!(restarted.enqueue(phone, hash.clone(), Uuid::new_v4(), true, Language::Fr).await, Err(RequestError::Cooldown(_))));
    sqlx::query("UPDATE mobile_otp_request SET created_at=created_at-interval '61 seconds',resend_at=resend_at-interval '61 seconds' WHERE id=$1")
        .bind(id).execute(database.pool()).await?;
    let next = Uuid::new_v4();
    restarted.enqueue(phone, hash, next, true, Language::Fr).await?;
    assert!(store.pending(next).await?.is_some());
    sqlx::query("UPDATE mobile_otp_request SET created_at=created_at-interval '61 seconds',expires_at=clock_timestamp()-interval '1 second',resend_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(next).execute(database.pool()).await?;
    assert!(store.pending(next).await?.is_none());
    let retention = PrivacyMaintenanceRepository::new(database.clone()).run_as_leader(chrono::Utc::now(), 1000).await?.expect("maintenance leader");
    assert_eq!(retention.expired_mobile_otp_requests, 1);
    Ok(())
}

async fn assert_otp_quota(
    database: &Database,
    safety: &SmsSafetyService,
) -> Result<(), Box<dyn std::error::Error>> {
    use histae_api_rust::{
        config::{SmsConfig, SmsProvider},
        identity::mobile::{
            otp::{OtpError, OtpService},
            sweego::SweegoSmsService,
        },
    };
    let canonical = "+33600000009";
    for _ in 0..5 {
        safety
            .reserve(Uuid::new_v4(), canonical)
            .await
            .map_err(|error| format!("unexpected reservation: {error:?}"))?;
    }
    let sms = SweegoSmsService::new(
        SmsConfig {
            provider: SmsProvider::Sweego,
            // The exhausted PostgreSQL guard must reject before this endpoint can be contacted.
            endpoint: "http://127.0.0.1:1/never-used".parse()?,
            api_key: SecretString::new("synthetic-key".into()),
            sender_id: "Histae".into(),
            region: "FR".into(),
            timeout: Duration::from_millis(100),
            otp_ttl: Duration::from_secs(600),
            resend_cooldown: Duration::from_secs(60),
            webhook_secret: SecretString::new(String::new()),
            safety: Default::default(),
        },
        Arc::new(safety.clone()),
    )?;
    let otp = OtpService::new(
        Arc::new(OtpRepository::new(database.clone())),
        Arc::new(sms),
        SecretString::new("h".repeat(32)),
        "FR".into(),
        Duration::from_secs(1),
        Duration::from_secs(600),
    );
    let key = Uuid::new_v4().to_string();
    assert_eq!(
        otp.send("+33 6 00 00 00 09", Some(&key)).await,
        Err(OtpError::PhoneRateLimited)
    );
    assert_eq!(
        otp.send(canonical, Some(&key)).await,
        Err(OtpError::PhoneRateLimited)
    );
    Ok(())
}

async fn assert_legacy_budget_upgrade(
    config: &PostgresConfig,
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let attempt = BeginOtpDelivery {
        language: histae_api_rust::localization::Language::Fr,
        id: Uuid::new_v4(),
        phone_hash: "legacy-budget-fixture".into(),
        otp_hash: "not-a-code".into(),
        idempotency_key: Uuid::new_v4(),
        ttl: Duration::from_secs(600),
        settlement: Duration::from_secs(35),
    };
    OtpRepository::new(database.clone())
        .begin(attempt.clone())
        .await?;
    let mut connection = database.acquire().await?;
    sqlx::raw_sql("DROP TABLE sms_send_attempt, sms_send_control")
        .execute(&mut *connection)
        .await?;
    drop(connection);
    migrations::apply(config).await?;
    migrations::apply(config).await?;
    let mut connection = database.acquire().await?;
    let imported: Vec<Uuid> = sqlx::query_scalar("SELECT delivery_id FROM sms_send_attempt")
        .fetch_all(&mut *connection)
        .await?;
    assert_eq!(imported, vec![attempt.id]);
    sqlx::query("DELETE FROM sms_send_attempt WHERE delivery_id=$1")
        .bind(attempt.id)
        .execute(&mut *connection)
        .await?;
    sqlx::query("DELETE FROM otp_verification WHERE id=$1")
        .bind(attempt.id)
        .execute(&mut *connection)
        .await?;
    Ok(())
}
