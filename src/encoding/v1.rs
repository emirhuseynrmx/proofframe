use arrow::record_batch::RecordBatchReader;

use super::plan::EncodingPlan;
use super::scratch::Scratch;
use super::{Fingerprint, FingerprintVersion};
use crate::ProofFrameError;

pub(crate) fn fingerprint_v1<R>(reader: R) -> Result<Fingerprint, ProofFrameError>
where
    R: RecordBatchReader,
{
    let schema = reader.schema();
    let plan = EncodingPlan::for_schema(schema.as_ref())?;
    let mut hasher = blake3::Hasher::new();
    let mut scratch = Scratch::default();
    let mut rows = 0_u64;

    hasher.update(b"pf-fp-v1\0");
    for field in schema.fields() {
        hasher.update(b"pf-schema-field-v1\0");
        update_len_prefixed(&mut hasher, field.name().as_bytes());
        let data_type = field.data_type().to_string();
        update_len_prefixed(&mut hasher, data_type.as_bytes());
        hasher.update(&[u8::from(field.is_nullable())]);
    }
    hasher.update(b"pf-fp-body-v1\0");

    for maybe_batch in reader {
        let batch = maybe_batch?;
        for row in 0..batch.num_rows() {
            for (column, encoder) in plan.encoders.iter().enumerate() {
                encoder.update_v1(
                    &mut hasher,
                    column,
                    batch.column(column).as_ref(),
                    row,
                    &mut scratch.bytes,
                )?;
            }
        }
        rows += batch.num_rows() as u64;
    }

    let digest = hasher.finalize();
    Ok(Fingerprint::new(
        FingerprintVersion::V1,
        *digest.as_bytes(),
        rows,
    ))
}

fn update_len_prefixed(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}
