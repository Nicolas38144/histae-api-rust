pub fn javascript_trim(value: &str) -> &str {
    value.trim_matches(is_javascript_whitespace)
}

pub fn utf8_len(value: &str) -> usize {
    value.len()
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
}
