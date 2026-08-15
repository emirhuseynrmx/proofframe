//! Resource-bounded exact train/test identity intersection.

use arrow::datatypes::{Field, Schema};
use arrow::record_batch::RecordBatchReader;
use tempfile::TempDir;

use crate::distinct::intersect_exact_states;
use crate::{
    CancellationToken, ExactState, LeakageReport, ProofFrameError, ResourceAccount, ResourceLimits,
    ValueKind, ValueRef,
};

#[derive(Debug, Clone)]
pub struct LeakageOptions {
    pub resources: ResourceLimits,
    pub max_samples: usize,
    pub cancellation: CancellationToken,
}

impl Default for LeakageOptions {
    fn default() -> Self {
        let resources = ResourceLimits::default();
        Self {
            max_samples: resources.max_samples,
            resources,
            cancellation: CancellationToken::new(),
        }
    }
}

pub fn detect_leakage_with_options<TR, TE>(
    train: TR,
    test: TE,
    keys: &[String],
    options: &LeakageOptions,
) -> Result<LeakageReport, ProofFrameError>
where
    TR: RecordBatchReader,
    TE: RecordBatchReader,
{
    let train_schema = train.schema();
    let test_schema = test.schema();
    let (train_indexes, test_indexes) =
        identity_indexes(train_schema.as_ref(), test_schema.as_ref(), keys)?;
    let directory = TempDir::new()?;
    let root = ResourceAccount::root(options.resources);
    let train_account = root.child(
        options.resources.max_memory_bytes,
        options.resources.max_temp_bytes,
    );
    let test_account = root.child(
        options.resources.max_memory_bytes,
        options.resources.max_temp_bytes,
    );
    let mut train_state = ExactState::new_with_cancellation(
        ValueKind::Bytes,
        train_account,
        directory.path().to_path_buf(),
        None,
        options.cancellation.clone(),
    )?;
    let mut test_state = ExactState::new_with_cancellation(
        ValueKind::Bytes,
        test_account,
        directory.path().to_path_buf(),
        None,
        options.cancellation.clone(),
    )?;
    let train_rows = collect_identities(
        train,
        &train_indexes,
        keys.is_empty(),
        &mut train_state,
        &options.cancellation,
    )?;
    let test_rows = collect_identities(
        test,
        &test_indexes,
        keys.is_empty(),
        &mut test_state,
        &options.cancellation,
    )?;
    let sample_limit = options.max_samples.min(options.resources.max_samples);
    let summary = intersect_exact_states(train_state, test_state, sample_limit)?;
    let overlap_count = usize::try_from(summary.overlap)
        .map_err(|_| ProofFrameError::CorruptData("Leakage overlap exceeds usize".into()))?;
    let sample_fingerprints = summary
        .samples
        .into_iter()
        .map(|digest| blake3::Hash::from_bytes(digest).to_hex().to_string())
        .collect::<Vec<_>>();
    Ok(LeakageReport {
        detected: overlap_count != 0,
        mode: if keys.is_empty() { "full_row" } else { "key" },
        keys: keys.to_vec(),
        train_rows,
        test_rows,
        overlap_count,
        train_overlap_rate: ratio(summary.overlap, summary.left_distinct),
        test_overlap_rate: ratio(summary.overlap, summary.right_distinct),
        truncated: overlap_count > sample_fingerprints.len(),
        sample_fingerprints,
    })
}

fn identity_indexes(
    train: &Schema,
    test: &Schema,
    keys: &[String],
) -> Result<(Vec<usize>, Vec<usize>), ProofFrameError> {
    if keys.is_empty() {
        if train != test {
            return Err(ProofFrameError::SchemaMismatch(
                "full-row leakage detection requires identical Arrow schemas".to_string(),
            ));
        }
        let indexes = (0..train.fields().len()).collect::<Vec<_>>();
        return Ok((indexes.clone(), indexes));
    }
    let train_indexes = resolve_keys(train, keys)?;
    let test_indexes = resolve_keys(test, keys)?;
    for (ordinal, (train_index, test_index)) in train_indexes.iter().zip(&test_indexes).enumerate()
    {
        let train_field = train.field(*train_index);
        let test_field = test.field(*test_index);
        if train_field.name() != test_field.name()
            || train_field.data_type() != test_field.data_type()
            || train_field.is_nullable() != test_field.is_nullable()
        {
            return Err(ProofFrameError::SchemaMismatch(format!(
                "logical leakage key {ordinal} has incompatible Arrow fields"
            )));
        }
    }
    Ok((train_indexes, test_indexes))
}

fn resolve_keys(schema: &Schema, keys: &[String]) -> Result<Vec<usize>, ProofFrameError> {
    keys.iter()
        .map(|key| {
            schema
                .index_of(key)
                .map_err(|_| ProofFrameError::MissingColumn(key.clone()))
        })
        .collect()
}

fn collect_identities<R: RecordBatchReader>(
    reader: R,
    indexes: &[usize],
    full_row: bool,
    state: &mut ExactState,
    cancellation: &CancellationToken,
) -> Result<usize, ProofFrameError> {
    let schema = reader.schema();
    let mut rows = 0_usize;
    for batch in reader {
        cancellation.check()?;
        let batch = batch?;
        for row in 0..batch.num_rows() {
            let mut hasher = blake3::Hasher::new();
            hasher.update(if full_row {
                b"proofframe:leakage:full-row:v2\0"
            } else {
                b"proofframe:leakage:key:v2\0"
            });
            for (ordinal, index) in indexes.iter().enumerate() {
                update_field_identity(&mut hasher, ordinal, schema.field(*index));
                let value = crate::canonical_value_bytes(batch.column(*index).as_ref(), row)?;
                hasher.update(&(value.len() as u64).to_le_bytes());
                hasher.update(&value);
            }
            let digest = hasher.finalize();
            state.insert(ValueRef::Bytes(digest.as_bytes()), rows as u64)?;
            rows = rows.checked_add(1).ok_or_else(|| {
                ProofFrameError::CorruptData("Leakage row count overflowed usize".into())
            })?;
        }
    }
    Ok(rows)
}

fn update_field_identity(hasher: &mut blake3::Hasher, ordinal: usize, field: &Field) {
    hasher.update(&(ordinal as u64).to_le_bytes());
    update_bytes(hasher, field.name().as_bytes());
    update_bytes(hasher, field.data_type().to_string().as_bytes());
    hasher.update(&[u8::from(field.is_nullable())]);
}

fn update_bytes(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}
