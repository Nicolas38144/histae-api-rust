use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures_util::StreamExt as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use reqwest::{Client, StatusCode};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use url::Url;

use super::delivery::NotificationDeliveryStore;
use super::domain::NotificationType;
use crate::config::{PushConfig, PushProvider};
use crate::shared::clock::{Clock, SystemClock};

const FCM_RESPONSE_LIMIT: usize = 64 * 1024;
const OAUTH_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PushDeliveryError;

impl fmt::Display for PushDeliveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("push_delivery_unavailable")
    }
}

impl std::error::Error for PushDeliveryError {}

pub type PushFuture<'a> = Pin<Box<dyn Future<Output = Result<(), PushDeliveryError>> + Send + 'a>>;

pub trait PushSender: Send + Sync {
    fn send<'a>(
        &'a self,
        token: &'a str,
        kind: NotificationType,
        data: BTreeMap<String, String>,
    ) -> PushFuture<'a>;
}

#[derive(Clone, Debug)]
pub struct ProviderResponse {
    pub status: u16,
    pub body: Option<Value>,
}

pub type TransportFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ProviderResponse, PushDeliveryError>> + Send + 'a>>;

pub trait PushTransport: Send + Sync {
    fn post_form<'a>(
        &'a self,
        url: &'a Url,
        fields: &'a BTreeMap<String, String>,
        timeout: std::time::Duration,
    ) -> TransportFuture<'a>;

    fn post_json<'a>(
        &'a self,
        url: &'a Url,
        bearer: &'a str,
        body: &'a Value,
        timeout: std::time::Duration,
    ) -> TransportFuture<'a>;
}

#[derive(Clone, Default)]
pub struct ReqwestPushTransport {
    client: Client,
}

impl ReqwestPushTransport {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }
}

impl PushTransport for ReqwestPushTransport {
    fn post_form<'a>(
        &'a self,
        url: &'a Url,
        fields: &'a BTreeMap<String, String>,
        timeout: std::time::Duration,
    ) -> TransportFuture<'a> {
        Box::pin(async move {
            let response = self
                .client
                .post(url.clone())
                .timeout(timeout)
                .form(fields)
                .send()
                .await
                .map_err(|_| PushDeliveryError)?;
            bounded_response(response).await
        })
    }

    fn post_json<'a>(
        &'a self,
        url: &'a Url,
        bearer: &'a str,
        body: &'a Value,
        timeout: std::time::Duration,
    ) -> TransportFuture<'a> {
        Box::pin(async move {
            let response = self
                .client
                .post(url.clone())
                .timeout(timeout)
                .bearer_auth(bearer)
                .json(body)
                .send()
                .await
                .map_err(|_| PushDeliveryError)?;
            bounded_response(response).await
        })
    }
}

async fn bounded_response(
    response: reqwest::Response,
) -> Result<ProviderResponse, PushDeliveryError> {
    let status = response.status().as_u16();
    if response
        .content_length()
        .is_some_and(|length| length > FCM_RESPONSE_LIMIT as u64)
    {
        return Err(PushDeliveryError);
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| PushDeliveryError)?;
        if bytes.len().saturating_add(chunk.len()) > FCM_RESPONSE_LIMIT {
            return Err(PushDeliveryError);
        }
        bytes.extend_from_slice(&chunk);
    }
    let body = (!bytes.is_empty())
        .then(|| serde_json::from_slice(&bytes).ok())
        .flatten();
    Ok(ProviderResponse { status, body })
}

#[derive(Clone)]
pub struct PushService {
    inner: Arc<PushInner>,
}

struct PushInner {
    config: PushConfig,
    store: Arc<dyn NotificationDeliveryStore>,
    transport: Arc<dyn PushTransport>,
    signer: Arc<dyn OAuthAssertionSigner>,
    clock: Arc<dyn Clock>,
    access_token: Mutex<Option<AccessToken>>,
}

struct AccessToken {
    value: String,
    expires_at: DateTime<Utc>,
}

impl PushService {
    pub fn new(config: PushConfig, store: Arc<dyn NotificationDeliveryStore>) -> Self {
        Self::with_dependencies(
            config,
            store,
            Arc::new(ReqwestPushTransport::new()),
            Arc::new(RsaOAuthAssertionSigner),
            Arc::new(SystemClock),
        )
    }

    pub fn with_dependencies(
        config: PushConfig,
        store: Arc<dyn NotificationDeliveryStore>,
        transport: Arc<dyn PushTransport>,
        signer: Arc<dyn OAuthAssertionSigner>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            inner: Arc::new(PushInner {
                config,
                store,
                transport,
                signer,
                clock,
                access_token: Mutex::new(None),
            }),
        }
    }

    async fn send_inner(
        &self,
        token: &str,
        kind: NotificationType,
        data: BTreeMap<String, String>,
    ) -> Result<(), PushDeliveryError> {
        if self.inner.config.provider == PushProvider::Disabled {
            return Ok(());
        }
        let access_token = self.google_access_token().await?;
        let (title, message) = kind.copy();
        let mut public_data = data;
        public_data.insert("type".to_owned(), kind.as_str().to_owned());
        let request = json!({
            "message": {
                "token": token,
                "notification": { "title": title, "body": message },
                "data": public_data,
            }
        });
        let url = Url::parse(&format!(
            "https://fcm.googleapis.com/v1/projects/{}/messages:send",
            encode_path_segment(&self.inner.config.project_id)
        ))
        .map_err(|_| PushDeliveryError)?;
        let response = self
            .inner
            .transport
            .post_json(&url, &access_token, &request, self.inner.config.timeout)
            .await?;
        if (200..300).contains(&response.status) {
            return Ok(());
        }
        if explicitly_unregistered(response.body.as_ref()) {
            self.inner
                .store
                .remove_token(token)
                .await
                .map_err(|_| PushDeliveryError)?;
            return Ok(());
        }
        if response.status == StatusCode::UNAUTHORIZED.as_u16() {
            *self.inner.access_token.lock().await = None;
        }
        Err(PushDeliveryError)
    }

    async fn google_access_token(&self) -> Result<String, PushDeliveryError> {
        let mut cache = self.inner.access_token.lock().await;
        let now = self.inner.clock.now();
        if let Some(token) = cache.as_ref()
            && token.expires_at > now + ChronoDuration::seconds(60)
        {
            return Ok(token.value.clone());
        }
        let assertion = self.inner.signer.sign(&self.inner.config, now)?;
        let fields = BTreeMap::from([
            (
                "grant_type".to_owned(),
                "urn:ietf:params:oauth:grant-type:jwt-bearer".to_owned(),
            ),
            ("assertion".to_owned(), assertion),
        ]);
        let response = self
            .inner
            .transport
            .post_form(
                &self.inner.config.token_uri,
                &fields,
                self.inner.config.timeout,
            )
            .await?;
        if !(200..300).contains(&response.status) {
            return Err(PushDeliveryError);
        }
        let body = response.body.ok_or(PushDeliveryError)?;
        let value = body
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or(PushDeliveryError)?
            .to_owned();
        let expires_in = body
            .get("expires_in")
            .and_then(Value::as_i64)
            .unwrap_or(3_600);
        let ttl = ChronoDuration::try_seconds(expires_in).ok_or(PushDeliveryError)?;
        let token_expiry = now.checked_add_signed(ttl).ok_or(PushDeliveryError)?;
        *cache = Some(AccessToken {
            value: value.clone(),
            expires_at: token_expiry,
        });
        Ok(value)
    }
}

impl PushSender for PushService {
    fn send<'a>(
        &'a self,
        token: &'a str,
        kind: NotificationType,
        data: BTreeMap<String, String>,
    ) -> PushFuture<'a> {
        Box::pin(self.send_inner(token, kind, data))
    }
}

#[derive(Serialize)]
struct OAuthClaims<'a> {
    iss: &'a str,
    scope: &'static str,
    aud: &'a str,
    iat: i64,
    exp: i64,
}

pub trait OAuthAssertionSigner: Send + Sync {
    fn sign(&self, config: &PushConfig, now: DateTime<Utc>) -> Result<String, PushDeliveryError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RsaOAuthAssertionSigner;

impl OAuthAssertionSigner for RsaOAuthAssertionSigner {
    fn sign(&self, config: &PushConfig, now: DateTime<Utc>) -> Result<String, PushDeliveryError> {
        let issued_at = now.timestamp();
        let expires_at = issued_at.checked_add(3_600).ok_or(PushDeliveryError)?;
        let claims = OAuthClaims {
            iss: &config.client_email,
            scope: OAUTH_SCOPE,
            aud: config.token_uri.as_str(),
            iat: issued_at,
            exp: expires_at,
        };
        let key = EncodingKey::from_rsa_pem(config.private_key.expose_secret().as_bytes())
            .map_err(|_| PushDeliveryError)?;
        encode(&Header::new(Algorithm::RS256), &claims, &key).map_err(|_| PushDeliveryError)
    }
}

fn explicitly_unregistered(body: Option<&Value>) -> bool {
    body.and_then(|body| body.pointer("/error/details"))
        .and_then(Value::as_array)
        .is_some_and(|details| {
            details.iter().any(|detail| {
                detail.get("errorCode").and_then(Value::as_str) == Some("UNREGISTERED")
            })
        })
}

fn encode_path_segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SecretString;
    use crate::infra::postgres::DatabaseError;
    use chrono::TimeZone as _;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct Store {
        removed: StdMutex<Vec<String>>,
    }

    impl NotificationDeliveryStore for Store {
        fn find_deliverable(
            &self,
            _id: uuid::Uuid,
        ) -> super::super::delivery::DeliveryStoreFuture<
            '_,
            Option<super::super::delivery::PendingPush>,
        > {
            Box::pin(async { Ok(None) })
        }

        fn remove_token<'a>(
            &'a self,
            token: &'a str,
        ) -> super::super::delivery::DeliveryStoreFuture<'a, ()> {
            Box::pin(async move {
                self.removed
                    .lock()
                    .map_err(|_| DatabaseError::QueryFailed)?
                    .push(token.to_owned());
                Ok(())
            })
        }
    }

    struct FixedClock(DateTime<Utc>);
    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
    }

    struct Signer;
    impl OAuthAssertionSigner for Signer {
        fn sign(
            &self,
            _config: &PushConfig,
            _now: DateTime<Utc>,
        ) -> Result<String, PushDeliveryError> {
            Ok("generated-assertion".to_owned())
        }
    }

    #[derive(Default)]
    struct Transport {
        forms: StdMutex<Vec<BTreeMap<String, String>>>,
        json: StdMutex<Vec<Value>>,
        token_responses: StdMutex<Vec<ProviderResponse>>,
        send_responses: StdMutex<Vec<ProviderResponse>>,
    }

    impl PushTransport for Transport {
        fn post_form<'a>(
            &'a self,
            _url: &'a Url,
            fields: &'a BTreeMap<String, String>,
            _timeout: std::time::Duration,
        ) -> TransportFuture<'a> {
            let fields = fields.clone();
            Box::pin(async move {
                self.forms
                    .lock()
                    .map_err(|_| PushDeliveryError)?
                    .push(fields);
                Ok(self
                    .token_responses
                    .lock()
                    .map_err(|_| PushDeliveryError)?
                    .remove(0))
            })
        }

        fn post_json<'a>(
            &'a self,
            _url: &'a Url,
            _bearer: &'a str,
            body: &'a Value,
            _timeout: std::time::Duration,
        ) -> TransportFuture<'a> {
            let body = body.clone();
            Box::pin(async move {
                self.json.lock().map_err(|_| PushDeliveryError)?.push(body);
                Ok(self
                    .send_responses
                    .lock()
                    .map_err(|_| PushDeliveryError)?
                    .remove(0))
            })
        }
    }

    fn config(provider: PushProvider) -> PushConfig {
        PushConfig {
            provider,
            project_id: "generated-project".to_owned(),
            client_email: "generated@example.invalid".to_owned(),
            private_key: SecretString::new("generated-private-key".to_owned()),
            token_uri: Url::parse("https://oauth.example.invalid/token")
                .unwrap_or_else(|_| unreachable!()),
            timeout: std::time::Duration::from_secs(1),
        }
    }

    fn response(status: u16, body: Value) -> ProviderResponse {
        ProviderResponse {
            status,
            body: Some(body),
        }
    }

    #[cfg(feature = "webauthn-probe")]
    #[test]
    fn rsa_oauth_assertions_are_valid_with_the_selected_crypto_backend() {
        use jsonwebtoken::{DecodingKey, Validation, decode};

        let rsa = openssl::rsa::Rsa::generate(2048).expect("test RSA key");
        let key = openssl::pkey::PKey::from_rsa(rsa).expect("test key");
        let mut config = config(PushProvider::Fcm);
        config.private_key = SecretString::new(
            String::from_utf8(key.private_key_to_pem_pkcs8().expect("PEM")).expect("UTF-8"),
        );
        let now = Utc::now();
        let assertion = RsaOAuthAssertionSigner
            .sign(&config, now)
            .expect("OAuth signature");
        let public = DecodingKey::from_rsa_pem(&key.public_key_to_pem().expect("public PEM"))
            .expect("public key");
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[config.token_uri.as_str()]);
        validation.set_issuer(&[config.client_email.as_str()]);
        let verified = decode::<Value>(&assertion, &public, &validation).expect("valid assertion");
        assert_eq!(verified.claims["scope"], OAUTH_SCOPE);
        assert_eq!(verified.claims["iat"], now.timestamp());
        assert_eq!(verified.claims["exp"], now.timestamp() + 3_600);
    }

    #[tokio::test]
    async fn disabled_push_performs_no_network_or_storage_operation() {
        let store = Arc::new(Store::default());
        let transport = Arc::new(Transport::default());
        let service = PushService::with_dependencies(
            config(PushProvider::Disabled),
            store.clone(),
            transport.clone(),
            Arc::new(Signer),
            Arc::new(FixedClock(Utc::now())),
        );
        service
            .send(
                "generated-token",
                NotificationType::NewMessage,
                BTreeMap::new(),
            )
            .await
            .unwrap_or_else(|_| unreachable!());
        assert!(
            transport
                .forms
                .lock()
                .unwrap_or_else(|_| unreachable!())
                .is_empty()
        );
        assert!(
            store
                .removed
                .lock()
                .unwrap_or_else(|_| unreachable!())
                .is_empty()
        );
    }

    #[tokio::test]
    async fn invalid_oauth_durations_return_errors_without_panicking_or_sending() {
        for expiry in [i64::MAX, i64::MIN, i64::MAX / 1000] {
            let transport = Arc::new(Transport::default());
            transport
                .token_responses
                .lock()
                .expect("responses")
                .push(response(
                    200,
                    json!({"access_token":"fixture-access", "expires_in":expiry}),
                ));
            let service = PushService::with_dependencies(
                config(PushProvider::Fcm),
                Arc::new(Store::default()),
                transport.clone(),
                Arc::new(Signer),
                Arc::new(FixedClock(Utc::now())),
            );
            assert_eq!(
                service
                    .send(
                        "fixture-device",
                        NotificationType::NewMessage,
                        BTreeMap::new()
                    )
                    .await,
                Err(PushDeliveryError)
            );
            assert!(transport.json.lock().expect("requests").is_empty());
        }
    }

    #[tokio::test]
    async fn caches_oauth_and_sends_only_the_given_metadata() {
        let store = Arc::new(Store::default());
        let transport = Arc::new(Transport::default());
        transport
            .token_responses
            .lock()
            .unwrap_or_else(|_| unreachable!())
            .push(response(
                200,
                json!({"access_token":"generated-access","expires_in":3600}),
            ));
        transport
            .send_responses
            .lock()
            .unwrap_or_else(|_| unreachable!())
            .extend([response(200, json!({})), response(200, json!({}))]);
        let now = Utc
            .with_ymd_and_hms(2030, 1, 1, 0, 0, 0)
            .single()
            .unwrap_or_else(|| unreachable!());
        let service = PushService::with_dependencies(
            config(PushProvider::Fcm),
            store,
            transport.clone(),
            Arc::new(Signer),
            Arc::new(FixedClock(now)),
        );
        let data = BTreeMap::from([(
            "notification_id".to_owned(),
            uuid::Uuid::new_v4().to_string(),
        )]);
        service
            .send(
                "generated-token",
                NotificationType::NewMessage,
                data.clone(),
            )
            .await
            .unwrap_or_else(|_| unreachable!());
        service
            .send("generated-token", NotificationType::NewMessage, data)
            .await
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            transport
                .forms
                .lock()
                .unwrap_or_else(|_| unreachable!())
                .len(),
            1
        );
        let sent = transport.json.lock().unwrap_or_else(|_| unreachable!());
        assert_eq!(sent.len(), 2);
        assert_eq!(
            sent[0]
                .pointer("/message/data/type")
                .and_then(Value::as_str),
            Some("new_message")
        );
        assert!(
            !serde_json::to_string(&sent[0])
                .unwrap_or_default()
                .contains("private")
        );
    }

    #[tokio::test]
    async fn deletes_only_explicitly_unregistered_tokens() {
        let store = Arc::new(Store::default());
        let transport = Arc::new(Transport::default());
        transport
            .token_responses
            .lock()
            .unwrap_or_else(|_| unreachable!())
            .push(response(200, json!({"access_token":"generated-access"})));
        transport
            .send_responses
            .lock()
            .unwrap_or_else(|_| unreachable!())
            .push(response(
                404,
                json!({"error":{"details":[{"errorCode":"UNREGISTERED"}]}}),
            ));
        let service = PushService::with_dependencies(
            config(PushProvider::Fcm),
            store.clone(),
            transport,
            Arc::new(Signer),
            Arc::new(FixedClock(Utc::now())),
        );
        service
            .send(
                "generated-token",
                NotificationType::NewMatch,
                BTreeMap::new(),
            )
            .await
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            store
                .removed
                .lock()
                .unwrap_or_else(|_| unreachable!())
                .as_slice(),
            ["generated-token"]
        );
    }

    #[tokio::test]
    async fn an_ordinary_not_found_is_retryable_and_keeps_the_device() {
        let store = Arc::new(Store::default());
        let transport = Arc::new(Transport::default());
        transport
            .token_responses
            .lock()
            .unwrap_or_else(|_| unreachable!())
            .push(response(200, json!({"access_token":"generated-access"})));
        transport
            .send_responses
            .lock()
            .unwrap_or_else(|_| unreachable!())
            .push(response(
                404,
                json!({"error":{"status":"NOT_FOUND","message":"private"}}),
            ));
        let service = PushService::with_dependencies(
            config(PushProvider::Fcm),
            store.clone(),
            transport,
            Arc::new(Signer),
            Arc::new(FixedClock(Utc::now())),
        );
        assert_eq!(
            service
                .send(
                    "generated-token",
                    NotificationType::NewMatch,
                    BTreeMap::new()
                )
                .await,
            Err(PushDeliveryError)
        );
        assert!(
            store
                .removed
                .lock()
                .unwrap_or_else(|_| unreachable!())
                .is_empty()
        );
    }

    #[tokio::test]
    async fn unauthorized_delivery_invalidates_the_cached_oauth_token() {
        let store = Arc::new(Store::default());
        let transport = Arc::new(Transport::default());
        transport
            .token_responses
            .lock()
            .unwrap_or_else(|_| unreachable!())
            .extend([
                response(200, json!({"access_token":"generated-access-1"})),
                response(200, json!({"access_token":"generated-access-2"})),
            ]);
        transport
            .send_responses
            .lock()
            .unwrap_or_else(|_| unreachable!())
            .extend([response(401, json!({})), response(200, json!({}))]);
        let service = PushService::with_dependencies(
            config(PushProvider::Fcm),
            store,
            transport.clone(),
            Arc::new(Signer),
            Arc::new(FixedClock(Utc::now())),
        );
        assert_eq!(
            service
                .send(
                    "generated-token",
                    NotificationType::NewMatch,
                    BTreeMap::new()
                )
                .await,
            Err(PushDeliveryError)
        );
        service
            .send(
                "generated-token",
                NotificationType::NewMatch,
                BTreeMap::new(),
            )
            .await
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            transport
                .forms
                .lock()
                .unwrap_or_else(|_| unreachable!())
                .len(),
            2
        );
    }
}
