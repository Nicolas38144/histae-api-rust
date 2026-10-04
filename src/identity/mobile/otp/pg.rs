use super::{
    domain::*,
    store::{OtpStore, OtpStoreFuture},
};
use crate::{
    identity::mobile::sweego::SmsFailureReason,
    infra::postgres::{Database, DatabaseError, map_sqlx_error},
};
use std::time::Duration;
use uuid::Uuid;

#[derive(Clone)]
pub struct OtpRepository {
    database: Database,
}

impl OtpRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    async fn begin_impl(&self, input: BeginOtpDelivery) -> Result<OtpDeliveryStart, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    lock_phone(connection, &input.phone_hash).await?;
                    let ttl_millis = duration_millis(input.ttl)?;
                    let settlement_millis = duration_millis(input.settlement)?;
                    let inserted = sqlx::query_scalar::<_, Uuid>(
                        "INSERT INTO otp_verification
                         (id, phone_number_hash, otp_hash, idempotency_key, expires_at, settlement_deadline)
                         VALUES ($1, $2, $3, $4,
                           clock_timestamp() + $5 * INTERVAL '1 millisecond',
                           clock_timestamp() + $6 * INTERVAL '1 millisecond')
                         ON CONFLICT (idempotency_key) DO NOTHING RETURNING id",
                    )
                    .bind(input.id)
                    .bind(&input.phone_hash)
                    .bind(&input.otp_hash)
                    .bind(input.idempotency_key)
                    .bind(ttl_millis)
                    .bind(settlement_millis)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    if let Some(id) = inserted {
                        return Ok(OtpDeliveryStart::Created(id));
                    }
                    sqlx::query(
                        "UPDATE otp_verification
                         SET delivery_status = 'unknown', delivery_error_code = 'delivery_unknown'
                         WHERE idempotency_key = $1 AND phone_number_hash = $2
                           AND delivery_status = 'pending'
                           AND settlement_deadline <= clock_timestamp()",
                    )
                    .bind(input.idempotency_key)
                    .bind(&input.phone_hash)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    let existing = sqlx::query_as::<_, (Uuid, String, String)>(
                        "SELECT id, phone_number_hash, delivery_status
                         FROM otp_verification WHERE idempotency_key = $1",
                    )
                    .bind(input.idempotency_key)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?
                    .ok_or(DatabaseError::QueryFailed)?;
                    if existing.1 != input.phone_hash {
                        return Ok(OtpDeliveryStart::Conflict);
                    }
                    Ok(OtpDeliveryStart::Existing(
                        OtpDeliveryState::parse(&existing.2)?,
                        existing.0,
                    ))
                })
            })
            .await
    }

    async fn mark_accepted_impl(
        &self,
        id: Uuid,
        phone_hash: String,
        transaction_id: String,
        message_id: String,
    ) -> Result<bool, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let Some(row) = locked_delivery(connection, id, &phone_hash).await? else {
                        return Ok(false);
                    };
                    if !matches_receipt(&row, &message_id, Some(&transaction_id))
                        || row.delivery_status == OtpDeliveryState::Failed
                    {
                        return Ok(false);
                    }
                    activate(connection, &row).await?;
                    let unexpired = sqlx::query_scalar::<_, bool>(
                        "UPDATE otp_verification SET
                           delivery_status = CASE WHEN delivery_status = 'sent' THEN 'sent' ELSE 'accepted' END,
                           provider_transaction_id = $2, provider_message_id = $3,
                           sent_at = COALESCE(sent_at, clock_timestamp()), delivery_error_code = NULL
                         WHERE id = $1 RETURNING expires_at > clock_timestamp()",
                    )
                    .bind(id)
                    .bind(transaction_id)
                    .bind(message_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    Ok(unexpired == Some(true))
                })
            })
            .await
    }

    async fn mark_outcome_impl(
        &self,
        id: Uuid,
        phone_hash: String,
        state: OtpDeliveryState,
        reason: SmsFailureReason,
    ) -> Result<OtpDeliveryState, DatabaseError> {
        debug_assert!(matches!(
            state,
            OtpDeliveryState::Unknown | OtpDeliveryState::Failed
        ));
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let Some(row) = locked_delivery(connection, id, &phone_hash).await? else {
                        return Ok(state);
                    };
                    if !matches!(
                        row.delivery_status,
                        OtpDeliveryState::Pending | OtpDeliveryState::Unknown
                    ) {
                        return Ok(row.delivery_status);
                    }
                    sqlx::query(
                        "UPDATE otp_verification SET delivery_status = $2, delivery_error_code = $3,
                           failed_at = CASE WHEN $2 = 'failed'
                             THEN COALESCE(failed_at, clock_timestamp()) ELSE failed_at END
                         WHERE id = $1",
                    )
                    .bind(id)
                    .bind(state.as_str())
                    .bind(reason.as_str())
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    Ok(state)
                })
            })
            .await
    }

    async fn apply_sms_event_impl(
        &self,
        event: SmsDeliveryEvent,
    ) -> Result<SmsEventOutcome, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let identity = sqlx::query_scalar::<_, String>(
                        "SELECT phone_number_hash FROM otp_verification
                         WHERE id = $1 AND provider = 'sweego'",
                    )
                    .bind(event.delivery_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    let Some(phone_hash) = identity else {
                        return Ok(SmsEventOutcome::Ignored);
                    };
                    let Some(row) =
                        locked_delivery(connection, event.delivery_id, &phone_hash).await?
                    else {
                        return Ok(SmsEventOutcome::Ignored);
                    };
                    if !matches_receipt(
                        &row,
                        &event.message_id,
                        event.transaction_id.as_deref(),
                    ) {
                        return Ok(SmsEventOutcome::Conflict);
                    }
                    if row.delivery_status == OtpDeliveryState::Failed
                        || (row.delivery_status == OtpDeliveryState::Sent
                            && event.kind == SmsEventKind::Sent)
                    {
                        return Ok(SmsEventOutcome::Ignored);
                    }
                    if event.kind == SmsEventKind::Sent {
                        activate(connection, &row).await?;
                    }
                    let state = if event.kind == SmsEventKind::Sent {
                        OtpDeliveryState::Sent
                    } else {
                        OtpDeliveryState::Failed
                    };
                    sqlx::query(
                        "UPDATE otp_verification SET delivery_status = $2,
                           provider_message_id = $3,
                           provider_transaction_id = COALESCE(provider_transaction_id, $4),
                           sent_at = CASE WHEN $2 = 'sent' THEN COALESCE(sent_at, clock_timestamp()) ELSE sent_at END,
                           provider_sent_at = CASE WHEN $2 = 'sent' THEN COALESCE(provider_sent_at, clock_timestamp()) ELSE provider_sent_at END,
                           failed_at = CASE WHEN $2 = 'failed' THEN COALESCE(failed_at, clock_timestamp()) ELSE failed_at END,
                           delivery_error_code = CASE WHEN $2 = 'failed' THEN 'provider_undelivered' ELSE NULL END,
                           last_webhook_at = clock_timestamp()
                         WHERE id = $1",
                    )
                    .bind(event.delivery_id)
                    .bind(state.as_str())
                    .bind(event.message_id)
                    .bind(event.transaction_id)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    Ok(SmsEventOutcome::Applied)
                })
            })
            .await
    }

    async fn consume_impl(
        &self,
        phone_hash: String,
        otp_hash: String,
    ) -> Result<bool, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    lock_phone(connection, &phone_hash).await?;
                    let result = sqlx::query(
                        "UPDATE otp_verification SET used = true
                         WHERE phone_number_hash = $1 AND otp_hash = $2
                           AND delivery_status IN ('accepted', 'sent')
                           AND used = false AND expires_at > clock_timestamp()
                         RETURNING id",
                    )
                    .bind(phone_hash)
                    .bind(otp_hash)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    Ok(result.rows_affected() == 1)
                })
            })
            .await
    }

    async fn snapshot_impl(&self) -> Result<OtpDeliverySnapshot, DatabaseError> {
        type SnapshotRow = (
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
            Option<f64>,
            Option<f64>,
            Option<f64>,
            Option<f64>,
        );
        let row = sqlx::query_as::<_, SnapshotRow>(
            "SELECT count(*) FILTER (WHERE state = 'pending')::int,
               count(*) FILTER (WHERE state = 'accepted')::int,
               count(*) FILTER (WHERE state = 'sent')::int,
               count(*) FILTER (WHERE state = 'failed')::int,
               count(*) FILTER (WHERE state = 'unknown')::int,
               count(*) FILTER (WHERE state = 'accepted' AND last_webhook_at IS NULL)::int,
               max(EXTRACT(EPOCH FROM (clock_timestamp() - created_at)))
                 FILTER (WHERE state IN ('pending', 'unknown', 'accepted'))::float8,
               avg(EXTRACT(EPOCH FROM (sent_at - created_at)) * 1000)::float8,
               avg(EXTRACT(EPOCH FROM (provider_sent_at - created_at)) * 1000)::float8,
               avg(EXTRACT(EPOCH FROM (failed_at - created_at)) * 1000)::float8
             FROM (
               SELECT *, CASE
                 WHEN delivery_status = 'pending' AND settlement_deadline <= clock_timestamp()
                 THEN 'unknown' ELSE delivery_status END AS state
               FROM otp_verification WHERE expires_at > clock_timestamp()
             ) deliveries",
        )
        .fetch_one(self.database.pool())
        .await
        .map_err(map_sqlx_error)?;
        Ok(OtpDeliverySnapshot {
            states: OtpDeliveryStates {
                pending: row.0,
                accepted: row.1,
                sent: row.2,
                failed: row.3,
                unknown: row.4,
            },
            awaiting_callback: row.5,
            oldest_unresolved_age_seconds: row.6,
            average_acceptance_ms: row.7,
            average_sent_callback_ms: row.8,
            average_failure_ms: row.9,
            retention: "otp_expiry",
            handset_delivery: "not_confirmed",
        })
    }
}

impl OtpStore for OtpRepository {
    fn begin(&self, input: BeginOtpDelivery) -> OtpStoreFuture<'_, OtpDeliveryStart> {
        Box::pin(self.begin_impl(input))
    }

    fn mark_accepted(
        &self,
        id: Uuid,
        phone_hash: String,
        transaction_id: String,
        message_id: String,
    ) -> OtpStoreFuture<'_, bool> {
        Box::pin(self.mark_accepted_impl(id, phone_hash, transaction_id, message_id))
    }

    fn mark_outcome(
        &self,
        id: Uuid,
        phone_hash: String,
        state: OtpDeliveryState,
        reason: SmsFailureReason,
    ) -> OtpStoreFuture<'_, OtpDeliveryState> {
        Box::pin(self.mark_outcome_impl(id, phone_hash, state, reason))
    }

    fn apply_sms_event(&self, event: SmsDeliveryEvent) -> OtpStoreFuture<'_, SmsEventOutcome> {
        Box::pin(self.apply_sms_event_impl(event))
    }

    fn consume(&self, phone_hash: String, otp_hash: String) -> OtpStoreFuture<'_, bool> {
        Box::pin(self.consume_impl(phone_hash, otp_hash))
    }

    fn snapshot(&self) -> OtpStoreFuture<'_, OtpDeliverySnapshot> {
        Box::pin(self.snapshot_impl())
    }
}

fn duration_millis(value: Duration) -> Result<i64, DatabaseError> {
    i64::try_from(value.as_millis()).map_err(|_| DatabaseError::QueryFailed)
}

struct DeliveryRow {
    id: Uuid,
    phone_hash: String,
    delivery_status: OtpDeliveryState,
    message_id: Option<String>,
    transaction_id: Option<String>,
    attempt_number: i64,
    used: bool,
}

async fn lock_phone(
    connection: &mut sqlx::PgConnection,
    phone_hash: &str,
) -> Result<(), DatabaseError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(phone_hash)
        .execute(connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

async fn locked_delivery(
    connection: &mut sqlx::PgConnection,
    id: Uuid,
    phone_hash: &str,
) -> Result<Option<DeliveryRow>, DatabaseError> {
    lock_phone(connection, phone_hash).await?;
    let row = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            String,
            Option<String>,
            Option<String>,
            i64,
            bool,
        ),
    >(
        "SELECT id, phone_number_hash, delivery_status, provider_message_id,
           provider_transaction_id, attempt_number, used
         FROM otp_verification
         WHERE id = $1 AND phone_number_hash = $2 FOR UPDATE",
    )
    .bind(id)
    .bind(phone_hash)
    .fetch_optional(connection)
    .await
    .map_err(map_sqlx_error)?;
    row.map(
        |(id, phone_hash, state, message_id, transaction_id, attempt_number, used)| {
            Ok(DeliveryRow {
                id,
                phone_hash,
                delivery_status: OtpDeliveryState::parse(&state)?,
                message_id,
                transaction_id,
                attempt_number,
                used,
            })
        },
    )
    .transpose()
}

fn matches_receipt(row: &DeliveryRow, message_id: &str, transaction_id: Option<&str>) -> bool {
    row.message_id
        .as_deref()
        .is_none_or(|stored| stored == message_id)
        && row
            .transaction_id
            .as_deref()
            .is_none_or(|stored| transaction_id.is_none_or(|supplied| supplied == stored))
}

async fn activate(
    connection: &mut sqlx::PgConnection,
    row: &DeliveryRow,
) -> Result<(), DatabaseError> {
    if row.used {
        return Ok(());
    }
    sqlx::query(
        "UPDATE otp_verification SET used = true WHERE id = $1 AND (
           expires_at <= clock_timestamp() OR EXISTS (
             SELECT 1 FROM otp_verification
             WHERE phone_number_hash = $2 AND attempt_number > $3 AND sent_at IS NOT NULL
           )
         )",
    )
    .bind(row.id)
    .bind(&row.phone_hash)
    .bind(row.attempt_number)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        "UPDATE otp_verification SET used = true
         WHERE phone_number_hash = $1 AND attempt_number < $2 AND used = false
           AND EXISTS (SELECT 1 FROM otp_verification WHERE id = $3 AND used = false)",
    )
    .bind(&row.phone_hash)
    .bind(row.attempt_number)
    .bind(row.id)
    .execute(connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}
