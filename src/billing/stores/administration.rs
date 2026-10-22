mod domain;
mod pg;
use super::pg::StoreRepository;
use crate::{
    administration::metrics::RevenuePeriod,
    config::StoreBillingConfig,
    http::{
        error::ApiError,
        extract::{ApiDto, ValidatedJson, ValidatedQuery},
        router::HttpState,
    },
    identity::{
        admin::http::{AdminAuthHttpState, AdminIdentity, RecentAdminIdentity},
        admin_role::AdminRole,
    },
};
use axum::{
    Json, Router,
    extract::Extension,
    http::StatusCode,
    routing::{get, post},
};
use domain::StoreBillingStatus;
pub use domain::StoreFinanceImport;
pub use pg::{FinanceImportError, import_report};
use serde::Deserialize;

#[derive(Clone)]
pub struct StoreAdminState {
    pub repository: StoreRepository,
    pub config: StoreBillingConfig,
}
pub fn routes(state: StoreAdminState, auth: AdminAuthHttpState) -> Router<HttpState> {
    Router::new()
        .route("/api/admin/store-billing", get(status))
        .route("/api/admin/store-billing/finance", post(import))
        .layer(Extension(state))
        .layer(Extension(auth))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    #[serde(default = "default_period")]
    period: RevenuePeriod,
}
fn default_period() -> RevenuePeriod {
    RevenuePeriod::Last30Days
}
impl ApiDto for Query {}
async fn status(
    AdminIdentity(_): AdminIdentity,
    Extension(state): Extension<StoreAdminState>,
    ValidatedQuery(query): ValidatedQuery<Query>,
) -> Result<Json<StoreBillingStatus>, ApiError> {
    pg::snapshot(&state.repository, &state.config, query.period)
        .await
        .map(Json)
        .map_err(crate::billing::stores::http::error)
}

impl ApiDto for StoreFinanceImport {
    fn is_valid(&self) -> bool {
        self.valid()
    }
}
async fn import(
    RecentAdminIdentity(actor): RecentAdminIdentity,
    Extension(state): Extension<StoreAdminState>,
    ValidatedJson(body): ValidatedJson<StoreFinanceImport>,
) -> Result<StatusCode, ApiError> {
    if actor.role != AdminRole::Superadmin {
        return Err(forbidden());
    }
    import_report(&state.repository, actor.user_id, body)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|error| match error {
            FinanceImportError::Invalid => ApiError::invalid_body(),
            FinanceImportError::Forbidden => forbidden(),
            FinanceImportError::Conflict => ApiError::new(
                StatusCode::CONFLICT,
                "finance_report_conflict",
                "A report row already exists with different values.",
            ),
            FinanceImportError::Database(error) => error.into(),
        })
}
fn forbidden() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "admin_action_forbidden",
        "This administrator action is not allowed.",
    )
}
