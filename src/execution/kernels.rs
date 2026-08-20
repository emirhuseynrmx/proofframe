use arrow::array::{
    Array, BinaryArray, BinaryViewArray, BooleanArray, Date32Array, Date64Array, Decimal128Array,
    Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array, LargeBinaryArray,
    LargeStringArray, StringArray, StringViewArray, TimestampMicrosecondArray,
    TimestampMillisecondArray, TimestampNanosecondArray, TimestampSecondArray, UInt8Array,
    UInt16Array, UInt32Array, UInt64Array,
};

use super::record_lazy;
use crate::{
    ColumnPlan, ExactState, KernelKind, NaNPolicy, ProofFrameError, TypedBound, ValidationState,
    ValueRef,
};

pub(crate) fn scan_column(
    plan: &ColumnPlan,
    array: &dyn Array,
    row_offset: u64,
    validation: &mut ValidationState,
    mut unique: Option<&mut ExactState>,
) -> Result<(), ProofFrameError> {
    if let Some(result) =
        scan_signed_kernel(plan, array, row_offset, validation, unique.as_deref_mut())
    {
        return result;
    }
    if let Some(result) =
        scan_unsigned_kernel(plan, array, row_offset, validation, unique.as_deref_mut())
    {
        return result;
    }
    if let Some(result) =
        scan_float_kernel(plan, array, row_offset, validation, unique.as_deref_mut())
    {
        return result;
    }
    if let Some(result) =
        scan_timestamp_kernel(plan, array, row_offset, validation, unique.as_deref_mut())
    {
        return result;
    }
    if let Some(result) =
        scan_scalar_kernel(plan, array, row_offset, validation, unique.as_deref_mut())
    {
        return result;
    }
    if let Some(result) =
        scan_text_binary_kernel(plan, array, row_offset, validation, unique.as_deref_mut())
    {
        return result;
    }
    scan_fallback(array, plan, row_offset, validation, unique)
}

fn scan_signed_kernel(
    plan: &ColumnPlan,
    array: &dyn Array,
    row_offset: u64,
    validation: &mut ValidationState,
    unique: Option<&mut ExactState>,
) -> Option<Result<(), ProofFrameError>> {
    let name = plan.field().name();
    let rules = plan.rules();
    let mut unique = unique;

    macro_rules! signed {
        ($variant:ident, $array_ty:ty) => {
            if matches!(plan.kernel(), KernelKind::$variant) {
                return Some((|| {
                    let values = downcast::<$array_ty>(array);
                    let minimum = signed_bound(rules.min());
                    let maximum = signed_bound(rules.max());
                    for row in 0..values.len() {
                        if values.is_null(row) {
                            record_null(rules.not_null(), validation, name, row_offset, row);
                            continue;
                        }
                        let value = values.value(row) as i64;
                        record_range(value, minimum, maximum, validation, name, row_offset, row);
                        insert_unique(&mut unique, ValueRef::I64(value), row_offset, row)?;
                    }
                    Ok(())
                })());
            }
        };
    }

    signed!(I8, Int8Array);
    signed!(I16, Int16Array);
    signed!(I32, Int32Array);
    signed!(I64, Int64Array);
    signed!(Date32, Date32Array);
    signed!(Date64, Date64Array);
    None
}

fn scan_unsigned_kernel(
    plan: &ColumnPlan,
    array: &dyn Array,
    row_offset: u64,
    validation: &mut ValidationState,
    unique: Option<&mut ExactState>,
) -> Option<Result<(), ProofFrameError>> {
    let name = plan.field().name();
    let rules = plan.rules();
    let mut unique = unique;

    macro_rules! unsigned {
        ($variant:ident, $array_ty:ty) => {
            if matches!(plan.kernel(), KernelKind::$variant) {
                return Some((|| {
                    let values = downcast::<$array_ty>(array);
                    let minimum = unsigned_bound(rules.min());
                    let maximum = unsigned_bound(rules.max());
                    for row in 0..values.len() {
                        if values.is_null(row) {
                            record_null(rules.not_null(), validation, name, row_offset, row);
                            continue;
                        }
                        let value = values.value(row) as u64;
                        record_range(value, minimum, maximum, validation, name, row_offset, row);
                        insert_unique(&mut unique, ValueRef::U64(value), row_offset, row)?;
                    }
                    Ok(())
                })());
            }
        };
    }

    unsigned!(U8, UInt8Array);
    unsigned!(U16, UInt16Array);
    unsigned!(U32, UInt32Array);
    unsigned!(U64, UInt64Array);
    None
}

fn scan_float_kernel(
    plan: &ColumnPlan,
    array: &dyn Array,
    row_offset: u64,
    validation: &mut ValidationState,
    unique: Option<&mut ExactState>,
) -> Option<Result<(), ProofFrameError>> {
    let name = plan.field().name();
    let rules = plan.rules();
    let mut unique = unique;

    if matches!(plan.kernel(), KernelKind::F32) {
        return Some((|| {
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
                insert_unique(
                    &mut unique,
                    ValueRef::F64(u64::from(value.to_bits())),
                    row_offset,
                    row,
                )?;
            }
            Ok(())
        })());
    }
    if matches!(plan.kernel(), KernelKind::F64) {
        return Some((|| {
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
                insert_unique(&mut unique, ValueRef::F64(value.to_bits()), row_offset, row)?;
            }
            Ok(())
        })());
    }
    None
}

fn scan_timestamp_kernel(
    plan: &ColumnPlan,
    array: &dyn Array,
    row_offset: u64,
    validation: &mut ValidationState,
    unique: Option<&mut ExactState>,
) -> Option<Result<(), ProofFrameError>> {
    let name = plan.field().name();
    let rules = plan.rules();
    let mut unique = unique;

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
                let value = values.value(row);
                record_range(value, minimum, maximum, validation, name, row_offset, row);
                insert_unique(&mut unique, ValueRef::I64(value), row_offset, row)?;
            }
            Ok(())
        }};
    }
    if let KernelKind::Timestamp(unit) = plan.kernel() {
        return Some((|| match unit {
            arrow::datatypes::TimeUnit::Second => timestamp!(TimestampSecondArray),
            arrow::datatypes::TimeUnit::Millisecond => timestamp!(TimestampMillisecondArray),
            arrow::datatypes::TimeUnit::Microsecond => timestamp!(TimestampMicrosecondArray),
            arrow::datatypes::TimeUnit::Nanosecond => timestamp!(TimestampNanosecondArray),
        })());
    }
    None
}

fn scan_scalar_kernel(
    plan: &ColumnPlan,
    array: &dyn Array,
    row_offset: u64,
    validation: &mut ValidationState,
    unique: Option<&mut ExactState>,
) -> Option<Result<(), ProofFrameError>> {
    let name = plan.field().name();
    let rules = plan.rules();
    let mut unique = unique;

    if matches!(plan.kernel(), KernelKind::Decimal128 { .. }) {
        return Some((|| {
            let values = downcast::<Decimal128Array>(array);
            let minimum = decimal_bound(rules.min());
            let maximum = decimal_bound(rules.max());
            for row in 0..values.len() {
                if values.is_null(row) {
                    record_null(rules.not_null(), validation, name, row_offset, row);
                    continue;
                }
                let value = values.value(row);
                record_range(value, minimum, maximum, validation, name, row_offset, row);
                let encoded = value.to_le_bytes();
                insert_unique(&mut unique, ValueRef::Bytes(&encoded), row_offset, row)?;
            }
            Ok(())
        })());
    }
    if matches!(plan.kernel(), KernelKind::Boolean) {
        return Some((|| {
            let values = downcast::<BooleanArray>(array);
            for row in 0..values.len() {
                if values.is_null(row) {
                    record_null(rules.not_null(), validation, name, row_offset, row);
                    continue;
                }
                insert_unique(
                    &mut unique,
                    ValueRef::U64(u64::from(values.value(row))),
                    row_offset,
                    row,
                )?;
            }
            Ok(())
        })());
    }
    None
}

fn scan_text_binary_kernel(
    plan: &ColumnPlan,
    array: &dyn Array,
    row_offset: u64,
    validation: &mut ValidationState,
    unique: Option<&mut ExactState>,
) -> Option<Result<(), ProofFrameError>> {
    let mut unique = unique;
    if matches!(plan.kernel(), KernelKind::Utf8) {
        return Some(scan_strings(
            downcast::<StringArray>(array),
            plan,
            row_offset,
            validation,
            unique.as_deref_mut(),
        ));
    }
    if matches!(plan.kernel(), KernelKind::LargeUtf8) {
        return Some(scan_strings(
            downcast::<LargeStringArray>(array),
            plan,
            row_offset,
            validation,
            unique.as_deref_mut(),
        ));
    }
    if matches!(plan.kernel(), KernelKind::Utf8View) {
        return Some(scan_string_view(
            downcast::<StringViewArray>(array),
            plan,
            row_offset,
            validation,
            unique.as_deref_mut(),
        ));
    }
    if matches!(plan.kernel(), KernelKind::Binary) {
        return Some(scan_binary(
            downcast::<BinaryArray>(array),
            plan,
            row_offset,
            validation,
            unique.as_deref_mut(),
        ));
    }
    if matches!(plan.kernel(), KernelKind::LargeBinary) {
        return Some(scan_binary(
            downcast::<LargeBinaryArray>(array),
            plan,
            row_offset,
            validation,
            unique.as_deref_mut(),
        ));
    }
    if matches!(plan.kernel(), KernelKind::BinaryView) {
        return Some(scan_binary_view(
            downcast::<BinaryViewArray>(array),
            plan,
            row_offset,
            validation,
            unique,
        ));
    }
    None
}

fn scan_strings<O: arrow::array::OffsetSizeTrait>(
    values: &arrow::array::GenericStringArray<O>,
    plan: &ColumnPlan,
    row_offset: u64,
    validation: &mut ValidationState,
    mut unique: Option<&mut ExactState>,
) -> Result<(), ProofFrameError> {
    let rules = plan.rules();
    let name = plan.field().name();
    for row in 0..values.len() {
        if values.is_null(row) {
            record_null(rules.not_null(), validation, name, row_offset, row);
            continue;
        }
        let value = values.value(row);
        insert_unique(
            &mut unique,
            ValueRef::Bytes(value.as_bytes()),
            row_offset,
            row,
        )?;
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
    Ok(())
}

fn scan_binary<O: arrow::array::OffsetSizeTrait>(
    values: &arrow::array::GenericBinaryArray<O>,
    plan: &ColumnPlan,
    row_offset: u64,
    validation: &mut ValidationState,
    mut unique: Option<&mut ExactState>,
) -> Result<(), ProofFrameError> {
    let rules = plan.rules();
    let name = plan.field().name();
    for row in 0..values.len() {
        if values.is_null(row) {
            record_null(rules.not_null(), validation, name, row_offset, row);
            continue;
        }
        insert_unique(
            &mut unique,
            ValueRef::Bytes(values.value(row)),
            row_offset,
            row,
        )?;
    }
    Ok(())
}

fn scan_string_view(
    values: &StringViewArray,
    plan: &ColumnPlan,
    row_offset: u64,
    validation: &mut ValidationState,
    mut unique: Option<&mut ExactState>,
) -> Result<(), ProofFrameError> {
    let rules = plan.rules();
    let name = plan.field().name();
    for row in 0..values.len() {
        if values.is_null(row) {
            record_null(rules.not_null(), validation, name, row_offset, row);
            continue;
        }
        let value = values.value(row);
        insert_unique(
            &mut unique,
            ValueRef::Bytes(value.as_bytes()),
            row_offset,
            row,
        )?;
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
    Ok(())
}

fn scan_binary_view(
    values: &BinaryViewArray,
    plan: &ColumnPlan,
    row_offset: u64,
    validation: &mut ValidationState,
    mut unique: Option<&mut ExactState>,
) -> Result<(), ProofFrameError> {
    let rules = plan.rules();
    let name = plan.field().name();
    for row in 0..values.len() {
        if values.is_null(row) {
            record_null(rules.not_null(), validation, name, row_offset, row);
            continue;
        }
        insert_unique(
            &mut unique,
            ValueRef::Bytes(values.value(row)),
            row_offset,
            row,
        )?;
    }
    Ok(())
}

fn scan_fallback(
    array: &dyn Array,
    plan: &ColumnPlan,
    row_offset: u64,
    validation: &mut ValidationState,
    mut unique: Option<&mut ExactState>,
) -> Result<(), ProofFrameError> {
    let rules = plan.rules();
    let name = plan.field().name();
    for row in 0..array.len() {
        if array.is_null(row) {
            record_null(rules.not_null(), validation, name, row_offset, row);
        } else if unique.is_some() {
            let encoded = crate::canonical_value_bytes(array, row)?;
            insert_unique(&mut unique, ValueRef::Bytes(&encoded), row_offset, row)?;
        }
    }
    Ok(())
}

fn insert_unique(
    unique: &mut Option<&mut ExactState>,
    value: ValueRef<'_>,
    row_offset: u64,
    row: usize,
) -> Result<(), ProofFrameError> {
    if let Some(state) = unique.as_deref_mut() {
        state.insert(value, row_offset + row as u64)?;
    }
    Ok(())
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

fn signed_bound(bound: Option<&TypedBound>) -> Option<i64> {
    match bound {
        Some(TypedBound::I64(value)) => Some(*value),
        None => None,
        _ => unreachable!("bound type is fixed by contract compilation"),
    }
}

fn unsigned_bound(bound: Option<&TypedBound>) -> Option<u64> {
    match bound {
        Some(TypedBound::U64(value)) => Some(*value),
        None => None,
        _ => unreachable!("bound type is fixed by contract compilation"),
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
