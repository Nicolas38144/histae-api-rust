use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use unicode_normalization::{UnicodeNormalization as _, char::is_combining_mark};

use crate::profiles::domain::{AutomatedModerationDecision, ModerationReason, ModerationStatus};

pub const TEXT_MODERATION_POLICY_VERSION: &str = "text_rules_v1";

static WORDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[a-z0-9]+").unwrap_or_else(|_| unreachable!()));
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[a-z0-9._%+-]+\s*(?:@|\bat\b)\s*[a-z0-9.-]+\.[a-z]{2,}\b")
        .unwrap_or_else(|_| unreachable!())
});
static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:https?://|www\.|\b[a-z0-9-]+\.(?:com|fr|net|org|io)\b)")
        .unwrap_or_else(|_| unreachable!())
});
static SOCIAL_HANDLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|\s)@[a-z0-9._-]{3,32}\b").unwrap_or_else(|_| unreachable!())
});
static PHONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\+?\d[\s().-]*){8,}").unwrap_or_else(|_| unreachable!()));
static SPAM_ACTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:contact|dm|ajoute|rejoins|gagne|gratuit|promo)")
        .unwrap_or_else(|_| unreachable!())
});

const INSULTS: &[&str] = &[
    "abruti", "abrutie", "connard", "connasse", "conne", "cretin", "cretine", "debile", "idiot",
    "idiote", "imbecile", "merde", "salope", "pute", "asshole", "bastard", "bitch", "moron",
    "slut", "whore",
];
const SEXUAL_TERMS: &[&str] = &[
    "baise",
    "baiser",
    "bite",
    "chatte",
    "cul",
    "escort",
    "nude",
    "nudes",
    "onlyfans",
    "porn",
    "porno",
    "pornographie",
    "sexe",
    "sexcam",
    "sugarbaby",
];
const SPAM_TERMS: &[&str] = &[
    "bitcoin",
    "casino",
    "crypto",
    "cryptomonnaie",
    "dropshipping",
    "investissement",
    "promotion",
    "promo",
    "telegram",
    "whatsapp",
    "snapchat",
    "onlyfans",
];

#[derive(Clone, Copy, Debug, Default)]
pub struct TextModerator;

impl TextModerator {
    pub fn analyze(self, value: &str) -> AutomatedModerationDecision {
        let normalized = normalize_for_rules(value);
        let words = WORDS
            .find_iter(&normalized)
            .map(|word| word.as_str())
            .collect::<Vec<_>>();
        let word_set = words.iter().copied().collect::<HashSet<_>>();
        let mut reasons = Vec::new();
        if looks_like_spam(&normalized, &words, &word_set) {
            reasons.push(ModerationReason::Spam);
        }
        if contains_any(&word_set, INSULTS) {
            reasons.push(ModerationReason::Insult);
        }
        if contains_personal_contact(&normalized) {
            reasons.push(ModerationReason::PersonalContact);
        }
        if contains_any(&word_set, SEXUAL_TERMS) {
            reasons.push(ModerationReason::SexualContent);
        }
        AutomatedModerationDecision {
            status: if reasons.is_empty() {
                ModerationStatus::Approved
            } else {
                ModerationStatus::Pending
            },
            reasons,
            policy_version: TEXT_MODERATION_POLICY_VERSION,
        }
    }
}

fn normalize_for_rules(value: &str) -> String {
    value
        .nfkd()
        .filter(|value| !is_combining_mark(*value))
        .flat_map(char::to_lowercase)
        .collect()
}

fn contains_any(words: &HashSet<&str>, candidates: &[&str]) -> bool {
    candidates.iter().any(|candidate| words.contains(candidate))
}

fn looks_like_spam(value: &str, words: &[&str], word_set: &HashSet<&str>) -> bool {
    let mut counts = HashMap::new();
    for word in words {
        *counts.entry(*word).or_insert(0_usize) += 1;
    }
    has_repeated_character_run(value, 8)
        || counts.values().any(|count| *count >= 5)
        || (contains_any(word_set, SPAM_TERMS) && SPAM_ACTION.is_match(value))
}

fn has_repeated_character_run(value: &str, minimum: usize) -> bool {
    let mut previous = None;
    let mut count = 0;
    for current in value.chars() {
        if previous == Some(current) {
            count += 1;
        } else {
            previous = Some(current);
            count = 1;
        }
        if count >= minimum {
            return true;
        }
    }
    false
}

fn contains_personal_contact(value: &str) -> bool {
    EMAIL.is_match(value)
        || URL.is_match(value)
        || SOCIAL_HANDLE.is_match(value)
        || PHONE.is_match(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_reason_order_and_nfkd_matching() {
        let result =
            TextModerator.analyze("PROMO crypto contacte moi, crétin: test@example.com sexe");
        assert_eq!(result.status, ModerationStatus::Pending);
        assert_eq!(
            result.reasons,
            vec![
                ModerationReason::Spam,
                ModerationReason::Insult,
                ModerationReason::PersonalContact,
                ModerationReason::SexualContent,
            ]
        );
    }

    #[test]
    fn approves_text_without_a_rule_match() {
        let result = TextModerator.analyze("Curieuse et voyageuse.");
        assert_eq!(result.status, ModerationStatus::Approved);
        assert!(result.reasons.is_empty());
        assert_eq!(result.policy_version, TEXT_MODERATION_POLICY_VERSION);
    }

    #[test]
    fn detects_the_javascript_eight_character_spam_run_without_regex_backreferences() {
        assert_eq!(
            TextModerator.analyze("heyyyyyyyy").reasons,
            vec![ModerationReason::Spam]
        );
    }
}
