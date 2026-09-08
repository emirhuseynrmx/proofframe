use arrow::array::{
    Array, BooleanArray, Date32Array, Date64Array, Decimal128Array, Float32Array, Float64Array,
    Int8Array, Int16Array, Int32Array, Int64Array, LargeStringArray, StringArray, StringViewArray,
    TimestampMicrosecondArray, TimestampMillisecondArray, TimestampNanosecondArray,
    TimestampSecondArray, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow::record_batch::RecordBatch;

use super::record_lazy;
use crate::{
    CompareOpAst, ComparePlan, CompiledRules, KernelKind, NaNPolicy, NullPolicyAst, OperandPlan,
    ProofFrameError, RowPlan, RowPlanKind, ScalarValuePlan, TypedBound, ValidationState,
};

pub(super) fn scan_row_plan(
    plan: &RowPlan,
    batch: &RecordBatch,
    row_offset: u64,
    validation: &mut ValidationState,
) -> Result<(), ProofFrameError> {
    match plan.kind() {
        RowPlanKind::Compare(compare) => {
            scan_predicate(compare, batch, |row, outcome| {
                if matches!(outcome, Some(false)) {
                    record_lazy(
                        validation,
                        "compare",
                        finding_column(compare),
                        Some(row_offset + row as u64),
                        || format!("Relational rule `{}` failed", plan.name()),
                    );
                }
            })?;
        }
        RowPlanKind::Conditional {
            predicate,
            assertion_column,
            assertion_field,
            assertion,
        } => {
            let target = batch.column(*assertion_column);
            let kernel = KernelKind::from_data_type_for_plan(assertion_field.data_type());
            let mut assertion_error = None;
            scan_predicate(predicate, batch, |row, outcome| {
                if assertion_error.is_none() && matches!(outcome, Some(true)) {
                    assertion_error = scan_assertion_row(
                        &kernel,
                        assertion,
                        target.as_ref(),
                        assertion_field.name(),
                        plan.name(),
                        row_offset,
                        row,
                        validation,
                    )
                    .err();
                }
            })?;
            if let Some(error) = assertion_error {
                return Err(error);
            }
        }
    }
    Ok(())
}

pub(super) fn scan_predicate(
    plan: &ComparePlan,
    batch: &RecordBatch,
    mut visit: impl FnMut(usize, Option<bool>),
) -> Result<(), ProofFrameError> {
    match (plan.left(), plan.right()) {
        (
            OperandPlan::Column {
                column_index: left,
                kernel,
                ..
            },
            OperandPlan::Column {
                column_index: right,
                ..
            },
        ) => scan_column_pair(
            kernel,
            batch.column(*left).as_ref(),
            batch.column(*right).as_ref(),
            plan,
            &mut visit,
        ),
        (
            OperandPlan::Column {
                column_index,
                kernel,
                ..
            },
            OperandPlan::Literal(literal),
        ) => scan_column_literal(
            kernel,
            batch.column(*column_index).as_ref(),
            literal,
            false,
            plan,
            &mut visit,
        ),
        (
            OperandPlan::Literal(literal),
            OperandPlan::Column {
                column_index,
                kernel,
                ..
            },
        ) => scan_column_literal(
            kernel,
            batch.column(*column_index).as_ref(),
            literal,
            true,
            plan,
            &mut visit,
        ),
        (OperandPlan::Literal(_), OperandPlan::Literal(_)) => {
            unreachable!("literal-only comparisons are rejected during compilation")
        }
    }
}

fn scan_column_pair(
    kernel: &KernelKind,
    left: &dyn Array,
    right: &dyn Array,
    plan: &ComparePlan,
    visit: &mut impl FnMut(usize, Option<bool>),
) -> Result<(), ProofFrameError> {
    macro_rules! pair {
        ($variant:pat, $array:ty) => {
            if matches!(kernel, $variant) {
                let left = downcast::<$array>(left);
                let right = downcast::<$array>(right);
                for row in 0..left.len() {
                    let outcome = if left.is_null(row) || right.is_null(row) {
                        null_outcome(left.is_null(row), right.is_null(row), plan)
                    } else {
                        Some(compare(left.value(row), right.value(row), plan.op()))
                    };
                    visit(row, outcome);
                }
                return Ok(());
            }
        };
    }

    pair!(KernelKind::Boolean, BooleanArray);
    pair!(KernelKind::I8, Int8Array);
    pair!(KernelKind::I16, Int16Array);
    pair!(KernelKind::I32, Int32Array);
    pair!(KernelKind::I64, Int64Array);
    pair!(KernelKind::U8, UInt8Array);
    pair!(KernelKind::U16, UInt16Array);
    pair!(KernelKind::U32, UInt32Array);
    pair!(KernelKind::U64, UInt64Array);
    pair!(KernelKind::F32, Float32Array);
    pair!(KernelKind::F64, Float64Array);
    pair!(KernelKind::Date32, Date32Array);
    pair!(KernelKind::Date64, Date64Array);
    pair!(KernelKind::Decimal128 { .. }, Decimal128Array);
    pair!(
        KernelKind::Timestamp(arrow::datatypes::TimeUnit::Second),
        TimestampSecondArray
    );
    pair!(
        KernelKind::Timestamp(arrow::datatypes::TimeUnit::Millisecond),
        TimestampMillisecondArray
    );
    pair!(
        KernelKind::Timestamp(arrow::datatypes::TimeUnit::Microsecond),
        TimestampMicrosecondArray
    );
    pair!(
        KernelKind::Timestamp(arrow::datatypes::TimeUnit::Nanosecond),
        TimestampNanosecondArray
    );
    pair!(KernelKind::Utf8, StringArray);
    pair!(KernelKind::LargeUtf8, LargeStringArray);
    pair!(KernelKind::Utf8View, StringViewArray);
    Err(unsupported(plan))
}

fn scan_column_literal(
    kernel: &KernelKind,
    array: &dyn Array,
    literal: &ScalarValuePlan,
    literal_is_left: bool,
    plan: &ComparePlan,
    visit: &mut impl FnMut(usize, Option<bool>),
) -> Result<(), ProofFrameError> {
    macro_rules! literal {
        ($variant:pat, $array:ty, $pattern:pat => $value:expr, $cast:expr) => {
            if matches!(kernel, $variant) {
                let values = downcast::<$array>(array);
                let $pattern = literal else {
                    return Err(unsupported(plan));
                };
                let literal_value = $cast($value);
                for row in 0..values.len() {
                    let outcome = if values.is_null(row) {
                        null_outcome(!literal_is_left, literal_is_left, plan)
                    } else if literal_is_left {
                        Some(compare(literal_value, values.value(row), plan.op()))
                    } else {
                        Some(compare(values.value(row), literal_value, plan.op()))
                    };
                    visit(row, outcome);
                }
                return Ok(());
            }
        };
    }

    literal!(KernelKind::Boolean, BooleanArray, ScalarValuePlan::Boolean(value) => value, |value: &bool| *value);
    literal!(KernelKind::I8, Int8Array, ScalarValuePlan::I64(value) => value, |value: &i64| *value as i8);
    literal!(KernelKind::I16, Int16Array, ScalarValuePlan::I64(value) => value, |value: &i64| *value as i16);
    literal!(KernelKind::I32, Int32Array, ScalarValuePlan::I64(value) => value, |value: &i64| *value as i32);
    literal!(KernelKind::I64, Int64Array, ScalarValuePlan::I64(value) => value, |value: &i64| *value);
    literal!(KernelKind::Date32, Date32Array, ScalarValuePlan::I64(value) => value, |value: &i64| *value as i32);
    literal!(KernelKind::Date64, Date64Array, ScalarValuePlan::I64(value) => value, |value: &i64| *value);
    literal!(KernelKind::U8, UInt8Array, ScalarValuePlan::U64(value) => value, |value: &u64| *value as u8);
    literal!(KernelKind::U16, UInt16Array, ScalarValuePlan::U64(value) => value, |value: &u64| *value as u16);
    literal!(KernelKind::U32, UInt32Array, ScalarValuePlan::U64(value) => value, |value: &u64| *value as u32);
    literal!(KernelKind::U64, UInt64Array, ScalarValuePlan::U64(value) => value, |value: &u64| *value);
    literal!(KernelKind::F32, Float32Array, ScalarValuePlan::F64(value) => value, |value: &f64| *value as f32);
    literal!(KernelKind::F64, Float64Array, ScalarValuePlan::F64(value) => value, |value: &f64| *value);
    literal!(KernelKind::Timestamp(arrow::datatypes::TimeUnit::Second), TimestampSecondArray, ScalarValuePlan::I64(value) => value, |value: &i64| *value);
    literal!(KernelKind::Timestamp(arrow::datatypes::TimeUnit::Millisecond), TimestampMillisecondArray, ScalarValuePlan::I64(value) => value, |value: &i64| *value);
    literal!(KernelKind::Timestamp(arrow::datatypes::TimeUnit::Microsecond), TimestampMicrosecondArray, ScalarValuePlan::I64(value) => value, |value: &i64| *value);
    literal!(KernelKind::Timestamp(arrow::datatypes::TimeUnit::Nanosecond), TimestampNanosecondArray, ScalarValuePlan::I64(value) => value, |value: &i64| *value);
    if matches!(
        kernel,
        KernelKind::Utf8 | KernelKind::LargeUtf8 | KernelKind::Utf8View
    ) {
        let ScalarValuePlan::Text(literal) = literal else {
            return Err(unsupported(plan));
        };
        return match kernel {
            KernelKind::Utf8 => scan_text_literal(
                downcast::<StringArray>(array),
                literal,
                literal_is_left,
                plan,
                visit,
            ),
            KernelKind::LargeUtf8 => scan_text_literal(
                downcast::<LargeStringArray>(array),
                literal,
                literal_is_left,
                plan,
                visit,
            ),
            KernelKind::Utf8View => scan_text_view_literal(
                downcast::<StringViewArray>(array),
                literal,
                literal_is_left,
                plan,
                visit,
            ),
            _ => unreachable!(),
        };
    }
    Err(unsupported(plan))
}

fn scan_text_literal<O: arrow::array::OffsetSizeTrait>(
    values: &arrow::array::GenericStringArray<O>,
    literal: &str,
    literal_is_left: bool,
    plan: &ComparePlan,
    visit: &mut impl FnMut(usize, Option<bool>),
) -> Result<(), ProofFrameError> {
    for row in 0..values.len() {
        let outcome = if values.is_null(row) {
            null_outcome(!literal_is_left, literal_is_left, plan)
        } else if literal_is_left {
            Some(compare(literal, values.value(row), plan.op()))
        } else {
            Some(compare(values.value(row), literal, plan.op()))
        };
        visit(row, outcome);
    }
    Ok(())
}

fn scan_text_view_literal(
    values: &StringViewArray,
    literal: &str,
    literal_is_left: bool,
    plan: &ComparePlan,
    visit: &mut impl FnMut(usize, Option<bool>),
) -> Result<(), ProofFrameError> {
    for row in 0..values.len() {
        let outcome = if values.is_null(row) {
            null_outcome(!literal_is_left, literal_is_left, plan)
        } else if literal_is_left {
            Some(compare(literal, values.value(row), plan.op()))
        } else {
            Some(compare(values.value(row), literal, plan.op()))
        };
        visit(row, outcome);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn scan_assertion_row(
    kernel: &KernelKind,
    rules: &CompiledRules,
    array: &dyn Array,
    column: &str,
    rule_name: &str,
    row_offset: u64,
    row: usize,
    validation: &mut ValidationState,
) -> Result<(), ProofFrameError> {
    if array.is_null(row) {
        if rules.not_null() {
            record_conditional(validation, "not_null", column, rule_name, row_offset, row);
        }
        return Ok(());
    }
    macro_rules! signed {
        ($variant:pat, $array:ty) => {
            if matches!(kernel, $variant) {
                scan_assertion_range(
                    downcast::<$array>(array).value(row) as i64,
                    signed_bound(rules.min()),
                    signed_bound(rules.max()),
                    column,
                    rule_name,
                    row_offset,
                    row,
                    validation,
                );
                return Ok(());
            }
        };
    }
    macro_rules! unsigned {
        ($variant:pat, $array:ty) => {
            if matches!(kernel, $variant) {
                scan_assertion_range(
                    downcast::<$array>(array).value(row) as u64,
                    unsigned_bound(rules.min()),
                    unsigned_bound(rules.max()),
                    column,
                    rule_name,
                    row_offset,
                    row,
                    validation,
                );
                return Ok(());
            }
        };
    }
    signed!(KernelKind::I8, Int8Array);
    signed!(KernelKind::I16, Int16Array);
    signed!(KernelKind::I32, Int32Array);
    signed!(KernelKind::I64, Int64Array);
    signed!(KernelKind::Date32, Date32Array);
    signed!(KernelKind::Date64, Date64Array);
    unsigned!(KernelKind::U8, UInt8Array);
    unsigned!(KernelKind::U16, UInt16Array);
    unsigned!(KernelKind::U32, UInt32Array);
    unsigned!(KernelKind::U64, UInt64Array);

    if matches!(kernel, KernelKind::F32) {
        return scan_float_assertion(
            downcast::<Float32Array>(array).value(row),
            float32_bound(rules.min()),
            float32_bound(rules.max()),
            rules,
            column,
            rule_name,
            row_offset,
            row,
            validation,
        );
    }
    if matches!(kernel, KernelKind::F64) {
        return scan_float_assertion(
            downcast::<Float64Array>(array).value(row),
            float64_bound(rules.min()),
            float64_bound(rules.max()),
            rules,
            column,
            rule_name,
            row_offset,
            row,
            validation,
        );
    }
    if matches!(kernel, KernelKind::Decimal128 { .. }) {
        scan_assertion_range(
            downcast::<Decimal128Array>(array).value(row),
            decimal_bound(rules.min()),
            decimal_bound(rules.max()),
            column,
            rule_name,
            row_offset,
            row,
            validation,
        );
        return Ok(());
    }
    macro_rules! timestamp {
        ($unit:ident, $array:ty) => {
            if matches!(
                kernel,
                KernelKind::Timestamp(arrow::datatypes::TimeUnit::$unit)
            ) {
                scan_assertion_range(
                    downcast::<$array>(array).value(row),
                    timestamp_bound(rules.min()),
                    timestamp_bound(rules.max()),
                    column,
                    rule_name,
                    row_offset,
                    row,
                    validation,
                );
                return Ok(());
            }
        };
    }
    timestamp!(Second, TimestampSecondArray);
    timestamp!(Millisecond, TimestampMillisecondArray);
    timestamp!(Microsecond, TimestampMicrosecondArray);
    timestamp!(Nanosecond, TimestampNanosecondArray);
    if matches!(kernel, KernelKind::Utf8) {
        scan_text_assertion(
            downcast::<StringArray>(array).value(row),
            rules,
            column,
            rule_name,
            row_offset,
            row,
            validation,
        );
        return Ok(());
    }
    if matches!(kernel, KernelKind::LargeUtf8) {
        scan_text_assertion(
            downcast::<LargeStringArray>(array).value(row),
            rules,
            column,
            rule_name,
            row_offset,
            row,
            validation,
        );
        return Ok(());
    }
    if matches!(kernel, KernelKind::Utf8View) {
        scan_text_assertion(
            downcast::<StringViewArray>(array).value(row),
            rules,
            column,
            rule_name,
            row_offset,
            row,
            validation,
        );
        return Ok(());
    }
    if matches!(kernel, KernelKind::Boolean) {
        return Ok(());
    }
    Err(ProofFrameError::UnsupportedType(format!(
        "conditional assertion for {}",
        array.data_type()
    )))
}

#[allow(clippy::too_many_arguments)]
fn scan_float_assertion<T: PartialOrd + Copy + IsNan>(
    value: T,
    minimum: Option<T>,
    maximum: Option<T>,
    rules: &CompiledRules,
    column: &str,
    rule_name: &str,
    row_offset: u64,
    row: usize,
    validation: &mut ValidationState,
) -> Result<(), ProofFrameError> {
    if value.nan() {
        if rules.validates_nan() && rules.nan() == NaNPolicy::Reject {
            record_conditional(validation, "nan", column, rule_name, row_offset, row);
        }
    } else {
        scan_assertion_range(
            value, minimum, maximum, column, rule_name, row_offset, row, validation,
        );
    }
    Ok(())
}

trait IsNan {
    fn nan(self) -> bool;
}

impl IsNan for f32 {
    fn nan(self) -> bool {
        self.is_nan()
    }
}

impl IsNan for f64 {
    fn nan(self) -> bool {
        self.is_nan()
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_assertion_range<T: PartialOrd + Copy>(
    value: T,
    minimum: Option<T>,
    maximum: Option<T>,
    column: &str,
    rule_name: &str,
    row_offset: u64,
    row: usize,
    validation: &mut ValidationState,
) {
    if minimum.is_some_and(|minimum| value < minimum) {
        record_conditional(validation, "min", column, rule_name, row_offset, row);
    }
    if maximum.is_some_and(|maximum| value > maximum) {
        record_conditional(validation, "max", column, rule_name, row_offset, row);
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_text_assertion(
    value: &str,
    rules: &CompiledRules,
    column: &str,
    rule_name: &str,
    row_offset: u64,
    row: usize,
    validation: &mut ValidationState,
) {
    if rules
        .pattern()
        .is_some_and(|pattern| !pattern.is_match(value))
    {
        record_conditional(validation, "pattern", column, rule_name, row_offset, row);
    }
    if rules
        .allowed()
        .is_some_and(|allowed| !allowed.contains(value))
    {
        record_conditional(validation, "allowed", column, rule_name, row_offset, row);
    }
}

fn record_conditional(
    validation: &mut ValidationState,
    assertion: &'static str,
    column: &str,
    rule_name: &str,
    row_offset: u64,
    row: usize,
) {
    record_lazy(
        validation,
        "conditional",
        column,
        Some(row_offset + row as u64),
        || format!("Conditional rule `{rule_name}` failed `{assertion}`"),
    );
}

fn null_outcome(left_null: bool, right_null: bool, plan: &ComparePlan) -> Option<bool> {
    match plan.nulls() {
        NullPolicyAst::Skip => None,
        NullPolicyAst::Fail => Some(false),
        NullPolicyAst::Equal => Some(match plan.op() {
            CompareOpAst::Eq => left_null && right_null,
            CompareOpAst::Ne => left_null != right_null,
            CompareOpAst::Lt | CompareOpAst::Lte | CompareOpAst::Gt | CompareOpAst::Gte => false,
        }),
    }
}

fn compare<T: PartialEq + PartialOrd>(left: T, right: T, op: CompareOpAst) -> bool {
    match op {
        CompareOpAst::Eq => left == right,
        CompareOpAst::Ne => left != right,
        CompareOpAst::Lt => left < right,
        CompareOpAst::Lte => left <= right,
        CompareOpAst::Gt => left > right,
        CompareOpAst::Gte => left >= right,
    }
}

fn finding_column(plan: &ComparePlan) -> &str {
    plan.left()
        .field()
        .or_else(|| plan.right().field())
        .map_or("$row", |field| field.name())
}

fn unsupported(plan: &ComparePlan) -> ProofFrameError {
    ProofFrameError::UnsupportedType(format!(
        "relational execution for {:?} and {:?}",
        plan.left().kernel(),
        plan.right().kernel()
    ))
}

fn downcast<T: 'static>(array: &dyn Array) -> &T {
    array
        .as_any()
        .downcast_ref::<T>()
        .expect("kernel is fixed from the compiled Arrow schema")
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

fn decimal_bound(bound: Option<&TypedBound>) -> Option<i128> {
    match bound {
        Some(TypedBound::Decimal128 { value, .. }) => Some(*value),
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
