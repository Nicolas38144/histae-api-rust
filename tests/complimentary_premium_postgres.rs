#![cfg(feature = "postgres-integration")]

use chrono::Utc;
use histae_api_rust::{
    administration::{
        domain::PremiumGrantResult, pg::PgAdministrationStore, store::AdministrationStore,
    },
    billing::{domain::SubscriptionProvider, pg::PgBillingRepository, store::BillingStore},
    config::{AppConfig, Environment},
    identity::admin_role::AdminRole,
    infra::postgres::Database,
    matches::pg::credits::lock_wallet,
};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    database: Database,
    actor: Uuid,
    target: Uuid,
    admin: Uuid,
    created_actor: bool,
}

impl Fixture {
    async fn new() -> TestResult<Self> {
        let config = AppConfig::from_env()?;
        assert_eq!(config.environment, Environment::Development);
        assert_eq!(config.postgres.database, "histae-dev");
        assert!(matches!(
            config.postgres.host.as_str(),
            "localhost" | "127.0.0.1" | "::1"
        ));
        let database = Database::connect(&config.postgres).await?;
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT user_id FROM user_account WHERE role='superadmin' AND NOT is_banned AND deleted_at IS NULL",
        ).fetch_optional(database.acquire().await?.as_mut()).await?;
        let actor = existing.unwrap_or_else(Uuid::new_v4);
        let fixture = Self {
            database,
            actor,
            target: Uuid::new_v4(),
            admin: Uuid::new_v4(),
            created_actor: existing.is_none(),
        };
        for (id, role) in [(fixture.target, "user"), (fixture.admin, "admin")]
            .into_iter()
            .chain(fixture.created_actor.then_some((actor, "superadmin")))
        {
            sqlx::query("INSERT INTO user_account(user_id,role,phone_number_hash,phone_number_encrypted) VALUES($1,$2,$3,''::bytea)")
                .bind(id).bind(role).bind(format!("premium-gift-{id}"))
                .execute(fixture.database.acquire().await?.as_mut()).await?;
        }
        Ok(fixture)
    }

    async fn set(&self, enabled: bool) -> TestResult<PremiumGrantResult> {
        Ok(PgAdministrationStore::new(self.database.clone())
            .set_complimentary_premium(
                self.target,
                enabled,
                "Complimentary Premium fixture".into(),
                self.actor,
            )
            .await?)
    }

    async fn finish(self, result: TestResult) -> TestResult {
        let mut users = vec![self.target, self.admin];
        if self.created_actor {
            users.push(self.actor);
        }
        sqlx::query("DELETE FROM data_access_log WHERE accessed_user_id=ANY($1)")
            .bind(&users)
            .execute(self.database.acquire().await?.as_mut())
            .await?;
        sqlx::query("DELETE FROM user_account WHERE user_id=ANY($1)")
            .bind(users)
            .execute(self.database.acquire().await?.as_mut())
            .await?;
        result
    }
}

#[tokio::test]
async fn gifts_are_idempotent_audited_and_shared_by_mobile_and_admin_views() -> TestResult {
    let fixture = Fixture::new().await?;
    let result: TestResult = async {
        assert_eq!(fixture.set(true).await?, PremiumGrantResult::Updated);
        assert_eq!(fixture.set(true).await?, PremiumGrantResult::Unchanged);
        let billing = PgBillingRepository::new(fixture.database.clone());
        let premium = billing
            .effective_subscription_for_user(fixture.target, Utc::now())
            .await?
            .expect("gift");
        assert_eq!(premium.plan, "premium");
        assert!(premium.access_granted);
        assert_eq!(
            (premium.provider, premium.current_period_ends_at),
            (None, None)
        );
        let detail = PgAdministrationStore::new(fixture.database.clone())
            .user_detail(
                fixture.target,
                fixture.actor,
                AdminRole::Superadmin,
                "Fixture profile view".into(),
                "continuation-test-v1".into(),
                "continuation-test-v1".into(),
            )
            .await?
            .expect("user detail");
        assert!(detail.complimentary_premium);
        assert!(!detail.is_review_account);
        assert_eq!(detail.user.plan, "premium");
        assert_eq!(detail.continuation_quota.expect("quota").weekly_limit, None);
        let no_payment: bool = sqlx::query_scalar(
            "SELECT NOT EXISTS(SELECT 1 FROM user_subscription WHERE user_id=$1)
             AND NOT EXISTS(SELECT 1 FROM billing_store_subscription WHERE user_id=$1)
             AND NOT EXISTS(SELECT 1 FROM billing_invoice WHERE user_id=$1)",
        )
        .bind(fixture.target)
        .fetch_one(fixture.database.acquire().await?.as_mut())
        .await?;
        assert!(no_payment);
        assert_eq!(fixture.set(false).await?, PremiumGrantResult::Updated);
        assert_eq!(fixture.set(false).await?, PremiumGrantResult::Unchanged);
        assert!(
            billing
                .effective_subscription_for_user(fixture.target, Utc::now())
                .await?
                .is_none()
        );
        let events: Vec<(String, Uuid)> = sqlx::query_as(
            "SELECT action,accessor_id FROM data_access_log WHERE accessed_user_id=$1
             AND action IN ('admin_grant_premium','admin_revoke_premium') ORDER BY accessed_at,id",
        )
        .bind(fixture.target)
        .fetch_all(fixture.database.acquire().await?.as_mut())
        .await?;
        assert_eq!(
            events,
            vec![
                ("admin_grant_premium".into(), fixture.actor),
                ("admin_revoke_premium".into(), fixture.actor)
            ]
        );
        Ok(())
    }
    .await;
    fixture.finish(result).await
}

#[tokio::test]
async fn gifts_and_revocation_never_replace_or_cancel_a_paid_subscription() -> TestResult {
    let fixture = Fixture::new().await?;
    let result: TestResult = async {
        let provider_id = format!("sub_{}", Uuid::new_v4().simple());
        sqlx::query("INSERT INTO user_subscription(user_id,plan,provider,provider_subscription_id,status,current_period_ends_at) VALUES($1,'premium','stripe',$2,'active',clock_timestamp()+INTERVAL '30 days')")
            .bind(fixture.target).bind(&provider_id).execute(fixture.database.acquire().await?.as_mut()).await?;
        let billing = PgBillingRepository::new(fixture.database.clone());
        for enabled in [true, false] {
            assert_eq!(fixture.set(enabled).await?, PremiumGrantResult::Updated);
            let view = billing.effective_subscription_for_user(fixture.target, Utc::now()).await?.expect("paid subscription");
            assert!(view.access_granted);
            assert_eq!(view.provider, Some(SubscriptionProvider::Stripe));
        }
        fixture.set(true).await?;
        sqlx::query("UPDATE user_subscription SET status='canceled',current_period_ends_at=clock_timestamp()-INTERVAL '1 day' WHERE user_id=$1")
            .bind(fixture.target).execute(fixture.database.acquire().await?.as_mut()).await?;
        let view = billing.effective_subscription_for_user(fixture.target, Utc::now()).await?.expect("gift fallback");
        assert!(view.access_granted);
        assert_eq!(view.provider, None);
        fixture.set(false).await?;
        assert!(!billing.effective_subscription_for_user(fixture.target, Utc::now()).await?.expect("historical subscription").access_granted);
        let retained: bool = sqlx::query_scalar("SELECT provider_subscription_id=$2 AND provider='stripe' AND status='canceled' FROM user_subscription WHERE user_id=$1")
            .bind(fixture.target).bind(provider_id).fetch_one(fixture.database.acquire().await?.as_mut()).await?;
        assert!(retained);
        Ok(())
    }.await;
    fixture.finish(result).await
}

#[tokio::test]
async fn grants_recheck_database_permissions_and_protect_banned_deleted_and_review_accounts()
-> TestResult {
    let fixture = Fixture::new().await?;
    let result: TestResult = async {
        let store = PgAdministrationStore::new(fixture.database.clone());
        for enabled in [true, false] {
            assert_eq!(store.set_complimentary_premium(fixture.target, enabled, "Forged superadmin claim".into(), fixture.admin).await?, PremiumGrantResult::Forbidden);
        }
        assert_eq!(store.set_complimentary_premium(Uuid::new_v4(), true, "Missing target".into(), fixture.actor).await?, PremiumGrantResult::NotFound);
        fixture.set(true).await?;
        sqlx::query("UPDATE user_account SET is_banned=true WHERE user_id=$1").bind(fixture.target).execute(fixture.database.acquire().await?.as_mut()).await?;
        assert_eq!(fixture.set(true).await?, PremiumGrantResult::Forbidden);
        assert_eq!(fixture.set(false).await?, PremiumGrantResult::Updated);
        sqlx::query("UPDATE user_account SET is_banned=false,is_review_account=true WHERE user_id=$1").bind(fixture.target).execute(fixture.database.acquire().await?.as_mut()).await?;
        for enabled in [true, false] { assert_eq!(fixture.set(enabled).await?, PremiumGrantResult::Forbidden); }
        sqlx::query("UPDATE user_account SET is_review_account=false,deleted_at=clock_timestamp() WHERE user_id=$1").bind(fixture.target).execute(fixture.database.acquire().await?.as_mut()).await?;
        assert_eq!(fixture.set(true).await?, PremiumGrantResult::NotFound);
        Ok(())
    }.await;
    fixture.finish(result).await
}

#[tokio::test]
async fn a_failed_audit_rolls_back_the_premium_grant() -> TestResult {
    let fixture = Fixture::new().await?;
    let result: TestResult = async {
        assert!(
            PgAdministrationStore::new(fixture.database.clone())
                .set_complimentary_premium(
                    fixture.target,
                    true,
                    "Unpersistable\0audit".into(),
                    fixture.actor
                )
                .await
                .is_err()
        );
        let absent: bool = sqlx::query_scalar(
            "SELECT NOT EXISTS(SELECT 1 FROM complimentary_premium_grant WHERE user_id=$1)",
        )
        .bind(fixture.target)
        .fetch_one(fixture.database.acquire().await?.as_mut())
        .await?;
        assert!(absent);
        Ok(())
    }
    .await;
    fixture.finish(result).await
}

#[tokio::test]
async fn granting_waits_for_the_same_wallet_lock_as_match_confirmation() -> TestResult {
    use sqlx::Acquire as _;
    use std::time::Duration;
    let fixture = Fixture::new().await?;
    let result: TestResult = async {
        let mut connection = fixture.database.acquire().await?;
        let mut confirmation = connection.begin().await?;
        lock_wallet(&mut confirmation, fixture.target).await?;
        let store = PgAdministrationStore::new(fixture.database.clone());
        let (target, actor) = (fixture.target, fixture.actor);
        let pending = tokio::spawn(async move { store.set_complimentary_premium(target, true, "Concurrent grant".into(), actor).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE usename=current_user AND wait_event_type='Lock' AND query LIKE 'SELECT pg_advisory_xact_lock%')")
                    .fetch_one(fixture.database.acquire().await?.as_mut()).await?;
                if waiting { return Ok::<_, Box<dyn std::error::Error>>(()); }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await??;
        assert!(!pending.is_finished());
        confirmation.commit().await?;
        assert_eq!(pending.await??, PremiumGrantResult::Updated);
        Ok(())
    }.await;
    fixture.finish(result).await
}

#[tokio::test]
async fn anonymization_removes_the_complimentary_grant() -> TestResult {
    let fixture = Fixture::new().await?;
    let result: TestResult = async {
        fixture.set(true).await?;
        sqlx::query("SELECT fct_anonymize_user($1)")
            .bind(fixture.target)
            .execute(fixture.database.acquire().await?.as_mut())
            .await?;
        let absent: bool = sqlx::query_scalar(
            "SELECT NOT EXISTS(SELECT 1 FROM complimentary_premium_grant WHERE user_id=$1)",
        )
        .bind(fixture.target)
        .fetch_one(fixture.database.acquire().await?.as_mut())
        .await?;
        assert!(absent);
        Ok(())
    }
    .await;
    fixture.finish(result).await
}
