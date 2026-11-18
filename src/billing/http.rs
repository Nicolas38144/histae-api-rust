use super::{
    domain::{BillingError, SubscriptionView},
    service::BillingService,
};
use crate::{
    http::{error::ApiError, router::HttpState},
    identity::mobile::http::{MobileAuthState, OnboardedMobile},
};
use axum::{Extension, Json, Router, http::StatusCode, routing::get};
use std::{future::Future, pin::Pin, sync::Arc};

#[derive(Clone)]
pub struct BillingHttpState {
    service: Arc<dyn BillingUseCases>,
}

impl BillingHttpState {
    pub fn new(service: Arc<dyn BillingUseCases>) -> Self {
        Self { service }
    }
}

pub type BillingHttpFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, BillingError>> + Send + 'a>>;

pub trait BillingUseCases: Send + Sync {
    fn subscription(&self, user_id: uuid::Uuid) -> BillingHttpFuture<'_, SubscriptionView>;
}

impl BillingUseCases for BillingService {
    fn subscription(&self, user_id: uuid::Uuid) -> BillingHttpFuture<'_, SubscriptionView> {
        Box::pin(BillingService::subscription(self, user_id))
    }
}

pub fn routes(state: BillingHttpState, auth: MobileAuthState) -> Router<HttpState> {
    Router::new()
        .route("/api/users/me/subscription", get(subscription))
        .layer(Extension(state))
        .layer(Extension(auth))
}

async fn subscription(
    OnboardedMobile(identity): OnboardedMobile,
    Extension(state): Extension<BillingHttpState>,
) -> Result<Json<SubscriptionView>, ApiError> {
    state
        .service
        .subscription(identity.account.user_id)
        .await
        .map(Json)
        .map_err(|error| match error {
            BillingError::Database(error) => error.into(),
            BillingError::InvalidStoredData => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "billing_unavailable",
                "The subscription is temporarily unavailable.",
            ),
        })
}
