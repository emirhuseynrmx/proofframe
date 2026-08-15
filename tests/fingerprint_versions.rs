mod support;

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{ArrayRef, BooleanArray, Int64Array, StringArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use proofframe::{
    FingerprintOptions, FingerprintVersion, fingerprint_reader, fingerprint_reader_with_options,
};

use support::reader_from_batches;

fn v1_golden_batch() -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, true),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![1_i64, 2, 3])) as ArrayRef,
            Arc::new(StringArray::from(vec![Some("a"), None, Some("c")])) as ArrayRef,
        ],
    )
    .expect("golden Arrow batch must be valid")
}

#[test]
fn v1_golden_vector_is_frozen() {
    assert_eq!(
        fingerprint_reader(reader_from_batches(vec![v1_golden_batch()])).unwrap(),
        "pf-fp-v1:4dc74e666725f040dea7e788827d0411e59c19a72b55f2ce27f22ed9a00afb42"
    );
}

#[test]
fn v1_remains_invariant_to_real_batch_segmentation() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let whole = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(Int64Array::from(vec![1_i64, 2, 3, 4])) as ArrayRef],
    )
    .unwrap();
    let first = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(Int64Array::from(vec![1_i64, 2])) as ArrayRef],
    )
    .unwrap();
    let second = RecordBatch::try_new(
        schema,
        vec![Arc::new(Int64Array::from(vec![3_i64, 4])) as ArrayRef],
    )
    .unwrap();

    let direct = fingerprint_reader(reader_from_batches(vec![whole])).unwrap();
    let segmented = fingerprint_reader(reader_from_batches(vec![first, second])).unwrap();

    assert_eq!(direct, segmented);
}

fn whole_v2_batch() -> RecordBatch {
    RecordBatch::try_from_iter(vec![
        (
            "id",
            Arc::new(Int64Array::from(vec![1_i64, 2, 3, 4])) as ArrayRef,
        ),
        (
            "flag",
            Arc::new(BooleanArray::from(vec![true, false, true, false])) as ArrayRef,
        ),
    ])
    .unwrap()
}

fn segmented_v2_batches() -> Vec<RecordBatch> {
    vec![
        RecordBatch::try_from_iter(vec![
            ("id", Arc::new(Int64Array::from(vec![1_i64, 2])) as ArrayRef),
            (
                "flag",
                Arc::new(BooleanArray::from(vec![true, false])) as ArrayRef,
            ),
        ])
        .unwrap(),
        RecordBatch::try_from_iter(vec![
            ("id", Arc::new(Int64Array::from(vec![3_i64, 4])) as ArrayRef),
            (
                "flag",
                Arc::new(BooleanArray::from(vec![true, false])) as ArrayRef,
            ),
        ])
        .unwrap(),
    ]
}

fn fingerprint_v2(batches: Vec<RecordBatch>) -> proofframe::Fingerprint {
    fingerprint_reader_with_options(
        reader_from_batches(batches),
        &FingerprintOptions::new(FingerprintVersion::V2),
    )
    .unwrap()
}

#[test]
fn v2_is_invariant_to_real_batch_segmentation() {
    let direct = fingerprint_v2(vec![whole_v2_batch()]);
    let segmented = fingerprint_v2(segmented_v2_batches());

    assert_eq!(direct, segmented);
    assert_eq!(direct.version(), FingerprintVersion::V2);
    assert_eq!(direct.rows(), 4);
    assert!(direct.to_string().starts_with("pf-fp-v2:"));
}

#[test]
fn explicit_v1_selection_matches_the_frozen_compatibility_entry_point() {
    let direct = fingerprint_reader(reader_from_batches(vec![v1_golden_batch()])).unwrap();
    let explicit = fingerprint_reader_with_options(
        reader_from_batches(vec![v1_golden_batch()]),
        &FingerprintOptions::new(FingerprintVersion::V1),
    )
    .unwrap();

    assert_eq!(explicit.to_string(), direct);
    assert_eq!(explicit.version(), FingerprintVersion::V1);
}

#[test]
fn v2_binds_arrow_type_and_nullability() {
    let signed_schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let unsigned_schema = Arc::new(Schema::new(vec![Field::new("id", DataType::UInt64, false)]));
    let nullable_schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, true)]));
    let signed = RecordBatch::try_new(
        signed_schema,
        vec![Arc::new(Int64Array::from(vec![1_i64, 2])) as ArrayRef],
    )
    .unwrap();
    let unsigned = RecordBatch::try_new(
        unsigned_schema,
        vec![Arc::new(UInt64Array::from(vec![1_u64, 2])) as ArrayRef],
    )
    .unwrap();
    let nullable = RecordBatch::try_new(
        nullable_schema,
        vec![Arc::new(Int64Array::from(vec![1_i64, 2])) as ArrayRef],
    )
    .unwrap();

    assert_ne!(
        fingerprint_v2(vec![signed.clone()]),
        fingerprint_v2(vec![unsigned])
    );
    assert_ne!(fingerprint_v2(vec![signed]), fingerprint_v2(vec![nullable]));
}

#[test]
fn v2_binds_row_and_column_order() {
    let ordered = RecordBatch::try_from_iter(vec![
        (
            "left",
            Arc::new(Int64Array::from(vec![1_i64, 2])) as ArrayRef,
        ),
        (
            "right",
            Arc::new(Int64Array::from(vec![10_i64, 20])) as ArrayRef,
        ),
    ])
    .unwrap();
    let rows_reversed = RecordBatch::try_from_iter(vec![
        (
            "left",
            Arc::new(Int64Array::from(vec![2_i64, 1])) as ArrayRef,
        ),
        (
            "right",
            Arc::new(Int64Array::from(vec![20_i64, 10])) as ArrayRef,
        ),
    ])
    .unwrap();
    let columns_reversed = RecordBatch::try_from_iter(vec![
        (
            "right",
            Arc::new(Int64Array::from(vec![10_i64, 20])) as ArrayRef,
        ),
        (
            "left",
            Arc::new(Int64Array::from(vec![1_i64, 2])) as ArrayRef,
        ),
    ])
    .unwrap();

    let baseline = fingerprint_v2(vec![ordered]);
    assert_ne!(baseline, fingerprint_v2(vec![rows_reversed]));
    assert_ne!(baseline, fingerprint_v2(vec![columns_reversed]));
}

#[test]
fn v2_binds_only_explicitly_selected_metadata() {
    let field = |unit: &str, ignored: &str| {
        Field::new("id", DataType::Int64, false).with_metadata(HashMap::from([
            ("unit".to_string(), unit.to_string()),
            ("ignored".to_string(), ignored.to_string()),
        ]))
    };
    let batch = |field: Field| {
        RecordBatch::try_new(
            Arc::new(Schema::new(vec![field])),
            vec![Arc::new(Int64Array::from(vec![1_i64, 2])) as ArrayRef],
        )
        .unwrap()
    };
    let options = FingerprintOptions::new(FingerprintVersion::V2).with_metadata_key("unit");
    let run = |field| {
        fingerprint_reader_with_options(reader_from_batches(vec![batch(field)]), &options).unwrap()
    };

    let baseline = run(field("milliseconds", "first"));
    assert_ne!(baseline, run(field("seconds", "first")));
    assert_eq!(
        baseline,
        run(field("milliseconds", "changed-but-unselected"))
    );
}
