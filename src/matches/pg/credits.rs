//! Wallet operations share ordered per-user transaction locks with advertising rewards.
use super::matches::effective_plan_on;
use crate::infra::postgres::{DatabaseError, map_sqlx_error};
use crate::matches::{
    domain::{ContinuationQuota, FREE_WEEKLY_CREDITS, start_of_utc_week},
    store::MatchStoreError,
};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

pub use crate::matches::domain::{ADVERTISING_STOCK_LIMIT, FREE_WEEKLY_CONTINUATION_LIMIT};

pub async fn lock_wallet(
    connection: &mut PgConnection,
    user_id: Uuid,
) -> Result<(), DatabaseError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 86302004))")
        .bind(user_id.to_string())
        .execute(connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

pub async fn quota_on(
    connection: &mut PgConnection,
    user_id: Uuid,
    now: DateTime<Utc>,
) -> Result<ContinuationQuota, MatchStoreError> {
    let plan = effective_plan_on(connection, user_id, now).await?;
    let row = sqlx::query(
        "SELECT COALESCE((SELECT used_count FROM continuation_usage WHERE user_id=$1 AND week_start=$2),0)::integer AS used,
           COALESCE((SELECT free_used_count FROM continuation_usage WHERE user_id=$1 AND week_start=$2),0)::integer AS free_used,
           COALESCE((SELECT advertising_credits FROM continuation_wallet WHERE user_id=$1 AND week_start=$2),0)::integer AS balance,
           count(*) FILTER (WHERE r.source <> 'premium' AND (r.source <> 'advertising' OR r.week_start=$2))::integer AS reserved,
           count(*) FILTER (WHERE r.source='free' AND r.week_start=$2)::integer AS free_reserved,
           count(*) FILTER (WHERE r.source='advertising' AND r.week_start=$2)::integer AS ad_reserved
         FROM continuation_reservation r JOIN match_init m ON m.id=r.match_id
         WHERE r.user_id=$1 AND m.status='awaiting_continuation' AND m.expires_at>$3"
    )
        .bind(user_id)
        .bind(start_of_utc_week(now))
        .bind(now)
        .fetch_one(connection)
        .await
        .map_err(map_sqlx_error)?;
    let used: i32 = row.try_get("used").map_err(map_sqlx_error)?;
    let balance: i32 = row.try_get("balance").map_err(map_sqlx_error)?;
    let reserved: i32 = row.try_get("reserved").map_err(map_sqlx_error)?;
    let Some(included) = plan.weekly_credits else {
        return Ok(ContinuationQuota {
            plan: plan.plan,
            used: 0,
            weekly_limit: None,
            remaining: None,
            weekly_credits: 0,
            free_remaining: 0,
            advertising_balance: balance,
            advertising_remaining: 0,
            reserved,
            advertising_stock_limit: ADVERTISING_STOCK_LIMIT,
        });
    };
    let included = i32::from(included).min(FREE_WEEKLY_CREDITS);
    let free_remaining = (included
        - row.try_get::<i32, _>("free_used").map_err(map_sqlx_error)?
        - row
            .try_get::<i32, _>("free_reserved")
            .map_err(map_sqlx_error)?)
    .max(0);
    let advertising_remaining = (balance
        - row
            .try_get::<i32, _>("ad_reserved")
            .map_err(map_sqlx_error)?)
    .max(0);
    Ok(ContinuationQuota {
        plan: plan.plan,
        used,
        weekly_limit: Some(FREE_WEEKLY_CONTINUATION_LIMIT),
        remaining: Some(
            (free_remaining + advertising_remaining)
                .min((FREE_WEEKLY_CONTINUATION_LIMIT - used - reserved).max(0)),
        ),
        weekly_credits: included,
        free_remaining,
        advertising_balance: balance,
        advertising_remaining,
        reserved,
        advertising_stock_limit: ADVERTISING_STOCK_LIMIT,
    })
}

pub(super) async fn reserve(
    connection: &mut PgConnection,
    match_id: Uuid,
    user_id: Uuid,
    now: DateTime<Utc>,
) -> Result<bool, MatchStoreError> {
    let quota = quota_on(connection, user_id, now).await?;
    let source = if quota.weekly_limit.is_none() {
        "premium"
    } else if quota.remaining.unwrap_or(0) == 0 {
        return Ok(false);
    } else if quota.free_remaining > 0 {
        "free"
    } else {
        "advertising"
    };
    sqlx::query("INSERT INTO continuation_reservation(match_id,user_id,source,week_start) VALUES($1,$2,$3,$4)")
        .bind(match_id)
        .bind(user_id)
        .bind(source)
        .bind(start_of_utc_week(now))
        .execute(connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(true)
}

pub(super) async fn debit(
    connection: &mut PgConnection,
    user_id: Uuid,
    source: &str,
    reserved_week: NaiveDate,
    now: DateTime<Utc>,
) -> Result<(), MatchStoreError> {
    if source == "premium" {
        return Ok(());
    }
    let week = start_of_utc_week(now);
    sqlx::query(
        "INSERT INTO continuation_usage(user_id,week_start,used_count,free_used_count)
         VALUES($1,$2,1,0)
         ON CONFLICT(user_id,week_start)
         DO UPDATE SET used_count=continuation_usage.used_count+1",
    )
    .bind(user_id)
    .bind(week)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    if source == "free" {
        sqlx::query(
            "INSERT INTO continuation_usage(user_id,week_start,used_count,free_used_count)
             VALUES($1,$2,0,1)
             ON CONFLICT(user_id,week_start)
             DO UPDATE SET free_used_count=continuation_usage.free_used_count+1",
        )
        .bind(user_id)
        .bind(reserved_week)
        .execute(connection)
        .await
        .map_err(map_sqlx_error)?;
    } else {
        let result = sqlx::query(
            "UPDATE continuation_wallet SET advertising_credits=advertising_credits-1
             WHERE user_id=$1 AND week_start=$2 AND advertising_credits>0",
        )
        .bind(user_id)
        .bind(reserved_week)
        .execute(connection)
        .await
        .map_err(map_sqlx_error)?;
        if result.rows_affected() != 1 {
            return Err(DatabaseError::QueryFailed.into());
        }
    }
    Ok(())
}

/// Caller holds the wallet lock and has verified the provider's reward proof.
pub(crate) async fn grant_advertising_credit(
    connection: &mut PgConnection,
    user_id: Uuid,
    now: DateTime<Utc>,
) -> Result<bool, DatabaseError> {
    let result = sqlx::query(
        "INSERT INTO continuation_wallet(user_id,advertising_credits,week_start)
         VALUES($1,1,$2)
         ON CONFLICT(user_id) DO UPDATE
         SET advertising_credits=CASE WHEN continuation_wallet.week_start=$2
               THEN continuation_wallet.advertising_credits+1 ELSE 1 END,
             week_start=$2
         WHERE continuation_wallet.week_start<$2
            OR (continuation_wallet.week_start=$2 AND continuation_wallet.advertising_credits<$3)",
    )
    .bind(user_id)
    .bind(start_of_utc_week(now))
    .bind(ADVERTISING_STOCK_LIMIT)
    .execute(connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(result.rows_affected() == 1)
}
