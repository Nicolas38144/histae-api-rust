//! Identity validation shared by mobile profile creation and audited corrections.
use chrono::{Datelike as _, NaiveDate};

use crate::shared::text::{javascript_trim, utf8_len};

pub(crate) fn normalize_firstname(value: &str) -> Option<String> {
    let value = javascript_trim(value);
    (!value.is_empty() && utf8_len(value) <= 100).then(|| value.to_owned())
}

pub(crate) fn strict_birthdate(value: &str) -> Option<NaiveDate> {
    if value.len() != 10
        || value.as_bytes().get(4) != Some(&b'-')
        || value.as_bytes().get(7) != Some(&b'-')
        || value
            .bytes()
            .enumerate()
            .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
    {
        return None;
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

pub(crate) fn is_adult(birthdate: NaiveDate, today: NaiveDate) -> bool {
    let threshold_year = today.year() - 18;
    birthdate.year() < threshold_year
        || (birthdate.year() == threshold_year
            && (birthdate.month(), birthdate.day()) <= (today.month(), today.day()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_calendar_dates_and_the_utc_eighteenth_birthday() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 21).expect("valid date");
        assert!(is_adult(
            strict_birthdate("2008-09-21").expect("valid date"),
            today
        ));
        assert!(!is_adult(
            strict_birthdate("2008-09-22").expect("valid date"),
            today
        ));
        for invalid in [
            "2000-02-30",
            "2001-13-01",
            "2001-00-10",
            "2001-01-00",
            "2001-1-01",
            "2001-01-01T00:00:00Z",
        ] {
            assert_eq!(strict_birthdate(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn firstnames_keep_the_same_trim_and_byte_limit_for_both_entry_points() {
        assert_eq!(
            normalize_firstname("  Alice\u{feff}"),
            Some("Alice".to_owned())
        );
        assert_eq!(normalize_firstname("  "), None);
        assert_eq!(normalize_firstname(&"é".repeat(51)), None);
        assert_eq!(normalize_firstname(&"é".repeat(50)), Some("é".repeat(50)));
    }
}
