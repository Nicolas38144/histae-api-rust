#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use histae_api_rust::config::{LegalConfig, PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::moderation::text::TextModerator;
use histae_api_rust::profiles::domain::{
    ConsentType, LookingFor, PreferencesInput, PresenceInput, ProfileInput, Sex,
    VersionedConsentChange, WriteOutcome,
};
use histae_api_rust::profiles::pg::PgProfileRepository;
use histae_api_rust::profiles::store::ProfileStore;
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
        application_name: "histae-rust-s12-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
    })
}

fn legal() -> LegalConfig {
    LegalConfig {
        document_dates: Default::default(),
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
async fn presence_is_coarse_and_cell_changes_are_serialized_without_delaying_same_cell_refreshes()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?).await?;
    let repository = PgProfileRepository::new(database.clone());
    let user_id = Uuid::new_v4();
    fixture(&database, user_id).await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let mut connection = database.acquire().await?;
        sqlx::query("DELETE FROM user_presence WHERE user_id=$1").bind(user_id).execute(&mut *connection).await?;
        drop(connection);
        let now = DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).expect("timestamp");
        let input = |latitude, longitude, updated_at| PresenceInput { latitude, longitude, updated_at };
        assert_eq!(repository.upsert_presence(user_id, input(48.856613, 2.352222, now), legal()).await?, WriteOutcome::Updated);
        let mut connection = database.acquire().await?;
        let centre: (f64, f64) = sqlx::query_as("SELECT latitude::double precision, longitude::double precision FROM user_presence WHERE user_id=$1")
            .bind(user_id).fetch_one(&mut *connection).await?;
        assert_ne!(centre, (48.856613, 2.352222));
        drop(connection);
        let later = now + TimeDelta::seconds(299);
        assert_eq!(repository.upsert_presence(user_id, input(centre.0 + 0.001, centre.1 + 0.001, later), legal()).await?, WriteOutcome::Updated);
        assert_eq!(repository.upsert_presence(user_id, input(45.76, 4.84, later), legal()).await?, WriteOutcome::PresenceUpdateLimited);
        let mut connection = database.acquire().await?;
        let timestamps: (DateTime<Utc>, DateTime<Utc>) = sqlx::query_as("SELECT updated_at, location_changed_at FROM user_presence WHERE user_id=$1")
            .bind(user_id).fetch_one(&mut *connection).await?;
        assert_eq!(timestamps, (later, now));
        drop(connection);
        let next = now + TimeDelta::seconds(300);
        let (first, second) = tokio::join!(
            repository.upsert_presence(user_id, input(45.76, 4.84, next), legal()),
            repository.upsert_presence(user_id, input(44.84, -0.58, next), legal()),
        );
        let results = [first?, second?];
        assert_eq!(results.iter().filter(|value| **value == WriteOutcome::Updated).count(), 1);
        assert_eq!(results.iter().filter(|value| **value == WriteOutcome::PresenceUpdateLimited).count(), 1);
        Ok(())
    }.await;
    cleanup(&database, user_id).await?;
    result
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
            document_language: histae_api_rust::localization::Language::Fr,
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
            document_language: histae_api_rust::localization::Language::Fr,
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
            document_language: histae_api_rust::localization::Language::Fr,
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


#[tokio::test]
async fn identity_is_fixed_after_creation_and_admin_corrections_are_audited_and_consent_bound()
-> Result<(), Box<dyn std::error::Error>> {
    use histae_api_rust::administration::{
        domain::{IdentityCorrectionResult, ProfileIdentityCorrection},
        pg::PgAdministrationStore,
        store::AdministrationStore,
    };
    use histae_api_rust::identity::admin_role::AdminRole;
    let database = Database::connect(&local_config()?).await?;
    let repository = PgProfileRepository::new(database.clone());
    let admin_store = PgAdministrationStore::new(database.clone());
    let target = Uuid::new_v4();
    let actor = Uuid::new_v4();
    fixture(&database, target).await?;
    fixture(&database, actor).await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let mut connection = database.acquire().await?;
        sqlx::query("UPDATE user_account SET role='admin' WHERE user_id=$1")
            .bind(actor).execute(&mut *connection).await?;
        // A missing profile may be created once; retries of the same identity work.
        sqlx::query("DELETE FROM user_profile WHERE user_id=$1")
            .bind(target).execute(&mut *connection).await?;
        drop(connection);
        let original_date = chrono::NaiveDate::from_ymd_opt(1990, 1, 1).expect("date");
        let changed_date = chrono::NaiveDate::from_ymd_opt(1991, 1, 1).expect("date");
        let input = |firstname: &str, birthdate, sex| ProfileInput {
            firstname: firstname.to_owned(), birthdate, sex,
            bio: Some("A biography that stays editable".to_owned()),
            bio_moderation: Some(TextModerator.analyze("A biography that stays editable")),
        };
        for _ in 0..2 {
            assert_eq!(repository.upsert_profile(
                target, input("Alice", original_date, Some(Sex::Female)), legal(),
            ).await?, WriteOutcome::Updated);
        }
        for (name, date, sex) in [
            ("Other", original_date, Some(Sex::Female)),
            ("Alice", changed_date, Some(Sex::Female)),
            ("Alice", original_date, Some(Sex::Male)),
            ("Alice", original_date, None),
        ] {
            let mut changed = input(name, date, sex);
            changed.bio = None;
            assert_eq!(repository.upsert_profile(target, changed, legal()).await?, WriteOutcome::IdentityLocked);
        }
        let unchanged = repository.find_profile(target).await?.ok_or(FixtureError("profile"))?;
        assert_eq!(unchanged.firstname, "Alice");
        assert_eq!(unchanged.birthdate, original_date);
        assert_eq!(unchanged.sex, Some(Sex::Female));
        assert_eq!(unchanged.bio.as_deref(), Some("A biography that stays editable"));
        let correction = ProfileIdentityCorrection {
            firstname: "Ada".to_owned(), birthdate: changed_date, sex: Some(Sex::Male),
        };
        assert_eq!(admin_store.correct_profile_identity(
            target, correction.clone(), "Owner correction".to_owned(), actor,
            AdminRole::Superadmin, "sensitive-v1".to_owned(),
        ).await?, IdentityCorrectionResult::Forbidden);
        assert_eq!(admin_store.correct_profile_identity(
            actor, correction.clone(), "Owner correction".to_owned(), actor,
            AdminRole::Admin, "sensitive-v1".to_owned(),
        ).await?, IdentityCorrectionResult::Forbidden);
        assert_eq!(admin_store.correct_profile_identity(
            target, correction.clone(), "Owner correction".to_owned(), actor,
            AdminRole::Admin, "sensitive-v1".to_owned(),
        ).await?, IdentityCorrectionResult::Updated);
        let corrected = repository.find_profile(target).await?.ok_or(FixtureError("profile"))?;
        assert_eq!(corrected.firstname, "Ada");
        assert_eq!(corrected.birthdate, changed_date);
        assert_eq!(corrected.sex, Some(Sex::Male));
        assert_eq!(corrected.bio, unchanged.bio);
        assert_eq!(repository.upsert_profile(
            target, input("Alice", original_date, Some(Sex::Female)), legal(),
        ).await?, WriteOutcome::IdentityLocked);
        let mut connection = database.acquire().await?;
        let audit: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM data_access_log WHERE accessed_user_id=$1
             AND accessor_id=$2 AND action='admin_correct_profile_identity'",
        ).bind(target).bind(actor).fetch_one(&mut *connection).await?;
        assert_eq!(audit, 1);
        drop(connection);
        let change_consent = |granted| VersionedConsentChange {
            consent_type: ConsentType::SensitiveDataConsent, granted,
            document_version: "sensitive-v1".to_owned(),
            document_language: histae_api_rust::localization::Language::Fr,
        };
        repository.record_consents(target, vec![change_consent(false)], String::new(), String::new()).await?;
        assert_eq!(admin_store.correct_profile_identity(
            target, correction, "Owner correction".to_owned(), actor,
            AdminRole::Admin, "sensitive-v1".to_owned(),
        ).await?, IdentityCorrectionResult::ConsentRequired);
        assert_eq!(repository.find_profile(target).await?.ok_or(FixtureError("profile"))?.sex, None);
        repository.record_consents(target, vec![change_consent(true)], String::new(), String::new()).await?;
        // Withdrawing/re-granting consent cannot bypass the mobile identity lock.
        assert_eq!(repository.upsert_profile(
            target, input("Ada", changed_date, Some(Sex::Female)), legal(),
        ).await?, WriteOutcome::IdentityLocked);
        Ok(())
    }.await;
    let target_cleanup = cleanup(&database, target).await;
    let actor_cleanup = cleanup(&database, actor).await;
    database.close().await;
    target_cleanup?;
    actor_cleanup?;
    result
}
