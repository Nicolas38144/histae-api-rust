use serde::{Deserialize, Deserializer, de::Error as _};
use uuid::{Uuid, Variant};

/// The textual shape accepted by validator.js `isUUID(value, 'all')`.
pub fn uuid_all(value: Uuid) -> bool {
    value.is_nil()
        || value == Uuid::max()
        || ((1..=8).contains(&value.get_version_num()) && value.get_variant() == Variant::RFC4122)
}

pub fn canonical_uuid(value: &str) -> Option<Uuid> {
    let parsed = Uuid::parse_str(value).ok()?;
    (parsed.hyphenated().to_string().eq_ignore_ascii_case(value) && uuid_all(parsed))
        .then_some(parsed)
}

pub fn deserialize_uuid<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Uuid, D::Error> {
    canonical_uuid(&String::deserialize(deserializer)?)
        .ok_or_else(|| D::Error::custom("invalid UUID"))
}

pub fn deserialize_optional_uuid<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Uuid>, D::Error> {
    Option::<String>::deserialize(deserializer)?
        .map(|value| canonical_uuid(&value).ok_or_else(|| D::Error::custom("invalid UUID")))
        .transpose()
}

/// String branch of ECMAScript Number(), used by class-transformer DTOs.
pub fn javascript_number(value: &str) -> Option<f64> {
    let value = super::text::javascript_trim(value);
    if value.is_empty() {
        return Some(0.0);
    }
    let radix = if value.starts_with("0x") || value.starts_with("0X") {
        Some(16)
    } else if value.starts_with("0b") || value.starts_with("0B") {
        Some(2)
    } else if value.starts_with("0o") || value.starts_with("0O") {
        Some(8)
    } else {
        None
    };
    if let Some(radix) = radix {
        let digits = value.get(2..)?;
        if digits.is_empty() {
            return None;
        }
        return digits.chars().try_fold(0.0, |number, digit| {
            Some(number * f64::from(radix) + f64::from(digit.to_digit(radix)?))
        });
    }
    value.parse().ok()
}

pub fn deserialize_query_u32<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let value = String::deserialize(deserializer)?;
    let number = javascript_number(&value).ok_or_else(|| D::Error::custom("invalid number"))?;
    if !number.is_finite()
        || number.fract() != 0.0
        || !(0.0..=f64::from(u32::MAX)).contains(&number)
    {
        return Err(D::Error::custom("invalid integer"));
    }
    Ok(number as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uuid_shape_is_canonical_and_preserves_nil_and_max() {
        for id in [Uuid::new_v4(), Uuid::nil(), Uuid::max()] {
            assert_eq!(canonical_uuid(&id.to_string()), Some(id));
            assert_eq!(canonical_uuid(&id.to_string().to_uppercase()), Some(id));
            assert_eq!(canonical_uuid(&id.simple().to_string()), None);
            assert_eq!(canonical_uuid(&id.urn().to_string()), None);
        }
    }
    #[test]
    fn numeric_strings_match_class_transformer() {
        for value in [
            "20",
            "0x14",
            "0X14",
            "0b10100",
            "0o24",
            "2e1",
            "20.0",
            "\u{feff}20",
        ] {
            assert_eq!(javascript_number(value), Some(20.0), "{value}");
        }
        assert_eq!(javascript_number(""), Some(0.0));
        for value in ["-0x14", "+0x14", "0x", "20x"] {
            assert_eq!(javascript_number(value), None);
        }
    }
}
