use histae_api_rust::billing::domain::BillingPeriod;
use histae_api_rust::catalog::domain::{PlanCatalog, PlanFeature, SubscriptionPlan};
use serde_json::json;

fn plan(code: &str, monthly: i32, annual: i32) -> SubscriptionPlan {
    let display_name = if code == "free" { "Free" } else { "Premium" };
    SubscriptionPlan {
        code: code.into(),
        display_name: display_name.into(),
        monthly_price_cents: monthly,
        annual_price_cents: annual,
        currency: "EUR".into(),
        weekly_continuation_limit: (code == "free").then_some(6),
        weekly_included_credits: (code == "free").then_some(3),
        advertising_credit_stock_limit: 3,
        ads_removed: code == "premium",
        rewarded_advertising_enabled: false,
        features: vec![PlanFeature {
            code: "messages".into(),
            display_name: None,
            description: None,
            feature_value: Some(json!({ "included": true })),
        }],
    }
}

#[test]
fn catalog_exposes_three_offers_and_keeps_the_two_entitlement_plans() {
    let catalog = PlanCatalog::new(vec![
        plan("free", 0, 0),
        plan("premium", 500, 4800),
    ]);
    let body = serde_json::to_value(&catalog).expect("serialized catalogue");
    assert_eq!(body["plans"].as_array().expect("plans").len(), 2);
    assert_eq!(
        body["offers"],
        json!([
            {
                "code": "free", "plan_code": "free", "display_name": "Free",
                "billing_period": null, "price_cents": 0, "currency": "EUR"
            },
            {
                "code": "premium_monthly", "plan_code": "premium", "display_name": "Premium Mensuel",
                "billing_period": "monthly", "price_cents": 500, "currency": "EUR"
            },
            {
                "code": "premium_annual", "plan_code": "premium", "display_name": "Premium Annuel",
                "billing_period": "annual", "price_cents": 4800, "currency": "EUR"
            }
        ])
    );
    assert_eq!(catalog.offers[1].plan_code, catalog.offers[2].plan_code);
    assert_eq!(catalog.plans[1].features[0].code, "messages");
    for plan in body["plans"].as_array().expect("plans") {
        assert!(plan.get("trial_days").is_none());
    }
}

#[test]
fn offers_follow_database_prices_currency_and_availability_without_trials() {
    let mut premium = plan("premium", 1234, 9876);
    premium.currency = "CHF".into();
    let catalog = PlanCatalog::new(vec![premium]);
    assert_eq!(catalog.offers.len(), 2);
    assert_eq!(catalog.offers[0].price_cents, 1234);
    assert_eq!(catalog.offers[1].price_cents, 9876);
    assert_eq!(catalog.offers[1].billing_period, Some(BillingPeriod::Annual));
    assert!(
        catalog
            .offers
            .iter()
            .all(|offer| offer.currency == "CHF")
    );
    assert!(PlanCatalog::new(Vec::new()).offers.is_empty());
    assert_eq!(PlanCatalog::new(vec![plan("free", 0, 0)]).offers.len(), 1);
}
