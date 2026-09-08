//! Ordering state for `dataset_rules.monotonicity`.
//!
//! One scalar of state per rule, compared against the value before it. That is the
//! whole check: it costs nothing per row beyond the comparison, and it says nothing
//! about rows it did not see in that order.

use arrow::array::{
    Array, BooleanArray, Date32Array, Date64Array, Decimal128Array, Float32Array, Float64Array,
    Int8Array, Int16Array, Int32Array, Int64Array, LargeStringArray, StringArray, StringViewArray,
    TimestampMicrosecondArray, TimestampMillisecondArray, TimestampNanosecondArray,
    TimestampSecondArray, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow::datatypes::TimeUnit;

use crate::{KernelKind, MonotonicDirectionAst, ProofFrameError};

use super::dataset_state::downcast;

/// A value compared by its own order rather than by its encoding.
///
/// The canonical byte form used elsewhere is built for equality and hashing, where a
/// negative integer and a float sort by their bit patterns. That is the wrong order
/// here, so ordering rules carry the value itself.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Ordered {
    Int(i128),
    Uint(u64),
    Float(f64),
    Text(Vec<u8>),
}

impl Ordered {
    /// `None` when two values are not comparable, which the compiler prevents for a
    /// single column but which must not become a silent pass if it ever happens.
    fn compare(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Self::Int(left), Self::Int(right)) => Some(left.cmp(right)),
            (Self::Uint(left), Self::Uint(right)) => Some(left.cmp(right)),
            (Self::Float(left), Self::Float(right)) => Some(left.total_cmp(right)),
            (Self::Text(left), Self::Text(right)) => Some(left.cmp(right)),
            _ => None,
        }
    }
}

/// Read one value in the order the rule compares it, or `None` for an unsupported type.
pub(super) fn ordered_value(
    kernel: &KernelKind,
    array: &dyn Array,
    row: usize,
) -> Result<Ordered, ProofFrameError> {
    macro_rules! int {
        ($variant:pat, $array:ty) => {
            if matches!(kernel, $variant) {
                return Ok(Ordered::Int(i128::from(
                    downcast::<$array>(array).value(row),
                )));
            }
        };
    }
    macro_rules! uint {
        ($variant:pat, $array:ty) => {
            if matches!(kernel, $variant) {
                return Ok(Ordered::Uint(u64::from(
                    downcast::<$array>(array).value(row),
                )));
            }
        };
    }
    int!(KernelKind::I8, Int8Array);
    int!(KernelKind::I16, Int16Array);
    int!(KernelKind::I32, Int32Array);
    int!(KernelKind::I64, Int64Array);
    int!(KernelKind::Date32, Date32Array);
    int!(KernelKind::Date64, Date64Array);
    int!(
        KernelKind::Timestamp(TimeUnit::Second),
        TimestampSecondArray
    );
    int!(
        KernelKind::Timestamp(TimeUnit::Millisecond),
        TimestampMillisecondArray
    );
    int!(
        KernelKind::Timestamp(TimeUnit::Microsecond),
        TimestampMicrosecondArray
    );
    int!(
        KernelKind::Timestamp(TimeUnit::Nanosecond),
        TimestampNanosecondArray
    );
    uint!(KernelKind::U8, UInt8Array);
    uint!(KernelKind::U16, UInt16Array);
    uint!(KernelKind::U32, UInt32Array);
    match kernel {
        KernelKind::U64 => Ok(Ordered::Uint(downcast::<UInt64Array>(array).value(row))),
        KernelKind::Boolean => Ok(Ordered::Uint(u64::from(
            downcast::<BooleanArray>(array).value(row),
        ))),
        KernelKind::F32 => Ok(Ordered::Float(f64::from(
            downcast::<Float32Array>(array).value(row),
        ))),
        KernelKind::F64 => Ok(Ordered::Float(downcast::<Float64Array>(array).value(row))),
        // The scale is fixed for the column, so the raw integer orders the values.
        KernelKind::Decimal128 { .. } => {
            Ok(Ordered::Int(downcast::<Decimal128Array>(array).value(row)))
        }
        KernelKind::Utf8 => Ok(Ordered::Text(
            downcast::<StringArray>(array).value(row).into(),
        )),
        KernelKind::LargeUtf8 => Ok(Ordered::Text(
            downcast::<LargeStringArray>(array).value(row).into(),
        )),
        KernelKind::Utf8View => Ok(Ordered::Text(
            downcast::<StringViewArray>(array).value(row).into(),
        )),
        _ => Err(ProofFrameError::UnsupportedType(format!(
            "monotonicity for {}",
            array.data_type()
        ))),
    }
}

/// Whether `next` may follow `previous` under `direction`.
pub(super) fn follows(
    direction: MonotonicDirectionAst,
    previous: &Ordered,
    next: &Ordered,
) -> Result<bool, ProofFrameError> {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let ordering = previous.compare(next).ok_or_else(|| {
        ProofFrameError::CorruptData("Monotonicity compared two different value kinds".into())
    })?;
    Ok(match direction {
        MonotonicDirectionAst::Increasing => matches!(ordering, Less | Equal),
        MonotonicDirectionAst::StrictlyIncreasing => ordering == Less,
        MonotonicDirectionAst::Decreasing => matches!(ordering, Greater | Equal),
        MonotonicDirectionAst::StrictlyDecreasing => ordering == Greater,
    })
}

/// The sentence a reader has to act on, naming the direction that was broken.
pub(super) fn describe(direction: MonotonicDirectionAst) -> &'static str {
    match direction {
        MonotonicDirectionAst::Increasing => "must not decrease",
        MonotonicDirectionAst::StrictlyIncreasing => "must increase",
        MonotonicDirectionAst::Decreasing => "must not increase",
        MonotonicDirectionAst::StrictlyDecreasing => "must decrease",
    }
}

/// The distance between two values, in the column's own units.
///
/// `None` for a kind that has no distance, which the compiler already refuses; a
/// step rule must never answer "no gap" because it could not subtract.
pub(super) fn distance(previous: &Ordered, next: &Ordered) -> Option<f64> {
    match (previous, next) {
        (Ordered::Int(left), Ordered::Int(right)) => Some((right - left) as f64),
        (Ordered::Uint(left), Ordered::Uint(right)) => Some(*right as f64 - *left as f64),
        (Ordered::Float(left), Ordered::Float(right)) => Some(right - left),
        _ => None,
    }
}
