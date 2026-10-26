use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Serialize;
use uuid::Uuid;

use super::cursor::FeedCursorCodec;
use super::domain::{
    DiscoveryCursor, DiscoverySettings, DiscoveryStatus, FeedCandidate, SwipeDecision,
};
use super::store::{DiscoveryRepository, DiscoveryStoreError, SwipeStore};
use crate::config::LegalConfig;
use crate::identity::mobile::domain::wire_timestamp;
use crate::infra::postgres::DatabaseError;
use crate::infra::postgres_locks::AccountActivityError;
use crate::matches::domain::PublicMatch;
use crate::matches::service::{MatchError, MatchService};

const MAX_FEED_BATCHES: usize = 20;

pub type MatchCreatorFuture<'a> =
    Pin<Box<dyn Future<Output = Result<PublicMatch, MatchError>> + Send + 'a>>;

pub trait MatchCreator: Send + Sync {
    fn create_from_mutual_like(
        &self,
        first_user_id: Uuid,
        second_user_id: Uuid,
    ) -> MatchCreatorFuture<'_>;
}

impl MatchCreator for MatchService {
    fn create_from_mutual_like(
        &self,
        first_user_id: Uuid,
        second_user_id: Uuid,
    ) -> MatchCreatorFuture<'_> {
        Box::pin(MatchService::create_from_mutual_like(
            self,
            first_user_id,
            second_user_id,
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryError {
    InvalidFeedRequest,
    InvalidCursor,
    InvalidSwipeRequest,
    NotReady,
    CandidateNotFound,
    SwipeAlreadyRecorded,
    Unavailable,
    Database(DatabaseError),
    AccountActivity(AccountActivityError),
    Match(MatchError),
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = match self {
            Self::InvalidFeedRequest => "invalid_feed_request",
            Self::InvalidCursor => "invalid_cursor",
            Self::InvalidSwipeRequest => "invalid_swipe_request",
            Self::NotReady => "discovery_not_ready",
            Self::CandidateNotFound => "discovery_candidate_not_found",
            Self::SwipeAlreadyRecorded => "swipe_already_recorded",
            Self::Unavailable => "discovery_unavailable",
            Self::Database(error) => error.safe_code(),
            Self::AccountActivity(error) => error.safe_code(),
            Self::Match(_) => "match_error",
        };
        formatter.write_str(code)
    }
}

impl std::error::Error for DiscoveryError {}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FeedPage {
    pub profiles: Vec<FeedCandidate>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SwipeResponse {
    pub decision: SwipeDecision,
    pub matched: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#match: Option<PublicMatch>,
}

#[derive(Clone)]
pub struct DiscoveryService {
    repository: Arc<dyn DiscoveryRepository>,
    swipes: Arc<dyn SwipeStore>,
    matches: Arc<dyn MatchCreator>,
    legal: LegalConfig,
    cursors: FeedCursorCodec,
}

impl DiscoveryService {
    pub fn new(
        repository: Arc<dyn DiscoveryRepository>,
        swipes: Arc<dyn SwipeStore>,
        matches: Arc<dyn MatchCreator>,
        legal: LegalConfig,
        cursors: FeedCursorCodec,
    ) -> Self {
        Self {
            repository,
            swipes,
            matches,
            legal,
            cursors,
        }
    }

    pub async fn settings(&self, user_id: Uuid) -> Result<DiscoverySettings, DiscoveryError> {
        self.repository
            .paused(user_id)
            .await
            .map(|paused| DiscoverySettings { paused })
            .map_err(repository_error)
    }

    pub async fn set_paused(
        &self,
        user_id: Uuid,
        paused: bool,
    ) -> Result<DiscoverySettings, DiscoveryError> {
        self.repository
            .set_paused(user_id, paused)
            .await
            .map(|paused| DiscoverySettings { paused })
            .map_err(repository_error)
    }

    pub async fn status(&self, user_id: Uuid) -> Result<DiscoveryStatus, DiscoveryError> {
        let row = self
            .repository
            .status(
                user_id,
                &self.legal.sensitive_data_consent_version,
                &self.legal.location_consent_version,
            )
            .await
            .map_err(repository_error)?;
        let required_actions = row.required_actions();
        Ok(DiscoveryStatus {
            ready: required_actions.is_empty(),
            required_actions,
            presence_expires_at: row.presence_expires_at.map(wire_timestamp),
        })
    }

    pub async fn feed(
        &self,
        user_id: Uuid,
        limit: u32,
        raw_cursor: Option<&str>,
    ) -> Result<FeedPage, DiscoveryError> {
        if !(1..=100).contains(&limit) {
            return Err(DiscoveryError::InvalidFeedRequest);
        }
        let mut cursor = match self.cursors.decode(user_id, raw_cursor) {
            Ok(cursor) => cursor,
            Err(error) => {
                // Preserve prerequisite-error precedence for malformed cursors.
                self.require_ready(user_id).await?;
                return Err(error);
            }
        };
        let mut visible = Vec::new();
        let maximum_batch = 50_u32.max((limit + 1).saturating_mul(4));
        let mut remaining_scan = maximum_batch as usize * MAX_FEED_BATCHES;
        let mut batch_size = limit + 1;
        let mut database_exhausted = false;

        while remaining_scan > 0 {
            if visible.len() > limit as usize {
                break;
            }
            let requested = batch_size.min(remaining_scan as u32);
            let batch = match self
                .repository
                .feed_batch(
                    user_id,
                    &self.legal.sensitive_data_consent_version,
                    &self.legal.location_consent_version,
                    requested,
                    cursor,
                )
                .await
            {
                Ok(batch) => batch,
                Err(_) => {
                    // Resolve the former readiness/candidate error distinction
                    // on failures, keeping the normal path to one selection.
                    self.require_ready(user_id).await?;
                    return Err(DiscoveryError::Unavailable);
                }
            };
            if !batch.ready {
                return Err(DiscoveryError::NotReady);
            }
            let candidates = batch.candidates;
            if candidates.is_empty() {
                database_exhausted = true;
                break;
            }
            let ids = candidates
                .iter()
                .map(|candidate| candidate.user_id)
                .collect();
            // PostgreSQL also excludes swipes before pagination. Recheck here to
            // catch a decision committed after that query and support other stores.
            let swiped = self
                .swipes
                .swiped_target_ids(user_id, ids)
                .await
                .map_err(|_| DiscoveryError::Unavailable)?;
            let count = candidates.len();
            remaining_scan = remaining_scan.saturating_sub(count);
            if let Some(last) = candidates.last() {
                cursor = Some(DiscoveryCursor {
                    distance_km: last.distance_km,
                    id: last.user_id,
                });
            }
            visible.extend(
                candidates
                    .into_iter()
                    .filter(|candidate| !swiped.contains(&candidate.user_id)),
            );
            if count < requested as usize {
                database_exhausted = true;
                break;
            }
            batch_size = batch_size.saturating_mul(2).min(maximum_batch);
        }

        let has_more = visible.len() > limit as usize || !database_exhausted;
        let page: Vec<_> = visible.into_iter().take(limit as usize).collect();
        let next_cursor = if has_more {
            if let Some(last) = page.last() {
                Some(self.cursors.encode(
                    user_id,
                    DiscoveryCursor {
                        distance_km: last.distance_km,
                        id: last.user_id,
                    },
                )?)
            } else {
                cursor
                    .map(|value| self.cursors.encode(user_id, value))
                    .transpose()?
            }
        } else {
            None
        };
        Ok(FeedPage {
            profiles: page.into_iter().map(FeedCandidate::from).collect(),
            next_cursor,
        })
    }

    pub async fn swipe(
        &self,
        actor_id: Uuid,
        target_id: Uuid,
        decision: SwipeDecision,
    ) -> Result<SwipeResponse, DiscoveryError> {
        if actor_id.is_nil() || target_id.is_nil() || actor_id == target_id {
            return Err(DiscoveryError::InvalidSwipeRequest);
        }
        self.require_ready(actor_id).await?;
        let available = self
            .repository
            .candidate_batch(
                actor_id,
                &self.legal.sensitive_data_consent_version,
                &self.legal.location_consent_version,
                1,
                None,
                Some(target_id),
            )
            .await
            .map_err(repository_error)?;
        if available.len() != 1 {
            return Err(DiscoveryError::CandidateNotFound);
        }

        let recorded = self
            .swipes
            .record(actor_id, target_id, decision)
            .await
            .map_err(swipe_error)?;
        if !recorded.created && recorded.decision != decision {
            return Err(DiscoveryError::SwipeAlreadyRecorded);
        }
        if decision == SwipeDecision::Pass {
            return Ok(SwipeResponse {
                decision,
                matched: false,
                r#match: None,
            });
        }
        let reciprocal = self
            .swipes
            .find(target_id, actor_id)
            .await
            .map_err(swipe_error)?;
        if reciprocal.is_none_or(|swipe| swipe.decision != SwipeDecision::Like) {
            return Ok(SwipeResponse {
                decision,
                matched: false,
                r#match: None,
            });
        }
        let created = self
            .matches
            .create_from_mutual_like(actor_id, target_id)
            .await
            .map_err(DiscoveryError::Match)?;
        Ok(SwipeResponse {
            decision,
            matched: true,
            r#match: Some(created),
        })
    }

    async fn require_ready(&self, user_id: Uuid) -> Result<(), DiscoveryError> {
        let ready = self
            .repository
            .is_ready(
                user_id,
                &self.legal.sensitive_data_consent_version,
                &self.legal.location_consent_version,
            )
            .await
            .map_err(repository_error)?;
        if ready {
            Ok(())
        } else {
            Err(DiscoveryError::NotReady)
        }
    }
}

fn repository_error(error: DiscoveryStoreError) -> DiscoveryError {
    match error {
        DiscoveryStoreError::NotReady => DiscoveryError::NotReady,
        DiscoveryStoreError::CandidateNotFound => DiscoveryError::CandidateNotFound,
        DiscoveryStoreError::Database(error) => DiscoveryError::Database(error),
        DiscoveryStoreError::AccountActivity(error) => DiscoveryError::AccountActivity(error),
        DiscoveryStoreError::InvalidStoredData => {
            DiscoveryError::Database(DatabaseError::QueryFailed)
        }
    }
}

fn swipe_error(error: DiscoveryStoreError) -> DiscoveryError {
    match error {
        DiscoveryStoreError::NotReady => DiscoveryError::NotReady,
        DiscoveryStoreError::CandidateNotFound => DiscoveryError::CandidateNotFound,
        DiscoveryStoreError::AccountActivity(error) => DiscoveryError::AccountActivity(error),
        DiscoveryStoreError::Database(_) | DiscoveryStoreError::InvalidStoredData => {
            DiscoveryError::Unavailable
        }
    }
}
