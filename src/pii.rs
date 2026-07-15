use regex::Regex;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Match {
    pub(crate) kind: &'static str,
    pub(crate) confidence: &'static str,
}

pub(crate) struct Detector {
    email: Regex,
    ipv4: Regex,
    phone: Regex,
    iban: Regex,
    card_candidate: Regex,
}

impl Detector {
    pub(crate) fn new() -> Result<Self, regex::Error> {
        Ok(Self {
            email: Regex::new(
                r"(?i)^[a-z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)+$",
            )?,
            ipv4: Regex::new(r"^(?:\d{1,3}\.){3}\d{1,3}$")?,
            phone: Regex::new(r"^\+?[0-9][0-9 ()-]{7,19}$")?,
            iban: Regex::new(r"(?i)^[A-Z]{2}\d{2}[A-Z0-9]{11,30}$")?,
            card_candidate: Regex::new(r"^[0-9 -]{13,23}$")?,
        })
    }

    pub(crate) fn classify(&self, value: &str) -> Option<Match> {
        let trimmed = value.trim();
        if self.email.is_match(trimmed) {
            return Some(Match {
                kind: "email",
                confidence: "high",
            });
        }
        if self.ipv4.is_match(trimmed) && valid_ipv4(trimmed) {
            return Some(Match {
                kind: "ipv4",
                confidence: "high",
            });
        }
        let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
        if self.iban.is_match(&compact) && valid_iban(&compact) {
            return Some(Match {
                kind: "iban",
                confidence: "high",
            });
        }
        if self.card_candidate.is_match(trimmed) {
            let digits: String = trimmed.chars().filter(char::is_ascii_digit).collect();
            if (13..=19).contains(&digits.len()) && luhn_valid(&digits) {
                return Some(Match {
                    kind: "payment_card",
                    confidence: "high",
                });
            }
        }
        if self.phone.is_match(trimmed) {
            let digit_count = trimmed.chars().filter(char::is_ascii_digit).count();
            if (8..=15).contains(&digit_count) {
                return Some(Match {
                    kind: "phone",
                    confidence: "medium",
                });
            }
        }
        None
    }
}

fn valid_ipv4(value: &str) -> bool {
    value.split('.').count() == 4
        && value
            .split('.')
            .all(|part| !part.is_empty() && part.parse::<u8>().is_ok())
}

fn valid_iban(value: &str) -> bool {
    if !(15..=34).contains(&value.len()) || !value.is_ascii() {
        return false;
    }
    let rearranged = format!("{}{}", &value[4..], &value[..4]);
    let mut remainder = 0_u32;
    for byte in rearranged.bytes() {
        if byte.is_ascii_digit() {
            remainder = (remainder * 10 + u32::from(byte - b'0')) % 97;
        } else if byte.is_ascii_alphabetic() {
            let number = u32::from(byte.to_ascii_uppercase() - b'A') + 10;
            remainder = (remainder * 100 + number) % 97;
        } else {
            return false;
        }
    }
    remainder == 1
}

pub(crate) fn luhn_valid(value: &str) -> bool {
    let mut sum = 0_u32;
    let mut double = false;
    for byte in value.bytes().rev() {
        if !byte.is_ascii_digit() {
            return false;
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
    !value.is_empty() && sum % 10 == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn identifies_high_confidence_examples() {
        let detector = Detector::new().unwrap();
        assert_eq!(
            detector.classify("person@example.com").unwrap().kind,
            "email"
        );
        assert_eq!(
            detector.classify("4111 1111 1111 1111").unwrap().kind,
            "payment_card"
        );
        assert_eq!(
            detector.classify("GB82WEST12345698765432").unwrap().kind,
            "iban"
        );
    }

    proptest! {
        #[test]
        fn random_non_digit_strings_never_pass_luhn(value in "[A-Za-z]{1,40}") {
            prop_assert!(!luhn_valid(&value));
        }
    }
}
