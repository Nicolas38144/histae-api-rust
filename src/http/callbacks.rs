use tokio::sync::Semaphore;

pub(crate) const ADMOB_WEBHOOK: &str = "/api/auth/admob/webhook";
pub(crate) const SWEEGO_WEBHOOK: &str = "/api/auth/sweego/webhook";
pub(crate) const APPLE_WEBHOOK: &str = "/api/billing/apple/webhook";
pub(crate) const GOOGLE_PLAY_WEBHOOK: &str = "/api/billing/google-play/webhook";

#[derive(Clone, Copy)]
pub(super) enum Callback {
    AdMob,
    Sweego,
    Apple,
    GooglePlay,
}

impl Callback {
    pub(super) fn for_route(path: &str) -> Option<Self> {
        match path {
            ADMOB_WEBHOOK => Some(Self::AdMob),
            SWEEGO_WEBHOOK => Some(Self::Sweego),
            APPLE_WEBHOOK => Some(Self::Apple),
            GOOGLE_PLAY_WEBHOOK => Some(Self::GooglePlay),
            _ => None,
        }
    }

    pub(super) fn store_quota(self) -> Option<&'static str> {
        match self {
            Self::Apple => Some("store-webhook-apple"),
            Self::GooglePlay => Some("store-webhook-google-play"),
            Self::AdMob | Self::Sweego => None,
        }
    }
}

pub(super) struct CallbackSlots {
    apple: Semaphore,
    google_play: Semaphore,
    other: Semaphore,
}

impl CallbackSlots {
    pub(super) fn new(per_store: usize, other: usize) -> Self {
        Self {
            apple: Semaphore::new(per_store),
            google_play: Semaphore::new(per_store),
            other: Semaphore::new(other),
        }
    }

    pub(super) fn for_callback(&self, callback: Callback) -> &Semaphore {
        match callback {
            Callback::Apple => &self.apple,
            Callback::GooglePlay => &self.google_play,
            Callback::AdMob | Callback::Sweego => &self.other,
        }
    }
}
