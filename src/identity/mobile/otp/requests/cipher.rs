use uuid::Uuid;

use crate::{
    config::SecretString,
    infra::crypto::{CryptoError, open_secret, seal_secret},
};

const ENCRYPTION_PURPOSE: &[u8] = b"histae:mobile-otp-request:v1";

#[derive(Clone)]
pub(super) struct RequestPhoneCipher(SecretString);

impl RequestPhoneCipher {
    pub(super) fn new(key: SecretString) -> Self {
        Self(key)
    }

    pub(super) fn encrypt(&self, id: Uuid, phone: &str) -> Result<Vec<u8>, CryptoError> {
        seal_secret(phone, &self.0, &purpose(id))
    }

    pub(super) fn decrypt(&self, id: Uuid, encrypted: &[u8]) -> Result<String, CryptoError> {
        open_secret(encrypted, &self.0, &purpose(id))
    }
}

fn purpose(id: Uuid) -> Vec<u8> {
    [ENCRYPTION_PURPOSE, id.as_bytes()].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_phones_are_bound_to_the_request_and_cannot_use_the_account_phone_context() {
        let key = SecretString::new("aa".repeat(32));
        let cipher = RequestPhoneCipher::new(key.clone());
        let id = Uuid::new_v4();
        let encrypted = cipher.encrypt(id, "+33612345678").expect("encrypted");
        assert_eq!(
            cipher.decrypt(id, &encrypted).expect("phone"),
            "+33612345678"
        );
        assert!(cipher.decrypt(Uuid::new_v4(), &encrypted).is_err());
        assert!(
            RequestPhoneCipher::new(SecretString::new("bb".repeat(32)))
                .decrypt(id, &encrypted)
                .is_err()
        );
        let account_phone =
            crate::infra::crypto::encrypt_phone("+33612345678", &key).expect("account phone");
        assert!(cipher.decrypt(id, &account_phone).is_err());
    }
}
