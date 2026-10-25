use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant, Version};

use super::domain::{
    ContinuationQuota, ContinuationResult, MATCH_WINDOW_HOURS, MatchAvailabilityFailure,
    MatchCommandResult, MatchRecord, MatchStatus, MessageCreationResult, MessageRead, PageCursor,
    PublicMatch, PublicMessage, PublicUserMatch,
};
use super::store::{MatchMessageStore, MatchStore, MatchStoreError};
use crate::infra::postgres::DatabaseError;
use crate::profiles::service::ProfilePhotoUrlProvider;
use crate::shared::clock::Clock;
use crate::shared::text::javascript_trim;

pub type MatchEventFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchUpdate {
    PhotosRevealed(bool),
    MatchConfirmed(bool),
    Ended,
}

pub trait MatchEventPublisher: Send + Sync {
    fn created<'a>(&'a self, item: &'a PublicMatch) -> MatchEventFuture<'a>;

    fn updated<'a>(
        &'a self,
        match_id: Uuid,
        participants: [Uuid; 2],
        update: MatchUpdate,
    ) -> MatchEventFuture<'a>;

    fn message_created<'a>(
        &'a self,
        message: &'a PublicMessage,
        participants: [Uuid; 2],
    ) -> MatchEventFuture<'a>;

    fn messages_read<'a>(
        &'a self,
        match_id: Uuid,
        participants: [Uuid; 2],
        reader_id: Uuid,
        read_through_message_id: Uuid,
    ) -> MatchEventFuture<'a>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopMatchEventPublisher;

impl MatchEventPublisher for NoopMatchEventPublisher {
    fn created<'a>(&'a self, _item: &'a PublicMatch) -> MatchEventFuture<'a> {
        Box::pin(async {})
    }

    fn updated<'a>(
        &'a self,
        _match_id: Uuid,
        _participants: [Uuid; 2],
        _update: MatchUpdate,
    ) -> MatchEventFuture<'a> {
        Box::pin(async {})
    }

    fn message_created<'a>(
        &'a self,
        _message: &'a PublicMessage,
        _participants: [Uuid; 2],
    ) -> MatchEventFuture<'a> {
        Box::pin(async {})
    }

    fn messages_read<'a>(
        &'a self,
        _match_id: Uuid,
        _participants: [Uuid; 2],
        _reader_id: Uuid,
        _read_through_message_id: Uuid,
    ) -> MatchEventFuture<'a> {
        Box::pin(async {})
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchError {
    InvalidRequest,
    InvalidMessageRequest,
    InvalidIdempotencyKey,
    InvalidCursor,
    NotFound,
    Blocked,
    CandidateNotFound,
    InvalidState,
    ContinuationNotAvailableYet,
    Expired,
    QuotaReached,
    PlanChanged,
    MessagingUnavailable,
    MessageNotFound,
    IdempotencyConflict,
    PhotoStorageUnavailable,
    PhotoRequired,
    Database(DatabaseError),
}

impl From<MatchStoreError> for MatchError {
    fn from(error: MatchStoreError) -> Self {
        match error {
            MatchStoreError::Blocked => Self::Blocked,
            MatchStoreError::ParticipantUnavailable => Self::CandidateNotFound,
            MatchStoreError::ContinuationExpired => Self::Expired,
            MatchStoreError::PhotoRequired => Self::PhotoRequired,
            MatchStoreError::Database(error) => Self::Database(error),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MatchPage {
    pub items: Vec<PublicUserMatch>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MessagePage {
    pub items: Vec<PublicMessage>,
    pub next_cursor: Option<String>,
}

#[derive(Clone)]
pub struct MatchService {
    store: Arc<dyn MatchStore>,
    messages: Arc<dyn MatchMessageStore>,
    photo_urls: Arc<dyn ProfilePhotoUrlProvider>,
    events: Arc<dyn MatchEventPublisher>,
    clock: Arc<dyn Clock>,
}

impl MatchService {
    pub fn new(
        store: Arc<dyn MatchStore>,
        messages: Arc<dyn MatchMessageStore>,
        photo_urls: Arc<dyn ProfilePhotoUrlProvider>,
        events: Arc<dyn MatchEventPublisher>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            messages,
            photo_urls,
            events,
            clock,
        }
    }

    pub async fn create_from_mutual_like(
        &self,
        first_user_id: Uuid,
        second_user_id: Uuid,
    ) -> Result<PublicMatch, MatchError> {
        if first_user_id.is_nil() || second_user_id.is_nil() || first_user_id == second_user_id {
            return Err(MatchError::InvalidRequest);
        }
        let [user1_id, user2_id] = if first_user_id < second_user_id {
            [first_user_id, second_user_id]
        } else {
            [second_user_id, first_user_id]
        };
        let now = self.clock.now();
        let record = MatchRecord {
            id: Uuid::new_v4(),
            user1_id,
            user2_id,
            status: MatchStatus::Active,
            expires_at: now + TimeDelta::hours(MATCH_WINDOW_HOURS),
            purge_after: None,
            continuation_initiator_id: None,
            created_at: now,
            last_message_at: None,
        };
        match self.store.create(record.clone()).await {
            Ok(()) => {
                let public = PublicMatch::from(record);
                self.events.created(&public).await;
                Ok(public)
            }
            Err(error) if error.is_unique() => self
                .store
                .find_by_pair(user1_id, user2_id)
                .await?
                .map(PublicMatch::from)
                .ok_or(MatchError::Database(DatabaseError::QueryFailed)),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn list(
        &self,
        user_id: Uuid,
        limit: u32,
        offset: u32,
        raw_cursor: Option<&str>,
    ) -> Result<MatchPage, MatchError> {
        if user_id.is_nil()
            || !(1..=100).contains(&limit)
            || (raw_cursor.is_some_and(|value| !value.is_empty()) && offset != 0)
        {
            return Err(MatchError::InvalidRequest);
        }
        let cursor = decode_cursor(raw_cursor)?;
        let rows = self
            .store
            .list_for_user(user_id, limit + 1, offset, cursor)
            .await?;
        let has_more = rows.len() > limit as usize;
        let next_cursor = if has_more {
            rows.get(limit as usize - 1)
                .map(|row| encode_cursor(&row.cursor_at, row.record.id))
                .transpose()?
        } else {
            None
        };
        let mut items = Vec::with_capacity(limit.min(rows.len() as u32) as usize);
        for mut row in rows.into_iter().take(limit as usize) {
            let now = self.clock.now();
            if row.record.status == MatchStatus::Active && row.record.expires_at <= now {
                row.record.status = MatchStatus::AwaitingContinuation;
                row.record.expires_at += TimeDelta::hours(MATCH_WINDOW_HOURS);
            }
            if row.record.status == MatchStatus::AwaitingContinuation
                && row.record.expires_at <= now
            {
                row.record.status = MatchStatus::Expired;
            }
            let photo = self
                .photo_urls
                .url_for_key(row.other_photo.clone())
                .await
                .map_err(|_| MatchError::PhotoStorageUnavailable)?;
            items.push(row.into_public(photo));
        }
        Ok(MatchPage { items, next_cursor })
    }

    pub async fn reveal(&self, match_id: Uuid, user_id: Uuid) -> Result<bool, MatchError> {
        self.set_reveal(match_id, user_id, true).await
    }

    pub async fn withdraw_reveal(&self, match_id: Uuid, user_id: Uuid) -> Result<bool, MatchError> {
        self.set_reveal(match_id, user_id, false).await
    }

    async fn set_reveal(
        &self,
        match_id: Uuid,
        user_id: Uuid,
        granted: bool,
    ) -> Result<bool, MatchError> {
        let revealed = match self.store.record_reveal(match_id, user_id, granted).await? {
            MatchCommandResult::Available(revealed) => revealed,
            MatchCommandResult::Unavailable(reason) => return Err(command_error(reason)),
        };
        if let Some(participants) = self.store.participant_ids(match_id, user_id).await? {
            self.events
                .updated(
                    match_id,
                    participants,
                    MatchUpdate::PhotosRevealed(revealed),
                )
                .await;
        }
        Ok(revealed)
    }

    pub async fn continue_match(
        &self,
        match_id: Uuid,
        user_id: Uuid,
        credit_cost: i32,
    ) -> Result<bool, MatchError> {
        if !(0..=1).contains(&credit_cost) {
            return Err(MatchError::InvalidRequest);
        }
        let outcome = self
            .store
            .record_continuation(match_id, user_id, credit_cost)
            .await?;
        let confirmed = match outcome {
            ContinuationResult::Confirmed | ContinuationResult::AlreadyConfirmed => true,
            ContinuationResult::Pending | ContinuationResult::AlreadyRecorded => false,
            ContinuationResult::NotAvailableYet => {
                return Err(MatchError::ContinuationNotAvailableYet);
            }
            ContinuationResult::QuotaReached => return Err(MatchError::QuotaReached),
            ContinuationResult::PlanChanged => return Err(MatchError::PlanChanged),
            ContinuationResult::Expired => return Err(MatchError::Expired),
            ContinuationResult::NotFound => return Err(MatchError::NotFound),
            ContinuationResult::InvalidState => return Err(MatchError::InvalidState),
        };
        if outcome == ContinuationResult::Confirmed
            && let Some(participants) = self.store.participant_ids(match_id, user_id).await?
        {
            self.events
                .updated(match_id, participants, MatchUpdate::MatchConfirmed(true))
                .await;
        }
        Ok(confirmed)
    }

    pub async fn end(&self, match_id: Uuid, user_id: Uuid) -> Result<(), MatchError> {
        let outcome = self
            .store
            .record_end(match_id, user_id)
            .await?
            .ok_or(MatchError::NotFound)?;
        if outcome.changed {
            self.events
                .updated(match_id, outcome.participant_ids, MatchUpdate::Ended)
                .await;
        }
        Ok(())
    }

    pub async fn continuation_quota(&self, user_id: Uuid) -> Result<ContinuationQuota, MatchError> {
        self.store
            .quota_snapshot(user_id, self.clock.now())
            .await
            .map_err(Into::into)
    }

    pub async fn messages(
        &self,
        match_id: Uuid,
        user_id: Uuid,
        limit: u32,
        offset: u32,
        raw_cursor: Option<&str>,
    ) -> Result<MessagePage, MatchError> {
        if !(1..=100).contains(&limit)
            || (raw_cursor.is_some_and(|value| !value.is_empty()) && offset != 0)
        {
            return Err(MatchError::InvalidMessageRequest);
        }
        let cursor = decode_cursor(raw_cursor)?;
        let rows = match self
            .messages
            .messages_for_user(match_id, user_id, limit + 1, offset, cursor)
            .await?
        {
            MatchCommandResult::Available(rows) => rows,
            MatchCommandResult::Unavailable(reason) => {
                return Err(message_command_error(reason));
            }
        };
        let has_more = rows.len() > limit as usize;
        let next_cursor = if has_more {
            rows.get(limit as usize - 1)
                .map(|row| encode_cursor(&row.cursor_at, row.message.id))
                .transpose()?
        } else {
            None
        };
        let items = rows
            .into_iter()
            .take(limit as usize)
            .map(|row| PublicMessage::from(row.message))
            .collect();
        Ok(MessagePage { items, next_cursor })
    }

    pub async fn send_message(
        &self,
        match_id: Uuid,
        sender_id: Uuid,
        raw_content: &str,
        idempotency_input: Option<&str>,
    ) -> Result<PublicMessage, MatchError> {
        let content = javascript_trim(raw_content);
        if content.is_empty() || content.chars().count() > 2_000 {
            return Err(MatchError::InvalidMessageRequest);
        }
        let idempotency_key = normalize_idempotency_key(idempotency_input)?;
        let creation = match self
            .messages
            .create_message(
                Uuid::new_v4(),
                match_id,
                sender_id,
                content.to_owned(),
                idempotency_key,
            )
            .await?
        {
            MessageCreationResult::Available(creation) => creation,
            MessageCreationResult::Unavailable(reason) => {
                return Err(message_command_error(reason));
            }
            MessageCreationResult::IdempotencyConflict => {
                return Err(MatchError::IdempotencyConflict);
            }
        };
        let created = creation.created;
        let participants = creation.participant_ids;
        let message = PublicMessage::from(creation.message);
        if created {
            self.events.message_created(&message, participants).await;
        }
        Ok(message)
    }

    pub async fn mark_message_read(
        &self,
        match_id: Uuid,
        message_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), MatchError> {
        let read = match self
            .messages
            .mark_message_read(match_id, message_id, user_id)
            .await?
        {
            MatchCommandResult::Available(Some(read)) => read,
            MatchCommandResult::Available(None) => return Err(MatchError::MessageNotFound),
            MatchCommandResult::Unavailable(reason) => {
                return Err(message_command_error(reason));
            }
        };
        self.publish_read(match_id, user_id, read).await;
        Ok(())
    }

    pub async fn mark_messages_read_through(
        &self,
        match_id: Uuid,
        message_id: Uuid,
        user_id: Uuid,
    ) -> Result<i32, MatchError> {
        let read = match self
            .messages
            .mark_messages_read_through(match_id, message_id, user_id)
            .await?
        {
            MatchCommandResult::Available(Some(read)) => read,
            MatchCommandResult::Available(None) => return Err(MatchError::MessageNotFound),
            MatchCommandResult::Unavailable(reason) => {
                return Err(message_command_error(reason));
            }
        };
        let updated_count = read.updated_count;
        if updated_count > 0 {
            self.publish_read(match_id, user_id, read).await;
        }
        Ok(updated_count)
    }

    async fn publish_read(&self, match_id: Uuid, reader_id: Uuid, read: MessageRead) {
        self.events
            .messages_read(
                match_id,
                read.participant_ids,
                reader_id,
                read.read_through_message_id,
            )
            .await;
    }
}

fn normalize_idempotency_key(input: Option<&str>) -> Result<Uuid, MatchError> {
    let normalized = input
        .map(javascript_trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let parsed = Uuid::parse_str(&normalized).map_err(|_| MatchError::InvalidIdempotencyKey)?;
    if normalized.len() != 36
        || [8, 13, 18, 23]
            .iter()
            .any(|index| normalized.as_bytes()[*index] != b'-')
        || parsed.hyphenated().to_string() != normalized
        || parsed.get_version() != Some(Version::Random)
        || parsed.get_variant() != Variant::RFC4122
    {
        return Err(MatchError::InvalidIdempotencyKey);
    }
    Ok(parsed)
}

fn command_error(reason: MatchAvailabilityFailure) -> MatchError {
    match reason {
        MatchAvailabilityFailure::NotFound => MatchError::NotFound,
        MatchAvailabilityFailure::InvalidState => MatchError::InvalidState,
        MatchAvailabilityFailure::Expired => MatchError::Expired,
    }
}

fn message_command_error(reason: MatchAvailabilityFailure) -> MatchError {
    match reason {
        MatchAvailabilityFailure::NotFound => MatchError::NotFound,
        MatchAvailabilityFailure::InvalidState => MatchError::MessagingUnavailable,
        MatchAvailabilityFailure::Expired => MatchError::Expired,
    }
}

#[derive(Deserialize, Serialize)]
struct CursorWire {
    at: String,
    id: String,
}

fn decode_cursor(value: Option<&str>) -> Result<Option<PageCursor>, MatchError> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.encode_utf16().count() > 512 {
        return Err(MatchError::InvalidRequest);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| MatchError::InvalidCursor)?;
    let cursor: CursorWire =
        serde_json::from_slice(&bytes).map_err(|_| MatchError::InvalidCursor)?;
    let id = Uuid::parse_str(&cursor.id).map_err(|_| MatchError::InvalidCursor)?;
    if !valid_cursor_timestamp(&cursor.at)
        || !canonical_uuid(&cursor.id, id)
        || !(1..=8).contains(&id.get_version_num())
        || id.get_variant() != Variant::RFC4122
    {
        return Err(MatchError::InvalidCursor);
    }
    let at = DateTime::parse_from_rfc3339(&cursor.at)
        .map_err(|_| MatchError::InvalidCursor)?
        .with_timezone(&Utc);
    Ok(Some(PageCursor { at, id }))
}

fn encode_cursor(at: &str, id: Uuid) -> Result<String, MatchError> {
    serde_json::to_vec(&CursorWire {
        at: at.to_owned(),
        id: id.hyphenated().to_string(),
    })
    .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
    .map_err(|_| MatchError::InvalidCursor)
}

fn canonical_uuid(value: &str, parsed: Uuid) -> bool {
    value.len() == 36
        && [8, 13, 18, 23]
            .iter()
            .all(|index| value.as_bytes()[*index] == b'-')
        && parsed.hyphenated().to_string().eq_ignore_ascii_case(value)
}

fn valid_cursor_timestamp(value: &str) -> bool {
    let Some((date, fraction)) = value
        .strip_suffix('Z')
        .and_then(|value| value.rsplit_once('.'))
    else {
        return false;
    };
    (3..=6).contains(&fraction.len())
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
        && date.len() == 19
        && DateTime::parse_from_rfc3339(value).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trip_keeps_microseconds_and_generated_ids() {
        let id = Uuid::new_v4();
        let encoded =
            encode_cursor("2030-01-01T00:00:00.123456Z", id).unwrap_or_else(|_| unreachable!());
        let decoded = decode_cursor(Some(&encoded))
            .unwrap_or_else(|_| unreachable!())
            .unwrap_or_else(|| unreachable!());
        assert_eq!(decoded.id, id);
        assert_eq!(decoded.at.to_rfc3339(), "2030-01-01T00:00:00.123456+00:00");
    }

    #[test]
    fn rejects_malformed_cursors() {
        assert!(decode_cursor(Some("not-a-cursor")).is_err());
    }
}
