use super::{ConfigError, EnvironmentSource, PlayReviewConfig, SecretString, validation::boolean};

pub(super) fn load(source: &EnvironmentSource) -> Result<Option<PlayReviewConfig>, ConfigError> {
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
    Ok(Some(PlayReviewConfig {
        phone_number: SecretString::new(phone),
        otp: SecretString::new(otp),
    }))
}
