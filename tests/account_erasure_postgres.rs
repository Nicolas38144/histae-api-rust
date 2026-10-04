#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::infra::postgres_locks::AccountActivityPool;
use histae_api_rust::media::codec::PhotoCodecError;
use histae_api_rust::media::pg::PgPhotoRepository;
use histae_api_rust::media::service::{PhotoProcessor, PhotoService, ProcessorFuture};
use histae_api_rust::media::storage::{ObjectStorageError, PhotoObjectStorage, StorageFuture};
use histae_api_rust::moderation::domain::AutomatedPhotoModeration;
use histae_api_rust::moderation::photo::{PhotoModerationFuture, PhotoModerator};
use histae_api_rust::outbox::pg::PgOutboxRepository;
use histae_api_rust::outbox::store::OutboxStore;
use histae_api_rust::privacy::erasure::pg::PgErasureRepository;
use histae_api_rust::privacy::erasure::{
    AccountDeletionError, AccountDeletionService, CustomerEraser, ErasureDependencyFuture,
    ErasureService, ErasureStepError,
};
use histae_api_rust::profiles::domain::{ModerationReason, ModerationStatus};
use histae_api_rust::shared::clock::Clock;
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PostgreSQL S25 fixture error ({})", self.0)
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

fn local_config() -> Result<PostgresConfig, FixtureError> {
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
        port: env::var("POSTGRES_PORT")
            .unwrap_or_else(|_| "5432".to_owned())
            .parse()
            .map_err(|_| FixtureError("POSTGRES_PORT"))?,
        user: variable("POSTGRES_USER")?,
        password: SecretString::new(variable("POSTGRES_PASSWORD")?),
        database,
        tls: env::var("POSTGRES_SSLMODE").is_ok_and(|value| value != "disable"),
        max_connections: 4,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        statement_timeout: Duration::from_secs(15),
        idle_transaction_timeout: Duration::from_secs(30),
        application_name: "histae-rust-s25-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
    })
}

async fn insert_account(database: &Database, user_id: Uuid) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO user_account
           (user_id, role, phone_number_hash, phone_number_encrypted)
         VALUES ($1, 'user', $2, $3)",
    )
    .bind(user_id)
    .bind(format!("s25-{user_id}"))
    .bind(Vec::<u8>::new())
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

async fn cleanup(database: &Database, users: &[Uuid]) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM data_access_log WHERE accessed_user_id = ANY($1::uuid[])")
        .bind(users)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM user_account WHERE user_id = ANY($1::uuid[])")
        .bind(users)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

#[derive(Clone)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

#[derive(Default)]
struct CustomerPort(AtomicU32);

impl CustomerEraser for CustomerPort {
    fn delete_customer_for_account(&self, _user_id: Uuid) -> ErasureDependencyFuture<'_> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(true)
        })
    }
}

struct UnusedPhotoProcessor;

impl PhotoProcessor for UnusedPhotoProcessor {
    fn process<'a>(
        &'a self,
        _upload: &'a histae_api_rust::media::domain::UploadPhoto,
    ) -> ProcessorFuture<'a> {
        Box::pin(async { Err(PhotoCodecError::CodecUnavailable) })
    }
}

struct UnusedPhotoModerator;

impl PhotoModerator for UnusedPhotoModerator {
    fn analyze<'a>(&'a self, _webp: &'a [u8]) -> PhotoModerationFuture<'a> {
        Box::pin(async {
            AutomatedPhotoModeration {
                status: ModerationStatus::Pending,
                reasons: vec![ModerationReason::AnalysisUnavailable],
                policy_version: "test",
                face_count: None,
                sharpness_score: None,
                nsfw_score: None,
            }
        })
    }
}

#[derive(Clone)]
struct CheckingStorage {
    database: Database,
    deleted: Arc<AtomicU32>,
}

impl PhotoObjectStorage for CheckingStorage {
    fn put<'a>(
        &'a self,
        _key: &'a str,
        _body: Vec<u8>,
        _content_type: &'static str,
        _cache_control: &'static str,
    ) -> StorageFuture<'a, ()> {
        Box::pin(async { Err(ObjectStorageError) })
    }

    fn delete<'a>(&'a self, key: &'a str) -> StorageFuture<'a, ()> {
        let database = self.database.clone();
        let deleted = Arc::clone(&self.deleted);
        let key = key.to_owned();
        Box::pin(async move {
            let mut connection = database.acquire().await.map_err(|_| ObjectStorageError)?;
            let still_tracked: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_photo WHERE object_key = $1)")
                    .bind(key)
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(|_| ObjectStorageError)?;
            if !still_tracked {
                return Err(ObjectStorageError);
            }
            deleted.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
    }

    fn signed_get_url<'a>(&'a self, _key: &'a str, _ttl_seconds: u32) -> StorageFuture<'a, String> {
        Box::pin(async { Err(ObjectStorageError) })
    }

    fn check(&self) -> StorageFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

async fn claim(database: &Database, event_id: Uuid, worker_id: Uuid) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query(
        "UPDATE outbox_event SET status = 'processing', locked_by = $2,
           locked_at = clock_timestamp(), attempts = attempts + 1
         WHERE id = $1",
    )
    .bind(event_id)
    .bind(worker_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

async fn step(database: &Database, request_id: Uuid) -> Result<String, DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query_scalar("SELECT step FROM account_erasure WHERE request_id = $1")
        .bind(request_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(map_sqlx_error)
}

#[tokio::test]
async fn token_acceptance_and_checkpointed_erasure_are_atomic_and_resumable()
-> Result<(), Box<dyn std::error::Error>> {
    let config = local_config()?;
    let database = Database::connect(&config)
        .await
        .map_err(|error| FixtureError(error.safe_code()))?;
    let activity = AccountActivityPool::connect(&config).await?;
    let user_id = Uuid::new_v4();
    let other_id = Uuid::new_v4();
    let users = [user_id, other_id];
    cleanup(&database, &users).await?;
    insert_account(&database, user_id).await?;
    insert_account(&database, other_id).await?;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let now = Utc::now();
        let repository = Arc::new(PgErasureRepository::new(database.clone()));
        let deletion = AccountDeletionService::new(
            repository.clone(),
            Duration::from_secs(600),
            Arc::new(FixedClock(now)),
        );
        let replaced = deletion.issue(user_id).await?;
        let active = deletion.issue(user_id).await?;
        assert_eq!(
            deletion.accept(user_id, &replaced.confirmation_token).await,
            Err(AccountDeletionError::InvalidOrExpiredToken)
        );

        let mut connection = database.acquire().await?;
        let stored_hash: String =
            sqlx::query_scalar("SELECT token_hash FROM account_deletion_token WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&mut *connection)
                .await?;
        assert_ne!(stored_hash, active.confirmation_token);
        assert!(!stored_hash.contains(':'));

        let swiped_at = Utc::now();
        sqlx::query(
            "INSERT INTO swipe_decision
               (actor_id, target_id, decision, swiped_at, expires_at)
             VALUES
               ($1, $2, 'like', $3, $3 + INTERVAL '365 days'),
               ($2, $1, 'pass', $3, $3 + INTERVAL '365 days')",
        )
        .bind(user_id)
        .bind(other_id)
        .bind(swiped_at)
        .execute(&mut *connection)
        .await?;
        sqlx::query(
            "INSERT INTO user_profile (user_id, firstname, birthdate)
             VALUES ($1, 'Alice', DATE '1990-01-01')",
        )
        .bind(user_id)
        .execute(&mut *connection)
        .await?;
        let photo_id = Uuid::new_v4();
        let object_key = format!("profile-photos/{user_id}/{photo_id}.webp");
        sqlx::query(
            "INSERT INTO user_photo
               (id, user_id, object_key, status, mime_type, size_bytes, width, height, sha256)
             VALUES ($1, $2, $3, 'ready', 'image/webp', 1, 1, 1, $4)",
        )
        .bind(photo_id)
        .bind(user_id)
        .bind(&object_key)
        .bind(vec![0_u8; 32])
        .execute(&mut *connection)
        .await?;

        let accepted = deletion.accept(user_id, &active.confirmation_token).await?;
        let event_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM outbox_event
             WHERE event_type = 'account.erase' AND aggregate_id = $1",
        )
        .bind(accepted.request_id)
        .fetch_one(&mut *connection)
        .await?;
        assert!(
            sqlx::query_scalar::<_, bool>(
                "SELECT deleted_at IS NOT NULL FROM user_account WHERE user_id = $1",
            )
            .bind(user_id)
            .fetch_one(&mut *connection)
            .await?
        );
        let token_count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM account_deletion_token WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&mut *connection)
        .await?;
        assert_eq!(token_count, 0);

        let customers = Arc::new(CustomerPort::default());
        let deleted_objects = Arc::new(AtomicU32::new(0));
        let storage = Arc::new(CheckingStorage {
            database: database.clone(),
            deleted: deleted_objects.clone(),
        });
        let photo_repository = Arc::new(PgPhotoRepository::new(
            database.clone(),
            PgOutboxRepository::new(database.clone()),
        ));
        let photos = Arc::new(PhotoService::new(
            photo_repository,
            Arc::new(UnusedPhotoProcessor),
            storage,
            Arc::new(UnusedPhotoModerator),
            activity.clone(),
            Arc::new(FixedClock(now)),
        ));
        let worker_id = Uuid::new_v4();
        claim(&database, event_id, worker_id).await?;
        let wrong_worker = Uuid::new_v4();
        let service = ErasureService::new(
            repository.clone(),
            activity.clone(),
            customers.clone(),
            photos.clone(),
        );
        let mut blocker = database.acquire().await?;
        let shared_lock: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_lock_shared(hashtextextended($1, 13092026))",
        )
        .bind(user_id.to_string())
        .fetch_one(&mut *blocker)
        .await?;
        assert!(shared_lock);
        assert!(!service.process(event_id, worker_id).await?);
        let deferred: (String, i16) =
            sqlx::query_as("SELECT status, attempts FROM outbox_event WHERE id = $1")
                .bind(event_id)
                .fetch_one(&mut *connection)
                .await?;
        assert_eq!(deferred, ("pending".to_owned(), 0));
        assert_eq!(customers.0.load(Ordering::Relaxed), 0);
        sqlx::query("SELECT pg_advisory_unlock_all()")
            .execute(&mut *blocker)
            .await?;
        drop(blocker);

        claim(&database, event_id, worker_id).await?;
        assert_eq!(
            service.process(event_id, wrong_worker).await,
            Err(ErasureStepError::new("erasure_invalid_state"))
        );

        assert!(!service.process(event_id, worker_id).await?);
        assert_eq!(step(&database, accepted.request_id).await?, "photos");
        claim(&database, event_id, worker_id).await?;

        let restarted = ErasureService::new(
            repository.clone(),
            activity.clone(),
            customers.clone(),
            photos.clone(),
        );
        assert!(!restarted.process(event_id, worker_id).await?);
        assert_eq!(step(&database, accepted.request_id).await?, "swipes");
        claim(&database, event_id, worker_id).await?;

        assert!(!restarted.process(event_id, worker_id).await?);
        assert_eq!(step(&database, accepted.request_id).await?, "postgres");
        let swipes: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM swipe_decision
             WHERE actor_id = $1 OR target_id = $1",
        )
        .bind(user_id)
        .fetch_one(&mut *connection)
        .await?;
        assert_eq!(swipes, 0);
        claim(&database, event_id, worker_id).await?;

        assert!(restarted.process(event_id, worker_id).await?);
        assert_eq!(step(&database, accepted.request_id).await?, "completed");
        let request_status: String =
            sqlx::query_scalar("SELECT status FROM data_subject_request WHERE id = $1")
                .bind(accepted.request_id)
                .fetch_one(&mut *connection)
                .await?;
        assert_eq!(request_status, "completed");
        let anonymized: bool = sqlx::query_scalar(
            "SELECT anonymized_at IS NOT NULL FROM user_account WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&mut *connection)
        .await?;
        assert!(anonymized);
        let audits: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM data_access_log
             WHERE accessed_user_id = $1 AND action = 'system_anonymize'",
        )
        .bind(user_id)
        .fetch_one(&mut *connection)
        .await?;
        assert_eq!(audits, 1);
        assert_eq!(customers.0.load(Ordering::Relaxed), 1);
        assert_eq!(deleted_objects.load(Ordering::Relaxed), 1);

        let outbox = PgOutboxRepository::new(database.clone());
        assert!(outbox.complete(event_id, worker_id, Utc::now()).await?);
        let event_status: String =
            sqlx::query_scalar("SELECT status FROM outbox_event WHERE id = $1")
                .bind(event_id)
                .fetch_one(&mut *connection)
                .await?;
        assert_eq!(event_status, "completed");
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, &users).await;
    activity.close().await;
    database.close().await;
    result?;
    cleanup_result?;
    Ok(())
}
