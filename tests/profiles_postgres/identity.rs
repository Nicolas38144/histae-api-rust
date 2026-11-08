use super::*;
use histae_api_rust::administration::{
    domain::{IdentityCorrectionResult, ProfileIdentityCorrection},
    pg::PgAdministrationStore,
    store::AdministrationStore,
};
use histae_api_rust::identity::admin_role::AdminRole;
use sqlx::Acquire as _;

#[tokio::test]
async fn identity_is_fixed_after_creation_and_admin_corrections_are_audited_and_consent_bound()
-> Result<(), Box<dyn std::error::Error>> {
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
            .bind(actor)
            .execute(&mut *connection)
            .await?;
        // A missing profile may be created once; retries of the same identity work.
        sqlx::query("DELETE FROM user_profile WHERE user_id=$1")
            .bind(target)
            .execute(&mut *connection)
            .await?;
        drop(connection);
        let original_date = chrono::NaiveDate::from_ymd_opt(1990, 1, 1).expect("date");
        let changed_date = chrono::NaiveDate::from_ymd_opt(1991, 1, 1).expect("date");
        let input = |firstname: &str, birthdate, sex| ProfileInput {
            firstname: firstname.to_owned(),
            birthdate,
            sex,
            bio: Some("A biography that stays editable".to_owned()),
            bio_moderation: Some(TextModerator.analyze("A biography that stays editable")),
        };
        for _ in 0..2 {
            assert_eq!(
                repository
                    .upsert_profile(
                        target,
                        input("Alice", original_date, Some(Sex::Female)),
                        legal(),
                    )
                    .await?,
                WriteOutcome::Updated
            );
        }
        for (name, date, sex) in [
            ("Other", original_date, Some(Sex::Female)),
            ("Alice", changed_date, Some(Sex::Female)),
            ("Alice", original_date, Some(Sex::Male)),
            ("Alice", original_date, None),
        ] {
            let mut changed = input(name, date, sex);
            changed.bio = None;
            assert_eq!(
                repository.upsert_profile(target, changed, legal()).await?,
                WriteOutcome::IdentityLocked
            );
        }
        let unchanged = repository
            .find_profile(target)
            .await?
            .ok_or(FixtureError("profile"))?;
        assert_eq!(unchanged.firstname, "Alice");
        assert_eq!(unchanged.birthdate, original_date);
        assert_eq!(unchanged.sex, Some(Sex::Female));
        assert_eq!(
            unchanged.bio.as_deref(),
            Some("A biography that stays editable")
        );
        let correction = ProfileIdentityCorrection {
            firstname: "Ada".to_owned(),
            birthdate: changed_date,
            sex: Some(Sex::Male),
        };
        assert_eq!(
            admin_store
                .correct_profile_identity(
                    target,
                    correction.clone(),
                    "Owner correction".to_owned(),
                    actor,
                    AdminRole::Superadmin,
                    "sensitive-v1".to_owned(),
                )
                .await?,
            IdentityCorrectionResult::Forbidden
        );
        assert_eq!(
            admin_store
                .correct_profile_identity(
                    actor,
                    correction.clone(),
                    "Owner correction".to_owned(),
                    actor,
                    AdminRole::Admin,
                    "sensitive-v1".to_owned(),
                )
                .await?,
            IdentityCorrectionResult::Forbidden
        );
        assert_eq!(
            admin_store
                .correct_profile_identity(
                    target,
                    correction.clone(),
                    "Owner correction".to_owned(),
                    actor,
                    AdminRole::Admin,
                    "sensitive-v1".to_owned(),
                )
                .await?,
            IdentityCorrectionResult::Updated
        );
        let corrected = repository
            .find_profile(target)
            .await?
            .ok_or(FixtureError("profile"))?;
        assert_eq!(corrected.firstname, "Ada");
        assert_eq!(corrected.birthdate, changed_date);
        assert_eq!(corrected.sex, Some(Sex::Male));
        assert_eq!(corrected.bio, unchanged.bio);
        assert_eq!(
            repository
                .upsert_profile(
                    target,
                    input("Alice", original_date, Some(Sex::Female)),
                    legal(),
                )
                .await?,
            WriteOutcome::IdentityLocked
        );
        let mut connection = database.acquire().await?;
        let audit: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM data_access_log WHERE accessed_user_id=$1
             AND accessor_id=$2 AND action='admin_correct_profile_identity'",
        )
        .bind(target)
        .bind(actor)
        .fetch_one(&mut *connection)
        .await?;
        assert_eq!(audit, 1);
        drop(connection);
        let change_consent = |granted| VersionedConsentChange {
            consent_type: ConsentType::SensitiveDataConsent,
            granted,
            document_version: "sensitive-v1".to_owned(),
            document_language: histae_api_rust::localization::Language::Fr,
        };
        repository
            .record_consents(
                target,
                vec![change_consent(false)],
                String::new(),
                String::new(),
            )
            .await?;
        assert_eq!(
            admin_store
                .correct_profile_identity(
                    target,
                    correction,
                    "Owner correction".to_owned(),
                    actor,
                    AdminRole::Admin,
                    "sensitive-v1".to_owned(),
                )
                .await?,
            IdentityCorrectionResult::ConsentRequired
        );
        assert_eq!(
            repository
                .find_profile(target)
                .await?
                .ok_or(FixtureError("profile"))?
                .sex,
            None
        );
        repository
            .record_consents(
                target,
                vec![change_consent(true)],
                String::new(),
                String::new(),
            )
            .await?;
        // Withdrawing/re-granting consent cannot bypass the mobile identity lock.
        assert_eq!(
            repository
                .upsert_profile(
                    target,
                    input("Ada", changed_date, Some(Sex::Female)),
                    legal(),
                )
                .await?,
            WriteOutcome::IdentityLocked
        );
        repository
            .record_consents(
                target,
                vec![change_consent(false)],
                String::new(),
                String::new(),
            )
            .await?;
        let clear = ProfileIdentityCorrection {
            firstname: "Ada".to_owned(),
            birthdate: changed_date,
            sex: None,
        };
        assert_eq!(
            admin_store
                .correct_profile_identity(
                    target,
                    clear.clone(),
                    "Owner correction".to_owned(),
                    actor,
                    AdminRole::Admin,
                    "sensitive-v1".to_owned(),
                )
                .await?,
            IdentityCorrectionResult::Updated,
            "clearing sensitive data does not require consent to store it"
        );
        sqlx::query("UPDATE user_account SET is_banned=true WHERE user_id=$1")
            .bind(actor)
            .execute(database.acquire().await?.as_mut())
            .await?;
        assert_eq!(
            admin_store
                .correct_profile_identity(
                    target,
                    clear,
                    "Owner correction".to_owned(),
                    actor,
                    AdminRole::Admin,
                    "sensitive-v1".to_owned(),
                )
                .await?,
            IdentityCorrectionResult::Forbidden
        );
        sqlx::query("UPDATE user_account SET is_banned=false,role='superadmin' WHERE user_id=$1")
            .bind(actor)
            .execute(database.acquire().await?.as_mut())
            .await?;
        assert_eq!(
            admin_store
                .correct_profile_identity(
                    actor,
                    ProfileIdentityCorrection {
                        firstname: "Operator".to_owned(),
                        birthdate: original_date,
                        sex: None,
                    },
                    "Owner correction".to_owned(),
                    actor,
                    AdminRole::Superadmin,
                    "sensitive-v1".to_owned(),
                )
                .await?,
            IdentityCorrectionResult::Updated,
            "the superadmin may correct an administrator profile, including their own"
        );
        Ok(())
    }
    .await;
    let target_cleanup = cleanup(&database, target).await;
    let actor_cleanup = cleanup(&database, actor).await;
    database.close().await;
    target_cleanup?;
    actor_cleanup?;
    result
}

#[tokio::test]
async fn concurrent_initial_profiles_choose_one_identity_without_partial_overwrites()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?).await?;
    let user = Uuid::new_v4();
    fixture(&database, user).await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let repository = PgProfileRepository::new(database.clone());
        sqlx::query("DELETE FROM user_profile WHERE user_id=$1")
            .bind(user)
            .execute(database.acquire().await?.as_mut())
            .await?;
        let input = |name: &str| ProfileInput {
            firstname: name.to_owned(),
            birthdate: chrono::NaiveDate::from_ymd_opt(1990, 1, 1).expect("date"),
            sex: Some(Sex::Female),
            bio: Some(name.to_owned()),
            bio_moderation: Some(TextModerator.analyze(name)),
        };
        let (first, second) = tokio::join!(
            repository.upsert_profile(user, input("Alice"), legal()),
            repository.upsert_profile(user, input("Ada"), legal()),
        );
        let outcomes = [first?, second?];
        assert_eq!(
            outcomes
                .iter()
                .filter(|&&r| r == WriteOutcome::Updated)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|&&r| r == WriteOutcome::IdentityLocked)
                .count(),
            1
        );
        let stored = repository
            .find_profile(user)
            .await?
            .ok_or(FixtureError("race_profile"))?;
        let winner = if outcomes[0] == WriteOutcome::Updated {
            "Alice"
        } else {
            "Ada"
        };
        assert_eq!(stored.firstname, winner);
        assert_eq!(stored.bio.as_deref(), Some(winner));
        Ok(())
    }
    .await;
    let cleaned = cleanup(&database, user).await;
    database.close().await;
    cleaned?;
    result
}

async fn wait_for_account_lock(
    database: &Database,
    blocker: i32,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut observer = database.acquire().await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                SELECT 1 FROM pg_stat_activity WHERE datname=current_database()
                  AND usename=current_user AND $1=ANY(pg_blocking_pids(pid)))",
            )
            .bind(blocker)
            .fetch_one(&mut *observer)
            .await?;
            if waiting {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn corrections_wait_for_role_and_consent_changes_without_writing_or_auditing_rejections()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?).await?;
    let target = Uuid::new_v4();
    let actor = Uuid::new_v4();
    fixture(&database, target).await?;
    fixture(&database, actor).await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let store = PgAdministrationStore::new(database.clone());
        let profiles = PgProfileRepository::new(database.clone());
        for demote_actor in [true, false] {
            let mut connection = database.acquire().await?;
            sqlx::query("UPDATE user_account SET role='admin' WHERE user_id=$1")
                .bind(actor)
                .execute(&mut *connection)
                .await?;
            let mut transaction = connection.begin().await?;
            let blocker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut *transaction)
                .await?;
            if demote_actor {
                sqlx::query("UPDATE user_account SET role='user' WHERE user_id=$1")
                    .bind(actor)
                    .execute(&mut *transaction)
                    .await?;
            } else {
                sqlx::query("SELECT user_id FROM user_account WHERE user_id=$1 FOR UPDATE")
                    .bind(target)
                    .execute(&mut *transaction)
                    .await?;
                sqlx::query(
                    "UPDATE user_consent SET granted=false,withdrawn_at=clock_timestamp()
                    WHERE user_id=$1 AND consent_type='sensitive_data_consent'",
                )
                .bind(target)
                .execute(&mut *transaction)
                .await?;
                sqlx::query("UPDATE user_profile SET sex=NULL WHERE user_id=$1")
                    .bind(target)
                    .execute(&mut *transaction)
                    .await?;
            }
            let correction = store.correct_profile_identity(
                target,
                ProfileIdentityCorrection {
                    firstname: "Ada".to_owned(),
                    birthdate: chrono::NaiveDate::from_ymd_opt(1991, 1, 1).expect("date"),
                    sex: Some(Sex::Male),
                },
                "Owner correction".to_owned(),
                actor,
                AdminRole::Admin,
                "sensitive-v1".to_owned(),
            );
            let commit = async {
                wait_for_account_lock(&database, blocker).await?;
                transaction.commit().await?;
                Ok::<_, Box<dyn std::error::Error>>(())
            };
            let (corrected, committed) = tokio::join!(correction, commit);
            committed?;
            assert_eq!(
                corrected?,
                if demote_actor {
                    IdentityCorrectionResult::Forbidden
                } else {
                    IdentityCorrectionResult::ConsentRequired
                }
            );
            let stored = profiles
                .find_profile(target)
                .await?
                .ok_or(FixtureError("rejected_profile"))?;
            assert_eq!(stored.firstname, "Alice");
            assert_eq!(
                stored.birthdate,
                chrono::NaiveDate::from_ymd_opt(1990, 1, 1).expect("date")
            );
            assert_eq!(
                stored.sex,
                if demote_actor {
                    Some(Sex::Female)
                } else {
                    None
                }
            );
            let audits: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM data_access_log
                WHERE accessed_user_id=$1 AND action='admin_correct_profile_identity'",
            )
            .bind(target)
            .fetch_one(&mut *connection)
            .await?;
            assert_eq!(audits, 0);
        }
        Ok(())
    }
    .await;
    let target_cleanup = cleanup(&database, target).await;
    let actor_cleanup = cleanup(&database, actor).await;
    database.close().await;
    target_cleanup?;
    actor_cleanup?;
    result
}
