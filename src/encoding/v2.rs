use std::collections::{BTreeSet, HashMap};

use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatchReader;

use super::plan::EncodingPlan;
use super::scratch::Scratch;
use super::{Fingerprint, FingerprintOptions, FingerprintVersion};
use crate::ProofFrameError;

pub(crate) fn fingerprint_v2<R>(
    reader: R,
    options: &FingerprintOptions,
) -> Result<Fingerprint, ProofFrameError>
where
    R: RecordBatchReader,
{
    let schema = reader.schema();
    let plan = EncodingPlan::for_schema(schema.as_ref())?;
    let schema_digest = fingerprint_schema(schema.as_ref(), options.metadata_keys())?;
    let mut columns = Vec::with_capacity(plan.encoders.len());
    for column in 0..plan.encoders.len() {
        let mut state = blake3::Hasher::new();
        state.update(b"pf-fp-v2-column\0");
        state.update(&(column as u64).to_le_bytes());
        columns.push(state);
    }

    let mut rows = 0_u64;
    let mut scratch = Scratch::default();
    for maybe_batch in reader {
        let batch = maybe_batch?;
        for (column, encoder) in plan.encoders.iter().enumerate() {
            let array = batch.column(column);
            for row in 0..batch.num_rows() {
                encoder.update_v2(
                    &mut columns[column],
                    array.as_ref(),
                    row,
                    &mut scratch.bytes,
                )?;
            }
        }
        rows += batch.num_rows() as u64;
    }

    let mut root = blake3::Hasher::new();
    root.update(b"pf-fp-v2\0");
    root.update(schema_digest.as_bytes());
    root.update(&rows.to_le_bytes());
    root.update(&(columns.len() as u64).to_le_bytes());
    for (column, state) in columns.into_iter().enumerate() {
        root.update(&(column as u64).to_le_bytes());
        root.update(state.finalize().as_bytes());
    }
    let digest = root.finalize();
    Ok(Fingerprint::new(
        FingerprintVersion::V2,
        *digest.as_bytes(),
        rows,
    ))
}

fn fingerprint_schema(
    schema: &Schema,
    metadata_keys: &BTreeSet<String>,
) -> Result<blake3::Hash, ProofFrameError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"pf-schema-v2\0");
    update_selected_metadata(&mut hasher, schema.metadata(), metadata_keys);
    hasher.update(&(schema.fields().len() as u64).to_le_bytes());
    for (ordinal, field) in schema.fields().iter().enumerate() {
        update_field(&mut hasher, ordinal, field, metadata_keys)?;
    }
    Ok(hasher.finalize())
}

fn update_field(
    hasher: &mut blake3::Hasher,
    ordinal: usize,
    field: &Field,
    metadata_keys: &BTreeSet<String>,
) -> Result<(), ProofFrameError> {
    hasher.update(b"pf-field-v2\0");
    hasher.update(&(ordinal as u64).to_le_bytes());
    update_len_prefixed(hasher, field.name().as_bytes());
    hasher.update(&[u8::from(field.is_nullable())]);
    update_selected_metadata(hasher, field.metadata(), metadata_keys);
    update_data_type(hasher, field.data_type(), metadata_keys)
}

fn update_data_type(
    hasher: &mut blake3::Hasher,
    data_type: &DataType,
    metadata_keys: &BTreeSet<String>,
) -> Result<(), ProofFrameError> {
    macro_rules! scalar {
        ($pattern:pat, $tag:literal) => {
            if matches!(data_type, $pattern) {
                hasher.update(&[$tag]);
                return Ok(());
            }
        };
    }

    scalar!(DataType::Int8, 1);
    scalar!(DataType::Int16, 2);
    scalar!(DataType::Int32, 3);
    scalar!(DataType::Int64, 4);
    scalar!(DataType::UInt8, 5);
    scalar!(DataType::UInt16, 6);
    scalar!(DataType::UInt32, 7);
    scalar!(DataType::UInt64, 8);
    scalar!(DataType::Float32, 9);
    scalar!(DataType::Float64, 10);
    scalar!(DataType::Date32, 11);
    scalar!(DataType::Date64, 12);

    match data_type {
        DataType::Timestamp(unit, timezone) => {
            hasher.update(&[13, time_unit_tag(*unit)]);
            update_optional_bytes(hasher, timezone.as_deref().map(str::as_bytes));
        }
        DataType::Decimal128(precision, scale) => {
            hasher.update(&[17, *precision, scale.to_le_bytes()[0]]);
        }
        DataType::Boolean => {
            hasher.update(&[18]);
        }
        DataType::Utf8 => {
            hasher.update(&[19]);
        }
        DataType::LargeUtf8 => {
            hasher.update(&[20]);
        }
        DataType::Binary => {
            hasher.update(&[21]);
        }
        DataType::LargeBinary => {
            hasher.update(&[22]);
        }
        DataType::List(field) => {
            hasher.update(&[23]);
            update_field(hasher, 0, field, metadata_keys)?;
        }
        DataType::LargeList(field) => {
            hasher.update(&[24]);
            update_field(hasher, 0, field, metadata_keys)?;
        }
        DataType::FixedSizeList(field, length) => {
            hasher.update(&[25]);
            hasher.update(&length.to_le_bytes());
            update_field(hasher, 0, field, metadata_keys)?;
        }
        DataType::Struct(fields) => {
            hasher.update(&[26]);
            hasher.update(&(fields.len() as u64).to_le_bytes());
            for (ordinal, field) in fields.iter().enumerate() {
                update_field(hasher, ordinal, field, metadata_keys)?;
            }
        }
        DataType::Map(field, sorted) => {
            hasher.update(&[27, u8::from(*sorted)]);
            update_field(hasher, 0, field, metadata_keys)?;
        }
        unsupported => return Err(ProofFrameError::UnsupportedType(unsupported.to_string())),
    }
    Ok(())
}

const fn time_unit_tag(unit: TimeUnit) -> u8 {
    match unit {
        TimeUnit::Second => 1,
        TimeUnit::Millisecond => 2,
        TimeUnit::Microsecond => 3,
        TimeUnit::Nanosecond => 4,
    }
}

fn update_selected_metadata(
    hasher: &mut blake3::Hasher,
    metadata: &HashMap<String, String>,
    selected: &BTreeSet<String>,
) {
    hasher.update(&(selected.len() as u64).to_le_bytes());
    for key in selected {
        update_len_prefixed(hasher, key.as_bytes());
        update_optional_bytes(hasher, metadata.get(key).map(String::as_bytes));
    }
}

fn update_optional_bytes(hasher: &mut blake3::Hasher, value: Option<&[u8]>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            update_len_prefixed(hasher, value);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn update_len_prefixed(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}
