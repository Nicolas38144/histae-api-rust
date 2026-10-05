use super::{
    BillingReconciliationListing, ReconciliationKind, ReconciliationListError, ReconciliationPage,
};
use crate::http::{
    error::ApiError,
    extract::{ApiDto, ValidatedQuery},
    router::HttpState,
};
use crate::identity::admin::http::{AdminAuthHttpState, AdminIdentity};
use crate::shared::text::validator_js_length;
use axum::{Json, Router, extract::Extension, http::StatusCode, routing::get};
use serde::Deserialize;
use std::sync::Arc;
#[derive(Clone)]
pub struct BillingReconciliationHttpState {
    service: Arc<dyn BillingReconciliationListing>,
}

impl BillingReconciliationHttpState {
    pub fn new(service: Arc<dyn BillingReconciliationListing>) -> Self {
        Self { service }
    }
}

pub fn routes(
    state: BillingReconciliationHttpState,
    auth: AdminAuthHttpState,
) -> Router<HttpState> {
    Router::new()
        .route(
            "/api/admin/billing-reconciliation",
            get(list_reconciliation),
        )
        .layer(Extension(state))
        .layer(Extension(auth))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    #[serde(
        default = "default_limit",
        deserialize_with = "crate::shared::validation::deserialize_query_u32"
    )]
    limit: u32,
    cursor: Option<String>,
    #[serde(default = "default_kind")]
    kind: ReconciliationFilter,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReconciliationFilter {
    All,
    Subscription,
    CustomerCreation,
}

impl ReconciliationFilter {
    const fn kind(self) -> Option<ReconciliationKind> {
        match self {
            Self::All => None,
            Self::Subscription => Some(ReconciliationKind::Subscription),
            Self::CustomerCreation => Some(ReconciliationKind::CustomerCreation),
        }
    }
}

impl ApiDto for ListQuery {
    const ERROR_CODE: &'static str = "invalid_billing_reconciliation_request";
    const ERROR_MESSAGE: &'static str = "The billing reconciliation request is invalid.";

    fn is_valid(&self) -> bool {
        (1..=100).contains(&self.limit)
            && self
                .cursor
                .as_ref()
                .is_none_or(|value| validator_js_length(value) <= 512)
    }
}

const fn default_limit() -> u32 {
    20
}

const fn default_kind() -> ReconciliationFilter {
    ReconciliationFilter::All
}

async fn list_reconciliation(
    AdminIdentity(_identity): AdminIdentity,
    Extension(state): Extension<BillingReconciliationHttpState>,
    ValidatedQuery(query): ValidatedQuery<ListQuery>,
) -> Result<Json<ReconciliationPage>, ApiError> {
    state
        .service
        .list(query.kind.kind(), query.limit, query.cursor.as_deref())
        .await
        .map(Json)
        .map_err(|error| match error {
            ReconciliationListError::InvalidCursor => ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_cursor",
                "The pagination cursor is invalid.",
            ),
            ReconciliationListError::Database(error) => error.into(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pagination_coercion_matches_class_transformer() {
        for value in ["20", "0x14", "2e1", "20.0", "0b10100", "0o24"] {
            let uri = format!("/?limit={value}").parse().expect("URI");
            let axum::extract::Query(query) =
                axum::extract::Query::<ListQuery>::try_from_uri(&uri).expect("query");
            assert!(query.is_valid());
            assert_eq!(query.limit, 20);
        }
        for value in ["0", "101", "1.5", "-1", "NaN", "Infinity"] {
            let uri = format!("/?limit={value}").parse().expect("URI");
            assert!(
                axum::extract::Query::<ListQuery>::try_from_uri(&uri)
                    .map_or(true, |query| !query.0.is_valid())
            );
        }
    }
    #[test]
    fn admin_query_is_strict_and_bounds_its_page_size() {
        let query: ListQuery = serde_json::from_value(serde_json::json!({
            "kind": "all",
            "limit": "100"
        }))
        .unwrap_or_else(|_| unreachable!());
        assert!(query.is_valid());
        assert!(query.kind.kind().is_none());

        let oversized: ListQuery = serde_json::from_value(serde_json::json!({
            "kind": "subscription",
            "limit": "101"
        }))
        .unwrap_or_else(|_| unreachable!());
        assert!(!oversized.is_valid());

        assert!(
            serde_json::from_value::<ListQuery>(serde_json::json!({
                "kind": "unsupported"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ListQuery>(serde_json::json!({
                "unknown": true
            }))
            .is_err()
        );
    }
}
