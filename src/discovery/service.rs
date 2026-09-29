use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant};

use super::domain::{
    DiscoveryCursor, DiscoveryRequiredAction, DiscoveryStatus, FeedCandidate, SwipeDecision,
};
use super::pg::{DiscoveryRepository, DiscoveryStoreError, SwipeStore};
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
}

impl DiscoveryService {
    pub fn new(
        repository: Arc<dyn DiscoveryRepository>,
        swipes: Arc<dyn SwipeStore>,
        matches: Arc<dyn MatchCreator>,
        legal: LegalConfig,
    ) -> Self {
        Self {
            repository,
            swipes,
            matches,
            legal,
        }
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
        let mut required_actions = Vec::new();
        if !row.has_profile {
            required_actions.push(DiscoveryRequiredAction::Profile);
        } else if !row.has_sex {
            required_actions.push(DiscoveryRequiredAction::Sex);
        }
        if !row.has_preferences {
            required_actions.push(DiscoveryRequiredAction::Preferences);
        }
        if !row.has_sensitive_consent {
            required_actions.push(DiscoveryRequiredAction::SensitiveDataConsent);
        }
        if !row.has_location_consent {
            required_actions.push(DiscoveryRequiredAction::LocationConsent);
        }
        if !row.has_fresh_presence {
            required_actions.push(DiscoveryRequiredAction::FreshPresence);
        }
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
        self.require_ready(user_id).await?;
        let mut cursor = decode_cursor(raw_cursor)?;
        let mut visible = Vec::new();
        let batch_size = 50_u32.max((limit + 1).saturating_mul(4));
        let mut database_exhausted = false;

        for _ in 0..MAX_FEED_BATCHES {
            if visible.len() > limit as usize {
                break;
            }
            let candidates = self
                .repository
                .candidate_batch(
                    user_id,
                    &self.legal.sensitive_data_consent_version,
                    &self.legal.location_consent_version,
                    batch_size,
                    cursor,
                    None,
                )
                .await
                .map_err(|_| DiscoveryError::Unavailable)?;
            if candidates.is_empty() {
                database_exhausted = true;
                break;
            }
            let ids = candidates
                .iter()
                .map(|candidate| candidate.user_id)
                .collect();
            let swiped = self
                .swipes
                .swiped_target_ids(user_id, ids)
                .await
                .map_err(|_| DiscoveryError::Unavailable)?;
            visible.extend(
                candidates
                    .iter()
                    .filter(|candidate| !swiped.contains(&candidate.user_id))
                    .cloned(),
            );
            if let Some(last) = candidates.last() {
                cursor = Some(DiscoveryCursor {
                    distance_km: last.distance_km,
                    id: last.user_id,
                });
            }
            if candidates.len() < batch_size as usize {
                database_exhausted = true;
                break;
            }
        }

        let has_more = visible.len() > limit as usize || !database_exhausted;
        let page: Vec<_> = visible.into_iter().take(limit as usize).collect();
        let next_cursor = if has_more {
            if let Some(last) = page.last() {
                Some(encode_cursor(last.distance_km, last.user_id)?)
            } else {
                cursor
                    .map(|value| encode_cursor(value.distance_km, value.id))
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

#[derive(Deserialize, Serialize)]
struct CursorWire {
    distance_km: f64,
    id: String,
}

fn decode_cursor(value: Option<&str>) -> Result<Option<DiscoveryCursor>, DiscoveryError> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| DiscoveryError::InvalidCursor)?;
    let decoded: CursorWire =
        serde_json::from_slice(&bytes).map_err(|_| DiscoveryError::InvalidCursor)?;
    let id = Uuid::parse_str(&decoded.id).map_err(|_| DiscoveryError::InvalidCursor)?;
    if !decoded.distance_km.is_finite()
        || decoded.distance_km < 0.0
        || !canonical_uuid(&decoded.id, id)
    {
        return Err(DiscoveryError::InvalidCursor);
    }
    Ok(Some(DiscoveryCursor {
        distance_km: decoded.distance_km,
        id,
    }))
}

fn encode_cursor(distance_km: f64, id: Uuid) -> Result<String, DiscoveryError> {
    serde_json::to_vec(&CursorWire {
        distance_km,
        id: id.hyphenated().to_string(),
    })
    .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
    .map_err(|_| DiscoveryError::InvalidCursor)
}

fn canonical_uuid(value: &str, parsed: Uuid) -> bool {
    value.len() == 36
        && [8, 13, 18, 23]
            .iter()
            .all(|index| value.as_bytes()[*index] == b'-')
        && parsed.hyphenated().to_string().eq_ignore_ascii_case(value)
        && (1..=8).contains(&parsed.get_version_num())
        && parsed.get_variant() == Variant::RFC4122
}

fn repository_error(error: DiscoveryStoreError) -> DiscoveryError {
    match error {
        DiscoveryStoreError::Database(error) => DiscoveryError::Database(error),
        DiscoveryStoreError::AccountActivity(error) => DiscoveryError::AccountActivity(error),
        DiscoveryStoreError::InvalidStoredData => {
            DiscoveryError::Database(DatabaseError::QueryFailed)
        }
    }
}

fn swipe_error(error: DiscoveryStoreError) -> DiscoveryError {
    match error {
        DiscoveryStoreError::AccountActivity(error) => DiscoveryError::AccountActivity(error),
        DiscoveryStoreError::Database(_) | DiscoveryStoreError::InvalidStoredData => {
            DiscoveryError::Unavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};

    use super::*;

    #[test]
    fn cursor_keeps_exact_distance_and_generated_uuid() {
        let id = Uuid::new_v4();
        let encoded = encode_cursor(1.23456, id).unwrap_or_else(|_| unreachable!());
        let decoded = decode_cursor(Some(&encoded))
            .unwrap_or_else(|_| unreachable!())
            .unwrap_or_else(|| unreachable!());
        assert_eq!(
            decoded,
            DiscoveryCursor {
                distance_km: 1.23456,
                id
            }
        );
    }

    #[test]
    fn rejects_negative_and_malformed_cursors() {
        assert_eq!(
            decode_cursor(Some("not-a-cursor")),
            Err(DiscoveryError::InvalidCursor)
        );
        let id = Uuid::new_v4();
        let bytes = serde_json::to_vec(&CursorWire {
            distance_km: -1.0,
            id: id.to_string(),
        })
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            decode_cursor(Some(&URL_SAFE_NO_PAD.encode(bytes))),
            Err(DiscoveryError::InvalidCursor)
        );
    }

    #[test]
    fn preserves_required_action_order_and_profile_sex_exclusivity() {
        let row = super::super::domain::DiscoveryStatusRow {
            has_profile: false,
            has_sex: false,
            has_preferences: false,
            has_sensitive_consent: false,
            has_location_consent: false,
            has_fresh_presence: false,
            presence_expires_at: Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).single(),
        };
        let mut actions = Vec::new();
        if !row.has_profile {
            actions.push(DiscoveryRequiredAction::Profile);
        } else if !row.has_sex {
            actions.push(DiscoveryRequiredAction::Sex);
        }
        if !row.has_preferences {
            actions.push(DiscoveryRequiredAction::Preferences);
        }
        if !row.has_sensitive_consent {
            actions.push(DiscoveryRequiredAction::SensitiveDataConsent);
        }
        if !row.has_location_consent {
            actions.push(DiscoveryRequiredAction::LocationConsent);
        }
        if !row.has_fresh_presence {
            actions.push(DiscoveryRequiredAction::FreshPresence);
        }
        assert_eq!(
            actions,
            vec![
                DiscoveryRequiredAction::Profile,
                DiscoveryRequiredAction::Preferences,
                DiscoveryRequiredAction::SensitiveDataConsent,
                DiscoveryRequiredAction::LocationConsent,
                DiscoveryRequiredAction::FreshPresence,
            ]
        );
    }
}
