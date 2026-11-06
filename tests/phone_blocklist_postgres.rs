#![cfg(feature = "postgres-integration")]
use chrono::Utc;
use histae_api_rust::{
    config::{PostgresConfig, SecretString, SmsSafetyConfig},
    identity::mobile::{
        otp::{BeginOtpDelivery, OtpRepository, OtpStore},
        phone_blocklist::{
            domain::*,
            pg::PgPhoneBlocklist,
            sync::{self, ImportedNumber, Source},
        },
        sms_safety::{PgSmsSafetyStore, SmsSafetyStore, SmsSendRejection},
    },
    infra::postgres::Database,
    privacy::maintenance::PrivacyMaintenanceRepository,
};
use sqlx::Acquire as _;
use std::{env, fmt, path::PathBuf, time::Duration};
use uuid::Uuid;
#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Phone policy fixture error ({})", self.0)
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
    if variable("HISTAE_PHONE_BLOCKLIST_TEST_ISOLATED")? != "true" {
        return Err(FixtureError("disposable_launcher_required"));
    }
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

fn key() -> SecretString {
    SecretString::new(variable("PHONE_HASH_KEY").expect("isolated HMAC key"))
}
fn phone() -> PhoneKey {
    PhoneKey::new("+33600000000", &key()).expect("synthetic phone")
}
fn observation() -> ImportedNumber {
    ImportedNumber {
        key: phone(),
        last_seen_at: Utc::now(),
    }
}
async fn account(
    database: &Database,
    id: Uuid,
    role: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query("INSERT INTO user_account(user_id,role,phone_number_hash,phone_number_encrypted) VALUES($1,$2,$3,''::bytea)")
        .bind(id).bind(role).bind(format!("policy-test-{id}")).execute(database.acquire().await?.as_mut()).await?;
    Ok(())
}
async fn clean(database: &Database, users: &[Uuid]) -> Result<(), Box<dyn std::error::Error>> {
    let mut c = database.acquire().await?;
    sqlx::query("DELETE FROM data_access_log WHERE accessor_id=ANY($1)")
        .bind(users)
        .execute(&mut *c)
        .await?;
    sqlx::query("DELETE FROM user_account WHERE user_id=ANY($1)")
        .bind(users)
        .execute(&mut *c)
        .await?;
    sqlx::query("DELETE FROM otp_verification WHERE phone_number_hash=$1")
        .bind(phone().hash)
        .execute(&mut *c)
        .await?;
    sqlx::query("DELETE FROM sms_send_attempt WHERE phone_hash=$1")
        .bind(phone().hash)
        .execute(&mut *c)
        .await?;
    sqlx::query("DELETE FROM phone_blocklist_entry WHERE phone_hash=$1")
        .bind(phone().hash)
        .execute(&mut *c)
        .await?;
    Ok(())
}

#[tokio::test]
async fn final_guard_and_audited_exceptions_keep_roles_expiry_imports_and_privacy_effective()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?).await?;
    histae_api_rust::identity::mobile::phone_blocklist::pg::initialize_key(&database, &key())
        .await?;
    let admin = Uuid::new_v4();
    let superadmin = Uuid::new_v4();
    account(&database, admin, "admin").await?;
    account(&database, superadmin, "superadmin").await?;
    let result:Result<(),Box<dyn std::error::Error>>=async {
        let guard=PgSmsSafetyStore::new(database.clone());let policy=SmsSafetyConfig::default();
        sqlx::query("UPDATE phone_blocklist_source SET last_success_at=NULL").execute(database.acquire().await?.as_mut()).await?;
        assert_eq!(guard.reserve(Uuid::new_v4(),phone().hash,policy.clone()).await?,Err(SmsSendRejection::Unavailable));
        let otp=OtpRepository::new(database.clone());
        let delivery=Uuid::new_v4();let code_hash="c".repeat(64);
        otp.begin(BeginOtpDelivery {language:histae_api_rust::localization::Language::Fr,id:delivery,phone_hash:phone().hash,otp_hash:code_hash.clone(),idempotency_key:Uuid::new_v4(),ttl:Duration::from_secs(600),settlement:Duration::from_secs(15)}).await?;
        otp.mark_accepted(delivery,phone().hash,"policy-transaction".into(),"policy-message".into()).await?;
        sync::import(&database,Source::Castle,vec![observation()],&key()).await?;
        assert!(!otp.consume(phone().hash,code_hash.clone()).await?,"a code issued before a block cannot open a new session");
        assert_eq!(guard.reserve(Uuid::new_v4(),phone().hash,policy.clone()).await?,Err(SmsSendRejection::PhoneBlocked));
        let attempts:i64=sqlx::query_scalar("SELECT count(*) FROM sms_send_attempt WHERE phone_hash=$1")
            .bind(phone().hash).fetch_one(database.acquire().await?.as_mut()).await?;
        assert_eq!(attempts,0,"a blocked request never spends the provider budget");
        assert_eq!(sync::import(&database,Source::Castle,vec![observation()],&SecretString::new("w".repeat(32))).await,Err(PolicyError::KeyMismatch));
        let store=PgPhoneBlocklist::new(database.clone());
        let entry=store.lookup(phone()).await?;let id=entry.id.ok_or(FixtureError("policy_id"))?;
        let json=serde_json::to_string(&store.overview(25,0).await?)?;
        assert!(!json.contains("+33600000000")&&!json.contains(&phone().hash));
        let change=|actor,decision,days,version,reason|Change{actor,target:Target::Id(id),decision,days,version,reason};
        assert_eq!(store.change(change(admin,Decision::Allow,Some(30),0,None)).await,Err(PolicyError::Invalid));
        assert_eq!(store.change(change(admin,Decision::Block,Some(30),0,Some("Unpermitted block attempt".into()))).await,Err(PolicyError::Forbidden));
        store.change(change(admin,Decision::Allow,Some(30),0,Some("Verified reassignment through support".into()))).await?;
        assert_eq!(store.change(change(admin,Decision::Allow,Some(30),0,Some("Stale administrator view".into()))).await,Err(PolicyError::Conflict));
        sync::import(&database,Source::Ip1,vec![observation()],&key()).await?;
        assert!(!store.lookup(phone()).await?.blocked,"feed imports cannot override a manual exception");
        assert!(otp.consume(phone().hash,code_hash.clone()).await?);
        assert!(!otp.consume(phone().hash,code_hash).await?);
        assert_eq!(guard.reserve(Uuid::new_v4(),phone().hash,policy.clone()).await?,Ok(()));
        sqlx::query("UPDATE phone_blocklist_entry SET decision_until=clock_timestamp()-INTERVAL '1 second' WHERE id=$1")
            .bind(id).execute(database.acquire().await?.as_mut()).await?;
        assert_eq!(guard.reserve(Uuid::new_v4(),phone().hash,policy.clone()).await?,Err(SmsSendRejection::PhoneBlocked));
        store.change(change(superadmin,Decision::Automatic,None,1,None)).await?;
        store.change(change(superadmin,Decision::Allow,Some(30),2,None)).await?;
        let audits:Vec<(String,String)>=sqlx::query_as("SELECT accessor_role,reason FROM data_access_log WHERE action='admin_allow_phone' AND accessor_id=ANY($1) ORDER BY accessed_at")
            .bind(vec![admin,superadmin]).fetch_all(database.acquire().await?.as_mut()).await?;
        assert_eq!(audits.len(),2);assert!(audits[1].1.starts_with("Superadmin phone policy change"));
        assert!(!audits.iter().any(|(_,reason)|reason.contains("+33600000000")));
        sqlx::query("UPDATE user_account SET role='user' WHERE user_id=$1").bind(admin).execute(database.acquire().await?.as_mut()).await?;
        assert_eq!(store.change(change(admin,Decision::Allow,Some(30),3,Some("Demoted actor attempt".into()))).await,Err(PolicyError::Forbidden));
        sync::record_failure(&database,Source::Castle).await?;
        assert!(store.overview(25,0).await?.ready,"a failed import retains the last good source");
        sqlx::query("UPDATE phone_blocklist_source SET last_success_at=clock_timestamp()-INTERVAL '8 days'")
            .execute(database.acquire().await?.as_mut()).await?;
        assert_eq!(guard.reserve(Uuid::new_v4(),"f".repeat(64),policy.clone()).await?,Err(SmsSendRejection::Unavailable));
        assert_eq!(guard.reserve(Uuid::new_v4(),phone().hash,policy.clone()).await?,Ok(()),"an active reviewed exception remains usable");
        sqlx::query("UPDATE phone_blocklist_source SET last_success_at=clock_timestamp()")
            .execute(database.acquire().await?.as_mut()).await?;
        sqlx::query("UPDATE phone_blocklist_entry SET decision_until=clock_timestamp()-INTERVAL '1 day' WHERE id=$1")
            .bind(id).execute(database.acquire().await?.as_mut()).await?;
        sqlx::query("UPDATE phone_blocklist_observation SET last_seen_at=clock_timestamp()-INTERVAL '91 days' WHERE entry_id=$1")
            .bind(id).execute(database.acquire().await?.as_mut()).await?;
        assert_eq!(guard.reserve(Uuid::new_v4(),phone().hash,policy).await?,Ok(()),"an old observation is not a permanent ban");
        sqlx::query("UPDATE phone_blocklist_entry SET updated_at=clock_timestamp()-INTERVAL '181 days' WHERE id=$1")
            .bind(id).execute(database.acquire().await?.as_mut()).await?;
        sqlx::query("UPDATE phone_blocklist_observation SET last_seen_at=clock_timestamp()-INTERVAL '181 days' WHERE entry_id=$1")
            .bind(id).execute(database.acquire().await?.as_mut()).await?;
        let cleanup=PrivacyMaintenanceRepository::new(database.clone()).run_as_leader(Utc::now(),10).await?.ok_or(FixtureError("privacy_lock"))?;
        assert_eq!(cleanup.expired_phone_observations,2);assert_eq!(cleanup.expired_phone_policies,1);
        assert!(store.lookup(phone()).await?.id.is_none());
        Ok(())
    }.await;
    let cleanup = clean(&database, &[admin, superadmin]).await;
    database.close().await;
    cleanup?;
    result
}

#[tokio::test]
async fn a_queued_sms_waits_for_the_policy_commit_then_uses_the_new_decision()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?).await?;
    histae_api_rust::identity::mobile::phone_blocklist::pg::initialize_key(&database, &key())
        .await?;
    let mut c = database.acquire().await?;
    let mut tx = c.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(POLICY_LOCK)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO phone_blocklist_entry(id,phone_hash,phone_hint,decision,decision_until) VALUES($1,$2,$3,'block',clock_timestamp()+INTERVAL '30 days') ON CONFLICT(phone_hash) DO UPDATE SET decision='block',decision_until=EXCLUDED.decision_until")
        .bind(Uuid::new_v4()).bind(phone().hash).bind(phone().hint).execute(&mut *tx).await?;
    let store = PgSmsSafetyStore::new(database.clone());
    let mut task = tokio::spawn(async move {
        store
            .reserve(Uuid::new_v4(), phone().hash, Default::default())
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut task)
            .await
            .is_err()
    );
    tx.commit().await?;
    drop(c);
    assert_eq!(task.await??, Err(SmsSendRejection::PhoneBlocked));
    let cleanup = clean(&database, &[]).await;
    database.close().await;
    cleanup?;
    Ok(())
}

#[tokio::test]
async fn a_policy_change_waits_before_locking_the_account_used_by_retention()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?).await?;
    let actor = Uuid::new_v4();
    account(&database, actor, "admin").await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let mut connection = database.acquire().await?;
        let mut retention = connection.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
            .bind(POLICY_LOCK)
            .execute(&mut *retention)
            .await?;
        let store = PgPhoneBlocklist::new(database.clone());
        let mut change = tokio::spawn(async move {
            store
                .change(Change {
                    actor,
                    target: Target::Phone(phone()),
                    decision: Decision::Allow,
                    days: Some(30),
                    version: 0,
                    reason: Some("Reassignment checked by support".into()),
                })
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut change)
                .await
                .is_err()
        );
        // Retention must still be able to lock this account while holding the shared policy lock.
        tokio::time::timeout(
            Duration::from_secs(2),
            sqlx::query("SELECT user_id FROM user_account WHERE user_id=$1 FOR UPDATE")
                .bind(actor)
                .fetch_one(&mut *retention),
        )
        .await??;
        retention.rollback().await?;
        drop(connection);
        change.await??;
        Ok(())
    }
    .await;
    let cleanup = clean(&database, &[actor]).await;
    database.close().await;
    cleanup?;
    result
}

#[tokio::test]
async fn sync_credentials_cannot_read_users_or_change_manual_exceptions()
-> Result<(), Box<dyn std::error::Error>> {
    let mut config = local_config()?;
    let app = Database::connect(&config).await?;
    histae_api_rust::identity::mobile::phone_blocklist::pg::initialize_key(&app, &key()).await?;
    config.user = "histae_phone_sync".into();
    config.password = SecretString::new(variable("PHONE_SYNC_TEST_PASSWORD")?);
    let sync_database = Database::connect(&config).await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        sync::import(&sync_database, Source::Castle, vec![observation()], &key()).await?;
        let mut c = sync_database.acquire().await?;
        assert!(
            sqlx::query("SELECT phone_number_hash FROM user_account LIMIT 1")
                .fetch_optional(&mut *c)
                .await
                .is_err()
        );
        assert!(
            sqlx::query("UPDATE phone_blocklist_entry SET decision='automatic'")
                .execute(&mut *c)
                .await
                .is_err()
        );
        assert!(
            sqlx::query("DELETE FROM phone_blocklist_entry")
                .execute(&mut *c)
                .await
                .is_err()
        );
        assert!(
            sqlx::query("CREATE TABLE phone_sync_privilege_escalation(id integer)")
                .execute(&mut *c)
                .await
                .is_err()
        );
        Ok(())
    }
    .await;
    sync_database.close().await;
    let cleanup = clean(&app, &[]).await;
    app.close().await;
    cleanup?;
    result
}
