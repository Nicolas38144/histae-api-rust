use serde_json::Value;
use sqlx::types::Json;
use sqlx::{Acquire as _, Row as _};
use uuid::Uuid;

use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::privacy::export::{
    DataExportFuture, DataExportStore, ExportBuildError, ExportSnapshot, JsonExportWriter,
};

#[derive(Clone)]
pub struct PgDataExportStore {
    database: Database,
}

impl PgDataExportStore {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    async fn write_snapshot_inner(
        &self,
        user_id: Uuid,
        writer: &mut JsonExportWriter,
        page_size: u32,
    ) -> Result<ExportSnapshot, ExportBuildError> {
        let mut connection = self.database.acquire().await?;
        let mut transaction = connection.begin().await.map_err(map_sqlx_error)?;
        let result = async {
                    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
                        .execute(&mut *transaction)
                        .await
                        .map_err(map_sqlx_error)?;
                    let snapshot_at = sqlx::query_scalar(
                        "SELECT transaction_timestamp() AS snapshot_at",
                    )
                    .fetch_one(&mut *transaction)
                    .await
                    .map_err(map_sqlx_error)?;

                    let account = optional_json(
                        &mut transaction,
                        "SELECT jsonb_build_object(
                           'user_id', user_id, 'role', role, 'is_banned', is_banned,
                           'deleted_at', CASE WHEN deleted_at IS NULL THEN NULL ELSE
                             to_char(deleted_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') END,
                           'anonymized_at', CASE WHEN anonymized_at IS NULL THEN NULL ELSE
                             to_char(anonymized_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') END,
                           'created_at', to_char(created_at AT TIME ZONE 'UTC',
                             'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'))
                         FROM user_account WHERE user_id = $1",
                        user_id,
                    )
                    .await?;
                    let stored_profile = sqlx::query(
                        "SELECT jsonb_build_object(
                           'firstname', profile.firstname, 'birthdate', profile.birthdate,
                           'sex', profile.sex, 'bio', profile.bio) AS value,
                           photo.object_key AS photo_key
                         FROM user_profile AS profile
                         LEFT JOIN user_photo AS photo
                           ON photo.user_id = profile.user_id AND photo.status = 'ready'
                         WHERE profile.user_id = $1",
                    )
                    .bind(user_id)
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(map_sqlx_error)?;
                    let (profile, photo_key) = match stored_profile {
                        Some(row) => (
                            row.try_get::<Json<Value>, _>("value")
                                .map_err(map_sqlx_error)?
                                .0,
                            row.try_get("photo_key").map_err(map_sqlx_error)?,
                        ),
                        None => (Value::Null, None),
                    };
                    let preferences = optional_json(
                        &mut transaction,
                        "SELECT jsonb_build_object(
                           'min_age', min_age, 'max_age', max_age,
                           'max_distance_km', max_distance_km, 'looking_for', looking_for)
                         FROM user_preferences WHERE user_id = $1",
                        user_id,
                    )
                    .await?;
                    let subscription = optional_json(
                        &mut transaction,
                        "SELECT jsonb_build_object(
                           'plan', plan, 'provider', provider,
                           'provider_subscription_id', provider_subscription_id,
                           'provider_price_id', provider_price_id, 'billing_period', billing_period,
                           'status', status, 'cancel_at_period_end', cancel_at_period_end,
                           'current_period_starts_at', CASE WHEN current_period_starts_at IS NULL THEN NULL ELSE
                             to_char(current_period_starts_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') END,
                           'current_period_ends_at', CASE WHEN current_period_ends_at IS NULL THEN NULL ELSE
                             to_char(current_period_ends_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') END,
                           'trial_ends_at', CASE WHEN trial_ends_at IS NULL THEN NULL ELSE
                             to_char(trial_ends_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') END,
                           'canceled_at', CASE WHEN canceled_at IS NULL THEN NULL ELSE
                             to_char(canceled_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') END,
                           'provider_event_created_at', CASE WHEN provider_event_created_at IS NULL THEN NULL ELSE
                             to_char(provider_event_created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') END,
                           'updated_at', to_char(updated_at AT TIME ZONE 'UTC',
                             'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'))
                         FROM user_subscription WHERE user_id = $1",
                        user_id,
                    )
                    .await?;

                    self.write_traits(&mut transaction, writer, user_id, page_size)
                        .await?;
                    self.write_profile_answers(&mut transaction, writer, user_id)
                        .await?;
                    self.write_consents(&mut transaction, writer, user_id, page_size)
                        .await?;
                    self.write_matches(&mut transaction, writer, user_id, page_size)
                        .await?;
                    self.write_messages(&mut transaction, writer, user_id, page_size)
                        .await?;
                    self.write_reports(&mut transaction, writer, user_id, page_size)
                        .await?;
                    self.write_blocks(&mut transaction, writer, user_id, page_size)
                        .await?;
                    self.write_invoices(&mut transaction, writer, user_id, page_size)
                        .await?;
                    self.write_mobile_sessions(&mut transaction, writer, user_id, page_size)
                        .await?;
                    let discovery_rows = self
                        .write_discovery_actions(&mut transaction, writer, user_id, page_size)
                        .await?;

                    Ok::<_, ExportBuildError>(ExportSnapshot {
                        snapshot_at,
                        account,
                        profile,
                        photo_key,
                        preferences,
                        subscription,
                        discovery_rows,
                    })
        }
        .await;
        match result {
            Ok(snapshot) => {
                transaction.commit().await.map_err(map_sqlx_error)?;
                Ok(snapshot)
            }
            Err(error) => {
                transaction.rollback().await.map_err(map_sqlx_error)?;
                Err(error)
            }
        }
    }

    async fn write_traits(
        &self,
        connection: &mut sqlx::PgConnection,
        writer: &mut JsonExportWriter,
        user_id: Uuid,
        page_size: u32,
    ) -> Result<(), ExportBuildError> {
        writer.start_array("traits").await?;
        let mut cursor: Option<(String, Uuid)> = None;
        loop {
            let rows = sqlx::query(
                "SELECT jsonb_build_object('id', trait.id, 'name', trait.name) AS value,
                        trait.name AS cursor_name, trait.id AS cursor_id
                 FROM trait JOIN user_trait ON user_trait.trait_id = trait.id
                 WHERE user_trait.user_id = $1
                   AND ($2::text IS NULL OR (trait.name, trait.id) > ($2, $3::uuid))
                 ORDER BY trait.name, trait.id LIMIT $4",
            )
            .bind(user_id)
            .bind(cursor.as_ref().map(|value| value.0.as_str()))
            .bind(cursor.as_ref().map(|value| value.1))
            .bind(i64::from(page_size))
            .fetch_all(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            for row in &rows {
                writer
                    .item(
                        &row.try_get::<Json<Value>, _>("value")
                            .map_err(map_sqlx_error)?
                            .0,
                    )
                    .await?;
            }
            if rows.len() < page_size as usize {
                break;
            }
            let last = rows.last().ok_or(DatabaseError::QueryFailed)?;
            cursor = Some((
                last.try_get("cursor_name").map_err(map_sqlx_error)?,
                last.try_get("cursor_id").map_err(map_sqlx_error)?,
            ));
        }
        writer.end_array().await
    }

    async fn write_profile_answers(
        &self,
        connection: &mut sqlx::PgConnection,
        writer: &mut JsonExportWriter,
        user_id: Uuid,
    ) -> Result<(), ExportBuildError> {
        let rows = sqlx::query_scalar::<_, Json<Value>>(
            "SELECT jsonb_build_object(
               'question_id', answer.question_id, 'code', question.code,
               'question', question.prompt, 'answer', answer.answer, 'position', answer.position)
             FROM user_profile_answer AS answer
             JOIN profile_question AS question ON question.id = answer.question_id
             WHERE answer.user_id = $1 ORDER BY answer.position",
        )
        .bind(user_id)
        .fetch_all(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
        writer.start_array("profile_answers").await?;
        for row in rows {
            writer.item(&row.0).await?;
        }
        writer.end_array().await
    }

    async fn write_consents(
        &self,
        connection: &mut sqlx::PgConnection,
        writer: &mut JsonExportWriter,
        user_id: Uuid,
        page_size: u32,
    ) -> Result<(), ExportBuildError> {
        writer.start_array("legal_choices").await?;
        let mut cursor: Option<i64> = None;
        loop {
            let rows = sqlx::query(
                "SELECT jsonb_build_object(
                   'consent_type', consent_type, 'granted', granted,
                   'document_version', document_version,
                   'granted_at', to_char(granted_at AT TIME ZONE 'UTC',
                     'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'),
                   'withdrawn_at', CASE WHEN withdrawn_at IS NULL THEN NULL ELSE
                     to_char(withdrawn_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') END) AS value,
                   event_sequence AS cursor_sequence
                 FROM user_consent
                 WHERE user_id = $1 AND ($2::bigint IS NULL OR event_sequence > $2)
                 ORDER BY event_sequence LIMIT $3",
            )
            .bind(user_id)
            .bind(cursor)
            .bind(i64::from(page_size))
            .fetch_all(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            for row in &rows {
                writer
                    .item(
                        &row.try_get::<Json<Value>, _>("value")
                            .map_err(map_sqlx_error)?
                            .0,
                    )
                    .await?;
            }
            if rows.len() < page_size as usize {
                break;
            }
            cursor = Some(
                rows.last()
                    .ok_or(DatabaseError::QueryFailed)?
                    .try_get("cursor_sequence")
                    .map_err(map_sqlx_error)?,
            );
        }
        writer.end_array().await
    }

    async fn write_matches(
        &self,
        connection: &mut sqlx::PgConnection,
        writer: &mut JsonExportWriter,
        user_id: Uuid,
        page_size: u32,
    ) -> Result<(), ExportBuildError> {
        self.write_uuid_timestamp_pages(
            connection,
            writer,
            "matches",
            user_id,
            page_size,
            "WITH participant_matches AS (
               SELECT id, user1_id, user2_id, status, expires_at, created_at, last_message_at
               FROM match_init WHERE user1_id = $1
                 AND ($2::timestamptz IS NULL OR (created_at, id) > ($2::timestamptz, $3::uuid))
               UNION ALL
               SELECT id, user1_id, user2_id, status, expires_at, created_at, last_message_at
               FROM match_init WHERE user2_id = $1
                 AND ($2::timestamptz IS NULL OR (created_at, id) > ($2::timestamptz, $3::uuid))
             )
             SELECT jsonb_build_object(
               'id', id, 'user1_id', user1_id, 'user2_id', user2_id, 'status', status,
               'expires_at', to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'),
               'created_at', to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'),
               'last_message_at', CASE WHEN last_message_at IS NULL THEN NULL ELSE
                 to_char(last_message_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') END) AS value,
               to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at,
               id AS cursor_id
             FROM participant_matches ORDER BY created_at, id LIMIT $4",
        )
        .await
    }

    async fn write_messages(
        &self,
        connection: &mut sqlx::PgConnection,
        writer: &mut JsonExportWriter,
        user_id: Uuid,
        page_size: u32,
    ) -> Result<(), ExportBuildError> {
        self.write_uuid_timestamp_pages(
            connection,
            writer,
            "authored_messages",
            user_id,
            page_size,
            "SELECT jsonb_build_object(
               'id', id, 'match_id', match_id, 'content', content,
               'created_at', to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'),
               'read_at', CASE WHEN read_at IS NULL THEN NULL ELSE
                 to_char(read_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') END) AS value,
               to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at,
               id AS cursor_id
             FROM chat_message WHERE sender_id = $1
               AND ($2::timestamptz IS NULL OR (created_at, id) > ($2::timestamptz, $3::uuid))
             ORDER BY created_at, id LIMIT $4",
        )
        .await
    }

    async fn write_reports(
        &self,
        connection: &mut sqlx::PgConnection,
        writer: &mut JsonExportWriter,
        user_id: Uuid,
        page_size: u32,
    ) -> Result<(), ExportBuildError> {
        self.write_uuid_timestamp_pages(
            connection,
            writer,
            "submitted_reports",
            user_id,
            page_size,
            "SELECT jsonb_build_object(
               'id', id, 'reported_id', reported_id, 'match_id', match_id,
               'reason', reason, 'description', description, 'status', status,
               'created_at', to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'),
               'resolved_at', CASE WHEN resolved_at IS NULL THEN NULL ELSE
                 to_char(resolved_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') END) AS value,
               to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at,
               id AS cursor_id
             FROM user_report WHERE reporter_id = $1
               AND ($2::timestamptz IS NULL OR (created_at, id) > ($2::timestamptz, $3::uuid))
             ORDER BY created_at, id LIMIT $4",
        )
        .await
    }

    async fn write_blocks(
        &self,
        connection: &mut sqlx::PgConnection,
        writer: &mut JsonExportWriter,
        user_id: Uuid,
        page_size: u32,
    ) -> Result<(), ExportBuildError> {
        self.write_uuid_timestamp_pages(
            connection,
            writer,
            "blocked_users",
            user_id,
            page_size,
            "SELECT jsonb_build_object(
               'blocked_id', blocked_id,
               'created_at', to_char(created_at AT TIME ZONE 'UTC',
                 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')) AS value,
               to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at,
               blocked_id AS cursor_id
             FROM user_block WHERE blocker_id = $1
               AND ($2::timestamptz IS NULL OR (created_at, blocked_id) > ($2::timestamptz, $3::uuid))
             ORDER BY created_at, blocked_id LIMIT $4",
        )
        .await
    }

    async fn write_invoices(
        &self,
        connection: &mut sqlx::PgConnection,
        writer: &mut JsonExportWriter,
        user_id: Uuid,
        page_size: u32,
    ) -> Result<(), ExportBuildError> {
        writer.start_array("billing_invoices").await?;
        let mut cursor: Option<(String, String)> = None;
        loop {
            let rows = sqlx::query(
                "SELECT jsonb_build_object(
                   'stripe_invoice_id', stripe_invoice_id,
                   'stripe_subscription_id', stripe_subscription_id, 'status', status,
                   'currency', currency, 'amount_due', amount_due, 'amount_paid', amount_paid,
                   'amount_remaining', amount_remaining,
                   'period_starts_at', CASE WHEN period_starts_at IS NULL THEN NULL ELSE
                     to_char(period_starts_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') END,
                   'period_ends_at', CASE WHEN period_ends_at IS NULL THEN NULL ELSE
                     to_char(period_ends_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') END,
                   'paid_at', CASE WHEN paid_at IS NULL THEN NULL ELSE
                     to_char(paid_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') END,
                   'created_at', to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'),
                   'provider_event_created_at', CASE WHEN provider_event_created_at IS NULL THEN NULL ELSE
                     to_char(provider_event_created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') END) AS value,
                   to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at,
                   stripe_invoice_id AS cursor_id
                 FROM billing_invoice WHERE user_id = $1
                   AND ($2::timestamptz IS NULL OR (created_at, stripe_invoice_id) > ($2::timestamptz, $3))
                 ORDER BY created_at, stripe_invoice_id LIMIT $4",
            )
            .bind(user_id)
            .bind(cursor.as_ref().map(|value| value.0.as_str()))
            .bind(cursor.as_ref().map(|value| value.1.as_str()))
            .bind(i64::from(page_size))
            .fetch_all(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            for row in &rows {
                writer
                    .item(
                        &row.try_get::<Json<Value>, _>("value")
                            .map_err(map_sqlx_error)?
                            .0,
                    )
                    .await?;
            }
            if rows.len() < page_size as usize {
                break;
            }
            let last = rows.last().ok_or(DatabaseError::QueryFailed)?;
            cursor = Some((
                last.try_get("cursor_at").map_err(map_sqlx_error)?,
                last.try_get("cursor_id").map_err(map_sqlx_error)?,
            ));
        }
        writer.end_array().await
    }

    async fn write_mobile_sessions(
        &self,
        connection: &mut sqlx::PgConnection,
        writer: &mut JsonExportWriter,
        user_id: Uuid,
        page_size: u32,
    ) -> Result<(), ExportBuildError> {
        self.write_uuid_timestamp_pages(
            connection,
            writer,
            "mobile_sessions",
            user_id,
            page_size,
            "SELECT jsonb_build_object(
               'id', id,
               'created_at', to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'),
               'last_refreshed_at', to_char(last_refreshed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'),
               'expires_at', to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'),
               'revoked_at', CASE WHEN revoked_at IS NULL THEN NULL ELSE
                 to_char(revoked_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') END,
               'revocation_reason', revocation_reason) AS value,
               to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at,
               id AS cursor_id
             FROM refresh_token_family WHERE user_id = $1
               AND ($2::timestamptz IS NULL OR (created_at, id) > ($2::timestamptz, $3::uuid))
             ORDER BY created_at, id LIMIT $4",
        )
        .await
    }

    async fn write_discovery_actions(
        &self,
        connection: &mut sqlx::PgConnection,
        writer: &mut JsonExportWriter,
        user_id: Uuid,
        page_size: u32,
    ) -> Result<u64, ExportBuildError> {
        writer.start_object(Some("discovery_actions")).await?;
        writer.start_array("outgoing").await?;
        let mut cursor: Option<(String, Uuid)> = None;
        let mut count = 0_u64;
        loop {
            let rows = sqlx::query(
                "SELECT jsonb_build_object(
                   'actor_id', actor_id, 'target_id', target_id, 'decision', decision,
                   'swiped_at', to_char(swiped_at AT TIME ZONE 'UTC',
                     'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')) AS value,
                   to_char(swiped_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_at,
                   target_id AS cursor_id
                 FROM swipe_decision
                 WHERE actor_id = $1 AND expires_at > transaction_timestamp()
                   AND ($2::timestamptz IS NULL OR (swiped_at, target_id) > ($2::timestamptz, $3::uuid))
                 ORDER BY swiped_at, target_id LIMIT $4",
            )
            .bind(user_id)
            .bind(cursor.as_ref().map(|value| value.0.as_str()))
            .bind(cursor.as_ref().map(|value| value.1))
            .bind(i64::from(page_size))
            .fetch_all(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            for row in &rows {
                writer
                    .item(
                        &row.try_get::<Json<Value>, _>("value")
                            .map_err(map_sqlx_error)?
                            .0,
                    )
                    .await?;
                count = count.saturating_add(1);
            }
            if rows.len() < page_size as usize {
                break;
            }
            let last = rows.last().ok_or(DatabaseError::QueryFailed)?;
            cursor = Some((
                last.try_get("cursor_at").map_err(map_sqlx_error)?,
                last.try_get("cursor_id").map_err(map_sqlx_error)?,
            ));
        }
        writer.end_array().await?;
        writer.end_object().await?;
        Ok(count)
    }

    async fn write_uuid_timestamp_pages(
        &self,
        connection: &mut sqlx::PgConnection,
        writer: &mut JsonExportWriter,
        name: &str,
        user_id: Uuid,
        page_size: u32,
        sql: &str,
    ) -> Result<(), ExportBuildError> {
        writer.start_array(name).await?;
        let mut cursor: Option<(String, Uuid)> = None;
        loop {
            let rows = sqlx::query(sql)
                .bind(user_id)
                .bind(cursor.as_ref().map(|value| value.0.as_str()))
                .bind(cursor.as_ref().map(|value| value.1))
                .bind(i64::from(page_size))
                .fetch_all(&mut *connection)
                .await
                .map_err(map_sqlx_error)?;
            for row in &rows {
                writer
                    .item(
                        &row.try_get::<Json<Value>, _>("value")
                            .map_err(map_sqlx_error)?
                            .0,
                    )
                    .await?;
            }
            if rows.len() < page_size as usize {
                break;
            }
            let last = rows.last().ok_or(DatabaseError::QueryFailed)?;
            cursor = Some((
                last.try_get("cursor_at").map_err(map_sqlx_error)?,
                last.try_get("cursor_id").map_err(map_sqlx_error)?,
            ));
        }
        writer.end_array().await
    }
}

impl DataExportStore for PgDataExportStore {
    fn write_snapshot<'a>(
        &'a self,
        user_id: Uuid,
        writer: &'a mut JsonExportWriter,
        page_size: u32,
    ) -> DataExportFuture<'a> {
        Box::pin(self.write_snapshot_inner(user_id, writer, page_size))
    }
}

async fn optional_json(
    connection: &mut sqlx::PgConnection,
    sql: &str,
    user_id: Uuid,
) -> Result<Value, ExportBuildError> {
    sqlx::query_scalar::<_, Json<Value>>(sql)
        .bind(user_id)
        .fetch_optional(connection)
        .await
        .map(|value| value.map_or(Value::Null, |value| value.0))
        .map_err(|error| map_sqlx_error(error).into())
}
