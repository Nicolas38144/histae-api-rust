use std::fmt;
use std::sync::OnceLock;

use regex::Regex;
use tracing::Level;

const ALLOWED_FIELDS: &[&str] = &[
    "batches",
    "billing_processed",
    "duration_ms",
    "environment",
    "error_code",
    "event_id",
    "event_type",
    "failures",
    "method",
    "matches_processed",
    "operation",
    "photo_failures",
    "photos_cleaned",
    "photos_expired_requests",
    "port",
    "privacy_processed",
    "request_id",
    "route",
    "status",
];

#[derive(Clone, Copy, Debug)]
pub enum SafeLogValue<'a> {
    String(&'a str),
    Number(f64),
    Integer(i64),
    Unsigned(u64),
    Bool(bool),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeLogError {
    InvalidEvent,
    UnsafeField,
    UnsafeValue,
    SubscriberUnavailable,
}

impl fmt::Display for SafeLogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidEvent => "invalid_log_event",
            Self::UnsafeField => "unsafe_log_field",
            Self::UnsafeValue => "unsafe_log_value",
            Self::SubscriberUnavailable => "log_subscriber_unavailable",
        })
    }
}

impl std::error::Error for SafeLogError {}

pub fn format_log_event(
    event: &str,
    fields: &[(&str, SafeLogValue<'_>)],
) -> Result<String, SafeLogError> {
    if !event_pattern().is_match(event) {
        return Err(SafeLogError::InvalidEvent);
    }
    let mut output = String::from(event);
    for (key, value) in fields {
        if !ALLOWED_FIELDS.contains(key) {
            return Err(SafeLogError::UnsafeField);
        }
        let rendered = match value {
            SafeLogValue::String(value) if string_value_pattern().is_match(value) => {
                (*value).to_owned()
            }
            SafeLogValue::Number(value) if value.is_finite() => value.to_string(),
            SafeLogValue::Integer(value) => value.to_string(),
            SafeLogValue::Unsigned(value) => value.to_string(),
            SafeLogValue::Bool(value) => value.to_string(),
            _ => return Err(SafeLogError::UnsafeValue),
        };
        output.push(' ');
        output.push_str(key);
        output.push('=');
        output.push_str(&rendered);
    }
    Ok(output)
}

pub fn normalized_error_code(value: Option<&str>) -> String {
    let Some(value) = value.filter(|value| code_pattern().is_match(value)) else {
        return "operation_failed".to_owned();
    };
    let normalized = value.replace('-', "_").to_ascii_lowercase();
    if normalized
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
    {
        normalized
    } else {
        format!("error_{normalized}")
    }
}

pub fn format_error_event(
    event: &str,
    explicit_code: Option<&str>,
    fields: &[(&str, SafeLogValue<'_>)],
) -> Result<String, SafeLogError> {
    let code = normalized_error_code(explicit_code);
    let mut all_fields = Vec::with_capacity(fields.len() + 1);
    all_fields.extend_from_slice(fields);
    all_fields.push(("error_code", SafeLogValue::String(&code)));
    format_log_event(event, &all_fields)
}

pub fn init() -> Result<(), SafeLogError> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("off,histae=info"))
        .with_target(false)
        .without_time()
        .with_ansi(false)
        .try_init()
        .map_err(|_| SafeLogError::SubscriberUnavailable)
}

pub fn info(event: &str, fields: &[(&str, SafeLogValue<'_>)]) -> Result<(), SafeLogError> {
    let line = format_log_event(event, fields)?;
    tracing::event!(target: "histae", Level::INFO, message = %line);
    Ok(())
}

pub fn error(event: &str, code: Option<&str>) -> Result<(), SafeLogError> {
    let line = format_error_event(event, code, &[])?;
    tracing::event!(target: "histae", Level::ERROR, message = %line);
    Ok(())
}

fn event_pattern() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| Regex::new(r"^[a-z][a-z0-9_]{0,63}$").unwrap_or_else(|_| unreachable!()))
}

fn string_value_pattern() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(r"^[A-Za-z0-9_./:<>{}*,-]{1,200}$").unwrap_or_else(|_| unreachable!())
    })
}

fn code_pattern() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(r"^(?:[A-Za-z][A-Za-z0-9_-]{0,63}|[0-9][A-Za-z0-9_-]{0,56})$")
            .unwrap_or_else(|_| unreachable!())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_nest_safe_log_format() {
        let output = format_log_event(
            "http_request_failed",
            &[
                ("method", SafeLogValue::String("GET")),
                ("route", SafeLogValue::String("/api/users/:id")),
                ("status", SafeLogValue::Integer(503)),
                (
                    "request_id",
                    SafeLogValue::String("123e4567-e89b-42d3-a456-426614174000"),
                ),
                ("duration_ms", SafeLogValue::Number(12.4)),
            ],
        );
        assert_eq!(
            output.as_deref(),
            Ok(
                "http_request_failed method=GET route=/api/users/:id status=503 request_id=123e4567-e89b-42d3-a456-426614174000 duration_ms=12.4"
            )
        );
    }

    #[test]
    fn rejects_sensitive_fields_and_unbounded_values() {
        assert_eq!(
            format_log_event(
                "unsafe_attempt",
                &[("phone_number", SafeLogValue::String("+33600000000"))]
            ),
            Err(SafeLogError::UnsafeField)
        );
        assert_eq!(
            format_log_event(
                "unsafe_attempt",
                &[(
                    "operation",
                    SafeLogValue::String("Bearer-private-token=secret")
                )]
            ),
            Err(SafeLogError::UnsafeValue)
        );
        assert_eq!(
            format_log_event(
                "unsafe_attempt",
                &[("duration_ms", SafeLogValue::Number(f64::NAN))]
            ),
            Err(SafeLogError::UnsafeValue)
        );
    }

    #[test]
    fn normalizes_only_bounded_explicit_codes() {
        assert_eq!(normalized_error_code(Some("ECONNREFUSED")), "econnrefused");
        assert_eq!(
            normalized_error_code(Some("404-NOT-FOUND")),
            "error_404_not_found"
        );
        assert_eq!(
            normalized_error_code(Some("private error content")),
            "operation_failed"
        );
    }
}
