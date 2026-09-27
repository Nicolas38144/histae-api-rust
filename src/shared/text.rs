pub fn javascript_trim(value: &str) -> &str {
    value.trim_matches(is_javascript_whitespace)
}

pub fn utf8_len(value: &str) -> usize {
    value.len()
}

/// Reproduces `validator.js`'s `isLength` accounting: JavaScript surrogate
/// pairs count as one scalar and a variation selector attached to a preceding
/// scalar does not add another character.
pub fn validator_js_length(value: &str) -> usize {
    let mut length = 0;
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        length += 1;
        if !is_variation_selector(character)
            && characters
                .peek()
                .is_some_and(|next| is_variation_selector(*next))
        {
            let _ = characters.next();
        }
    }
    length
}

fn is_variation_selector(value: char) -> bool {
    matches!(value, '\u{FE0E}' | '\u{FE0F}')
}

fn is_javascript_whitespace(value: char) -> bool {
    matches!(
        value,
        '\u{0009}'
            ..='\u{000D}'
                | '\u{0020}'
                | '\u{00A0}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_ecmascript_trim_including_the_byte_order_mark() {
        assert_eq!(javascript_trim("\u{feff}\u{00a0}Alice\u{3000}"), "Alice");
        assert_eq!(
            javascript_trim("\u{200b}Alice\u{200b}"),
            "\u{200b}Alice\u{200b}"
        );
    }

    #[test]
    fn counts_utf8_bytes_instead_of_unicode_scalars() {
        assert_eq!(utf8_len("é"), 2);
        assert_eq!(utf8_len("🦀"), 4);
    }

    #[test]
    fn matches_validator_js_length_for_surrogates_and_variation_selectors() {
        assert_eq!(validator_js_length("abc"), 3);
        assert_eq!(validator_js_length("🦀"), 1);
        assert_eq!(validator_js_length("❤\u{fe0f}"), 1);
        assert_eq!(validator_js_length("\u{fe0f}"), 1);
    }
}
