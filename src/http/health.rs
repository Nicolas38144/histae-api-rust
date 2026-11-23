use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Json;
use serde::Serialize;
use tokio::{
    sync::Mutex,
    time::{self, Duration, Instant},
};

use crate::infra::postgres::Database;
use crate::infra::redis::RedisService;

use super::error::ApiError;

pub type ProbeFuture<'a> = Pin<Box<dyn Future<Output = Result<(), WebauthnError>> + Send + 'a>>;

pub trait DependencyProbe: Send + Sync {
    fn check(&self) -> ProbeFuture<'_>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WebauthnError;

const READINESS_CACHE_TTL: Duration = Duration::from_secs(3);
const READINESS_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Clone)]
pub struct Readiness {
    postgres: Arc<dyn DependencyProbe>,
    redis: Arc<dyn DependencyProbe>,
    object_storage: Arc<dyn DependencyProbe>,
    cached: Arc<Mutex<Option<(Instant, bool)>>>,
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
            cached: Arc::default(),
        }
    }

    pub async fn check(&self) -> Result<(), ApiError> {
        let mut cache = self.cached.lock().await;
        let ready = match *cache {
            Some((at, ready)) if at.elapsed() < READINESS_CACHE_TTL => ready,
            _ => {
                let ready = matches!(
                    time::timeout(READINESS_TIMEOUT, async {
                        tokio::try_join!(
                            self.postgres.check(),
                            self.redis.check(),
                            self.object_storage.check()
                        )
                    })
                    .await,
                    Ok(Ok(_))
                );
                *cache = Some((Instant::now(), ready));
                ready
            }
        };
        ready
            .then_some(())
            .ok_or_else(ApiError::dependency_unavailable)
    }
}

impl DependencyProbe for Database {
    fn check(&self) -> ProbeFuture<'_> {
        Box::pin(async move { self.ping().await.map_err(|_| WebauthnError) })
    }
}

impl DependencyProbe for RedisService {
    fn check(&self) -> ProbeFuture<'_> {
        Box::pin(async move { RedisService::check(self).await.map_err(|_| WebauthnError) })
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
