use super::domain::{
    ContinuationResult, CursorMessageRow, EffectivePlan, MatchCommandResult, MatchRecord,
    MessageCreationResult, MessageRead, PageCursor, UserMatchRow,
};
use crate::infra::postgres::{ConstraintKind, DatabaseError};
use chrono::{DateTime, NaiveDate, Utc};
use std::{future::Future, pin::Pin};
use uuid::Uuid;

pub type MatchStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, MatchStoreError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchStoreError {
    Blocked,
    ParticipantUnavailable,
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
    ) -> MatchStoreFuture<'_, MatchCommandResult<bool>>;

    fn participant_ids(
        &self,
        match_id: Uuid,
        user_id: Uuid,
    ) -> MatchStoreFuture<'_, Option<[Uuid; 2]>>;

    fn effective_plan(
        &self,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> MatchStoreFuture<'_, EffectivePlan>;

    fn continuation_usage(&self, user_id: Uuid, week_start: NaiveDate)
    -> MatchStoreFuture<'_, i32>;

    fn record_continuation(
        &self,
        match_id: Uuid,
        user_id: Uuid,
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
