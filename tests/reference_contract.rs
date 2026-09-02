mod support;

use std::sync::Arc;

use arrow::array::{ArrayRef, Int32Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::error::ArrowError;
use arrow::record_batch::{RecordBatch, RecordBatchIterator, RecordBatchReader};
use proofframe::{
    CompiledContract, ContractDocument, ExecutionOptions, ReferenceBindings, execute_reader,
    execute_reader_with_references,
};
use serde_json::{Value, json};

use support::reader_from_batches;

fn compile_json(source: Value, schema: &Schema) -> CompiledContract {
    let document = ContractDocument::from_json(&source.to_string()).unwrap();
    CompiledContract::compile_document(&document, schema).unwrap()
}

fn boxed(batches: Vec<RecordBatch>) -> Box<dyn RecordBatchReader + Send> {
    let schema = batches.first().expect("at least one batch").schema();
    Box::new(RecordBatchIterator::new(
        batches.into_iter().map(Ok::<RecordBatch, ArrowError>),
        schema,
    ))
}

fn bind(name: &str, batches: Vec<RecordBatch>) -> ReferenceBindings {
    let mut bindings = ReferenceBindings::new();
    bindings.insert(name, boxed(batches));
    bindings
}

fn orders_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![Field::new(
        "customer_id",
        DataType::Int64,
        true,
    )]))
}

fn orders(values: Vec<Option<i64>>) -> RecordBatch {
    RecordBatch::try_new(
        orders_schema(),
        vec![Arc::new(Int64Array::from(values)) as ArrayRef],
    )
    .unwrap()
}

fn customers_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]))
}

fn customers(values: Vec<i64>) -> RecordBatch {
    RecordBatch::try_new(
        customers_schema(),
        vec![Arc::new(Int64Array::from(values)) as ArrayRef],
    )
    .unwrap()
}

fn single_key_contract() -> Value {
    json!({
        "version": "proofframe.contract.v2",
        "dataset_rules": {
            "references": [{
                "name": "orders_customer_fk",
                "columns": ["customer_id"],
                "reference": "customers",
                "reference_columns": ["id"]
            }]
        }
    })
}

#[test]
fn every_key_present_in_the_reference_is_valid() {
    let schema = orders_schema();
    let plan = compile_json(single_key_contract(), schema.as_ref());

    let report = execute_reader_with_references(
        reader_from_batches(vec![orders(vec![Some(1), Some(2), Some(1)])]),
        &plan,
        &ExecutionOptions::default(),
        bind("customers", vec![customers(vec![1, 2, 3])]),
    )
    .unwrap();

    assert!(report.valid);
    assert_eq!(report.violation_count, 0);
    let outcome = &report.references[0];
    assert_eq!(outcome.name, "orders_customer_fk");
    assert_eq!(outcome.reference_rows, 3);
    assert_eq!(outcome.reference_distinct_keys, 3);
    assert_eq!(outcome.checked_distinct_keys, 2);
    assert_eq!(outcome.missing_distinct_keys, 0);
}

#[test]
fn a_missing_key_reports_the_first_row_that_carried_it() {
    let schema = orders_schema();
    let plan = compile_json(single_key_contract(), schema.as_ref());

    // 99 first appears at row 1 and repeats at row 3; 77 appears at row 4.
    let report = execute_reader_with_references(
        reader_from_batches(vec![orders(vec![
            Some(1),
            Some(99),
            Some(2),
            Some(99),
            Some(77),
        ])]),
        &plan,
        &ExecutionOptions::default(),
        bind("customers", vec![customers(vec![1, 2])]),
    )
    .unwrap();

    assert!(!report.valid);
    // Two distinct keys are absent, not the three rows that carried them.
    assert_eq!(report.violation_count, 2);
    assert_eq!(report.references[0].missing_distinct_keys, 2);
    assert_eq!(report.references[0].checked_distinct_keys, 4);
    let rows = report
        .findings
        .iter()
        .filter(|finding| finding.rule == "references")
        .map(|finding| finding.row)
        .collect::<Vec<_>>();
    assert_eq!(rows, vec![Some(4), Some(1)]);
    assert_eq!(report.findings[0].column, "customer_id");
}

#[test]
fn keys_are_resolved_across_record_batch_boundaries() {
    let schema = orders_schema();
    let plan = compile_json(single_key_contract(), schema.as_ref());

    let report = execute_reader_with_references(
        reader_from_batches(vec![orders(vec![Some(1)]), orders(vec![Some(2)])]),
        &plan,
        &ExecutionOptions::default(),
        // The reference is split too, so neither side can be resolved batch by batch.
        bind("customers", vec![customers(vec![1]), customers(vec![2])]),
    )
    .unwrap();

    assert!(report.valid);
    assert_eq!(report.references[0].reference_distinct_keys, 2);
}

#[test]
fn the_reference_dataset_is_fingerprinted_into_the_report() {
    let schema = orders_schema();
    let plan = compile_json(single_key_contract(), schema.as_ref());
    let run = |rows: Vec<i64>| {
        execute_reader_with_references(
            reader_from_batches(vec![orders(vec![Some(1)])]),
            &plan,
            &ExecutionOptions::default(),
            bind("customers", vec![customers(rows)]),
        )
        .unwrap()
        .references
        .remove(0)
        .reference_fingerprint
    };

    let first = run(vec![1, 2]);
    assert!(first.starts_with("pf-fp-v2:"));
    assert_eq!(first, run(vec![1, 2]));
    // A pass against a different reference is a different claim and must not look identical.
    assert_ne!(first, run(vec![1, 2, 3]));
}

#[test]
fn a_rule_without_a_bound_dataset_fails_closed() {
    let schema = orders_schema();
    let plan = compile_json(single_key_contract(), schema.as_ref());

    let error = execute_reader(
        reader_from_batches(vec![orders(vec![Some(1)])]),
        &plan,
        &ExecutionOptions::default(),
    )
    .unwrap_err();

    assert_eq!(error.code().as_str(), "PF_REFERENCE_UNBOUND");
}

#[test]
fn a_bound_dataset_no_rule_uses_fails_closed() {
    let schema = orders_schema();
    let plan = compile_json(single_key_contract(), schema.as_ref());
    let mut bindings = bind("customers", vec![customers(vec![1])]);
    bindings.insert("custmoers", boxed(vec![customers(vec![1])]));

    let error = execute_reader_with_references(
        reader_from_batches(vec![orders(vec![Some(1)])]),
        &plan,
        &ExecutionOptions::default(),
        bindings,
    )
    .unwrap_err();

    assert_eq!(error.code().as_str(), "PF_REFERENCE_UNBOUND");
}

#[test]
fn mismatched_key_types_are_rejected_instead_of_never_matching() {
    let schema = orders_schema();
    let plan = compile_json(single_key_contract(), schema.as_ref());
    let narrow = Arc::new(Schema::new(vec![Field::new("id", DataType::Int32, false)]));
    let batch = RecordBatch::try_new(
        narrow,
        vec![Arc::new(Int32Array::from(vec![1, 2])) as ArrayRef],
    )
    .unwrap();

    let error = execute_reader_with_references(
        reader_from_batches(vec![orders(vec![Some(1)])]),
        &plan,
        &ExecutionOptions::default(),
        bind("customers", vec![batch]),
    )
    .unwrap_err();

    assert_eq!(error.code().as_str(), "PF_CONTRACT_TYPE_MISMATCH");
}

#[test]
fn an_absent_reference_column_is_reported_before_the_subject_is_scanned() {
    let schema = orders_schema();
    let plan = compile_json(single_key_contract(), schema.as_ref());
    let other = Arc::new(Schema::new(vec![Field::new(
        "customer_id",
        DataType::Int64,
        false,
    )]));
    let batch = RecordBatch::try_new(
        other,
        vec![Arc::new(Int64Array::from(vec![1_i64])) as ArrayRef],
    )
    .unwrap();

    let error = execute_reader_with_references(
        reader_from_batches(vec![orders(vec![Some(1)])]),
        &plan,
        &ExecutionOptions::default(),
        bind("customers", vec![batch]),
    )
    .unwrap_err();

    assert_eq!(error.code().as_str(), "PF_MISSING_COLUMN");
}

#[test]
fn null_keys_are_skipped_by_default_and_rejected_on_request() {
    let schema = orders_schema();
    let rows = vec![orders(vec![Some(1), None, None])];

    let skipping = execute_reader_with_references(
        reader_from_batches(rows.clone()),
        &compile_json(single_key_contract(), schema.as_ref()),
        &ExecutionOptions::default(),
        bind("customers", vec![customers(vec![1])]),
    )
    .unwrap();
    assert!(skipping.valid);
    assert_eq!(skipping.references[0].checked_distinct_keys, 1);

    let mut strict = single_key_contract();
    strict["dataset_rules"]["references"][0]["nulls"] = json!("reject");
    let rejecting = execute_reader_with_references(
        reader_from_batches(rows),
        &compile_json(strict, schema.as_ref()),
        &ExecutionOptions::default(),
        bind("customers", vec![customers(vec![1])]),
    )
    .unwrap();
    assert!(!rejecting.valid);
    assert_eq!(rejecting.violation_count, 2);
    assert_eq!(
        rejecting
            .findings
            .iter()
            .map(|finding| finding.row)
            .collect::<Vec<_>>(),
        vec![Some(1), Some(2)]
    );
}

#[test]
fn a_null_in_the_reference_is_not_an_identity_a_local_null_can_match() {
    let schema = orders_schema();
    let nullable = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, true)]));
    let batch = RecordBatch::try_new(
        nullable,
        vec![Arc::new(Int64Array::from(vec![None, Some(1)])) as ArrayRef],
    )
    .unwrap();

    let report = execute_reader_with_references(
        reader_from_batches(vec![orders(vec![Some(1), None])]),
        &compile_json(single_key_contract(), schema.as_ref()),
        &ExecutionOptions::default(),
        bind("customers", vec![batch]),
    )
    .unwrap();

    assert!(report.valid);
    assert_eq!(report.references[0].reference_rows, 2);
    assert_eq!(report.references[0].reference_distinct_keys, 1);
}

#[test]
fn composite_keys_pair_by_position_and_ignore_column_names() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("region", DataType::Utf8, false),
        Field::new("sku", DataType::Int64, false),
    ]));
    let subject = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(vec!["eu", "eu", "us"])) as ArrayRef,
            Arc::new(Int64Array::from(vec![1_i64, 2, 1])) as ArrayRef,
        ],
    )
    .unwrap();
    // The catalogue names both key columns differently.
    let catalogue_schema = Arc::new(Schema::new(vec![
        Field::new("market", DataType::Utf8, false),
        Field::new("product", DataType::Int64, false),
    ]));
    let catalogue = RecordBatch::try_new(
        catalogue_schema,
        vec![
            Arc::new(StringArray::from(vec!["eu", "eu"])) as ArrayRef,
            Arc::new(Int64Array::from(vec![1_i64, 2])) as ArrayRef,
        ],
    )
    .unwrap();
    let plan = compile_json(
        json!({
            "version": "proofframe.contract.v2",
            "dataset_rules": {
                "references": [{
                    "name": "sku_in_catalogue",
                    "columns": ["region", "sku"],
                    "reference": "catalogue",
                    "reference_columns": ["market", "product"]
                }]
            }
        }),
        schema.as_ref(),
    );

    let report = execute_reader_with_references(
        reader_from_batches(vec![subject]),
        &plan,
        &ExecutionOptions::default(),
        bind("catalogue", vec![catalogue]),
    )
    .unwrap();

    // ("us", 1) is absent even though "us" and 1 each appear on their own.
    assert_eq!(report.violation_count, 1);
    assert_eq!(report.findings[0].row, Some(2));
    assert_eq!(report.references[0].missing_distinct_keys, 1);
}

#[test]
fn an_empty_reference_dataset_makes_every_key_a_violation() {
    let schema = orders_schema();
    let plan = compile_json(single_key_contract(), schema.as_ref());
    let empty = RecordBatch::new_empty(customers_schema());

    let report = execute_reader_with_references(
        reader_from_batches(vec![orders(vec![Some(1), Some(2)])]),
        &plan,
        &ExecutionOptions::default(),
        bind("customers", vec![empty]),
    )
    .unwrap();

    assert_eq!(report.violation_count, 2);
    assert_eq!(report.references[0].reference_rows, 0);
    assert_eq!(report.references[0].missing_distinct_keys, 2);
}

#[test]
fn two_rules_over_the_same_dataset_and_key_share_one_scan() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("buyer_id", DataType::Int64, false),
        Field::new("seller_id", DataType::Int64, false),
    ]));
    let subject = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(vec![1_i64, 2])) as ArrayRef,
            Arc::new(Int64Array::from(vec![1_i64, 9])) as ArrayRef,
        ],
    )
    .unwrap();
    let plan = compile_json(
        json!({
            "version": "proofframe.contract.v2",
            "dataset_rules": {
                "references": [
                    {
                        "name": "buyer_fk",
                        "columns": ["buyer_id"],
                        "reference": "customers",
                        "reference_columns": ["id"]
                    },
                    {
                        "name": "seller_fk",
                        "columns": ["seller_id"],
                        "reference": "customers",
                        "reference_columns": ["id"]
                    }
                ]
            }
        }),
        schema.as_ref(),
    );

    let report = execute_reader_with_references(
        reader_from_batches(vec![subject]),
        &plan,
        &ExecutionOptions::default(),
        bind("customers", vec![customers(vec![1, 2])]),
    )
    .unwrap();

    assert_eq!(report.references.len(), 2);
    // One reader served both rules, so both carry the same reference identity.
    assert_eq!(
        report.references[0].reference_fingerprint,
        report.references[1].reference_fingerprint
    );
    assert_eq!(report.references[0].missing_distinct_keys, 0);
    assert_eq!(report.references[1].missing_distinct_keys, 1);
}

#[test]
fn an_empty_reference_list_does_not_change_the_compiled_plan_digest() {
    // The digest appends references only when a plan carries them. An absent list and an
    // empty one execute identically, so they must stay one identity: otherwise every
    // contract written before this rule existed would silently stop matching the receipts
    // already issued for it.
    let schema = orders_schema();
    let absent = compile_json(
        json!({
            "version": "proofframe.contract.v2",
            "columns": {"customer_id": {"not_null": true}}
        }),
        schema.as_ref(),
    );
    let empty = compile_json(
        json!({
            "version": "proofframe.contract.v2",
            "columns": {"customer_id": {"not_null": true}},
            "dataset_rules": {"references": []}
        }),
        schema.as_ref(),
    );

    assert_eq!(
        absent.compiled_plan_digest().unwrap(),
        empty.compiled_plan_digest().unwrap()
    );
}

#[test]
fn adding_a_reference_rule_changes_the_compiled_plan_digest() {
    let schema = orders_schema();
    let without = compile_json(
        json!({"version": "proofframe.contract.v2", "columns": {}}),
        schema.as_ref(),
    );
    let with = compile_json(single_key_contract(), schema.as_ref());

    assert_ne!(
        without.compiled_plan_digest().unwrap(),
        with.compiled_plan_digest().unwrap()
    );
}
