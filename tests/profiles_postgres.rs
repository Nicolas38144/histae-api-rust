#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use histae_api_rust::config::{LegalConfig, PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::moderation::text::TextModerator;
use histae_api_rust::profiles::domain::{
    ConsentType, LookingFor, PreferencesInput, PresenceInput, ProfileInput, Sex,
    VersionedConsentChange, WriteOutcome,
};
use histae_api_rust::profiles::pg::{PgProfileRepository, ProfileStore};
use url::Url;
use uuid::Uuid;

#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PostgreSQL S12 fixture error ({})", self.0)
    }
}

impl std::error::Error for FixtureError {}

fn variable(name: &'static str) -> Result<String, FixtureError> {
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
        application_name: "histae-rust-s12-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
    })
}

fn legal() -> LegalConfig {
    LegalConfig {
        terms_version: "terms-v1".to_owned(),
        privacy_version: "privacy-v1".to_owned(),
        sensitive_data_consent_version: "sensitive-v1".to_owned(),
        location_consent_version: "location-v1".to_owned(),
        terms_url: Url::parse("https://histae.test/legal/terms").expect("terms URL"),
        privacy_url: Url::parse("https://histae.test/legal/privacy").expect("privacy URL"),
        sensitive_data_consent_url: Url::parse("https://histae.test/legal/sensitive")
            .expect("sensitive URL"),
        location_consent_url: Url::parse("https://histae.test/legal/location")
            .expect("location URL"),
        review_reference: "test-review".to_owned(),
    }
}

async fn cleanup(database: &Database, user_id: Uuid) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM user_account WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

async fn fixture(database: &Database, user_id: Uuid) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query(
        r#"
        INSERT INTO user_account (
            user_id, role, phone_number_hash, phone_number_encrypted
        ) VALUES ($1, 'user', $2, $3)
        "#,
    )
    .bind(user_id)
    .bind(format!("s12-{user_id}"))
    .bind(Vec::<u8>::new())
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        r#"
        INSERT INTO user_profile (user_id, firstname, birthdate, sex, bio)
        VALUES ($1, 'Alice', '1990-01-01', 'female', 'Curieuse')
        "#,
    )
    .bind(user_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    for (consent_type, version) in [
        ("terms_of_service_acceptance", "terms-v1"),
        ("privacy_notice_acknowledgement", "privacy-v1"),
        ("sensitive_data_consent", "sensitive-v1"),
        ("location_consent", "location-v1"),
    ] {
        sqlx::query(
            r#"
            INSERT INTO user_consent (
                user_id, consent_type, granted, document_version
            ) VALUES ($1, $2, true, $3)
            "#,
        )
        .bind(user_id)
        .bind(consent_type)
        .bind(version)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    }
    sqlx::query(
        r#"
        INSERT INTO user_preferences (
            user_id, min_age, max_age, max_distance_km, looking_for
        ) VALUES ($1, 25, 40, 30, 'both')
        "#,
    )
    .bind(user_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        r#"
        INSERT INTO user_presence (
            user_id, latitude, longitude, is_location_fresh
        ) VALUES ($1, 48.8566, 2.3522, true)
        "#,
    )
    .bind(user_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

#[tokio::test]
async fn consent_withdrawal_serializes_with_sensitive_writes_and_erases_immediately()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?)
        .await
        .map_err(|error| FixtureError(error.safe_code()))?;
    let repository = PgProfileRepository::new(database.clone());
    let user_id = Uuid::new_v4();
    cleanup(&database, user_id).await?;
    fixture(&database, user_id).await?;

    let test_result: Result<(), Box<dyn std::error::Error>> = async {
        assert_eq!(
            repository
                .upsert_profile(
                    user_id,
                    ProfileInput {
                        firstname: "Alice".to_owned(),
                        birthdate: chrono::NaiveDate::from_ymd_opt(1990, 1, 1)
                            .expect("valid date"),
                        sex: Some(Sex::Female),
                        bio: Some("Écris-moi à alice@example.com".to_owned()),
                        bio_moderation: Some(
                            TextModerator.analyze("Écris-moi à alice@example.com"),
                        ),
                    },
                    legal(),
                )
                .await?,
            WriteOutcome::Updated
        );
        let profile = repository
            .find_profile(user_id)
            .await?
            .ok_or(FixtureError("profile_projection"))?;
        assert_eq!(profile.sex, Some(Sex::Female));
        assert_eq!(profile.bio_moderation_status, Some(histae_api_rust::profiles::domain::ModerationStatus::Pending));
        assert_eq!(
            profile.bio_moderation_reasons,
            vec![histae_api_rust::profiles::domain::ModerationReason::PersonalContact]
        );

        let withdrawal = repository.record_consents(
            user_id,
            vec![VersionedConsentChange {
                consent_type: ConsentType::SensitiveDataConsent,
                granted: false,
                document_version: "sensitive-v1".to_owned(),
            }],
            "127.0.0.1".to_owned(),
            "S12 integration".to_owned(),
        );
        let write = repository.upsert_preferences(
            user_id,
            PreferencesInput {
                min_age: 30,
                max_age: 45,
                max_distance_km: 50,
                looking_for: LookingFor::Both,
            },
            legal(),
        );
        let (withdrawn, write_outcome) = tokio::join!(withdrawal, write);
        assert!(withdrawn?);
        assert!(matches!(
            write_outcome?,
            WriteOutcome::Updated | WriteOutcome::RequiredConsentMissing
        ));

        let mut connection = database.acquire().await?;
        let sex: Option<String> =
            sqlx::query_scalar("SELECT sex FROM user_profile WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&mut *connection)
                .await?;
        let preferences: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM user_preferences WHERE user_id = $1)",
        )
        .bind(user_id)
        .fetch_one(&mut *connection)
        .await?;
        assert_eq!(sex, None);
        assert!(!preferences);

        assert_eq!(
            repository
                .upsert_profile(
                    user_id,
                    ProfileInput {
                        firstname: "Alice".to_owned(),
                        birthdate: chrono::NaiveDate::from_ymd_opt(1990, 1, 1)
                            .expect("valid date"),
                        sex: Some(Sex::Female),
                        bio: None,
                        bio_moderation: None,
                    },
                    legal(),
                )
                .await?,
            WriteOutcome::RequiredConsentMissing
        );

        assert!(
            repository
                .record_consents(
                    user_id,
                    vec![VersionedConsentChange {
                        consent_type: ConsentType::LocationConsent,
                        granted: false,
                        document_version: "location-v1".to_owned(),
                    }],
                    String::new(),
                    String::new(),
                )
                .await?
        );
        let presence: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM user_presence WHERE user_id = $1)",
        )
        .bind(user_id)
        .fetch_one(&mut *connection)
        .await?;
        assert!(!presence);
        assert_eq!(
            repository
                .upsert_presence(
                    user_id,
                    PresenceInput {
                        latitude: 48.0,
                        longitude: 2.0,
                        updated_at: Utc::now(),
                    },
                    legal(),
                )
                .await?,
            WriteOutcome::RequiredConsentMissing
        );

        let before: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM user_consent WHERE user_id = $1 AND consent_type = 'location_consent'",
        )
        .bind(user_id)
        .fetch_one(&mut *connection)
        .await?;
        assert!(
            repository
                .record_consents(
                    user_id,
                    vec![VersionedConsentChange {
                        consent_type: ConsentType::LocationConsent,
                        granted: false,
                        document_version: "location-v1".to_owned(),
                    }],
                    String::new(),
                    String::new(),
                )
                .await?
        );
        let after: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM user_consent WHERE user_id = $1 AND consent_type = 'location_consent'",
        )
        .bind(user_id)
        .fetch_one(&mut *connection)
        .await?;
        assert_eq!(after, before);
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, user_id).await;
    database.close().await;
    cleanup_result?;
    test_result
}
