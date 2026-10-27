use crate::{
    config::LegalConfig,
    identity::pg_guards::lock_eligible_accounts,
    infra::postgres::map_sqlx_error,
    matches::{
        domain::{ContinuationResult, MatchCommandResult, MatchStatus, start_of_utc_week},
        store::MatchStoreError,
    },
};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

pub(super) async fn record(
    connection: &mut PgConnection,
    match_id: Uuid,
    user_id: Uuid,
    legal: Option<LegalConfig>,
    acknowledged_cost: i32,
) -> Result<ContinuationResult, MatchStoreError> {
    let available = super::access::lock_available_match(connection, match_id, user_id).await?;
    let record = match available {
        MatchCommandResult::Available(record) => record,
        MatchCommandResult::Unavailable(
            super::super::domain::MatchAvailabilityFailure::NotFound,
        ) => return Ok(ContinuationResult::NotFound),
        MatchCommandResult::Unavailable(
            super::super::domain::MatchAvailabilityFailure::Expired,
        ) => return Ok(ContinuationResult::Expired),
        MatchCommandResult::Unavailable(_) => return Ok(ContinuationResult::InvalidState),
    };
    if record.status == MatchStatus::Confirmed {
        return Ok(ContinuationResult::AlreadyConfirmed);
    }
    if record.status == MatchStatus::Active {
        return Ok(ContinuationResult::NotAvailableYet);
    }
    let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_block WHERE (blocker_id=$1 AND blocked_id=$2) OR (blocker_id=$2 AND blocked_id=$1))")
        .bind(record.user1_id)
        .bind(record.user2_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    if blocked {
        return Ok(ContinuationResult::NotFound);
    }
    let mut participants = [record.user1_id, record.user2_id];
    participants.sort();
    // Match lock first, then wallets in UUID order. No wallet operation
    // acquires a match lock, avoiding cycles across concurrent matches.
    lock_eligible_accounts(connection, &participants, legal.as_ref()).await?;
    // Lock subscriptions before the wallet: their row triggers also take the
    // wallet lock, so Premium changes and continuation debits have one order.
    sqlx::query(
        "SELECT user_id FROM user_subscription WHERE user_id=ANY($1) ORDER BY user_id FOR SHARE",
    )
    .bind(participants.to_vec())
    .fetch_all(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query("SELECT id FROM billing_store_subscription WHERE user_id=ANY($1) ORDER BY user_id,id FOR SHARE")
        .bind(participants.to_vec()).fetch_all(&mut *connection).await.map_err(map_sqlx_error)?;
    for participant in participants {
        super::credits::lock_wallet(connection, participant).await?;
    }
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    if record.expires_at <= now {
        sqlx::query(
            "UPDATE match_init SET status='expired',purge_after=$2+INTERVAL '30 days' WHERE id=$1",
        )
        .bind(match_id)
        .bind(now)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
        sqlx::query("DELETE FROM continuation_reservation WHERE match_id=$1")
            .bind(match_id)
            .execute(connection)
            .await
            .map_err(map_sqlx_error)?;
        return Ok(ContinuationResult::Expired);
    }
    let plan = super::matches::effective_plan_on(connection, user_id, now).await?;
    let existing = sqlx::query_as::<_, (String, NaiveDate)>(
        "SELECT source,week_start FROM continuation_reservation WHERE match_id=$1 AND user_id=$2",
    )
    .bind(match_id)
    .bind(user_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    if existing
        .as_ref()
        .is_some_and(|(source, _)| source == "premium")
        && plan.weekly_credits.is_some()
    {
        clear_choice(connection, match_id, user_id).await?;
        return Ok(ContinuationResult::PlanChanged);
    }
    if plan.weekly_credits.is_some() && acknowledged_cost != 1 {
        return Ok(ContinuationResult::PlanChanged);
    }
    if let Some((source, week)) = existing {
        if source == "advertising" && week != start_of_utc_week(now) {
            clear_choice(connection, match_id, user_id).await?;
        } else {
            return Ok(ContinuationResult::AlreadyRecorded);
        }
    }
    if !super::credits::reserve(connection, match_id, user_id, now).await? {
        return Ok(ContinuationResult::QuotaReached);
    }
    sqlx::query("UPDATE match_state SET continued=true WHERE match_id=$1 AND user_id=$2")
        .bind(match_id)
        .bind(user_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    let reservations = sqlx::query(
        "SELECT user_id,source,week_start
         FROM continuation_reservation
         WHERE match_id=$1
         ORDER BY user_id",
    )
    .bind(match_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    if reservations.len() != 2 {
        return Ok(ContinuationResult::Pending);
    }
    // Validate both entitlements before applying either debit.
    let mut charges = Vec::with_capacity(2);
    for reservation in reservations {
        let participant: Uuid = reservation.try_get("user_id").map_err(map_sqlx_error)?;
        let source: String = reservation.try_get("source").map_err(map_sqlx_error)?;
        let week: NaiveDate = reservation.try_get("week_start").map_err(map_sqlx_error)?;
        if source == "advertising" && week != start_of_utc_week(now) {
            clear_choice(connection, match_id, participant).await?;
            return Ok(ContinuationResult::Pending);
        }
        let quota = super::credits::quota_on(connection, participant, now).await?;
        if quota.weekly_limit.is_none() {
            charges.push((participant, "premium".to_owned(), week));
        } else if source == "premium"
            || quota.used >= super::credits::FREE_WEEKLY_CONTINUATION_LIMIT
        {
            clear_choice(connection, match_id, participant).await?;
            return Ok(ContinuationResult::Pending);
        } else {
            charges.push((participant, source, week));
        }
    }
    for (participant, source, week) in charges {
        super::credits::debit(connection, participant, &source, week, now).await?;
    }
    let confirmed = sqlx::query(
        "UPDATE match_init SET status='confirmed',purge_after=NULL
         WHERE id=$1 AND expires_at>clock_timestamp()",
    )
    .bind(match_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    if confirmed.rows_affected() != 1 {
        return Err(MatchStoreError::ContinuationExpired);
    }
    sqlx::query("DELETE FROM continuation_reservation WHERE match_id=$1")
        .bind(match_id)
        .execute(connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(ContinuationResult::Confirmed)
}

async fn clear_choice(
    connection: &mut PgConnection,
    match_id: Uuid,
    user_id: Uuid,
) -> Result<(), MatchStoreError> {
    sqlx::query("DELETE FROM continuation_reservation WHERE match_id=$1 AND user_id=$2")
        .bind(match_id)
        .bind(user_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query("UPDATE match_state SET continued=false WHERE match_id=$1 AND user_id=$2")
        .bind(match_id)
        .bind(user_id)
        .execute(connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}
