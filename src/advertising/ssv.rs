//! AdMob signatures cover the original query bytes, excluding signature and key_id.
use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use openssl::{
    ecdsa::EcdsaSig,
    hash::MessageDigest,
    pkey::{PKey, Public},
    sign::Verifier,
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

const KEYS_URL: &str = "https://www.gstatic.com/admob/reward/verifier-keys.json";
const MAX_QUERY_BYTES: usize = 4_096;
const MAX_KEY_DOCUMENT_BYTES: usize = 65_536;
const KEY_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const KEY_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const KEY_REFRESH_COOLDOWN: Duration = Duration::from_secs(30);
const MAX_CONCURRENT_VERIFICATIONS: usize = 32;
const MAX_KEY_REFRESH_WAITERS: usize = 32;
pub const URL_VALIDATION_CUSTOM_DATA: &str = "histae-ssv-validation";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationError {
    Invalid,
    Unavailable,
}
#[derive(Debug)]
pub struct Callback {
    pub intent_id: Uuid,
    pub ad_unit: String,
    pub transaction_id: String,
    pub timestamp_ms: i64,
    pub reward_amount: i32,
    pub reward_item: String,
}
#[derive(Debug)]
pub enum VerifiedCallback {
    Reward(Callback),
    UrlValidation,
}
struct SignedQuery<'a> {
    content: &'a str,
    signature: Vec<u8>,
    key_id: u64,
    callback: VerifiedCallback,
}
fn invalid_callback(code: &'static str) -> VerificationError {
    super::log_callback_rejection(code);
    VerificationError::Invalid
}
fn parse(raw: &str) -> Result<SignedQuery<'_>, VerificationError> {
    if raw.len() > MAX_QUERY_BYTES {
        return Err(invalid_callback("admob_query_too_large"));
    }
    let (content, tail) = raw
        .rsplit_once("&signature=")
        .ok_or_else(|| invalid_callback("admob_signature_missing"))?;
    let (signature, key_id) = tail
        .split_once("&key_id=")
        .ok_or_else(|| invalid_callback("admob_key_id_missing"))?;
    if signature.contains('&') || key_id.contains('&') {
        return Err(invalid_callback("admob_signature_parameter_order_invalid"));
    }
    let signature_pair = format!("signature={signature}");
    let decoded_signature = url::form_urlencoded::parse(signature_pair.as_bytes())
        .next()
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| invalid_callback("admob_signature_encoding_invalid"))?;
    let signature = decoded_signature.as_str();
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .or_else(|_| URL_SAFE.decode(signature))
        .map_err(|_| invalid_callback("admob_signature_encoding_invalid"))?;
    // P-256 signatures are DER encoded. Reject malformed signatures before any
    // key refresh so empty or arbitrary bytes cannot initiate a Google request.
    if !(8..=72).contains(&signature.len()) || EcdsaSig::from_der(&signature).is_err() {
        return Err(invalid_callback("admob_signature_der_invalid"));
    }
    let key_id = key_id
        .parse()
        .map_err(|_| invalid_callback("admob_key_id_invalid"))?;
    let mut fields = BTreeMap::new();
    for (key, value) in url::form_urlencoded::parse(content.as_bytes()) {
        if ![
            "ad_network",
            "ad_unit",
            "custom_data",
            "reward_amount",
            "reward_item",
            "timestamp",
            "transaction_id",
            "user_id",
        ]
        .contains(&key.as_ref())
            || fields
                .insert(key.into_owned(), value.into_owned())
                .is_some()
        {
            return Err(invalid_callback("admob_callback_fields_invalid"));
        }
    }
    let required = |name: &str| {
        fields
            .get(name)
            .cloned()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| invalid_callback("admob_required_field_missing"))
    };
    let transaction_id = required("transaction_id")?;
    if transaction_id.len() > 128 || !transaction_id.bytes().all(|v| v.is_ascii_hexdigit()) {
        return Err(invalid_callback("admob_transaction_id_invalid"));
    }
    // user_id must never contain a Histae account identifier. Only custom_data is used.
    if fields.get("user_id").is_some_and(|value| !value.is_empty()) {
        return Err(invalid_callback("admob_user_id_not_allowed"));
    }
    let _network: u64 = required("ad_network")?
        .parse()
        .map_err(|_| invalid_callback("admob_ad_network_invalid"))?;
    let custom_data = required("custom_data")?;
    let ad_unit = required("ad_unit")?;
    let timestamp_ms = required("timestamp")?
        .parse()
        .map_err(|_| invalid_callback("admob_timestamp_invalid"))?;
    let reward_amount = required("reward_amount")?
        .parse()
        .map_err(|_| invalid_callback("admob_reward_amount_invalid"))?;
    let reward_item = required("reward_item")?;
    let callback = if custom_data == URL_VALIDATION_CUSTOM_DATA {
        // Console URL validation is authenticated below, but never grants a reward.
        VerifiedCallback::UrlValidation
    } else {
        VerifiedCallback::Reward(Callback {
            intent_id: Uuid::parse_str(&custom_data)
                .map_err(|_| invalid_callback("admob_custom_data_invalid"))?,
            ad_unit,
            transaction_id,
            timestamp_ms,
            reward_amount,
            reward_item,
        })
    };
    Ok(SignedQuery {
        content,
        signature,
        key_id,
        callback,
    })
}
fn verify(key: &PKey<Public>, signed: &SignedQuery<'_>) -> Result<(), VerificationError> {
    let mut verifier = Verifier::new(MessageDigest::sha256(), key)
        .map_err(|_| invalid_callback("admob_signature_verification_failed"))?;
    verifier
        .update(signed.content.as_bytes())
        .map_err(|_| invalid_callback("admob_signature_verification_failed"))?;
    if verifier
        .verify(&signed.signature)
        .map_err(|_| invalid_callback("admob_signature_verification_failed"))?
    {
        Ok(())
    } else {
        Err(invalid_callback("admob_signature_mismatch"))
    }
}
#[derive(Default)]
struct KeyCache {
    keys: BTreeMap<u64, PKey<Public>>,
    fetched_at: Option<Instant>,
    failed_at: Option<Instant>,
    unknown_refresh_at: Option<Instant>,
}

impl KeyCache {
    fn fresh_key(&self, id: u64) -> Option<PKey<Public>> {
        self.fetched_at
            .filter(|at| at.elapsed() < KEY_CACHE_TTL)
            .and_then(|_| self.keys.get(&id).cloned())
    }
}

#[derive(Clone)]
pub struct AdMobVerifier {
    client: reqwest::Client,
    cache: Arc<Mutex<KeyCache>>,
    refresh_gate: Arc<Semaphore>,
    refresh_waiters: Arc<Semaphore>,
    verification_slots: Arc<Semaphore>,
}
#[derive(Deserialize)]
struct KeyDocument {
    keys: Vec<KeyEntry>,
}
#[derive(Deserialize)]
struct KeyEntry {
    #[serde(rename = "keyId")]
    id: u64,
    pem: String,
}
impl AdMobVerifier {
    pub fn new() -> Result<Self, VerificationError> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(KEY_REQUEST_TIMEOUT)
            .build()
            .map_err(|_| VerificationError::Unavailable)?;
        Ok(Self {
            client,
            cache: Arc::new(Mutex::new(KeyCache::default())),
            refresh_gate: Arc::new(Semaphore::new(1)),
            refresh_waiters: Arc::new(Semaphore::new(MAX_KEY_REFRESH_WAITERS)),
            verification_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_VERIFICATIONS)),
        })
    }
    pub async fn verify(&self, raw: &str) -> Result<VerifiedCallback, VerificationError> {
        let signed = parse(raw)?;
        let key = self.key(signed.key_id).await?;
        let _slot = self
            .verification_slots
            .try_acquire()
            .map_err(|_| VerificationError::Unavailable)?;
        verify(&key, &signed)?;
        Ok(signed.callback)
    }

    async fn key(&self, id: u64) -> Result<PKey<Public>, VerificationError> {
        if let Some(key) = self.cache.lock().await.fresh_key(id) {
            return Ok(key);
        }
        // Bound cache-miss waiters independently from signature verification:
        // an unknown-key burst must not consume the fresh-key verification slots.
        let _waiter = self
            .refresh_waiters
            .try_acquire()
            .map_err(|_| VerificationError::Unavailable)?;
        let _refresh = tokio::time::timeout(KEY_REQUEST_TIMEOUT, self.refresh_gate.acquire())
            .await
            .map_err(|_| VerificationError::Unavailable)?
            .map_err(|_| VerificationError::Unavailable)?;
        {
            let mut cache = self.cache.lock().await;
            if let Some(key) = cache.fresh_key(id) {
                return Ok(key);
            }
            if cache
                .failed_at
                .is_some_and(|at| at.elapsed() < KEY_REFRESH_COOLDOWN)
            {
                return Err(VerificationError::Unavailable);
            }
            let fresh = cache
                .fetched_at
                .is_some_and(|at| at.elapsed() < KEY_CACHE_TTL);
            let unknown = !cache.keys.contains_key(&id);
            if fresh
                && unknown
                && cache
                    .unknown_refresh_at
                    .is_some_and(|at| at.elapsed() < KEY_REFRESH_COOLDOWN)
            {
                return Err(invalid_callback("admob_key_id_unknown"));
            }
            // Set this before awaiting: failure or cancellation also observes
            // the cooldown. Expired keys are never accepted as a fallback.
            cache.failed_at = Some(Instant::now());
            if unknown {
                cache.unknown_refresh_at = Some(Instant::now());
            }
        }
        let keys = self.fetch_keys().await?;
        let mut cache = self.cache.lock().await;
        cache.keys = keys;
        cache.fetched_at = Some(Instant::now());
        cache.failed_at = None;
        cache
            .keys
            .get(&id)
            .cloned()
            .ok_or_else(|| invalid_callback("admob_key_id_unknown"))
    }
    async fn fetch_keys(&self) -> Result<BTreeMap<u64, PKey<Public>>, VerificationError> {
        let mut response = self
            .client
            .get(KEYS_URL)
            .send()
            .await
            .map_err(|_| VerificationError::Unavailable)?
            .error_for_status()
            .map_err(|_| VerificationError::Unavailable)?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| VerificationError::Unavailable)?
        {
            if bytes.len() + chunk.len() > MAX_KEY_DOCUMENT_BYTES {
                return Err(VerificationError::Unavailable);
            }
            bytes.extend_from_slice(&chunk);
        }
        let document: KeyDocument =
            serde_json::from_slice(&bytes).map_err(|_| VerificationError::Unavailable)?;
        let mut keys = BTreeMap::new();
        for entry in document.keys {
            let key = PKey::public_key_from_pem(entry.pem.as_bytes())
                .map_err(|_| VerificationError::Unavailable)?;
            if key
                .ec_key()
                .map_err(|_| VerificationError::Unavailable)?
                .group()
                .curve_name()
                != Some(openssl::nid::Nid::X9_62_PRIME256V1)
            {
                return Err(VerificationError::Unavailable);
            }
            if keys.insert(entry.id, key).is_some() {
                return Err(VerificationError::Unavailable);
            }
        }
        if keys.is_empty() {
            return Err(VerificationError::Unavailable);
        }
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openssl::{
        ec::{EcGroup, EcKey},
        nid::Nid,
        sign::Signer,
    };
    fn signed_query() -> (PKey<Public>, String) {
        sign_content(&format!(
            "ad_network=5450213213286189855&ad_unit=123&custom_data={}&reward_amount=1&reward_item=continuation_credit&timestamp=1791907200000&transaction_id=abcdef",
            Uuid::new_v4()
        ))
    }

    fn sign_content(content: &str) -> (PKey<Public>, String) {
        let key = PKey::from_ec_key(
            EcKey::generate(&EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap()).unwrap(),
        )
        .unwrap();
        let mut signer = Signer::new(MessageDigest::sha256(), &key).unwrap();
        signer.update(content.as_bytes()).unwrap();
        let raw = format!(
            "{content}&signature={}&key_id=42",
            URL_SAFE_NO_PAD.encode(signer.sign_to_vec().unwrap())
        );
        (
            PKey::public_key_from_pem(&key.public_key_to_pem().unwrap()).unwrap(),
            raw,
        )
    }
    #[test]
    fn signature_authenticates_original_bytes_and_rejects_tampering() {
        let (key, raw) = signed_query();
        let escaped = raw.replace("&signature=M", "&signature=%4D");
        verify(&key, &parse(&escaped).unwrap()).unwrap();
        verify(&key, &parse(&raw).unwrap()).unwrap();
        assert_eq!(
            verify(
                &key,
                &parse(&raw.replace("reward_amount=1", "reward_amount=2")).unwrap()
            ),
            Err(VerificationError::Invalid)
        );
        assert!(parse(&raw.replace("&signature=", "&reward_amount=1&signature=")).is_err());
        assert!(parse(&format!("{raw}&ignored=1")).is_err());
        assert!(parse(&raw.replace("&signature=", "&user_id=account-id&signature=")).is_err());
    }

    #[test]
    fn rejected_signature_logs_the_reason_without_callback_data() {
        // Tracing's callsite cache is process-wide. Isolate the temporary
        // subscriber from parallel tests that emit through the same callsite.
        const CHILD: &str = "HISTAE_ADMOB_LOG_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "advertising::ssv::tests::rejected_signature_logs_the_reason_without_callback_data",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .output()
                .expect("isolated logging test");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            return;
        }

        #[derive(Clone)]
        struct LogWriter(Arc<std::sync::Mutex<Vec<u8>>>);

        impl std::io::Write for LogWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let (key, raw) = signed_query();
        let tampered = raw.replace("continuation_credit", "private_reward_value");
        let signed = parse(&tampered).unwrap();
        let output = Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = LogWriter(output.clone());
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_target(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || writer.clone())
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            assert_eq!(verify(&key, &signed), Err(VerificationError::Invalid));
        });

        let log = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(log.contains("admob_callback_rejected"));
        assert!(log.contains("error_code=admob_signature_mismatch"));
        let VerifiedCallback::Reward(callback) = &signed.callback else {
            panic!("a reward UUID must remain a reward callback");
        };
        assert!(!log.contains(&callback.intent_id.to_string()));
        assert!(!log.contains("private_reward_value"));
        assert!(!log.contains("custom_data="));
        assert!(!log.contains("signature="));
        assert!(!log.contains(&URL_SAFE_NO_PAD.encode(&signed.signature)));
    }

    #[tokio::test]
    async fn console_url_validation_requires_a_valid_signature_and_explicit_marker() {
        let content = format!(
            "ad_network=5450213213286189855&ad_unit=1234567890&custom_data={URL_VALIDATION_CUSTOM_DATA}&reward_amount=1&reward_item=continuation_credit&timestamp=1791907200000&transaction_id=123456789"
        );
        let (key, raw) = sign_content(&content);
        let verifier = AdMobVerifier::new().unwrap();
        {
            let mut cache = verifier.cache.lock().await;
            cache.keys.insert(42, key);
            cache.fetched_at = Some(Instant::now());
        }
        assert!(matches!(
            verifier.verify(&raw).await,
            Ok(VerifiedCallback::UrlValidation)
        ));
        assert!(matches!(
            verifier
                .verify(&raw.replace("reward_amount=1", "reward_amount=2"))
                .await,
            Err(VerificationError::Invalid)
        ));
        assert!(matches!(
            verifier
                .verify(&format!("custom_data={URL_VALIDATION_CUSTOM_DATA}"))
                .await,
            Err(VerificationError::Invalid)
        ));

        let intent_id = Uuid::new_v4();
        let (key, reward_query) =
            sign_content(&content.replace(URL_VALIDATION_CUSTOM_DATA, &intent_id.to_string()));
        verifier.cache.lock().await.keys.insert(42, key);
        let Ok(VerifiedCallback::Reward(callback)) = verifier.verify(&reward_query).await else {
            panic!("a signed reward UUID must not become URL validation");
        };
        assert_eq!(callback.intent_id, intent_id);
        assert_eq!(callback.ad_unit, "1234567890");
    }

    #[tokio::test]
    async fn unknown_key_and_invalid_signature_do_not_use_stale_or_other_keys() {
        let (key, raw) = signed_query();
        let verifier = AdMobVerifier::new().unwrap();
        {
            let mut cache = verifier.cache.lock().await;
            cache.keys.insert(42, key);
            cache.fetched_at = Some(Instant::now());
            cache.failed_at = None;
            cache.unknown_refresh_at = Some(Instant::now());
        }
        assert!(verifier.verify(&raw).await.is_ok());
        assert!(matches!(
            verifier
                .verify(&raw.replace("key_id=42", "key_id=43"))
                .await,
            Err(VerificationError::Invalid)
        ));
    }

    #[tokio::test]
    async fn fresh_keys_remain_available_during_refresh_and_after_refresh_failure() {
        let (key, raw) = signed_query();
        let verifier = AdMobVerifier::new().unwrap();
        {
            let mut cache = verifier.cache.lock().await;
            cache.keys.insert(42, key);
            cache.fetched_at = Some(Instant::now());
            cache.failed_at = Some(Instant::now());
        }
        let refresh = verifier.refresh_gate.try_acquire().unwrap();
        let waiting_verifier = verifier.clone();
        let unknown = raw.replace("key_id=42", "key_id=43");
        let waiting = tokio::spawn(async move { waiting_verifier.verify(&unknown).await });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        let result = tokio::time::timeout(Duration::from_millis(100), verifier.verify(&raw))
            .await
            .expect("a fresh key must not wait for the downloader");
        assert!(result.is_ok());
        drop(refresh);
        assert!(matches!(
            waiting.await.unwrap(),
            Err(VerificationError::Unavailable)
        ));
        assert!(verifier.verify(&raw).await.is_ok());
    }

    #[tokio::test]
    async fn expired_keys_are_rejected_during_refresh_and_failure_cooldown() {
        let (key, raw) = signed_query();
        let verifier = AdMobVerifier::new().unwrap();
        {
            let mut cache = verifier.cache.lock().await;
            cache.keys.insert(42, key);
            cache.fetched_at = Some(Instant::now() - KEY_CACHE_TTL);
            cache.failed_at = Some(Instant::now());
        }
        let refresh = verifier.refresh_gate.try_acquire().unwrap();
        let waiting_verifier = verifier.clone();
        let waiting_raw = raw.clone();
        let waiting = tokio::spawn(async move { waiting_verifier.verify(&waiting_raw).await });
        tokio::task::yield_now().await;
        assert!(
            !waiting.is_finished(),
            "expired keys cannot satisfy a cache miss"
        );
        drop(refresh);
        assert!(matches!(
            waiting.await.unwrap(),
            Err(VerificationError::Unavailable)
        ));
        assert!(matches!(
            verifier.verify(&raw).await,
            Err(VerificationError::Unavailable)
        ));
    }

    #[tokio::test]
    async fn verification_capacity_is_bounded_without_a_waiting_queue() {
        let (key, raw) = signed_query();
        let verifier = AdMobVerifier::new().unwrap();
        {
            let mut cache = verifier.cache.lock().await;
            cache.keys.insert(42, key);
            cache.fetched_at = Some(Instant::now());
        }
        let slots = verifier
            .verification_slots
            .try_acquire_many(MAX_CONCURRENT_VERIFICATIONS as u32)
            .unwrap();
        let result = tokio::time::timeout(Duration::from_millis(100), verifier.verify(&raw))
            .await
            .expect("capacity exhaustion must not enqueue a request");
        assert!(matches!(result, Err(VerificationError::Unavailable)));
        drop(slots);
        assert!(verifier.verify(&raw).await.is_ok());
    }

    #[tokio::test]
    async fn refresh_waiters_are_bounded_and_share_the_downloaded_keys() {
        let (key, raw) = signed_query();
        let verifier = AdMobVerifier::new().unwrap();
        {
            let mut cache = verifier.cache.lock().await;
            cache.keys.insert(42, key.clone());
            cache.fetched_at = Some(Instant::now());
        }
        let waiters = verifier
            .refresh_waiters
            .try_acquire_many(MAX_KEY_REFRESH_WAITERS as u32)
            .unwrap();
        let unknown = raw.replace("key_id=42", "key_id=43");
        assert!(matches!(
            verifier.verify(&unknown).await,
            Err(VerificationError::Unavailable)
        ));
        assert!(
            verifier.verify(&raw).await.is_ok(),
            "refresh saturation must not block a fresh key"
        );
        drop(waiters);

        let refresh = verifier.refresh_gate.try_acquire().unwrap();
        let waiting_verifier = verifier.clone();
        let waiting = tokio::spawn(async move { waiting_verifier.verify(&unknown).await });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        {
            let mut cache = verifier.cache.lock().await;
            cache.keys.insert(43, key);
            cache.fetched_at = Some(Instant::now());
        }
        drop(refresh);
        assert!(waiting.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn malformed_signatures_are_rejected_before_key_fetch_or_admission() {
        let (_, raw) = signed_query();
        let (content, _) = raw.rsplit_once("&signature=").unwrap();
        let verifier = AdMobVerifier::new().unwrap();
        let _slots = verifier
            .verification_slots
            .try_acquire_many(MAX_CONCURRENT_VERIFICATIONS as u32)
            .unwrap();
        for signature in ["", "AA", &URL_SAFE_NO_PAD.encode([0_u8; 73])] {
            assert!(matches!(
                verifier
                    .verify(&format!("{content}&signature={signature}&key_id=42"))
                    .await,
                Err(VerificationError::Invalid)
            ));
        }
        assert!(verifier.cache.lock().await.failed_at.is_none());
    }
}
