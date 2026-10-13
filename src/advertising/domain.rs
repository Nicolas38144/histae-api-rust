use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const REWARD_CREDITS: i32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RewardPlatform {
    Android,
    Ios,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RewardStatus {
    Pending,
    Credited,
    Cancelled,
    Expired,
}

#[derive(Debug, Serialize)]
pub struct RewardIntent {
    pub id: Uuid,
    pub ad_unit: String,
    pub custom_data: String,
    pub expires_at: DateTime<Utc>,
    pub status: RewardStatus,
    pub reward_credits: i32,
}
