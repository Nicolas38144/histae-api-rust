mod domain;
mod pg;
pub mod requests;
mod service;
mod store;

pub use domain::{
    BeginOtpDelivery, OtpChannel, OtpDeliverySnapshot, OtpDeliveryStart, OtpDeliveryState,
    OtpDeliveryStates, OtpScope, SmsDeliveryEvent, SmsEventKind, SmsEventOutcome,
};
pub use pg::OtpRepository;
pub(crate) use requests::delivery_outcome;
pub use requests::pg::PgOtpRequestStore;
pub use requests::{OtpRequestHandler, OtpRequestService};
pub use service::{ConsumedOtp, OtpError, OtpService};
pub use store::{OtpStore, OtpStoreFuture};
