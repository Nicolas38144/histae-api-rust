use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use uuid::Uuid;

use super::domain::{
    DiscoveryBatch, DiscoveryCandidateRow, DiscoveryCursor, DiscoveryStatusRow, RecordedSwipe,
    SwipeDecision, SwipeRecord,
};
use crate::infra::postgres::DatabaseError;
use crate::infra::postgres_locks::AccountActivityError;

pub type DiscoveryStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DiscoveryStoreError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryStoreError {
    Database(DatabaseError),
    AccountActivity(AccountActivityError),
    InvalidStoredData,
    NotReady,
    CandidateNotFound,
}

impl fmt::Display for DiscoveryStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = match self {
            Self::Database(error) => error.safe_code(),
            Self::AccountActivity(error) => error.safe_code(),
            Self::InvalidStoredData => "invalid_discovery_data",
            Self::NotReady => "discovery_not_ready",
            Self::CandidateNotFound => "discovery_candidate_not_found",
        };
        formatter.write_str(code)
    }
}

impl std::error::Error for DiscoveryStoreError {}

impl From<DatabaseError> for DiscoveryStoreError {
    fn from(value: DatabaseError) -> Self {
        Self::Database(value)
    }
}

impl From<AccountActivityError> for DiscoveryStoreError {
    fn from(value: AccountActivityError) -> Self {
        Self::AccountActivity(value)
    }
}

pub trait DiscoveryRepository: Send + Sync {
    fn paused(&self, user_id: Uuid) -> DiscoveryStoreFuture<'_, bool>;
    fn set_paused(&self, user_id: Uuid, paused: bool) -> DiscoveryStoreFuture<'_, bool>;

    fn feed_batch(
        &self,
        user_id: Uuid,
        sensitive_version: &str,
        location_version: &str,
        limit: u32,
        cursor: Option<DiscoveryCursor>,
    ) -> DiscoveryStoreFuture<'_, DiscoveryBatch> {
        let sensitive_version = sensitive_version.to_owned();
        let location_version = location_version.to_owned();
        Box::pin(async move {
            let ready = self
                .is_ready(user_id, &sensitive_version, &location_version)
                .await?;
            let candidates = if ready {
                self.candidate_batch(
                    user_id,
                    &sensitive_version,
                    &location_version,
                    limit,
                    cursor,
                    None,
                )
                .await?
            } else {
                Vec::new()
            };
            Ok(DiscoveryBatch { ready, candidates })
        })
    }

    fn status(
        &self,
        user_id: Uuid,
        sensitive_version: &str,
        location_version: &str,
    ) -> DiscoveryStoreFuture<'_, DiscoveryStatusRow>;

    fn is_ready(
        &self,
        user_id: Uuid,
        sensitive_version: &str,
        location_version: &str,
    ) -> DiscoveryStoreFuture<'_, bool>;

    fn candidate_batch(
        &self,
        user_id: Uuid,
        sensitive_version: &str,
        location_version: &str,
        limit: u32,
        cursor: Option<DiscoveryCursor>,
        target_id: Option<Uuid>,
    ) -> DiscoveryStoreFuture<'_, Vec<DiscoveryCandidateRow>>;
}

pub trait SwipeStore: Send + Sync {
    fn record(
        &self,
        actor_id: Uuid,
        target_id: Uuid,
        decision: SwipeDecision,
    ) -> DiscoveryStoreFuture<'_, RecordedSwipe>;

    fn find(
        &self,
        actor_id: Uuid,
        target_id: Uuid,
    ) -> DiscoveryStoreFuture<'_, Option<SwipeRecord>>;

    fn swiped_target_ids(
        &self,
        actor_id: Uuid,
        target_ids: Vec<Uuid>,
    ) -> DiscoveryStoreFuture<'_, HashSet<Uuid>>;
}
