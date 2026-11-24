use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
    time::Duration,
};

use histae_api_rust::media::codec::{
    InvalidPhotoReason, MAX_PHOTO_EDGE, MAX_STORED_PHOTO_BYTES, PhotoCodec, PhotoCodecError,
    UploadedPhoto,
};
use sha2::{Digest, Sha256};

fn codec_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tools")
        .join("photo-codec")
}

#[cfg(unix)]
#[tokio::test]
async fn converts_a_photo_through_the_private_worker_socket()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("histae-codec-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory)?;
    let socket = directory.join("worker.sock");
    let mut child = tokio::process::Command::new("node")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/photo-codec-server.cjs"))
        .arg(&socket)
        .kill_on_drop(true)
        .spawn()?;
    let result = async {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !socket.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        let body = include_bytes!("fixtures/photos/sample.jpg");
        let codec = PhotoCodec::with_socket(&socket)?;
        let image = codec
            .to_webp(UploadedPhoto {
                filename: "sample.jpg",
                mime_type: "image/jpeg",
                body,
            })
            .await?;
        assert_eq!(image.mime_type, "image/webp");
        assert!(image.size_bytes <= MAX_STORED_PHOTO_BYTES);
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    child.kill().await?;
    fs::remove_dir_all(directory)?;
    result
}

fn generated_fixtures() -> &'static PathBuf {
    static FIXTURES: OnceLock<PathBuf> = OnceLock::new();
    FIXTURES.get_or_init(|| {
        let root = codec_root();
        let output = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("photo-codec-fixtures");
        let status = Command::new("node")
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tools")
                    .join("generate-photo-codec-fixtures.cjs"),
            )
            .arg(&root)
            .arg(&output)
            .status()
            .unwrap_or_else(|error| panic!("fixture generator did not start: {error}"));
        assert!(status.success(), "fixture generator failed");
        output
    })
}

#[tokio::test]
async fn accepts_every_nest_format_and_returns_bounded_webp() {
    let codec = PhotoCodec::for_runtime();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("photos");
    for (fixture, filename, mime_type) in [
        ("sample.jpg", "sample.jpg", "image/jpeg"),
        ("sample.jpeg", "sample.jpeg", "image/jpeg"),
        ("sample.png", "sample.png", "image/png"),
        ("sample.heic", "sample.heic", "image/heic"),
        ("sample.heic", "sample.heif", "image/heif"),
        ("sample.webp", "sample.webp", "image/webp"),
    ] {
        let body = fs::read(fixtures.join(fixture))
            .unwrap_or_else(|error| panic!("failed to read {fixture}: {error}"));
        let result = codec
            .to_webp(UploadedPhoto {
                filename,
                mime_type,
                body: &body,
            })
            .await
            .unwrap_or_else(|error| panic!("failed to process {filename}: {error}"));
        assert_eq!(result.mime_type, "image/webp");
        assert_eq!(result.size_bytes, result.body.len());
        let expected_hash: [u8; 32] = Sha256::digest(&result.body).into();
        assert_eq!(result.sha256, expected_hash);
        assert!(result.size_bytes <= MAX_STORED_PHOTO_BYTES);
        assert!(result.width <= MAX_PHOTO_EDGE);
        assert!(result.height <= MAX_PHOTO_EDGE);
        assert_eq!(&result.body[0..4], b"RIFF");
        assert_eq!(&result.body[8..12], b"WEBP");
    }
}

#[tokio::test]
async fn applies_exif_orientation_and_removes_source_metadata() {
    let codec = PhotoCodec::for_runtime();
    let body = fs::read(generated_fixtures().join("orientation-6.jpg"))
        .unwrap_or_else(|error| panic!("failed to read orientation fixture: {error}"));
    let result = codec
        .to_webp(UploadedPhoto {
            filename: "orientation-6.jpg",
            mime_type: "image/jpeg",
            body: &body,
        })
        .await
        .unwrap_or_else(|error| panic!("orientation conversion failed: {error}"));
    assert_eq!((result.width, result.height), (2, 3));
    assert!(
        !result
            .body
            .windows(4)
            .any(|value| matches!(value, b"EXIF" | b"ICCP" | b"XMP "))
    );
}

#[tokio::test]
async fn rejects_animated_and_over_pixel_inputs_like_nest() {
    let codec = PhotoCodec::for_runtime();
    for (filename, mime_type) in [
        ("animated.webp", "image/webp"),
        ("over-40mp.jpg", "image/jpeg"),
    ] {
        let body = fs::read(generated_fixtures().join(filename))
            .unwrap_or_else(|error| panic!("failed to read {filename}: {error}"));
        assert_eq!(
            codec
                .to_webp(UploadedPhoto {
                    filename,
                    mime_type,
                    body: &body,
                })
                .await,
            Err(PhotoCodecError::InvalidPhoto(
                InvalidPhotoReason::DecodeFailed
            ))
        );
    }

    assert_eq!(
        codec
            .to_webp(UploadedPhoto {
                filename: "corrupt.jpg",
                mime_type: "image/jpeg",
                body: &[0xff, 0xd8, 0xff, 0, 1, 2, 3],
            })
            .await,
        Err(PhotoCodecError::InvalidPhoto(
            InvalidPhotoReason::DecodeFailed
        ))
    );
}

#[tokio::test]
async fn kills_a_codec_process_that_exceeds_the_deadline() {
    let codec = PhotoCodec::with_process(
        "node",
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("photo_codec")
            .join("hanging-worker.cjs"),
        codec_root(),
        Duration::from_millis(100),
    );
    assert_eq!(
        codec
            .to_webp(UploadedPhoto {
                filename: "photo.jpg",
                mime_type: "image/jpeg",
                body: &[0xff, 0xd8, 0xff],
            })
            .await,
        Err(PhotoCodecError::CodecTimedOut)
    );
}

#[tokio::test]
async fn kills_a_codec_process_before_accepting_an_oversized_output() {
    let codec = PhotoCodec::with_process(
        "node",
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("photo_codec")
            .join("oversized-worker.cjs"),
        codec_root(),
        Duration::from_secs(15),
    );
    assert_eq!(
        codec
            .to_webp(UploadedPhoto {
                filename: "photo.jpg",
                mime_type: "image/jpeg",
                body: &[0xff, 0xd8, 0xff],
            })
            .await,
        Err(PhotoCodecError::PhotoTooLarge)
    );
}
