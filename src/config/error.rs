use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigErrorKind {
    Missing,
    Invalid,
    Conflict,
    Dotenv,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigError {
    pub variable: &'static str,
    pub kind: ConfigErrorKind,
}

impl ConfigError {
    pub(super) fn missing(variable: &'static str) -> Self {
        Self {
            variable,
            kind: ConfigErrorKind::Missing,
        }
    }
    pub(super) fn invalid(variable: &'static str) -> Self {
        Self {
            variable,
            kind: ConfigErrorKind::Invalid,
        }
    }
    pub(super) fn conflict(variable: &'static str) -> Self {
        Self {
            variable,
            kind: ConfigErrorKind::Conflict,
        }
    }
    pub fn safe_code(self) -> &'static str {
        match self.kind {
            ConfigErrorKind::Missing => "config_missing",
            ConfigErrorKind::Invalid => "config_invalid",
            ConfigErrorKind::Conflict => "config_conflict",
            ConfigErrorKind::Dotenv => "dotenv_failed",
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "configuration error ({})", self.variable)
    }
}
impl std::error::Error for ConfigError {}
