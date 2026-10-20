use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    Fr,
    En,
    Es,
    It,
}

impl Language {
    pub const ALL: [Self; 4] = [Self::Fr, Self::En, Self::Es, Self::It];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fr => "fr",
            Self::En => "en",
            Self::Es => "es",
            Self::It => "it",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "fr" => Some(Self::Fr),
            "en" => Some(Self::En),
            "es" => Some(Self::Es),
            "it" => Some(Self::It),
            _ => None,
        }
    }

    /// Select supported primary tags by quality, preserving header order on ties.
    /// Malformed, oversized and unsupported headers have a deterministic French fallback.
    pub fn negotiate(value: &str) -> Self {
        if value.len() > 1024 {
            return Self::Fr;
        }
        let mut best = None;
        for item in value.split(',').take(32) {
            let mut parts = item.trim().split(';');
            let tag = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
            let Some(language) = Self::parse(tag.split('-').next().unwrap_or_default()) else {
                continue;
            };
            if !tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                continue;
            }
            let mut quality = 1.0_f64;
            let mut valid = true;
            for parameter in parts {
                let Some(raw) = parameter.trim().strip_prefix("q=") else {
                    valid = false;
                    break;
                };
                match raw.parse::<f64>() {
                    Ok(q) if q.is_finite() && (0.0..=1.0).contains(&q) => quality = q,
                    _ => {
                        valid = false;
                        break;
                    }
                }
            }
            if valid && quality > 0.0 && best.is_none_or(|(_, q)| quality > q) {
                best = Some((language, quality));
            }
        }
        best.map_or(Self::Fr, |(language, _)| language)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn negotiates_quality_regions_and_safe_fallbacks() {
        for (header, expected) in [
            ("de, it-IT;q=0.8, en;q=0.5", Language::It),
            ("en-US,en;q=0.9,fr;q=0.8", Language::En),
            ("es;q=0.5,it;q=0.5", Language::Es),
            ("es;q=0,it;q=1", Language::It),
            ("de, *;q=0.5", Language::Fr),
            ("en;q=NaN,es;q=2,fr", Language::Fr),
            ("IT-it", Language::It),
        ] {
            assert_eq!(Language::negotiate(header), expected, "{header}");
        }
        assert_eq!(Language::negotiate(&"x".repeat(1025)), Language::Fr);
        assert!(serde_json::from_str::<Language>("\"de\"").is_err());
    }
}
