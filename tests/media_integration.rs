#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use histae_api_rust::config::{ObjectStorageConfig, PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::media::codec::ProcessedPhoto;
use histae_api_rust::media::domain::{CreationResult, ProcessingPhoto};
use histae_api_rust::media::pg::PgPhotoRepository;
use histae_api_rust::media::s3::S3ObjectStorage;
use histae_api_rust::media::storage::PhotoObjectStorage;
use histae_api_rust::media::store::PhotoStore;
use histae_api_rust::moderation::domain::AutomatedPhotoModeration;
use histae_api_rust::outbox::pg::PgOutboxRepository;
use histae_api_rust::profiles::domain::ModerationStatus;
use url::Url;
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "S15 integration fixture error ({})", self.0)
    }
}

impl std::error::Error for FixtureError {}

fn variable(name: &'static str) -> Result<String, FixtureError> {
    let _ = dotenvy::dotenv();
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(FixtureError(name))
}

fn postgres_config() -> Result<PostgresConfig, FixtureError> {
    if variable("ENV")? != "development" {
        return Err(FixtureError("ENV"));
    }
    let host = variable("POSTGRES_HOST")?;
    if !matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1") {
        return Err(FixtureError("POSTGRES_HOST"));
    }
    let database = variable("POSTGRES_DB")?;
    if database != "histae-dev" {
        return Err(FixtureError("POSTGRES_DB"));
    }
    Ok(PostgresConfig {
        host,
        port: variable("POSTGRES_PORT")?
            .parse()
            .map_err(|_| FixtureError("POSTGRES_PORT"))?,
        user: variable("POSTGRES_USER")?,
        password: SecretString::new(variable("POSTGRES_PASSWORD")?),
        database,
        tls: false,
        max_connections: 4,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        statement_timeout: Duration::from_secs(15),
        idle_transaction_timeout: Duration::from_secs(30),
        application_name: "histae-rust-s15-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS").ok().map(PathBuf::from),
    })
}

fn storage_config() -> Result<ObjectStorageConfig, FixtureError> {
    let endpoint =
        variable("OBJECT_STORAGE_ENDPOINT")?.replace("storage.histae.localhost", "127.0.0.1");
    Ok(ObjectStorageConfig {
        endpoint: Url::parse(&endpoint).map_err(|_| FixtureError("OBJECT_STORAGE_ENDPOINT"))?,
        region: variable("OBJECT_STORAGE_REGION")?,
        bucket: variable("OBJECT_STORAGE_BUCKET")?,
        access_key: variable("OBJECT_STORAGE_ACCESS_KEY")?,
        secret_key: SecretString::new(variable("OBJECT_STORAGE_SECRET_KEY")?),
        force_path_style: true,
    })
}

async fn fixture(database: &Database, user_id: Uuid) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query("INSERT INTO user_account (user_id, role, phone_number_hash, phone_number_encrypted) VALUES ($1, 'user', $2, $3)")
        .bind(user_id).bind(format!("s15-{user_id}")).bind(Vec::<u8>::new())
        .execute(&mut *connection).await.map_err(map_sqlx_error)?;
    sqlx::query("INSERT INTO user_profile (user_id, firstname, birthdate) VALUES ($1, 'Photo', '1990-01-01')")
        .bind(user_id).execute(&mut *connection).await.map_err(map_sqlx_error)?;
    Ok(())
}

async fn cleanup(
    database: &Database,
    user_id: Uuid,
    photo_ids: &[Uuid],
) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM outbox_event WHERE aggregate_id = ANY($1::uuid[])")
        .bind(photo_ids)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM user_account WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

#[tokio::test]
async fn postgres_protocol_preserves_replay_replacement_and_consumption()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&postgres_config()?).await?;
    let repository =
        PgPhotoRepository::new(database.clone(), PgOutboxRepository::new(database.clone()));
    let user_id = Uuid::new_v4();
    let photo_id = Uuid::new_v4();
    let key = Uuid::new_v4();
    cleanup(&database, user_id, &[photo_id]).await?;
    let missing_user = Uuid::new_v4();
    let missing_photo = Uuid::new_v4();
    let missing_now = Utc::now();
    assert_eq!(
        repository
            .create_processing(ProcessingPhoto {
                id: missing_photo,
                user_id: missing_user,
                object_key: format!("profile-photos/{missing_user}/{missing_photo}.webp"),
                idempotency_key: Uuid::new_v4(),
                request_sha256: [1; 32],
                created_at: missing_now,
                expires_at: missing_now + TimeDelta::hours(24),
            })
            .await?,
        CreationResult::ProfileNotFound
    );
    fixture(&database, user_id).await?;
    let now = Utc::now();
    let processing = ProcessingPhoto {
        id: photo_id,
        user_id,
        object_key: format!("profile-photos/{user_id}/{photo_id}.webp"),
        idempotency_key: key,
        request_sha256: [7; 32],
        created_at: now,
        expires_at: now + TimeDelta::hours(24),
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        assert_eq!(repository.create_processing(processing.clone()).await?, CreationResult::Created);
        let processed = ProcessedPhoto { body: b"RIFF....WEBP".to_vec(), mime_type: "image/webp", size_bytes: 12, width: 2, height: 2, sha256: [9; 32] };
        assert!(repository.record_processed(photo_id, user_id, &processed).await?);
        assert!(repository.activate(photo_id, user_id, AutomatedPhotoModeration {
            status: ModerationStatus::Approved,
            reasons: Vec::new(),
            policy_version: "local_vision_v1",
            face_count: Some(1),
            sharpness_score: Some(123.5),
            nsfw_score: Some(0.02),
        }).await?);
        let stored_moderation: (String, Vec<String>, String, Option<i16>, Option<f64>, Option<f64>) =
            sqlx::query_as(
                "SELECT status, reason_codes, policy_version, face_count, sharpness_score, nsfw_score
                 FROM content_moderation_case WHERE photo_id = $1",
            )
            .bind(photo_id)
            .fetch_one(database.acquire().await?.as_mut())
            .await?;
        assert_eq!(stored_moderation.0, "approved");
        assert!(stored_moderation.1.is_empty());
        assert_eq!(stored_moderation.2, "local_vision_v1");
        assert_eq!(stored_moderation.3, Some(1));
        assert_eq!(stored_moderation.4, Some(123.5));
        assert_eq!(stored_moderation.5, Some(0.02));
        assert!(matches!(repository.create_processing(ProcessingPhoto { id: Uuid::new_v4(), ..processing.clone() }).await?, CreationResult::Replay(_)));
        assert_eq!(repository.create_processing(ProcessingPhoto { id: Uuid::new_v4(), request_sha256: [8; 32], ..processing.clone() }).await?, CreationResult::IdempotencyConflict);
        assert!(repository.begin_delete(user_id).await?);
        let outbox_count: i64 = sqlx::query_scalar("SELECT count(*) FROM outbox_event WHERE event_type = 'photo.delete' AND aggregate_id = $1")
            .bind(photo_id).fetch_one(database.acquire().await?.as_mut()).await?;
        assert_eq!(outbox_count, 1);
        repository.complete_deletion(photo_id).await?;
        assert_eq!(repository.create_processing(ProcessingPhoto { id: Uuid::new_v4(), ..processing }).await?, CreationResult::IdempotencyConsumed);
        Ok(())
    }.await;
    let cleanup_result = cleanup(&database, user_id, &[photo_id]).await;
    database.close().await;
    result?;
    cleanup_result?;
    Ok(())
}

#[tokio::test]
async fn s3_put_presign_and_idempotent_delete_work_against_local_storage()
-> Result<(), Box<dyn std::error::Error>> {
    let storage =
        S3ObjectStorage::new(&storage_config()?).map_err(|_| FixtureError("storage_new"))?;
    storage
        .check()
        .await
        .map_err(|_| FixtureError("storage_check"))?;
    let key = format!("profile-photos/{}/{}.webp", Uuid::new_v4(), Uuid::new_v4());
    let body = b"private-photo-test".to_vec();
    storage
        .put(&key, body.clone(), "image/webp", "private, max-age=300")
        .await
        .map_err(|_| FixtureError("storage_put"))?;
    let signed = storage
        .signed_get_url(&key, 300)
        .await
        .map_err(|_| FixtureError("storage_presign"))?;
    let response = reqwest::get(&signed).await?;
    assert!(response.status().is_success());
    assert_eq!(response.bytes().await?.as_ref(), body.as_slice());
    storage.delete(&key).await?;
    storage.delete(&key).await?;
    assert!(!reqwest::get(&signed).await?.status().is_success());
    Ok(())
}
