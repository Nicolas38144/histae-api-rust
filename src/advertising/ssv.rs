//! AdMob signatures cover the original query bytes, excluding signature and key_id.
use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use openssl::{
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
use tokio::sync::Mutex;
use uuid::Uuid;

const KEYS_URL: &str = "https://www.gstatic.com/admob/reward/verifier-keys.json";
const MAX_QUERY_BYTES: usize = 4_096;
const MAX_KEY_DOCUMENT_BYTES: usize = 65_536;
const KEY_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const KEY_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const KEY_REFRESH_COOLDOWN: Duration = Duration::from_secs(30);

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
struct SignedQuery<'a> {
    content: &'a str,
    signature: Vec<u8>,
    key_id: u64,
    callback: Callback,
}
fn parse(raw: &str) -> Result<SignedQuery<'_>, VerificationError> {
    let invalid = VerificationError::Invalid;
    if raw.len() > MAX_QUERY_BYTES {
        return Err(invalid);
    }
    let (content, tail) = raw.rsplit_once("&signature=").ok_or(invalid)?;
    let (signature, key_id) = tail.split_once("&key_id=").ok_or(invalid)?;
    if signature.contains('&') || key_id.contains('&') {
        return Err(invalid);
    }
    let signature_pair = format!("signature={signature}");
    let decoded_signature = url::form_urlencoded::parse(signature_pair.as_bytes())
        .next()
        .map(|(_, value)| value.into_owned())
        .ok_or(invalid)?;
    let signature = decoded_signature.as_str();
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .or_else(|_| URL_SAFE.decode(signature))
        .map_err(|_| invalid)?;
    let key_id = key_id.parse().map_err(|_| invalid)?;
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
            return Err(invalid);
        }
    }
    let required = |name: &str| {
        fields
            .get(name)
            .cloned()
            .filter(|v| !v.is_empty())
            .ok_or(invalid)
    };
    let transaction_id = required("transaction_id")?;
    if transaction_id.len() > 128 || !transaction_id.bytes().all(|v| v.is_ascii_hexdigit()) {
        return Err(invalid);
    }
    // user_id must never contain a Histae account identifier. Only custom_data is used.
    if fields.get("user_id").is_some_and(|value| !value.is_empty()) {
        return Err(invalid);
    }
    let _network: u64 = required("ad_network")?.parse().map_err(|_| invalid)?;
    let callback = Callback {
        intent_id: Uuid::parse_str(&required("custom_data")?).map_err(|_| invalid)?,
        ad_unit: required("ad_unit")?,
        transaction_id,
        timestamp_ms: required("timestamp")?.parse().map_err(|_| invalid)?,
        reward_amount: required("reward_amount")?.parse().map_err(|_| invalid)?,
        reward_item: required("reward_item")?,
    };
    Ok(SignedQuery {
        content,
        signature,
        key_id,
        callback,
    })
}
fn verify(key: &PKey<Public>, signed: &SignedQuery<'_>) -> Result<(), VerificationError> {
    let mut verifier =
        Verifier::new(MessageDigest::sha256(), key).map_err(|_| VerificationError::Invalid)?;
    verifier
        .update(signed.content.as_bytes())
        .map_err(|_| VerificationError::Invalid)?;
    if verifier
        .verify(&signed.signature)
        .map_err(|_| VerificationError::Invalid)?
    {
        Ok(())
    } else {
        Err(VerificationError::Invalid)
    }
}
#[derive(Default)]
struct KeyCache {
    keys: BTreeMap<u64, PKey<Public>>,
    fetched_at: Option<Instant>,
    failed_at: Option<Instant>,
    unknown_refresh_at: Option<Instant>,
}
#[derive(Clone)]
pub struct AdMobVerifier {
    client: reqwest::Client,
    cache: Arc<Mutex<KeyCache>>,
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
        })
    }
    pub async fn verify(&self, raw: &str) -> Result<Callback, VerificationError> {
        let signed = parse(raw)?;
        let mut cache = self.cache.lock().await;
        let age = cache.fetched_at.map(|at| at.elapsed());
        let unknown = !cache.keys.contains_key(&signed.key_id);
        let refresh_unknown = unknown
            && cache
                .unknown_refresh_at
                .is_none_or(|at| at.elapsed() >= KEY_REFRESH_COOLDOWN);
        if age.is_none_or(|age| age >= KEY_CACHE_TTL) || refresh_unknown {
            if cache
                .failed_at
                .is_some_and(|at| at.elapsed() < KEY_REFRESH_COOLDOWN)
            {
                return Err(VerificationError::Unavailable);
            }
            // Bound retries when Google is unavailable, without accepting stale keys.
            cache.failed_at = Some(Instant::now());
            if unknown {
                cache.unknown_refresh_at = Some(Instant::now());
            }
            cache.keys = self.fetch_keys().await?;
            cache.fetched_at = Some(Instant::now());
            cache.failed_at = None;
        }
        let key = cache
            .keys
            .get(&signed.key_id)
            .cloned()
            .ok_or(VerificationError::Invalid)?;
        drop(cache);
        verify(&key, &signed)?;
        Ok(signed.callback)
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
        let key = PKey::from_ec_key(
            EcKey::generate(&EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap()).unwrap(),
        )
        .unwrap();
        let content = format!(
            "ad_network=5450213213286189855&ad_unit=123&custom_data={}&reward_amount=1&reward_item=continuation_credit&timestamp=1791907200000&transaction_id=abcdef",
            Uuid::new_v4()
        );
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
}
