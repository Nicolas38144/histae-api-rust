#![cfg(feature = "postgres-integration")]

#[path = "play_review/premium.rs"]
mod premium;

use chrono::Utc;
use histae_api_rust::{
    administration::{
        domain::{AdminUserRole, RoleChangeResult},
        pg::PgAdministrationStore,
        store::AdministrationStore,
    },
    billing::{pg::PgBillingRepository, store::BillingStore},
    config::{AppConfig, Environment},
    identity::mobile::{
        otp::{
            PgOtpRequestStore,
            requests::{NewOtpRequest, OtpRequestStore},
        },
        pg::MobileSessionRepository,
        play_review::NewReviewAccount,
        store::MobileSessionStore,
        tokens::TokenService,
    },
    infra::postgres::Database,
};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

async fn database() -> TestResult<(Database, TokenService)> {
    let config = AppConfig::from_env()?;
    assert_eq!(config.environment, Environment::Development);
    assert_eq!(config.postgres.database, "histae-dev");
    assert!(matches!(
        config.postgres.host.as_str(),
        "localhost" | "127.0.0.1" | "::1"
    ));
    Ok((
        Database::connect(&config.postgres).await?,
        TokenService::new(config.jwt),
    ))
}

fn review_account() -> NewReviewAccount {
    NewReviewAccount {
        user_id: Uuid::new_v4(),
        phone_hash: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
        encrypted_phone: vec![1, 2, 3],
    }
}

async fn signup_intent(
    database: &Database,
    account: &NewReviewAccount,
    registration: bool,
) -> TestResult<Uuid> {
    let id = Uuid::new_v4();
    PgOtpRequestStore::new(database.clone())
        .record_review(NewOtpRequest {
            id,
            phone_hash: account.phone_hash.clone(),
            encrypted_phone: Vec::new(),
            registration,
            language: histae_api_rust::localization::Language::Fr,
            ttl: std::time::Duration::from_secs(600),
            cooldown: std::time::Duration::from_secs(60),
        })
        .await?;
    Ok(id)
}

async fn remove(database: &Database, phone_hash: &str) -> TestResult {
    sqlx::query("DELETE FROM mobile_otp_request WHERE phone_hash=$1")
        .bind(phone_hash)
        .execute(database.acquire().await?.as_mut())
        .await?;
    sqlx::query("DELETE FROM user_account WHERE phone_number_hash=$1")
        .bind(phone_hash)
        .execute(database.acquire().await?.as_mut())
        .await?;
    sqlx::query("DELETE FROM account_tombstone WHERE phone_number_hash=$1")
        .bind(phone_hash)
        .execute(database.acquire().await?.as_mut())
        .await?;
    Ok(())
}

#[tokio::test]
async fn concurrent_review_logins_share_one_dedicated_account_and_keep_distinct_sessions()
-> TestResult {
    let (database, tokens) = database().await?;
    let sessions = MobileSessionRepository::new(database.clone());
    let first = review_account();
    signup_intent(&database, &first, true).await?;
    let second = NewReviewAccount {
        user_id: Uuid::new_v4(),
        ..first.clone()
    };
    let result: TestResult = async {
        let (a, b) = tokio::join!(
            sessions.create_review_session(first.clone(), tokens.new_refresh_token()?),
            sessions.create_review_session(second, tokens.new_refresh_token()?),
        );
        let (a, b) = (a?.expect("first review login"), b?.expect("second review login"));
        assert_eq!(a.user_id, b.user_id);
        assert_ne!(a.session_id, b.session_id);
        let state: (String, bool, i64) = sqlx::query_as(
            "SELECT role,is_review_account,(SELECT count(*) FROM refresh_token_family WHERE user_id=$1)
             FROM user_account WHERE user_id=$1",
        ).bind(a.user_id).fetch_one(database.acquire().await?.as_mut()).await?;
        assert_eq!(state, ("user".into(), true, 2));
        for role in ["admin", "superadmin"] {
            assert!(sqlx::query("UPDATE user_account SET role=$2 WHERE user_id=$1")
                .bind(a.user_id).bind(role).execute(database.acquire().await?.as_mut()).await.is_err());
        }
        Ok(())
    }.await;
    remove(&database, &first.phone_hash).await?;
    result
}

#[tokio::test]
async fn review_login_never_adopts_an_existing_real_or_privileged_account() -> TestResult {
    let (database, tokens) = database().await?;
    let sessions = MobileSessionRepository::new(database.clone());
    // A superadmin may already exist in a shared local test database.
    for role in ["user", "admin"] {
        let account = review_account();
        sqlx::query("INSERT INTO user_account(user_id,phone_number_hash,phone_number_encrypted,role) VALUES($1,$2,$3,$4)")
            .bind(account.user_id).bind(&account.phone_hash).bind(&account.encrypted_phone).bind(role)
            .execute(database.acquire().await?.as_mut()).await?;
        signup_intent(&database, &account, true).await?;
        let result: TestResult = async {
            assert!(
                sessions
                    .create_review_session(account.clone(), tokens.new_refresh_token()?)
                    .await?
                    .is_none()
            );
            let unchanged: bool = sqlx::query_scalar(
                "SELECT NOT is_review_account AND role=$2 AND NOT EXISTS(
                   SELECT 1 FROM refresh_token_family WHERE user_id=$1
                 ) FROM user_account WHERE user_id=$1",
            )
            .bind(account.user_id)
            .bind(role)
            .fetch_one(database.acquire().await?.as_mut())
            .await?;
            assert!(unchanged);
            assert!(
                PgBillingRepository::new(database.clone())
                    .effective_subscription_for_user(account.user_id, Utc::now())
                    .await?
                    .is_none()
            );
            Ok(())
        }
        .await;
        remove(&database, &account.phone_hash).await?;
        result?;
    }
    Ok(())
}

#[tokio::test]
async fn review_accounts_keep_bans_deletions_and_tombstones_effective() -> TestResult {
    let (database, tokens) = database().await?;
    let sessions = MobileSessionRepository::new(database.clone());
    for state in ["banned", "deleted", "tombstone"] {
        let account = review_account();
        let result: TestResult = async {
            if state == "tombstone" {
                sqlx::query("INSERT INTO account_tombstone(phone_number_hash,reason,expires_at) VALUES($1,'banned_account',clock_timestamp()+INTERVAL '1 day')")
                    .bind(&account.phone_hash).execute(database.acquire().await?.as_mut()).await?;
            } else {
                signup_intent(&database, &account, true).await?;
                sessions.create_review_session(account.clone(), tokens.new_refresh_token()?).await?.expect("created review account");
                sqlx::query("UPDATE user_account SET is_banned=($2='banned'),deleted_at=CASE WHEN $2='deleted' THEN clock_timestamp() END WHERE phone_number_hash=$1")
                    .bind(&account.phone_hash).bind(state).execute(database.acquire().await?.as_mut()).await?;
            }
            let families_before: i64 = sqlx::query_scalar("SELECT count(*) FROM refresh_token_family WHERE user_id IN (SELECT user_id FROM user_account WHERE phone_number_hash=$1)")
                .bind(&account.phone_hash).fetch_one(database.acquire().await?.as_mut()).await?;
            assert!(sessions.create_review_session(account.clone(), tokens.new_refresh_token()?).await?.is_none());
            let families: i64 = sqlx::query_scalar("SELECT count(*) FROM refresh_token_family WHERE user_id IN (SELECT user_id FROM user_account WHERE phone_number_hash=$1)")
                .bind(&account.phone_hash).fetch_one(database.acquire().await?.as_mut()).await?;
            assert_eq!(families, families_before, "a refused review login must not create a session");
            assert!(PgBillingRepository::new(database.clone()).effective_subscription_for_user(account.user_id, Utc::now()).await?.is_none());
            Ok(())
        }.await;
        remove(&database, &account.phone_hash).await?;
        result?;
    }
    Ok(())
}

#[tokio::test]
async fn superadmin_promotion_api_refuses_a_review_account() -> TestResult {
    let (database, tokens) = database().await?;
    let account = review_account();
    signup_intent(&database, &account, true).await?;
    let session = MobileSessionRepository::new(database.clone())
        .create_review_session(account.clone(), tokens.new_refresh_token()?)
        .await?
        .expect("review account");
    let actor: Option<Uuid> = sqlx::query_scalar("SELECT user_id FROM user_account WHERE role='superadmin' AND NOT is_banned AND deleted_at IS NULL")
        .fetch_optional(database.acquire().await?.as_mut()).await?;
    let created_actor = actor.is_none();
    let actor = actor.unwrap_or_else(Uuid::new_v4);
    if created_actor {
        sqlx::query("INSERT INTO user_account(user_id,phone_number_hash,phone_number_encrypted,role) VALUES($1,$2,''::bytea,'superadmin')")
            .bind(actor).bind(format!("review-admin-{actor}" )).execute(database.acquire().await?.as_mut()).await?;
    }
    let result = PgAdministrationStore::new(database.clone())
        .set_role(
            session.user_id,
            AdminUserRole::Admin,
            "review promotion regression".into(),
            actor,
        )
        .await;
    remove(&database, &account.phone_hash).await?;
    if created_actor {
        sqlx::query("DELETE FROM user_account WHERE user_id=$1")
            .bind(actor)
            .execute(database.acquire().await?.as_mut())
            .await?;
    }
    assert_eq!(result?, RoleChangeResult::Forbidden);
    Ok(())
}

#[tokio::test]
async fn a_deletion_committing_during_review_creation_cannot_bypass_its_tombstone() -> TestResult {
    use sqlx::Acquire as _;
    use std::time::Duration;

    let (database, tokens) = database().await?;
    let account = review_account();
    let sessions = MobileSessionRepository::new(database.clone());
    signup_intent(&database, &account, true).await?;
    sessions
        .create_review_session(account.clone(), tokens.new_refresh_token()?)
        .await?
        .expect("initial account");
    let result: TestResult = async {
        let mut connection = database.acquire().await?;
        let mut deletion = connection.begin().await?;
        sqlx::query("DELETE FROM user_account WHERE phone_number_hash=$1")
            .bind(&account.phone_hash).execute(&mut *deletion).await?;
        sqlx::query("INSERT INTO account_tombstone(phone_number_hash,reason,expires_at) VALUES($1,'banned_account',clock_timestamp()+INTERVAL '1 day')")
            .bind(&account.phone_hash).execute(&mut *deletion).await?;
        let token = tokens.new_refresh_token()?;
        let recreated = NewReviewAccount { user_id: Uuid::new_v4(), ..account.clone() };
        let attempt = tokio::spawn(async move { sessions.create_review_session(recreated, token).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let waiting: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity
                     WHERE usename=current_user AND wait_event_type='Lock'
                       AND query LIKE 'INSERT INTO user_account%is_review_account%')",
                ).fetch_one(database.acquire().await?.as_mut()).await?;
                if waiting { return Ok::<_, Box<dyn std::error::Error>>(()); }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await??;
        deletion.commit().await?;
        assert!(attempt.await??.is_none());
        let absent: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM user_account WHERE phone_number_hash=$1)")
            .bind(&account.phone_hash).fetch_one(database.acquire().await?.as_mut()).await?;
        assert!(absent, "review creation must roll back after a concurrent tombstone");
        Ok(())
    }.await;
    remove(&database, &account.phone_hash).await?;
    result
}

#[tokio::test]
async fn only_a_current_signup_intent_can_create_a_review_account_without_sms() -> TestResult {
    let (database, tokens) = database().await?;
    let account = review_account();
    let sessions = MobileSessionRepository::new(database.clone());
    let result: TestResult = async {
        assert!(sessions.create_review_session(account.clone(), tokens.new_refresh_token()?).await?.is_none());
        signup_intent(&database, &account, false).await?;
        assert!(sessions.create_review_session(account.clone(), tokens.new_refresh_token()?).await?.is_none());
        let absent: bool = sqlx::query_scalar("SELECT NOT EXISTS (SELECT 1 FROM user_account WHERE phone_number_hash=$1)")
            .bind(&account.phone_hash).fetch_one(database.acquire().await?.as_mut()).await?;
        assert!(absent, "login must not create a reviewer");
        sqlx::query("UPDATE mobile_otp_request SET created_at=created_at-INTERVAL '61 seconds',resend_at=resend_at-INTERVAL '61 seconds' WHERE phone_hash=$1")
            .bind(&account.phone_hash).execute(database.acquire().await?.as_mut()).await?;
        let signup_id = signup_intent(&database, &account, true).await?;
        let registered = sessions.create_review_session(account.clone(), tokens.new_refresh_token()?).await?.expect("signup creates the reviewer");
        let no_sms: bool = sqlx::query_scalar(
            "SELECT encrypted_phone IS NULL AND NOT EXISTS (SELECT 1 FROM outbox_event WHERE aggregate_id=$1)
             FROM mobile_otp_request WHERE id=$1",
        ).bind(signup_id).fetch_one(database.acquire().await?.as_mut()).await?;
        assert!(no_sms, "review signup stores intent without a phone payload or SMS job");
        assert!(PgOtpRequestStore::new(database.clone()).pending(signup_id).await?.is_none());
        let no_consents: bool = sqlx::query_scalar("SELECT NOT EXISTS (SELECT 1 FROM user_consent WHERE user_id=$1)")
            .bind(registered.user_id).fetch_one(database.acquire().await?.as_mut()).await?;
        assert!(no_consents, "signup must not fill the user's consent forms");
        sqlx::query("UPDATE mobile_otp_request SET created_at=created_at-INTERVAL '61 seconds',resend_at=resend_at-INTERVAL '61 seconds' WHERE phone_hash=$1")
            .bind(&account.phone_hash).execute(database.acquire().await?.as_mut()).await?;
        signup_intent(&database, &account, false).await?;
        let signed_in = sessions.create_review_session(account.clone(), tokens.new_refresh_token()?).await?.expect("login finds the registered reviewer");
        assert_eq!(registered.user_id, signed_in.user_id);
        Ok(())
    }.await;
    remove(&database, &account.phone_hash).await?;
    result
}

#[tokio::test]
async fn expired_signup_intents_cannot_create_a_review_account() -> TestResult {
    let (database, tokens) = database().await?;
    let account = review_account();
    let result: TestResult = async {
        signup_intent(&database, &account, true).await?;
        sqlx::query("UPDATE mobile_otp_request SET created_at=clock_timestamp()-INTERVAL '1 hour',expires_at=clock_timestamp()-INTERVAL '50 minutes',resend_at=clock_timestamp()-INTERVAL '59 minutes' WHERE phone_hash=$1")
            .bind(&account.phone_hash).execute(database.acquire().await?.as_mut()).await?;
        assert!(MobileSessionRepository::new(database.clone()).create_review_session(account.clone(), tokens.new_refresh_token()?).await?.is_none());
        Ok(())
    }.await;
    remove(&database, &account.phone_hash).await?;
    result
}
