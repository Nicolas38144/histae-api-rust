use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AdminRole {
    Admin,
    Superadmin,
}

impl AdminRole {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "admin" => Some(Self::Admin),
            "superadmin" => Some(Self::Superadmin),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Superadmin => "superadmin",
        }
    }

    pub fn audit_reason<'a>(self, supplied: &'a str, automatic: &'static str) -> &'a str {
        if self == Self::Superadmin && crate::shared::text::javascript_trim(supplied).is_empty() {
            automatic
        } else {
            supplied
        }
    }
}
