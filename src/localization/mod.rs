pub mod catalog;
pub mod copy;
pub mod http;
mod language;
pub mod legal;
pub(crate) mod pg;
pub mod store;

use crate::infra::postgres::DatabaseError;
use catalog::CatalogSnapshot;
pub use language::Language;
pub use pg::PgLocalizationRepository;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

#[derive(Clone)]
pub struct LocalizationService {
    store: Arc<dyn store::LocalizationStore>,
    cache: Arc<RwLock<Option<CachedCatalog>>>,
    refresh: Arc<Mutex<()>>,
}
impl LocalizationService {
    pub fn new(store: Arc<dyn store::LocalizationStore>) -> Self {
        Self {
            store,
            cache: Arc::new(RwLock::new(None)),
            refresh: Arc::new(Mutex::new(())),
        }
    }
    pub async fn language(&self, user_id: Uuid) -> Result<Language, DatabaseError> {
        self.store.language(user_id).await
    }
    pub async fn set_language(
        &self,
        user_id: Uuid,
        language: Language,
    ) -> Result<Language, DatabaseError> {
        self.store.set_language(user_id, language).await
    }
    pub async fn invalidate(&self) {
        let _refresh = self.refresh.lock().await;
        *self.cache.write().await = None;
    }
    pub async fn catalog(&self, fresh: bool) -> Result<Arc<CatalogSnapshot>, DatabaseError> {
        if !fresh {
            if let Some(cached) = &*self.cache.read().await {
                if cached.at.elapsed() < Duration::from_secs(15) {
                    return Ok(cached.value.clone());
                }
            }
        }
        let _refresh = self.refresh.lock().await;
        if !fresh {
            if let Some(cached) = &*self.cache.read().await {
                if cached.at.elapsed() < Duration::from_secs(15) {
                    return Ok(cached.value.clone());
                }
            }
        }
        let catalog = Arc::new(self.store.catalog().await?);
        *self.cache.write().await = Some(CachedCatalog {
            at: Instant::now(),
            value: catalog.clone(),
        });
        Ok(catalog)
    }
}

struct CachedCatalog {
    at: Instant,
    value: Arc<CatalogSnapshot>,
}
