use crate::{billing::domain::BillingPeriod, http::extract::ApiDto};
use serde::Deserialize;
use uuid::Uuid;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SendBody {
    pub user_id: Uuid,
    pub phone_number: String,
}
impl ApiDto for SendBody {
    fn is_valid(&self) -> bool {
        self.user_id.get_version_num() == 4 && self.phone_number.len() <= 32
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VerifyBody {
    pub challenge_id: Uuid,
    pub phone_number: String,
    pub otp: String,
}
impl ApiDto for VerifyBody {
    fn is_valid(&self) -> bool {
        self.phone_number.len() <= 32
            && self.otp.len() == 6
            && self.otp.bytes().all(|b| b.is_ascii_digit())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckoutBody {
    pub billing_period: BillingPeriod,
}
impl ApiDto for CheckoutBody {}
