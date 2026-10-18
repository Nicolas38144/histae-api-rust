use super::{
    domain::*,
    store::{AdminMetricsStore, MetricsFuture},
};
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use chrono::{DateTime, TimeDelta, Utc};
use sqlx::{Row, postgres::PgRow, types::Json};
use std::sync::Arc;

#[derive(Clone)]
pub struct PgAdminMetricsRepository {
    database: Database,
    terms_version: Arc<str>,
    privacy_version: Arc<str>,
}

impl PgAdminMetricsRepository {
    pub fn new(database: Database, terms_version: String, privacy_version: String) -> Self {
        Self {
            database,
            terms_version: terms_version.into(),
            privacy_version: privacy_version.into(),
        }
    }

    async fn revenue_impl(&self, period: RevenuePeriod) -> Result<AdminRevenue, DatabaseError> {
        let query = format!("{REVENUE_CTES_SQL} SELECT * FROM revenue");
        let row = sqlx::query(&query)
            .bind(period.as_str())
            .fetch_one(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
        revenue_from_row(&row, period)
    }

    async fn metrics_impl(
        &self,
        period: RevenuePeriod,
    ) -> Result<AdminBusinessMetrics, DatabaseError> {
        // One snapshot and one pool acquisition; account totals and plan distribution
        // share a scan, and active consents are joined instead of probed per account.
        let query = format!("{REVENUE_CTES_SQL}{METRICS_SQL}");
        let row = sqlx::query(&query)
            .bind(period.as_str())
            .bind(&*self.terms_version)
            .bind(&*self.privacy_version)
            .bind(Utc::now() - TimeDelta::minutes(30))
            .fetch_one(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
        let match_counts: Json<Vec<(String, i64)>> =
            row.try_get("match_counts").map_err(map_sqlx_error)?;
        let mut matches = MatchMetrics::default();
        for (status, total) in match_counts.0 {
            match status.as_str() {
                "active" => matches.active = total,
                "awaiting_continuation" => matches.awaiting_continuation = total,
                "confirmed" => matches.confirmed = total,
                "expired" => matches.expired = total,
                "ended" => matches.ended = total,
                _ => return Err(DatabaseError::QueryFailed),
            }
        }
        let subscriptions: Json<Vec<(String, i64)>> =
            row.try_get("subscriptions").map_err(map_sqlx_error)?;
        let subscription_offers: Json<Vec<(String, i64)>> =
            row.try_get("subscription_offers").map_err(map_sqlx_error)?;
        Ok(AdminBusinessMetrics {
            users: UserMetrics {
                total: row.try_get("users_total").map_err(map_sqlx_error)?,
                active: row.try_get("users_active").map_err(map_sqlx_error)?,
                banned: row.try_get("users_banned").map_err(map_sqlx_error)?,
                onboarded: row.try_get("users_onboarded").map_err(map_sqlx_error)?,
                created_last_30_days: row
                    .try_get("users_created_last_30_days")
                    .map_err(map_sqlx_error)?,
            },
            moderation: ModerationMetrics {
                pending_reports: row.try_get("pending_reports").map_err(map_sqlx_error)?,
                pending_content: row.try_get("pending_content").map_err(map_sqlx_error)?,
                open_data_requests: row.try_get("open_data_requests").map_err(map_sqlx_error)?,
            },
            matches,
            messages: MessageMetrics {
                total: row.try_get("messages_total").map_err(map_sqlx_error)?,
            },
            photos: PhotoMetrics {
                pending: row.try_get("photos_pending").map_err(map_sqlx_error)?,
                processing: row.try_get("photos_processing").map_err(map_sqlx_error)?,
                ready: row.try_get("photos_ready").map_err(map_sqlx_error)?,
                deleting: row.try_get("photos_deleting").map_err(map_sqlx_error)?,
                stale_processing: row
                    .try_get("photos_stale_processing")
                    .map_err(map_sqlx_error)?,
                deletion_dead_letters: row
                    .try_get("photos_deletion_dead_letters")
                    .map_err(map_sqlx_error)?,
                deletion_without_active_event: row
                    .try_get("photos_deletion_without_active_event")
                    .map_err(map_sqlx_error)?,
            },
            subscriptions: subscriptions
                .0
                .into_iter()
                .map(|(plan, users)| SubscriptionMetrics { plan, users })
                .collect(),
            subscription_offers: subscription_offers
                .0
                .into_iter()
                .map(|(offer, users)| SubscriptionOfferMetrics { offer, users })
                .collect(),
            revenue: revenue_from_row(&row, period)?,
        })
    }
}

fn revenue_from_row(row: &PgRow, period: RevenuePeriod) -> Result<AdminRevenue, DatabaseError> {
    Ok(AdminRevenue {
        period,
        period_start: row
            .try_get::<Option<DateTime<Utc>>, _>("period_start")
            .map_err(map_sqlx_error)?,
        period_end: row.try_get("period_end").map_err(map_sqlx_error)?,
        premium_subscriptions: row
            .try_get("premium_subscriptions")
            .map_err(map_sqlx_error)?,
        paid_invoices: row.try_get("paid_invoices").map_err(map_sqlx_error)?,
        estimated_revenue_cents: row
            .try_get("estimated_revenue_cents")
            .map_err(map_sqlx_error)?,
        currency: row.try_get("currency").map_err(map_sqlx_error)?,
        excluded_currency_invoices: row
            .try_get("excluded_currency_invoices")
            .map_err(map_sqlx_error)?,
        basis: "stripe_paid_invoices",
    })
}

// $1 is the period in both callers. Capture the anchor once, including the Paris
// calendar boundaries; revenue intentionally also includes deleted accounts.
const REVENUE_CTES_SQL: &str = r#"
WITH anchor AS MATERIALIZED (
  SELECT clock_timestamp() AS now_utc
), bounds AS (
  SELECT CASE $1::text
    WHEN 'last_7_days' THEN now_utc - INTERVAL '7 days'
    WHEN 'last_30_days' THEN now_utc - INTERVAL '30 days'
    WHEN 'month_to_date' THEN date_trunc('month', now_utc AT TIME ZONE 'Europe/Paris') AT TIME ZONE 'Europe/Paris'
    WHEN 'previous_month' THEN date_trunc('month', (now_utc AT TIME ZONE 'Europe/Paris') - INTERVAL '1 month') AT TIME ZONE 'Europe/Paris'
    WHEN 'year_to_date' THEN date_trunc('year', now_utc AT TIME ZONE 'Europe/Paris') AT TIME ZONE 'Europe/Paris'
    WHEN 'all_time' THEN NULL END AS period_start,
    CASE $1::text WHEN 'previous_month' THEN date_trunc('month', now_utc AT TIME ZONE 'Europe/Paris') AT TIME ZONE 'Europe/Paris'
      ELSE now_utc END AS period_end
  FROM anchor
), revenue AS (
  SELECT bounds.period_start, bounds.period_end,
    count(DISTINCT invoice.stripe_subscription_id) FILTER (WHERE invoice.currency = COALESCE(plan.currency, 'EUR'))::bigint AS premium_subscriptions,
    count(invoice.stripe_invoice_id) FILTER (WHERE invoice.currency = COALESCE(plan.currency, 'EUR'))::bigint AS paid_invoices,
    COALESCE(sum(invoice.amount_paid) FILTER (WHERE invoice.currency = COALESCE(plan.currency, 'EUR')), 0)::bigint AS estimated_revenue_cents,
    COALESCE(plan.currency, 'EUR')::text AS currency,
    count(invoice.stripe_invoice_id) FILTER (WHERE invoice.currency <> COALESCE(plan.currency, 'EUR'))::bigint AS excluded_currency_invoices
  FROM bounds
  LEFT JOIN subscription_plan plan ON plan.code = 'premium'
  LEFT JOIN billing_invoice invoice ON invoice.status = 'paid' AND invoice.amount_paid > 0
    AND invoice.stripe_subscription_id IS NOT NULL
    AND (bounds.period_start IS NULL OR invoice.paid_at >= bounds.period_start)
    AND invoice.paid_at < bounds.period_end
  GROUP BY bounds.period_start, bounds.period_end, plan.currency
)
"#;

const METRICS_SQL: &str = r#"
, account_plans AS MATERIALIZED (
  SELECT COALESCE(subscription.plan, 'free') AS plan,
    CASE WHEN subscription.plan = 'premium' THEN subscription.billing_period END AS billing_period,
    count(*)::bigint AS total,
    count(*) FILTER (WHERE NOT account.is_banned)::bigint AS active,
    count(*) FILTER (WHERE account.is_banned)::bigint AS banned,
    count(*) FILTER (WHERE account.role <> 'user'
      OR (terms.user_id IS NOT NULL AND privacy.user_id IS NOT NULL))::bigint AS onboarded,
    count(*) FILTER (WHERE account.created_at >= now() - INTERVAL '30 days')::bigint AS created_last_30_days
  FROM user_account account
  LEFT JOIN user_subscription subscription ON subscription.user_id = account.user_id AND (subscription.plan='free'
    OR (subscription.provider IS NULL AND (subscription.current_period_ends_at IS NULL OR subscription.current_period_ends_at > clock_timestamp()))
    OR (subscription.provider='stripe' AND subscription.status IN ('trialing','active','past_due')
        AND subscription.current_period_ends_at > clock_timestamp()))
  LEFT JOIN user_consent terms ON terms.user_id = account.user_id
    AND terms.consent_type = 'terms_of_service_acceptance' AND terms.granted
    AND terms.withdrawn_at IS NULL AND terms.document_version = $2
  LEFT JOIN user_consent privacy ON privacy.user_id = account.user_id
    AND privacy.consent_type = 'privacy_notice_acknowledgement' AND privacy.granted
    AND privacy.withdrawn_at IS NULL AND privacy.document_version = $3
  WHERE account.deleted_at IS NULL
  GROUP BY COALESCE(subscription.plan, 'free'),
    CASE WHEN subscription.plan = 'premium' THEN subscription.billing_period END
), users AS (
  SELECT COALESCE(sum(total), 0)::bigint AS users_total,
    COALESCE(sum(active), 0)::bigint AS users_active,
    COALESCE(sum(banned), 0)::bigint AS users_banned,
    COALESCE(sum(onboarded), 0)::bigint AS users_onboarded,
    COALESCE(sum(created_last_30_days), 0)::bigint AS users_created_last_30_days
  FROM account_plans
), plan_counts AS (
  SELECT plan, sum(total)::bigint AS total FROM account_plans GROUP BY plan
), subscriptions AS (
  SELECT COALESCE(jsonb_agg(jsonb_build_array(plan.code, COALESCE(plan_counts.total, 0)) ORDER BY plan.code), '[]'::jsonb) AS subscriptions
  FROM subscription_plan plan LEFT JOIN plan_counts ON plan_counts.plan = plan.code
), subscription_offers AS (
  SELECT jsonb_build_array(
    jsonb_build_array('free', COALESCE(sum(total) FILTER (WHERE plan = 'free'), 0)::bigint),
    jsonb_build_array('premium_monthly', COALESCE(sum(total) FILTER (WHERE plan = 'premium' AND billing_period = 'monthly'), 0)::bigint),
    jsonb_build_array('premium_annual', COALESCE(sum(total) FILTER (WHERE plan = 'premium' AND billing_period = 'annual'), 0)::bigint),
    jsonb_build_array('premium_unspecified', COALESCE(sum(total) FILTER (WHERE plan = 'premium' AND billing_period IS NULL), 0)::bigint)
  ) AS subscription_offers
  FROM account_plans
), matches AS (
  SELECT COALESCE(jsonb_agg(jsonb_build_array(status, total) ORDER BY status), '[]'::jsonb) AS match_counts
  FROM (SELECT status, count(*)::bigint AS total FROM match_init GROUP BY status) counts
), photos AS (
  SELECT count(*) FILTER (WHERE photo.status = 'pending')::bigint AS photos_pending,
    count(*) FILTER (WHERE photo.status = 'processing')::bigint AS photos_processing,
    count(*) FILTER (WHERE photo.status = 'ready')::bigint AS photos_ready,
    count(*) FILTER (WHERE photo.status = 'deleting')::bigint AS photos_deleting,
    count(*) FILTER (WHERE photo.status IN ('pending','processing') AND photo.updated_at <= $4)::bigint AS photos_stale_processing,
    count(*) FILTER (WHERE photo.status = 'deleting' AND event.status = 'dead_letter')::bigint AS photos_deletion_dead_letters,
    count(*) FILTER (WHERE photo.status = 'deleting' AND (event.id IS NULL OR event.status = 'completed'))::bigint AS photos_deletion_without_active_event
  FROM user_photo photo LEFT JOIN outbox_event event
    ON photo.status = 'deleting' AND event.event_type = 'photo.delete' AND event.aggregate_id = photo.id
)
SELECT users.*, matches.*, photos.*, subscriptions.*, subscription_offers.*, revenue.*,
  (SELECT count(*) FROM user_report WHERE status = 'pending')::bigint AS pending_reports,
  (SELECT count(*) FROM content_moderation_case WHERE status = 'pending')::bigint AS pending_content,
  (SELECT count(*) FROM data_subject_request WHERE status IN ('pending','in_progress'))::bigint AS open_data_requests,
  (SELECT count(*) FROM chat_message)::bigint AS messages_total
FROM users CROSS JOIN matches CROSS JOIN photos CROSS JOIN subscriptions CROSS JOIN subscription_offers CROSS JOIN revenue
"#;

impl AdminMetricsStore for PgAdminMetricsRepository {
    fn revenue(&self, period: RevenuePeriod) -> MetricsFuture<'_, AdminRevenue> {
        Box::pin(self.revenue_impl(period))
    }
    fn metrics(&self, period: RevenuePeriod) -> MetricsFuture<'_, AdminBusinessMetrics> {
        Box::pin(self.metrics_impl(period))
    }
}
