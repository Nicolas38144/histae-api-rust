use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use uuid::Uuid;

use super::domain::{DevicePlatform, DeviceRecord, DeviceRegistration, PublicDevice};
use crate::infra::postgres::DatabaseError;
use crate::shared::text::javascript_trim;

pub type DeviceStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

pub trait DeviceStore: Send + Sync {
    fn register(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        registration: DeviceRegistration,
    ) -> DeviceStoreFuture<'_, Option<DeviceRecord>>;

    fn list(&self, user_id: Uuid) -> DeviceStoreFuture<'_, Vec<DeviceRecord>>;

    fn remove(&self, user_id: Uuid, device_id: Uuid) -> DeviceStoreFuture<'_, bool>;
}

#[derive(Clone)]
pub struct DeviceService {
    store: Arc<dyn DeviceStore>,
}

impl DeviceService {
    pub fn new(store: Arc<dyn DeviceStore>) -> Self {
        Self { store }
    }

    pub async fn register(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        raw_token: String,
        platform: DevicePlatform,
        raw_app_version: Option<String>,
    ) -> Result<PublicDevice, DeviceError> {
        let token = javascript_trim(&raw_token).to_owned();
        let app_version = raw_app_version
            .map(|value| javascript_trim(&value).to_owned())
            .filter(|value| !value.is_empty());
        let device = self
            .store
            .register(
                user_id,
                session_id,
                DeviceRegistration {
                    token,
                    platform,
                    app_version,
                },
            )
            .await?
            .ok_or(DeviceError::AuthenticationRequired)?;
        Ok(device.into())
    }

    pub async fn list(&self, user_id: Uuid) -> Result<Vec<PublicDevice>, DeviceError> {
        self.store
            .list(user_id)
            .await
            .map(|devices| devices.into_iter().map(PublicDevice::from).collect())
            .map_err(DeviceError::from)
    }

    pub async fn remove(&self, user_id: Uuid, device_id: Uuid) -> Result<(), DeviceError> {
        if self.store.remove(user_id, device_id).await? {
            Ok(())
        } else {
            Err(DeviceError::NotFound)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceError {
    AuthenticationRequired,
    NotFound,
    Database(DatabaseError),
}

impl From<DatabaseError> for DeviceError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use chrono::Utc;

    use super::*;

    #[derive(Default)]
    struct FakeStore {
        registration: Mutex<Option<DeviceRegistration>>,
        device: Mutex<Option<DeviceRecord>>,
        removed: Mutex<bool>,
    }

    impl DeviceStore for FakeStore {
        fn register(
            &self,
            _user_id: Uuid,
            _session_id: Uuid,
            registration: DeviceRegistration,
        ) -> DeviceStoreFuture<'_, Option<DeviceRecord>> {
            Box::pin(async move {
                *self
                    .registration
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)? = Some(registration);
                self.device
                    .lock()
                    .map(|device| device.clone())
                    .map_err(|_| DatabaseError::QueryFailed)
            })
        }

        fn list(&self, _user_id: Uuid) -> DeviceStoreFuture<'_, Vec<DeviceRecord>> {
            Box::pin(async move {
                self.device
                    .lock()
                    .map(|device| device.clone().into_iter().collect())
                    .map_err(|_| DatabaseError::QueryFailed)
            })
        }

        fn remove(&self, _user_id: Uuid, _device_id: Uuid) -> DeviceStoreFuture<'_, bool> {
            Box::pin(async move {
                self.removed
                    .lock()
                    .map(|removed| *removed)
                    .map_err(|_| DatabaseError::QueryFailed)
            })
        }
    }

    fn device() -> DeviceRecord {
        DeviceRecord {
            id: Uuid::new_v4(),
            session_id: Some(Uuid::new_v4()),
            platform: DevicePlatform::Android,
            app_version: Some("1.2.3".to_owned()),
            created_at: Utc::now(),
            last_used_at: Some(Utc::now()),
        }
    }

    #[tokio::test]
    async fn trims_private_registration_values_and_never_places_the_token_in_public_output() {
        let store = Arc::new(FakeStore::default());
        *store.device.lock().expect("fake store") = Some(device());
        let service = DeviceService::new(store.clone());
        let public = service
            .register(
                Uuid::new_v4(),
                Uuid::new_v4(),
                "\u{feff} private-provider-token \u{3000}".to_owned(),
                DevicePlatform::Android,
                Some(" 1.2.3 ".to_owned()),
            )
            .await
            .expect("registration succeeds");
        let registration = store
            .registration
            .lock()
            .expect("fake store")
            .clone()
            .expect("registration captured");
        assert_eq!(registration.token, "private-provider-token");
        assert_eq!(registration.app_version.as_deref(), Some("1.2.3"));
        let serialized = serde_json::to_string(&public).expect("public device serializes");
        assert!(!serialized.contains("private-provider-token"));
        assert!(!serialized.contains("token"));
    }

    #[tokio::test]
    async fn distinguishes_a_stale_session_from_an_unknown_device() {
        let store = Arc::new(FakeStore::default());
        let service = DeviceService::new(store.clone());
        assert_eq!(
            service
                .register(
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    "provider-token-with-enough-characters".to_owned(),
                    DevicePlatform::Ios,
                    None,
                )
                .await,
            Err(DeviceError::AuthenticationRequired)
        );
        assert_eq!(
            service.remove(Uuid::new_v4(), Uuid::new_v4()).await,
            Err(DeviceError::NotFound)
        );
    }
}
