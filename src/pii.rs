use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

static DETECTOR: LazyLock<Detector> = LazyLock::new(|| {
    Detector::new().expect("ProofFrame's built-in PII regular expressions must compile")
});

pub(crate) fn detector() -> &'static Detector {
    &DETECTOR
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Match {
    pub(crate) kind: &'static str,
    pub(crate) confidence: &'static str,
}

pub(crate) struct Detector {
    email: Regex,
    ipv4: Regex,
    phone: Regex,
}

impl Detector {
    pub(crate) fn new() -> Result<Self, regex::Error> {
        Ok(Self {
            email: Regex::new(
                r"(?i)^[a-z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)+$",
            )?,
            ipv4: Regex::new(r"^(?:\d{1,3}\.){3}\d{1,3}$")?,
            phone: Regex::new(r"^\+?[0-9][0-9 ()-]{7,19}$")?,
        })
    }

    pub(crate) fn classify_cell(&self, value: &str, numeric_column: bool) -> Option<Match> {
        let trimmed = value.trim();
        if trimmed.as_bytes().contains(&b'@') && self.email.is_match(trimmed) {
            return Some(Match {
                kind: "email",
                confidence: "high",
            });
        }
        if trimmed.as_bytes().contains(&b'.')
            && trimmed
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
            && self.ipv4.is_match(trimmed)
            && valid_ipv4(trimmed)
        {
            return Some(Match {
                kind: "ipv4",
                confidence: "high",
            });
        }
        if valid_iban(trimmed) {
            return Some(Match {
                kind: "iban",
                confidence: "high",
            });
        }
        if luhn_filtered(trimmed) {
            return Some(Match {
                kind: "payment_card",
                confidence: digit_only_confidence(trimmed, numeric_column, "high"),
            });
        }
        if phone_candidate(trimmed) && self.phone.is_match(trimmed) {
            let digit_count = trimmed.chars().filter(char::is_ascii_digit).count();
            if (8..=15).contains(&digit_count) {
                return Some(Match {
                    kind: "phone",
                    confidence: digit_only_confidence(trimmed, numeric_column, "medium"),
                });
            }
        }
        None
    }
}

#[inline]
fn phone_candidate(value: &str) -> bool {
    (8..=20).contains(&value.len())
        && matches!(value.as_bytes().first(), Some(b'+' | b'0'..=b'9'))
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b' ' | b'(' | b')' | b'-'))
}

fn digit_only_confidence(
    value: &str,
    numeric_column: bool,
    default_confidence: &'static str,
) -> &'static str {
    let digit_only = value.chars().all(|character| character.is_ascii_digit());
    if numeric_column && digit_only {
        "low"
    } else {
        default_confidence
    }
}

fn valid_ipv4(value: &str) -> bool {
    value.split('.').count() == 4
        && value
            .split('.')
            .all(|part| !part.is_empty() && part.parse::<u8>().is_ok())
}

fn valid_iban(value: &str) -> bool {
    if !value.is_ascii() {
        return false;
    }

    let mut normalized_len = 0_usize;
    for byte in value.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        let valid = match normalized_len {
            0..=1 => byte.is_ascii_alphabetic(),
            2..=3 => byte.is_ascii_digit(),
            _ => byte.is_ascii_alphanumeric(),
        };
        if !valid {
            return false;
        }
        normalized_len += 1;
    }
    if !(15..=34).contains(&normalized_len) {
        return false;
    }

    let mut remainder = 0_u32;
    for byte in value
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .skip(4)
        .chain(
            value
                .bytes()
                .filter(|byte| !byte.is_ascii_whitespace())
                .take(4),
        )
    {
        remainder = iban_remainder(remainder, byte);
    }
    remainder == 1
}

#[inline]
fn iban_remainder(remainder: u32, byte: u8) -> u32 {
    if byte.is_ascii_digit() {
        (remainder * 10 + u32::from(byte - b'0')) % 97
    } else {
        let number = u32::from(byte.to_ascii_uppercase() - b'A') + 10;
        (remainder * 100 + number) % 97
    }
}

fn luhn_filtered(value: &str) -> bool {
    if !(13..=23).contains(&value.len()) || !value.is_ascii() {
        return false;
    }
    let mut digit_count = 0_usize;
    for byte in value.bytes() {
        match byte {
            b'0'..=b'9' => digit_count += 1,
            b' ' | b'-' => {}
            _ => return false,
        }
    }
    if !(13..=19).contains(&digit_count) {
        return false;
    }

    let mut sum = 0_u32;
    let mut double = false;
    for byte in value.bytes().rev() {
        if !byte.is_ascii_digit() {
            continue;
        }
        let mut digit = u32::from(byte - b'0');
        if double {
            digit *= 2;
            if digit > 9 {
                digit -= 9;
            }
        }
        sum += digit;
        double = !double;
    }
    sum % 10 == 0
}

#[cfg(test)]
fn luhn_valid(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) && luhn_filtered(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn identifies_high_confidence_examples() {
        let detector = Detector::new().unwrap();
        assert_eq!(
            detector
                .classify_cell("person@example.com", false)
                .unwrap()
                .kind,
            "email"
        );
        assert_eq!(
            detector
                .classify_cell("4111 1111 1111 1111", false)
                .unwrap()
                .kind,
            "payment_card"
        );
        assert_eq!(
            detector
                .classify_cell("GB82WEST12345698765432", false)
                .unwrap()
                .kind,
            "iban"
        );
    }

    #[test]
    fn formatted_iban_and_card_are_checked_without_compaction() {
        let detector = Detector::new().unwrap();
        assert_eq!(
            detector
                .classify_cell("GB82 WEST 1234 5698 7654 32", false)
                .unwrap()
                .kind,
            "iban"
        );
        assert!(luhn_filtered("4111-1111-1111-1111"));
        assert!(!luhn_filtered("4111-1111-1111-1112"));
    }

    proptest! {
        #[test]
        fn random_non_digit_strings_never_pass_luhn(value in "[A-Za-z]{1,40}") {
            prop_assert!(!luhn_valid(&value));
        }
    }
}
