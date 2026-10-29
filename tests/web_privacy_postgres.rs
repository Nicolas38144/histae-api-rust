#![cfg(feature = "postgres-integration")]

#[path = "web_privacy/fixture.rs"]
mod fixture;

use axum::http::{StatusCode, header};
use fixture::{Fixture, ORIGIN, PHONE, TestResult};
use histae_api_rust::infra::crypto::sha256_hex;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn origin_and_cookie_guards_precede_field_validation_and_never_accept_target_ids()
-> TestResult {
    let f = Fixture::new(100, 100).await?;
    let result: TestResult = async {
        for (method, path) in [
            ("POST", "/api/web/privacy/deletion-token"),
            ("DELETE", "/api/web/privacy/account"),
            ("DELETE", "/api/web/privacy/data"),
        ] {
            let mut no_origin = f.request(method, path, Some(&f.cookie), "{}");
            no_origin.headers_mut().remove(header::ORIGIN);
            let mut bad_origin = f.request(method, path, Some(&f.cookie), "{}");
            bad_origin
                .headers_mut()
                .insert(header::ORIGIN, "https://www.histae.test.evil.test".parse()?);
            let mut duplicate_origin = f.request(method, path, Some(&f.cookie), "{}");
            duplicate_origin
                .headers_mut()
                .append(header::ORIGIN, ORIGIN.parse()?);
            let mut duplicate_cookie = f.request(method, path, Some(&f.cookie), "{}");
            duplicate_cookie
                .headers_mut()
                .append(header::COOKIE, f.cookie.parse()?);
            let mut bearer = f.request(method, path, None, "{}");
            bearer
                .headers_mut()
                .insert(header::AUTHORIZATION, "Bearer mobile-token".parse()?);
            let admin_cookie = format!("__Host-histae_admin={}", "a".repeat(43));
            let forged_cookie = format!("__Host-histae_billing={}", "a".repeat(43));
            for (request, status, code) in [
                (no_origin, 403, "invalid_web_request_origin"),
                (bad_origin, 403, "invalid_web_request_origin"),
                (duplicate_origin, 403, "invalid_web_request_origin"),
                (duplicate_cookie, 401, "web_authentication_failed"),
                (bearer, 401, "web_authentication_failed"),
                (
                    f.request(method, path, Some(&admin_cookie), "{}"),
                    401,
                    "web_authentication_failed",
                ),
                (
                    f.request(method, path, Some(&forged_cookie), "{}"),
                    401,
                    "web_authentication_failed",
                ),
            ] {
                let (actual, _, body) = f.send(request).await?;
                assert_eq!(actual.as_u16(), status, "{method} {path}: {body}");
                assert_eq!(body["error"]["code"], code);
            }
        }
        for body in [
            "{".to_owned(),
            json!({"category":"account"}).to_string(),
            json!({"category":"photo","user_id":f.other}).to_string(),
        ] {
            let (status, _, _) = f
                .send(f.request("DELETE", "/api/web/privacy/data", Some(&f.cookie), &body))
                .await?;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM account_deletion_token WHERE user_id=$1")
                .bind(f.user)
                .fetch_one(&mut *f.db.acquire().await?)
                .await?;
        assert_eq!(count, 0);
        Ok(())
    }
    .await;
    f.cleanup().await?;
    result
}

#[tokio::test]
async fn privacy_budget_is_shared_by_mutations_and_enforced_before_field_validation() -> TestResult
{
    let f = Fixture::new(1, 100).await?;
    let result: TestResult = async {
        let (status, _, _) = f
            .send(f.request(
                "POST",
                "/api/web/privacy/deletion-token",
                Some(&f.cookie),
                "",
            ))
            .await?;
        assert_eq!(status, StatusCode::CREATED);
        let (status, headers, body) = f
            .send(f.request("DELETE", "/api/web/privacy/data", Some(&f.cookie), "{}"))
            .await?;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"]["code"], "privacy_rate_limit_exceeded");
        assert!(headers.contains_key(header::RETRY_AFTER));
        // A second account has its own budget.
        let (status, _, _) = f
            .send(f.request(
                "POST",
                "/api/web/privacy/deletion-token",
                Some(&f.other_cookie),
                "",
            ))
            .await?;
        assert_eq!(status, StatusCode::CREATED);
        Ok(())
    }
    .await;
    f.cleanup().await?;
    result
}

#[tokio::test]
async fn partial_deletions_are_scoped_idempotent_and_preserve_consent_audits() -> TestResult {
    let f = Fixture::new(100, 100).await?;
    let result: TestResult = async {
        for user in [f.user, f.other] {
            sqlx::query("INSERT INTO user_profile(user_id,firstname,birthdate,sex,bio) VALUES($1,'Fixture','1990-01-01','male','Keep this biography')").bind(user).execute(&mut *f.db.acquire().await?).await?;
            for (consent, version) in [("sensitive_data_consent", "web-sensitive-v1"), ("location_consent", "web-location-v1")] {
                sqlx::query("INSERT INTO user_consent(user_id,consent_type,granted,document_version) VALUES($1,$2,true,$3)").bind(user).bind(consent).bind(version).execute(&mut *f.db.acquire().await?).await?;
            }
            sqlx::query("INSERT INTO user_preferences(user_id,looking_for) VALUES($1,'both')").bind(user).execute(&mut *f.db.acquire().await?).await?;
            sqlx::query("INSERT INTO user_presence(user_id,latitude,longitude,is_location_fresh) VALUES($1,48.85,2.35,true)").bind(user).execute(&mut *f.db.acquire().await?).await?;
            let photo = Uuid::new_v4();
            sqlx::query("INSERT INTO user_photo(id,user_id,object_key,status,mime_type,size_bytes,width,height,sha256) VALUES($1,$2,$3,'ready','image/webp',1,1,1,$4)")
                .bind(photo).bind(user).bind(format!("profile-photos/{user}/{photo}.webp")).bind(vec![0_u8;32]).execute(&mut *f.db.acquire().await?).await?;
        }
        for category in ["photo", "location", "sensitive_profile"] {
            for _ in 0..2 {
                let (status, _, _) = f.send(f.request("DELETE", "/api/web/privacy/data", Some(&f.cookie), &json!({"category":category}).to_string())).await?;
                assert_eq!(status, StatusCode::NO_CONTENT, "{category}");
            }
            let other_unchanged: bool = sqlx::query_scalar("SELECT (sex='male' AND firstname='Fixture' AND bio='Keep this biography') AND EXISTS(SELECT 1 FROM user_preferences WHERE user_id=$1) AND EXISTS(SELECT 1 FROM user_presence WHERE user_id=$1) AND EXISTS(SELECT 1 FROM user_photo WHERE user_id=$1 AND status='ready') FROM user_profile WHERE user_id=$1")
                .bind(f.other).fetch_one(&mut *f.db.acquire().await?).await?;
            assert!(other_unchanged);
        }
        let deleted: bool = sqlx::query_scalar("SELECT sex IS NULL AND firstname='Fixture' AND bio='Keep this biography' AND NOT EXISTS(SELECT 1 FROM user_preferences WHERE user_id=$1) AND NOT EXISTS(SELECT 1 FROM user_presence WHERE user_id=$1) FROM user_profile WHERE user_id=$1")
            .bind(f.user).fetch_one(&mut *f.db.acquire().await?).await?;
        assert!(deleted);
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM outbox_event WHERE event_type='photo.delete' AND aggregate_id IN (SELECT id FROM user_photo WHERE user_id=$1 AND status='deleting')")
            .bind(f.user).fetch_one(&mut *f.db.acquire().await?).await?;
        assert_eq!(events, 1);
        // The Italian request uses the fixture's French-only legal documents.
        // Consent evidence must keep the actual document language.
        let withdrawals: i64 = sqlx::query_scalar("SELECT count(*) FROM user_consent WHERE user_id=$1 AND NOT granted AND withdrawn_at IS NOT NULL AND user_agent='web-privacy-test' AND ip_address IS NOT NULL AND document_language='fr'")
            .bind(f.user).fetch_one(&mut *f.db.acquire().await?).await?;
        assert_eq!(withdrawals, 2);
        let preserved_history: i64 = sqlx::query_scalar("SELECT count(*) FROM user_consent WHERE user_id=$1 AND granted AND withdrawn_at IS NOT NULL").bind(f.user).fetch_one(&mut *f.db.acquire().await?).await?;
        assert_eq!(preserved_history, 2);
        let (status, _, session) = f.send(f.request("GET", "/api/web/account/session", Some(&f.cookie), "")).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(session["onboarding_complete"], false);
        Ok(())
    }.await;
    f.cleanup().await?;
    result
}

#[tokio::test]
async fn account_erasure_requires_its_own_unexpired_token_and_invalidates_all_sessions()
-> TestResult {
    let f = Fixture::new(100, 100).await?;
    let result: TestResult = async {
        let (status, _, token) = f.send(f.request("POST", "/api/web/privacy/deletion-token", Some(&f.cookie), "")).await?;
        assert_eq!(status, StatusCode::CREATED);
        let body = json!({"confirmation_token":token["confirmation_token"]}).to_string();
        let (status, _, error) = f.send(f.request("DELETE", "/api/web/privacy/account", Some(&f.other_cookie), &body)).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(error["error"]["code"], "invalid_or_expired_deletion_token");
        sqlx::query("UPDATE account_deletion_token SET expires_at=clock_timestamp()-INTERVAL '1 second' WHERE user_id=$1").bind(f.user).execute(&mut *f.db.acquire().await?).await?;
        let (status, _, _) = f.send(f.request("DELETE", "/api/web/privacy/account", Some(&f.cookie), &body)).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (_, _, token) = f.send(f.request("POST", "/api/web/privacy/deletion-token", Some(&f.cookie), "")).await?;
        let body = json!({"confirmation_token":token["confirmation_token"]}).to_string();
        let second_token = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, Uuid::new_v4().as_bytes().repeat(2));
        sqlx::query("INSERT INTO web_billing_session(token_hash,user_id) VALUES($1,$2)").bind(sha256_hex(second_token.as_bytes())).bind(f.user).execute(&mut *f.db.acquire().await?).await?;
        let (status, headers, accepted) = f.send(f.request("DELETE", "/api/web/privacy/account", Some(&f.cookie), &body)).await?;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(accepted["status"], "in_progress");
        let cookie = headers[header::SET_COOKIE].to_str()?;
        assert!(cookie.contains("Max-Age=0") && cookie.contains("Secure") && cookie.contains("HttpOnly"));
        let request = Uuid::parse_str(accepted["request_id"].as_str().unwrap())?;
        let queued: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM account_erasure WHERE user_id=$1 AND request_id=$2 AND step='stripe') AND EXISTS(SELECT 1 FROM outbox_event WHERE aggregate_id=$2 AND event_type='account.erase') AND NOT EXISTS(SELECT 1 FROM account_deletion_token WHERE user_id=$1)")
            .bind(f.user).bind(request).fetch_one(&mut *f.db.acquire().await?).await?;
        assert!(queued);
        let second_cookie = format!("__Host-histae_billing={second_token}");
        for cookie in [&f.cookie, &second_cookie] {
            for path in ["/api/web/account/session", "/api/web/billing/session"] {
                let (status, _, _) = f.send(f.request("GET", path, Some(cookie), "")).await?;
                assert_eq!(status, StatusCode::UNAUTHORIZED);
            }
            let (status, _, _) = f.send(f.request("DELETE", "/api/web/privacy/account", Some(cookie), &body)).await?;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
        let (status, _, _) = f.send(f.request("GET", "/api/web/account/session", Some(&f.other_cookie), "")).await?;
        assert_eq!(status, StatusCode::OK);
        Ok(())
    }.await;
    f.cleanup().await?;
    result
}

#[tokio::test]
async fn expired_banned_and_deleted_accounts_cannot_use_website_privacy() -> TestResult {
    let f = Fixture::new(100, 100).await?;
    let result: TestResult = async {
        for query in [
            "UPDATE web_billing_session SET expires_at=clock_timestamp()-INTERVAL '1 second' WHERE user_id=$1",
            "UPDATE user_account SET is_banned=true WHERE user_id=$1",
            "UPDATE user_account SET deleted_at=clock_timestamp() WHERE user_id=$1",
        ] {
            sqlx::query(query).bind(f.user).execute(&mut *f.db.acquire().await?).await?;
            for (method,path) in [("GET","/api/web/account/session"),("POST","/api/web/privacy/deletion-token"),("DELETE","/api/web/privacy/data")] {
                let (status, _, _) = f.send(f.request(method,path,Some(&f.cookie),"{}")).await?;
                assert_eq!(status, StatusCode::UNAUTHORIZED);
            }
            sqlx::query("UPDATE web_billing_session SET expires_at=clock_timestamp()+INTERVAL '30 minutes' WHERE user_id=$1").bind(f.user).execute(&mut *f.db.acquire().await?).await?;
            sqlx::query("UPDATE user_account SET is_banned=false WHERE user_id=$1").bind(f.user).execute(&mut *f.db.acquire().await?).await?;
        }
        Ok(())
    }.await;
    f.cleanup().await?;
    result
}

#[tokio::test]
async fn account_otp_aliases_share_limits_and_session_without_enabling_payment() -> TestResult {
    let f = Fixture::new(100, 3).await?;
    let result: TestResult = async {
        let mut unknown = f.request(
            "POST",
            "/api/web/account/otp/send",
            None,
            &json!({"user_id":f.other,"phone_number":PHONE}).to_string(),
        );
        unknown
            .headers_mut()
            .insert("idempotency-key", Uuid::new_v4().to_string().parse()?);
        let (status, _, _) = f.send(unknown).await?;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(f.sms.0.lock().unwrap().is_empty());
        let key = Uuid::new_v4();
        let mut challenge = None;
        for prefix in ["account", "billing"] {
            let mut send = f.request(
                "POST",
                &format!("/api/web/{prefix}/otp/send"),
                None,
                &json!({"user_id":f.user,"phone_number":PHONE}).to_string(),
            );
            send.headers_mut()
                .insert("idempotency-key", key.to_string().parse()?);
            let (status, _, body) = f.send(send).await?;
            assert_eq!(status, StatusCode::ACCEPTED);
            if let Some(previous) = &challenge {
                assert_eq!(previous, &body["challenge_id"]);
            }
            challenge = Some(body["challenge_id"].clone());
        }
        assert_eq!(f.sms.0.lock().unwrap().len(), 1);
        let mut send = f.request(
            "POST",
            "/api/web/account/otp/send",
            None,
            &json!({"user_id":f.user,"phone_number":PHONE}).to_string(),
        );
        send.headers_mut()
            .insert("idempotency-key", Uuid::new_v4().to_string().parse()?);
        let (status, _, _) = f.send(send).await?;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        let code = f.sms.0.lock().unwrap()[0].clone();
        let verify =
            json!({"challenge_id":challenge.unwrap(),"phone_number":PHONE,"otp":code}).to_string();
        let (status, headers, _) = f
            .send(f.request("POST", "/api/web/billing/otp/verify", None, &verify))
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let set_cookie = headers[header::SET_COOKIE].to_str()?;
        assert!(
            set_cookie.contains("HttpOnly")
                && set_cookie.contains("Secure")
                && set_cookie.contains("SameSite=Strict")
                && set_cookie.contains("Max-Age=1800")
        );
        let cookie = set_cookie.split(';').next().unwrap();
        let (status, _, identity) = f
            .send(f.request("GET", "/api/web/account/session", Some(cookie), ""))
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            identity,
            json!({"user_id":f.user,"onboarding_complete":false})
        );
        for (path, body) in [
            (
                "/api/web/billing/checkout",
                "{\"billing_period\":\"monthly\"}",
            ),
            ("/api/web/billing/portal", ""),
        ] {
            let (status, _, body) = f.send(f.request("POST", path, Some(cookie), body)).await?;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(body["error"]["code"], "web_billing_unavailable");
        }
        let (status, headers, _) = f
            .send(f.request("DELETE", "/api/web/account/session", Some(cookie), ""))
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(headers[header::SET_COOKIE].to_str()?.contains("Max-Age=0"));
        let (status, _, _) = f
            .send(f.request("GET", "/api/web/billing/session", Some(cookie), ""))
            .await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _, _) = f
            .send(f.request("POST", "/api/web/account/otp/verify", None, &verify))
            .await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        Ok(())
    }
    .await;
    f.cleanup().await?;
    result
}
