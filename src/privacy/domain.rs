use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BlockedUser {
    pub user_id: Uuid,
    pub firstname: Option<String>,
    pub photo: Option<String>,
    pub blocked_at: String,
}

#[derive(Clone, Debug)]
pub struct BlockedUserRow {
    pub user_id: Uuid,
    pub firstname: Option<String>,
    pub blocked_at: DateTime<Utc>,
}

impl From<BlockedUserRow> for BlockedUser {
    fn from(row: BlockedUserRow) -> Self {
        Self {
            user_id: row.user_id,
            firstname: row.firstname,
            photo: None,
            blocked_at: row.blocked_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        }
    }
}
