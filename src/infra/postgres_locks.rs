use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, PgPool, Postgres};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval};
use uuid::Uuid;

use crate::config::PostgresConfig;

use super::postgres::{DatabaseError, PoolStats, connect_pool};

const ACCOUNT_ACTIVITY_POOL_MAX: u32 = 4;
const ACCOUNT_ACTIVITY_APPLICATION_NAME: &str = "histae-account-activity";
const ACCOUNT_ACTIVITY_HASH_SEED: i64 = 13_092_026;
const ACTIVITY_HEALTH_INTERVAL: Duration = Duration::from_millis(100);

pub const MATCH_MAINTENANCE_LOCK: i64 = 37_142_581;

const LEASE_HELD: u8 = 0;
const LEASE_LOST: u8 = 1;
const LEASE_RELEASED: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountActivityError {
    AccountUnavailable,
    ActivityUnavailable,
    Database(DatabaseError),
}

impl AccountActivityError {
    pub fn safe_code(self) -> &'static str {
        match self {
            Self::AccountUnavailable => "account_unavailable",
            Self::ActivityUnavailable => "account_activity_unavailable",
            Self::Database(error) => error.safe_code(),
        }
    }
}

impl fmt::Display for AccountActivityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.safe_code())
    }
}

impl std::error::Error for AccountActivityError {}

impl From<DatabaseError> for AccountActivityError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TryExclusive<T> {
    Acquired(T),
    NotAcquired,
}

pub type ActivityWorkFuture<'lease, T, E> =
    Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'lease>>;

/// A synchronous proof checked immediately before an external side effect.
///
/// The dedicated connection is probed while work runs. Once a probe observes
/// a broken PostgreSQL session, this lease can never become held again.
pub struct ActivityLease {
    state: Arc<AtomicU8>,
    backend_process_id: i32,
}

impl ActivityLease {
    pub fn assert_held(&self) -> Result<(), AccountActivityError> {
        if self.state.load(Ordering::Acquire) == LEASE_HELD {
            Ok(())
        } else {
            Err(AccountActivityError::ActivityUnavailable)
        }
    }

    #[doc(hidden)]
    pub fn backend_process_id(&self) -> i32 {
        self.backend_process_id
    }
}

#[derive(Clone)]
pub struct AccountActivityPool {
    pool: PgPool,
    waiting: Arc<AtomicU32>,
}

impl AccountActivityPool {
    pub async fn connect(config: &PostgresConfig) -> Result<Self, AccountActivityError> {
        let pool = connect_pool(
            config,
            ACCOUNT_ACTIVITY_POOL_MAX,
            ACCOUNT_ACTIVITY_APPLICATION_NAME,
        )
        .await
        .map_err(AccountActivityError::Database)?;
        sqlx::query("SELECT 1")
            .execute(&pool)
            .await
            .map_err(|_| AccountActivityError::ActivityUnavailable)?;
        Ok(Self {
            pool,
            waiting: Arc::new(AtomicU32::new(0)),
        })
    }

    pub async fn run<T, E, F>(&self, user_ids: &[Uuid], work: F) -> Result<T, E>
    where
        T: Send,
        E: From<AccountActivityError>,
        F: for<'lease> FnOnce(&'lease ActivityLease) -> ActivityWorkFuture<'lease, T, E>,
    {
        self.run_shared(user_ids, false, work).await
    }

    /// Maintenance still protects banned accounts, but never erased accounts.
    pub async fn run_existing<T, E, F>(&self, user_ids: &[Uuid], work: F) -> Result<T, E>
    where
        T: Send,
        E: From<AccountActivityError>,
        F: for<'lease> FnOnce(&'lease ActivityLease) -> ActivityWorkFuture<'lease, T, E>,
    {
        self.run_shared(user_ids, true, work).await
    }

    pub async fn try_exclusive<T, E, F>(&self, user_id: Uuid, work: F) -> Result<TryExclusive<T>, E>
    where
        T: Send,
        E: From<AccountActivityError>,
        F: for<'lease> FnOnce(&'lease ActivityLease) -> ActivityWorkFuture<'lease, T, E>,
    {
        let Some(owner) = self
            .acquire(&[user_id], LockMode::Exclusive, None, None)
            .await
            .map_err(E::from)?
        else {
            return Ok(TryExclusive::NotAcquired);
        };
        let result = run_guarded(owner.lease(), work).await;
        owner.release().await;
        result.map(TryExclusive::Acquired)
    }

    /// Serialize one kind of operation without blocking the account's other
    /// shared activities. Both locks belong to the monitored PostgreSQL session.
    pub async fn try_serialized<T, E, F>(
        &self,
        user_id: Uuid,
        operation: &'static str,
        work: F,
    ) -> Result<TryExclusive<T>, E>
    where
        T: Send,
        E: From<AccountActivityError>,
        F: for<'lease> FnOnce(&'lease ActivityLease) -> ActivityWorkFuture<'lease, T, E>,
    {
        let Some(owner) = self
            .acquire(&[user_id], LockMode::Shared, Some(false), Some(operation))
            .await
            .map_err(E::from)?
        else {
            return Ok(TryExclusive::NotAcquired);
        };
        let result = run_guarded(owner.lease(), work).await;
        owner.release().await;
        result.map(TryExclusive::Acquired)
    }

    pub fn pool_stats(&self) -> PoolStats {
        PoolStats {
            total: self.pool.size(),
            idle: u32::try_from(self.pool.num_idle()).unwrap_or(u32::MAX),
            waiting: self.waiting.load(Ordering::Relaxed),
            max: ACCOUNT_ACTIVITY_POOL_MAX,
        }
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    async fn run_shared<T, E, F>(
        &self,
        user_ids: &[Uuid],
        allow_banned: bool,
        work: F,
    ) -> Result<T, E>
    where
        T: Send,
        E: From<AccountActivityError>,
        F: for<'lease> FnOnce(&'lease ActivityLease) -> ActivityWorkFuture<'lease, T, E>,
    {
        let owner = self
            .acquire(user_ids, LockMode::Shared, Some(allow_banned), None)
            .await
            .map_err(E::from)?
            .ok_or_else(|| E::from(AccountActivityError::AccountUnavailable))?;
        let result = run_guarded(owner.lease(), work).await;
        owner.release().await;
        result
    }

    async fn acquire(
        &self,
        user_ids: &[Uuid],
        mode: LockMode,
        allow_banned: Option<bool>,
        operation: Option<&'static str>,
    ) -> Result<Option<ActivityOwner>, AccountActivityError> {
        let account_ids = canonical_ids(user_ids);
        let waiting = ActivityAcquisition::new(&self.waiting);
        let connection = self.pool.acquire().await;
        drop(waiting);
        let connection = connection.map_err(|_| AccountActivityError::ActivityUnavailable)?;
        let mut session = CheckedOutSession::new(connection);

        for account_id in &account_ids {
            let statement = match mode {
                LockMode::Shared => "SELECT pg_try_advisory_lock_shared(hashtextextended($1, $2))",
                LockMode::Exclusive => "SELECT pg_try_advisory_lock(hashtextextended($1, $2))",
            };
            let acquired: bool = sqlx::query_scalar(statement)
                .bind(account_id.to_string())
                .bind(ACCOUNT_ACTIVITY_HASH_SEED)
                .fetch_one(session.connection_mut())
                .await
                .map_err(|_| AccountActivityError::ActivityUnavailable)?;
            if !acquired {
                let _ = unlock_all(&mut session).await;
                return match mode {
                    LockMode::Shared => Err(AccountActivityError::AccountUnavailable),
                    LockMode::Exclusive => Ok(None),
                };
            }
        }

        if let Some(allow_banned) = allow_banned {
            let account_count: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM user_account
                 WHERE user_id = ANY($1::uuid[])
                   AND deleted_at IS NULL AND ($2 OR NOT is_banned)",
            )
            .bind(&account_ids)
            .bind(allow_banned)
            .fetch_one(session.connection_mut())
            .await
            .map_err(|_| AccountActivityError::ActivityUnavailable)?;
            if account_count != i64::try_from(account_ids.len()).unwrap_or(i64::MAX) {
                let _ = unlock_all(&mut session).await;
                return Err(AccountActivityError::AccountUnavailable);
            }
        }

        if let Some(operation) = operation {
            for account_id in &account_ids {
                let acquired: bool =
                    sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1, $2))")
                        .bind(format!("{operation}:{account_id}"))
                        .bind(ACCOUNT_ACTIVITY_HASH_SEED)
                        .fetch_one(session.connection_mut())
                        .await
                        .map_err(|_| AccountActivityError::ActivityUnavailable)?;
                if !acquired {
                    let _ = unlock_all(&mut session).await;
                    return Ok(None);
                }
            }
        }
        let backend_process_id: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(session.connection_mut())
            .await
            .map_err(|_| AccountActivityError::ActivityUnavailable)?;
        Ok(Some(ActivityOwner::start(session, backend_process_id)))
    }
}

struct ActivityAcquisition<'counter> {
    counter: &'counter AtomicU32,
}

impl<'counter> ActivityAcquisition<'counter> {
    fn new(counter: &'counter AtomicU32) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for ActivityAcquisition<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn run_guarded<T, E, F>(lease: &ActivityLease, work: F) -> Result<T, E>
where
    T: Send,
    E: From<AccountActivityError>,
    F: for<'scope> FnOnce(&'scope ActivityLease) -> ActivityWorkFuture<'scope, T, E>,
{
    let result = work(lease).await;
    match result {
        Ok(value) => {
            lease.assert_held().map_err(E::from)?;
            Ok(value)
        }
        Err(error) => Err(error),
    }
}

#[derive(Clone, Copy)]
enum LockMode {
    Shared,
    Exclusive,
}

fn canonical_ids(user_ids: &[Uuid]) -> Vec<Uuid> {
    let mut ids = user_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    ids
}

enum ActivityCommand {
    Release(oneshot::Sender<()>),
}

struct ActivityOwner {
    lease: ActivityLease,
    commands: Option<mpsc::Sender<ActivityCommand>>,
    task: Option<JoinHandle<()>>,
}

impl ActivityOwner {
    fn start(session: CheckedOutSession, backend_process_id: i32) -> Self {
        let state = Arc::new(AtomicU8::new(LEASE_HELD));
        let (commands, receiver) = mpsc::channel(1);
        let task_state = Arc::clone(&state);
        let task = tokio::spawn(activity_session_owner(session, task_state, receiver));
        Self {
            lease: ActivityLease {
                state,
                backend_process_id,
            },
            commands: Some(commands),
            task: Some(task),
        }
    }

    fn lease(&self) -> &ActivityLease {
        &self.lease
    }

    async fn release(mut self) {
        let Some(commands) = self.commands.take() else {
            return;
        };
        let (completed, receiver) = oneshot::channel();
        if commands
            .send(ActivityCommand::Release(completed))
            .await
            .is_ok()
        {
            let _ = receiver.await;
        }
        drop(commands);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for ActivityOwner {
    fn drop(&mut self) {
        self.commands.take();
        // Dropping the last sender wakes the detached owner. It unlocks the
        // session, or destroys the connection if that cannot be proven.
        self.task.take();
    }
}

async fn activity_session_owner(
    mut session: CheckedOutSession,
    state: Arc<AtomicU8>,
    mut commands: mpsc::Receiver<ActivityCommand>,
) {
    let mut health = interval(ACTIVITY_HEALTH_INTERVAL);
    health.set_missed_tick_behavior(MissedTickBehavior::Delay);
    health.tick().await;

    loop {
        tokio::select! {
            command = commands.recv() => {
                match command {
                    Some(ActivityCommand::Release(completed)) => {
                        if unlock_all(&mut session).await.is_ok() {
                            state.store(LEASE_RELEASED, Ordering::Release);
                        } else {
                            state.store(LEASE_LOST, Ordering::Release);
                            tracing::warn!(event_code = "account_activity_session_cleanup_failed");
                        }
                        let _ = completed.send(());
                    }
                    None => {
                        if unlock_all(&mut session).await.is_err() {
                            state.store(LEASE_LOST, Ordering::Release);
                            tracing::warn!(event_code = "account_activity_session_cleanup_failed");
                        } else {
                            state.store(LEASE_RELEASED, Ordering::Release);
                        }
                    }
                }
                return;
            }
            _ = health.tick() => {
                if sqlx::query("SELECT 1").execute(session.connection_mut()).await.is_err() {
                    state.store(LEASE_LOST, Ordering::Release);
                    tracing::warn!(event_code = "account_activity_session_lost");
                    return;
                }
            }
        }
    }
}

async fn unlock_all(session: &mut CheckedOutSession) -> Result<(), AccountActivityError> {
    sqlx::query("SELECT pg_advisory_unlock_all()")
        .execute(session.connection_mut())
        .await
        .map_err(|_| AccountActivityError::ActivityUnavailable)?;
    session.mark_safe_for_pool();
    Ok(())
}

struct CheckedOutSession {
    connection: PoolConnection<Postgres>,
    safe_for_pool: bool,
}

impl CheckedOutSession {
    fn new(connection: PoolConnection<Postgres>) -> Self {
        Self {
            connection,
            safe_for_pool: false,
        }
    }

    fn connection_mut(&mut self) -> &mut PgConnection {
        &mut self.connection
    }

    fn mark_safe_for_pool(&mut self) {
        self.safe_for_pool = true;
    }
}

impl Drop for CheckedOutSession {
    fn drop(&mut self) {
        if !self.safe_for_pool {
            self.connection.close_on_drop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_uuid_identity_and_lock_order() {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut expected = vec![first, second];
        expected.sort_unstable();
        assert_eq!(canonical_ids(&[second, first, first]), expected);
    }

    #[test]
    fn exposes_stable_public_error_contract() {
        assert_eq!(
            AccountActivityError::AccountUnavailable.safe_code(),
            "account_unavailable"
        );
        assert_eq!(
            AccountActivityError::ActivityUnavailable.safe_code(),
            "account_activity_unavailable"
        );
        assert_eq!(MATCH_MAINTENANCE_LOCK, 37_142_581);
    }
}
