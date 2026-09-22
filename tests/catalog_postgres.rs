#![cfg(feature = "postgres-integration")]

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use histae_api_rust::catalog::domain::{
    PreparedProfileAnswer, ProfileQuestionCategory, ProfileQuestionInput, ReplaceAnswersOutcome,
    Trait,
};
use histae_api_rust::catalog::pg::{CatalogStore, PgCatalogRepository};
use histae_api_rust::catalog::service::CatalogService;
use histae_api_rust::config::{PostgresConfig, SecretString};
use histae_api_rust::infra::postgres::{Database, DatabaseError, map_sqlx_error};
use histae_api_rust::moderation::text::TextModerator;
use histae_api_rust::profiles::domain::{ModerationReason, ModerationStatus};
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
    .bind(format!("s13-{user_id}"))
    .bind(Vec::<u8>::new())
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query(
        r#"
        INSERT INTO user_profile (user_id, firstname, birthdate)
        VALUES ($1, 'Alice', '1990-01-01')
        "#,
    )
    .bind(user_id)
    .execute(&mut *connection)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

async fn cleanup(
    database: &Database,
    user_id: Uuid,
    question_ids: &[Uuid],
    trait_id: Uuid,
) -> Result<(), DatabaseError> {
    let mut connection = database.acquire().await?;
    sqlx::query("DELETE FROM user_account WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM profile_question WHERE id = ANY($1::uuid[])")
        .bind(question_ids)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query("DELETE FROM trait WHERE id = $1")
        .bind(trait_id)
        .execute(&mut *connection)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

#[tokio::test]
async fn catalogs_preserve_order_atomic_replacement_counts_and_cascades()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?)
        .await
        .map_err(|error| FixtureError(error.safe_code()))?;
    let repository = PgCatalogRepository::new(database.clone());
    let service = CatalogService::new(Arc::new(repository.clone()));
    let user_id = Uuid::new_v4();
    let trait_id = Uuid::new_v4();
    let question_ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    cleanup(&database, user_id, &question_ids, trait_id).await?;
    fixture(&database, user_id).await?;

    let test_result: Result<(), Box<dyn std::error::Error>> = async {
        let plans = service.plans().await.map_err(|_| FixtureError("plans"))?;
        assert!(!plans.is_empty());
        assert!(plans.windows(2).all(|pair| {
            (pair[0].monthly_price_cents, pair[0].code.as_str())
                <= (pair[1].monthly_price_cents, pair[1].code.as_str())
        }));

        let trait_value = Trait {
            id: trait_id,
            name: format!("Trait {trait_id}"),
        };
        repository.create_trait(trait_value.clone()).await?;
        repository.add_trait_to_user(user_id, trait_id).await?;
        repository.add_trait_to_user(user_id, trait_id).await?;
        assert_eq!(
            repository.list_traits_for_user(user_id).await?,
            vec![trait_value]
        );

        for (position, id) in question_ids.iter().copied().enumerate() {
            repository
                .create_question(
                    id,
                    format!("test_{}", id.simple()),
                    ProfileQuestionInput {
                        prompt: format!("Question S13 {id} ?"),
                        category: ProfileQuestionCategory::Conversation,
                        display_order: 9_000 + i32::try_from(position)?,
                    },
                )
                .await?;
        }

        assert_eq!(
            repository
                .replace_answers_for_user(
                    Uuid::new_v4(),
                    vec![PreparedProfileAnswer {
                        question_id: question_ids[0],
                        answer: "Réponse pour un profil absent".to_owned(),
                        moderation: TextModerator.analyze("Réponse pour un profil absent"),
                    }],
                )
                .await?,
            ReplaceAnswersOutcome::ProfileNotFound
        );

        let answers = vec![
            PreparedProfileAnswer {
                question_id: question_ids[1],
                answer: "Première réponse ordonnée".to_owned(),
                moderation: TextModerator.analyze("Première réponse ordonnée"),
            },
            PreparedProfileAnswer {
                question_id: question_ids[0],
                answer: "Contacte moi à alice@example.com".to_owned(),
                moderation: TextModerator.analyze("Contacte moi à alice@example.com"),
            },
            PreparedProfileAnswer {
                question_id: question_ids[2],
                answer: "Troisième réponse ordonnée".to_owned(),
                moderation: TextModerator.analyze("Troisième réponse ordonnée"),
            },
        ];
        assert_eq!(
            repository
                .replace_answers_for_user(user_id, answers)
                .await?,
            ReplaceAnswersOutcome::Updated
        );
        let current = repository.list_answers_for_user(user_id).await?;
        assert_eq!(
            current
                .iter()
                .map(|answer| answer.position)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(current[0].question_id, question_ids[1]);
        assert_eq!(current[1].moderation_status, ModerationStatus::Pending);
        assert_eq!(
            current[1].moderation_reasons,
            vec![ModerationReason::PersonalContact]
        );

        let missing_question = Uuid::new_v4();
        assert_eq!(
            repository
                .replace_answers_for_user(
                    user_id,
                    vec![PreparedProfileAnswer {
                        question_id: missing_question,
                        answer: "Cette réponse ne doit pas remplacer les autres".to_owned(),
                        moderation: TextModerator
                            .analyze("Cette réponse ne doit pas remplacer les autres"),
                    }],
                )
                .await?,
            ReplaceAnswersOutcome::QuestionNotFound
        );
        assert_eq!(repository.list_answers_for_user(user_id).await?.len(), 3);

        let admin = repository.list_questions_for_admin().await?;
        let counts = question_ids
            .iter()
            .map(|id| {
                admin
                    .iter()
                    .find(|question| question.question.id == *id)
                    .map(|question| question.answer_count)
                    .ok_or(FixtureError("answer_count"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(counts, vec![1, 1, 1]);

        let deleted_answer_id: Uuid = {
            let mut connection = database.acquire().await?;
            sqlx::query_scalar(
                "SELECT id FROM user_profile_answer WHERE user_id = $1 AND question_id = $2",
            )
            .bind(user_id)
            .bind(question_ids[0])
            .fetch_one(&mut *connection)
            .await?
        };
        assert!(repository.delete_question(question_ids[0]).await?);
        let mut connection = database.acquire().await?;
        let answer_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_profile_answer WHERE id = $1)")
                .bind(deleted_answer_id)
                .fetch_one(&mut *connection)
                .await?;
        let moderation_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM content_moderation_case WHERE profile_answer_id = $1)",
        )
        .bind(deleted_answer_id)
        .fetch_one(&mut *connection)
        .await?;
        assert!(!answer_exists);
        assert!(!moderation_exists);
        Ok(())
    }
    .await;

    let cleanup_result = cleanup(&database, user_id, &question_ids, trait_id).await;
    database.close().await;
    cleanup_result?;
    test_result
}
