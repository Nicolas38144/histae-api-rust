//! A retry receipt requires both the original refresh secret and its attempt ID.
//! Only authenticated ciphertext and a request hash are persisted. No server key
//! or plaintext refresh token is stored in the receipt.
use uuid::Uuid;

use crate::config::SecretString;
use crate::infra::crypto::{CryptoError, open_secret, seal_secret, sha256_hex};

use super::domain::{MobileSessionIdentity, RotationOutcome};

pub struct RefreshRecovery {
    pub request_hash: String,
    pub encrypted_token: Vec<u8>,
    key: SecretString,
    purpose: Vec<u8>,
}

impl RefreshRecovery {
    pub fn new(parent: &str, attempt: Uuid, replacement: &str) -> Result<Self, CryptoError> {
        let material = [
            b"histae/refresh-recovery/key/v1\0".as_slice(),
            parent.as_bytes(),
            attempt.as_bytes(),
        ]
        .concat();
        let key = SecretString::new(sha256_hex(&material));
        let purpose = [
            b"histae/refresh-recovery/result/v1\0".as_slice(),
            attempt.as_bytes(),
        ]
        .concat();
        let encrypted_token = seal_secret(replacement, &key, &purpose)?;
        Ok(Self {
            request_hash: sha256_hex(attempt.as_bytes()),
            encrypted_token,
            key,
            purpose,
        })
    }

    pub fn open(&self, encrypted: &[u8]) -> Result<String, CryptoError> {
        open_secret(encrypted, &self.key, &self.purpose)
    }
}

pub struct RefreshRotation {
    pub outcome: RotationOutcome,
    pub recovered_token: Option<String>,
}

impl RefreshRotation {
    pub fn outcome(outcome: RotationOutcome) -> Self {
        Self {
            outcome,
            recovered_token: None,
        }
    }

    pub fn recovered(session: MobileSessionIdentity, token: String) -> Self {
        Self {
            outcome: RotationOutcome::Rotated(session),
            recovered_token: Some(token),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_requires_parent_and_attempt_and_authenticates_ciphertext() {
        let attempt = Uuid::new_v4();
        let receipt = RefreshRecovery::new("synthetic-parent", attempt, "synthetic-child").unwrap();
        let retry = RefreshRecovery::new("synthetic-parent", attempt, "ignored-new-candidate").unwrap();
        assert_eq!(retry.open(&receipt.encrypted_token).unwrap(), "synthetic-child");
        assert!(
            RefreshRecovery::new("another-parent", attempt, "ignored")
                .unwrap()
                .open(&receipt.encrypted_token)
                .is_err()
        );
        assert!(
            RefreshRecovery::new("synthetic-parent", Uuid::new_v4(), "ignored")
                .unwrap()
                .open(&receipt.encrypted_token)
                .is_err()
        );
        let mut corrupted = receipt.encrypted_token.clone();
        corrupted[12] ^= 1;
        assert!(retry.open(&corrupted).is_err());
    }
}
