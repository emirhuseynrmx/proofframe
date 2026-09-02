use std::collections::BTreeSet;

use arrow::array::{
    Array, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array, UInt8Array,
    UInt16Array, UInt32Array, UInt64Array,
};
use arrow::datatypes::{DataType, SchemaRef, TimeUnit};
use arrow::record_batch::RecordBatchReader;
use arrow::util::display::array_value_to_string;
use serde_json::{Map, Value, json};

use crate::{
    ExactState, ProofFrameError, ResourceAccount, ResourceLimits, ValueKind, ValueRef,
    canonical_value_bytes, profile_hasher, update_hash,
};

/// Controls which potentially expensive or brittle rules are included in a suggestion.
#[derive(Debug, Clone, Copy)]
pub struct SuggestOptions {
    pub infer_uniqueness: bool,
    pub infer_categories: bool,
    pub max_categories: usize,
    pub infer_required: bool,
    pub infer_ranges: bool,
    pub range_tolerance: f64,
    pub infer_row_count: bool,
    pub resources: ResourceLimits,
}

impl Default for SuggestOptions {
    fn default() -> Self {
        Self {
            infer_uniqueness: false,
            infer_categories: false,
            max_categories: 20,
            infer_required: false,
            infer_ranges: true,
            range_tolerance: 0.0,
            infer_row_count: true,
            resources: ResourceLimits::default(),
        }
    }
}

#[derive(Debug)]
enum RangeStats {
    Signed {
        min: i64,
        max: i64,
        last: i64,
        monotonic: bool,
    },
    Unsigned {
        min: u64,
        max: u64,
        last: u64,
        monotonic: bool,
    },
    Float {
        min: f64,
        max: f64,
        last: f64,
        monotonic: bool,
    },
}

impl RangeStats {
    fn observe(&mut self, array: &dyn Array, row: usize) {
        match self {
            Self::Signed {
                min,
                max,
                last,
                monotonic,
            } => {
                if let Some(value) = signed_value(array, row) {
                    *monotonic &= value >= *last;
                    *min = (*min).min(value);
                    *max = (*max).max(value);
                    *last = value;
                }
            }
            Self::Unsigned {
                min,
                max,
                last,
                monotonic,
            } => {
                if let Some(value) = unsigned_value(array, row) {
                    *monotonic &= value >= *last;
                    *min = (*min).min(value);
                    *max = (*max).max(value);
                    *last = value;
                }
            }
            Self::Float {
                min,
                max,
                last,
                monotonic,
            } => {
                if let Some(value) = float_value(array, row).filter(|value| value.is_finite()) {
                    *monotonic &= value >= *last;
                    *min = (*min).min(value);
                    *max = (*max).max(value);
                    *last = value;
                }
            }
        }
    }

    fn bounds(&self, tolerance: f64) -> Option<(Value, Value)> {
        match self {
            Self::Signed {
                min,
                max,
                monotonic,
                ..
            } if !monotonic => {
                let span = (*max as i128 - *min as i128) as f64;
                let delta = (span * tolerance).ceil() as i64;
                Some((
                    json!(min.saturating_sub(delta)),
                    json!(max.saturating_add(delta)),
                ))
            }
            Self::Unsigned {
                min,
                max,
                monotonic,
                ..
            } if !monotonic => {
                let delta = ((*max - *min) as f64 * tolerance).ceil() as u64;
                Some((
                    json!(min.saturating_sub(delta)),
                    json!(max.saturating_add(delta)),
                ))
            }
            Self::Float {
                min,
                max,
                monotonic,
                ..
            } if !monotonic => {
                let delta = (*max - *min) * tolerance;
                Some((json!(min - delta), json!(max + delta)))
            }
            _ => None,
        }
    }

    fn is_monotonic(&self) -> bool {
        match self {
            Self::Signed { monotonic, .. }
            | Self::Unsigned { monotonic, .. }
            | Self::Float { monotonic, .. } => *monotonic,
        }
    }
}

struct SuggestionState {
    null_count: u64,
    non_null_count: u64,
    distinct: Option<ExactState>,
    categories: Option<BTreeSet<String>>,
    range: Option<RangeStats>,
    timestamp: bool,
}

impl SuggestionState {
    fn new(
        data_type: &DataType,
        options: SuggestOptions,
        account: &ResourceAccount,
        directory: Option<&std::path::Path>,
        row_count_hint: Option<u64>,
    ) -> Result<Self, ProofFrameError> {
        let distinct = if options.infer_uniqueness {
            Some(ExactState::new(
                ValueKind::Bytes,
                account.child(
                    account.limits().max_memory_bytes,
                    account.limits().max_temp_bytes,
                ),
                directory
                    .expect("exact suggestion owns a temporary directory")
                    .to_path_buf(),
                row_count_hint,
            )?)
        } else {
            None
        };
        Ok(Self {
            null_count: 0,
            non_null_count: 0,
            distinct,
            categories: if options.infer_categories && is_utf8(data_type) {
                Some(BTreeSet::new())
            } else {
                None
            },
            range: range_for(data_type),
            timestamp: matches!(data_type, DataType::Timestamp(_, _)),
        })
    }
}

/// Suggest a strict V2 contract from one Arrow stream without materializing it.
pub fn suggest_reader_with_options<R>(
    reader: R,
    options: SuggestOptions,
    row_count_hint: Option<u64>,
) -> Result<Value, ProofFrameError>
where
    R: RecordBatchReader,
{
    if options.max_categories == 0 {
        return Err(ProofFrameError::InvalidContract(
            "max_categories must be positive".into(),
        ));
    }
    if !options.range_tolerance.is_finite() || options.range_tolerance < 0.0 {
        return Err(ProofFrameError::InvalidContract(
            "range_tolerance must be a finite non-negative number".into(),
        ));
    }

    let schema = reader.schema();
    let root = ResourceAccount::root(options.resources);
    let directory = options
        .infer_uniqueness
        .then(tempfile::TempDir::new)
        .transpose()?;
    let mut states = schema
        .fields()
        .iter()
        .map(|field| {
            SuggestionState::new(
                field.data_type(),
                options,
                &root,
                directory.as_ref().map(tempfile::TempDir::path),
                row_count_hint,
            )
        })
        .collect::<Result<Vec<_>, ProofFrameError>>()?;
    let mut rows = 0_u64;
    let mut hasher = profile_hasher(&schema);

    for batch in reader {
        let batch = batch?;
        if batch.schema().as_ref() != schema.as_ref() {
            return Err(ProofFrameError::SchemaMismatch(
                "record batch schema changed during suggestion".to_string(),
            ));
        }
        for row in 0..batch.num_rows() {
            for (index, array) in batch.columns().iter().enumerate() {
                update_hash(&mut hasher, index, array.as_ref(), row)?;
                let state = &mut states[index];
                if array.is_null(row) {
                    state.null_count += 1;
                    continue;
                }
                state.non_null_count += 1;
                if let Some(distinct) = &mut state.distinct {
                    let key = canonical_value_bytes(array.as_ref(), row)?;
                    distinct.insert(ValueRef::Bytes(&key), rows + row as u64)?;
                }
                if let Some(range) = &mut state.range {
                    range.observe(array.as_ref(), row);
                }
                if let Some(categories) = &mut state.categories {
                    categories.insert(array_value_to_string(array.as_ref(), row)?);
                    if categories.len() > options.max_categories {
                        state.categories = None;
                    }
                }
            }
        }
        rows += batch.num_rows() as u64;
    }

    render_contract(
        &schema,
        states,
        rows,
        hasher.finalize().to_hex().to_string(),
        options,
    )
}

fn render_contract(
    schema: &SchemaRef,
    mut states: Vec<SuggestionState>,
    rows: u64,
    fingerprint: String,
    options: SuggestOptions,
) -> Result<Value, ProofFrameError> {
    let mut columns = Map::new();
    let mut distinct_ratio = Map::new();
    let mut review = Vec::new();

    for (field, state) in schema.fields().iter().zip(states.iter_mut()) {
        let mut rules = Map::new();
        if let Some(expected_type) = type_value(field.data_type()) {
            rules.insert("type".to_string(), expected_type);
        }
        if state.non_null_count == rows && rows > 0 {
            rules.insert("not_null".to_string(), json!(true));
        }
        if options.infer_required {
            rules.insert("required".to_string(), json!(true));
        }
        if options.infer_ranges {
            if state.timestamp {
                review.push(json!({"column": field.name(), "reason": "timestamp_range_omitted"}));
            } else if let Some(range) = &state.range {
                if let Some((min, max)) = range.bounds(options.range_tolerance) {
                    rules.insert("min".to_string(), min);
                    rules.insert("max".to_string(), max);
                } else if range.is_monotonic() {
                    review
                        .push(json!({"column": field.name(), "reason": "monotonic_range_omitted"}));
                }
            }
        }
        if let Some(categories) = state.categories.take().filter(|values| !values.is_empty()) {
            rules.insert("allowed".to_string(), json!(categories));
        }
        if let Some(distinct) = state.distinct.take() {
            if distinct.finish()?.distinct_count == rows {
                distinct_ratio.insert(field.name().to_string(), json!({"min": 1.0}));
            }
        }
        columns.insert(field.name().to_string(), Value::Object(rules));
    }

    let mut dataset_rules = Map::new();
    if options.infer_row_count {
        dataset_rules.insert("row_count".to_string(), json!({"min": rows}));
    }
    if !distinct_ratio.is_empty() {
        dataset_rules.insert("distinct_ratio".to_string(), Value::Object(distinct_ratio));
    }

    Ok(json!({
        "version": "proofframe.contract.v2",
        "status": "draft",
        "columns": columns,
        "dataset_rules": dataset_rules,
        "suggested_from": {
            "proofframe_version": env!("CARGO_PKG_VERSION"),
            "dataset_fingerprint": format!("pf-fp-v1:{fingerprint}"),
            "rows_observed": rows,
            "uniqueness_inferred": options.infer_uniqueness,
            "review": review,
        }
    }))
}

fn range_for(data_type: &DataType) -> Option<RangeStats> {
    match data_type {
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => {
            Some(RangeStats::Signed {
                min: i64::MAX,
                max: i64::MIN,
                last: i64::MIN,
                monotonic: true,
            })
        }
        DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => {
            Some(RangeStats::Unsigned {
                min: u64::MAX,
                max: u64::MIN,
                last: u64::MIN,
                monotonic: true,
            })
        }
        DataType::Float32 | DataType::Float64 => Some(RangeStats::Float {
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            last: f64::NEG_INFINITY,
            monotonic: true,
        }),
        _ => None,
    }
}

fn type_value(data_type: &DataType) -> Option<Value> {
    let primitive = match data_type {
        DataType::Boolean => "boolean",
        DataType::Int8 => "int8",
        DataType::Int16 => "int16",
        DataType::Int32 => "int32",
        DataType::Int64 => "int64",
        DataType::UInt8 => "uint8",
        DataType::UInt16 => "uint16",
        DataType::UInt32 => "uint32",
        DataType::UInt64 => "uint64",
        DataType::Float32 => "float32",
        DataType::Float64 => "float64",
        DataType::Utf8 => "utf8",
        DataType::LargeUtf8 => "large_utf8",
        DataType::Utf8View => "utf8_view",
        DataType::Binary => "binary",
        DataType::LargeBinary => "large_binary",
        DataType::BinaryView => "binary_view",
        DataType::Date32 => "date32",
        DataType::Date64 => "date64",
        _ => return parameterized_type_value(data_type),
    };
    Some(json!(primitive))
}

fn parameterized_type_value(data_type: &DataType) -> Option<Value> {
    match data_type {
        DataType::Decimal128(precision, scale) => {
            Some(json!({"name":"decimal128","precision":precision,"scale":scale}))
        }
        DataType::Timestamp(unit, timezone) => Some(json!({
            "name": "timestamp",
            "unit": time_unit_name(*unit),
            "timezone": timezone.as_ref().map(ToString::to_string),
        })),
        _ => None,
    }
}

fn time_unit_name(unit: TimeUnit) -> &'static str {
    match unit {
        TimeUnit::Second => "s",
        TimeUnit::Millisecond => "ms",
        TimeUnit::Microsecond => "us",
        TimeUnit::Nanosecond => "ns",
    }
}

fn is_utf8(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View
    )
}

fn signed_value(array: &dyn Array, row: usize) -> Option<i64> {
    if let Some(values) = array.as_any().downcast_ref::<Int8Array>() {
        Some(i64::from(values.value(row)))
    } else if let Some(values) = array.as_any().downcast_ref::<Int16Array>() {
        Some(i64::from(values.value(row)))
    } else if let Some(values) = array.as_any().downcast_ref::<Int32Array>() {
        Some(i64::from(values.value(row)))
    } else {
        array
            .as_any()
            .downcast_ref::<Int64Array>()
            .map(|values| values.value(row))
    }
}

fn unsigned_value(array: &dyn Array, row: usize) -> Option<u64> {
    if let Some(values) = array.as_any().downcast_ref::<UInt8Array>() {
        Some(u64::from(values.value(row)))
    } else if let Some(values) = array.as_any().downcast_ref::<UInt16Array>() {
        Some(u64::from(values.value(row)))
    } else if let Some(values) = array.as_any().downcast_ref::<UInt32Array>() {
        Some(u64::from(values.value(row)))
    } else {
        array
            .as_any()
            .downcast_ref::<UInt64Array>()
            .map(|values| values.value(row))
    }
}

fn float_value(array: &dyn Array, row: usize) -> Option<f64> {
    if let Some(values) = array.as_any().downcast_ref::<Float32Array>() {
        Some(f64::from(values.value(row)))
    } else {
        array
            .as_any()
            .downcast_ref::<Float64Array>()
            .map(|values| values.value(row))
    }
}
