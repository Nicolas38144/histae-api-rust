use std::fs::{DirBuilder, OpenOptions};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Bytes;
use chrono::{DateTime, SecondsFormat, Utc};
use futures_util::Stream;
use futures_util::task::AtomicWaker;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::fs::File;
use tokio::io::{AsyncSeekExt as _, AsyncWriteExt as _, SeekFrom};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
use tokio::task::AbortHandle;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::infra::postgres::DatabaseError;
use crate::privacy::rights::DataRightsStore;
use crate::profiles::service::ProfilePhotoUrlProvider;

pub const EXPORT_FILENAME: &str = "histae-data-export.json";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug)]
pub enum ExportBuildError {
    TooLarge,
    Io(io::Error),
    Database(DatabaseError),
}

impl From<io::Error> for ExportBuildError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<DatabaseError> for ExportBuildError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

pub struct JsonExportWriter {
    file: File,
    max_bytes: u64,
    written_bytes: u64,
    contexts: Vec<JsonContext>,
}

#[derive(Clone, Copy)]
struct JsonContext {
    kind: JsonContextKind,
    first: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum JsonContextKind {
    Array,
    Object,
}

impl JsonExportWriter {
    pub fn new(file: File, max_bytes: u64) -> Self {
        Self {
            file,
            max_bytes,
            written_bytes: 0,
            contexts: Vec::new(),
        }
    }

    pub async fn start_object(&mut self, name: Option<&str>) -> Result<(), ExportBuildError> {
        self.before_value(name).await?;
        self.write(b"{").await?;
        self.contexts.push(JsonContext {
            kind: JsonContextKind::Object,
            first: true,
        });
        Ok(())
    }

    pub async fn end_object(&mut self) -> Result<(), ExportBuildError> {
        self.close_context(JsonContextKind::Object)?;
        self.write(b"}").await
    }

    pub async fn start_array(&mut self, name: &str) -> Result<(), ExportBuildError> {
        self.before_value(Some(name)).await?;
        self.write(b"[").await?;
        self.contexts.push(JsonContext {
            kind: JsonContextKind::Array,
            first: true,
        });
        Ok(())
    }

    pub async fn end_array(&mut self) -> Result<(), ExportBuildError> {
        self.close_context(JsonContextKind::Array)?;
        self.write(b"]").await
    }

    pub async fn property<T: Serialize + ?Sized>(
        &mut self,
        name: &str,
        value: &T,
    ) -> Result<(), ExportBuildError> {
        self.before_value(Some(name)).await?;
        let encoded = serde_json::to_vec(value).map_err(|_| DatabaseError::QueryFailed)?;
        self.write(&encoded).await
    }

    pub async fn item<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), ExportBuildError> {
        self.before_value(None).await?;
        let encoded = serde_json::to_vec(value).map_err(|_| DatabaseError::QueryFailed)?;
        self.write(&encoded).await
    }

    pub async fn finish(mut self) -> Result<(File, u64), ExportBuildError> {
        if !self.contexts.is_empty() {
            return Err(DatabaseError::QueryFailed.into());
        }
        self.file.flush().await?;
        self.file.seek(SeekFrom::Start(0)).await?;
        Ok((self.file, self.written_bytes))
    }

    async fn before_value(&mut self, name: Option<&str>) -> Result<(), ExportBuildError> {
        let Some(context) = self.contexts.last().copied() else {
            if self.written_bytes != 0 || name.is_some() {
                return Err(DatabaseError::QueryFailed.into());
            }
            return Ok(());
        };
        if (context.kind == JsonContextKind::Object && name.is_none())
            || (context.kind == JsonContextKind::Array && name.is_some())
        {
            return Err(DatabaseError::QueryFailed.into());
        }
        if !context.first {
            self.write(b",").await?;
        }
        if let Some(last) = self.contexts.last_mut() {
            last.first = false;
        }
        if let Some(name) = name {
            let encoded = serde_json::to_vec(name).map_err(|_| DatabaseError::QueryFailed)?;
            self.write(&encoded).await?;
            self.write(b":").await?;
        }
        Ok(())
    }

    fn close_context(&mut self, expected: JsonContextKind) -> Result<(), ExportBuildError> {
        if self
            .contexts
            .pop()
            .is_none_or(|context| context.kind != expected)
        {
            return Err(DatabaseError::QueryFailed.into());
        }
        Ok(())
    }

    async fn write(&mut self, value: &[u8]) -> Result<(), ExportBuildError> {
        let length = u64::try_from(value.len()).map_err(|_| ExportBuildError::TooLarge)?;
        if self.written_bytes.saturating_add(length) > self.max_bytes {
            return Err(ExportBuildError::TooLarge);
        }
        self.file.write_all(value).await?;
        self.written_bytes += length;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ExportSnapshot {
    pub snapshot_at: DateTime<Utc>,
    pub account: Value,
    pub profile: Value,
    pub photo_key: Option<String>,
    pub preferences: Value,
    pub subscription: Value,
    pub discovery_rows: u64,
}

pub type DataExportFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ExportSnapshot, ExportBuildError>> + Send + 'a>>;

pub trait DataExportStore: Send + Sync {
    fn write_snapshot<'a>(
        &'a self,
        user_id: Uuid,
        writer: &'a mut JsonExportWriter,
        page_size: u32,
    ) -> DataExportFuture<'a>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataExportError {
    Busy,
    TooLarge,
    Unavailable,
}

#[derive(Clone)]
pub struct DataExportService {
    store: Arc<dyn DataExportStore>,
    rights: Arc<dyn DataRightsStore>,
    photos: Arc<dyn ProfilePhotoUrlProvider>,
    page_size: u32,
    max_bytes: u64,
    slots: Arc<Semaphore>,
    spool_dir: PathBuf,
    download_timeout: Duration,
}

impl DataExportService {
    pub fn new(
        store: Arc<dyn DataExportStore>,
        rights: Arc<dyn DataRightsStore>,
        photos: Arc<dyn ProfilePhotoUrlProvider>,
        page_size: u32,
        max_bytes: u64,
        max_concurrency: u8,
    ) -> Self {
        Self {
            store,
            rights,
            photos,
            page_size,
            max_bytes,
            slots: Arc::new(Semaphore::new(usize::from(max_concurrency))),
            spool_dir: std::env::temp_dir(),
            download_timeout: DOWNLOAD_TIMEOUT,
        }
    }

    pub fn with_storage(mut self, spool_dir: Option<PathBuf>, download_timeout: Duration) -> Self {
        if let Some(dir) = spool_dir {
            self.spool_dir = dir;
        }
        self.download_timeout = download_timeout;
        self
    }

    pub async fn prepare(&self, user_id: Uuid) -> Result<PreparedDataExport, DataExportError> {
        self.prepare_bounded(user_id, self.max_bytes).await
    }

    #[cfg(feature = "webauthn-probe")]
    pub async fn prepare_assisted(
        &self,
        user_id: Uuid,
    ) -> Result<PreparedDataExport, DataExportError> {
        self.prepare_bounded(
            user_id,
            crate::config::assisted_export_max_bytes(self.max_bytes),
        )
        .await
    }

    async fn prepare_bounded(
        &self,
        user_id: Uuid,
        max_bytes: u64,
    ) -> Result<PreparedDataExport, DataExportError> {
        let permit = Arc::clone(&self.slots)
            .try_acquire_owned()
            .map_err(|error| match error {
                TryAcquireError::NoPermits | TryAcquireError::Closed => DataExportError::Busy,
            })?;
        let spool_dir = self.spool_dir.clone();
        let (file, directory) = tokio::task::spawn_blocking(move || open_spool_file(&spool_dir))
            .await
            .map_err(|_| DataExportError::Unavailable)?
            .map_err(|_| DataExportError::Unavailable)?;
        self.prepare_in_directory(user_id, directory, file, permit, max_bytes)
            .await
    }

    async fn prepare_in_directory(
        &self,
        user_id: Uuid,
        directory: Option<ExportDirectory>,
        file: std::fs::File,
        permit: OwnedSemaphorePermit,
        max_bytes: u64,
    ) -> Result<PreparedDataExport, DataExportError> {
        let mut writer = JsonExportWriter::new(File::from_std(file), max_bytes);
        writer.start_object(None).await.map_err(map_build_error)?;

        let discovery_started_at = Utc::now();
        let snapshot = self
            .store
            .write_snapshot(user_id, &mut writer, self.page_size)
            .await
            .map_err(map_build_error)?;
        let discovery_completed_at = Utc::now();
        let photo = self
            .photos
            .url_for_key(snapshot.photo_key)
            .await
            .map_err(|_| DataExportError::Unavailable)?;
        let profile = profile_with_photo(snapshot.profile, photo);
        writer
            .property("account", &snapshot.account)
            .await
            .map_err(map_build_error)?;
        writer
            .property("profile", &profile)
            .await
            .map_err(map_build_error)?;
        writer
            .property("preferences", &snapshot.preferences)
            .await
            .map_err(map_build_error)?;
        writer
            .property("subscription", &snapshot.subscription)
            .await
            .map_err(map_build_error)?;

        let completed_at = Utc::now();
        writer
            .property("exported_at", &wire_timestamp(completed_at))
            .await
            .map_err(map_build_error)?;
        writer
            .property(
                "consistency",
                &json!({
                    "postgres": {
                        "level": "repeatable_read",
                        "snapshot_at": wire_timestamp(snapshot.snapshot_at),
                    },
                    "discovery": {
                        "level": "repeatable_read",
                        "snapshot_at": wire_timestamp(snapshot.snapshot_at),
                        "started_at": wire_timestamp(discovery_started_at),
                        "completed_at": wire_timestamp(discovery_completed_at),
                        "rows": snapshot.discovery_rows,
                    }
                }),
            )
            .await
            .map_err(map_build_error)?;
        writer.end_object().await.map_err(map_build_error)?;
        let (file, bytes) = writer.finish().await.map_err(map_build_error)?;

        self.rights
            .record_self_export(user_id)
            .await
            .map_err(|_| DataExportError::Unavailable)?;
        Ok(PreparedDataExport::new(
            file,
            bytes,
            directory,
            permit,
            self.download_timeout,
        ))
    }
}

fn profile_with_photo(mut profile: Value, photo: Option<String>) -> Value {
    if let Value::Object(ref mut object) = profile {
        object.insert("photo".to_owned(), photo.map_or(Value::Null, Value::String));
    }
    profile
}

fn map_build_error(error: ExportBuildError) -> DataExportError {
    match error {
        ExportBuildError::TooLarge => DataExportError::TooLarge,
        ExportBuildError::Io(_) | ExportBuildError::Database(_) => DataExportError::Unavailable,
    }
}

pub struct PreparedDataExport {
    bytes: u64,
    download: Arc<Download>,
    watchdog: AbortHandle,
}

#[derive(Default)]
struct Download {
    state: Mutex<DownloadState>,
    waker: AtomicWaker,
}

#[derive(Default)]
struct DownloadState {
    active: Option<OpenDownload>,
    timed_out: bool,
}

struct OpenDownload {
    stream: ReaderStream<File>,
    _directory: Option<ExportDirectory>,
    _permit: OwnedSemaphorePermit,
}

impl PreparedDataExport {
    fn new(
        file: File,
        bytes: u64,
        directory: Option<ExportDirectory>,
        permit: OwnedSemaphorePermit,
        timeout: Duration,
    ) -> Self {
        let download = Arc::new(Download {
            state: Mutex::new(DownloadState {
                active: Some(OpenDownload {
                    stream: ReaderStream::new(file),
                    _directory: directory,
                    _permit: permit,
                }),
                timed_out: false,
            }),
            waker: AtomicWaker::new(),
        });
        let state = Arc::clone(&download);
        let deadline = tokio::time::Instant::now() + timeout;
        // A socket with a full send buffer may never poll its HTTP body again.
        let watchdog = tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            if let Ok(mut download) = state.state.lock() {
                download.timed_out = download.active.take().is_some();
            }
            state.waker.wake();
        })
        .abort_handle();
        Self {
            bytes,
            download,
            watchdog,
        }
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Stream for PreparedDataExport {
    type Item = Result<Bytes, io::Error>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.download.waker.register(context.waker());
        let Ok(mut download) = self.download.state.lock() else {
            return Poll::Ready(None);
        };
        if std::mem::take(&mut download.timed_out) {
            return Poll::Ready(Some(Err(io::ErrorKind::TimedOut.into())));
        }
        let Some(open) = download.active.as_mut() else {
            return Poll::Ready(None);
        };
        let polled = Pin::new(&mut open.stream).poll_next(context);
        if matches!(polled, Poll::Ready(None) | Poll::Ready(Some(Err(_)))) {
            download.active.take();
            self.watchdog.abort();
        }
        polled
    }
}

impl Drop for PreparedDataExport {
    fn drop(&mut self) {
        self.watchdog.abort();
        if let Ok(mut download) = self.download.state.lock() {
            download.active.take();
        }
    }
}

struct ExportDirectory(PathBuf);

impl Drop for ExportDirectory {
    fn drop(&mut self) {
        let path = std::mem::take(&mut self.0);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(move || remove_directory(&path));
        } else {
            // Already outside the async runtime, including cancelled preparation.
            remove_directory(&path);
        }
    }
}

fn open_spool_file(root: &Path) -> io::Result<(std::fs::File, Option<ExportDirectory>)> {
    let directory = ExportDirectory(create_private_directory(root)?);
    let path = directory.0.join(EXPORT_FILENAME);
    let file = open_private_file(&path)?;
    #[cfg(unix)]
    {
        // Unlink immediately so a killed API cannot leave personal data on disk.
        std::fs::remove_file(path)?;
        drop(directory);
        Ok((file, None))
    }
    #[cfg(not(unix))]
    Ok((file, Some(directory)))
}

fn create_private_directory(root: &Path) -> io::Result<PathBuf> {
    for _ in 0..16 {
        let path = root.join(format!("histae-export-{}", Uuid::new_v4()));
        #[cfg(unix)]
        let mut builder = DirBuilder::new();
        #[cfg(not(unix))]
        let builder = DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        match builder.create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "private export directory collision",
    ))
}

fn open_private_file(path: &Path) -> io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

fn remove_directory(path: &Path) {
    for attempt in 0..3 {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return,
            Err(_) if attempt < 2 => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(_) => {
                let _ = crate::operations::logging::error("data_export_cleanup_failed", None);
            }
        }
    }
}

fn wire_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt as _;

    #[tokio::test]
    async fn an_unpolled_download_releases_its_slot_at_the_deadline() {
        let (file, directory) = open_spool_file(&std::env::temp_dir()).unwrap();
        let semaphore = Arc::new(Semaphore::new(1));
        let permit = Arc::clone(&semaphore).acquire_owned().await.unwrap();
        let mut download = PreparedDataExport::new(
            File::from_std(file),
            0,
            directory,
            permit,
            Duration::from_millis(20),
        );
        let next_permit = tokio::time::timeout(Duration::from_secs(2), semaphore.acquire())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            download.next().await.unwrap().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        drop(next_permit);
    }

    #[tokio::test]
    async fn dropping_an_export_closes_its_file_and_removes_its_private_directory() {
        let path = create_private_directory(&std::env::temp_dir()).expect("directory");
        let file = open_private_file(&path.join(EXPORT_FILENAME)).expect("file");
        let semaphore = Arc::new(Semaphore::new(1));
        let export = PreparedDataExport::new(
            File::from_std(file),
            0,
            Some(ExportDirectory(path.clone())),
            semaphore.clone().acquire_owned().await.expect("permit"),
            DOWNLOAD_TIMEOUT,
        );
        assert_eq!(semaphore.available_permits(), 0);
        drop(export);
        assert_eq!(semaphore.available_permits(), 1);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while tokio::fs::try_exists(&path).await.expect("exists") {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cleanup completes");
    }

    #[tokio::test]
    async fn writer_rejects_oversized_documents_before_streaming() {
        let directory =
            create_private_directory(&std::env::temp_dir()).unwrap_or_else(|_| unreachable!());
        let file =
            open_private_file(&directory.join(EXPORT_FILENAME)).unwrap_or_else(|_| unreachable!());
        let mut writer = JsonExportWriter::new(File::from_std(file), 32);
        writer
            .start_object(None)
            .await
            .unwrap_or_else(|_| unreachable!());
        assert!(matches!(
            writer.property("value", &"x".repeat(64)).await,
            Err(ExportBuildError::TooLarge)
        ));
        remove_directory(&directory);
    }

    #[tokio::test]
    async fn writer_builds_nested_json_without_holding_the_document_in_memory() {
        let directory =
            create_private_directory(&std::env::temp_dir()).unwrap_or_else(|_| unreachable!());
        let file =
            open_private_file(&directory.join(EXPORT_FILENAME)).unwrap_or_else(|_| unreachable!());
        let mut writer = JsonExportWriter::new(File::from_std(file), 1_024);
        writer
            .start_object(None)
            .await
            .unwrap_or_else(|_| unreachable!());
        writer
            .start_array("items")
            .await
            .unwrap_or_else(|_| unreachable!());
        writer
            .item(&json!({"id": Uuid::new_v4()}))
            .await
            .unwrap_or_else(|_| unreachable!());
        writer.end_array().await.unwrap_or_else(|_| unreachable!());
        writer.end_object().await.unwrap_or_else(|_| unreachable!());
        let (mut file, bytes) = writer.finish().await.unwrap_or_else(|_| unreachable!());
        let mut body = String::new();
        use tokio::io::AsyncReadExt as _;
        file.read_to_string(&mut body)
            .await
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(u64::try_from(body.len()).unwrap_or(u64::MAX), bytes);
        assert!(serde_json::from_str::<Value>(&body).is_ok());
        remove_directory(&directory);
    }
}
