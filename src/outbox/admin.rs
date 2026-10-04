use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row as _;
use unicode_normalization::UnicodeNormalization as _;
use uuid::Uuid;

use crate::identity::admin_role::AdminRole;
use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::shared::text::{javascript_trim, validator_js_length};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DeadLetter {
    pub event_id: Uuid,
    pub event_type: String,
    pub attempts: u16,
    pub last_error_code: Option<String>,
    pub created_at: String,
    pub dead_lettered_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeadLetterRow {
    pub id: Uuid,
    pub event_type: String,
    pub attempts: u16,
    pub last_error_code: Option<String>,
    pub created_at: String,
    pub dead_lettered_at: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadLetterCursor {
    pub at: DateTime<Utc>,
    pub id: Uuid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutboxOperator {
    pub user_id: Uuid,
    pub role: AdminRole,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperatorResult {
    Updated,
    NotFound,
    NotDeadLetter,
    DiscardNotAllowed,
}

pub type OutboxAdminFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait OutboxAdminStore: Send + Sync {
    fn list_dead_letters(
        &self,
        limit: u32,
        cursor: Option<DeadLetterCursor>,
    ) -> OutboxAdminFuture<'_, Vec<DeadLetterRow>>;

    fn retry_dead_letter(
        &self,
        event_id: Uuid,
        operator: OutboxOperator,
        reason: String,
    ) -> OutboxAdminFuture<'_, OperatorResult>;

    fn discard_dead_letter(
        &self,
        event_id: Uuid,
        operator: OutboxOperator,
        reason: String,
    ) -> OutboxAdminFuture<'_, OperatorResult>;
}

#[derive(Clone)]
pub struct PgOutboxAdminRepository {
    database: Database,
}

impl PgOutboxAdminRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    async fn resolve(
        &self,
        event_id: Uuid,
        operator: OutboxOperator,
        reason: String,
        action: OperatorAction,
    ) -> Result<OperatorResult, DatabaseError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let event = sqlx::query(
                        "SELECT event_type, aggregate_id, status
                         FROM outbox_event WHERE id = $1 FOR UPDATE",
                    )
                    .bind(event_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    let Some(event) = event else {
                        return Ok(OperatorResult::NotFound);
                    };
                    let event_type: String = event.try_get("event_type").map_err(map_sqlx_error)?;
                    let aggregate_id: Uuid =
                        event.try_get("aggregate_id").map_err(map_sqlx_error)?;
                    let status: String = event.try_get("status").map_err(map_sqlx_error)?;
                    if status != "dead_letter" {
                        return Ok(OperatorResult::NotDeadLetter);
                    }
                    if action == OperatorAction::Discard && event_type != "notification.push" {
                        if event_type != "photo.delete" {
                            return Ok(OperatorResult::DiscardNotAllowed);
                        }
                        let photo_exists =
                            sqlx::query_scalar::<_, i32>("SELECT 1 FROM user_photo WHERE id = $1")
                                .bind(aggregate_id)
                                .fetch_optional(&mut *connection)
                                .await
                                .map_err(map_sqlx_error)?
                                .is_some();
                        if photo_exists {
                            return Ok(OperatorResult::DiscardNotAllowed);
                        }
                    }
                    sqlx::query(
                        "INSERT INTO outbox_operator_action
                           (outbox_event_id, administrator_id, administrator_role,
                            event_type, action, reason)
                         VALUES ($1, $2, $3, $4, $5, $6)",
                    )
                    .bind(event_id)
                    .bind(operator.user_id)
                    .bind(operator.role.as_str())
                    .bind(&event_type)
                    .bind(action.as_str())
                    .bind(&reason)
                    .execute(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?;
                    match action {
                        OperatorAction::Retry => {
                            sqlx::query(
                                "UPDATE outbox_event
                                 SET status = 'pending', attempts = 0,
                                   available_at = clock_timestamp(), locked_at = NULL,
                                   locked_by = NULL, last_error_code = NULL,
                                   processed_at = NULL, dead_lettered_at = NULL,
                                   resolved_at = NULL, resolved_by = NULL,
                                   resolution_reason = NULL
                                 WHERE id = $1",
                            )
                            .bind(event_id)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                        }
                        OperatorAction::Discard => {
                            sqlx::query(
                                "UPDATE outbox_event
                                 SET status = 'discarded', locked_at = NULL,
                                   locked_by = NULL, processed_at = NULL,
                                   resolved_at = clock_timestamp(), resolved_by = $2,
                                   resolution_reason = $3
                                 WHERE id = $1",
                            )
                            .bind(event_id)
                            .bind(operator.user_id)
                            .bind(&reason)
                            .execute(&mut *connection)
                            .await
                            .map_err(map_sqlx_error)?;
                        }
                    }
                    Ok(OperatorResult::Updated)
                })
            })
            .await
    }
}

impl OutboxAdminStore for PgOutboxAdminRepository {
    fn list_dead_letters(
        &self,
        limit: u32,
        cursor: Option<DeadLetterCursor>,
    ) -> OutboxAdminFuture<'_, Vec<DeadLetterRow>> {
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT id, event_type, attempts, last_error_code,
                        to_char(created_at AT TIME ZONE 'UTC',
                          'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS created_at,
                        to_char(dead_lettered_at AT TIME ZONE 'UTC',
                          'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS dead_lettered_at
                 FROM outbox_event
                 WHERE status = 'dead_letter'
                   AND ($2::timestamptz IS NULL
                     OR (dead_lettered_at, id) < ($2::timestamptz, $3::uuid))
                 ORDER BY dead_lettered_at DESC, id DESC LIMIT $1",
            )
            .bind(i64::from(limit))
            .bind(cursor.map(|value| value.at))
            .bind(cursor.map(|value| value.id))
            .fetch_all(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            rows.into_iter()
                .map(|row| {
                    let attempts: i16 = row.try_get("attempts").map_err(map_sqlx_error)?;
                    Ok(DeadLetterRow {
                        id: row.try_get("id").map_err(map_sqlx_error)?,
                        event_type: row.try_get("event_type").map_err(map_sqlx_error)?,
                        attempts: u16::try_from(attempts)
                            .map_err(|_| DatabaseError::QueryFailed)?,
                        last_error_code: row.try_get("last_error_code").map_err(map_sqlx_error)?,
                        created_at: row.try_get("created_at").map_err(map_sqlx_error)?,
                        dead_lettered_at: row
                            .try_get("dead_lettered_at")
                            .map_err(map_sqlx_error)?,
                    })
                })
                .collect()
        })
    }

    fn retry_dead_letter(
        &self,
        event_id: Uuid,
        operator: OutboxOperator,
        reason: String,
    ) -> OutboxAdminFuture<'_, OperatorResult> {
        Box::pin(self.resolve(event_id, operator, reason, OperatorAction::Retry))
    }

    fn discard_dead_letter(
        &self,
        event_id: Uuid,
        operator: OutboxOperator,
        reason: String,
    ) -> OutboxAdminFuture<'_, OperatorResult> {
        Box::pin(self.resolve(event_id, operator, reason, OperatorAction::Discard))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperatorAction {
    Retry,
    Discard,
}

impl OperatorAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::Discard => "discard",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeadLetterPage {
    pub events: Vec<DeadLetter>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboxAdminError {
    InvalidRequest,
    InvalidCursor,
    EventNotFound,
    EventNotDeadLetter,
    DiscardNotAllowed,
    Database(DatabaseError),
}

impl fmt::Display for OutboxAdminError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRequest => "invalid_outbox_request",
            Self::InvalidCursor => "invalid_cursor",
            Self::EventNotFound => "outbox_event_not_found",
            Self::EventNotDeadLetter => "outbox_event_not_dead_letter",
            Self::DiscardNotAllowed => "outbox_discard_not_allowed",
            Self::Database(error) => error.safe_code(),
        })
    }
}

impl std::error::Error for OutboxAdminError {}

impl From<DatabaseError> for OutboxAdminError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

#[derive(Clone)]
pub struct OutboxAdminService {
    store: Arc<dyn OutboxAdminStore>,
}

impl OutboxAdminService {
    pub fn new(store: Arc<dyn OutboxAdminStore>) -> Self {
        Self { store }
    }

    pub async fn dead_letters(
        &self,
        limit: u32,
        raw_cursor: Option<&str>,
    ) -> Result<DeadLetterPage, OutboxAdminError> {
        if !(1..=100).contains(&limit) {
            return Err(OutboxAdminError::InvalidRequest);
        }
        let cursor = decode_cursor(raw_cursor)?;
        let mut rows = self
            .store
            .list_dead_letters(limit.saturating_add(1), cursor)
            .await?;
        let has_more = rows.len() > usize::try_from(limit).unwrap_or(usize::MAX);
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        let next_cursor = if has_more {
            rows.last()
                .map(|row| encode_cursor(&row.dead_lettered_at, row.id))
                .transpose()?
        } else {
            None
        };
        Ok(DeadLetterPage {
            events: rows
                .into_iter()
                .map(|row| DeadLetter {
                    event_id: row.id,
                    event_type: row.event_type,
                    attempts: row.attempts,
                    last_error_code: row.last_error_code,
                    created_at: row.created_at,
                    dead_lettered_at: row.dead_lettered_at,
                })
                .collect(),
            next_cursor,
        })
    }

    pub async fn retry(
        &self,
        event_id: Uuid,
        operator: OutboxOperator,
        raw_reason: &str,
    ) -> Result<(), OutboxAdminError> {
        let reason = normalize_reason(raw_reason)?;
        map_operator_result(
            self.store
                .retry_dead_letter(event_id, operator, reason)
                .await?,
        )
    }

    pub async fn discard(
        &self,
        event_id: Uuid,
        operator: OutboxOperator,
        raw_reason: &str,
    ) -> Result<(), OutboxAdminError> {
        let reason = normalize_reason(raw_reason)?;
        map_operator_result(
            self.store
                .discard_dead_letter(event_id, operator, reason)
                .await?,
        )
    }
}

fn map_operator_result(result: OperatorResult) -> Result<(), OutboxAdminError> {
    match result {
        OperatorResult::Updated => Ok(()),
        OperatorResult::NotFound => Err(OutboxAdminError::EventNotFound),
        OperatorResult::NotDeadLetter => Err(OutboxAdminError::EventNotDeadLetter),
        OperatorResult::DiscardNotAllowed => Err(OutboxAdminError::DiscardNotAllowed),
    }
}

fn normalize_reason(value: &str) -> Result<String, OutboxAdminError> {
    let normalized = javascript_trim(value).nfkc().collect::<String>();
    let normalized = javascript_trim(&normalized).to_owned();
    if !(3..=500).contains(&validator_js_length(&normalized))
        || normalized
            .chars()
            .any(|character| character <= '\u{001f}' || character == '\u{007f}')
    {
        return Err(OutboxAdminError::InvalidRequest);
    }
    Ok(normalized)
}

#[derive(Deserialize, Serialize)]
struct CursorWire {
    at: String,
    id: String,
}

fn decode_cursor(value: Option<&str>) -> Result<Option<DeadLetterCursor>, OutboxAdminError> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| OutboxAdminError::InvalidCursor)?;
    let wire: CursorWire =
        serde_json::from_slice(&decoded).map_err(|_| OutboxAdminError::InvalidCursor)?;
    let at = DateTime::parse_from_rfc3339(&wire.at)
        .map_err(|_| OutboxAdminError::InvalidCursor)?
        .with_timezone(&Utc);
    if !valid_cursor_timestamp(&wire.at) {
        return Err(OutboxAdminError::InvalidCursor);
    }
    let id = Uuid::parse_str(&wire.id).map_err(|_| OutboxAdminError::InvalidCursor)?;
    if wire.id.len() != 36 || id.hyphenated().to_string() != wire.id.to_ascii_lowercase() {
        return Err(OutboxAdminError::InvalidCursor);
    }
    Ok(Some(DeadLetterCursor { at, id }))
}

fn encode_cursor(at: &str, id: Uuid) -> Result<String, OutboxAdminError> {
    let timestamp = DateTime::parse_from_rfc3339(at)
        .map_err(|_| OutboxAdminError::InvalidCursor)?
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Millis, true);
    let encoded = serde_json::to_vec(&CursorWire {
        at: timestamp,
        id: id.hyphenated().to_string(),
    })
    .map_err(|_| OutboxAdminError::InvalidCursor)?;
    Ok(URL_SAFE_NO_PAD.encode(encoded))
}

fn valid_cursor_timestamp(value: &str) -> bool {
    let Some((date, suffix)) = value.split_once('.') else {
        return false;
    };
    date.len() == 19
        && suffix.ends_with('Z')
        && (suffix.len() == 4 || suffix.len() == 7)
        && suffix[..suffix.len() - 1]
            .bytes()
            .all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct Store {
        rows: Mutex<Vec<DeadLetterRow>>,
        result: Mutex<OperatorResult>,
        reasons: Mutex<Vec<String>>,
    }

    impl Store {
        fn new(rows: Vec<DeadLetterRow>, result: OperatorResult) -> Self {
            Self {
                rows: Mutex::new(rows),
                result: Mutex::new(result),
                reasons: Mutex::new(Vec::new()),
            }
        }
    }

    impl OutboxAdminStore for Store {
        fn list_dead_letters(
            &self,
            _limit: u32,
            _cursor: Option<DeadLetterCursor>,
        ) -> OutboxAdminFuture<'_, Vec<DeadLetterRow>> {
            Box::pin(async move {
                self.rows
                    .lock()
                    .map(|rows| rows.clone())
                    .map_err(|_| DatabaseError::QueryFailed)
            })
        }

        fn retry_dead_letter(
            &self,
            _event_id: Uuid,
            _operator: OutboxOperator,
            reason: String,
        ) -> OutboxAdminFuture<'_, OperatorResult> {
            Box::pin(async move {
                self.reasons
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)?
                    .push(reason);
                self.result
                    .lock()
                    .map(|result| *result)
                    .map_err(|_| DatabaseError::QueryFailed)
            })
        }

        fn discard_dead_letter(
            &self,
            event_id: Uuid,
            operator: OutboxOperator,
            reason: String,
        ) -> OutboxAdminFuture<'_, OperatorResult> {
            self.retry_dead_letter(event_id, operator, reason)
        }
    }

    #[test]
    fn cursor_round_trip_and_reason_normalization_preserve_the_nest_contract() {
        let id = Uuid::new_v4();
        let cursor =
            encode_cursor("2030-01-01T00:00:00.123456Z", id).unwrap_or_else(|_| unreachable!());
        let decoded = decode_cursor(Some(&cursor))
            .unwrap_or_else(|_| unreachable!())
            .unwrap_or_else(|| unreachable!());
        assert_eq!(decoded.id, id);
        assert_eq!(decoded.at.timestamp_subsec_millis(), 123);
        assert_eq!(
            normalize_reason("  Stockage re\u{301}tabli  "),
            Ok("Stockage rétabli".to_owned())
        );
        assert_eq!(
            normalize_reason("ab\n"),
            Err(OutboxAdminError::InvalidRequest)
        );
    }

    #[tokio::test]
    async fn exposes_only_the_minimal_page_and_normalizes_audited_reasons() {
        let event_id = Uuid::new_v4();
        let store = Arc::new(Store::new(
            vec![DeadLetterRow {
                id: event_id,
                event_type: "photo.delete".to_owned(),
                attempts: 10,
                last_error_code: Some("object_storage_unavailable".to_owned()),
                created_at: "2030-01-01T00:00:00.000000Z".to_owned(),
                dead_lettered_at: "2030-01-02T00:00:00.000000Z".to_owned(),
            }],
            OperatorResult::Updated,
        ));
        let service = OutboxAdminService::new(store.clone());
        let page = service
            .dead_letters(20, None)
            .await
            .unwrap_or_else(|_| unreachable!());
        let json = serde_json::to_value(&page.events).unwrap_or_else(|_| unreachable!());
        assert_eq!(page.events[0].event_id, event_id);
        assert!(json.to_string().find("aggregate_id").is_none());
        assert!(json.to_string().find("payload").is_none());
        service
            .retry(
                event_id,
                OutboxOperator {
                    user_id: Uuid::new_v4(),
                    role: AdminRole::Admin,
                },
                "  Stockage re\u{301}tabli  ",
            )
            .await
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            store
                .reasons
                .lock()
                .unwrap_or_else(|_| unreachable!())
                .as_slice(),
            ["Stockage rétabli"]
        );
    }

    #[tokio::test]
    async fn maps_missing_stale_and_forbidden_operator_decisions() {
        for (result, expected) in [
            (OperatorResult::NotFound, OutboxAdminError::EventNotFound),
            (
                OperatorResult::NotDeadLetter,
                OutboxAdminError::EventNotDeadLetter,
            ),
            (
                OperatorResult::DiscardNotAllowed,
                OutboxAdminError::DiscardNotAllowed,
            ),
        ] {
            let service = OutboxAdminService::new(Arc::new(Store::new(Vec::new(), result)));
            assert_eq!(
                service
                    .discard(
                        Uuid::new_v4(),
                        OutboxOperator {
                            user_id: Uuid::new_v4(),
                            role: AdminRole::Superadmin,
                        },
                        "Décision opérateur",
                    )
                    .await,
                Err(expected)
            );
        }
    }
}
