use chrono::{DateTime, Utc};

use super::{ConfigError, EnvironmentSource, PlayReviewConfig, SecretString, validation::boolean};
use crate::infra::crypto::hmac_sha256;

pub(super) fn load(
    source: &EnvironmentSource,
    hash_key: &SecretString,
) -> Result<Option<PlayReviewConfig>, ConfigError> {
    if !boolean(
        source.value("GOOGLE_PLAY_REVIEW_ENABLED")?.as_deref(),
        false,
        "GOOGLE_PLAY_REVIEW_ENABLED",
    )? {
        return Ok(None);
    }
    let phone = source.required("GOOGLE_PLAY_REVIEW_PHONE_NUMBER")?;
    // French fictional mobile numbers cannot belong to a real subscriber.
    if phone.len() != 12
        || !phone.starts_with("+3363998")
        || !phone.as_bytes()[8..].iter().all(u8::is_ascii_digit)
    {
        return Err(ConfigError::invalid("GOOGLE_PLAY_REVIEW_PHONE_NUMBER"));
    }
    let otp = source.required("GOOGLE_PLAY_REVIEW_OTP")?;
    if otp.len() != 6 || !otp.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ConfigError::invalid("GOOGLE_PLAY_REVIEW_OTP"));
    }
    const EXPIRES_AT: &str = "GOOGLE_PLAY_REVIEW_EXPIRES_AT";
    let expires_at = DateTime::parse_from_rfc3339(&source.required(EXPIRES_AT)?)
        .map_err(|_| ConfigError::invalid(EXPIRES_AT))?
        .with_timezone(&Utc);
    let cycle = format!(
        "google-play-review:v1:{phone}:{otp}:{}",
        expires_at.to_rfc3339()
    );
    let cycle_hash =
        hmac_sha256(&cycle, hash_key).map_err(|_| ConfigError::invalid("PHONE_HASH_KEY"))?;
    Ok(Some(PlayReviewConfig {
        phone_number: SecretString::new(phone),
        otp: SecretString::new(otp),
        expires_at,
        cycle_hash: SecretString::new(cycle_hash),
    }))
}
