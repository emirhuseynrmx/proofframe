use arrow::array::{
    Array, Date32Array, Date64Array, Decimal128Array, Float32Array, Float64Array, Int8Array,
    Int16Array, Int32Array, Int64Array, LargeStringArray, StringArray, TimestampMicrosecondArray,
    TimestampMillisecondArray, TimestampNanosecondArray, TimestampSecondArray, UInt8Array,
    UInt16Array, UInt32Array, UInt64Array,
};

use super::record_lazy;
use crate::{ColumnPlan, KernelKind, NaNPolicy, ProofFrameError, TypedBound, ValidationState};

pub(crate) fn scan_column(
    plan: &ColumnPlan,
    array: &dyn Array,
    row_offset: u64,
    validation: &mut ValidationState,
) -> Result<(), ProofFrameError> {
    let name = plan.field().name();
    let rules = plan.rules();

    macro_rules! signed {
        ($variant:ident, $array_ty:ty) => {
            if matches!(plan.kernel(), KernelKind::$variant) {
                let values = downcast::<$array_ty>(array);
                let minimum = match rules.min() {
                    Some(TypedBound::I64(value)) => Some(*value),
                    None => None,
                    _ => unreachable!("bound type is fixed by contract compilation"),
                };
                let maximum = match rules.max() {
                    Some(TypedBound::I64(value)) => Some(*value),
                    None => None,
                    _ => unreachable!("bound type is fixed by contract compilation"),
                };
                for row in 0..values.len() {
                    if values.is_null(row) {
                        record_null(rules.not_null(), validation, name, row_offset, row);
                        continue;
                    }
                    let value = values.value(row) as i64;
                    record_range(value, minimum, maximum, validation, name, row_offset, row);
                }
                return Ok(());
            }
        };
    }

    macro_rules! unsigned {
        ($variant:ident, $array_ty:ty) => {
            if matches!(plan.kernel(), KernelKind::$variant) {
                let values = downcast::<$array_ty>(array);
                let minimum = match rules.min() {
                    Some(TypedBound::U64(value)) => Some(*value),
                    None => None,
                    _ => unreachable!("bound type is fixed by contract compilation"),
                };
                let maximum = match rules.max() {
                    Some(TypedBound::U64(value)) => Some(*value),
                    None => None,
                    _ => unreachable!("bound type is fixed by contract compilation"),
                };
                for row in 0..values.len() {
                    if values.is_null(row) {
                        record_null(rules.not_null(), validation, name, row_offset, row);
                        continue;
                    }
                    let value = values.value(row) as u64;
                    record_range(value, minimum, maximum, validation, name, row_offset, row);
                }
                return Ok(());
            }
        };
    }

    signed!(I8, Int8Array);
    signed!(I16, Int16Array);
    signed!(I32, Int32Array);
    signed!(I64, Int64Array);
    signed!(Date32, Date32Array);
    signed!(Date64, Date64Array);
    unsigned!(U8, UInt8Array);
    unsigned!(U16, UInt16Array);
    unsigned!(U32, UInt32Array);
    unsigned!(U64, UInt64Array);

    if matches!(plan.kernel(), KernelKind::F32) {
        let values = downcast::<Float32Array>(array);
        let minimum = float32_bound(rules.min());
        let maximum = float32_bound(rules.max());
        for row in 0..values.len() {
            if values.is_null(row) {
                record_null(rules.not_null(), validation, name, row_offset, row);
                continue;
            }
            let value = values.value(row);
            if value.is_nan() {
                record_nan(rules, validation, name, row_offset, row);
            } else {
                record_range(value, minimum, maximum, validation, name, row_offset, row);
            }
        }
        return Ok(());
    }
    if matches!(plan.kernel(), KernelKind::F64) {
        let values = downcast::<Float64Array>(array);
        let minimum = float64_bound(rules.min());
        let maximum = float64_bound(rules.max());
        for row in 0..values.len() {
            if values.is_null(row) {
                record_null(rules.not_null(), validation, name, row_offset, row);
                continue;
            }
            let value = values.value(row);
            if value.is_nan() {
                record_nan(rules, validation, name, row_offset, row);
            } else {
                record_range(value, minimum, maximum, validation, name, row_offset, row);
            }
        }
        return Ok(());
    }

    macro_rules! timestamp {
        ($array_ty:ty) => {{
            let values = downcast::<$array_ty>(array);
            let minimum = timestamp_bound(rules.min());
            let maximum = timestamp_bound(rules.max());
            for row in 0..values.len() {
                if values.is_null(row) {
                    record_null(rules.not_null(), validation, name, row_offset, row);
                    continue;
                }
                record_range(
                    values.value(row),
                    minimum,
                    maximum,
                    validation,
                    name,
                    row_offset,
                    row,
                );
            }
            return Ok(());
        }};
    }
    if let KernelKind::Timestamp(unit) = plan.kernel() {
        match unit {
            arrow::datatypes::TimeUnit::Second => timestamp!(TimestampSecondArray),
            arrow::datatypes::TimeUnit::Millisecond => timestamp!(TimestampMillisecondArray),
            arrow::datatypes::TimeUnit::Microsecond => timestamp!(TimestampMicrosecondArray),
            arrow::datatypes::TimeUnit::Nanosecond => timestamp!(TimestampNanosecondArray),
        }
    }

    if matches!(plan.kernel(), KernelKind::Decimal128 { .. }) {
        let values = downcast::<Decimal128Array>(array);
        let minimum = decimal_bound(rules.min());
        let maximum = decimal_bound(rules.max());
        for row in 0..values.len() {
            if values.is_null(row) {
                record_null(rules.not_null(), validation, name, row_offset, row);
                continue;
            }
            record_range(
                values.value(row),
                minimum,
                maximum,
                validation,
                name,
                row_offset,
                row,
            );
        }
        return Ok(());
    }

    if matches!(plan.kernel(), KernelKind::Utf8) {
        scan_strings(downcast::<StringArray>(array), plan, row_offset, validation);
        return Ok(());
    }
    if matches!(plan.kernel(), KernelKind::LargeUtf8) {
        scan_strings(
            downcast::<LargeStringArray>(array),
            plan,
            row_offset,
            validation,
        );
        return Ok(());
    }

    scan_nulls(array, rules.not_null(), name, row_offset, validation);
    Ok(())
}

fn scan_strings<O: arrow::array::OffsetSizeTrait>(
    values: &arrow::array::GenericStringArray<O>,
    plan: &ColumnPlan,
    row_offset: u64,
    validation: &mut ValidationState,
) {
    let rules = plan.rules();
    let name = plan.field().name();
    for row in 0..values.len() {
        if values.is_null(row) {
            record_null(rules.not_null(), validation, name, row_offset, row);
            continue;
        }
        let value = values.value(row);
        if rules
            .pattern()
            .is_some_and(|pattern| !pattern.is_match(value))
        {
            record_lazy(
                validation,
                "pattern",
                name,
                Some(row_offset + row as u64),
                || "Value does not match the required pattern".to_string(),
            );
        }
        if rules
            .allowed()
            .is_some_and(|allowed| !allowed.contains(value))
        {
            record_lazy(
                validation,
                "allowed",
                name,
                Some(row_offset + row as u64),
                || "Value is not in the allowlist".to_string(),
            );
        }
    }
}

fn scan_nulls(
    array: &dyn Array,
    not_null: bool,
    name: &str,
    row_offset: u64,
    validation: &mut ValidationState,
) {
    if !not_null || array.null_count() == 0 {
        return;
    }
    for row in 0..array.len() {
        if array.is_null(row) {
            record_null(true, validation, name, row_offset, row);
        }
    }
}

fn record_null(
    not_null: bool,
    validation: &mut ValidationState,
    name: &str,
    row_offset: u64,
    row: usize,
) {
    if not_null {
        record_lazy(
            validation,
            "not_null",
            name,
            Some(row_offset + row as u64),
            || "Null value is not allowed".to_string(),
        );
    }
}

fn record_nan(
    rules: &crate::CompiledRules,
    validation: &mut ValidationState,
    name: &str,
    row_offset: u64,
    row: usize,
) {
    if rules.validates_nan() && rules.nan() == NaNPolicy::Reject {
        record_lazy(
            validation,
            "nan",
            name,
            Some(row_offset + row as u64),
            || "NaN value is not allowed".to_string(),
        );
    }
}

fn record_range<T: PartialOrd + Copy>(
    value: T,
    minimum: Option<T>,
    maximum: Option<T>,
    validation: &mut ValidationState,
    name: &str,
    row_offset: u64,
    row: usize,
) {
    if minimum.is_some_and(|minimum| value < minimum) {
        record_lazy(
            validation,
            "min",
            name,
            Some(row_offset + row as u64),
            || "Value is below the compiled minimum".to_string(),
        );
    }
    if maximum.is_some_and(|maximum| value > maximum) {
        record_lazy(
            validation,
            "max",
            name,
            Some(row_offset + row as u64),
            || "Value is above the compiled maximum".to_string(),
        );
    }
}

fn float32_bound(bound: Option<&TypedBound>) -> Option<f32> {
    match bound {
        Some(TypedBound::F32(value)) => Some(*value),
        None => None,
        _ => unreachable!("bound type is fixed by contract compilation"),
    }
}

fn float64_bound(bound: Option<&TypedBound>) -> Option<f64> {
    match bound {
        Some(TypedBound::F64(value)) => Some(*value),
        None => None,
        _ => unreachable!("bound type is fixed by contract compilation"),
    }
}

fn timestamp_bound(bound: Option<&TypedBound>) -> Option<i64> {
    match bound {
        Some(TypedBound::Timestamp { value, .. }) => Some(*value),
        None => None,
        _ => unreachable!("bound type is fixed by contract compilation"),
    }
}

fn decimal_bound(bound: Option<&TypedBound>) -> Option<i128> {
    match bound {
        Some(TypedBound::Decimal128 { value, .. }) => Some(*value),
        None => None,
        _ => unreachable!("bound type is fixed by contract compilation"),
    }
}

fn downcast<T: 'static>(array: &dyn Array) -> &T {
    array
        .as_any()
        .downcast_ref::<T>()
        .expect("kernel is fixed from the compiled Arrow schema")
}
