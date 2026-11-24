use super::*;
use histae_api_rust::identity::mobile::refresh_recovery::RefreshRecovery;
use sqlx::Connection as _;

#[tokio::test]
async fn recoverable_refresh_survives_lost_responses_without_extending_or_replaying_the_family()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?).await?;
    let user = Uuid::new_v4();
    insert_account(&database, user).await?;
    let repository = Arc::new(MobileSessionRepository::new(database.clone()));
    let tokens = token_service();
    let service = MobileAuthService::new(
        tokens.clone(),
        repository.clone(),
        "terms-v1".into(),
        "privacy-v1".into(),
    );
    let original = tokens.new_refresh_token()?;
    let identity = repository
        .create(user, original.clone())
        .await?
        .ok_or(FixtureError("create"))?;
    let attempt = Uuid::new_v4();
    let (first, simultaneous) = tokio::join!(
        service.refresh_with_attempt(&original.plain, Some(attempt)),
        service.refresh_with_attempt(&original.plain, Some(attempt)),
    );
    let first = first?;
    assert_eq!(simultaneous?.refresh_token, first.refresh_token);
    let mut connection = database.acquire().await?;
    let before: (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("SELECT last_refreshed_at,expires_at FROM refresh_token_family WHERE id=$1")
            .bind(identity.session_id)
            .fetch_one(&mut *connection)
            .await?;
    let (key_hash, encrypted): (String, Vec<u8>) = sqlx::query_as(
        "SELECT rotation_request_hash,rotation_result FROM refresh_tokens WHERE id=$1",
    )
    .bind(original.id)
    .fetch_one(&mut *connection)
    .await?;
    assert_eq!(key_hash, sha256_hex(attempt.as_bytes()));
    assert!(
        !encrypted
            .windows(first.refresh_token.len())
            .any(|part| part == first.refresh_token.as_bytes())
    );
    drop(connection);

    // One committed child, including concurrent retries carrying the same key.
    let (retry_a, retry_b) = tokio::join!(
        service.refresh_with_attempt(&original.plain, Some(attempt)),
        service.refresh_with_attempt(&original.plain, Some(attempt)),
    );
    assert_eq!(retry_a?.refresh_token, first.refresh_token);
    assert_eq!(retry_b?.refresh_token, first.refresh_token);
    let mut connection = database.acquire().await?;
    let after: (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("SELECT last_refreshed_at,expires_at FROM refresh_token_family WHERE id=$1")
            .bind(identity.session_id)
            .fetch_one(&mut *connection)
            .await?;
    assert_eq!(before, after);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM refresh_tokens WHERE family_id=$1")
        .bind(identity.session_id)
        .fetch_one(&mut *connection)
        .await?;
    assert_eq!(count, 2);
    // A rotation committed just before parent expiry must still be recoverable.
    sqlx::query(
        "UPDATE refresh_tokens SET expires_at=clock_timestamp()-INTERVAL '1 second' WHERE id=$1",
    )
    .bind(original.id)
    .execute(&mut *connection)
    .await?;
    drop(connection);
    let maintenance =
        histae_api_rust::privacy::maintenance::PrivacyMaintenanceRepository::new(database.clone());
    maintenance.run_as_leader(chrono::Utc::now(), 1000).await?;
    assert_eq!(
        service
            .refresh_with_attempt(&original.plain, Some(attempt))
            .await?
            .refresh_token,
        first.refresh_token
    );

    // Advancing the child makes the old receipt unusable, without revoking the
    // newer child merely because a legitimate delayed retry arrives.
    let next = service
        .refresh_with_attempt(&first.refresh_token, Some(Uuid::new_v4()))
        .await?;
    assert_eq!(
        service
            .refresh_with_attempt(&original.plain, Some(attempt))
            .await,
        Err(MobileAuthError::InvalidRefreshToken)
    );
    assert!(repository.is_active(user, identity.session_id).await?);
    assert!(
        service
            .refresh_with_attempt(&next.refresh_token, Some(Uuid::new_v4()))
            .await
            .is_ok()
    );

    // A different key remains an authentic replay and revokes its own family.
    let replay_parent = tokens.new_refresh_token()?;
    let replay_family = repository
        .create(user, replay_parent.clone())
        .await?
        .ok_or(FixtureError("replay family"))?;
    let replay_key = Uuid::new_v4();
    service
        .refresh_with_attempt(&replay_parent.plain, Some(replay_key))
        .await?;
    assert_eq!(
        service
            .refresh_with_attempt(&replay_parent.plain, Some(Uuid::new_v4()))
            .await,
        Err(MobileAuthError::InvalidRefreshToken)
    );
    assert!(!repository.is_active(user, replay_family.session_id).await?);
    let mut connection = database.acquire().await?;
    let retained: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM refresh_tokens WHERE family_id=$1 AND rotation_result IS NOT NULL)")
        .bind(replay_family.session_id).fetch_one(&mut *connection).await?;
    assert!(!retained);
    drop(connection);

    // Expired receipts fail closed but do not revoke a current child. A logout
    // or ban also prevents recovery, regardless of the key's authenticity.
    let expiring = tokens.new_refresh_token()?;
    let expiry_family = repository
        .create(user, expiring.clone())
        .await?
        .ok_or(FixtureError("expiry family"))?;
    let expiry_key = Uuid::new_v4();
    service
        .refresh_with_attempt(&expiring.plain, Some(expiry_key))
        .await?;
    let mut connection = database.acquire().await?;
    sqlx::query(
        "UPDATE refresh_tokens SET rotated_at=clock_timestamp()-INTERVAL '8 days' WHERE id=$1",
    )
    .bind(expiring.id)
    .execute(&mut *connection)
    .await?;
    drop(connection);
    assert_eq!(
        service
            .refresh_with_attempt(&expiring.plain, Some(expiry_key))
            .await,
        Err(MobileAuthError::InvalidRefreshToken)
    );
    assert!(repository.is_active(user, expiry_family.session_id).await?);
    let cleaned = maintenance
        .run_as_leader(chrono::Utc::now(), 1000)
        .await?
        .ok_or(FixtureError("maintenance leader"))?;
    assert!(cleaned.expired_refresh_recoveries >= 1);
    let mut connection = database.acquire().await?;
    let receipt_removed: bool =
        sqlx::query_scalar("SELECT rotation_result IS NULL FROM refresh_tokens WHERE id=$1")
            .bind(expiring.id)
            .fetch_one(&mut *connection)
            .await?;
    assert!(receipt_removed);
    drop(connection);
    assert_eq!(
        service
            .refresh_with_attempt(&expiring.plain, Some(expiry_key))
            .await,
        Err(MobileAuthError::InvalidRefreshToken)
    );
    repository
        .revoke(
            user,
            expiry_family.session_id,
            Some(expiry_family.session_id),
        )
        .await?;
    assert_eq!(
        service
            .refresh_with_attempt(&expiring.plain, Some(expiry_key))
            .await,
        Err(MobileAuthError::InvalidRefreshToken)
    );

    // An insert constraint rolls back both the receipt and parent consumption.
    let rollback = tokens.new_refresh_token()?;
    repository.create(user, rollback.clone()).await?;
    let parsed =
        TokenService::parse_refresh_token(&rollback.plain).ok_or(FixtureError("rollback token"))?;
    let mut colliding = tokens.new_refresh_token()?;
    colliding.id = rollback.id;
    let recovery = RefreshRecovery::new(&rollback.plain, Uuid::new_v4(), &colliding.plain)?;
    assert!(
        repository
            .rotate_recoverable(parsed.jti, parsed.hash, colliding, recovery)
            .await
            .is_err()
    );
    assert!(
        service
            .refresh_with_attempt(&rollback.plain, Some(Uuid::new_v4()))
            .await
            .is_ok()
    );
    cleanup(&database, user).await?;
    database.close().await;
    Ok(())
}

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct RecoveryFixture {
    database: Database,
    user: Uuid,
    users: Vec<Uuid>,
    repository: Arc<MobileSessionRepository>,
    tokens: TokenService,
    service: MobileAuthService,
}

impl RecoveryFixture {
    async fn new() -> TestResult<Self> {
        let database = Database::connect(&local_config()?).await?;
        let user = Uuid::new_v4();
        insert_account(&database, user).await?;
        let repository = Arc::new(MobileSessionRepository::new(database.clone()));
        let tokens = token_service();
        let service = MobileAuthService::new(
            tokens.clone(),
            repository.clone(),
            "terms-v1".into(),
            "privacy-v1".into(),
        );
        Ok(Self {
            database,
            user,
            users: vec![user],
            repository,
            tokens,
            service,
        })
    }

    async fn add_user(&mut self, role: &str) -> TestResult<Uuid> {
        let user = Uuid::new_v4();
        insert_account(&self.database, user).await?;
        self.users.push(user);
        sqlx::query("UPDATE user_account SET role=$2 WHERE user_id=$1")
            .bind(user)
            .bind(role)
            .execute(&mut *self.database.acquire().await?)
            .await?;
        Ok(user)
    }

    async fn cleanup(self) -> TestResult {
        sqlx::query("DELETE FROM data_access_log WHERE accessed_user_id=ANY($1::uuid[])")
            .bind(&self.users)
            .execute(&mut *self.database.acquire().await?)
            .await?;
        for user in self.users {
            cleanup(&self.database, user).await?;
        }
        self.database.close().await;
        Ok(())
    }
}

#[tokio::test]
async fn receipt_retention_skips_a_parent_locked_by_an_uncommitted_rotation() -> TestResult {
    let f = RecoveryFixture::new().await?;
    let result: TestResult = async {
        let original = f.tokens.new_refresh_token()?;
        let identity = f.repository.create(f.user, original.clone()).await?.ok_or(FixtureError("create"))?;
        let child = f.tokens.new_refresh_token()?;
        let attempt = Uuid::new_v4();
        let receipt = RefreshRecovery::new(&original.plain, attempt, &child.plain)?;
        // Model expiry after the rotation's validation, before its commit.
        sqlx::query("UPDATE refresh_tokens SET expires_at=clock_timestamp()-INTERVAL '1 second' WHERE id=$1")
            .bind(original.id).execute(&mut *f.database.acquire().await?).await?;
        let mut connection = f.database.acquire().await?;
        let mut pending = connection.begin().await?;
        sqlx::query("UPDATE refresh_tokens SET revoked=true,rotated_at=clock_timestamp(),rotation_request_hash=$2,rotation_result=$3 WHERE id=$1")
            .bind(original.id).bind(receipt.request_hash()).bind(receipt.encrypted_token()).execute(&mut *pending).await?;
        sqlx::query("INSERT INTO refresh_tokens(id,user_id,family_id,parent_token_id,token_hash,jti,expires_at,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(child.id).bind(f.user).bind(identity.session_id).bind(original.id).bind(&child.hash)
            .bind(child.jti).bind(child.expires_at).bind(child.created_at).execute(&mut *pending).await?;
        sqlx::query("UPDATE refresh_token_family SET expires_at=$2,last_refreshed_at=clock_timestamp() WHERE id=$1")
            .bind(identity.session_id).bind(child.expires_at).execute(&mut *pending).await?;
        let maintenance = histae_api_rust::privacy::maintenance::PrivacyMaintenanceRepository::new(f.database.clone());
        tokio::time::timeout(Duration::from_secs(3), maintenance.run_as_leader(chrono::Utc::now(), 1000))
            .await.map_err(|_| FixtureError("retention waited for an in-flight rotation"))??
            .ok_or(FixtureError("maintenance leader"))?;
        pending.commit().await?;
        drop(connection);
        assert_eq!(f.service.refresh_with_attempt(&original.plain, Some(attempt)).await?.refresh_token, child.plain);
        Ok(())
    }.await;
    f.cleanup().await?;
    result
}

#[tokio::test]
async fn banning_purges_every_receipt_without_affecting_another_account() -> TestResult {
    use histae_api_rust::{
        administration::{
            domain::BanResult, pg::PgAdministrationStore, store::AdministrationStore,
        },
        identity::admin_role::AdminRole,
    };
    let mut f = RecoveryFixture::new().await?;
    let admin = f.add_user("admin").await?;
    let other = f.add_user("user").await?;
    let result: TestResult = async {
        let mut recoveries = Vec::new();
        for user in [f.user, f.user, other] {
            let parent = f.tokens.new_refresh_token()?;
            let family = f
                .repository
                .create(user, parent.clone())
                .await?
                .ok_or(FixtureError("create"))?;
            let attempt = Uuid::new_v4();
            let child = f
                .service
                .refresh_with_attempt(&parent.plain, Some(attempt))
                .await?;
            recoveries.push((user, family.session_id, parent, attempt, child));
        }
        let administration = PgAdministrationStore::new(f.database.clone());
        assert_eq!(
            administration
                .set_ban(
                    f.user,
                    true,
                    "Recovery regression".into(),
                    admin,
                    AdminRole::Admin
                )
                .await?,
            BanResult::Updated
        );
        let retained: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM refresh_tokens WHERE user_id=$1 AND rotation_result IS NOT NULL",
        )
        .bind(f.user)
        .fetch_one(&mut *f.database.acquire().await?)
        .await?;
        assert_eq!(retained, 0);
        for (user, family, parent, attempt, child) in &recoveries {
            if *user == f.user {
                assert!(!f.repository.is_active(*user, *family).await?);
                assert_eq!(
                    f.service
                        .refresh_with_attempt(&parent.plain, Some(*attempt))
                        .await,
                    Err(MobileAuthError::InvalidRefreshToken)
                );
            } else {
                assert!(f.repository.is_active(*user, *family).await?);
                assert_eq!(
                    f.service
                        .refresh_with_attempt(&parent.plain, Some(*attempt))
                        .await?
                        .refresh_token,
                    child.refresh_token
                );
            }
        }
        administration
            .set_ban(
                f.user,
                false,
                "Recovery regression".into(),
                admin,
                AdminRole::Admin,
            )
            .await?;
        for (user, _, parent, attempt, _) in &recoveries {
            if *user == f.user {
                assert_eq!(
                    f.service
                        .refresh_with_attempt(&parent.plain, Some(*attempt))
                        .await,
                    Err(MobileAuthError::InvalidRefreshToken)
                );
            }
        }
        Ok(())
    }
    .await;
    f.cleanup().await?;
    result
}

#[tokio::test]
async fn logout_and_session_revocations_remove_healthy_recovery_receipts() -> TestResult {
    let f = RecoveryFixture::new().await?;
    let result: TestResult = async {
        for action in ["logout", "user_revoked", "logout_all"] {
            let parent = f.tokens.new_refresh_token()?;
            let family = f.repository.create(f.user,parent.clone()).await?.ok_or(FixtureError("create"))?;
            let attempt = Uuid::new_v4();
            let child = f.service.refresh_with_attempt(&parent.plain,Some(attempt)).await?;
            assert_eq!(f.service.refresh_with_attempt(&parent.plain,Some(attempt)).await?.refresh_token,child.refresh_token);
            match action {
                "logout" => f.service.logout(f.user,family.session_id,&child.refresh_token,None).await?,
                "user_revoked" => { f.repository.revoke(f.user,family.session_id,Some(family.session_id)).await?; }
                _ => { f.repository.revoke(f.user,family.session_id,None).await?; }
            }
            assert_eq!(f.service.refresh_with_attempt(&parent.plain,Some(attempt)).await,Err(MobileAuthError::InvalidRefreshToken));
            let retained: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM refresh_tokens WHERE family_id=$1 AND rotation_result IS NOT NULL)")
                .bind(family.session_id).fetch_one(&mut *f.database.acquire().await?).await?;
            assert!(!retained);
        }
        Ok(())
    }.await;
    f.cleanup().await?;
    result
}

#[tokio::test]
async fn receipt_integrity_and_child_fingerprint_fail_closed_without_revoking_the_family()
-> TestResult {
    let f = RecoveryFixture::new().await?;
    let result: TestResult = async {
        let parent = f.tokens.new_refresh_token()?;
        let family = f
            .repository
            .create(f.user, parent.clone())
            .await?
            .ok_or(FixtureError("create"))?;
        let attempt = Uuid::new_v4();
        let child = f
            .service
            .refresh_with_attempt(&parent.plain, Some(attempt))
            .await?;
        let original: Vec<u8> =
            sqlx::query_scalar("SELECT rotation_result FROM refresh_tokens WHERE id=$1")
                .bind(parent.id)
                .fetch_one(&mut *f.database.acquire().await?)
                .await?;
        let mut tampered = original.clone();
        tampered[12] ^= 1;
        let different_child = f.tokens.new_refresh_token()?;
        let same_jti = format!(
            "{}:{}",
            TokenService::parse_refresh_token(&child.refresh_token)
                .ok_or(FixtureError("child"))?
                .jti,
            "A".repeat(43)
        );
        for ciphertext in [
            tampered,
            RefreshRecovery::new(&parent.plain, attempt, &different_child.plain)?
                .encrypted_token()
                .to_vec(),
            RefreshRecovery::new(&parent.plain, attempt, &same_jti)?
                .encrypted_token()
                .to_vec(),
        ] {
            sqlx::query("UPDATE refresh_tokens SET rotation_result=$2 WHERE id=$1")
                .bind(parent.id)
                .bind(ciphertext)
                .execute(&mut *f.database.acquire().await?)
                .await?;
            assert!(
                f.service
                    .refresh_with_attempt(&parent.plain, Some(attempt))
                    .await
                    .is_err()
            );
            assert!(f.repository.is_active(f.user, family.session_id).await?);
            let count: i64 =
                sqlx::query_scalar("SELECT count(*) FROM refresh_tokens WHERE family_id=$1")
                    .bind(family.session_id)
                    .fetch_one(&mut *f.database.acquire().await?)
                    .await?;
            assert_eq!(count, 2);
        }
        sqlx::query("UPDATE refresh_tokens SET rotation_result=$2 WHERE id=$1")
            .bind(parent.id)
            .bind(original)
            .execute(&mut *f.database.acquire().await?)
            .await?;
        assert_eq!(
            f.service
                .refresh_with_attempt(&parent.plain, Some(attempt))
                .await?
                .refresh_token,
            child.refresh_token
        );
        Ok(())
    }
    .await;
    f.cleanup().await?;
    result
}
