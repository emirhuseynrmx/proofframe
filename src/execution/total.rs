//! Totals for `dataset_rules.sum`.
//!
//! Two things decide how this is written, and both were measured rather than assumed.
//!
//! `arrow::compute::sum` on an `Int64Array` answers `i64::MAX + 1` with
//! `-9223372036854775808`. It wraps, silently, with no error and no `None`. A rule
//! whose whole job is to catch a total that grew too large cannot be built on an
//! addition that quietly makes large totals small, so integers accumulate in `i128`
//! and a total that leaves even that range is an error rather than a number.
//!
//! The same function on a `Float64Array` gives a different answer for the same
//! values depending on how they were split into batches, because it reduces in a
//! tree whose shape follows the batch length. Batch size is a reader setting, not a
//! property of the data, so that would make the verdict depend on how the file
//! happened to be read. Floats are accumulated in row order with Neumaier
//! compensation: one addition per value, the lost low bits carried in a companion
//! term, and the same answer whatever the batching.

use arrow::array::{
    Array, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array, UInt8Array,
    UInt16Array, UInt32Array, UInt64Array,
};

use super::dataset_state::downcast;
use crate::{KernelKind, ProofFrameError, SumBounds};

/// A running total that keeps the precision its column deserves.
#[derive(Debug, Clone, Copy)]
pub(super) enum Total {
    Integer(i128),
    Float(Neumaier),
}

impl Total {
    pub(super) const fn integer() -> Self {
        Self::Integer(0)
    }

    pub(super) const fn float() -> Self {
        Self::Float(Neumaier::new())
    }

    pub(super) fn add_integer(&mut self, value: i128) -> Result<(), ProofFrameError> {
        match self {
            Self::Integer(total) => {
                *total = total.checked_add(value).ok_or_else(|| {
                    ProofFrameError::CorruptData(
                        "Column total left the 128-bit integer range".into(),
                    )
                })?;
                Ok(())
            }
            Self::Float(total) => {
                total.add(value as f64);
                Ok(())
            }
        }
    }

    pub(super) fn add_float(&mut self, value: f64) {
        match self {
            Self::Integer(total) => *total = total.saturating_add(value as i128),
            Self::Float(total) => total.add(value),
        }
    }
}

/// Neumaier summation: Kahan's compensation, corrected for the case where the value
/// being added is larger than the running total.
///
/// Ordinary addition drops the low bits of the smaller operand every time. This
/// keeps them in `compensation` and adds them back once at the end, so a million
/// small values do not quietly erode the total.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Neumaier {
    sum: f64,
    compensation: f64,
}

impl Neumaier {
    pub(super) const fn new() -> Self {
        Self {
            sum: 0.0,
            compensation: 0.0,
        }
    }

    pub(super) fn add(&mut self, value: f64) {
        let total = self.sum + value;
        self.compensation += if self.sum.abs() >= value.abs() {
            (self.sum - total) + value
        } else {
            (value - total) + self.sum
        };
        self.sum = total;
    }

    pub(super) fn value(self) -> f64 {
        self.sum + self.compensation
    }
}

/// The total as text, so a finding can state it without an `f64` round trip.
pub(super) fn describe(total: &Total) -> String {
    match total {
        Total::Integer(value) => value.to_string(),
        Total::Float(value) => value.value().to_string(),
    }
}

/// Add every non-null value of `array` to `total`, in row order.
///
/// Row order is the point: it is a property of the data, so the answer does not
/// change when a reader picks a different batch size.
pub(super) fn accumulate(
    total: &mut Total,
    kernel: &KernelKind,
    array: &dyn Array,
) -> Result<(), ProofFrameError> {
    macro_rules! integers {
        ($variant:pat, $array:ty) => {
            if matches!(kernel, $variant) {
                let values = downcast::<$array>(array);
                for row in 0..values.len() {
                    if !values.is_null(row) {
                        total.add_integer(i128::from(values.value(row)))?;
                    }
                }
                return Ok(());
            }
        };
    }
    integers!(KernelKind::I8, Int8Array);
    integers!(KernelKind::I16, Int16Array);
    integers!(KernelKind::I32, Int32Array);
    integers!(KernelKind::I64, Int64Array);
    integers!(KernelKind::U8, UInt8Array);
    integers!(KernelKind::U16, UInt16Array);
    integers!(KernelKind::U32, UInt32Array);
    integers!(KernelKind::U64, UInt64Array);
    match kernel {
        KernelKind::F32 => {
            let values = downcast::<Float32Array>(array);
            for row in 0..values.len() {
                if !values.is_null(row) {
                    total.add_float(f64::from(values.value(row)));
                }
            }
            Ok(())
        }
        KernelKind::F64 => {
            let values = downcast::<Float64Array>(array);
            for row in 0..values.len() {
                if !values.is_null(row) {
                    total.add_float(values.value(row));
                }
            }
            Ok(())
        }
        _ => Err(ProofFrameError::UnsupportedType(format!(
            "total for {}",
            array.data_type()
        ))),
    }
}

/// Why the total is unacceptable, or `None` when it is inside its bounds.
pub(super) fn outside(total: &Total, bounds: &SumBounds) -> Option<String> {
    match (total, bounds) {
        (&Total::Integer(value), &SumBounds::Integer { min, max }) => match (min, max) {
            (Some(min), _) if value < min => Some(format!("below the minimum {min}")),
            (_, Some(max)) if value > max => Some(format!("above the maximum {max}")),
            _ => None,
        },
        (&Total::Float(value), &SumBounds::Float { min, max }) => {
            let value = value.value();
            match (min, max) {
                (Some(min), _) if value < min => Some(format!("below the minimum {min}")),
                (_, Some(max)) if value > max => Some(format!("above the maximum {max}")),
                _ => None,
            }
        }
        // The compiler pairs the accumulator with the bounds, so this cannot be
        // reached; refusing rather than passing keeps it that way.
        _ => Some("not comparable with its declared bounds".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compensation_keeps_small_values_that_plain_addition_drops() {
        let mut plain = 1.0e16_f64;
        let mut compensated = Neumaier::new();
        compensated.add(1.0e16);
        for _ in 0..1_000 {
            plain += 1.0;
            compensated.add(1.0);
        }
        assert_eq!(plain, 1.0e16, "plain addition loses every one of them");
        assert_eq!(compensated.value(), 1.0e16 + 1_000.0);
    }

    #[test]
    fn an_integer_total_past_i128_is_an_error_rather_than_a_wrapped_number() {
        let mut total = Total::Integer(i128::MAX - 1);
        assert!(total.add_integer(1).is_ok());
        assert!(total.add_integer(1).is_err());
    }
}
