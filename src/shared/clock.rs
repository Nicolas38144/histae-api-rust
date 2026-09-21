use chrono::{DateTime, NaiveDate, Utc};

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;

    fn today_utc(&self) -> NaiveDate {
        self.now().date_naive()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
