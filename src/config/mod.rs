//! Typed configuration with stable defaults and redacted secrets.
mod database;
mod phone_blocklist;
pub use phone_blocklist::PhoneBlocklistSyncConfig;
mod environment;
mod error;
mod loader;
mod object_storage;
mod play_review;
mod stores;
mod types;
mod validation;
mod web_account;

pub use environment::EnvironmentSource;
pub use error::{ConfigError, ConfigErrorKind};
pub use types::*;

#[cfg(test)]
mod tests;
