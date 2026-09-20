use std::{
    fmt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::Semaphore,
    time::timeout,
};

pub const MAX_PHOTO_UPLOAD_BYTES: usize = 500_000;
pub const MAX_STORED_PHOTO_BYTES: usize = 500_000;
pub const MAX_PHOTO_PIXELS: u64 = 40_000_000;
pub const MAX_PHOTO_EDGE: usize = 2_048;
pub const DEFAULT_CODEC_TIMEOUT: Duration = Duration::from_secs(35);

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
}

impl fmt::Display for PhotoCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPhoto(reason) => formatter.write_str(reason.public_message()),
            Self::PhotoTooLarge => formatter.write_str("The uploaded photo is too large."),
            Self::CodecTimedOut => formatter.write_str("The photo codec timed out."),
            Self::CodecUnavailable => formatter.write_str("The photo codec is unavailable."),
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

pub struct PhotoCodecProbe {
    node_executable: PathBuf,
    runner_script: PathBuf,
    nest_root: PathBuf,
    timeout: Duration,
    slots: Arc<Semaphore>,
}

impl PhotoCodecProbe {
    pub fn for_nest_root(nest_root: impl Into<PathBuf>) -> Self {
        Self {
            node_executable: PathBuf::from("node"),
            runner_script: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tools")
                .join("photo-codec-runner.cjs"),
            nest_root: nest_root.into(),
            timeout: DEFAULT_CODEC_TIMEOUT,
            slots: Arc::new(Semaphore::new(1)),
        }
    }

    pub fn with_process(
        node_executable: impl Into<PathBuf>,
        runner_script: impl Into<PathBuf>,
        nest_root: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Self {
        Self {
            node_executable: node_executable.into(),
            runner_script: runner_script.into(),
            nest_root: nest_root.into(),
            timeout,
            slots: Arc::new(Semaphore::new(1)),
        }
    }

    pub async fn to_webp(
        &self,
        upload: UploadedPhoto<'_>,
    ) -> Result<ProcessedPhoto, PhotoCodecError> {
        validate_upload(&upload)?;
        let _permit = self
            .slots
            .acquire()
            .await
            .map_err(|_| PhotoCodecError::CodecUnavailable)?;

        let mut child = Command::new(&self.node_executable)
            .arg(&self.runner_script)
            .arg(upload.filename)
            .arg(upload.mime_type)
            .current_dir(&self.nest_root)
            .env("HISTAE_NEST_ROOT", &self.nest_root)
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

fn validate_upload(upload: &UploadedPhoto<'_>) -> Result<(), PhotoCodecError> {
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
    Ok(())
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
            Ok(())
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
