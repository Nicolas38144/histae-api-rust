use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Json;
use serde::Serialize;

use crate::infra::postgres::Database;
use crate::infra::redis::RedisService;

use super::error::ApiError;

pub type ProbeFuture<'a> = Pin<Box<dyn Future<Output = Result<(), ProbeError>> + Send + 'a>>;

pub trait DependencyProbe: Send + Sync {
    fn check(&self) -> ProbeFuture<'_>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbeError;

#[derive(Clone)]
pub struct Readiness {
    postgres: Arc<dyn DependencyProbe>,
    redis: Arc<dyn DependencyProbe>,
    object_storage: Arc<dyn DependencyProbe>,
}

impl Readiness {
    pub fn new(
        postgres: Arc<dyn DependencyProbe>,
        redis: Arc<dyn DependencyProbe>,
        object_storage: Arc<dyn DependencyProbe>,
    ) -> Self {
        Self {
            postgres,
            redis,
            object_storage,
        }
    }

    pub async fn check(&self) -> Result<(), ApiError> {
        self.postgres
            .check()
            .await
            .map_err(|_| ApiError::dependency_unavailable())?;
        self.redis
            .check()
            .await
            .map_err(|_| ApiError::dependency_unavailable())?;
        self.object_storage
            .check()
            .await
            .map_err(|_| ApiError::dependency_unavailable())?;
        Ok(())
    }
}

impl DependencyProbe for Database {
    fn check(&self) -> ProbeFuture<'_> {
        Box::pin(async move { self.ping().await.map_err(|_| ProbeError) })
    }
}

impl DependencyProbe for RedisService {
    fn check(&self) -> ProbeFuture<'_> {
        Box::pin(async move { RedisService::check(self).await.map_err(|_| ProbeError) })
    }
}

#[derive(Serialize)]
pub struct HealthStatus {
    status: &'static str,
}

pub async fn live() -> Json<HealthStatus> {
    Json(HealthStatus { status: "ok" })
}

pub async fn ready(readiness: &Readiness) -> Result<Json<HealthStatus>, ApiError> {
    readiness.check().await?;
    Ok(Json(HealthStatus { status: "ready" }))
}
