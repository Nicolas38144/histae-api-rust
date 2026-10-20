#![cfg(feature = "postgres-integration")]
use histae_api_rust::{
    catalog::{
        domain::{ProfileQuestionCategory, ProfileQuestionInput, ProfileQuestionPatch},
        pg::PgCatalogRepository,
        service::CatalogService,
    },
    config::{PostgresConfig, SecretString},
    identity::mobile::otp::{BeginOtpDelivery, OtpDeliveryStart, OtpRepository, OtpStore},
    infra::postgres::Database,
    localization::{Language, LocalizationService},
    profiles::{
        domain::{ConsentType, VersionedConsentChange},
        pg::PgProfileRepository,
        store::ProfileStore,
    },
};
use std::{env, fmt, path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;
#[derive(Debug)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PostgreSQL S13 fixture error ({})", self.0)
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
        application_name: "histae-rust-s13-integration",
        root_certificate: env::var("NODE_EXTRA_CA_CERTS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
    })
}

#[tokio::test]
async fn localized_catalog_writes_are_atomic_and_fall_back_without_altering_identifiers()
-> Result<(), Box<dyn std::error::Error>> {
    let db = Database::connect(&local_config()?)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let service = CatalogService::new(Arc::new(PgCatalogRepository::new(db.clone())));
    let localization = LocalizationService::new(Arc::new(
        histae_api_rust::localization::PgLocalizationRepository::new(db.clone()),
    ));
    let name = format!("Test {}", Uuid::new_v4());
    let item = service
        .create_translated_trait(
            name.clone(),
            [
                (Language::En, Some("Kind".into())),
                (Language::Es, Some("Amable".into())),
            ]
            .into(),
        )
        .await
        .map_err(|e| format!("{e:?}"))?;
    let snapshot = localization
        .catalog(true)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(snapshot.traits[&item.id].name(Language::En), "Kind");
    assert_eq!(snapshot.traits[&item.id].name(Language::It), name);
    service
        .update_translated_trait(
            item.id,
            name.clone(),
            [(Language::En, None), (Language::It, Some("Gentile".into()))].into(),
        )
        .await
        .map_err(|e| format!("{e:?}"))?;
    localization.invalidate().await;
    let snapshot = localization
        .catalog(false)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(snapshot.traits[&item.id].name(Language::En), name);
    assert_eq!(snapshot.traits[&item.id].name(Language::It), "Gentile");
    assert!(
        service
            .update_translated_trait(item.id, "Unchanged".into(), [(Language::Fr, None)].into())
            .await
            .is_err()
    );
    assert_eq!(
        service
            .traits()
            .await
            .map_err(|e| format!("{e:?}"))?
            .into_iter()
            .find(|v| v.id == item.id)
            .expect("trait")
            .name,
        name
    );
    let question = service
        .create_translated_question(
            ProfileQuestionInput {
                prompt: "Une question originale ?".into(),
                category: ProfileQuestionCategory::Conversation,
                display_order: 9999,
            },
            [
                (Language::En, Some("An original question?".into())),
                (Language::It, Some("Una domanda originale?".into())),
            ]
            .into(),
        )
        .await
        .map_err(|e| format!("{e:?}"))?;
    service
        .update_translated_question(
            question.id,
            ProfileQuestionPatch {
                prompt: None,
                category: None,
                display_order: None,
                supplied: false,
            },
            [(Language::Es, Some("¿Una pregunta original?".into()))].into(),
        )
        .await
        .map_err(|e| format!("{e:?}"))?;
    let snapshot = localization
        .catalog(true)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let mut answers = vec![
        serde_json::json!({"question_id":question.id,"question":question.prompt,"answer":"My own answer stays in its original language"}),
    ];
    snapshot.answers(&mut answers, Language::It);
    assert_eq!(answers[0]["question"], "Una domanda originale?");
    assert_eq!(
        answers[0]["answer"],
        "My own answer stays in its original language"
    );
    for language in Language::ALL {
        assert!(!snapshot.plans["free"].name(language).is_empty());
        assert!(!snapshot.plans["premium"].name(language).is_empty());
        assert!(
            snapshot
                .features
                .values()
                .all(|entry| entry.descriptions.contains_key(&language))
        );
    }
    service
        .delete_trait(item.id)
        .await
        .map_err(|e| format!("{e:?}"))?;
    service
        .delete_question(question.id)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let snapshot = localization
        .catalog(true)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert!(!snapshot.traits.contains_key(&item.id));
    assert!(!snapshot.questions.contains_key(&question.id));
    db.close().await;
    Ok(())
}

#[tokio::test]
async fn language_preferences_consent_evidence_and_otp_replays_remain_independent()
-> Result<(), Box<dyn std::error::Error>> {
    let db = Database::connect(&local_config()?)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let user = Uuid::new_v4();
    let localization = LocalizationService::new(Arc::new(
        histae_api_rust::localization::PgLocalizationRepository::new(db.clone()),
    ));
    {
        let mut connection = db.acquire().await.map_err(|e| format!("{e:?}"))?;
        sqlx::query("INSERT INTO user_account(user_id,phone_number_hash,phone_number_encrypted) VALUES($1,$2,''::bytea)").bind(user).bind(format!("i18n-{user}")).execute(&mut *connection).await.map_err(|e| format!("{e:?}"))?;
    }
    assert_eq!(localization.language(user).await?, Language::Fr);
    localization
        .set_language(user, Language::It)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let session = Uuid::new_v4();
    {
        let mut connection = db.acquire().await.map_err(|e| format!("{e:?}"))?;
        sqlx::query("INSERT INTO refresh_token_family(id,user_id,created_at,last_refreshed_at,expires_at) VALUES($1,$2,clock_timestamp(),clock_timestamp(),clock_timestamp()+INTERVAL '1 hour')").bind(session).bind(user).execute(&mut *connection).await?;
    }
    let account = histae_api_rust::identity::mobile::pg::MobileSessionRepository::new(db.clone())
        .active_account(user, session, "terms-v1", "privacy-v1")
        .await
        .map_err(|e| format!("{e:?}"))?
        .expect("active account");
    assert_eq!(account.preferred_language, Language::It);
    assert!(!account.onboarding_complete);
    let profiles = PgProfileRepository::new(db.clone());
    assert!(
        profiles
            .current_consents(user)
            .await
            .map_err(|e| format!("{e:?}"))?
            .is_empty()
    );
    let change = |language| VersionedConsentChange {
        consent_type: ConsentType::LocationConsent,
        granted: true,
        document_version: "i18n-v1".into(),
        document_language: language,
    };
    profiles
        .record_consents(
            user,
            vec![change(Language::It)],
            String::new(),
            String::new(),
        )
        .await
        .map_err(|e| format!("{e:?}"))?;
    localization
        .set_language(user, Language::En)
        .await
        .map_err(|e| format!("{e:?}"))?;
    profiles
        .record_consents(
            user,
            vec![change(Language::En)],
            String::new(),
            String::new(),
        )
        .await
        .map_err(|e| format!("{e:?}"))?;
    let consents = profiles
        .current_consents(user)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(consents.len(), 1);
    assert_eq!(consents[0].document_language, Language::It);
    let otp = OtpRepository::new(db.clone());
    let input = BeginOtpDelivery {
        id: Uuid::new_v4(),
        phone_hash: format!("i18n-otp-{user}"),
        otp_hash: "not-a-real-code".into(),
        idempotency_key: Uuid::new_v4(),
        ttl: Duration::from_secs(300),
        settlement: Duration::from_secs(30),
        language: Language::Es,
    };
    assert!(matches!(
        otp.begin(input.clone()).await?,
        OtpDeliveryStart::Created(_)
    ));
    let mut changed = input.clone();
    changed.language = Language::It;
    assert_eq!(otp.begin(changed).await?, OtpDeliveryStart::Conflict);
    assert!(matches!(
        otp.begin(input.clone()).await?,
        OtpDeliveryStart::Existing(_, _)
    ));
    {
        let mut connection = db.acquire().await.map_err(|e| format!("{e:?}"))?;
        sqlx::query("DELETE FROM otp_verification WHERE id=$1")
            .bind(input.id)
            .execute(&mut *connection)
            .await
            .map_err(|e| format!("{e:?}"))?;
        sqlx::query("SELECT fct_anonymize_user($1)")
            .bind(user)
            .execute(&mut *connection)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let language: String =
            sqlx::query_scalar("SELECT preferred_language FROM user_account WHERE user_id=$1")
                .bind(user)
                .fetch_one(&mut *connection)
                .await
                .map_err(|e| format!("{e:?}"))?;
        assert_eq!(language, "fr");
        sqlx::query("DELETE FROM user_account WHERE user_id=$1")
            .bind(user)
            .execute(&mut *connection)
            .await
            .map_err(|e| format!("{e:?}"))?;
    }
    assert!(localization.language(user).await.is_err());
    assert!(localization.set_language(user, Language::Es).await.is_err());
    db.close().await;
    Ok(())
}
