use super::domain::*;
use crate::{identity::mobile::sweego::SmsFailureReason, infra::postgres::DatabaseError};
use std::{future::Future, pin::Pin};
use uuid::Uuid;

pub type OtpStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait OtpStore: Send + Sync {
    fn begin(&self, input: BeginOtpDelivery) -> OtpStoreFuture<'_, OtpDeliveryStart>;
    fn mark_accepted(
        &self,
        id: Uuid,
        phone_hash: String,
        transaction_id: String,
        message_id: String,
    ) -> OtpStoreFuture<'_, bool>;
    fn mark_outcome(
        &self,
        id: Uuid,
        phone_hash: String,
        state: OtpDeliveryState,
        reason: SmsFailureReason,
    ) -> OtpStoreFuture<'_, OtpDeliveryState>;
    fn apply_sms_event(&self, event: SmsDeliveryEvent) -> OtpStoreFuture<'_, SmsEventOutcome>;
    fn consume(&self, phone_hash: String, otp_hash: String) -> OtpStoreFuture<'_, bool>;
    fn snapshot(&self) -> OtpStoreFuture<'_, OtpDeliverySnapshot>;
}
