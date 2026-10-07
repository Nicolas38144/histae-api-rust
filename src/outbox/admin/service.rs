use super::{domain::*, store::OutboxAdminStore};
use crate::{
    infra::postgres::DatabaseError,
    shared::text::{javascript_trim, validator_js_length},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::{fmt, sync::Arc};
use unicode_normalization::UnicodeNormalization as _;
use uuid::Uuid;

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
        let reason = normalize_reason(operator.role.audit_reason(raw_reason, "Superadmin retry"))?;
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
        let reason =
            normalize_reason(operator.role.audit_reason(raw_reason, "Superadmin discard"))?;
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

    use super::super::store::OutboxAdminFuture;
    use super::*;
    use crate::identity::admin_role::AdminRole;

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
        assert!(!json.to_string().contains("aggregate_id"));
        assert!(!json.to_string().contains("payload"));
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
