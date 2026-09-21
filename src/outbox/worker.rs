use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use futures_util::{StreamExt, stream};
use tokio::time;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::WorkloadConfig;
use crate::infra::postgres::DatabaseError;
use crate::operations::logging::{self, SafeLogValue};
use crate::operations::maintenance::{MaintenanceJobName, MaintenanceProgress, MaintenanceTracker};
use crate::outbox::pg::OutboxStore;
use crate::outbox::types::{
    ClaimWindow, DispatchFailure, DispatchOutcome, OutboxEvent, OutboxEventType,
    OutboxWorkerResult, PurgeResult, RetryResult,
};

pub const OUTBOX_LOCK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub const OUTBOX_POLL_INTERVAL: Duration = Duration::from_secs(1);
pub const OUTBOX_COMPLETED_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub const OUTBOX_PURGE_INTERVAL: Duration = Duration::from_secs(60 * 60);
pub const OUTBOX_STATUS_INTERVAL: Duration = Duration::from_secs(60);
pub const OUTBOX_BATCH_SIZE: u32 = 50;
pub const OUTBOX_HANDLER_CONCURRENCY: usize = 5;
pub const OUTBOX_MAX_ATTEMPTS: u16 = 10;
pub const OUTBOX_MAX_RETRY_DELAY: Duration = Duration::from_secs(60);

pub type DispatchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<DispatchOutcome, DispatchFailure>> + Send + 'a>>;

pub trait OutboxDispatcher: Send + Sync {
    fn dispatch<'a>(&'a self, event: &'a OutboxEvent, worker_id: Uuid) -> DispatchFuture<'a>;
}

pub trait OutboxHandler: Send + Sync {
    fn handle<'a>(&'a self, event: &'a OutboxEvent, worker_id: Uuid) -> DispatchFuture<'a>;
}

pub struct OutboxEventDispatcher {
    photo_delete: Arc<dyn OutboxHandler>,
    notification_push: Arc<dyn OutboxHandler>,
    account_erase: Arc<dyn OutboxHandler>,
    billing_subscription: Arc<dyn OutboxHandler>,
    billing_customer: Arc<dyn OutboxHandler>,
}

impl OutboxEventDispatcher {
    pub fn new(
        photo_delete: Arc<dyn OutboxHandler>,
        notification_push: Arc<dyn OutboxHandler>,
        account_erase: Arc<dyn OutboxHandler>,
        billing_subscription: Arc<dyn OutboxHandler>,
        billing_customer: Arc<dyn OutboxHandler>,
    ) -> Self {
        Self {
            photo_delete,
            notification_push,
            account_erase,
            billing_subscription,
            billing_customer,
        }
    }
}

impl OutboxDispatcher for OutboxEventDispatcher {
    fn dispatch<'a>(&'a self, event: &'a OutboxEvent, worker_id: Uuid) -> DispatchFuture<'a> {
        match &event.event_type {
            OutboxEventType::PhotoDelete => self.photo_delete.handle(event, worker_id),
            OutboxEventType::NotificationPush => self.notification_push.handle(event, worker_id),
            OutboxEventType::AccountErase => self.account_erase.handle(event, worker_id),
            OutboxEventType::BillingSubscriptionReconcile => {
                self.billing_subscription.handle(event, worker_id)
            }
            OutboxEventType::BillingCustomerReconcile => {
                self.billing_customer.handle(event, worker_id)
            }
            OutboxEventType::Unsupported(_) => {
                Box::pin(async { Err(DispatchFailure::transient("handler_failed")) })
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct OutboxWorkerConfig {
    pub batch_size: u32,
    pub handler_concurrency: usize,
    pub lock_timeout: Duration,
    pub max_attempts: u16,
    pub max_retry_delay: Duration,
    pub poll_interval: Duration,
    pub resolved_retention: Duration,
    pub purge_interval: Duration,
    pub purge_batch_size: u32,
    pub purge_max_batches: u32,
    pub status_interval: Duration,
}

impl OutboxWorkerConfig {
    pub fn from_workloads(workloads: &WorkloadConfig) -> Self {
        Self {
            batch_size: OUTBOX_BATCH_SIZE,
            handler_concurrency: OUTBOX_HANDLER_CONCURRENCY,
            lock_timeout: OUTBOX_LOCK_TIMEOUT,
            max_attempts: OUTBOX_MAX_ATTEMPTS,
            max_retry_delay: OUTBOX_MAX_RETRY_DELAY,
            poll_interval: OUTBOX_POLL_INTERVAL,
            resolved_retention: OUTBOX_COMPLETED_RETENTION,
            purge_interval: OUTBOX_PURGE_INTERVAL,
            purge_batch_size: workloads.outbox_purge_batch_size,
            purge_max_batches: workloads.outbox_purge_max_batches,
            status_interval: OUTBOX_STATUS_INTERVAL,
        }
    }
}

pub struct OutboxWorker {
    store: Arc<dyn OutboxStore>,
    dispatcher: Arc<dyn OutboxDispatcher>,
    tracker: MaintenanceTracker,
    config: OutboxWorkerConfig,
    worker_id: Uuid,
    last_purge_at: Option<DateTime<Utc>>,
    last_status_at: Option<DateTime<Utc>>,
}

impl OutboxWorker {
    pub fn new(
        store: Arc<dyn OutboxStore>,
        dispatcher: Arc<dyn OutboxDispatcher>,
        tracker: MaintenanceTracker,
        config: OutboxWorkerConfig,
    ) -> Self {
        Self {
            store,
            dispatcher,
            tracker,
            config,
            worker_id: Uuid::new_v4(),
            last_purge_at: None,
            last_status_at: None,
        }
    }

    pub fn worker_id(&self) -> Uuid {
        self.worker_id
    }

    pub async fn run_once(
        &mut self,
        now: DateTime<Utc>,
    ) -> Result<OutboxWorkerResult, DatabaseError> {
        let stale_before = subtract_duration(now, self.config.lock_timeout)?;
        let events = self
            .store
            .claim_batch(
                self.worker_id,
                ClaimWindow { now, stale_before },
                self.config.batch_size,
            )
            .await?;
        let claimed = u32::try_from(events.len()).map_err(|_| DatabaseError::QueryFailed)?;
        let mut result = OutboxWorkerResult {
            claimed,
            work_remaining: claimed == self.config.batch_size,
            ..OutboxWorkerResult::default()
        };

        let store = self.store.clone();
        let dispatcher = self.dispatcher.clone();
        let worker_id = self.worker_id;
        let max_attempts = self.config.max_attempts;
        let max_retry_delay = self.config.max_retry_delay;
        let outcomes = stream::iter(events)
            .map(|event| {
                let store = store.clone();
                let dispatcher = dispatcher.clone();
                async move {
                    process_event(
                        store,
                        dispatcher,
                        event,
                        worker_id,
                        max_attempts,
                        max_retry_delay,
                    )
                    .await
                }
            })
            .buffer_unordered(self.config.handler_concurrency.max(1))
            .collect::<Vec<_>>()
            .await;

        let mut first_error = None;
        for outcome in outcomes {
            match outcome {
                Ok(ProcessResult::Completed) => result.completed += 1,
                Ok(ProcessResult::Deferred) => result.deferred += 1,
                Ok(ProcessResult::Retried) => result.retried += 1,
                Ok(ProcessResult::DeadLettered) => result.dead_lettered += 1,
                Ok(ProcessResult::NoChange) => {}
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }

        if self.purge_is_due(now)? {
            let cutoff = subtract_duration(now, self.config.resolved_retention)?;
            let purge = self.purge_resolved(cutoff).await?;
            result.purged = purge.purged;
            result.purge_batches = purge.batches;
            result.work_remaining |= purge.work_remaining;
            self.last_purge_at = Some(now);
        }
        Ok(result)
    }

    pub async fn run_until_cancelled(&mut self, cancellation: CancellationToken) {
        while !cancellation.is_cancelled() {
            self.poll_once(Utc::now()).await;
            tokio::select! {
                () = cancellation.cancelled() => break,
                () = time::sleep(self.config.poll_interval) => {}
            }
        }
    }

    async fn poll_once(&mut self, now: DateTime<Utc>) {
        let should_record = match self.last_status_at {
            Some(last) => elapsed_at_least(last, now, self.config.status_interval).unwrap_or(true),
            None => true,
        };
        let outcome = if should_record {
            let tracker = self.tracker.clone();
            let tracked = tracker
                .track(
                    MaintenanceJobName::Outbox,
                    async { self.run_once(now).await.map(Some) },
                    |result| MaintenanceProgress {
                        processed_count: u64::from(result.claimed) + result.purged,
                        batch_count: 1_u32.saturating_add(result.purge_batches),
                        work_remaining: result.work_remaining,
                    },
                    |error: &DatabaseError| (*error).safe_code(),
                )
                .await;
            if tracked.is_ok() {
                self.last_status_at = Some(now);
            }
            tracked.map(|_| ())
        } else {
            let result = self.run_once(now).await;
            if let Err(error) = result {
                self.tracker
                    .record_failure(MaintenanceJobName::Outbox, error.safe_code())
                    .await;
                self.last_status_at = Some(now);
                Err(error)
            } else {
                Ok(())
            }
        };
        if outcome.is_err() {
            let _ = logging::error("outbox_poll_failed", None);
        }
    }

    fn purge_is_due(&self, now: DateTime<Utc>) -> Result<bool, DatabaseError> {
        self.last_purge_at
            .map(|last| elapsed_at_least(last, now, self.config.purge_interval))
            .transpose()
            .map(|due| due.unwrap_or(true))
    }

    async fn purge_resolved(&self, before: DateTime<Utc>) -> Result<PurgeResult, DatabaseError> {
        let mut purged = 0_u64;
        let mut batches = 0_u32;
        let mut last_batch_size = 0_u64;
        while batches < self.config.purge_max_batches {
            last_batch_size = self
                .store
                .purge_resolved(before, self.config.purge_batch_size)
                .await?;
            purged = purged.saturating_add(last_batch_size);
            batches += 1;
            if last_batch_size < u64::from(self.config.purge_batch_size) {
                return Ok(PurgeResult {
                    purged,
                    batches,
                    work_remaining: false,
                });
            }
        }
        let _ = logging::warn(
            "outbox_completed_purge_batch_limit",
            &[("batches", SafeLogValue::Unsigned(u64::from(batches)))],
        );
        Ok(PurgeResult {
            purged,
            batches,
            work_remaining: last_batch_size >= u64::from(self.config.purge_batch_size),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessResult {
    Completed,
    Deferred,
    Retried,
    DeadLettered,
    NoChange,
}

async fn process_event(
    store: Arc<dyn OutboxStore>,
    dispatcher: Arc<dyn OutboxDispatcher>,
    event: OutboxEvent,
    worker_id: Uuid,
    max_attempts: u16,
    max_retry_delay: Duration,
) -> Result<ProcessResult, DatabaseError> {
    let operation: Result<ProcessResult, DispatchFailure> = async {
        let renewed = store
            .renew_claim(event.id, worker_id)
            .await
            .map_err(|_| DispatchFailure::transient("handler_failed"))?;
        if !renewed {
            return Ok(ProcessResult::NoChange);
        }
        match dispatcher.dispatch(&event, worker_id).await? {
            DispatchOutcome::Deferred => Ok(ProcessResult::Deferred),
            DispatchOutcome::Completed => {
                let completed = store
                    .complete(event.id, worker_id, Utc::now())
                    .await
                    .map_err(|_| DispatchFailure::transient("handler_failed"))?;
                Ok(if completed {
                    ProcessResult::Completed
                } else {
                    ProcessResult::NoChange
                })
            }
        }
    }
    .await;

    let failure = match operation {
        Ok(result) => return Ok(result),
        Err(failure) => failure,
    };
    let retry_at = Utc::now()
        + TimeDelta::milliseconds(
            i64::try_from(retry_delay(event.attempts, max_retry_delay).as_millis())
                .map_err(|_| DatabaseError::QueryFailed)?,
        );
    let retry = store
        .reschedule(
            event.id,
            worker_id,
            retry_at,
            failure.code,
            if failure.permanent { 1 } else { max_attempts },
        )
        .await?;
    match retry {
        RetryResult::Pending => Ok(ProcessResult::Retried),
        RetryResult::NotOwned => Ok(ProcessResult::NoChange),
        RetryResult::DeadLetter => {
            let event_id = event.id.to_string();
            let _ = logging::error_with_fields(
                "outbox_event_dead_lettered",
                &[
                    ("event_id", SafeLogValue::String(&event_id)),
                    (
                        "event_type",
                        SafeLogValue::String(event.event_type.as_str()),
                    ),
                ],
            );
            Ok(ProcessResult::DeadLettered)
        }
    }
}

fn retry_delay(attempts: u16, maximum: Duration) -> Duration {
    let exponent = u32::from(attempts.saturating_sub(1)).min(63);
    let seconds = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
    Duration::from_secs(seconds).min(maximum)
}

fn subtract_duration(
    value: DateTime<Utc>,
    duration: Duration,
) -> Result<DateTime<Utc>, DatabaseError> {
    let delta = TimeDelta::from_std(duration).map_err(|_| DatabaseError::QueryFailed)?;
    value
        .checked_sub_signed(delta)
        .ok_or(DatabaseError::QueryFailed)
}

fn elapsed_at_least(
    earlier: DateTime<Utc>,
    later: DateTime<Utc>,
    duration: Duration,
) -> Result<bool, DatabaseError> {
    let delta = TimeDelta::from_std(duration).map_err(|_| DatabaseError::QueryFailed)?;
    Ok(later.signed_duration_since(earlier) >= delta)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use crate::operations::maintenance::{MaintenanceFuture, MaintenanceStatusStore};
    use crate::outbox::pg::OutboxFuture;
    use crate::outbox::types::OutboxStatus;

    #[derive(Default)]
    struct NullMaintenanceStore;

    impl MaintenanceStatusStore for NullMaintenanceStore {
        fn start<'a>(
            &'a self,
            _job_name: MaintenanceJobName,
            _run_id: Uuid,
            _started_at: DateTime<Utc>,
        ) -> MaintenanceFuture<'a> {
            Box::pin(async { Ok(()) })
        }

        fn finish<'a>(
            &'a self,
            _finish: crate::operations::maintenance::MaintenanceFinish,
        ) -> MaintenanceFuture<'a> {
            Box::pin(async { Ok(()) })
        }
    }

    struct FakeStore {
        events: Mutex<Vec<OutboxEvent>>,
        renew: AtomicBool,
        complete: AtomicBool,
        complete_fails: AtomicBool,
        retry: Mutex<RetryResult>,
        purge: Mutex<VecDeque<u64>>,
        claim_calls: AtomicUsize,
        complete_calls: AtomicUsize,
        reschedule_calls: AtomicUsize,
        last_max_attempts: AtomicUsize,
        last_error_code: Mutex<Option<&'static str>>,
    }

    impl FakeStore {
        fn new(events: Vec<OutboxEvent>) -> Self {
            Self {
                events: Mutex::new(events),
                renew: AtomicBool::new(true),
                complete: AtomicBool::new(true),
                complete_fails: AtomicBool::new(false),
                retry: Mutex::new(RetryResult::Pending),
                purge: Mutex::new(VecDeque::from([0])),
                claim_calls: AtomicUsize::new(0),
                complete_calls: AtomicUsize::new(0),
                reschedule_calls: AtomicUsize::new(0),
                last_max_attempts: AtomicUsize::new(0),
                last_error_code: Mutex::new(None),
            }
        }
    }

    impl OutboxStore for FakeStore {
        fn claim_batch<'a>(
            &'a self,
            _worker_id: Uuid,
            _window: ClaimWindow,
            _limit: u32,
        ) -> OutboxFuture<'a, Vec<OutboxEvent>> {
            Box::pin(async move {
                self.claim_calls.fetch_add(1, Ordering::SeqCst);
                let mut events = self.events.lock().map_err(|_| DatabaseError::QueryFailed)?;
                Ok(std::mem::take(&mut *events))
            })
        }

        fn renew_claim<'a>(&'a self, _event_id: Uuid, _worker_id: Uuid) -> OutboxFuture<'a, bool> {
            Box::pin(async move { Ok(self.renew.load(Ordering::SeqCst)) })
        }

        fn complete<'a>(
            &'a self,
            _event_id: Uuid,
            _worker_id: Uuid,
            _processed_at: DateTime<Utc>,
        ) -> OutboxFuture<'a, bool> {
            Box::pin(async move {
                self.complete_calls.fetch_add(1, Ordering::SeqCst);
                if self.complete_fails.load(Ordering::SeqCst) {
                    Err(DatabaseError::ConnectionFailed)
                } else {
                    Ok(self.complete.load(Ordering::SeqCst))
                }
            })
        }

        fn reschedule<'a>(
            &'a self,
            _event_id: Uuid,
            _worker_id: Uuid,
            _available_at: DateTime<Utc>,
            error_code: &'static str,
            max_attempts: u16,
        ) -> OutboxFuture<'a, RetryResult> {
            Box::pin(async move {
                self.reschedule_calls.fetch_add(1, Ordering::SeqCst);
                self.last_max_attempts
                    .store(usize::from(max_attempts), Ordering::SeqCst);
                *self
                    .last_error_code
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)? = Some(error_code);
                self.retry
                    .lock()
                    .map(|retry| *retry)
                    .map_err(|_| DatabaseError::QueryFailed)
            })
        }

        fn purge_resolved<'a>(
            &'a self,
            _before: DateTime<Utc>,
            _limit: u32,
        ) -> OutboxFuture<'a, u64> {
            Box::pin(async move {
                self.purge
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)
                    .map(|mut values| values.pop_front().unwrap_or(0))
            })
        }
    }

    #[derive(Clone, Copy)]
    enum FakeDispatch {
        Complete,
        Deferred,
        TransientFailure,
        PermanentFailure,
    }

    struct FakeDispatcher {
        mode: FakeDispatch,
        delay: Duration,
        calls: AtomicUsize,
        active: AtomicUsize,
        maximum_active: AtomicUsize,
    }

    impl FakeDispatcher {
        fn new(mode: FakeDispatch) -> Self {
            Self {
                mode,
                delay: Duration::ZERO,
                calls: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                maximum_active: AtomicUsize::new(0),
            }
        }

        fn delayed(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }
    }

    impl OutboxDispatcher for FakeDispatcher {
        fn dispatch<'a>(&'a self, _event: &'a OutboxEvent, _worker_id: Uuid) -> DispatchFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.maximum_active.fetch_max(active, Ordering::SeqCst);
                if !self.delay.is_zero() {
                    time::sleep(self.delay).await;
                }
                self.active.fetch_sub(1, Ordering::SeqCst);
                match self.mode {
                    FakeDispatch::Complete => Ok(DispatchOutcome::Completed),
                    FakeDispatch::Deferred => Ok(DispatchOutcome::Deferred),
                    FakeDispatch::TransientFailure => {
                        Err(DispatchFailure::transient("handler_failed"))
                    }
                    FakeDispatch::PermanentFailure => {
                        Err(DispatchFailure::permanent("mapping_conflict"))
                    }
                }
            })
        }
    }

    #[derive(Default)]
    struct CountingHandler {
        calls: AtomicUsize,
    }

    impl OutboxHandler for CountingHandler {
        fn handle<'a>(&'a self, _event: &'a OutboxEvent, _worker_id: Uuid) -> DispatchFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(DispatchOutcome::Completed)
            })
        }
    }

    fn event() -> OutboxEvent {
        OutboxEvent {
            id: Uuid::new_v4(),
            event_type: OutboxEventType::PhotoDelete,
            aggregate_id: Uuid::new_v4(),
            payload: serde_json::Map::new(),
            status: OutboxStatus::Processing,
            attempts: 1,
        }
    }

    fn config() -> OutboxWorkerConfig {
        OutboxWorkerConfig {
            batch_size: OUTBOX_BATCH_SIZE,
            handler_concurrency: OUTBOX_HANDLER_CONCURRENCY,
            lock_timeout: OUTBOX_LOCK_TIMEOUT,
            max_attempts: OUTBOX_MAX_ATTEMPTS,
            max_retry_delay: OUTBOX_MAX_RETRY_DELAY,
            poll_interval: Duration::from_millis(1),
            resolved_retention: OUTBOX_COMPLETED_RETENTION,
            purge_interval: OUTBOX_PURGE_INTERVAL,
            purge_batch_size: 500,
            purge_max_batches: 20,
            status_interval: OUTBOX_STATUS_INTERVAL,
        }
    }

    fn worker(
        store: Arc<FakeStore>,
        dispatcher: Arc<FakeDispatcher>,
        config: OutboxWorkerConfig,
    ) -> OutboxWorker {
        OutboxWorker::new(
            store,
            dispatcher,
            MaintenanceTracker::new(Arc::new(NullMaintenanceStore)),
            config,
        )
    }

    #[tokio::test]
    async fn acknowledges_only_after_a_successful_dispatch_and_renews_first() {
        let store = Arc::new(FakeStore::new(vec![event()]));
        let dispatcher = Arc::new(FakeDispatcher::new(FakeDispatch::Complete));
        let result = worker(store.clone(), dispatcher.clone(), config())
            .run_once(Utc::now())
            .await
            .expect("fake store should succeed");
        assert_eq!(result.completed, 1);
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.complete_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn routes_only_known_types_and_never_acknowledges_an_unknown_type() {
        let photo = Arc::new(CountingHandler::default());
        let push = Arc::new(CountingHandler::default());
        let erasure = Arc::new(CountingHandler::default());
        let subscription = Arc::new(CountingHandler::default());
        let customer = Arc::new(CountingHandler::default());
        let dispatcher = OutboxEventDispatcher::new(
            photo.clone(),
            push.clone(),
            erasure.clone(),
            subscription.clone(),
            customer.clone(),
        );
        let mut known = event();
        known.event_type = OutboxEventType::BillingCustomerReconcile;
        assert_eq!(
            dispatcher.dispatch(&known, Uuid::new_v4()).await,
            Ok(DispatchOutcome::Completed)
        );
        assert_eq!(customer.calls.load(Ordering::SeqCst), 1);
        assert_eq!(photo.calls.load(Ordering::SeqCst), 0);
        assert_eq!(push.calls.load(Ordering::SeqCst), 0);
        assert_eq!(erasure.calls.load(Ordering::SeqCst), 0);
        assert_eq!(subscription.calls.load(Ordering::SeqCst), 0);

        known.event_type = OutboxEventType::Unsupported("future.effect".to_owned());
        assert_eq!(
            dispatcher.dispatch(&known, Uuid::new_v4()).await,
            Err(DispatchFailure::transient("handler_failed"))
        );
        assert_eq!(customer.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn never_dispatches_a_claim_that_is_no_longer_owned() {
        let store = Arc::new(FakeStore::new(vec![event()]));
        store.renew.store(false, Ordering::SeqCst);
        let dispatcher = Arc::new(FakeDispatcher::new(FakeDispatch::Complete));
        let result = worker(store.clone(), dispatcher.clone(), config())
            .run_once(Utc::now())
            .await
            .expect("fake store should succeed");
        assert_eq!(result.completed, 0);
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 0);
        assert_eq!(store.complete_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn preserves_deferred_work_and_retries_acknowledgement_failures() {
        let deferred_store = Arc::new(FakeStore::new(vec![event()]));
        let deferred = worker(
            deferred_store.clone(),
            Arc::new(FakeDispatcher::new(FakeDispatch::Deferred)),
            config(),
        )
        .run_once(Utc::now())
        .await
        .expect("fake store should succeed");
        assert_eq!(deferred.deferred, 1);
        assert_eq!(deferred_store.complete_calls.load(Ordering::SeqCst), 0);
        assert_eq!(deferred_store.reschedule_calls.load(Ordering::SeqCst), 0);

        let retry_store = Arc::new(FakeStore::new(vec![event()]));
        retry_store.complete_fails.store(true, Ordering::SeqCst);
        let retried = worker(
            retry_store.clone(),
            Arc::new(FakeDispatcher::new(FakeDispatch::Complete)),
            config(),
        )
        .run_once(Utc::now())
        .await
        .expect("reschedule should recover the failed acknowledgement");
        assert_eq!(retried.retried, 1);
        assert_eq!(retry_store.reschedule_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn distinguishes_transient_retry_from_immediate_dead_letter() {
        let transient_store = Arc::new(FakeStore::new(vec![event()]));
        let transient = worker(
            transient_store.clone(),
            Arc::new(FakeDispatcher::new(FakeDispatch::TransientFailure)),
            config(),
        )
        .run_once(Utc::now())
        .await
        .expect("transient retry should succeed");
        assert_eq!(transient.retried, 1);
        assert_eq!(transient_store.last_max_attempts.load(Ordering::SeqCst), 10);
        assert_eq!(
            *transient_store
                .last_error_code
                .lock()
                .expect("test mutex should be healthy"),
            Some("handler_failed")
        );

        let permanent_store = Arc::new(FakeStore::new(vec![event()]));
        *permanent_store
            .retry
            .lock()
            .expect("test mutex should be healthy") = RetryResult::DeadLetter;
        let permanent = worker(
            permanent_store.clone(),
            Arc::new(FakeDispatcher::new(FakeDispatch::PermanentFailure)),
            config(),
        )
        .run_once(Utc::now())
        .await
        .expect("dead-letter transition should succeed");
        assert_eq!(permanent.dead_lettered, 1);
        assert_eq!(permanent_store.last_max_attempts.load(Ordering::SeqCst), 1);
        assert_eq!(
            *permanent_store
                .last_error_code
                .lock()
                .expect("test mutex should be healthy"),
            Some("mapping_conflict")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bounds_handler_concurrency_to_five() {
        let store = Arc::new(FakeStore::new((0..10).map(|_| event()).collect()));
        let dispatcher = Arc::new(
            FakeDispatcher::new(FakeDispatch::Complete).delayed(Duration::from_millis(10)),
        );
        let result = worker(store, dispatcher.clone(), config())
            .run_once(Utc::now())
            .await
            .expect("fake store should succeed");
        assert_eq!(result.completed, 10);
        assert_eq!(dispatcher.maximum_active.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn bounds_resolved_event_purge_and_reports_remaining_work() {
        let store = Arc::new(FakeStore::new(Vec::new()));
        *store.purge.lock().expect("test mutex should be healthy") = VecDeque::from([500, 500]);
        let mut worker_config = config();
        worker_config.purge_max_batches = 2;
        let result = worker(
            store,
            Arc::new(FakeDispatcher::new(FakeDispatch::Complete)),
            worker_config,
        )
        .run_once(Utc::now())
        .await
        .expect("fake store should succeed");
        assert_eq!(result.purged, 1_000);
        assert_eq!(result.purge_batches, 2);
        assert!(result.work_remaining);
    }

    #[tokio::test]
    async fn an_already_cancelled_worker_does_not_claim_work() {
        let store = Arc::new(FakeStore::new(vec![event()]));
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        worker(
            store.clone(),
            Arc::new(FakeDispatcher::new(FakeDispatch::Complete)),
            config(),
        )
        .run_until_cancelled(cancellation)
        .await;
        assert_eq!(store.claim_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn retry_backoff_matches_nest_and_caps_at_sixty_seconds() {
        assert_eq!(
            retry_delay(1, OUTBOX_MAX_RETRY_DELAY),
            Duration::from_secs(1)
        );
        assert_eq!(
            retry_delay(2, OUTBOX_MAX_RETRY_DELAY),
            Duration::from_secs(2)
        );
        assert_eq!(
            retry_delay(10, OUTBOX_MAX_RETRY_DELAY),
            Duration::from_secs(60)
        );
        assert_eq!(
            retry_delay(u16::MAX, OUTBOX_MAX_RETRY_DELAY),
            Duration::from_secs(60)
        );
    }
}
