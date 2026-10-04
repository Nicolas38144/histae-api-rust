use crate::infra::postgres::DatabaseError;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
pub const PHOTO_PROCESSING_STALE_MINUTES: i64 = 30;
pub const OUTBOX_LOCK_STALE_MINUTES: i64 = 5;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PhotoReconciliationFilter {
    #[default]
    All,
    StaleProcessing,
    Deleting,
    DeadLetter,
}

impl PhotoReconciliationFilter {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::StaleProcessing => "stale_processing",
            Self::Deleting => "deleting",
            Self::DeadLetter => "dead_letter",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AdminPhotoReconciliation {
    pub photo_id: Uuid,
    pub user_id: Uuid,
    pub status: String,
    pub size_bytes: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub created_at: String,
    pub updated_at: String,
    pub outbox_status: Option<String>,
    pub outbox_attempts: Option<i16>,
    pub outbox_available_at: Option<String>,
    pub outbox_locked_at: Option<String>,
    pub outbox_last_error_code: Option<String>,
    pub issue: String,
}

#[derive(Clone, Debug)]
pub struct AdminPhotoRow {
    pub item: AdminPhotoReconciliation,
    pub cursor_at: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconciliationResult {
    Queued,
    NotFound,
    NotActionable,
    AlreadyProcessing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdminPhotoError {
    InvalidRequest,
    InvalidCursor,
    NotFound,
    NotActionable,
    AlreadyProcessing,
    Database(DatabaseError),
}

impl From<DatabaseError> for AdminPhotoError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AdminPhotoPage {
    pub photos: Vec<AdminPhotoReconciliation>,
    pub next_cursor: Option<String>,
}
