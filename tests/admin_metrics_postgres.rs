#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use histae_api_rust::administration::metrics::{
    AdminBusinessMetrics, AdminMetricsStore, PgAdminMetricsRepository, RevenuePeriod,
};
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use uuid::Uuid;
#[path = "support/store_subscription.rs"]
mod paid_store;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "S28 PostgreSQL fixture error ({})", self.0)
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
        application_name: "histae-rust-s28-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
    })
}

async fn insert_premium_account(database: &Database, user_id: Uuid) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO user_account
         (user_id, role, phone_number_hash, phone_number_encrypted)
         VALUES ($1, 'user', $2, $3)",
    )
    .bind(user_id)
    .bind(format!("s28-{user_id}"))
    .bind(Vec::<u8>::new())
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query("INSERT INTO user_subscription (user_id, plan) VALUES ($1, 'premium')")
        .bind(user_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

async fn cleanup(database: &Database, user_id: Uuid) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM user_account WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

#[tokio::test]
async fn admin_metrics_and_revenue_execute_against_the_real_schema()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?)
        .await
        .map_err(|error| FixtureError(error.safe_code()))?;
    let user_id = Uuid::new_v4();
    cleanup(&database, user_id).await?;
    insert_premium_account(&database, user_id).await?;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let repository = PgAdminMetricsRepository::new(
            database.clone(),
            env::var("TERMS_OF_SERVICE_VERSION")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "development-unversioned".to_owned()),
            env::var("PRIVACY_POLICY_VERSION")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "development-unversioned".to_owned()),
        );
        let revenue = repository.revenue(RevenuePeriod::Last7Days).await?;
        assert_eq!(revenue.period, RevenuePeriod::Last7Days);
        assert!(revenue.period_start.is_some());
        assert_eq!(revenue.basis, "store_financial_reports");
        assert_eq!(revenue.currency, "EUR");

        let metrics = repository.metrics(RevenuePeriod::MonthToDate).await?;
        assert!(metrics.users.total >= 1);
        assert!(metrics.users.active >= 1);
        assert!(
            metrics
                .subscriptions
                .iter()
                .any(|entry| entry.plan == "premium" && entry.users >= 1)
        );
        assert_eq!(metrics.revenue.period, RevenuePeriod::MonthToDate);
        assert_eq!(metrics.subscription_offers.len(), 4);
        assert!(
            metrics
                .subscription_offers
                .iter()
                .any(|entry| entry.offer == "premium_unspecified" && entry.users >= 1)
        );
        let mut connection = database.acquire().await?;
        let expected_users: (i64, i64, i64) = sqlx::query_as(
            "SELECT count(*)::bigint, count(*) FILTER (WHERE NOT is_banned)::bigint,
                    count(*) FILTER (WHERE is_banned)::bigint
             FROM user_account WHERE deleted_at IS NULL",
        )
        .fetch_one(&mut *connection)
        .await?;
        assert_eq!(
            (
                metrics.users.total,
                metrics.users.active,
                metrics.users.banned
            ),
            expected_users
        );
        for (status, count) in sqlx::query_as::<_, (String, i64)>(
            "SELECT status, count(*)::bigint FROM match_init GROUP BY status",
        )
        .fetch_all(&mut *connection)
        .await?
        {
            let actual = match status.as_str() {
                "active" => metrics.matches.active,
                "awaiting_continuation" => metrics.matches.awaiting_continuation,
                "confirmed" => metrics.matches.confirmed,
                "expired" => metrics.matches.expired,
                "ended" => metrics.matches.ended,
                _ => {
                    return Err(
                        Box::new(FixtureError("match_status")) as Box<dyn std::error::Error>
                    );
                }
            };
            assert_eq!(actual, count);
        }
        let expected_premium: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM billing_store_subscription
             WHERE user_id IS NOT NULL AND NOT superseded AND starts_at<=clock_timestamp()
               AND expires_at>clock_timestamp() AND
               (status IN ('active','grace_period') OR (provider='google_play' AND status='canceled'))",
        )
        .fetch_one(&mut *connection)
        .await?;
        drop(connection);
        let all_time = repository.metrics(RevenuePeriod::AllTime).await?;
        assert_eq!(all_time.revenue.premium_subscriptions, expected_premium);
        assert!(all_time.revenue.period_start.is_none());
        let previous_month = repository.revenue(RevenuePeriod::PreviousMonth).await?;
        assert!(previous_month.period_start.is_some());
        assert!(previous_month.period_start < Some(previous_month.period_end));
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, user_id).await;
    result?;
    cleanup_result?;
    Ok(())
}

#[tokio::test]
async fn subscription_offer_metrics_follow_effective_access_and_billing_period()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?).await?;
    let repository = PgAdminMetricsRepository::new(database.clone(), "test".into(), "test".into());
    let count = |metrics: &AdminBusinessMetrics, offer: &str| {
        metrics
            .subscription_offers
            .iter()
            .find(|entry| entry.offer == offer)
            .expect("all offer counters are present, including zero counts")
            .users
    };
    let before = repository.metrics(RevenuePeriod::MonthToDate).await?;
    let user_id = Uuid::new_v4();
    insert_premium_account(&database, user_id).await?;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        // The manual fixture has no billing period: never guess monthly or annual.
        let manual = repository.metrics(RevenuePeriod::MonthToDate).await?;
        assert!(count(&manual, "premium_unspecified") >= 1);
        assert_eq!(
            count(&manual, "premium_monthly"),
            count(&before, "premium_monthly")
        );
        assert_eq!(
            count(&manual, "premium_annual"),
            count(&before, "premium_annual")
        );

        sqlx::query("DELETE FROM user_subscription WHERE user_id=$1")
            .bind(user_id).execute(database.acquire().await?.as_mut()).await?;
        paid_store::grant(&database,user_id,&Uuid::new_v4().to_string()).await?;
        for (period, status, expired, expected_monthly, expected_annual) in [
            ("monthly", "active", false, 1, 0),
            ("annual", "grace_period", false, 0, 1),
            ("annual", "past_due", false, 0, 0),
            ("annual", "active", true, 0, 0),
            ("monthly", "canceled", false, 0, 0),
        ] {
            let mut connection = database.acquire().await?;
            sqlx::query(
                "UPDATE billing_store_subscription SET billing_period=$2,status=$3,
                 expires_at=now()+CASE WHEN $4::boolean THEN -INTERVAL '1 day' ELSE INTERVAL '1 day' END
                 WHERE user_id=$1",            )
            .bind(user_id)
            .bind(period)
            .bind(status)
            .bind(expired)
            .execute(&mut *connection)
            .await?;
            drop(connection);
            let metrics = repository.metrics(RevenuePeriod::MonthToDate).await?;
            assert_eq!(
                count(&metrics, "premium_monthly"),
                count(&before, "premium_monthly") + expected_monthly
            );
            assert_eq!(
                count(&metrics, "premium_annual"),
                count(&before, "premium_annual") + expected_annual
            );
            assert_eq!(
                metrics
                    .subscription_offers
                    .iter()
                    .map(|entry| entry.users)
                    .sum::<i64>(),
                metrics.users.total
            );
            let premium = metrics
                .subscriptions
                .iter()
                .find(|entry| entry.plan == "premium")
                .expect("legacy Premium counter is retained");
            assert_eq!(
                premium.users,
                count(&metrics, "premium_monthly")
                    + count(&metrics, "premium_annual")
                    + count(&metrics, "premium_unspecified")
            );
        }
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, user_id).await;
    result?;
    cleanup_result?;
    Ok(())
}

#[tokio::test]
async fn revenue_uses_imported_store_reports_without_mixing_currencies_or_catalog_estimates()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?).await?;
    let repository = PgAdminMetricsRepository::new(database.clone(), "test".into(), "test".into());
    let before = repository.revenue(RevenuePeriod::Last7Days).await?;
    let before_all = repository.revenue(RevenuePeriod::AllTime).await?;
    let before_previous = repository.revenue(RevenuePeriod::PreviousMonth).await?;
    let report = Uuid::new_v4().to_string();
    let result:Result<(),Box<dyn std::error::Error>>=async {
  sqlx::query(r#"INSERT INTO billing_store_finance(provider,report_id,row_id,paid_at,currency,gross_cents,net_cents)
   SELECT 'app_store',$1,name,paid_at,currency,amount,NULL FROM (VALUES
    ('Monthly','EUR',500::bigint,clock_timestamp()-interval '1 minute'),
    ('Renewal','EUR',300,clock_timestamp()-interval '2 minutes'),
    ('Annual','EUR',4800,clock_timestamp()-interval '1 minute'),
    ('Other','EUR',500,clock_timestamp()-interval '1 minute'),
    ('Foreign','USD',900,clock_timestamp()-interval '1 minute'),
    ('Historical','EUR',500,clock_timestamp()-interval '50 days'),
    ('PreviousMonth','EUR',700,(date_trunc('month',clock_timestamp() AT TIME ZONE 'Europe/Paris')-interval '1 second') AT TIME ZONE 'Europe/Paris')
   ) entries(name,currency,amount,paid_at)"#).bind(&report)
    .execute(database.acquire().await?.as_mut()).await?;
  let revenue=repository.revenue(RevenuePeriod::Last7Days).await?;
  assert_eq!(revenue.gross_revenue_cents-before.gross_revenue_cents,6100);
  assert_eq!(revenue.financial_rows-before.financial_rows,4);
  assert_eq!(revenue.excluded_currency_rows-before.excluded_currency_rows,1);
  assert_eq!(revenue.basis,"store_financial_reports");
  let all=repository.metrics(RevenuePeriod::AllTime).await?.revenue;
  assert_eq!(all.gross_revenue_cents-before_all.gross_revenue_cents,7300);
  assert_eq!(all.financial_rows-before_all.financial_rows,6);
  let previous=repository.revenue(RevenuePeriod::PreviousMonth).await?;
  // Fifty days ago may also fall in the previous calendar month.
  let expected:i64=sqlx::query_scalar("SELECT COALESCE(sum(gross_cents),0)::bigint FROM billing_store_finance
    WHERE report_id=$1 AND currency='EUR' AND paid_at>=$2 AND paid_at<$3")
    .bind(&report).bind(previous.period_start).bind(previous.period_end)
    .fetch_one(database.acquire().await?.as_mut()).await?;
  assert_eq!(previous.gross_revenue_cents-before_previous.gross_revenue_cents,expected);
  Ok(())
 }.await;
    sqlx::query("DELETE FROM billing_store_finance WHERE report_id=$1")
        .bind(report)
        .execute(database.acquire().await?.as_mut())
        .await?;
    result
}
