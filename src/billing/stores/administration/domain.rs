use crate::billing::stores::StoreProvider;
use crate::{administration::metrics::RevenuePeriod, billing::domain::BillingPeriod};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreFinanceImport {
    pub(super) provider: StoreProvider,
    pub(super) report_id: String,
    pub(super) rows: Vec<FinanceRow>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FinanceRow {
    pub(super) row_id: String,
    pub(super) paid_at: DateTime<Utc>,
    pub(super) currency: String,
    pub(super) gross_cents: i64,
    pub(super) net_cents: Option<i64>,
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
impl StoreFinanceImport {
    pub fn valid(&self) -> bool {
        let mut ids = HashSet::new();
        identifier(&self.report_id)
            && !self.rows.is_empty()
            && self.rows.len() <= 1000
            && self.rows.iter().all(|row| {
                identifier(&row.row_id)
                    && ids.insert(&row.row_id)
                    && row.currency.len() == 3
                    && row.currency.bytes().all(|b| b.is_ascii_uppercase())
                    && row.gross_cents.unsigned_abs() <= 1_000_000_000_000
                    && row
                        .net_cents
                        .is_none_or(|v| v.unsigned_abs() <= 1_000_000_000_000)
                    && row.paid_at <= Utc::now() + chrono::TimeDelta::minutes(5)
            })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn financial_reports_require_distinct_rows_valid_currency_and_bounded_signed_amounts() {
        let value = serde_json::json!({"provider":"google_play","report_id":"2026-10","rows":[{"row_id":"line1","paid_at":"2026-10-01T00:00:00Z","currency":"EUR","gross_cents":500,"net_cents":425}]});
        let body: StoreFinanceImport = serde_json::from_value(value.clone()).unwrap();
        assert!(body.valid());
        let mut duplicate = value.clone();
        let row = duplicate["rows"][0].clone();
        duplicate["rows"].as_array_mut().unwrap().push(row);
        assert!(
            !serde_json::from_value::<StoreFinanceImport>(duplicate)
                .unwrap()
                .valid()
        );
        for (field, invalid_value) in [
            ("currency", serde_json::json!("eur")),
            ("gross_cents", serde_json::json!(1_000_000_000_001_i64)),
            ("paid_at", serde_json::json!("2099-01-01T00:00:00Z")),
        ] {
            let mut invalid = value.clone();
            invalid["rows"][0][field] = invalid_value;
            assert!(
                !serde_json::from_value::<StoreFinanceImport>(invalid)
                    .unwrap()
                    .valid()
            );
        }
        let mut refund = value.clone();
        refund["rows"][0]["gross_cents"] = serde_json::json!(-500);
        refund["rows"][0]["net_cents"] = serde_json::json!(-425);
        assert!(
            serde_json::from_value::<StoreFinanceImport>(refund)
                .unwrap()
                .valid()
        );
    }
}

#[derive(Serialize)]
pub(super) struct StoreAvailability {
    pub(super) app_store: bool,
    pub(super) google_play: bool,
}

#[derive(Serialize)]
pub(super) struct StoreSource {
    pub(super) provider: StoreProvider,
    pub(super) subscriptions: i64,
    pub(super) active_accounts: i64,
    pub(super) unacknowledged: i64,
    pub(super) last_verified_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
pub(super) struct StoreFinanceSummary {
    pub(super) provider: StoreProvider,
    pub(super) currency: String,
    pub(super) gross_cents: i64,
    pub(super) net_cents: Option<i64>,
    pub(super) rows: i64,
    pub(super) last_imported_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub(super) struct StoreSubscriptionSummary {
    pub(super) user_id: Uuid,
    pub(super) provider: StoreProvider,
    pub(super) billing_period: BillingPeriod,
    pub(super) status: String,
    pub(super) expires_at: DateTime<Utc>,
    pub(super) auto_renew: bool,
    pub(super) verified_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub(super) struct StoreBillingStatus {
    pub(super) enabled: StoreAvailability,
    pub(super) sources: Vec<StoreSource>,
    pub(super) period: RevenuePeriod,
    pub(super) finance: Vec<StoreFinanceSummary>,
    pub(super) recent: Vec<StoreSubscriptionSummary>,
    pub(super) pending_notifications: i64,
    pub(super) dead_letters: i64,
    pub(super) finance_basis: &'static str,
}
