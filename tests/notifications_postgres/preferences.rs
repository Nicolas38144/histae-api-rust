use super::*;
use histae_api_rust::notifications::preferences::{NotificationPreferenceRepository, PreferenceChanges};

#[tokio::test]
async fn push_preferences_mutes_and_discreet_copy_are_rechecked_without_hiding_matches()
    -> Result<(), Box<dyn std::error::Error>> {
    let database = Database::connect(&local_config()?).await
        .map_err(|e| FixtureError(e.safe_code()))?;
    let mut users = [Uuid::new_v4(),Uuid::new_v4(),Uuid::new_v4()];
    users.sort();
    let session = Uuid::new_v4();
    fixture(&database,users[0],&[session]).await?;
    fixture(&database,users[1],&[]).await?;
    fixture(&database,users[2],&[]).await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let choices = NotificationPreferenceRepository::new(database.clone());
        let devices = PgNotificationRepository::new(database.clone());
        devices.register(users[0],session,DeviceRegistration {
            token: format!("preferences-device-{}",users[0]), platform: DevicePlatform::Android,
            app_version: None, language: None,
        }).await?;
        let initial = choices.get(users[0]).await?.ok_or(FixtureError("default_preferences"))?;
        assert!(initial.messages && initial.new_matches && initial.continuations && initial.photos
            && initial.reminders && initial.account_security && !initial.discreet);
        let match_id = Uuid::new_v4();
        let mut c = database.acquire().await?;
        sqlx::query("INSERT INTO user_profile (user_id,firstname,birthdate) VALUES ($1,'Lucas','1990-01-01')")
            .bind(users[1]).execute(&mut *c).await?;
        sqlx::query("INSERT INTO match_init(id,user1_id,user2_id,status,expires_at)
            VALUES ($1,$2,$3,'active',clock_timestamp()+interval '1 day')")
            .bind(match_id).bind(users[0]).bind(users[1]).execute(&mut *c).await?;
        for user in &users[..2] {
            sqlx::query("INSERT INTO match_state(match_id,user_id) VALUES ($1,$2)")
                .bind(match_id).bind(user).execute(&mut *c).await?;
        }
        enqueue_notification(&mut c,users[0],&Uuid::new_v4().to_string(),
            &NotificationIntent::NewMatch { match_id }).await?;
        let push_id: Uuid = sqlx::query_scalar("SELECT d.id FROM notification_push_delivery d
            JOIN notification n ON n.id=d.notification_id WHERE n.user_id=$1")
            .bind(users[0]).fetch_one(&mut *c).await?;
        drop(c);
        let delivery = PgNotificationDeliveryStore::new(database.clone());
        assert_eq!(delivery.find_deliverable(push_id).await?.ok_or(FixtureError("initial_push"))?
            .partner_firstname.as_deref(),Some("Lucas"));
        let changed = choices.update(users[0],PreferenceChanges { discreet: Some(true),
            ..Default::default() }).await?.ok_or(FixtureError("changed_preferences"))?;
        assert!(changed.discreet && changed.messages && changed.new_matches);
        assert!(delivery.find_deliverable(push_id).await?.ok_or(FixtureError("discreet_push"))?
            .partner_firstname.is_none());
        choices.update(users[0],PreferenceChanges { new_matches: Some(false),
            ..Default::default() }).await?;
        assert!(delivery.find_deliverable(push_id).await?.is_none());
        choices.update(users[0],PreferenceChanges { new_matches: Some(true),
            ..Default::default() }).await?;
        assert!(choices.get_mute(users[2],match_id).await?.is_none());
        assert!(choices.set_mute(users[2],match_id,true,None).await?.is_none());
        choices.set_mute(users[0],match_id,true,None).await?;
        assert!(delivery.find_deliverable(push_id).await?.is_none());
        assert!(!choices.get_mute(users[1],match_id).await?.ok_or(FixtureError("partner_mute"))?.muted);
        let mut c = database.acquire().await?;
        let suppressed_id = Uuid::new_v4().to_string();
        enqueue_notification(&mut c,users[0],&suppressed_id,
            &NotificationIntent::NewMatch { match_id }).await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM notification_push_delivery d
            JOIN notification n ON n.id=d.notification_id WHERE n.user_id=$1")
            .bind(users[0]).fetch_one(&mut *c).await?;
        assert_eq!(count,1,"muted events must not accumulate jobs for later replay");
        sqlx::query("UPDATE match_notification_preference SET muted_until=clock_timestamp()-interval '1 second'
            WHERE user_id=$1 AND match_id=$2").bind(users[0]).bind(match_id).execute(&mut *c).await?;
        drop(c);
        assert!(!choices.get_mute(users[0],match_id).await?.ok_or(FixtureError("expired_mute"))?.muted);
        assert!(delivery.find_deliverable(push_id).await?.is_some());
        choices.set_mute(users[0],match_id,false,None).await?;
        let mut c = database.acquire().await?;
        let visible: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM match_init WHERE id=$1 AND status='active')")
            .bind(match_id).fetch_one(&mut *c).await?;
        assert!(visible,"mutes must not change match visibility or status");
        drop(c);
        Ok(())
    }.await;
    for user in users { cleanup(&database,user,&[]).await?; }
    database.close().await;
    result
}
