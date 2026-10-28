//! Complimentary billing rights are verified independently of OTP authentication.

use chrono::{TimeDelta, Utc};
use histae_api_rust::{
    billing::{
        domain::{BeginCheckoutResult, BillingPeriod, SubscriptionProvider, SubscriptionView},
        pg::PgBillingRepository,
        store::{BeginCheckoutInput, BillingStore},
        stores::pg::StoreRepository,
    },
    config::SecretString,
    identity::mobile::{
        pg::MobileSessionRepository, play_review::NewReviewAccount, store::MobileSessionStore,
        tokens::TokenService,
    },
    infra::postgres::Database,
    matches::{pg::PgMatchRepository, store::MatchStore},
};
use uuid::Uuid;

use super::{TestResult, database, remove, review_account, signup_intent};

async fn register_review_account(
    database: &Database,
    tokens: &TokenService,
    account: &NewReviewAccount,
) -> TestResult<Uuid> {
    signup_intent(database, account, true).await?;
    Ok(MobileSessionRepository::new(database.clone())
        .create_review_session(account.clone(), tokens.new_refresh_token()?)
        .await?
        .expect("review signup")
        .user_id)
}

fn assert_complimentary_premium(subscription: &SubscriptionView) {
    assert_eq!(subscription.plan, "premium");
    assert!(subscription.access_granted);
    assert_eq!(subscription.provider, None);
    assert_eq!(subscription.status, None);
    assert_eq!(subscription.billing_period, None);
    assert_eq!(subscription.current_period_ends_at, None);
    assert!(!subscription.customer_portal_available);
    assert_eq!(subscription.management_url, None);
}

async fn assert_no_purchase(database: &Database, user_id: Uuid) -> TestResult {
    let stores = StoreRepository::new(database.clone(), SecretString::new("01".repeat(32)));
    assert!(!stores.purchase_allowed(user_id).await?);

    let now = Utc::now();
    let checkout = PgBillingRepository::new(database.clone())
        .begin_checkout(BeginCheckoutInput {
            user_id,
            idempotency_key: Uuid::new_v4(),
            billing_period: BillingPeriod::Monthly,
            attempt_id: Uuid::new_v4(),
            now,
            expires_at: now + TimeDelta::minutes(35),
            stale_before: now - TimeDelta::seconds(60),
        })
        .await?;
    assert_eq!(checkout, BeginCheckoutResult::AlreadySubscribed);

    let no_billing: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (SELECT 1 FROM user_subscription WHERE user_id=$1)
         AND NOT EXISTS (SELECT 1 FROM billing_store_account WHERE user_id=$1)
         AND NOT EXISTS (SELECT 1 FROM billing_store_subscription WHERE user_id=$1)
         AND NOT EXISTS (SELECT 1 FROM billing_checkout_session WHERE user_id=$1)
         AND NOT EXISTS (SELECT 1 FROM billing_customer WHERE user_id=$1)
         AND NOT EXISTS (SELECT 1 FROM billing_invoice WHERE user_id=$1)",
    )
    .bind(user_id)
    .fetch_one(database.acquire().await?.as_mut())
    .await?;
    assert!(
        no_billing,
        "complimentary premium never creates a purchase or invoice"
    );
    Ok(())
}

#[tokio::test]
async fn new_and_existing_reviewers_receive_complimentary_premium_without_a_purchase() -> TestResult
{
    let (database, tokens) = database().await?;
    let billing = PgBillingRepository::new(database.clone());
    let sessions = MobileSessionRepository::new(database.clone());

    for already_registered in [false, true] {
        let account = review_account();
        let result: TestResult = async {
            let user_id = if already_registered {
                // Simulate a reviewer registered before complimentary access was added.
                sqlx::query(
                    "INSERT INTO user_account
                     (user_id,phone_number_hash,phone_number_encrypted,is_review_account)
                     VALUES($1,$2,$3,true)",
                )
                .bind(account.user_id)
                .bind(&account.phone_hash)
                .bind(&account.encrypted_phone)
                .execute(database.acquire().await?.as_mut())
                .await?;
                account.user_id
            } else {
                register_review_account(&database, &tokens, &account).await?
            };

            let now = Utc::now();
            let subscription = billing
                .effective_subscription_for_user(user_id, now)
                .await?
                .expect("complimentary premium");
            assert_complimentary_premium(&subscription);

            let quota = PgMatchRepository::new(database.clone())
                .quota_snapshot(user_id, now)
                .await?;
            assert_eq!(quota.plan, "premium");
            assert_eq!(quota.weekly_limit, None);
            assert_eq!(quota.remaining, None);
            assert_no_purchase(&database, user_id).await?;

            let signed_in = sessions
                .create_review_session(account.clone(), tokens.new_refresh_token()?)
                .await?
                .expect("review login");
            assert_eq!(signed_in.user_id, user_id);
            assert_eq!(
                billing
                    .effective_subscription_for_user(user_id, Utc::now())
                    .await?
                    .as_ref(),
                Some(&subscription)
            );

            sqlx::query("UPDATE user_account SET is_review_account=false WHERE user_id=$1")
                .bind(user_id)
                .execute(database.acquire().await?.as_mut())
                .await?;
            assert!(
                billing
                    .effective_subscription_for_user(user_id, Utc::now())
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
async fn complimentary_review_access_preserves_real_provider_subscription_history() -> TestResult {
    let (database, tokens) = database().await?;
    let account = review_account();
    let billing = PgBillingRepository::new(database.clone());

    let result: TestResult = async {
        let user_id = register_review_account(&database, &tokens, &account).await?;
        let provider_id = format!("sub_{}", Uuid::new_v4().simple());
        sqlx::query(
            "INSERT INTO user_subscription
             (user_id,plan,provider,provider_subscription_id,status,current_period_ends_at)
             VALUES($1,'premium','stripe',$2,'active',clock_timestamp()+INTERVAL '30 days')",
        )
        .bind(user_id)
        .bind(&provider_id)
        .execute(database.acquire().await?.as_mut())
        .await?;
        let paid = billing
            .effective_subscription_for_user(user_id, Utc::now())
            .await?
            .expect("paid subscription");
        assert!(paid.access_granted);
        assert_eq!(paid.provider, Some(SubscriptionProvider::Stripe));

        sqlx::query(
            "UPDATE user_subscription
             SET status='canceled',current_period_ends_at=clock_timestamp()-INTERVAL '1 day'
             WHERE user_id=$1",
        )
        .bind(user_id)
        .execute(database.acquire().await?.as_mut())
        .await?;
        let complimentary = billing
            .effective_subscription_for_user(user_id, Utc::now())
            .await?
            .expect("complimentary fallback");
        assert_complimentary_premium(&complimentary);

        let retained: bool = sqlx::query_scalar(
            "SELECT provider='stripe' AND provider_subscription_id=$2 AND status='canceled'
             AND current_period_ends_at<clock_timestamp() FROM user_subscription WHERE user_id=$1",
        )
        .bind(user_id)
        .bind(provider_id)
        .fetch_one(database.acquire().await?.as_mut())
        .await?;
        assert!(
            retained,
            "complimentary access must not overwrite a provider projection"
        );
        Ok(())
    }
    .await;
    remove(&database, &account.phone_hash).await?;
    result
}
