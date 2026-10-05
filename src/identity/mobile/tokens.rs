use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, decode_header, encode,
};
use serde::{Deserialize, Serialize};
use uuid::{Builder, Uuid, Variant, Version};

use crate::config::JwtConfig;
use crate::infra::crypto::sha256_hex;

pub const ACCESS_TOKEN_ISSUER: &str = "histae-api";
pub const ACCESS_TOKEN_AUDIENCE: &str = "histae-app";
pub const ACCESS_TOKEN_TYPE: &str = "access";

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewRefreshToken {
    pub id: Uuid,
    pub jti: Uuid,
    pub plain: String,
    pub hash: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedRefreshToken {
    pub jti: Uuid,
    pub hash: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedAccessToken {
    pub user_id: Uuid,
    pub session_id: Uuid,
    pub expires_at_seconds: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenError {
    RandomUnavailable,
    TimeOutOfRange,
    SigningFailed,
    InvalidAccessToken,
}

impl TokenError {
    pub fn safe_code(self) -> &'static str {
        match self {
            Self::RandomUnavailable => "token_random_unavailable",
            Self::TimeOutOfRange => "token_time_out_of_range",
            Self::SigningFailed => "token_signing_failed",
            Self::InvalidAccessToken => "invalid_access_token",
        }
    }
}

impl fmt::Display for TokenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.safe_code())
    }
}

impl std::error::Error for TokenError {}

#[derive(Clone)]
pub struct TokenService {
    config: JwtConfig,
    clock: Arc<dyn Clock>,
}

impl TokenService {
    pub fn new(config: JwtConfig) -> Self {
        Self::with_clock(config, Arc::new(SystemClock))
    }

    pub fn with_clock(config: JwtConfig, clock: Arc<dyn Clock>) -> Self {
        Self { config, clock }
    }

    pub fn new_refresh_token(&self) -> Result<NewRefreshToken, TokenError> {
        let id = random_uuid_v4()?;
        let jti = random_uuid_v4()?;
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|_| TokenError::RandomUnavailable)?;
        let encoded = URL_SAFE_NO_PAD.encode(secret);
        let created_at = self.clock.now();
        let ttl = ChronoDuration::from_std(self.config.refresh_ttl)
            .map_err(|_| TokenError::TimeOutOfRange)?;
        let expires_at = created_at
            .checked_add_signed(ttl)
            .ok_or(TokenError::TimeOutOfRange)?;
        Ok(NewRefreshToken {
            id,
            jti,
            plain: format!("{}:{encoded}", jti.hyphenated()),
            hash: sha256_hex(encoded.as_bytes()),
            created_at,
            expires_at,
        })
    }

    pub fn parse_refresh_token(value: &str) -> Option<ParsedRefreshToken> {
        let (jti, secret) = value.split_once(':')?;
        if secret.len() != 43
            || !secret
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || value[jti.len() + 1..].contains(':')
        {
            return None;
        }
        let jti = crate::shared::validation::canonical_uuid(jti)?;
        if jti.get_version() != Some(Version::Random) || jti.get_variant() != Variant::RFC4122 {
            return None;
        }
        Some(ParsedRefreshToken {
            jti,
            hash: sha256_hex(secret.as_bytes()),
        })
    }

    pub fn access_token(&self, user_id: Uuid, session_id: Uuid) -> Result<String, TokenError> {
        let now = self.clock.now().timestamp();
        let issued_at = u64::try_from(now).map_err(|_| TokenError::TimeOutOfRange)?;
        self.access_token_at(user_id, session_id, issued_at)
    }

    pub fn access_token_at(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        issued_at: u64,
    ) -> Result<String, TokenError> {
        let ttl = self.config.access_ttl.as_secs();
        let exp = issued_at
            .checked_add(ttl)
            .ok_or(TokenError::TimeOutOfRange)?;
        if exp > 9_007_199_254_740_991 {
            return Err(TokenError::TimeOutOfRange);
        }
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some(self.config.active_kid.clone());
        encode(
            &header,
            &AccessClaims {
                sub: user_id.hyphenated().to_string(),
                sid: session_id.hyphenated().to_string(),
                typ: ACCESS_TOKEN_TYPE,
                iat: issued_at,
                exp,
                aud: ACCESS_TOKEN_AUDIENCE,
                iss: ACCESS_TOKEN_ISSUER,
            },
            &EncodingKey::from_secret(self.config.secret.expose_secret().as_bytes()),
        )
        .map_err(|_| TokenError::SigningFailed)
    }

    pub fn verify_access_token(&self, token: &str) -> Result<VerifiedAccessToken, TokenError> {
        let header = decode_header(token).map_err(|_| TokenError::InvalidAccessToken)?;
        if header.alg != Algorithm::HS256 {
            return Err(TokenError::InvalidAccessToken);
        }
        let kid = header.kid.ok_or(TokenError::InvalidAccessToken)?;
        let key = self
            .config
            .verification_keys
            .get(&kid)
            .ok_or(TokenError::InvalidAccessToken)?;
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_audience(&[ACCESS_TOKEN_AUDIENCE]);
        validation.set_issuer(&[ACCESS_TOKEN_ISSUER]);
        validation.set_required_spec_claims(&["exp", "sub", "iss", "aud"]);
        validation.leeway = 0;
        // Use the injected clock and Nest's exact NumericDate comparisons.
        // jsonwebtoken rounds fractional nbf values and uses its own clock.
        validation.validate_exp = false;
        validation.validate_nbf = false;
        let claims = decode::<AccessClaimsOwned>(
            token,
            &DecodingKey::from_secret(key.expose_secret().as_bytes()),
            &validation,
        )
        .map_err(|_| TokenError::InvalidAccessToken)?
        .claims;
        let now = u64::try_from(self.clock.now().timestamp())
            .map_err(|_| TokenError::InvalidAccessToken)?;
        if claims.typ != ACCESS_TOKEN_TYPE
            || claims.iss != ACCESS_TOKEN_ISSUER
            || claims.exp > 9_007_199_254_740_991
            || claims.exp <= now
            || claims.nbf > now as f64
        {
            return Err(TokenError::InvalidAccessToken);
        }
        let user_id = crate::shared::validation::canonical_uuid(&claims.sub)
            .ok_or(TokenError::InvalidAccessToken)?;
        let session_id = crate::shared::validation::canonical_uuid(&claims.sid)
            .ok_or(TokenError::InvalidAccessToken)?;
        if session_id.get_version() != Some(Version::Random)
            || session_id.get_variant() != Variant::RFC4122
        {
            return Err(TokenError::InvalidAccessToken);
        }
        Ok(VerifiedAccessToken {
            user_id,
            session_id,
            expires_at_seconds: claims.exp,
        })
    }
}

#[derive(Serialize)]
struct AccessClaims<'a> {
    sub: String,
    sid: String,
    typ: &'a str,
    iat: u64,
    exp: u64,
    aud: &'a str,
    iss: &'a str,
}

#[derive(Clone, Deserialize)]
struct AccessClaimsOwned {
    sub: String,
    sid: String,
    typ: String,
    exp: u64,
    iss: String,
    #[serde(default)]
    nbf: f64,
}

fn random_uuid_v4() -> Result<Uuid, TokenError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| TokenError::RandomUnavailable)?;
    Ok(Builder::from_random_bytes(bytes).into_uuid())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use hmac::{Hmac, Mac as _};
    use sha2::Sha256;

    use crate::config::SecretString;

    use super::*;

    fn config() -> JwtConfig {
        let secret = SecretString::new("jwt-signing-secret-0123456789abcdef".to_owned());
        JwtConfig {
            secret: secret.clone(),
            active_kid: "primary".to_owned(),
            verification_keys: BTreeMap::from([("primary".to_owned(), secret)]),
            access_ttl: Duration::from_secs(900),
            refresh_ttl: Duration::from_secs(3_600),
        }
    }

    #[test]
    fn rejects_expiry_equality_and_future_not_before_claims() {
        struct Fixed(DateTime<Utc>);
        impl Clock for Fixed {
            fn now(&self) -> DateTime<Utc> {
                self.0
            }
        }
        let now = Utc::now().timestamp();
        let service = TokenService::with_clock(
            config(),
            Arc::new(Fixed(DateTime::from_timestamp(now, 0).expect("clock"))),
        );
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("primary".to_owned());
        for (expiry, nbf, accepted) in [
            (now, now - 1, false),
            (now + 300, now + 100, false),
            (now + 300, now - 1, true),
        ] {
            let claims = serde_json::json!({"sub":Uuid::new_v4(),"sid":Uuid::new_v4(),"typ":"access","exp":expiry,"nbf":nbf,"aud":ACCESS_TOKEN_AUDIENCE,"iss":ACCESS_TOKEN_ISSUER});
            let token = encode(
                &header,
                &claims,
                &EncodingKey::from_secret(config().secret.expose_secret().as_bytes()),
            )
            .expect("sign");
            assert_eq!(service.verify_access_token(&token).is_ok(), accepted);
        }
    }

    #[test]
    fn rejects_missing_or_malformed_identity_claims_even_with_a_valid_signature() {
        let service = TokenService::new(config());
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("primary".to_owned());
        let base = serde_json::json!({
            "sub": Uuid::new_v4(), "sid": Uuid::new_v4(), "typ": "access",
            "exp": Utc::now().timestamp() + 300,
            "iss": ACCESS_TOKEN_ISSUER, "aud": ACCESS_TOKEN_AUDIENCE
        });
        let sign = |claims: &serde_json::Value| {
            encode(
                &header,
                claims,
                &EncodingKey::from_secret(config().secret.expose_secret().as_bytes()),
            )
            .expect("sign fixture")
        };
        for field in ["iss", "aud", "exp", "sub", "sid", "typ"] {
            let mut claims = base.clone();
            claims.as_object_mut().expect("claims object").remove(field);
            assert_eq!(
                service.verify_access_token(&sign(&claims)),
                Err(TokenError::InvalidAccessToken),
                "missing {field}"
            );
        }
        for (field, value) in [
            ("iss", serde_json::json!([ACCESS_TOKEN_ISSUER])),
            ("iss", serde_json::json!(null)),
            ("aud", serde_json::json!(null)),
            ("aud", serde_json::json!(42)),
            ("aud", serde_json::json!([])),
            ("aud", serde_json::json!("another-service")),
            ("nbf", serde_json::json!(null)),
            ("nbf", serde_json::json!("0")),
            (
                "sub",
                serde_json::json!(Uuid::new_v4().simple().to_string()),
            ),
        ] {
            let mut claims = base.clone();
            claims[field] = value;
            assert_eq!(
                service.verify_access_token(&sign(&claims)),
                Err(TokenError::InvalidAccessToken),
                "invalid {field}"
            );
        }
        let mut claims = base;
        claims["aud"] = serde_json::json!(["another-service", ACCESS_TOKEN_AUDIENCE]);
        claims["nbf"] = serde_json::json!(-0.5);
        assert!(service.verify_access_token(&sign(&claims)).is_ok());
    }

    #[test]
    fn fractional_not_before_uses_the_injected_clock_without_rounding() {
        struct Fixed(DateTime<Utc>);
        impl Clock for Fixed {
            fn now(&self) -> DateTime<Utc> {
                self.0
            }
        }
        let now = 2_000_000_000;
        let service = TokenService::with_clock(
            config(),
            Arc::new(Fixed(DateTime::from_timestamp(now, 0).expect("clock"))),
        );
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("primary".to_owned());
        for (nbf, accepted) in [(now as f64, true), (now as f64 + 0.1, false)] {
            let claims = serde_json::json!({"sub":Uuid::new_v4(),"sid":Uuid::new_v4(),"typ":"access","exp":now+300,"nbf":nbf,"aud":ACCESS_TOKEN_AUDIENCE,"iss":ACCESS_TOKEN_ISSUER});
            let token = encode(
                &header,
                &claims,
                &EncodingKey::from_secret(config().secret.expose_secret().as_bytes()),
            )
            .expect("sign");
            assert_eq!(service.verify_access_token(&token).is_ok(), accepted);
        }
    }

    #[test]
    fn signs_and_verifies_the_required_access_claims() {
        let service = TokenService::new(config());
        let user = Uuid::new_v4();
        let session = Uuid::new_v4();
        let token = service
            .access_token_at(user, session, 2_000_000_000)
            .expect("sign vector");
        let header = r#"{"typ":"JWT","alg":"HS256","kid":"primary"}"#;
        let claims = format!(
            r#"{{"sub":"{user}","sid":"{session}","typ":"access","iat":2000000000,"exp":2000000900,"aud":"histae-app","iss":"histae-api"}}"#
        );
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header),
            URL_SAFE_NO_PAD.encode(claims)
        );
        let mut hmac = Hmac::<Sha256>::new_from_slice(config().secret.expose_secret().as_bytes())
            .expect("fixture HMAC key");
        hmac.update(signing_input.as_bytes());
        let expected = format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(hmac.finalize().into_bytes())
        );
        assert_eq!(token, expected);
        assert_eq!(
            service.verify_access_token(&token),
            Ok(VerifiedAccessToken {
                user_id: user,
                session_id: session,
                expires_at_seconds: 2_000_000_900,
            })
        );
    }

    #[test]
    fn parses_only_the_historical_refresh_shape() {
        let jti = Uuid::new_v4();
        let valid = format!("{jti}:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        let parsed = TokenService::parse_refresh_token(&valid).expect("valid refresh fixture");
        assert_eq!(parsed.jti, jti);
        assert_eq!(
            parsed.hash,
            "0f007385b6f9d4b7eeb2748605afe1a984a0a3bfa3f014d09e2a784ce9e5cd1a"
        );
        let mut wrong_version = jti.to_string();
        wrong_version.replace_range(14..15, "1");
        for invalid in [
            String::new(),
            "not-a-uuid:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
            format!("{wrong_version}:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            format!("{jti}:short"),
            format!(
                "{}:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                jti.simple()
            ),
            format!("{jti}:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA!"),
        ] {
            assert_eq!(TokenService::parse_refresh_token(&invalid), None);
        }
    }

    #[test]
    fn generated_refresh_tokens_are_parseable_without_retaining_the_secret_in_the_hash() {
        let service = TokenService::new(config());
        let token = service.new_refresh_token().expect("secure random token");
        let parsed =
            TokenService::parse_refresh_token(&token.plain).expect("parse generated token");
        assert_eq!(parsed.jti, token.jti);
        assert_eq!(parsed.hash, token.hash);
        assert!(
            !token
                .hash
                .contains(token.plain.split_once(':').expect("shape").1)
        );
    }
}
