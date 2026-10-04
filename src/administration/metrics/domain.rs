use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RevenuePeriod {
    #[serde(rename = "last_7_days")]
    Last7Days,
    #[serde(rename = "last_30_days")]
    Last30Days,
    #[serde(rename = "month_to_date")]
    MonthToDate,
    #[serde(rename = "previous_month")]
    PreviousMonth,
    #[serde(rename = "year_to_date")]
    YearToDate,
    #[serde(rename = "all_time")]
    AllTime,
}

impl RevenuePeriod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Last7Days => "last_7_days",
            Self::Last30Days => "last_30_days",
            Self::MonthToDate => "month_to_date",
            Self::PreviousMonth => "previous_month",
            Self::YearToDate => "year_to_date",
            Self::AllTime => "all_time",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AdminRevenue {
    pub period: RevenuePeriod,
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: DateTime<Utc>,
    pub premium_subscriptions: i64,
    pub price_per_subscription_cents: i32,
    pub estimated_revenue_cents: i64,
    pub currency: String,
    pub basis: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct UserMetrics {
    pub total: i64,
    pub active: i64,
    pub banned: i64,
    pub onboarded: i64,
    pub created_last_30_days: i64,
}
#[derive(Clone, Debug, Serialize)]
pub struct ModerationMetrics {
    pub pending_reports: i64,
    pub pending_content: i64,
    pub open_data_requests: i64,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct MatchMetrics {
    pub active: i64,
    pub awaiting_continuation: i64,
    pub confirmed: i64,
    pub expired: i64,
    pub ended: i64,
}
#[derive(Clone, Debug, Serialize)]
pub struct MessageMetrics {
    pub total: i64,
}
#[derive(Clone, Debug, Serialize)]
pub struct PhotoMetrics {
    pub pending: i64,
    pub processing: i64,
    pub ready: i64,
    pub deleting: i64,
    pub stale_processing: i64,
    pub deletion_dead_letters: i64,
    pub deletion_without_active_event: i64,
}
#[derive(Clone, Debug, Serialize)]
pub struct SubscriptionMetrics {
    pub plan: String,
    pub users: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdminBusinessMetrics {
    pub users: UserMetrics,
    pub moderation: ModerationMetrics,
    pub matches: MatchMetrics,
    pub messages: MessageMetrics,
    pub photos: PhotoMetrics,
    pub subscriptions: Vec<SubscriptionMetrics>,
    pub revenue: AdminRevenue,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdminMetrics {
    #[serde(flatten)]
    pub business: AdminBusinessMetrics,
    pub operations: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revenue_period_uses_the_existing_wire_values() {
        let cases = [
            (RevenuePeriod::Last7Days, "last_7_days"),
            (RevenuePeriod::Last30Days, "last_30_days"),
            (RevenuePeriod::MonthToDate, "month_to_date"),
            (RevenuePeriod::PreviousMonth, "previous_month"),
            (RevenuePeriod::YearToDate, "year_to_date"),
            (RevenuePeriod::AllTime, "all_time"),
        ];
        for (period, wire) in cases {
            let encoded = serde_json::to_string(&period).expect("revenue period serializes");
            assert_eq!(encoded, format!("\"{wire}\""));
            let decoded = serde_json::from_str::<RevenuePeriod>(&encoded)
                .expect("revenue period deserializes");
            assert_eq!(decoded, period);
        }
        assert!(serde_json::from_str::<RevenuePeriod>("\"last7_days\"").is_err());
    }
}
