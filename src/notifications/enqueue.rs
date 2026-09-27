use std::sync::OnceLock;

use sha2::{Digest as _, Sha256};
use sqlx::PgConnection;
use sqlx::types::Json;
use uuid::Uuid;

use super::domain::NotificationIntent;
use super::eligibility::BILLING_NOTIFICATION_ELIGIBLE_SQL;
use crate::infra::postgres::{DatabaseError, map_sqlx_error};

static ENQUEUE_NOTIFICATION_SQL: OnceLock<String> = OnceLock::new();

/// Persists the inbox notification, one outbox job per currently eligible device,
/// and the delivery references on the caller's business transaction.
pub async fn enqueue_notification(
    connection: &mut PgConnection,
    user_id: Uuid,
    source_id: &str,
    intent: &NotificationIntent,
) -> Result<(), DatabaseError> {
    let prepared = prepare_notification(user_id, source_id, intent)?;
    sqlx::query(enqueue_sql())
        .bind(prepared.notification_id)
        .bind(user_id)
        .bind(intent.notification_type())
        .bind(Json(intent.payload()))
        .bind(prepared.deduplication_key)
        .bind(intent.billing_reference())
        .bind(intent.billing_trial_ends_at())
        .execute(connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

struct PreparedNotification {
    notification_id: Uuid,
    deduplication_key: String,
}

fn prepare_notification(
    user_id: Uuid,
    source_id: &str,
    intent: &NotificationIntent,
) -> Result<PreparedNotification, DatabaseError> {
    let user_id = user_id.hyphenated().to_string();
    let source = [intent.notification_type(), source_id, user_id.as_str()];
    let serialized = serde_json::to_vec(&source).map_err(|_| DatabaseError::QueryFailed)?;
    let deduplication_key = format!("{:x}", Sha256::digest(serialized));
    Ok(PreparedNotification {
        notification_id: Uuid::new_v4(),
        deduplication_key,
    })
}

fn enqueue_sql() -> &'static str {
    ENQUEUE_NOTIFICATION_SQL.get_or_init(|| {
        [
            r#"
            WITH account AS MATERIALIZED (
              SELECT user_id FROM user_account
              WHERE user_id = $2 AND deleted_at IS NULL AND NOT is_banned
              FOR SHARE
            ), candidate AS (
              SELECT $1::uuid AS id, user_id, $3::text AS type, $4::jsonb AS payload,
                $5::text AS deduplication_key, $6::text AS billing_reference,
                $7::timestamptz AS billing_trial_ends_at
              FROM account
            ), created_notification AS (
              INSERT INTO notification
                (id, user_id, type, payload, deduplication_key,
                 billing_reference, billing_trial_ends_at)
              SELECT id, user_id, type, payload, deduplication_key,
                     billing_reference, billing_trial_ends_at
              FROM candidate n
              WHERE n.type IN ('new_match', 'new_message') OR (
            "#,
            BILLING_NOTIFICATION_ELIGIBLE_SQL,
            r#"
              )
              ON CONFLICT (deduplication_key) DO NOTHING
              RETURNING id, user_id
            ), targets AS MATERIALIZED (
              SELECT uuid_generate_v4() AS id, n.id AS notification_id,
                     d.id AS device_id, d.session_id
              FROM created_notification n
              JOIN device_token d ON d.user_id = n.user_id
              LEFT JOIN refresh_token_family session ON session.id = d.session_id
              WHERE d.session_id IS NULL
                 OR (session.revoked_at IS NULL AND session.expires_at > clock_timestamp())
            ), jobs AS (
              INSERT INTO outbox_event (id, event_type, aggregate_id)
              SELECT id, 'notification.push', id FROM targets
              RETURNING id
            )
            INSERT INTO notification_push_delivery
              (id, notification_id, device_id, session_id)
            SELECT targets.id, notification_id, device_id, session_id
            FROM targets JOIN jobs ON jobs.id = targets.id
            "#,
        ]
        .concat()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deduplication_matches_the_compact_javascript_array_shape() {
        let user_id = Uuid::new_v4();
        let source_id = Uuid::new_v4().hyphenated().to_string();
        let intent = NotificationIntent::NewMatch {
            match_id: Uuid::new_v4(),
        };
        let first =
            prepare_notification(user_id, &source_id, &intent).expect("preparation should succeed");
        let second =
            prepare_notification(user_id, &source_id, &intent).expect("preparation should succeed");
        let javascript_shape = format!(r#"["new_match","{source_id}","{}"]"#, user_id.hyphenated());
        assert_eq!(
            first.deduplication_key,
            format!("{:x}", Sha256::digest(javascript_shape.as_bytes()))
        );
        assert_eq!(first.deduplication_key, second.deduplication_key);
        assert_ne!(first.notification_id, second.notification_id);
    }

    #[test]
    fn message_payload_is_an_explicit_content_free_allowlist() {
        let match_id = Uuid::new_v4();
        let message_id = Uuid::new_v4();
        let sender_id = Uuid::new_v4();
        let payload = NotificationIntent::NewMessage {
            match_id,
            message_id,
            sender_id,
        }
        .payload();
        assert_eq!(
            payload,
            serde_json::json!({
                "match_id": match_id,
                "message_id": message_id,
                "sender_id": sender_id,
            })
        );
        assert!(!payload.to_string().contains("content"));
    }
}
