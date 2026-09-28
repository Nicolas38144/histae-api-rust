use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::profiles::domain::{ModerationReason, ModerationStatus};

pub const PHOTO_URL_TTL_SECONDS: u32 = 300;
pub const PHOTO_IDEMPOTENCY_HOURS: i64 = 24;
pub const PHOTO_CACHE_CONTROL: &str = "private, max-age=300";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UploadPhoto {
    pub filename: String,
    pub mime_type: String,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ProcessingPhoto {
    pub id: Uuid,
    pub user_id: Uuid,
    pub object_key: String,
    pub idempotency_key: Uuid,
    pub request_sha256: [u8; 32],
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhotoObject {
    pub id: Uuid,
    pub user_id: Uuid,
    pub object_key: String,
    pub moderation_status: ModerationStatus,
    pub moderation_reasons: Vec<ModerationReason>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CreationResult {
    Created,
    Replay(PhotoObject),
    ProfileNotFound,
    UpdateInProgress,
    IdempotencyConflict,
    IdempotencyConsumed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UploadResult {
    pub photo: String,
    pub moderation_status: ModerationStatus,
    pub moderation_reasons: Vec<ModerationReason>,
}
