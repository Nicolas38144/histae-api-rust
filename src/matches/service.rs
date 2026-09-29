use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant};

use super::domain::{
    ContinuationQuota, ContinuationResult, MATCH_WINDOW_HOURS, MatchAvailabilityFailure,
    MatchCommandResult, MatchRecord, MatchStatus, PageCursor, PublicMatch, PublicUserMatch,
    start_of_utc_week,
};
use super::pg::{MatchStore, MatchStoreError};
use crate::infra::postgres::DatabaseError;
use crate::profiles::service::ProfilePhotoUrlProvider;
use crate::shared::clock::Clock;

pub type MatchEventFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchUpdate {
    PhotosRevealed(bool),
    MatchConfirmed(bool),
}

pub trait MatchEventPublisher: Send + Sync {
    fn created<'a>(&'a self, item: &'a PublicMatch) -> MatchEventFuture<'a>;

    fn updated<'a>(
        &'a self,
        match_id: Uuid,
        participants: [Uuid; 2],
        update: MatchUpdate,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchError {
    InvalidRequest,
    InvalidCursor,
    NotFound,
    Blocked,
    CandidateNotFound,
    InvalidState,
    ContinuationNotAvailableYet,
    Expired,
    QuotaReached,
    PhotoStorageUnavailable,
    Database(DatabaseError),
}

impl From<MatchStoreError> for MatchError {
    fn from(error: MatchStoreError) -> Self {
        match error {
            MatchStoreError::Blocked => Self::Blocked,
            MatchStoreError::ParticipantUnavailable => Self::CandidateNotFound,
            MatchStoreError::Database(error) => Self::Database(error),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MatchPage {
    pub items: Vec<PublicUserMatch>,
    pub next_cursor: Option<String>,
}

#[derive(Clone)]
pub struct MatchService {
    store: Arc<dyn MatchStore>,
    photo_urls: Arc<dyn ProfilePhotoUrlProvider>,
    events: Arc<dyn MatchEventPublisher>,
    clock: Arc<dyn Clock>,
}

impl MatchService {
    pub fn new(
        store: Arc<dyn MatchStore>,
        photo_urls: Arc<dyn ProfilePhotoUrlProvider>,
        events: Arc<dyn MatchEventPublisher>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
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
        for row in rows.into_iter().take(limit as usize) {
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
        let revealed = match self.store.record_reveal(match_id, user_id).await? {
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

    pub async fn continue_match(&self, match_id: Uuid, user_id: Uuid) -> Result<bool, MatchError> {
        let confirmed = match self.store.record_continuation(match_id, user_id).await? {
            ContinuationResult::Confirmed => true,
            ContinuationResult::Pending | ContinuationResult::AlreadyRecorded => false,
            ContinuationResult::NotAvailableYet => {
                return Err(MatchError::ContinuationNotAvailableYet);
            }
            ContinuationResult::QuotaReached => return Err(MatchError::QuotaReached),
            ContinuationResult::Expired => return Err(MatchError::Expired),
            ContinuationResult::NotFound => return Err(MatchError::NotFound),
            ContinuationResult::InvalidState => return Err(MatchError::InvalidState),
        };
        if let Some(participants) = self.store.participant_ids(match_id, user_id).await? {
            self.events
                .updated(
                    match_id,
                    participants,
                    MatchUpdate::MatchConfirmed(confirmed),
                )
                .await;
        }
        Ok(confirmed)
    }

    pub async fn continuation_quota(&self, user_id: Uuid) -> Result<ContinuationQuota, MatchError> {
        let plan = self.store.effective_plan(user_id, self.clock.now()).await?;
        let Some(limit) = plan.weekly_limit else {
            return Ok(ContinuationQuota {
                plan: plan.plan,
                used: 0,
                weekly_limit: None,
                remaining: None,
            });
        };
        let limit = i32::from(limit);
        let used = self
            .store
            .continuation_usage(user_id, start_of_utc_week(self.clock.now()))
            .await?;
        Ok(ContinuationQuota {
            plan: plan.plan,
            used,
            weekly_limit: Some(limit),
            remaining: Some((limit - used).max(0)),
        })
    }
}

fn command_error(reason: MatchAvailabilityFailure) -> MatchError {
    match reason {
        MatchAvailabilityFailure::NotFound => MatchError::NotFound,
        MatchAvailabilityFailure::InvalidState => MatchError::InvalidState,
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
