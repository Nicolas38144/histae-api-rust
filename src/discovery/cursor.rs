use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::{Uuid, Variant};

use super::domain::DiscoveryCursor;
use super::service::DiscoveryError;
use crate::config::SecretString;
use crate::shared::clock::Clock;

const VERSION: u8 = 1;
const NONCE_BYTES: usize = 12;
const PAYLOAD_BYTES: usize = 8 + 16 + 8;
const TOKEN_BYTES: usize = 1 + NONCE_BYTES + PAYLOAD_BYTES + 16;
const TTL_SECONDS: i64 = 900;
const KEY_CONTEXT: &[u8] = b"histae/discovery/cursor/v1/aes256gcm";

/// Encrypts pagination state with a purpose-specific key and binds it to its viewer.
#[derive(Clone)]
pub struct FeedCursorCodec {
    cipher: Aes256Gcm,
    clock: Arc<dyn Clock>,
}

impl FeedCursorCodec {
    pub fn new(secret: &SecretString, clock: Arc<dyn Clock>) -> Result<Self, DiscoveryError> {
        if secret.expose_secret().len() < 32 {
            return Err(DiscoveryError::Unavailable);
        }
        // Domain separation: never use the JWT signing key directly for encryption.
        let mut derivation =
            <Hmac<Sha256> as Mac>::new_from_slice(secret.expose_secret().as_bytes())
                .map_err(|_| DiscoveryError::Unavailable)?;
        derivation.update(KEY_CONTEXT);
        let key = derivation.finalize().into_bytes();
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| DiscoveryError::Unavailable)?;
        Ok(Self { cipher, clock })
    }

    pub fn encode(&self, viewer: Uuid, cursor: DiscoveryCursor) -> Result<String, DiscoveryError> {
        if !valid_cursor(cursor) {
            return Err(DiscoveryError::Unavailable);
        }
        let expires_at = self
            .clock
            .now()
            .timestamp()
            .checked_add(TTL_SECONDS)
            .ok_or(DiscoveryError::Unavailable)?;
        // Fixed-length plaintext also hides the number of digits in the precise distance.
        let mut plaintext = [0_u8; PAYLOAD_BYTES];
        plaintext[..8].copy_from_slice(&cursor.distance_km.to_be_bytes());
        plaintext[8..24].copy_from_slice(cursor.id.as_bytes());
        plaintext[24..].copy_from_slice(&expires_at.to_be_bytes());
        let mut nonce = [0_u8; NONCE_BYTES];
        getrandom::fill(&mut nonce).map_err(|_| DiscoveryError::Unavailable)?;
        let encrypted = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: viewer.as_bytes(),
                },
            )
            .map_err(|_| DiscoveryError::Unavailable)?;
        let mut token = Vec::with_capacity(TOKEN_BYTES);
        token.push(VERSION);
        token.extend_from_slice(&nonce);
        token.extend_from_slice(&encrypted);
        Ok(URL_SAFE_NO_PAD.encode(token))
    }

    pub fn decode(
        &self,
        viewer: Uuid,
        value: Option<&str>,
    ) -> Result<Option<DiscoveryCursor>, DiscoveryError> {
        let Some(value) = value.filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        if value.len() > 128 {
            return Err(DiscoveryError::InvalidCursor);
        }
        let token = URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| DiscoveryError::InvalidCursor)?;
        if token.len() != TOKEN_BYTES || token[0] != VERSION {
            return Err(DiscoveryError::InvalidCursor);
        }
        let plaintext = self
            .cipher
            .decrypt(
                Nonce::from_slice(&token[1..1 + NONCE_BYTES]),
                Payload {
                    msg: &token[1 + NONCE_BYTES..],
                    aad: viewer.as_bytes(),
                },
            )
            .map_err(|_| DiscoveryError::InvalidCursor)?;
        if plaintext.len() != PAYLOAD_BYTES {
            return Err(DiscoveryError::InvalidCursor);
        }
        let distance_km = f64::from_be_bytes(
            plaintext[..8]
                .try_into()
                .map_err(|_| DiscoveryError::InvalidCursor)?,
        );
        let id = Uuid::from_slice(&plaintext[8..24]).map_err(|_| DiscoveryError::InvalidCursor)?;
        let expires_at = i64::from_be_bytes(
            plaintext[24..]
                .try_into()
                .map_err(|_| DiscoveryError::InvalidCursor)?,
        );
        let cursor = DiscoveryCursor { distance_km, id };
        if expires_at <= self.clock.now().timestamp() || !valid_cursor(cursor) {
            return Err(DiscoveryError::InvalidCursor);
        }
        Ok(Some(cursor))
    }
}

fn valid_cursor(cursor: DiscoveryCursor) -> bool {
    cursor.distance_km.is_finite()
        && cursor.distance_km >= 0.0
        && (1..=8).contains(&cursor.id.get_version_num())
        && cursor.id.get_variant() == Variant::RFC4122
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, Ordering};

    use chrono::{DateTime, Utc};

    use super::*;

    struct TestClock(AtomicI64);

    impl Clock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            DateTime::from_timestamp(self.0.load(Ordering::SeqCst), 0).expect("test timestamp")
        }
    }

    fn codec(clock: Arc<TestClock>, key: char) -> FeedCursorCodec {
        FeedCursorCodec::new(&SecretString::new(key.to_string().repeat(32)), clock)
            .expect("test key")
    }

    fn position() -> DiscoveryCursor {
        DiscoveryCursor {
            distance_km: 1.23456,
            id: Uuid::new_v4(),
        }
    }

    #[test]
    fn hides_precise_distance_and_preserves_pagination_state() {
        let clock = Arc::new(TestClock(AtomicI64::new(1_000)));
        let codec = codec(clock.clone(), 'k');
        let viewer = Uuid::new_v4();
        let position = position();
        let token = codec.encode(viewer, position).expect("cursor");
        let bytes = URL_SAFE_NO_PAD.decode(&token).expect("base64");
        assert!(serde_json::from_slice::<serde_json::Value>(&bytes).is_err());
        assert!(
            !bytes
                .windows(8)
                .any(|bytes| bytes == position.distance_km.to_be_bytes())
        );
        assert!(
            !bytes
                .windows(16)
                .any(|bytes| bytes == position.id.as_bytes())
        );
        assert_eq!(codec.decode(viewer, Some(&token)), Ok(Some(position)));
        assert_ne!(codec.encode(viewer, position).expect("fresh nonce"), token);
        let distant = DiscoveryCursor {
            distance_km: 999.123456789,
            ..position
        };
        assert_eq!(
            codec.encode(viewer, distant).expect("cursor").len(),
            token.len()
        );
        assert_eq!(
            self::codec(clock, 'k').decode(viewer, Some(&token)),
            Ok(Some(position))
        );
    }

    #[test]
    fn rejects_other_viewers_tampering_and_other_keys() {
        let clock = Arc::new(TestClock(AtomicI64::new(1_000)));
        let codec = codec(clock.clone(), 'k');
        let viewer = Uuid::new_v4();
        let token = codec.encode(viewer, position()).expect("cursor");
        assert_eq!(
            codec.decode(Uuid::new_v4(), Some(&token)),
            Err(DiscoveryError::InvalidCursor)
        );
        assert_eq!(
            self::codec(clock, 'z').decode(viewer, Some(&token)),
            Err(DiscoveryError::InvalidCursor)
        );
        let mut bytes = URL_SAFE_NO_PAD.decode(&token).expect("base64");
        for index in [0, 1, TOKEN_BYTES - 1] {
            bytes[index] ^= 1;
            assert_eq!(
                codec.decode(viewer, Some(&URL_SAFE_NO_PAD.encode(&bytes))),
                Err(DiscoveryError::InvalidCursor)
            );
            bytes[index] ^= 1;
        }
    }

    #[test]
    fn expires_and_rejects_legacy_or_malformed_tokens() {
        let clock = Arc::new(TestClock(AtomicI64::new(1_000)));
        let codec = codec(clock.clone(), 'k');
        let viewer = Uuid::new_v4();
        let token = codec.encode(viewer, position()).expect("cursor");
        clock.0.store(1_000 + TTL_SECONDS - 1, Ordering::SeqCst);
        assert!(codec.decode(viewer, Some(&token)).is_ok());
        clock.0.store(1_000 + TTL_SECONDS, Ordering::SeqCst);
        assert_eq!(
            codec.decode(viewer, Some(&token)),
            Err(DiscoveryError::InvalidCursor)
        );
        let legacy = URL_SAFE_NO_PAD
            .encode(br#"{"distance_km":1.23456,"id":"00000000-0000-4000-8000-000000000001"}"#);
        for invalid in [legacy, "not-a-cursor".to_owned(), "a".repeat(129)] {
            assert_eq!(
                codec.decode(viewer, Some(&invalid)),
                Err(DiscoveryError::InvalidCursor)
            );
        }
        assert_eq!(codec.decode(viewer, None), Ok(None));
    }
}
