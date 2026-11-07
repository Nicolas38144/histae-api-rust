//! Account-wide push choices and private, participant-scoped conversation mutes.
pub mod http;
mod pg;
mod queries;

pub use pg::NotificationPreferenceRepository;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NotificationPreferences {
    pub messages: bool,
    pub new_matches: bool,
    pub continuations: bool,
    pub photos: bool,
    pub reminders: bool,
    pub account_security: bool,
    pub discreet: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreferenceChanges {
    pub messages: Option<bool>,
    pub new_matches: Option<bool>,
    pub continuations: Option<bool>,
    pub photos: Option<bool>,
    pub reminders: Option<bool>,
    pub account_security: Option<bool>,
    pub discreet: Option<bool>,
}

impl PreferenceChanges {
    pub fn is_empty(&self) -> bool {
        self.messages.is_none()
            && self.new_matches.is_none()
            && self.continuations.is_none()
            && self.photos.is_none()
            && self.reminders.is_none()
            && self.account_security.is_none()
            && self.discreet.is_none()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationMute {
    pub muted: bool,
    pub muted_until: Option<DateTime<Utc>>,
}
