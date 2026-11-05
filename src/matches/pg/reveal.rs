use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::notifications::{domain::NotificationIntent, enqueue_notification};

use super::access::lock_available_match;
use crate::infra::postgres::map_sqlx_error;
use crate::matches::domain::{MatchAvailabilityFailure, MatchCommandResult, PhotoRevealConsent};
use crate::matches::store::MatchStoreError;

pub(super) async fn record_reveal(
    connection: &mut PgConnection,
    match_id: Uuid,
    user_id: Uuid,
    consent: PhotoRevealConsent,
) -> Result<MatchCommandResult<bool>, MatchStoreError> {
    let record = match lock_available_match(connection, match_id, user_id).await? {
        MatchCommandResult::Available(record) => record,
        MatchCommandResult::Unavailable(reason) => {
            return Ok(MatchCommandResult::Unavailable(reason));
        }
    };

    // A withdrawal must remain possible after either photo has been removed.
    if consent.is_granted() {
        let photos_ready: bool = sqlx::query_scalar(PARTICIPANT_PHOTOS_SQL)
            .bind([record.user1_id, record.user2_id].as_slice())
            .fetch_one(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
        if !photos_ready {
            return Err(MatchStoreError::PhotoRequired);
        }
    }

    let was_revealed: bool = sqlx::query_scalar(
        "SELECT count(*)=2 AND COALESCE(bool_and(revealed),false) FROM match_state WHERE match_id=$1"
    )
    .bind(match_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;

    let row = sqlx::query(RECORD_REVEAL_SQL)
        .bind(match_id)
        .bind(user_id)
        .bind(consent.is_granted())
        .fetch_one(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    if !row.try_get::<bool, _>("updated").map_err(map_sqlx_error)? {
        return Ok(MatchCommandResult::Unavailable(
            MatchAvailabilityFailure::NotFound,
        ));
    }
    let revealed: bool = row.try_get("revealed").map_err(map_sqlx_error)?;
    if revealed && !was_revealed {
        let version: i64 = sqlx::query_scalar("UPDATE match_init SET photo_reveal_version=photo_reveal_version+1 WHERE id=$1 RETURNING photo_reveal_version")
            .bind(match_id)
            .fetch_one(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
        let intent = NotificationIntent::PhotosRevealed {
            match_id,
            reveal_version: version,
        };
        let source = format!("{match_id}:{version}");
        for participant in [record.user1_id, record.user2_id] {
            enqueue_notification(connection, participant, &source, &intent).await?;
        }
    }
    Ok(MatchCommandResult::Available(revealed))
}

const PARTICIPANT_PHOTOS_SQL: &str = r#"
SELECT count(DISTINCT photo.user_id) = 2
FROM user_photo AS photo
JOIN content_moderation_case AS moderation
  ON moderation.photo_id = photo.id AND moderation.status = 'approved'
WHERE photo.user_id = ANY($1::uuid[]) AND photo.status = 'ready'
"#;

// The SELECT shares the UPDATE's snapshot. Overlay the caller's new consent
// explicitly rather than reading its old value from match_state.
const RECORD_REVEAL_SQL: &str = r#"
WITH updated AS (
  UPDATE match_state SET revealed = $3
  WHERE match_id = $1 AND user_id = $2
  RETURNING match_id
)
SELECT EXISTS (SELECT 1 FROM updated) AS updated,
  COALESCE((
    SELECT count(*) = 2
      AND bool_and(CASE WHEN state.user_id = $2 THEN $3 ELSE state.revealed END)
    FROM match_state AS state
    WHERE state.match_id = (SELECT match_id FROM updated)
  ), false) AS revealed
"#;
