use super::domain::{
    ContinuationResult, CursorMessageRow, MatchCommandResult, MatchEnd, MatchRecord,
    MessageCreationResult, MessageRead, PageCursor, PhotoRevealConsent, UserMatchRow,
};
use crate::infra::postgres::{ConstraintKind, DatabaseError};
use chrono::{DateTime, Utc};
use std::{future::Future, pin::Pin};
use uuid::Uuid;

pub type MatchStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, MatchStoreError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchStoreError {
    Blocked,
    ParticipantUnavailable,
    ContinuationExpired,
    PhotoRequired,
    Database(DatabaseError),
}

impl MatchStoreError {
    pub const fn is_unique(self) -> bool {
        matches!(
            self,
            Self::Database(DatabaseError::Constraint(ConstraintKind::Unique))
        )
    }
}

impl From<DatabaseError> for MatchStoreError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

pub trait MatchStore: Send + Sync {
    fn create(&self, record: MatchRecord) -> MatchStoreFuture<'_, ()>;

    fn find_by_pair(
        &self,
        user1_id: Uuid,
        user2_id: Uuid,
    ) -> MatchStoreFuture<'_, Option<MatchRecord>>;

    fn list_for_user(
        &self,
        user_id: Uuid,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> MatchStoreFuture<'_, Vec<UserMatchRow>>;

    fn record_reveal(
        &self,
        match_id: Uuid,
        user_id: Uuid,
        consent: PhotoRevealConsent,
    ) -> MatchStoreFuture<'_, MatchCommandResult<bool>>;

    fn record_end(&self, match_id: Uuid, user_id: Uuid) -> MatchStoreFuture<'_, Option<MatchEnd>>;

    fn participant_ids(
        &self,
        match_id: Uuid,
        user_id: Uuid,
    ) -> MatchStoreFuture<'_, Option<[Uuid; 2]>>;

    fn quota_snapshot(
        &self,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> MatchStoreFuture<'_, super::domain::ContinuationQuota>;

    fn record_continuation(
        &self,
        match_id: Uuid,
        user_id: Uuid,
        acknowledged_cost: i32,
    ) -> MatchStoreFuture<'_, ContinuationResult>;
}

pub trait MatchMessageStore: Send + Sync {
    fn messages_for_user(
        &self,
        match_id: Uuid,
        user_id: Uuid,
        limit: u32,
        offset: u32,
        cursor: Option<PageCursor>,
    ) -> MatchStoreFuture<'_, MatchCommandResult<Vec<CursorMessageRow>>>;

    fn create_message(
        &self,
        message_id: Uuid,
        match_id: Uuid,
        sender_id: Uuid,
        content: String,
        idempotency_key: Uuid,
    ) -> MatchStoreFuture<'_, MessageCreationResult>;

    fn mark_message_read(
        &self,
        match_id: Uuid,
        message_id: Uuid,
        user_id: Uuid,
    ) -> MatchStoreFuture<'_, MatchCommandResult<Option<MessageRead>>>;

    fn mark_messages_read_through(
        &self,
        match_id: Uuid,
        message_id: Uuid,
        user_id: Uuid,
    ) -> MatchStoreFuture<'_, MatchCommandResult<Option<MessageRead>>>;
}

impl std::fmt::Display for MatchStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Blocked => "match_blocked",
            Self::ParticipantUnavailable => "participant_unavailable",
            Self::ContinuationExpired => "match_expired",
            Self::PhotoRequired => "photo_required",
            Self::Database(error) => error.safe_code(),
        })
    }
}
impl std::error::Error for MatchStoreError {}
