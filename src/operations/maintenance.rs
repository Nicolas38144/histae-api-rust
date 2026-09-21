use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use crate::operations::logging;

type MaintenanceSnapshotRow = (
    String,
    String,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<i32>,
    i64,
    i32,
    bool,
    Option<String>,
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaintenanceJobName {
    Matches,
    Photos,
    Privacy,
    Outbox,
    Billing,
}

impl MaintenanceJobName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Matches => "matches",
            Self::Photos => "photos",
            Self::Privacy => "privacy",
            Self::Outbox => "outbox",
            Self::Billing => "billing",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "matches" => Some(Self::Matches),
            "photos" => Some(Self::Photos),
            "privacy" => Some(Self::Privacy),
            "outbox" => Some(Self::Outbox),
            "billing" => Some(Self::Billing),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaintenanceStatus {
    Running,
    Succeeded,
    Failed,
    Skipped,
}

impl MaintenanceStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MaintenanceProgress {
    pub processed_count: u64,
    pub batch_count: u32,
    pub work_remaining: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaintenanceSnapshot {
    pub job_name: MaintenanceJobName,
    pub status: MaintenanceStatus,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub last_succeeded_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u32>,
    pub processed_count: u64,
    pub batch_count: u32,
    pub work_remaining: bool,
    pub last_error_code: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaintenanceFinish {
    pub job_name: MaintenanceJobName,
    pub run_id: Uuid,
    pub status: MaintenanceStatus,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: u32,
    pub progress: MaintenanceProgress,
    pub error_code: Option<String>,
}

pub type MaintenanceFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), DatabaseError>> + Send + 'a>>;

pub trait MaintenanceStatusStore: Send + Sync {
    fn start<'a>(
        &'a self,
        job_name: MaintenanceJobName,
        run_id: Uuid,
        started_at: DateTime<Utc>,
    ) -> MaintenanceFuture<'a>;

    fn finish<'a>(&'a self, finish: MaintenanceFinish) -> MaintenanceFuture<'a>;
}

#[derive(Clone)]
pub struct PgMaintenanceStatusRepository {
    database: Database,
}

impl PgMaintenanceStatusRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn list(&self) -> Result<Vec<MaintenanceSnapshot>, DatabaseError> {
        let rows = sqlx::query_as::<_, MaintenanceSnapshotRow>(
            "SELECT job_name, status, started_at, finished_at, last_succeeded_at,
                    duration_ms, processed_count, batch_count, work_remaining, last_error_code
             FROM maintenance_job_status
             ORDER BY job_name",
        )
        .fetch_all(self.database.pool())
        .await
        .map_err(map_sqlx_error)?;
        rows.into_iter().map(map_snapshot).collect()
    }
}

impl MaintenanceStatusStore for PgMaintenanceStatusRepository {
    fn start<'a>(
        &'a self,
        job_name: MaintenanceJobName,
        run_id: Uuid,
        started_at: DateTime<Utc>,
    ) -> MaintenanceFuture<'a> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO maintenance_job_status (job_name, run_id, status, started_at)
                 VALUES ($1, $2, 'running', $3)
                 ON CONFLICT (job_name) DO UPDATE
                 SET run_id = EXCLUDED.run_id, status = 'running', started_at = EXCLUDED.started_at,
                     finished_at = NULL, duration_ms = NULL, processed_count = 0,
                     batch_count = 0, work_remaining = false,
                     last_error_code = NULL, updated_at = clock_timestamp()
                 WHERE maintenance_job_status.started_at <= EXCLUDED.started_at",
            )
            .bind(job_name.as_str())
            .bind(run_id)
            .bind(started_at)
            .execute(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            Ok(())
        })
    }

    fn finish<'a>(&'a self, finish: MaintenanceFinish) -> MaintenanceFuture<'a> {
        Box::pin(async move {
            let processed_count = i64::try_from(finish.progress.processed_count)
                .map_err(|_| DatabaseError::QueryFailed)?;
            let duration_ms =
                i32::try_from(finish.duration_ms).map_err(|_| DatabaseError::QueryFailed)?;
            let batch_count = i32::try_from(finish.progress.batch_count)
                .map_err(|_| DatabaseError::QueryFailed)?;
            sqlx::query(
                "UPDATE maintenance_job_status
                 SET status = $3, finished_at = $4, duration_ms = $5, processed_count = $6,
                     batch_count = $7, work_remaining = $8, last_error_code = $9,
                     last_succeeded_at = CASE WHEN $3 = 'succeeded' THEN $4 ELSE last_succeeded_at END,
                     updated_at = clock_timestamp()
                 WHERE job_name = $1 AND run_id = $2",
            )
            .bind(finish.job_name.as_str())
            .bind(finish.run_id)
            .bind(finish.status.as_str())
            .bind(finish.finished_at)
            .bind(duration_ms)
            .bind(processed_count)
            .bind(batch_count)
            .bind(finish.progress.work_remaining)
            .bind(finish.error_code)
            .execute(self.database.pool())
            .await
            .map_err(map_sqlx_error)?;
            Ok(())
        })
    }
}

#[derive(Clone)]
pub struct MaintenanceTracker {
    store: Arc<dyn MaintenanceStatusStore>,
}

impl MaintenanceTracker {
    pub fn new(store: Arc<dyn MaintenanceStatusStore>) -> Self {
        Self { store }
    }

    pub async fn track<T, E, F, P, C>(
        &self,
        job_name: MaintenanceJobName,
        work: F,
        progress: P,
        failure_code: C,
    ) -> Result<Option<T>, E>
    where
        F: Future<Output = Result<Option<T>, E>>,
        P: FnOnce(&T) -> MaintenanceProgress,
        C: FnOnce(&E) -> &'static str,
    {
        let run_id = Uuid::new_v4();
        let started_at = Utc::now();
        let timer = Instant::now();
        self.record(self.store.start(job_name, run_id, started_at))
            .await;

        match work.await {
            Ok(result) => {
                let (status, progress) = match result.as_ref() {
                    Some(value) => (MaintenanceStatus::Succeeded, progress(value)),
                    None => (MaintenanceStatus::Skipped, MaintenanceProgress::default()),
                };
                self.finish(job_name, run_id, status, timer, progress, None)
                    .await;
                Ok(result)
            }
            Err(error) => {
                let code = logging::normalized_error_code(Some(failure_code(&error)));
                self.finish(
                    job_name,
                    run_id,
                    MaintenanceStatus::Failed,
                    timer,
                    MaintenanceProgress {
                        work_remaining: true,
                        ..MaintenanceProgress::default()
                    },
                    Some(code),
                )
                .await;
                Err(error)
            }
        }
    }

    pub async fn record_failure(&self, job_name: MaintenanceJobName, failure_code: &'static str) {
        let _: Result<Option<()>, ()> = self
            .track(
                job_name,
                async { Err(()) },
                |_| MaintenanceProgress::default(),
                |_| failure_code,
            )
            .await;
    }

    async fn finish(
        &self,
        job_name: MaintenanceJobName,
        run_id: Uuid,
        status: MaintenanceStatus,
        timer: Instant,
        progress: MaintenanceProgress,
        error_code: Option<String>,
    ) {
        let duration_ms = u32::try_from(timer.elapsed().as_millis())
            .unwrap_or(u32::MAX)
            .min(86_400_000);
        self.record(self.store.finish(MaintenanceFinish {
            job_name,
            run_id,
            status,
            finished_at: Utc::now(),
            duration_ms,
            progress,
            error_code,
        }))
        .await;
    }

    async fn record(&self, operation: MaintenanceFuture<'_>) {
        if operation.await.is_err() {
            let _ = logging::warn("maintenance_status_record_failed", &[]);
        }
    }
}

fn map_snapshot(row: MaintenanceSnapshotRow) -> Result<MaintenanceSnapshot, DatabaseError> {
    let (
        job_name,
        status,
        started_at,
        finished_at,
        last_succeeded_at,
        duration_ms,
        processed_count,
        batch_count,
        work_remaining,
        last_error_code,
    ) = row;
    Ok(MaintenanceSnapshot {
        job_name: MaintenanceJobName::parse(&job_name).ok_or(DatabaseError::QueryFailed)?,
        status: MaintenanceStatus::parse(&status).ok_or(DatabaseError::QueryFailed)?,
        started_at,
        finished_at,
        last_succeeded_at,
        duration_ms: duration_ms
            .map(|value| u32::try_from(value).map_err(|_| DatabaseError::QueryFailed))
            .transpose()?,
        processed_count: u64::try_from(processed_count).map_err(|_| DatabaseError::QueryFailed)?,
        batch_count: u32::try_from(batch_count).map_err(|_| DatabaseError::QueryFailed)?,
        work_remaining,
        last_error_code,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakeStore {
        starts: Mutex<Vec<(MaintenanceJobName, Uuid)>>,
        finishes: Mutex<Vec<MaintenanceFinish>>,
        fail_writes: bool,
    }

    impl MaintenanceStatusStore for FakeStore {
        fn start<'a>(
            &'a self,
            job_name: MaintenanceJobName,
            run_id: Uuid,
            _started_at: DateTime<Utc>,
        ) -> MaintenanceFuture<'a> {
            Box::pin(async move {
                if self.fail_writes {
                    return Err(DatabaseError::QueryFailed);
                }
                self.starts
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)?
                    .push((job_name, run_id));
                Ok(())
            })
        }

        fn finish<'a>(&'a self, finish: MaintenanceFinish) -> MaintenanceFuture<'a> {
            Box::pin(async move {
                if self.fail_writes {
                    return Err(DatabaseError::QueryFailed);
                }
                self.finishes
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)?
                    .push(finish);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn records_success_skip_and_normalized_failure() {
        let store = Arc::new(FakeStore::default());
        let tracker = MaintenanceTracker::new(store.clone());

        let success = tracker
            .track(
                MaintenanceJobName::Outbox,
                async { Ok::<_, ()>(Some(7_u64)) },
                |processed| MaintenanceProgress {
                    processed_count: *processed,
                    batch_count: 2,
                    work_remaining: true,
                },
                |_| "unused",
            )
            .await;
        assert_eq!(success, Ok(Some(7)));

        let skipped = tracker
            .track(
                MaintenanceJobName::Privacy,
                async { Ok::<Option<()>, ()>(None) },
                |_| MaintenanceProgress::default(),
                |_| "unused",
            )
            .await;
        assert_eq!(skipped, Ok(None));

        let failed = tracker
            .track(
                MaintenanceJobName::Matches,
                async { Err::<Option<()>, _>(()) },
                |_| MaintenanceProgress::default(),
                |_| "PRIVATE invalid detail",
            )
            .await;
        assert_eq!(failed, Err(()));

        let finishes = store.finishes.lock().expect("test mutex should be healthy");
        assert_eq!(finishes[0].status, MaintenanceStatus::Succeeded);
        assert_eq!(finishes[0].progress.processed_count, 7);
        assert_eq!(finishes[1].status, MaintenanceStatus::Skipped);
        assert_eq!(finishes[2].status, MaintenanceStatus::Failed);
        assert_eq!(finishes[2].error_code.as_deref(), Some("operation_failed"));
        assert!(finishes[2].progress.work_remaining);
    }

    #[tokio::test]
    async fn status_storage_failure_never_breaks_the_maintenance_work() {
        let tracker = MaintenanceTracker::new(Arc::new(FakeStore {
            fail_writes: true,
            ..FakeStore::default()
        }));
        let result = tracker
            .track(
                MaintenanceJobName::Photos,
                async { Ok::<_, ()>(Some(3)) },
                |_| MaintenanceProgress::default(),
                |_| "unused",
            )
            .await;
        assert_eq!(result, Ok(Some(3)));
    }
}
