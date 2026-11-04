//! A retry receipt requires both the original refresh secret and its attempt ID.
//! Only authenticated ciphertext and a request hash are persisted. No server key
//! or plaintext refresh token is stored in the receipt.
use uuid::Uuid;

use crate::config::SecretString;
use crate::infra::crypto::{CryptoError, open_secret, seal_secret, sha256_hex};

use super::domain::{MobileSessionIdentity, RotationOutcome};

pub struct RefreshRecovery {
    request_hash: String,
    encrypted_token: Vec<u8>,
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

    pub fn request_hash(&self) -> &str {
        &self.request_hash
    }

    pub fn encrypted_token(&self) -> &[u8] {
        &self.encrypted_token
    }

    pub fn open(&self, encrypted: &[u8]) -> Result<String, CryptoError> {
        open_secret(encrypted, &self.key, &self.purpose)
    }
}

/// A recovered credential always carries its identity; invalid outcomes cannot
/// accidentally fall back to the unused replacement generated for this request.
pub enum RefreshRotation {
    Outcome(RotationOutcome),
    Recovered {
        session: MobileSessionIdentity,
        refresh_token: SecretString,
    },
}

impl RefreshRotation {
    pub fn outcome(outcome: RotationOutcome) -> Self {
        Self::Outcome(outcome)
    }

    pub fn recovered(session: MobileSessionIdentity, token: String) -> Self {
        Self::Recovered {
            session,
            refresh_token: SecretString::new(token),
        }
    }

    pub fn into_outcome(self) -> RotationOutcome {
        match self {
            Self::Outcome(outcome) => outcome,
            Self::Recovered { session, .. } => RotationOutcome::Rotated(session),
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
        let retry =
            RefreshRecovery::new("synthetic-parent", attempt, "ignored-new-candidate").unwrap();
        assert_eq!(
            retry.open(&receipt.encrypted_token).unwrap(),
            "synthetic-child"
        );
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
