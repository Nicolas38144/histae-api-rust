#![cfg(feature = "postgres-integration")]

use chrono::{DateTime, TimeDelta, Utc};
use histae_api_rust::{
    config::{AppConfig, Environment},
    infra::postgres::Database,
    matches::{
        domain::{
            ContinuationResult, MatchCommandResult, MatchRecord, MatchStatus,
            MessageCreationResult, PublicUserMatch,
        },
        maintenance::{MatchMaintenanceRepository, MatchMaintenanceResult},
        pg::{PgMatchMessageRepository, PgMatchRepository},
        store::{MatchMessageStore, MatchStore},
    },
    notifications::{
        delivery::{NotificationDeliveryStore, PgNotificationDeliveryStore},
        domain::NotificationType,
    },
};
use sqlx::Acquire as _;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    database: Database,
    matches: PgMatchRepository,
    messages: PgMatchMessageRepository,
    users: [Uuid; 2],
    id: Uuid,
}

impl Fixture {
    async fn new(premium: bool) -> TestResult<Self> {
        let config = AppConfig::from_env()?;
        assert_eq!(config.environment, Environment::Development);
        assert_eq!(config.postgres.database, "histae-dev");
        assert!(matches!(
            config.postgres.host.as_str(),
            "localhost" | "127.0.0.1" | "::1"
        ));
        let database = Database::connect(&config.postgres).await?;
        let users = [Uuid::new_v4(), Uuid::new_v4()];
        let mut connection = database.acquire().await?;
        let mut transaction = connection.begin().await?;
        for user in users {
            sqlx::query(
                "INSERT INTO user_account(user_id,phone_number_hash,phone_number_encrypted)
                 VALUES($1,$2,''::bytea)",
            )
            .bind(user)
            .bind(format!("inactivity-{user}"))
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                "INSERT INTO user_profile(user_id,firstname,birthdate,sex)
                 VALUES($1,'Inactivity test',DATE '2000-01-01','female')",
            )
            .bind(user)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                "INSERT INTO device_token(id,user_id,token,platform) VALUES($1,$2,$3,'android')",
            )
            .bind(Uuid::new_v4())
            .bind(user)
            .bind(format!("inactivity-token-{user}"))
            .execute(&mut *transaction)
            .await?;
            if premium {
                sqlx::query("INSERT INTO user_subscription(user_id,plan) VALUES($1,'premium')")
                    .bind(user)
                    .execute(&mut *transaction)
                    .await?;
            }
        }
        transaction.commit().await?;
        drop(connection);
        let matches = PgMatchRepository::new(database.clone());
        let id = Uuid::new_v4();
        let now = Utc::now();
        matches
            .create(MatchRecord {
                id,
                user1_id: users[0].min(users[1]),
                user2_id: users[0].max(users[1]),
                status: MatchStatus::Active,
                expires_at: now - TimeDelta::seconds(1),
                purge_after: None,
                continuation_initiator_id: None,
                created_at: now,
                last_message_at: None,
            })
            .await?;
        assert_eq!(
            matches.record_continuation(id, users[0], 1).await?,
            ContinuationResult::Pending
        );
        assert_eq!(
            matches.record_continuation(id, users[1], 1).await?,
            ContinuationResult::Confirmed
        );
        Ok(Self {
            messages: PgMatchMessageRepository::new(database.clone()),
            matches,
            database,
            users,
            id,
        })
    }

    async fn expiry(&self) -> TestResult<DateTime<Utc>> {
        Ok(
            sqlx::query_scalar("SELECT expires_at FROM match_init WHERE id=$1")
                .bind(self.id)
                .fetch_one(self.database.acquire().await?.as_mut())
                .await?,
        )
    }

    async fn set_expiry(&self, value: DateTime<Utc>) -> TestResult {
        sqlx::query("UPDATE match_init SET expires_at=$2 WHERE id=$1")
            .bind(self.id)
            .bind(value)
            .execute(self.database.acquire().await?.as_mut())
            .await?;
        Ok(())
    }

    async fn public_match(&self, now: DateTime<Utc>) -> TestResult<PublicUserMatch> {
        let mut rows = self
            .matches
            .list_for_user(self.users[0], 20, 0, None)
            .await?;
        assert_eq!(rows.len(), 1);
        Ok(rows.remove(0).into_public(None, now))
    }

    async fn send(&self, user: Uuid, key: Uuid) -> TestResult<MessageCreationResult> {
        Ok(self
            .messages
            .create_message(Uuid::new_v4(), self.id, user, "A new message".into(), key)
            .await?)
    }

    async fn maintain(
        &self,
        now: DateTime<Utc>,
        batch_size: u32,
        max_batches: u32,
    ) -> TestResult<MatchMaintenanceResult> {
        Ok(MatchMaintenanceRepository::new(self.database.clone())
            .run_as_leader(now, batch_size, max_batches)
            .await?
            .expect("isolated test owns the leadership lock"))
    }

    async fn warning_jobs(&self) -> TestResult<Vec<Uuid>> {
        Ok(sqlx::query_scalar(
            "SELECT d.id FROM notification_push_delivery d
             JOIN notification n ON n.id=d.notification_id
             WHERE n.user_id=ANY($1) AND n.type='match_expiring'",
        )
        .bind(self.users.to_vec())
        .fetch_all(self.database.acquire().await?.as_mut())
        .await?)
    }

    async fn cleanup(&self) -> TestResult {
        sqlx::query(
            "DELETE FROM outbox_event WHERE id IN (
               SELECT d.id FROM notification_push_delivery d
               JOIN notification n ON n.id=d.notification_id WHERE n.user_id=ANY($1)
             )",
        )
        .bind(self.users.to_vec())
        .execute(self.database.acquire().await?.as_mut())
        .await?;
        sqlx::query("DELETE FROM user_account WHERE user_id=ANY($1)")
            .bind(self.users.to_vec())
            .execute(self.database.acquire().await?.as_mut())
            .await?;
        Ok(())
    }
}

#[tokio::test]
async fn archived_matches_reopen_only_on_new_messages_without_charging_again() -> TestResult {
    for premium in [false, true] {
        let fixture = Fixture::new(premium).await?;
        let result: TestResult = async {
            let expiry = fixture.expiry().await?;
            assert!(
                (expiry - Utc::now() - TimeDelta::days(30))
                    .num_seconds()
                    .abs()
                    < 5
            );
            let now = Utc::now();
            fixture.set_expiry(now + TimeDelta::days(20)).await?;
            assert!(fixture.public_match(now).await?.archived);

            let before = fixture.expiry().await?;
            let messages = fixture
                .messages
                .messages_for_user(fixture.id, fixture.users[0], 20, 0, None)
                .await?;
            assert!(matches!(messages, MatchCommandResult::Available(_)));
            assert_eq!(
                fixture.expiry().await?,
                before,
                "reading must not renew inactivity"
            );

            let usage = fixture
                .matches
                .quota_snapshot(fixture.users[0], now)
                .await?
                .used;
            let key = Uuid::new_v4();
            let sent = fixture.send(fixture.users[1], key).await?;
            assert!(matches!(sent, MessageCreationResult::Available(created) if created.created));
            let renewed = fixture.expiry().await?;
            assert!(renewed > before + TimeDelta::days(9));
            assert!(!fixture.public_match(Utc::now()).await?.archived);

            let replay = fixture.send(fixture.users[1], key).await?;
            assert!(
                matches!(replay, MessageCreationResult::Available(created) if !created.created)
            );
            assert_eq!(
                fixture.expiry().await?,
                renewed,
                "a replay is not a new message"
            );
            assert_eq!(
                fixture
                    .matches
                    .quota_snapshot(fixture.users[0], Utc::now())
                    .await?
                    .used,
                usage
            );
            assert_eq!(
                fixture
                    .matches
                    .record_continuation(fixture.id, fixture.users[0], 1)
                    .await?,
                ContinuationResult::AlreadyConfirmed
            );
            Ok(())
        }
        .await;
        fixture.cleanup().await?;
        result?;
    }
    Ok(())
}

#[tokio::test]
async fn inactivity_warnings_are_deduplicated_and_obsolete_pushes_are_suppressed() -> TestResult {
    let fixture = Fixture::new(false).await?;
    let result: TestResult = async {
        let delivery = PgNotificationDeliveryStore::new(fixture.database.clone());
        let deadline = Utc::now() + TimeDelta::days(7) - TimeDelta::seconds(2);
        fixture.set_expiry(deadline).await?;
        let warning_at = deadline - TimeDelta::days(7);
        let before_warning = fixture
            .maintain(warning_at - TimeDelta::microseconds(1), 10, 1)
            .await?;
        assert_eq!(before_warning.warned, 0, "no warning before day 23");
        assert!(fixture.warning_jobs().await?.is_empty());

        let batch = fixture.maintain(warning_at, 10, 1).await?;
        assert_eq!(batch.warned, 1);
        let jobs = fixture.warning_jobs().await?;
        assert_eq!(jobs.len(), 2, "both participants must receive the warning");
        for id in &jobs {
            let notification = delivery
                .find_deliverable(*id)
                .await?
                .expect("relevant warning");
            assert_eq!(
                notification.notification_type,
                NotificationType::MatchExpiring
            );
        }
        assert_eq!(fixture.maintain(warning_at, 10, 1).await?.warned, 0);

        fixture.send(fixture.users[0], Uuid::new_v4()).await?;
        for id in jobs {
            assert!(
                delivery.find_deliverable(id).await?.is_none(),
                "late warning must not be delivered after activity"
            );
        }
        fixture.set_expiry(deadline + TimeDelta::seconds(1)).await?;
        assert_eq!(
            fixture.maintain(Utc::now(), 10, 1).await?.warned,
            1,
            "a later inactivity cycle gets a fresh warning"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
async fn inactivity_deadline_revokes_access_and_purges_without_an_extra_retention_period()
-> TestResult {
    let fixture = Fixture::new(true).await?;
    let result: TestResult = async {
        fixture.send(fixture.users[0], Uuid::new_v4()).await?;
        sqlx::query(
            "INSERT INTO user_report(reporter_id,reported_id,match_id,reason) VALUES($1,$2,$3,'other')",
        )
        .bind(fixture.users[0])
        .bind(fixture.users[1])
        .bind(fixture.id)
        .execute(fixture.database.acquire().await?.as_mut())
        .await?;
        fixture.set_expiry(Utc::now() - TimeDelta::seconds(1)).await?;
        assert!(fixture.matches.list_for_user(fixture.users[0], 20, 0, None).await?.is_empty());
        let sent = fixture.send(fixture.users[1], Uuid::new_v4()).await?;
        assert!(matches!(sent, MessageCreationResult::Unavailable(_)));
        let messages = fixture.messages
            .messages_for_user(fixture.id, fixture.users[0], 20, 0, None).await?;
        assert!(matches!(messages, MatchCommandResult::Unavailable(_)));
        let result = fixture.maintain(Utc::now(), 1, 30).await?;
        assert_eq!(result.purged, 1);
        let removed: bool = sqlx::query_scalar(
            "SELECT NOT EXISTS(SELECT 1 FROM match_init WHERE id=$1)
                AND NOT EXISTS(SELECT 1 FROM chat_message WHERE match_id=$1)
                AND NOT EXISTS(SELECT 1 FROM match_state WHERE match_id=$1)
                AND NOT EXISTS(SELECT 1 FROM notification WHERE payload->>'match_id'=$1::text)",
        )
        .bind(fixture.id)
        .fetch_one(fixture.database.acquire().await?.as_mut())
        .await?;
        assert!(removed);
        let report_kept: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM user_report WHERE reporter_id=$1 AND match_id IS NULL)",
        )
        .bind(fixture.users[0])
        .fetch_one(fixture.database.acquire().await?.as_mut())
        .await?;
        assert!(report_kept, "report records follow their own retention");
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
async fn a_message_holding_the_match_lock_is_not_purged_by_a_stale_maintenance_scan() -> TestResult
{
    let fixture = Fixture::new(false).await?;
    let result: TestResult = async {
        let deadline = Utc::now() + TimeDelta::minutes(10);
        fixture.set_expiry(deadline).await?;
        let mut connection = fixture.database.acquire().await?;
        let mut lock = connection.begin().await?;
        sqlx::query("SELECT id FROM match_init WHERE id=$1 FOR UPDATE")
            .bind(fixture.id)
            .execute(&mut *lock)
            .await?;
        let sender = fixture.messages.clone();
        let (id, user) = (fixture.id, fixture.users[0]);
        let task = tokio::spawn(async move {
            sender
                .create_message(
                    Uuid::new_v4(),
                    id,
                    user,
                    "A timely message".into(),
                    Uuid::new_v4(),
                )
                .await
        });
        assert_eq!(fixture.maintain(deadline, 10, 1).await?.closed_inactive, 0);
        lock.commit().await?;
        assert!(matches!(task.await??, MessageCreationResult::Available(_)));
        assert_eq!(fixture.maintain(deadline, 10, 1).await?.closed_inactive, 0);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}
