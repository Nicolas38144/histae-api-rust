use std::{future::Future, pin::Pin};
use uuid::Uuid;

use super::{
    domain::{RewardIntent, RewardPlatform},
    error::RewardError,
    ssv::Callback,
};

pub type RewardFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, RewardError>> + Send + 'a>>;

/// Reward operations keep eligibility checks and wallet mutations atomic.
pub trait RewardStore: Send + Sync {
    fn enabled(&self) -> bool;
    fn create(&self, user_id: Uuid, platform: RewardPlatform) -> RewardFuture<'_, RewardIntent>;
    fn status(&self, user_id: Uuid, id: Uuid) -> RewardFuture<'_, RewardIntent>;
    fn cancel(&self, user_id: Uuid, id: Uuid) -> RewardFuture<'_, ()>;
    /// The transport must authenticate the callback before passing it to the store.
    fn complete(&self, callback: Callback) -> RewardFuture<'_, ()>;
}
