use std::{sync::Arc, time::Duration};

use uuid::Uuid;

use super::{NewOtpRequest, OtpRequestStore, RequestError, cipher::RequestPhoneCipher};
use crate::{config::SecretString, infra::postgres::DatabaseError, localization::Language};

#[derive(Clone)]
pub struct OtpRequestService {
    store: Arc<dyn OtpRequestStore>,
    cipher: RequestPhoneCipher,
    ttl: Duration,
    cooldown: Duration,
}

impl OtpRequestService {
    pub fn new(
        store: Arc<dyn OtpRequestStore>,
        encryption_key: SecretString,
        ttl: Duration,
        cooldown: Duration,
    ) -> Self {
        Self {
            store,
            cipher: RequestPhoneCipher::new(encryption_key),
            ttl,
            cooldown,
        }
    }

    pub async fn enqueue(
        &self,
        phone: &str,
        phone_hash: String,
        id: Uuid,
        registration: bool,
        language: Language,
    ) -> Result<u64, RequestError> {
        let encrypted_phone = self
            .cipher
            .encrypt(id, phone)
            .map_err(|_| RequestError::Database(DatabaseError::QueryFailed))?;
        self.store
            .enqueue(NewOtpRequest {
                id,
                phone_hash,
                encrypted_phone,
                registration,
                language,
                ttl: self.ttl,
                cooldown: self.cooldown,
            })
            .await
    }
}
