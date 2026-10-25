use std::sync::Arc;

use uuid::Uuid;

use super::{OtpRequestStore, PendingOtpRequest, cipher::RequestPhoneCipher};
use crate::{
    config::SecretString,
    identity::mobile::{
        account::MobileAccountStore,
        otp::{OtpError, OtpService},
    },
    outbox::{
        types::{DispatchFailure, DispatchOutcome, OutboxEvent},
        worker::{DispatchFuture, OutboxHandler},
    },
};

pub struct OtpRequestHandler {
    store: Arc<dyn OtpRequestStore>,
    accounts: Arc<dyn MobileAccountStore>,
    otp: OtpService,
    cipher: RequestPhoneCipher,
}

impl OtpRequestHandler {
    pub fn new(
        store: Arc<dyn OtpRequestStore>,
        accounts: Arc<dyn MobileAccountStore>,
        otp: OtpService,
        encryption_key: SecretString,
    ) -> Self {
        Self {
            store,
            accounts,
            otp,
            cipher: RequestPhoneCipher::new(encryption_key),
        }
    }

    async fn process(&self, id: Uuid) -> Result<DispatchOutcome, DispatchFailure> {
        if let Some(request) = self.store.pending(id).await.map_err(|_| unavailable())? {
            self.dispatch(request).await?;
        }
        self.store.finish(id).await.map_err(|_| unavailable())?;
        Ok(DispatchOutcome::Completed)
    }

    async fn dispatch(&self, request: PendingOtpRequest) -> Result<(), DispatchFailure> {
        let phone = self
            .cipher
            .decrypt(request.id, &request.encrypted_phone)
            .map_err(|_| DispatchFailure::permanent("otp_request_invalid"))?;
        let account = self
            .accounts
            .find_by_phone_hash(request.phone_hash)
            .await
            .map_err(|_| unavailable())?;
        let eligible = match (request.registration, account) {
            (true, None) => true,
            (false, Some(account)) => !account.is_banned,
            _ => false,
        };
        if !eligible {
            return Ok(());
        }
        // Stable UUID + existing OTP ledger prevent a duplicate provider call
        // when a worker retries or recovers an interrupted claim.
        delivery_outcome(
            self.otp
                .send_in(&phone, Some(&request.id.to_string()), request.language)
                .await,
        )
    }
}

impl OutboxHandler for OtpRequestHandler {
    fn handle<'a>(&'a self, event: &'a OutboxEvent, _worker_id: Uuid) -> DispatchFuture<'a> {
        Box::pin(self.process(event.aggregate_id))
    }
}

fn delivery_outcome(result: Result<(), OtpError>) -> Result<(), DispatchFailure> {
    match result {
        Ok(())
        | Err(
            OtpError::DeliveryUnknown | OtpError::DeliveryUnavailable | OtpError::PhoneRateLimited,
        ) => Ok(()),
        Err(OtpError::Internal) => Err(unavailable()),
        Err(_) => Err(DispatchFailure::permanent("otp_request_invalid")),
    }
}

fn unavailable() -> DispatchFailure {
    DispatchFailure::transient("otp_dispatch_unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settled_and_uncertain_deliveries_never_trigger_another_provider_attempt() {
        for result in [
            Ok(()),
            Err(OtpError::DeliveryUnknown),
            Err(OtpError::DeliveryUnavailable),
            Err(OtpError::PhoneRateLimited),
        ] {
            assert_eq!(delivery_outcome(result), Ok(()));
        }
        assert_eq!(
            delivery_outcome(Err(OtpError::Internal)),
            Err(DispatchFailure::transient("otp_dispatch_unavailable"))
        );
        assert_eq!(
            delivery_outcome(Err(OtpError::InvalidPhone)),
            Err(DispatchFailure::permanent("otp_request_invalid"))
        );
    }
}
