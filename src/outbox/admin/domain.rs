use crate::identity::admin_role::AdminRole;
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

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
