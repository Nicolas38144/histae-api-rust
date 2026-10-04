use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use uuid::Uuid;

use super::domain::BlockedUser;
use super::store::PrivacyStore;
use crate::infra::postgres::DatabaseError;
use crate::notifications::delivery::MobileDeliveryService;

pub type PrivacyEventFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

pub trait PrivacyEventPublisher: Send + Sync {
    fn matches_invalidated<'a>(&'a self, recipients: [Uuid; 2]) -> PrivacyEventFuture<'a>;
}

impl PrivacyEventPublisher for MobileDeliveryService {
    fn matches_invalidated<'a>(&'a self, recipients: [Uuid; 2]) -> PrivacyEventFuture<'a> {
        Box::pin(async move {
            self.matches_invalidated(recipients).await;
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopPrivacyEventPublisher;

impl PrivacyEventPublisher for NoopPrivacyEventPublisher {
    fn matches_invalidated<'a>(&'a self, _recipients: [Uuid; 2]) -> PrivacyEventFuture<'a> {
        Box::pin(async {})
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivacyError {
    InvalidBlock,
    UserNotFound,
    Database(DatabaseError),
}

impl From<DatabaseError> for PrivacyError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

#[derive(Clone)]
pub struct PrivacyService {
    store: Arc<dyn PrivacyStore>,
    events: Arc<dyn PrivacyEventPublisher>,
}

impl PrivacyService {
    pub fn new(store: Arc<dyn PrivacyStore>, events: Arc<dyn PrivacyEventPublisher>) -> Self {
        Self { store, events }
    }

    pub async fn block(&self, blocker_id: Uuid, blocked_id: Uuid) -> Result<(), PrivacyError> {
        if blocker_id == blocked_id {
            return Err(PrivacyError::InvalidBlock);
        }
        if !self.store.block(blocker_id, blocked_id).await? {
            return Err(PrivacyError::UserNotFound);
        }
        self.events
            .matches_invalidated([blocker_id, blocked_id])
            .await;
        Ok(())
    }

    pub async fn unblock(&self, blocker_id: Uuid, blocked_id: Uuid) -> Result<(), PrivacyError> {
        self.store.unblock(blocker_id, blocked_id).await?;
        self.events
            .matches_invalidated([blocker_id, blocked_id])
            .await;
        Ok(())
    }

    pub async fn blocked_users(&self, blocker_id: Uuid) -> Result<Vec<BlockedUser>, PrivacyError> {
        self.store
            .blocked_users(blocker_id)
            .await
            .map(|rows| rows.into_iter().map(BlockedUser::from).collect())
            .map_err(PrivacyError::from)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::privacy::domain::BlockedUserRow;
    use crate::privacy::store::PrivacyStoreFuture;

    #[derive(Default)]
    struct FakeStore {
        target_exists: bool,
        calls: Mutex<Vec<&'static str>>,
    }

    impl PrivacyStore for FakeStore {
        fn block(&self, _blocker: Uuid, _blocked: Uuid) -> PrivacyStoreFuture<'_, bool> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)?
                    .push("block");
                Ok(self.target_exists)
            })
        }

        fn unblock(&self, _blocker: Uuid, _blocked: Uuid) -> PrivacyStoreFuture<'_, ()> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)?
                    .push("unblock");
                Ok(())
            })
        }

        fn blocked_users(&self, _blocker: Uuid) -> PrivacyStoreFuture<'_, Vec<BlockedUserRow>> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    #[derive(Default)]
    struct Events(Mutex<Vec<[Uuid; 2]>>);

    impl PrivacyEventPublisher for Events {
        fn matches_invalidated<'a>(&'a self, recipients: [Uuid; 2]) -> PrivacyEventFuture<'a> {
            Box::pin(async move {
                if let Ok(mut events) = self.0.lock() {
                    events.push(recipients);
                }
            })
        }
    }

    #[tokio::test]
    async fn rejects_self_block_before_storage_and_publishes_only_after_success() {
        let store = Arc::new(FakeStore::default());
        let events = Arc::new(Events::default());
        let service = PrivacyService::new(store.clone(), events.clone());
        let user_id = Uuid::new_v4();
        assert_eq!(
            service.block(user_id, user_id).await,
            Err(PrivacyError::InvalidBlock)
        );
        assert!(store.calls.lock().is_ok_and(|calls| calls.is_empty()));
        assert!(events.0.lock().is_ok_and(|items| items.is_empty()));

        let target = Uuid::new_v4();
        assert_eq!(
            service.block(user_id, target).await,
            Err(PrivacyError::UserNotFound)
        );
        assert!(events.0.lock().is_ok_and(|items| items.is_empty()));
    }

    #[tokio::test]
    async fn successful_block_and_idempotent_unblock_invalidate_both_users() {
        let store = Arc::new(FakeStore {
            target_exists: true,
            ..FakeStore::default()
        });
        let events = Arc::new(Events::default());
        let service = PrivacyService::new(store, events.clone());
        let users = [Uuid::new_v4(), Uuid::new_v4()];
        assert_eq!(service.block(users[0], users[1]).await, Ok(()));
        assert_eq!(service.unblock(users[0], users[1]).await, Ok(()));
        assert!(
            events
                .0
                .lock()
                .is_ok_and(|items| *items == vec![users, users])
        );
    }
}
