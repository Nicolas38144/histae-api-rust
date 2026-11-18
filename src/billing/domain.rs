use serde::{Deserialize, Serialize};

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
    AppStore,
    GooglePlay,
}

impl SubscriptionProvider {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "app_store" => Some(Self::AppStore),
            "google_play" => Some(Self::GooglePlay),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SubscriptionView {
    pub plan: &'static str,
    pub provider: Option<SubscriptionProvider>,
    pub status: Option<String>,
    pub access_granted: bool,
    pub billing_period: Option<BillingPeriod>,
    pub cancel_at_period_end: bool,
    pub current_period_starts_at: Option<String>,
    pub current_period_ends_at: Option<String>,
    pub canceled_at: Option<String>,
    /// Compatibility for installed clients; web billing has been retired.
    pub customer_portal_available: bool,
    /// Trials are unavailable; retained as null for installed clients.
    pub trial_ends_at: Option<String>,
    pub management_url: Option<String>,
}

impl SubscriptionView {
    pub fn free() -> Self {
        Self {
            plan: "free",
            provider: None,
            status: None,
            access_granted: false,
            billing_period: None,
            cancel_at_period_end: false,
            current_period_starts_at: None,
            current_period_ends_at: None,
            canceled_at: None,
            management_url: None,
            customer_portal_available: false,
            trial_ends_at: None,
        }
    }
}
