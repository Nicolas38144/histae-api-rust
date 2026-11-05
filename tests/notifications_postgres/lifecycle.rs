use super::*;

#[tokio::test]
async fn push_deadlines_follow_match_and_recipient_session_and_replies_expire_within_a_day()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?).await?;
    let mut users = [Uuid::new_v4(), Uuid::new_v4()];
    users.sort();
    let session_id = Uuid::new_v4();
    fixture(&database, users[0], &[session_id]).await?;
    fixture(&database, users[1], &[]).await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        PgNotificationRepository::new(database.clone())
            .register(users[0], session_id, DeviceRegistration {
                language: None,
                token: format!("deadline-{}", users[0]),
                platform: DevicePlatform::Android,
                app_version: None,
            }).await?;
        let match_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let mut connection = database.acquire().await?;
        let deadline: chrono::DateTime<Utc> = sqlx::query_scalar(
            "INSERT INTO match_init (id,user1_id,user2_id,status,expires_at)
             VALUES ($1,$2,$3,'active',clock_timestamp()+INTERVAL '10 minutes') RETURNING expires_at"
        ).bind(match_id).bind(users[0]).bind(users[1])
            .fetch_one(&mut *connection).await?;
        enqueue_notification(&mut connection, users[0], &match_id.to_string(),
            &NotificationIntent::NewMatch { match_id }).await?;
        let push_id: Uuid = sqlx::query_scalar(
            "SELECT delivery.id FROM notification_push_delivery delivery
             JOIN notification n ON n.id=delivery.notification_id WHERE n.user_id=$1"
        ).bind(users[0]).fetch_one(&mut *connection).await?;
        drop(connection);
        let delivery = PgNotificationDeliveryStore::new(database.clone());
        assert_eq!(delivery.find_deliverable(push_id).await?
            .ok_or(FixtureError("deadline_match"))?.expires_at, deadline);

        let mut connection = database.acquire().await?;
        let session_deadline: chrono::DateTime<Utc> = sqlx::query_scalar(
            "UPDATE refresh_token_family SET expires_at=clock_timestamp()+INTERVAL '1 minute'
             WHERE id=$1 RETURNING expires_at"
        ).bind(session_id).fetch_one(&mut *connection).await?;
        drop(connection);
        assert_eq!(delivery.find_deliverable(push_id).await?
            .ok_or(FixtureError("deadline_session"))?.expires_at, session_deadline);

        let mut connection = database.acquire().await?;
        sqlx::query("INSERT INTO data_subject_request (id,user_id,type,status,response,completed_at)
            VALUES ($1,$2,'rectification','completed','Synthetic reply',clock_timestamp())")
            .bind(request_id).bind(users[0]).execute(&mut *connection).await?;
        enqueue_notification(&mut connection, users[0], &request_id.to_string(),
            &NotificationIntent::DataRequestReplied {
                request_id, response_status: "completed".to_owned()
            }).await?;
        let expires_within_day: bool = sqlx::query_scalar(
            "SELECT expires_at-created_at BETWEEN INTERVAL '23 hours' AND INTERVAL '25 hours'
             FROM notification WHERE user_id=$1 AND type='data_request_replied'"
        ).bind(users[0]).fetch_one(&mut *connection).await?;
        assert!(expires_within_day, "a public reply must not remain queued for ninety days");
        sqlx::query("UPDATE match_init SET expires_at=clock_timestamp()-INTERVAL '1 second' WHERE id=$1")
            .bind(match_id).execute(&mut *connection).await?;
        drop(connection);
        assert!(delivery.find_deliverable(push_id).await?.is_none());
        Ok(())
    }.await;
    let first_cleanup = cleanup(&database, users[0], &[]).await;
    let second_cleanup = cleanup(&database, users[1], &[]).await;
    database.close().await;
    first_cleanup?;
    second_cleanup?;
    result
}

/// Run against the guarded local histae-dev database after migration.
#[tokio::test]
async fn lifecycle_notifications_recheck_names_choices_reveals_replies_and_session_targets()
-> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?)
        .await
        .map_err(|e| FixtureError(e.safe_code()))?;
    let mut users = [Uuid::new_v4(), Uuid::new_v4()];
    users.sort();
    let sessions = [Uuid::new_v4(), Uuid::new_v4()];
    fixture(&database, users[0], &sessions).await?;
    fixture(&database, users[1], &[]).await?;
    let devices = PgNotificationRepository::new(database.clone());
    let delivery = PgNotificationDeliveryStore::new(database.clone());
    let result: Result<(), Box<dyn std::error::Error>> = async {
        for (index, session) in sessions.into_iter().enumerate() {
            devices.register(users[0], session, DeviceRegistration { language: Some(histae_api_rust::localization::Language::Fr),
                token: format!("lifecycle-{index}-{}",users[0]), platform: DevicePlatform::Android, app_version: None }).await?;
        }
        let match_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let mut connection = database.acquire().await?;
        for (user, name) in [(users[0], "Alex"), (users[1], "Lucas")] {
            sqlx::query("INSERT INTO user_profile (user_id,firstname,birthdate) VALUES ($1,$2,'1990-01-01')")
                .bind(user).bind(name).execute(&mut *connection).await?;
            let photo = Uuid::new_v4();
            sqlx::query("INSERT INTO user_photo (id,user_id,object_key,status,mime_type,size_bytes,width,height,sha256)
                VALUES ($1,$2,$3,'ready','image/webp',100,10,10,$4)")
                .bind(photo).bind(user).bind(format!("profile-photos/{user}/{photo}.webp"))
                .bind(vec![0_u8;32]).execute(&mut *connection).await?;
            sqlx::query("INSERT INTO content_moderation_case (user_id,content_type,photo_id,status,policy_version)
                VALUES ($1,'photo',$2,'approved','manual_photo_v1')")
                .bind(user).bind(photo).execute(&mut *connection).await?;
        }
        sqlx::query("INSERT INTO match_init (id,user1_id,user2_id,status,expires_at,photo_reveal_version)
            VALUES ($1,$2,$3,'awaiting_continuation',clock_timestamp()+INTERVAL '6 hours',1)")
            .bind(match_id).bind(users[0]).bind(users[1]).execute(&mut *connection).await?;
        for user in users {
            sqlx::query("INSERT INTO match_state (match_id,user_id,revealed,continued) VALUES ($1,$2,true,$3)")
                .bind(match_id).bind(user).bind(user==users[1]).execute(&mut *connection).await?;
        }
        sqlx::query("INSERT INTO data_subject_request (id,user_id,type,status,response,completed_at)
            VALUES ($1,$2,'rectification','completed','Synthetic reply',clock_timestamp())")
            .bind(request_id).bind(users[0]).execute(&mut *connection).await?;
        drop(connection);
        for intent in [
            NotificationIntent::ContinuationRequested { match_id, actor_id: users[1] },
            NotificationIntent::PhotosRevealed { match_id, reveal_version: 1 },
            NotificationIntent::DataRequestReplied { request_id, response_status: "completed".to_owned() },
            NotificationIntent::NewSession { session_id: sessions[1] },
        ] {
            let source = Uuid::new_v4().to_string();
            let transaction_intent = intent.clone();
            database.transaction(|connection| Box::pin(async move {
                enqueue_notification(connection,users[0],&source,&transaction_intent).await
            })).await?;
        }
        let mut connection = database.acquire().await?;
        let rows = sqlx::query_as::<_,(Uuid,String,Option<Uuid>)>("SELECT push.id,n.type,push.session_id FROM notification_push_delivery push JOIN notification n ON n.id=push.notification_id WHERE n.user_id=$1")
            .bind(users[0]).fetch_all(&mut *connection).await?;
        assert_eq!(rows.iter().filter(|(_,kind,_)|kind=="new_session").count(),1);
        assert!(rows.iter().filter(|(_,kind,_)|kind=="new_session").all(|(_,_,session)|*session==Some(sessions[0])));
        let continuation = rows.iter().find(|(_,kind,_)|kind=="continuation_requested").ok_or(FixtureError("continuation_job"))?.0;
        let reveal = rows.iter().find(|(_,kind,_)|kind=="photos_revealed").ok_or(FixtureError("reveal_job"))?.0;
        let reply = rows.iter().find(|(_,kind,_)|kind=="data_request_replied").ok_or(FixtureError("reply_job"))?.0;
        let login = rows.iter().find(|(_,kind,_)|kind=="new_session").ok_or(FixtureError("login_job"))?.0;
        drop(connection);
        assert_eq!(delivery.find_deliverable(continuation).await?.ok_or(FixtureError("deliverable_choice"))?.partner_firstname.as_deref(),Some("Lucas"));
        let mut connection=database.acquire().await?;
        sqlx::query("UPDATE user_profile SET firstname='Lucía' WHERE user_id=$1").bind(users[1]).execute(&mut *connection).await?;
        drop(connection);
        assert_eq!(delivery.find_deliverable(continuation).await?.ok_or(FixtureError("renamed_choice"))?.partner_firstname.as_deref(),Some("Lucía"));
        assert!(delivery.find_deliverable(reveal).await?.is_some());
        assert!(delivery.find_deliverable(reply).await?.is_some());
        assert!(delivery.find_deliverable(login).await?.is_some());
        let mut connection=database.acquire().await?;
        let opened_deadline: chrono::DateTime<Utc> = sqlx::query_scalar(
            "UPDATE refresh_token_family SET expires_at=clock_timestamp()+INTERVAL '5 minutes'
             WHERE id=$1 RETURNING expires_at"
        ).bind(sessions[1]).fetch_one(&mut *connection).await?;
        drop(connection);
        assert_eq!(delivery.find_deliverable(login).await?
            .ok_or(FixtureError("opened_session_deadline"))?.expires_at, opened_deadline);
        let mut connection=database.acquire().await?;
        sqlx::query("UPDATE match_state SET continued=true,revealed=false WHERE match_id=$1 AND user_id=$2").bind(match_id).bind(users[0]).execute(&mut *connection).await?;
        sqlx::query("UPDATE data_subject_request SET response=NULL WHERE id=$1").bind(request_id).execute(&mut *connection).await?;
        sqlx::query("UPDATE refresh_token_family SET revoked_at=clock_timestamp(),revocation_reason='user_revoked' WHERE id=$1").bind(sessions[1]).execute(&mut *connection).await?;
        drop(connection);
        assert!(delivery.find_deliverable(continuation).await?.is_none());
        assert!(delivery.find_deliverable(reveal).await?.is_none());
        assert!(delivery.find_deliverable(reply).await?.is_none());
        assert!(delivery.find_deliverable(login).await?.is_none());
        let mut connection=database.acquire().await?;
        sqlx::query("UPDATE match_state SET revealed=true WHERE match_id=$1").bind(match_id).execute(&mut *connection).await?;
        sqlx::query("UPDATE match_init SET photo_reveal_version=2 WHERE id=$1").bind(match_id).execute(&mut *connection).await?;
        drop(connection);
        assert!(delivery.find_deliverable(reveal).await?.is_none(),"an old grant cannot revive after withdrawal and a new grant");
        let deadline = Utc::now() + TimeDelta::hours(1);
        let mut connection=database.acquire().await?;
        sqlx::query("UPDATE match_init SET status='active',expires_at=$2 WHERE id=$1").bind(match_id).bind(deadline).execute(&mut *connection).await?;
        sqlx::query("UPDATE match_state SET continued=false WHERE match_id=$1").bind(match_id).execute(&mut *connection).await?;
        let intent=NotificationIntent::FirstPeriodEnding { match_id, expires_at: deadline };
        let source=Uuid::new_v4().to_string();
        enqueue_notification(&mut connection,users[0],&source,&intent).await?;
        let reminder: Uuid=sqlx::query_scalar("SELECT delivery.id FROM notification_push_delivery delivery JOIN notification n ON n.id=delivery.notification_id WHERE n.user_id=$1 AND n.type='first_period_ending'")
            .bind(users[0]).fetch_one(&mut *connection).await?;
        drop(connection);
        assert!(delivery.find_deliverable(reminder).await?.is_some());
        let mut connection=database.acquire().await?;
        sqlx::query("UPDATE match_init SET status='awaiting_continuation' WHERE id=$1").bind(match_id).execute(&mut *connection).await?;
        drop(connection);
        assert!(delivery.find_deliverable(reminder).await?.is_none(),"a reminder cannot arrive after the first period");
        Ok(())
    }.await;
    let first_cleanup = cleanup(&database, users[0], &[]).await;
    let second_cleanup = cleanup(&database, users[1], &[]).await;
    database.close().await;
    first_cleanup?;
    second_cleanup?;
    result
}
