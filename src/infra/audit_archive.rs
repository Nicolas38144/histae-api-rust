use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, Months, NaiveDate, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    app::wait_for_shutdown_signal,
    config::AuditArchiveConfig,
    operations::logging::{self, SafeLogValue},
};

use super::postgres::connect_pool;

const ARCHIVE_LOCK: i64 = 86_302_004;
const BATCH: i32 = 128;
const POLL_INTERVAL: Duration = Duration::from_secs(15);

struct Entry {
    sequence_id: i64,
    source: String,
    event_id: Uuid,
    occurred_at: DateTime<Utc>,
    event: Value,
}

pub async fn run(config: AuditArchiveConfig) -> Result<(), &'static str> {
    let directory = Arc::new(config.directory);
    let metadata =
        fs::symlink_metadata(&*directory).map_err(|_| "audit_archive_directory_invalid")?;
    if !metadata.is_dir() {
        return Err("audit_archive_directory_invalid");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("audit_archive_directory_invalid");
        }
    }
    let pool = connect_pool(&config.postgres, 1, "histae-audit-archive")
        .await
        .map_err(|_| "audit_archive_database_unavailable")?;
    let result = tokio::select! {
        result = drain(&pool, directory) => result,
        result = wait_for_shutdown_signal() => result.map_err(|error| error.safe_code()),
    };
    pool.close().await;
    result
}

async fn drain(pool: &PgPool, directory: Arc<PathBuf>) -> Result<(), &'static str> {
    let mut last_prune = None;
    loop {
        let (archived, purged) = archive_batch(pool, Arc::clone(&directory)).await?;
        if archived > 0 || purged > 0 {
            let _ = logging::info(
                "audit_archive_batch",
                &[
                    ("events_archived", SafeLogValue::Unsigned(archived)),
                    ("events_purged", SafeLogValue::Unsigned(purged)),
                ],
            );
        }
        let today = Utc::now().date_naive();
        if last_prune != Some(today) {
            let directory = Arc::clone(&directory);
            tokio::task::spawn_blocking(move || prune(&directory, today))
                .await
                .map_err(|_| "audit_archive_retention_failed")?
                .map_err(|_| "audit_archive_retention_failed")?;
            last_prune = Some(today);
        }
        if archived < BATCH as u64 && purged < BATCH as u64 {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

async fn archive_batch(pool: &PgPool, directory: Arc<PathBuf>) -> Result<(u64, u64), &'static str> {
    const DB_ERROR: &str = "audit_archive_database_failed";
    let mut tx = pool.begin().await.map_err(|_| DB_ERROR)?;
    let leader: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(ARCHIVE_LOCK)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| DB_ERROR)?;
    if !leader {
        return Ok((0, 0));
    }
    let rows = sqlx::query_as::<_, (i64, String, Uuid, DateTime<Utc>, Value)>(
        "SELECT sequence_id, source, event_id, occurred_at, event
           FROM public.audit_archive_pending ORDER BY sequence_id LIMIT $1",
    )
    .bind(BATCH)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| DB_ERROR)?;
    let ids = tokio::task::spawn_blocking(move || {
        rows.into_iter()
            .map(|(sequence_id, source, event_id, occurred_at, event)| {
                let entry = Entry {
                    sequence_id,
                    source,
                    event_id,
                    occurred_at,
                    event,
                };
                persist(&directory, &entry)?;
                Ok(entry.sequence_id)
            })
            .collect::<io::Result<Vec<_>>>()
    })
    .await
    .map_err(|_| "audit_archive_write_failed")?
    .map_err(|_| "audit_archive_write_failed")?;
    if !ids.is_empty() {
        sqlx::query("DELETE FROM public.audit_archive_pending WHERE sequence_id = ANY($1)")
            .bind(&ids)
            .execute(&mut *tx)
            .await
            .map_err(|_| DB_ERROR)?;
    }
    let purged: i64 = sqlx::query_scalar("SELECT public.purge_archived_audit_events($1)")
        .bind(BATCH)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| DB_ERROR)?;
    tx.commit().await.map_err(|_| DB_ERROR)?;
    Ok((ids.len() as u64, purged as u64))
}

fn persist(root: &Path, entry: &Entry) -> io::Result<()> {
    if !matches!(
        entry.source.as_str(),
        "data_access_log" | "admin_auth_event"
    ) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let day = root.join(entry.occurred_at.format("%Y-%m-%d").to_string());
    let mut directory = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        directory.mode(0o700);
    }
    match directory.create(&day) {
        Ok(()) => fs::File::open(root)?.sync_all()?,
        Err(error)
            if error.kind() == io::ErrorKind::AlreadyExists
                && fs::symlink_metadata(&day)?.is_dir() => {}
        Err(error) => return Err(error),
    }
    let key = format!("{}-{}", entry.source, entry.event_id);
    let target = day.join(format!("{key}.json"));
    let partial = day.join(format!(".{key}.{}.tmp", Uuid::new_v4()));
    let contents = serde_json::to_vec(&entry.event).map_err(io::Error::other)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&partial)?;
    let result = (|| {
        file.write_all(&contents)?;
        file.sync_all()?;
        match fs::hard_link(&partial, &target) {
            Ok(()) => {}
            Err(error)
                if error.kind() == io::ErrorKind::AlreadyExists
                    && fs::read(&target)? == contents => {}
            Err(error) => return Err(error),
        }
        fs::File::open(&day)?.sync_all()?;
        Ok(())
    })();
    drop(file);
    fs::remove_file(partial)?;
    result
}

fn prune(root: &Path, today: NaiveDate) -> io::Result<()> {
    let cutoff = today
        .checked_sub_months(Months::new(12))
        .ok_or(io::ErrorKind::InvalidInput)?;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && entry
                .file_name()
                .to_str()
                .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
                .is_some_and(|day| day < cutoff)
        {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}
