use crate::{infra::postgres::DatabaseError, matches::store::MatchStoreError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RewardError {
    Disabled,
    NotEligible,
    StockFull,
    NotFound,
    InvalidProof,
    Database(DatabaseError),
}

impl From<DatabaseError> for RewardError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

impl From<MatchStoreError> for RewardError {
    fn from(error: MatchStoreError) -> Self {
        match error {
            MatchStoreError::Database(error) => Self::Database(error),
            _ => Self::NotEligible,
        }
    }
}

impl std::fmt::Display for RewardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Disabled => "advertising_disabled",
            Self::NotEligible => "advertising_not_eligible",
            Self::StockFull => "advertising_stock_full",
            Self::NotFound => "reward_not_found",
            Self::InvalidProof => "invalid_admob_reward",
            Self::Database(error) => error.safe_code(),
        })
    }
}

impl std::error::Error for RewardError {}
