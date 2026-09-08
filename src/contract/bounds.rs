use std::cmp::Ordering;

use arrow::datatypes::{DataType, TimeUnit};
use chrono::{DateTime, NaiveDate};

use super::BoundAst;
use crate::{ErrorCode, ProofFrameError};

/// Exact semantic value used by compiled range checks.
#[derive(Debug, Clone, PartialEq)]
pub enum TypedBound {
    I64(i64),
    U64(u64),
    F32(f32),
    F64(f64),
    Decimal128 { value: i128, scale: i8 },
    Timestamp { value: i64, unit: TimeUnit },
}

impl TypedBound {
    pub(crate) fn compare(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Self::I64(left), Self::I64(right)) => Some(left.cmp(right)),
            (Self::U64(left), Self::U64(right)) => Some(left.cmp(right)),
            (Self::F32(left), Self::F32(right)) => left.partial_cmp(right),
            (Self::F64(left), Self::F64(right)) => left.partial_cmp(right),
            (
                Self::Decimal128 {
                    value: left,
                    scale: left_scale,
                },
                Self::Decimal128 {
                    value: right,
                    scale: right_scale,
                },
            ) if left_scale == right_scale => Some(left.cmp(right)),
            (
                Self::Timestamp {
                    value: left,
                    unit: left_unit,
                },
                Self::Timestamp {
                    value: right,
                    unit: right_unit,
                },
            ) if left_unit == right_unit => Some(left.cmp(right)),
            _ => None,
        }
    }
}

pub(crate) fn parse_bound(
    bound: &BoundAst,
    data_type: &DataType,
    path: &str,
) -> Result<TypedBound, ProofFrameError> {
    let text = bound.as_text();
    let invalid = |reason: &str| {
        ProofFrameError::contract(
            ErrorCode::ContractInvalidBound,
            format!("Invalid bound `{text}` at {path}: {reason}"),
            Some(path.to_string()),
        )
    };

    match data_type {
        DataType::Int8 => text
            .parse::<i8>()
            .map(|value| TypedBound::I64(i64::from(value)))
            .map_err(|_| invalid("expected an integer in the Int8 domain")),
        DataType::Int16 => text
            .parse::<i16>()
            .map(|value| TypedBound::I64(i64::from(value)))
            .map_err(|_| invalid("expected an integer in the Int16 domain")),
        DataType::Int32 => text
            .parse::<i32>()
            .map(|value| TypedBound::I64(i64::from(value)))
            .map_err(|_| invalid("expected an integer in the Int32 domain")),
        DataType::Int64 => text
            .parse::<i64>()
            .map(TypedBound::I64)
            .map_err(|_| invalid("expected an integer in the Int64 domain")),
        DataType::UInt8 => text
            .parse::<u8>()
            .map(|value| TypedBound::U64(u64::from(value)))
            .map_err(|_| invalid("expected an integer in the UInt8 domain")),
        DataType::UInt16 => text
            .parse::<u16>()
            .map(|value| TypedBound::U64(u64::from(value)))
            .map_err(|_| invalid("expected an integer in the UInt16 domain")),
        DataType::UInt32 => text
            .parse::<u32>()
            .map(|value| TypedBound::U64(u64::from(value)))
            .map_err(|_| invalid("expected an integer in the UInt32 domain")),
        DataType::UInt64 => text
            .parse::<u64>()
            .map(TypedBound::U64)
            .map_err(|_| invalid("expected an integer in the UInt64 domain")),
        // A date bound is written the way a person writes a date. Day counts still
        // parse, so contracts that spelled `19723` keep working.
        DataType::Date32 => text
            .parse::<i32>()
            .map(i64::from)
            .ok()
            .or_else(|| iso_days(text))
            .map(TypedBound::I64)
            .ok_or_else(|| invalid("expected `YYYY-MM-DD` or a day count in the Date32 domain")),
        DataType::Date64 => text
            .parse::<i64>()
            .ok()
            .or_else(|| iso_days(text).map(|days| days * MILLISECONDS_PER_DAY))
            .map(TypedBound::I64)
            .ok_or_else(|| {
                invalid("expected `YYYY-MM-DD` or a millisecond count in the Date64 domain")
            }),
        DataType::Float32 => {
            let value = text
                .parse::<f32>()
                .map_err(|_| invalid("expected a finite 32-bit float"))?;
            if value.is_finite() {
                Ok(TypedBound::F32(value))
            } else {
                Err(invalid("floating-point bounds must be finite"))
            }
        }
        DataType::Float64 => {
            let value = text
                .parse::<f64>()
                .map_err(|_| invalid("expected a finite 64-bit float"))?;
            if value.is_finite() {
                Ok(TypedBound::F64(value))
            } else {
                Err(invalid("floating-point bounds must be finite"))
            }
        }
        DataType::Decimal128(precision, scale) => parse_decimal(text, *precision, *scale, path)
            .map(|value| TypedBound::Decimal128 {
                value,
                scale: *scale,
            }),
        DataType::Timestamp(unit, _) => parse_timestamp(text, *unit, path),
        _ => Err(ProofFrameError::contract(
            ErrorCode::ContractTypeMismatch,
            format!("Bounds are not supported for Arrow type `{data_type}`"),
            Some(path.to_string()),
        )),
    }
}

fn parse_timestamp(
    source: &str,
    unit: TimeUnit,
    path: &str,
) -> Result<TypedBound, ProofFrameError> {
    if let Ok(value) = source.parse::<i64>() {
        return Ok(TypedBound::Timestamp { value, unit });
    }
    let invalid = |reason: &str| {
        ProofFrameError::contract(
            ErrorCode::ContractInvalidBound,
            format!("Invalid timestamp bound `{source}` at {path}: {reason}"),
            Some(path.to_string()),
        )
    };
    let timestamp = DateTime::parse_from_rfc3339(source).map_err(|_| {
        invalid("expected integer ticks or an ISO-8601 timestamp with an explicit UTC offset")
    })?;
    let seconds = timestamp.timestamp();
    let nanoseconds = i64::from(timestamp.timestamp_subsec_nanos());
    let (scale, precision) = match unit {
        TimeUnit::Second => (1_i64, 1_000_000_000_i64),
        TimeUnit::Millisecond => (1_000, 1_000_000),
        TimeUnit::Microsecond => (1_000_000, 1_000),
        TimeUnit::Nanosecond => (1_000_000_000, 1),
    };
    if nanoseconds % precision != 0 {
        return Err(invalid(
            "fractional precision exceeds the Arrow timestamp unit; rounding is not allowed",
        ));
    }
    let value = seconds
        .checked_mul(scale)
        .and_then(|whole| whole.checked_add(nanoseconds / precision))
        .ok_or_else(|| invalid("timestamp exceeds the signed Arrow tick range"))?;
    Ok(TypedBound::Timestamp { value, unit })
}

fn parse_decimal(
    source: &str,
    precision: u8,
    scale: i8,
    path: &str,
) -> Result<i128, ProofFrameError> {
    let invalid = |reason: &str| {
        ProofFrameError::contract(
            ErrorCode::ContractInvalidBound,
            format!("Invalid decimal bound `{source}` at {path}: {reason}"),
            Some(path.to_string()),
        )
    };
    if source.is_empty() || source.trim() != source || source.contains(['e', 'E']) {
        return Err(invalid("expected a plain base-10 decimal"));
    }

    let (negative, unsigned) = match source.as_bytes().first() {
        Some(b'-') => (true, &source[1..]),
        Some(b'+') => (false, &source[1..]),
        _ => (false, source),
    };
    if unsigned.is_empty() {
        return Err(invalid("missing decimal digits"));
    }
    let mut pieces = unsigned.split('.');
    let integer = pieces.next().expect("split always has one item");
    let fraction = pieces.next().unwrap_or("");
    if pieces.next().is_some()
        || integer.is_empty()
        || !integer.bytes().all(|value| value.is_ascii_digit())
        || !fraction.bytes().all(|value| value.is_ascii_digit())
    {
        return Err(invalid("expected digits with at most one decimal point"));
    }

    let magnitude = if scale >= 0 {
        let scale = scale as usize;
        if fraction.len() > scale
            && fraction.as_bytes()[scale..]
                .iter()
                .any(|digit| *digit != b'0')
        {
            return Err(invalid("fractional digits exceed the Arrow scale"));
        }
        let retained = &fraction[..fraction.len().min(scale)];
        let mut digits = String::with_capacity(integer.len() + scale);
        digits.push_str(integer);
        digits.push_str(retained);
        digits.extend(std::iter::repeat_n('0', scale - retained.len()));
        digits
            .parse::<i128>()
            .map_err(|_| invalid("decimal magnitude exceeds i128"))?
    } else {
        if !fraction.is_empty() && fraction.bytes().any(|digit| digit != b'0') {
            return Err(invalid(
                "negative-scale decimals cannot contain a fractional value",
            ));
        }
        let integer_value = integer
            .parse::<i128>()
            .map_err(|_| invalid("decimal magnitude exceeds i128"))?;
        let divisor = 10_i128
            .checked_pow((-scale) as u32)
            .ok_or_else(|| invalid("decimal scale is too large"))?;
        if integer_value % divisor != 0 {
            return Err(invalid("value is not aligned to the negative Arrow scale"));
        }
        integer_value / divisor
    };

    let signed = if negative {
        magnitude
            .checked_neg()
            .ok_or_else(|| invalid("decimal magnitude exceeds i128"))?
    } else {
        magnitude
    };
    if decimal_digits(signed) > usize::from(precision) {
        return Err(invalid("value exceeds the Arrow decimal precision"));
    }
    Ok(signed)
}

fn decimal_digits(value: i128) -> usize {
    if value == 0 {
        1
    } else {
        value.unsigned_abs().ilog10() as usize + 1
    }
}

const MILLISECONDS_PER_DAY: i64 = 86_400_000;

/// Days since the Unix epoch for a `YYYY-MM-DD` bound.
///
/// Only the complete calendar form is accepted: `2024-1-1` and `2024-01` are the
/// spellings where a reader and a parser start to disagree about what was meant.
fn iso_days(text: &str) -> Option<i64> {
    if text.len() != 10 {
        return None;
    }
    let date = NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?;
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1)?;
    Some(date.signed_duration_since(epoch).num_days())
}
