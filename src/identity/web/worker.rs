use super::WebAccountRepository;
use crate::{
    config::SecretString,
    identity::mobile::otp::{OtpError, OtpService, delivery_outcome},
    infra::crypto::decrypt_phone,
    localization::Language,
    outbox::{
        types::{DispatchFailure, DispatchOutcome, OutboxEvent},
        worker::{DispatchFuture, OutboxHandler},
    },
};
use uuid::Uuid;

pub struct WebOtpHandler {
    repository: WebAccountRepository,
    otp: OtpService,
    encryption_key: SecretString,
}

impl WebOtpHandler {
    pub fn new(
        repository: WebAccountRepository,
        otp: OtpService,
        encryption_key: SecretString,
    ) -> Self {
        Self {
            repository,
            otp,
            encryption_key,
        }
    }

    async fn process(&self, challenge: Uuid) -> Result<DispatchOutcome, DispatchFailure> {
        let Some((encrypted, expected_hash, language)) = self
            .repository
            .pending_delivery(challenge)
            .await
            .map_err(|_| DispatchFailure::transient("web_otp_dispatch_unavailable"))?
        else {
            return Ok(DispatchOutcome::Completed);
        };
        let invalid = || DispatchFailure::permanent("web_otp_request_invalid");
        let language = Language::parse(&language).ok_or_else(invalid)?;
        let phone = decrypt_phone(&encrypted, &self.encryption_key).map_err(|_| invalid())?;
        if self
            .otp
            .rate_limit_key(&phone, OtpError::InvalidPhone)
            .map_err(|_| invalid())?
            != expected_hash
        {
            return Err(invalid());
        }
        delivery_outcome(
            self.otp
                .for_web_challenge(challenge)
                .send_in(&phone, Some(&challenge.to_string()), language)
                .await,
        )?;
        Ok(DispatchOutcome::Completed)
    }
}

impl OutboxHandler for WebOtpHandler {
    fn handle<'a>(&'a self, event: &'a OutboxEvent, _worker_id: Uuid) -> DispatchFuture<'a> {
        Box::pin(self.process(event.aggregate_id))
    }
}
