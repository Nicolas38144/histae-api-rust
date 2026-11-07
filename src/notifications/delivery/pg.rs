//! PostgreSQL eligibility and device cleanup for queued push deliveries.
use std::sync::LazyLock;

use serde_json::Value;
use sqlx::Row as _;
use uuid::Uuid;

use super::{DeliveryStoreFuture, NotificationDeliveryStore, PendingPush};
use crate::{
    infra::postgres::{Database, DatabaseError, map_sqlx_error},
    localization::Language,
    notifications::{
        domain::NotificationType,
        eligibility::{
            ACCOUNT_NOTIFICATION_ELIGIBLE_SQL, BILLING_NOTIFICATION_ELIGIBLE_SQL,
            PUSH_PREFERENCES_ELIGIBLE_SQL,
        },
    },
};

// Reuse SQL text only. Access and state are checked afresh on every delivery.
static PENDING_PUSH_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"SELECT n.id AS notification_id, d.token, n.type, n.payload,
                 LEAST(n.expires_at, m.expires_at, session.expires_at, opened_session.expires_at) AS expires_at,
                 COALESCE(d.language, account.preferred_language) AS language,
                 CASE WHEN COALESCE(preference.discreet,false) THEN NULL
                      ELSE partner_profile.firstname END AS partner_firstname
               FROM notification_push_delivery delivery
               JOIN notification n ON n.id = delivery.notification_id
               JOIN device_token d ON d.id = delivery.device_id AND d.user_id = n.user_id
                 AND d.session_id IS NOT DISTINCT FROM delivery.session_id
               JOIN user_account account ON account.user_id = n.user_id
               LEFT JOIN user_notification_preference preference ON preference.user_id=n.user_id
               LEFT JOIN match_notification_preference mute
                 ON mute.user_id=n.user_id AND mute.match_id=(n.payload->>'match_id')::uuid
               LEFT JOIN refresh_token_family session ON session.id = d.session_id
               LEFT JOIN refresh_token_family opened_session
                 ON n.type='new_session' AND opened_session.id=(n.payload->>'session_id')::uuid
               LEFT JOIN match_init m ON m.id = (n.payload->>'match_id')::uuid
               LEFT JOIN user_account first_account ON first_account.user_id = m.user1_id
               LEFT JOIN user_account second_account ON second_account.user_id = m.user2_id
               LEFT JOIN user_profile partner_profile ON partner_profile.user_id =
                 CASE WHEN n.user_id=m.user1_id THEN m.user2_id ELSE m.user1_id END
               WHERE delivery.id = $1 AND account.deleted_at IS NULL AND NOT account.is_banned
                 AND n.expires_at > clock_timestamp() AND n.read_at IS NULL
                 AND ({PUSH_PREFERENCES_ELIGIBLE_SQL})
                 AND (d.session_id IS NULL OR
                      (session.revoked_at IS NULL AND session.expires_at > clock_timestamp()))
                 AND (n.type <> 'new_session' OR d.session_id IS DISTINCT FROM (n.payload->>'session_id')::uuid)
                 AND (({BILLING_NOTIFICATION_ELIGIBLE_SQL}) OR ({ACCOUNT_NOTIFICATION_ELIGIBLE_SQL}) OR (
                   m.status IN ('active', 'awaiting_continuation', 'confirmed')
                   AND m.expires_at > clock_timestamp()
                   AND n.user_id IN (m.user1_id, m.user2_id)
                   AND first_account.deleted_at IS NULL AND NOT first_account.is_banned
                   AND second_account.deleted_at IS NULL AND NOT second_account.is_banned
                   AND NOT EXISTS (
                     SELECT 1 FROM user_block
                     WHERE (blocker_id = m.user1_id AND blocked_id = m.user2_id)
                        OR (blocker_id = m.user2_id AND blocked_id = m.user1_id)
                   )
                   AND (n.type = 'new_match'
                     OR (n.type = 'match_expiring' AND m.status='confirmed'
                       AND m.expires_at=(n.payload->>'expires_at')::timestamptz)
                     OR (n.type = 'first_period_ending' AND m.status='active'
                       AND m.expires_at=(n.payload->>'expires_at')::timestamptz
                       AND NOT EXISTS (SELECT 1 FROM match_state state
                         WHERE state.match_id=m.id AND state.user_id=n.user_id AND state.continued))
                     OR (n.type = 'continuation_requested' AND m.status='awaiting_continuation'
                       AND EXISTS (SELECT 1 FROM match_state actor
                         JOIN match_state recipient ON recipient.match_id=actor.match_id
                         WHERE actor.match_id=m.id AND actor.user_id=(n.payload->>'actor_id')::uuid
                           AND actor.user_id IN (m.user1_id,m.user2_id) AND actor.user_id<>n.user_id
                           AND actor.continued AND recipient.user_id=n.user_id AND NOT recipient.continued))
                     OR (n.type = 'conversation_continued' AND m.status='confirmed')
                     OR (n.type = 'photos_revealed'
                       AND m.photo_reveal_version=(n.payload->>'reveal_version')::bigint
                       AND (SELECT count(*)=2 AND bool_and(state.revealed)
                            FROM match_state state WHERE state.match_id=m.id)
                       AND (SELECT count(DISTINCT photo.user_id)=2 FROM user_photo photo
                            JOIN content_moderation_case moderation ON moderation.photo_id=photo.id AND moderation.status='approved'
                            WHERE photo.user_id IN (m.user1_id,m.user2_id) AND photo.status='ready'))
                     OR (n.type = 'new_message' AND EXISTS (
                       SELECT 1 FROM chat_message message
                       WHERE message.id = (n.payload->>'message_id')::uuid
                         AND message.match_id = m.id AND message.sender_id <> n.user_id
                         AND message.read_at IS NULL
                     )))
                 ))"#
    )
});

#[derive(Clone)]
pub struct PgNotificationDeliveryStore {
    database: Database,
}

impl PgNotificationDeliveryStore {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    async fn pending(&self, id: Uuid) -> Result<Option<PendingPush>, DatabaseError> {
        let Some(row) = sqlx::query(PENDING_PUSH_SQL.as_str())
            .bind(id)
            .fetch_optional(self.database.pool())
            .await
            .map_err(map_sqlx_error)?
        else {
            return Ok(None);
        };
        let kind: String = row.try_get("type").map_err(map_sqlx_error)?;
        let payload: Value = row.try_get("payload").map_err(map_sqlx_error)?;
        Ok(Some(PendingPush {
            language: Language::parse(
                &row.try_get::<String, _>("language")
                    .map_err(map_sqlx_error)?,
            )
            .ok_or(DatabaseError::QueryFailed)?,
            notification_id: row.try_get("notification_id").map_err(map_sqlx_error)?,
            partner_firstname: row.try_get("partner_firstname").map_err(map_sqlx_error)?,
            expires_at: row.try_get("expires_at").map_err(map_sqlx_error)?,
            token: row.try_get("token").map_err(map_sqlx_error)?,
            notification_type: NotificationType::parse(&kind).ok_or(DatabaseError::QueryFailed)?,
            payload: payload
                .as_object()
                .cloned()
                .ok_or(DatabaseError::QueryFailed)?,
        }))
    }
}

impl NotificationDeliveryStore for PgNotificationDeliveryStore {
    fn find_deliverable(&self, id: Uuid) -> DeliveryStoreFuture<'_, Option<PendingPush>> {
        Box::pin(self.pending(id))
    }

    fn remove_token<'a>(&'a self, token: &'a str) -> DeliveryStoreFuture<'a, ()> {
        Box::pin(async move {
            sqlx::query("DELETE FROM device_token WHERE token = $1")
                .bind(token)
                .execute(self.database.pool())
                .await
                .map_err(map_sqlx_error)?;
            Ok(())
        })
    }
}
