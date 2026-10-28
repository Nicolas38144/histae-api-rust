//! Reusable, opt-in credentials for the dedicated mobile store review account.
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

use super::otp::{OtpError, OtpService};
use crate::{
    config::{PlayReviewConfig, SecretString},
    infra::crypto::{encrypt_phone, hmac_sha256},
};

#[derive(Clone, Debug)]
pub struct NewReviewAccount {
    pub user_id: Uuid,
    pub phone_hash: String,
    pub encrypted_phone: Vec<u8>,
}

#[derive(Clone)]
pub(super) struct PlayReviewAccess {
    config: PlayReviewConfig,
    hash_key: SecretString,
    encryption_key: SecretString,
}

impl PlayReviewAccess {
    pub(super) fn new(
        config: PlayReviewConfig,
        hash_key: SecretString,
        encryption_key: SecretString,
    ) -> Self {
        Self {
            config,
            hash_key,
            encryption_key,
        }
    }

    pub(super) fn matches_phone(&self, input: &str) -> bool {
        OtpService::canonical_phone(input).is_ok_and(|phone| {
            bool::from(
                phone
                    .as_bytes()
                    .ct_eq(self.config.phone_number.expose_secret().as_bytes()),
            )
        })
    }

    pub(super) fn authorize(&self, phone: &str, code: &str) -> Result<NewReviewAccount, OtpError> {
        if !self.matches_phone(phone) {
            return Err(OtpError::InvalidOtp);
        }
        if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(OtpError::InvalidOtpRequest);
        }
        if !bool::from(
            code.as_bytes()
                .ct_eq(self.config.otp.expose_secret().as_bytes()),
        ) {
            return Err(OtpError::InvalidOtp);
        }
        let phone = self.config.phone_number.expose_secret();
        Ok(NewReviewAccount {
            user_id: Uuid::new_v4(),
            phone_hash: hmac_sha256(phone, &self.hash_key).map_err(|_| OtpError::Internal)?,
            encrypted_phone: encrypt_phone(phone, &self.encryption_key)
                .map_err(|_| OtpError::Internal)?,
        })
    }
}
