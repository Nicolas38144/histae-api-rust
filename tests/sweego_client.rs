use std::time::Duration;

use histae_api_rust::config::{SecretString, SmsConfig, SmsProvider};
use histae_api_rust::identity::mobile::sweego::{
    MAX_SMS_PROVIDER_BODY_BYTES, SmsDelivery, SmsFailureOutcome, SmsFailureReason, SmsMessage,
    SweegoSmsService,
};
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use url::Url;
use uuid::Uuid;

struct AllowSmsGuard;
impl histae_api_rust::identity::mobile::sms_safety::SmsSendGuard for AllowSmsGuard {
    fn reserve<'a>(
        &'a self,
        _: Uuid,
        _: &'a str,
    ) -> histae_api_rust::identity::mobile::sms_safety::SmsPermitFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}
fn guarded_service(
    config: SmsConfig,
) -> Result<SweegoSmsService, histae_api_rust::identity::mobile::sweego::SmsDeliveryError> {
    SweegoSmsService::new(config, std::sync::Arc::new(AllowSmsGuard))
}

fn config(endpoint: Url, provider: SmsProvider) -> SmsConfig {
    SmsConfig {
        provider,
        endpoint,
        api_key: SecretString::new("fixture-api-key".to_owned()),
        sender_id: "Histae".to_owned(),
        region: "FR".to_owned(),
        timeout: Duration::from_secs(2),
        otp_ttl: Duration::from_secs(601),
        resend_cooldown: Duration::from_secs(60),
        safety: Default::default(),
        webhook_secret: SecretString::new(String::new()),
    }
}

fn message() -> SmsMessage {
    SmsMessage {
        language: histae_api_rust::localization::Language::Fr,
        phone: "+33600000000".to_owned(),
        region: "FR".to_owned(),
        code: "123456".to_owned(),
        delivery_id: Uuid::new_v4(),
    }
}

#[tokio::test]
async fn policy_rejections_never_connect_to_sweego_or_count_as_provider_failures() {
    use histae_api_rust::identity::mobile::sms_safety::{
        SmsPermitFuture, SmsSendGuard, SmsSendRejection,
    };
    struct Reject(SmsSendRejection);
    impl SmsSendGuard for Reject {
        fn reserve<'a>(&'a self, _: Uuid, _: &'a str) -> SmsPermitFuture<'a> {
            Box::pin(async { Err(self.0) })
        }
    }
    for (rejection, reason) in [
        (
            SmsSendRejection::HourlyLimit,
            SmsFailureReason::PhoneHourlyLimit,
        ),
        (
            SmsSendRejection::DailyLimit,
            SmsFailureReason::PhoneDailyLimit,
        ),
        (
            SmsSendRejection::Suspended,
            SmsFailureReason::SendingSuspended,
        ),
        (
            SmsSendRejection::GlobalHourlyLimit,
            SmsFailureReason::GlobalHourlyLimit,
        ),
        (
            SmsSendRejection::GlobalDailyLimit,
            SmsFailureReason::GlobalDailyLimit,
        ),
        (
            SmsSendRejection::Unavailable,
            SmsFailureReason::SafetyUnavailable,
        ),
        (
            SmsSendRejection::AlreadyReserved,
            SmsFailureReason::AttemptAlreadyReserved,
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake endpoint");
        let endpoint = Url::parse(&format!(
            "http://{}/send",
            listener.local_addr().expect("address")
        ))
        .expect("URL");
        let metrics =
            std::sync::Arc::new(histae_api_rust::operations::metrics::OperationalMetrics::new());
        let service = SweegoSmsService::new(
            config(endpoint, SmsProvider::Sweego),
            std::sync::Arc::new(Reject(rejection)),
        )
        .expect("HTTP client")
        .with_metrics(metrics.clone());
        let error = service
            .send_otp(message())
            .await
            .expect_err("policy rejection");
        assert_eq!(error.reason, reason);
        assert_eq!(error.outcome, SmsFailureOutcome::Failed);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
        assert!(
            metrics
                .snapshot()
                .dependencies
                .iter()
                .all(|(_, values)| values.calls == 0)
        );
    }
}

async fn server(status: u16, response_body: Vec<u8>) -> (Url, tokio::task::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Sweego");
    let address = listener.local_addr().expect("listener address");
    let endpoint = Url::parse(&format!("http://{address}/send")).expect("endpoint URL");
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept request");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2_048];
        loop {
            let read = socket.read(&mut buffer).await.expect("read request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if complete_http_request(&request) {
                break;
            }
        }
        let reason = if status == 200 { "OK" } else { "Error" };
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response_body.len()
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("write headers");
        socket.write_all(&response_body).await.expect("write body");
        request
    });
    (endpoint, task)
}

fn complete_http_request(request: &[u8]) -> bool {
    let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&request[..header_end]);
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    request.len() >= header_end + 4 + length
}

fn request_body(request: &[u8]) -> &[u8] {
    request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map_or(&[], |position| &request[position + 4..])
}

#[tokio::test]
async fn sends_the_exact_transactional_payload_once_and_parses_one_receipt() {
    let response = br#"{"transaction_id":"transaction-1","swg_uids":{"+33600000000":"message-1"}}"#;
    let (endpoint, captured) = server(200, response.to_vec()).await;
    let service = guarded_service(config(endpoint, SmsProvider::Sweego)).expect("HTTP client");
    let sent = message();
    let delivery_id = sent.delivery_id;
    let receipt = service.send_otp(sent).await.expect("receipt");
    assert_eq!(receipt.transaction_id, "transaction-1");
    assert_eq!(receipt.message_id, "message-1");

    let request = captured.await.expect("fake server");
    let request_text = String::from_utf8_lossy(&request);
    assert_eq!(request_text.matches("POST /send HTTP/1.1").count(), 1);
    assert!(
        request_text
            .to_ascii_lowercase()
            .contains("api-key: fixture-api-key")
    );
    let payload: Value = serde_json::from_slice(request_body(&request)).expect("request JSON");
    assert_eq!(payload["channel"], "sms");
    assert_eq!(payload["provider"], "sweego");
    assert_eq!(payload["campaign-type"], "transac");
    assert_eq!(payload["campaign-id"], delivery_id.to_string());
    assert_eq!(
        payload["recipients"],
        serde_json::json!([{ "num": "+33600000000", "region": "FR" }])
    );
    assert!(
        payload["message-txt"]
            .as_str()
            .is_some_and(|text| { text.contains("123456") && text.contains("11 minutes") })
    );
    assert_eq!(payload["shorten-urls"], false);
    assert_eq!(payload["shorten-with-protocol"], false);
    assert!(!String::from_utf8_lossy(request_body(&request)).contains("fixture-api-key"));
}

#[tokio::test]
async fn classifies_definitive_and_uncertain_http_failures_without_retrying() {
    for (status, expected_outcome, expected_reason) in [
        (
            422,
            SmsFailureOutcome::Failed,
            SmsFailureReason::ProviderRejected,
        ),
        (
            500,
            SmsFailureOutcome::Unknown,
            SmsFailureReason::ProviderUnavailable,
        ),
        (
            302,
            SmsFailureOutcome::Unknown,
            SmsFailureReason::ProviderUnavailable,
        ),
    ] {
        let (endpoint, captured) = server(status, b"private-provider-body".to_vec()).await;
        let service = guarded_service(config(endpoint, SmsProvider::Sweego)).expect("HTTP client");
        let error = service
            .send_otp(message())
            .await
            .expect_err("provider failure");
        assert_eq!(error.outcome, expected_outcome);
        assert_eq!(error.reason, expected_reason);
        let request = captured.await.expect("fake server");
        assert_eq!(
            String::from_utf8_lossy(&request)
                .matches("POST /send HTTP/1.1")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn bounds_provider_responses_and_fails_closed_when_disabled() {
    let (endpoint, captured) = server(200, vec![b'x'; MAX_SMS_PROVIDER_BODY_BYTES + 1]).await;
    let service = guarded_service(config(endpoint, SmsProvider::Sweego)).expect("HTTP client");
    let error = service
        .send_otp(message())
        .await
        .expect_err("oversized response");
    assert_eq!(error.outcome, SmsFailureOutcome::Unknown);
    assert_eq!(error.reason, SmsFailureReason::ProviderInvalidResponse);
    captured.await.expect("fake server");

    let disabled = guarded_service(config(
        "http://127.0.0.1:9/send".parse().expect("URL"),
        SmsProvider::Disabled,
    ))
    .expect("HTTP client");
    let error = disabled
        .send_otp(message())
        .await
        .expect_err("disabled delivery");
    assert_eq!(error.outcome, SmsFailureOutcome::Failed);
    assert_eq!(error.reason, SmsFailureReason::NotConfigured);
}

#[tokio::test]
async fn provider_failures_update_only_aggregated_metrics() {
    let (endpoint, captured) = server(503, b"{}".to_vec()).await;
    let metrics =
        std::sync::Arc::new(histae_api_rust::operations::metrics::OperationalMetrics::new());
    let service = guarded_service(config(endpoint, SmsProvider::Sweego))
        .expect("client")
        .with_metrics(metrics.clone());
    assert!(service.send_otp(message()).await.is_err());
    tokio::time::timeout(Duration::from_secs(5), captured)
        .await
        .expect("local provider received the request")
        .expect("server");
    let snapshot = metrics.snapshot();
    let (_, counters) = snapshot
        .dependencies
        .iter()
        .find(|(name, _)| *name == "sweego")
        .expect("metrics");
    assert_eq!((counters.calls, counters.errors), (1, 1));
    assert_eq!(
        counters.last_error_code.as_deref(),
        Some("sms_delivery_failed")
    );
}

#[tokio::test]
async fn sends_localized_codes_without_changing_sms_delivery_metadata() {
    use histae_api_rust::localization::Language;
    for (language, expected) in [
        (Language::En, "your verification code"),
        (Language::Es, "tu codigo de verificacion"),
        (Language::It, "il tuo codice di verifica"),
    ] {
        let (endpoint, captured) = server(
            200,
            br#"{"transaction_id":"transaction-1","swg_uids":{"+33600000000":"message-1"}}"#
                .to_vec(),
        )
        .await;
        let service = guarded_service(config(endpoint, SmsProvider::Sweego)).expect("client");
        let mut input = message();
        input.language = language;
        service.send_otp(input).await.expect("receipt");
        let request = captured.await.expect("server");
        let body: Value = serde_json::from_slice(request_body(&request)).expect("JSON");
        let text = body["message-txt"].as_str().expect("text");
        assert!(text.contains(expected));
        assert!(text.contains("123456"));
        assert_eq!(body["recipients"][0]["region"], "FR");
        assert_eq!(body["campaign-type"], "transac");
    }
}
