use super::ConfigError;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
};

#[derive(Clone, Default)]
pub struct EnvironmentSource(BTreeMap<OsString, OsString>);

impl std::fmt::Debug for EnvironmentSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EnvironmentSource")
            .field("entry_count", &self.0.len())
            .finish_non_exhaustive()
    }
}

impl EnvironmentSource {
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        Self(
            pairs
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        )
    }

    pub(super) fn value(&self, name: &'static str) -> Result<Option<String>, ConfigError> {
        let Some(value) = self.0.get(OsStr::new(name)) else {
            return Ok(None);
        };
        let value = value.to_str().ok_or_else(|| ConfigError::invalid(name))?;
        Ok(Some(value.trim().to_owned()))
    }

    pub(super) fn raw_value(&self, name: &'static str) -> Result<Option<String>, ConfigError> {
        let Some(value) = self.0.get(OsStr::new(name)) else {
            return Ok(None);
        };
        Ok(Some(
            value
                .to_str()
                .ok_or_else(|| ConfigError::invalid(name))?
                .to_owned(),
        ))
    }

    pub(super) fn or(&self, name: &'static str, fallback: &str) -> Result<String, ConfigError> {
        Ok(self
            .value(name)?
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| fallback.to_owned()))
    }

    pub(super) fn required(&self, name: &'static str) -> Result<String, ConfigError> {
        self.value(name)?
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ConfigError::missing(name))
    }
}
