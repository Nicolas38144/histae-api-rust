use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::identity::mobile::domain::wire_timestamp;

pub const CHECKOUT_TTL_MINUTES: i64 = 30;
pub const CHECKOUT_CREATION_STALE_SECONDS: i64 = 60;
pub const CUSTOMER_CREATE_SAFETY_HOURS: i64 = 23;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingPeriod {
    Monthly,
    Annual,
}

impl BillingPeriod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Monthly => "monthly",
            Self::Annual => "annual",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "monthly" => Some(Self::Monthly),
            "annual" => Some(Self::Annual),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionProvider {
    Stripe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StripeSubscriptionStatus {
    Incomplete,
    IncompleteExpired,
    Trialing,
    Active,
    PastDue,
    Canceled,
    Unpaid,
    Paused,
}

impl StripeSubscriptionStatus {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "incomplete" => Some(Self::Incomplete),
            "incomplete_expired" => Some(Self::IncompleteExpired),
            "trialing" => Some(Self::Trialing),
            "active" => Some(Self::Active),
            "past_due" => Some(Self::PastDue),
            "canceled" => Some(Self::Canceled),
            "unpaid" => Some(Self::Unpaid),
            "paused" => Some(Self::Paused),
            _ => None,
        }
    }

    pub const fn grants_access(self) -> bool {
        matches!(self, Self::Trialing | Self::Active | Self::PastDue)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscriptionRow {
    pub plan: String,
    pub provider: Option<SubscriptionProvider>,
    pub billing_period: Option<BillingPeriod>,
    pub status: Option<StripeSubscriptionStatus>,
    pub cancel_at_period_end: bool,
    pub current_period_starts_at: Option<DateTime<Utc>>,
    pub current_period_ends_at: Option<DateTime<Utc>>,
    pub trial_ends_at: Option<DateTime<Utc>>,
    pub canceled_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SubscriptionView {
    pub plan: &'static str,
    pub provider: Option<SubscriptionProvider>,
    pub status: Option<StripeSubscriptionStatus>,
    pub access_granted: bool,
    pub billing_period: Option<BillingPeriod>,
    pub cancel_at_period_end: bool,
    pub current_period_starts_at: Option<String>,
    pub current_period_ends_at: Option<String>,
    pub trial_ends_at: Option<String>,
    pub canceled_at: Option<String>,
    pub customer_portal_available: bool,
}

impl SubscriptionView {
    pub fn free(customer_portal_available: bool) -> Self {
        Self {
            plan: "free",
            provider: None,
            status: None,
            access_granted: false,
            billing_period: None,
            cancel_at_period_end: false,
            current_period_starts_at: None,
            current_period_ends_at: None,
            trial_ends_at: None,
            canceled_at: None,
            customer_portal_available,
        }
    }

    pub fn from_row(row: SubscriptionRow, now: DateTime<Utc>, portal: bool) -> Self {
        let within_period = row.current_period_ends_at.is_none_or(|end| end > now);
        let provider_grants = row.provider.is_none_or(|_| {
            row.status
                .is_some_and(StripeSubscriptionStatus::grants_access)
        });
        let access_granted = row.plan == "premium" && within_period && provider_grants;
        Self {
            plan: if access_granted { "premium" } else { "free" },
            provider: row.provider,
            status: row.status,
            access_granted,
            billing_period: row.billing_period,
            cancel_at_period_end: row.cancel_at_period_end,
            current_period_starts_at: row.current_period_starts_at.map(wire_timestamp),
            current_period_ends_at: row.current_period_ends_at.map(wire_timestamp),
            trial_ends_at: row.trial_ends_at.map(wire_timestamp),
            canceled_at: row.canceled_at.map(wire_timestamp),
            customer_portal_available: portal,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CheckoutSessionView {
    pub session_id: String,
    pub url: String,
    pub expires_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistedCheckoutSession {
    pub session_id: String,
    pub url: String,
    pub expires_at: DateTime<Utc>,
}

impl From<PersistedCheckoutSession> for CheckoutSessionView {
    fn from(value: PersistedCheckoutSession) -> Self {
        Self {
            session_id: value.session_id,
            url: value.url,
            expires_at: wire_timestamp(value.expires_at),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckoutContext {
    pub attempt_id: Uuid,
    pub stripe_customer_id: Option<String>,
    pub trial_days: i16,
    pub trial_used: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BeginCheckoutResult {
    Created(CheckoutContext),
    Retry(CheckoutContext),
    Replay(PersistedCheckoutSession),
    NotFound,
    AlreadySubscribed,
    InProgress,
    CustomerReconciliationRequired,
    IdempotencyConflict,
    IdempotencyConsumed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomerCreation {
    pub id: Uuid,
    pub user_id: Uuid,
    pub customer_creation_started_at: DateTime<Utc>,
    pub created_customer_id: Option<String>,
    pub customer_erased_at: Option<DateTime<Utc>>,
}
