use super::{
    domain::*,
    store::{AdminMetricsStore, MetricsFuture},
};
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use chrono::{DateTime, TimeDelta, Utc};
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
        type Row = (Option<DateTime<Utc>>, DateTime<Utc>, i64, i32, i64, String);
        let row = sqlx::query_as::<_, Row>(
            "WITH anchor AS (
               SELECT clock_timestamp() AS now_utc, clock_timestamp() AT TIME ZONE 'Europe/Paris' AS paris_now
             ), bounds AS (
               SELECT CASE $1::text
                 WHEN 'last_7_days' THEN now_utc - INTERVAL '7 days'
                 WHEN 'last_30_days' THEN now_utc - INTERVAL '30 days'
                 WHEN 'month_to_date' THEN date_trunc('month', paris_now) AT TIME ZONE 'Europe/Paris'
                 WHEN 'previous_month' THEN date_trunc('month', paris_now - INTERVAL '1 month') AT TIME ZONE 'Europe/Paris'
                 WHEN 'year_to_date' THEN date_trunc('year', paris_now) AT TIME ZONE 'Europe/Paris'
                 WHEN 'all_time' THEN NULL END AS period_start,
                 CASE $1::text WHEN 'previous_month' THEN date_trunc('month', paris_now) AT TIME ZONE 'Europe/Paris' ELSE now_utc END AS period_end
               FROM anchor)
             SELECT bounds.period_start, bounds.period_end,
               count(subscription.user_id)::bigint,
               COALESCE(max(plan.monthly_price_cents), 0)::int,
               (count(subscription.user_id) * COALESCE(max(plan.monthly_price_cents), 0))::bigint,
               COALESCE(max(plan.currency), 'EUR')::text
             FROM bounds
             LEFT JOIN subscription_plan plan ON plan.code='premium'
             LEFT JOIN user_subscription subscription ON subscription.plan=plan.code
               AND (bounds.period_start IS NULL OR subscription.updated_at >= bounds.period_start)
               AND subscription.updated_at < bounds.period_end
             GROUP BY bounds.period_start, bounds.period_end",
        ).bind(period.as_str()).fetch_one(self.database.pool()).await.map_err(map_sqlx_error)?;
        Ok(AdminRevenue {
            period,
            period_start: row.0,
            period_end: row.1,
            premium_subscriptions: row.2,
            price_per_subscription_cents: row.3,
            estimated_revenue_cents: row.4,
            currency: row.5,
            basis: "premium_monthly_price",
        })
    }

    async fn metrics_impl(
        &self,
        period: RevenuePeriod,
    ) -> Result<AdminBusinessMetrics, DatabaseError> {
        let users = sqlx::query_as::<_, (i64, i64, i64, i64, i64)>(
            "SELECT count(*)::bigint,
               count(*) FILTER (WHERE NOT is_banned)::bigint,
               count(*) FILTER (WHERE is_banned)::bigint,
               count(*) FILTER (WHERE role <> 'user' OR (
                 EXISTS (SELECT 1 FROM user_consent WHERE user_id=user_account.user_id AND consent_type='terms_of_service_acceptance' AND granted AND withdrawn_at IS NULL AND document_version=$1)
                 AND EXISTS (SELECT 1 FROM user_consent WHERE user_id=user_account.user_id AND consent_type='privacy_notice_acknowledgement' AND granted AND withdrawn_at IS NULL AND document_version=$2)))::bigint,
               count(*) FILTER (WHERE created_at >= now() - INTERVAL '30 days')::bigint
             FROM user_account WHERE deleted_at IS NULL",
        ).bind(&*self.terms_version).bind(&*self.privacy_version).fetch_one(self.database.pool()).await.map_err(map_sqlx_error)?;
        let moderation = sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT (SELECT count(*) FROM user_report WHERE status='pending')::bigint,
                    (SELECT count(*) FROM content_moderation_case WHERE status='pending')::bigint,
                    (SELECT count(*) FROM data_subject_request WHERE status IN ('pending','in_progress'))::bigint",
        ).fetch_one(self.database.pool()).await.map_err(map_sqlx_error)?;
        let mut matches = MatchMetrics::default();
        for (status, count) in sqlx::query_as::<_, (String, i64)>(
            "SELECT status::text, count(*)::bigint FROM match_init GROUP BY status",
        )
        .fetch_all(self.database.pool())
        .await
        .map_err(map_sqlx_error)?
        {
            match status.as_str() {
                "active" => matches.active = count,
                "awaiting_continuation" => matches.awaiting_continuation = count,
                "confirmed" => matches.confirmed = count,
                "expired" => matches.expired = count,
                "ended" => matches.ended = count,
                _ => return Err(DatabaseError::QueryFailed),
            }
        }
        let messages: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM chat_message")
            .fetch_one(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
        let stale_before = Utc::now() - TimeDelta::minutes(30);
        let photos = sqlx::query_as::<_, (i64,i64,i64,i64,i64,i64,i64)>(
            "SELECT count(*) FILTER (WHERE photo.status='pending')::bigint,
              count(*) FILTER (WHERE photo.status='processing')::bigint,
              count(*) FILTER (WHERE photo.status='ready')::bigint,
              count(*) FILTER (WHERE photo.status='deleting')::bigint,
              count(*) FILTER (WHERE photo.status IN ('pending','processing') AND photo.updated_at <= $1)::bigint,
              count(*) FILTER (WHERE photo.status='deleting' AND event.status='dead_letter')::bigint,
              count(*) FILTER (WHERE photo.status='deleting' AND (event.id IS NULL OR event.status='completed'))::bigint
             FROM user_photo photo LEFT JOIN outbox_event event ON event.event_type='photo.delete' AND event.aggregate_id=photo.id",
        ).bind(stale_before).fetch_one(self.database.pool()).await.map_err(map_sqlx_error)?;
        let subscriptions = sqlx::query_as::<_, (String,i64)>(
            "WITH account_plans AS MATERIALIZED (
               SELECT COALESCE(subscription.plan,'free') AS plan, count(*)::bigint AS users
               FROM user_account account LEFT JOIN user_subscription subscription ON subscription.user_id=account.user_id
               WHERE account.deleted_at IS NULL GROUP BY COALESCE(subscription.plan,'free'))
             SELECT plan.code, COALESCE(account_plans.users,0)::bigint FROM subscription_plan plan
             LEFT JOIN account_plans ON account_plans.plan=plan.code ORDER BY plan.code",
        ).fetch_all(self.database.pool()).await.map_err(map_sqlx_error)?;
        Ok(AdminBusinessMetrics {
            users: UserMetrics {
                total: users.0,
                active: users.1,
                banned: users.2,
                onboarded: users.3,
                created_last_30_days: users.4,
            },
            moderation: ModerationMetrics {
                pending_reports: moderation.0,
                pending_content: moderation.1,
                open_data_requests: moderation.2,
            },
            matches,
            messages: MessageMetrics { total: messages },
            photos: PhotoMetrics {
                pending: photos.0,
                processing: photos.1,
                ready: photos.2,
                deleting: photos.3,
                stale_processing: photos.4,
                deletion_dead_letters: photos.5,
                deletion_without_active_event: photos.6,
            },
            subscriptions: subscriptions
                .into_iter()
                .map(|(plan, users)| SubscriptionMetrics { plan, users })
                .collect(),
            revenue: self.revenue_impl(period).await?,
        })
    }
}

impl AdminMetricsStore for PgAdminMetricsRepository {
    fn revenue(&self, period: RevenuePeriod) -> MetricsFuture<'_, AdminRevenue> {
        Box::pin(self.revenue_impl(period))
    }
    fn metrics(&self, period: RevenuePeriod) -> MetricsFuture<'_, AdminBusinessMetrics> {
        Box::pin(self.metrics_impl(period))
    }
}
