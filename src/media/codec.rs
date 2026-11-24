use std::{
    fmt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

#[cfg(unix)]
use futures_util::StreamExt as _;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::Semaphore,
    time::timeout,
};

pub const MAX_PHOTO_UPLOAD_BYTES: usize = 500_000;
pub const MAX_STORED_PHOTO_BYTES: usize = 500_000;
pub const MAX_PHOTO_EDGE: usize = 2_048;
pub const DEFAULT_CODEC_TIMEOUT: Duration = Duration::from_secs(35);
pub const DEFAULT_CODEC_QUEUE_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_CODEC_PENDING_REQUESTS: usize = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvalidPhotoReason {
    Empty,
    UnsupportedExtension,
    MediaTypeMismatch,
    ContentMismatch,
    DecodeFailed,
}

impl InvalidPhotoReason {
    pub fn public_message(&self) -> &'static str {
        match self {
            Self::Empty => "The uploaded photo is invalid.",
            Self::UnsupportedExtension => "The photo file extension is not supported.",
            Self::MediaTypeMismatch => "The photo media type does not match its extension.",
            Self::ContentMismatch => "The photo contents do not match its extension.",
            Self::DecodeFailed => "The photo could not be decoded.",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PhotoCodecError {
    InvalidPhoto(InvalidPhotoReason),
    PhotoTooLarge,
    CodecTimedOut,
    CodecUnavailable,
    CodecBusy,
}

impl fmt::Display for PhotoCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPhoto(reason) => formatter.write_str(reason.public_message()),
            Self::PhotoTooLarge => formatter.write_str("The uploaded photo is too large."),
            Self::CodecTimedOut => formatter.write_str("The photo codec timed out."),
            Self::CodecUnavailable => formatter.write_str("The photo codec is unavailable."),
            Self::CodecBusy => formatter.write_str("The photo codec is busy."),
        }
    }
}

impl std::error::Error for PhotoCodecError {}

pub struct UploadedPhoto<'a> {
    pub filename: &'a str,
    pub mime_type: &'a str,
    pub body: &'a [u8],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessedPhoto {
    pub body: Vec<u8>,
    pub mime_type: &'static str,
    pub size_bytes: usize,
    pub width: usize,
    pub height: usize,
    pub sha256: [u8; 32],
}

pub struct PhotoCodec {
    backend: Backend,
    timeout: Duration,
    queue_timeout: Duration,
    admissions: Semaphore,
    slots: Arc<Semaphore>,
}

enum Backend {
    Process {
        executable: PathBuf,
        script: PathBuf,
        directory: PathBuf,
    },
    #[cfg(unix)]
    Socket(reqwest::Client),
}

impl PhotoCodec {
    pub fn for_runtime() -> Self {
        let configured_root = std::env::var_os("HISTAE_RUNTIME_ROOT").map(PathBuf::from);
        let root = configured_root
            .as_deref()
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")));
        Self::with_process(
            "node",
            root.join("tools/photo-codec-runner.cjs"),
            root.join("tools/photo-codec"),
            DEFAULT_CODEC_TIMEOUT,
        )
    }

    #[cfg(unix)]
    pub fn with_socket(path: &Path) -> Result<Self, PhotoCodecError> {
        let client = reqwest::Client::builder()
            .unix_socket(path)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(DEFAULT_CODEC_TIMEOUT)
            .build()
            .map_err(|_| PhotoCodecError::CodecUnavailable)?;
        Ok(Self::new(Backend::Socket(client), DEFAULT_CODEC_TIMEOUT))
    }

    pub fn with_process(
        node_executable: impl Into<PathBuf>,
        runner_script: impl Into<PathBuf>,
        working_directory: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Self {
        Self::new(
            Backend::Process {
                executable: node_executable.into(),
                script: runner_script.into(),
                directory: working_directory.into(),
            },
            timeout,
        )
    }

    fn new(backend: Backend, timeout: Duration) -> Self {
        Self {
            backend,
            timeout,
            queue_timeout: DEFAULT_CODEC_QUEUE_TIMEOUT,
            admissions: Semaphore::new(1 + MAX_CODEC_PENDING_REQUESTS),
            slots: Arc::new(Semaphore::new(1)),
        }
    }

    pub async fn to_webp(
        &self,
        upload: UploadedPhoto<'_>,
    ) -> Result<ProcessedPhoto, PhotoCodecError> {
        let format = validate_upload(&upload)?;
        #[cfg(not(unix))]
        let _ = format;
        // Admission covers the active worker and the bounded waiting queue.
        // Both permits are released on timeout, cancellation and process errors.
        let _admission = self
            .admissions
            .try_acquire()
            .map_err(|_| PhotoCodecError::CodecBusy)?;
        let _permit = timeout(self.queue_timeout, self.slots.acquire())
            .await
            .map_err(|_| PhotoCodecError::CodecBusy)?
            .map_err(|_| PhotoCodecError::CodecUnavailable)?;

        match &self.backend {
            Backend::Process {
                executable,
                script,
                directory,
            } => {
                self.run_process(upload, executable, script, directory)
                    .await
            }
            #[cfg(unix)]
            Backend::Socket(client) => run_remote(client, format, upload.body).await,
        }
    }

    async fn run_process(
        &self,
        upload: UploadedPhoto<'_>,
        executable: &Path,
        script: &Path,
        directory: &Path,
    ) -> Result<ProcessedPhoto, PhotoCodecError> {
        let mut child = Command::new(executable)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .arg(script)
            .arg(upload.filename)
            .arg(upload.mime_type)
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| PhotoCodecError::CodecUnavailable)?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or(PhotoCodecError::CodecUnavailable)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(PhotoCodecError::CodecUnavailable)?;
        let run = async {
            stdin
                .write_all(upload.body)
                .await
                .map_err(|_| PhotoCodecError::CodecUnavailable)?;
            stdin
                .shutdown()
                .await
                .map_err(|_| PhotoCodecError::CodecUnavailable)?;
            drop(stdin);

            let mut body = Vec::with_capacity(64 * 1_024);
            let mut bounded_stdout = stdout.take((MAX_STORED_PHOTO_BYTES + 1) as u64);
            bounded_stdout
                .read_to_end(&mut body)
                .await
                .map_err(|_| PhotoCodecError::CodecUnavailable)?;
            if body.len() > MAX_STORED_PHOTO_BYTES {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(PhotoCodecError::PhotoTooLarge);
            }

            let status = child
                .wait()
                .await
                .map_err(|_| PhotoCodecError::CodecUnavailable)?;
            match status.code() {
                Some(0) => processed_photo(body),
                Some(20) => Err(PhotoCodecError::InvalidPhoto(
                    InvalidPhotoReason::DecodeFailed,
                )),
                Some(21) => Err(PhotoCodecError::PhotoTooLarge),
                _ => Err(PhotoCodecError::CodecUnavailable),
            }
        };

        match timeout(self.timeout, run).await {
            Ok(result) => result,
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err(PhotoCodecError::CodecTimedOut)
            }
        }
    }
}

#[cfg(unix)]
async fn run_remote(
    client: &reqwest::Client,
    format: PhotoFormat,
    bytes: &[u8],
) -> Result<ProcessedPhoto, PhotoCodecError> {
    let route = match format {
        PhotoFormat::Jpeg => "http://photo-codec/jpeg",
        PhotoFormat::Png => "http://photo-codec/png",
        PhotoFormat::Heif => "http://photo-codec/heif",
        PhotoFormat::Webp => "http://photo-codec/webp",
    };
    let response = client
        .post(route)
        .body(bytes.to_vec())
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                PhotoCodecError::CodecTimedOut
            } else {
                PhotoCodecError::CodecUnavailable
            }
        })?;
    match response.status().as_u16() {
        200 => {}
        413 => return Err(PhotoCodecError::PhotoTooLarge),
        422 => {
            return Err(PhotoCodecError::InvalidPhoto(
                InvalidPhotoReason::DecodeFailed,
            ));
        }
        429 => return Err(PhotoCodecError::CodecBusy),
        504 => return Err(PhotoCodecError::CodecTimedOut),
        _ => return Err(PhotoCodecError::CodecUnavailable),
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| PhotoCodecError::CodecUnavailable)?;
        if body.len() + chunk.len() > MAX_STORED_PHOTO_BYTES {
            return Err(PhotoCodecError::PhotoTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    processed_photo(body)
}

fn processed_photo(body: Vec<u8>) -> Result<ProcessedPhoto, PhotoCodecError> {
    if body.is_empty() || body.len() > MAX_STORED_PHOTO_BYTES {
        return Err(PhotoCodecError::PhotoTooLarge);
    }
    if detect_format(&body) != Some(PhotoFormat::Webp) {
        return Err(PhotoCodecError::CodecUnavailable);
    }
    let dimensions = imagesize::blob_size(&body).map_err(|_| PhotoCodecError::CodecUnavailable)?;
    if dimensions.width == 0
        || dimensions.height == 0
        || dimensions.width > MAX_PHOTO_EDGE
        || dimensions.height > MAX_PHOTO_EDGE
    {
        return Err(PhotoCodecError::CodecUnavailable);
    }
    let sha256: [u8; 32] = Sha256::digest(&body).into();
    Ok(ProcessedPhoto {
        size_bytes: body.len(),
        body,
        mime_type: "image/webp",
        width: dimensions.width,
        height: dimensions.height,
        sha256,
    })
}

fn validate_upload(upload: &UploadedPhoto<'_>) -> Result<PhotoFormat, PhotoCodecError> {
    if upload.body.is_empty() {
        return Err(PhotoCodecError::InvalidPhoto(InvalidPhotoReason::Empty));
    }
    if upload.body.len() > MAX_PHOTO_UPLOAD_BYTES {
        return Err(PhotoCodecError::PhotoTooLarge);
    }

    let extension = extension(upload.filename).ok_or(PhotoCodecError::InvalidPhoto(
        InvalidPhotoReason::UnsupportedExtension,
    ))?;
    let expected = match extension.as_str() {
        ".jpg" | ".jpeg" => PhotoFormat::Jpeg,
        ".png" => PhotoFormat::Png,
        ".heic" | ".heif" => PhotoFormat::Heif,
        ".webp" => PhotoFormat::Webp,
        _ => {
            return Err(PhotoCodecError::InvalidPhoto(
                InvalidPhotoReason::UnsupportedExtension,
            ));
        }
    };
    if !mime_matches(expected, &upload.mime_type.to_ascii_lowercase()) {
        return Err(PhotoCodecError::InvalidPhoto(
            InvalidPhotoReason::MediaTypeMismatch,
        ));
    }
    if detect_format(upload.body) != Some(expected) {
        return Err(PhotoCodecError::InvalidPhoto(
            InvalidPhotoReason::ContentMismatch,
        ));
    }
    Ok(expected)
}

fn extension(filename: &str) -> Option<String> {
    let basename = filename.rsplit(['/', '\\']).next()?;
    let dot = basename.rfind('.')?;
    (dot > 0).then(|| basename[dot..].to_lowercase())
}

fn mime_matches(format: PhotoFormat, mime_type: &str) -> bool {
    mime_type == "application/octet-stream"
        || match format {
            PhotoFormat::Jpeg => matches!(mime_type, "image/jpeg" | "image/jpg"),
            PhotoFormat::Png => mime_type == "image/png",
            PhotoFormat::Heif => matches!(mime_type, "image/heic" | "image/heif"),
            PhotoFormat::Webp => mime_type == "image/webp",
        }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PhotoFormat {
    Jpeg,
    Png,
    Heif,
    Webp,
}

fn detect_format(body: &[u8]) -> Option<PhotoFormat> {
    if body.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some(PhotoFormat::Jpeg);
    }
    if body.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some(PhotoFormat::Png);
    }
    if body.len() >= 12 && &body[0..4] == b"RIFF" && &body[8..12] == b"WEBP" {
        return Some(PhotoFormat::Webp);
    }
    is_heif(body).then_some(PhotoFormat::Heif)
}

fn is_heif(body: &[u8]) -> bool {
    if body.len() < 16 || &body[4..8] != b"ftyp" {
        return false;
    }
    let box_size = u32::from_be_bytes([body[0], body[1], body[2], body[3]]) as usize;
    if box_size < 16 || box_size > body.len() {
        return false;
    }
    (8..=box_size.saturating_sub(4)).step_by(4).any(|offset| {
        matches!(
            &body[offset..offset + 4],
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"hevm" | b"hevs"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_codec() -> PhotoCodec {
        PhotoCodec::with_process("must-not-start", "unused", ".", Duration::from_secs(1))
    }

    fn test_upload() -> UploadedPhoto<'static> {
        UploadedPhoto {
            filename: "photo.jpg",
            mime_type: "image/jpeg",
            body: &[0xff, 0xd8, 0xff],
        }
    }

    #[tokio::test]
    async fn rejects_queue_overflow_without_starting_a_process() {
        let codec = test_codec();
        let _full = codec
            .admissions
            .acquire_many((MAX_CODEC_PENDING_REQUESTS + 1) as u32)
            .await
            .expect("admission");
        assert_eq!(
            codec.to_webp(test_upload()).await,
            Err(PhotoCodecError::CodecBusy)
        );
    }

    #[tokio::test]
    async fn bounds_queue_wait_and_releases_admission_after_timeout_and_cancellation() {
        let mut codec = test_codec();
        codec.queue_timeout = Duration::from_millis(10);
        let _active = codec.slots.acquire().await.expect("active worker");
        assert_eq!(
            codec.to_webp(test_upload()).await,
            Err(PhotoCodecError::CodecBusy)
        );
        assert_eq!(
            codec.admissions.available_permits(),
            1 + MAX_CODEC_PENDING_REQUESTS
        );

        codec.queue_timeout = Duration::from_secs(60);
        {
            let pending = codec.to_webp(test_upload());
            tokio::pin!(pending);
            tokio::select! {
                result = &mut pending => panic!("queue should still be waiting: {result:?}"),
                () = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
            assert_eq!(
                codec.admissions.available_permits(),
                MAX_CODEC_PENDING_REQUESTS
            );
        }
        assert_eq!(
            codec.admissions.available_permits(),
            1 + MAX_CODEC_PENDING_REQUESTS
        );
    }

    #[test]
    fn recognises_the_same_container_signatures_as_nest() {
        assert_eq!(detect_format(&[0xff, 0xd8, 0xff]), Some(PhotoFormat::Jpeg));
        assert_eq!(
            detect_format(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
            Some(PhotoFormat::Png)
        );
        assert_eq!(detect_format(b"RIFF....WEBP"), Some(PhotoFormat::Webp));

        let mut heif = Vec::from(&b"\0\0\0\x18ftypheic\0\0\0\0heix"[..]);
        heif.extend_from_slice(b"tail");
        assert_eq!(detect_format(&heif), Some(PhotoFormat::Heif));
        heif[3] = 0xff;
        assert_eq!(detect_format(&heif), None);
    }

    #[test]
    fn validates_extension_mime_and_upload_size_before_starting_a_process() {
        let jpeg = [0xff, 0xd8, 0xff];
        let cases = [
            (
                UploadedPhoto {
                    filename: "x.gif",
                    mime_type: "image/gif",
                    body: &jpeg,
                },
                PhotoCodecError::InvalidPhoto(InvalidPhotoReason::UnsupportedExtension),
            ),
            (
                UploadedPhoto {
                    filename: "x.jpg",
                    mime_type: "image/png",
                    body: &jpeg,
                },
                PhotoCodecError::InvalidPhoto(InvalidPhotoReason::MediaTypeMismatch),
            ),
            (
                UploadedPhoto {
                    filename: "x.png",
                    mime_type: "image/png",
                    body: &jpeg,
                },
                PhotoCodecError::InvalidPhoto(InvalidPhotoReason::ContentMismatch),
            ),
        ];
        for (upload, expected) in cases {
            assert_eq!(validate_upload(&upload), Err(expected));
        }
        assert_eq!(
            validate_upload(&UploadedPhoto {
                filename: "x.JPEG",
                mime_type: "application/octet-stream",
                body: &jpeg,
            }),
            Ok(PhotoFormat::Jpeg)
        );
        assert_eq!(
            validate_upload(&UploadedPhoto {
                filename: "x.jpg",
                mime_type: "image/jpeg",
                body: &[],
            }),
            Err(PhotoCodecError::InvalidPhoto(InvalidPhotoReason::Empty))
        );
        let oversized = vec![0_u8; MAX_PHOTO_UPLOAD_BYTES + 1];
        assert_eq!(
            validate_upload(&UploadedPhoto {
                filename: "x.jpg",
                mime_type: "image/jpeg",
                body: &oversized,
            }),
            Err(PhotoCodecError::PhotoTooLarge)
        );
    }
}
