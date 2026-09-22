#![cfg(feature = "postgres-integration")]

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use histae_api_rust::config::{JwtConfig, PostgresConfig, SecretString};
use histae_api_rust::identity::mobile::domain::RotationOutcome;
use histae_api_rust::identity::mobile::pg::{MobileSessionRepository, MobileSessionStore};
use histae_api_rust::identity::mobile::service::{MobileAuthError, MobileAuthService};
use histae_api_rust::identity::mobile::tokens::{TokenService, VerifiedAccessToken};
use histae_api_rust::infra::crypto::sha256_hex;
use histae_api_rust::infra::postgres::{Database, DatabaseError};
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PostgreSQL S08 fixture error ({})", self.0)
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
        max_connections: 8,
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        statement_timeout: Duration::from_secs(15),
        idle_transaction_timeout: Duration::from_secs(30),
        application_name: "histae-rust-s08-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
    })
}

fn token_service() -> TokenService {
    let secret = SecretString::new("jwt-signing-secret-0123456789abcdef".to_owned());
    TokenService::new(JwtConfig {
        secret: secret.clone(),
        active_kid: "primary".to_owned(),
        verification_keys: BTreeMap::from([("primary".to_owned(), secret)]),
        access_ttl: Duration::from_secs(900),
        refresh_ttl: Duration::from_secs(3_600),
    })
}

async fn insert_account(database: &Database, user_id: Uuid) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO user_account
         (user_id, phone_number_hash, phone_number_encrypted)
         VALUES ($1, $2, $3)",
    )
    .bind(user_id)
    .bind(format!("s08-{}", Uuid::new_v4().simple()))
    .bind(vec![1_u8, 2, 3])
    .execute(&mut *connection)
    .await
    .map_err(histae_api_rust::infra::postgres::map_sqlx_error)?;
    Ok(())
}

async fn cleanup(database: &Database, user_id: Uuid) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM user_account WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *connection)
        .await
        .map_err(histae_api_rust::infra::postgres::map_sqlx_error)?;
    Ok(())
}

#[tokio::test]
async fn refresh_families_preserve_rotation_replay_and_rollback_semantics()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?)
        .await
        .map_err(|_| FixtureError("connect"))?;
    let user_id = Uuid::new_v4();
    insert_account(&database, user_id).await?;
    let repository = MobileSessionRepository::new(database.clone());
    let tokens = token_service();

    let original = tokens.new_refresh_token()?;
    let original_plain = original.plain.clone();
    let original_id = original.id;
    let session = repository
        .create(user_id, original.clone())
        .await?
        .ok_or(FixtureError("create_session"))?;
    let parsed =
        TokenService::parse_refresh_token(&original_plain).ok_or(FixtureError("parse_refresh"))?;
    let mut connection = database.acquire().await?;
    let stored_hash: String =
        sqlx::query_scalar("SELECT token_hash FROM refresh_tokens WHERE id = $1")
            .bind(original_id)
            .fetch_one(&mut *connection)
            .await?;
    assert_eq!(stored_hash, original.hash);
    assert!(!stored_hash.contains(&original_plain));
    drop(connection);

    let forged = repository
        .rotate(
            parsed.jti,
            sha256_hex("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB".as_bytes()),
            tokens.new_refresh_token()?,
        )
        .await?;
    assert_eq!(forged, RotationOutcome::Invalid);
    assert!(repository.is_active(user_id, session.session_id).await?);

    let rotated = repository
        .rotate(parsed.jti, parsed.hash.clone(), tokens.new_refresh_token()?)
        .await?;
    assert!(matches!(rotated, RotationOutcome::Rotated(_)));
    let replay = repository
        .rotate(parsed.jti, parsed.hash, tokens.new_refresh_token()?)
        .await?;
    assert_eq!(replay, RotationOutcome::ReplayRevoked);
    assert!(!repository.is_active(user_id, session.session_id).await?);
    let mut connection = database.acquire().await?;
    let reason: Option<String> =
        sqlx::query_scalar("SELECT revocation_reason FROM refresh_token_family WHERE id = $1")
            .bind(session.session_id)
            .fetch_one(&mut *connection)
            .await?;
    assert_eq!(reason.as_deref(), Some("replay"));
    drop(connection);

    let rollback_token = tokens.new_refresh_token()?;
    let rollback_plain = rollback_token.plain.clone();
    let rollback_identity = repository
        .create(user_id, rollback_token.clone())
        .await?
        .ok_or(FixtureError("create_rollback_session"))?;
    let rollback_parsed = TokenService::parse_refresh_token(&rollback_plain)
        .ok_or(FixtureError("parse_rollback_refresh"))?;
    let mut colliding = tokens.new_refresh_token()?;
    colliding.id = rollback_token.id;
    assert!(matches!(
        repository
            .rotate(rollback_parsed.jti, rollback_parsed.hash, colliding)
            .await,
        Err(DatabaseError::Constraint(_))
    ));
    let mut connection = database.acquire().await?;
    let still_usable: bool =
        sqlx::query_scalar("SELECT NOT revoked FROM refresh_tokens WHERE id = $1")
            .bind(rollback_token.id)
            .fetch_one(&mut *connection)
            .await?;
    assert!(still_usable);
    drop(connection);
    assert!(
        repository
            .is_active(user_id, rollback_identity.session_id)
            .await?
    );

    let concurrent = tokens.new_refresh_token()?;
    let concurrent_plain = concurrent.plain.clone();
    let concurrent_identity = repository
        .create(user_id, concurrent)
        .await?
        .ok_or(FixtureError("create_concurrent_session"))?;
    let concurrent_parsed = TokenService::parse_refresh_token(&concurrent_plain)
        .ok_or(FixtureError("parse_concurrent_refresh"))?;
    let first_repository = repository.clone();
    let second_repository = repository.clone();
    let first_hash = concurrent_parsed.hash.clone();
    let first_next = tokens.new_refresh_token()?;
    let second_next = tokens.new_refresh_token()?;
    let (first, second) = tokio::join!(
        first_repository.rotate(concurrent_parsed.jti, first_hash, first_next),
        second_repository.rotate(concurrent_parsed.jti, concurrent_parsed.hash, second_next),
    );
    let outcomes = [first?, second?];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, RotationOutcome::Rotated(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, RotationOutcome::ReplayRevoked))
            .count(),
        1
    );
    assert!(
        !repository
            .is_active(user_id, concurrent_identity.session_id)
            .await?
    );

    let service_store: Arc<dyn MobileSessionStore> = Arc::new(repository.clone());
    let service = MobileAuthService::new(
        tokens.clone(),
        service_store,
        "terms-v1".to_owned(),
        "privacy-v1".to_owned(),
    );
    let pair = service.issue_token_pair(user_id).await?;
    let first_pair = service.refresh(&pair.refresh_token).await?;
    let verified: VerifiedAccessToken = tokens.verify_access_token(&first_pair.access_token)?;
    assert!(repository.is_active(user_id, verified.session_id).await?);
    assert_eq!(
        service.refresh(&pair.refresh_token).await,
        Err(MobileAuthError::InvalidRefreshToken)
    );
    assert!(!repository.is_active(user_id, verified.session_id).await?);

    cleanup(&database, user_id).await?;
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn logout_listing_and_revocation_keep_session_ownership_and_device_cleanup()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?)
        .await
        .map_err(|_| FixtureError("connect"))?;
    let user_id = Uuid::new_v4();
    let other_user_id = Uuid::new_v4();
    insert_account(&database, user_id).await?;
    insert_account(&database, other_user_id).await?;
    let repository = MobileSessionRepository::new(database.clone());
    let tokens = token_service();

    let predecessor = tokens.new_refresh_token()?;
    let predecessor_plain = predecessor.plain.clone();
    let session = repository
        .create(user_id, predecessor)
        .await?
        .ok_or(FixtureError("create_session"))?;
    let parsed = TokenService::parse_refresh_token(&predecessor_plain)
        .ok_or(FixtureError("parse_refresh"))?;
    assert!(matches!(
        repository
            .rotate(parsed.jti, parsed.hash.clone(), tokens.new_refresh_token()?)
            .await?,
        RotationOutcome::Rotated(_)
    ));
    let device_id = Uuid::new_v4();
    let mut connection = database.acquire().await?;
    sqlx::query(
        "INSERT INTO device_token (id, user_id, token, platform, session_id)
         VALUES ($1, $2, $3, 'ios', $4)",
    )
    .bind(device_id)
    .bind(user_id)
    .bind(format!("s08-device-{}", Uuid::new_v4().simple()))
    .bind(session.session_id)
    .execute(&mut *connection)
    .await?;
    drop(connection);
    assert!(
        repository
            .logout(
                user_id,
                session.session_id,
                parsed.jti,
                parsed.hash,
                Some(device_id),
            )
            .await?
    );
    let mut connection = database.acquire().await?;
    let device_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM device_token WHERE id = $1)")
            .bind(device_id)
            .fetch_one(&mut *connection)
            .await?;
    assert!(!device_exists);
    drop(connection);

    let current = repository
        .create(user_id, tokens.new_refresh_token()?)
        .await?
        .ok_or(FixtureError("create_current"))?;
    let target = repository
        .create(user_id, tokens.new_refresh_token()?)
        .await?
        .ok_or(FixtureError("create_target"))?;
    let foreign = repository
        .create(other_user_id, tokens.new_refresh_token()?)
        .await?
        .ok_or(FixtureError("create_foreign"))?;
    let listed = repository.list(user_id, 100, None).await?;
    assert!(listed.iter().any(|row| row.id == current.session_id));
    assert!(listed.iter().any(|row| row.id == target.session_id));
    assert!(!listed.iter().any(|row| row.id == foreign.session_id));
    assert_eq!(
        repository
            .revoke(user_id, current.session_id, Some(foreign.session_id))
            .await?,
        Some(0)
    );
    assert_eq!(
        repository
            .revoke(user_id, current.session_id, Some(target.session_id))
            .await?,
        Some(1)
    );
    assert_eq!(
        repository
            .revoke(user_id, current.session_id, Some(target.session_id))
            .await?,
        Some(1)
    );
    assert_eq!(
        repository.revoke(user_id, current.session_id, None).await?,
        Some(1)
    );
    assert!(!repository.is_active(user_id, current.session_id).await?);

    cleanup(&database, user_id).await?;
    cleanup(&database, other_user_id).await?;
    database.close().await;
    Ok(())
}
