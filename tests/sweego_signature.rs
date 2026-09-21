use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use histae_api_rust::config::SecretString;
use histae_api_rust::identity::mobile::sweego::{
    SweegoWebhookError, SweegoWebhookHeaders, parse_event, verify_signature,
};
use hmac::{Hmac, Mac as _};
use serde_json::json;
use sha2::Sha256;
use uuid::Uuid;

fn secret() -> SecretString {
    SecretString::new(STANDARD.encode([0x42_u8; 48]))
}

fn headers(body: &[u8], timestamp: &str) -> SweegoWebhookHeaders {
    let id = "event_fixture";
    let secret = secret();
    let key = STANDARD
        .decode(secret.expose_secret())
        .expect("fixture key");
    let mut hmac = Hmac::<Sha256>::new_from_slice(&key).expect("HMAC key");
    hmac.update(id.as_bytes());
    hmac.update(b".");
    hmac.update(timestamp.as_bytes());
    hmac.update(b".");
    hmac.update(body);
    SweegoWebhookHeaders {
        id: Some(id.to_owned()),
        timestamp: Some(timestamp.to_owned()),
        signature: Some(STANDARD.encode(hmac.finalize().into_bytes())),
    }
}

#[test]
fn signature_window_includes_the_boundaries_and_rejects_replays() {
    let now = 2_000_000_000_000_i64;
    let body = br#"{"event_type":"sms_sent"}"#;
    for signed_at in [now - 300_000, now + 60_000] {
        let timestamp = (signed_at / 1_000).to_string();
        assert_eq!(
            verify_signature(body, &headers(body, &timestamp), &secret(), now),
            Ok(())
        );
    }
    for signed_at in [now - 301_000, now + 61_000] {
        let timestamp = (signed_at / 1_000).to_string();
        assert_eq!(
            verify_signature(body, &headers(body, &timestamp), &secret(), now),
            Err(SweegoWebhookError::InvalidSignature)
        );
    }
}

#[test]
fn unsupported_signed_events_are_ignored_before_provider_dto_validation() {
    assert_eq!(
        parse_event(
            json!({ "event_type": "sms_clicked", "unsupported_nested": { "private": true } }),
            "Histae"
        ),
        Ok(None)
    );
    let invalid_supported = parse_event(
        json!({
            "event_type": "sms_sent",
            "channel": "sms",
            "test_mode": false,
            "sender_id": "Histae",
            "timestamp": "2026-09-21T12:00:00Z",
            "event_id": Uuid::new_v4(),
            "campaign_id": Uuid::new_v4(),
            "swg_uid": "message-1",
            "unsupported_nested": { "private": true }
        }),
        "Histae",
    );
    assert_eq!(invalid_supported, Err(SweegoWebhookError::InvalidEvent));
}
