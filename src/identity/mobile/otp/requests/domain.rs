use std::{fmt, time::Duration};

use uuid::Uuid;

use crate::{infra::postgres::DatabaseError, localization::Language};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestError {
    Conflict,
    Cooldown(u64),
    Database(DatabaseError),
}

impl From<DatabaseError> for RequestError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

impl fmt::Display for RequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Conflict => "idempotency_key_conflict",
            Self::Cooldown(_) => "otp_resend_too_soon",
            Self::Database(error) => error.safe_code(),
        })
    }
}

impl std::error::Error for RequestError {}

#[derive(Clone)]
pub struct NewOtpRequest {
    pub id: Uuid,
    pub phone_hash: String,
    pub encrypted_phone: Vec<u8>,
    pub registration: bool,
    pub language: Language,
    pub ttl: Duration,
    pub cooldown: Duration,
}

pub struct PendingOtpRequest {
    pub id: Uuid,
    pub phone_hash: String,
    pub encrypted_phone: Vec<u8>,
    pub registration: bool,
    pub language: Language,
}
