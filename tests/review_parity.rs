//! The Rust renderer is the only renderer, so it has to produce what the Python
//! one produced before it was removed. The fixtures cover the cases where the two
//! languages could plausibly disagree: escaping, non-ASCII, truncation, and the
//! pretty-printed contract disclosure.

use std::fs;
use std::path::PathBuf;

use proofframe::{review_html, review_markdown};
use serde_json::Value;

const FIXTURES: [&str; 4] = ["passing", "hostile", "no_samples", "long_value"];

fn support(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/support/review_parity")
        .join(name)
}

fn read(name: &str) -> String {
    fs::read_to_string(support(name)).unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn columns(fixture: &Value) -> Vec<String> {
    fixture["column_names"]
        .as_array()
        .expect("column_names")
        .iter()
        .map(|name| name.as_str().expect("column name").to_owned())
        .collect()
}

fn diff(expected: &str, actual: &str) -> String {
    let at = expected
        .char_indices()
        .zip(actual.chars())
        .find(|((_, left), right)| left != right)
        .map_or(expected.len().min(actual.len()), |((index, _), _)| index);
    let window = |text: &str| {
        text.chars()
            .skip(at.saturating_sub(60))
            .take(160)
            .collect::<String>()
    };
    format!(
        "first difference at byte {at}\n  python: …{}…\n  rust:   …{}…",
        window(expected),
        window(actual)
    )
}

#[test]
fn the_review_renders_exactly_what_the_python_implementation_rendered() {
    for name in FIXTURES {
        let fixture: Value = serde_json::from_str(&read(&format!("{name}.json"))).expect("fixture");
        let names = columns(&fixture);
        let label = fixture["label"].as_str().expect("label");

        let html = review_html(
            &fixture["report"],
            &fixture["evidence"],
            &fixture["contract"],
            label,
            &names,
        )
        .expect("html");
        let expected_html = read(&format!("{name}.html"));
        assert_eq!(
            html,
            expected_html,
            "{name}.html: {}",
            diff(&expected_html, &html)
        );

        let markdown = review_markdown(&fixture["report"], &fixture["evidence"], label, &names)
            .expect("markdown");
        let expected_markdown = read(&format!("{name}.md"));
        assert_eq!(
            markdown,
            expected_markdown,
            "{name}.md: {}",
            diff(&expected_markdown, &markdown)
        );
    }
}

#[test]
fn a_report_that_is_not_a_report_is_refused_rather_than_half_rendered() {
    let evidence = serde_json::json!({
        "dataset": {"fingerprint_version": "v2", "fingerprint_digest": vec![0; 32]},
        "engine": {"version": "0.7.1"},
    });
    let contract = serde_json::json!({"columns": {}});

    for broken in [
        serde_json::json!({"findings": [], "rows": 1, "violation_count": 0}),
        serde_json::json!({"valid": true, "rows": 1, "violation_count": 0}),
        serde_json::json!({"valid": true, "findings": [], "violation_count": 0}),
    ] {
        assert!(
            review_html(&broken, &evidence, &contract, "x", &[]).is_err(),
            "rendered a report missing a required field: {broken}"
        );
    }
}

/// The one deliberate difference from the renderer this replaced.
///
/// Python disclosed the contract in the order its keys happened to be inserted.
/// Two runs over the same contract could therefore produce two different documents.
/// The keys are now sorted, so the disclosure is a function of the contract alone.
#[test]
fn the_disclosed_contract_is_ordered_by_key_not_by_insertion() {
    let evidence = serde_json::json!({
        "dataset": {"fingerprint_version": "v2", "fingerprint_digest": vec![0; 32]},
        "engine": {"version": "0.7.1"},
    });
    let report = serde_json::json!({
        "valid": true, "violation_count": 0, "rows": 1, "findings": [],
        "resources": {"max_temp_bytes": 2, "max_memory_bytes": 1},
        "metrics": {},
    });
    let contract = serde_json::json!({
        "columns": {"zebra": {"type": "utf8"}, "alpha": {"type": "int64"}},
    });

    let html = review_html(&report, &evidence, &contract, "x", &[]).expect("html");
    let disclosure = html.split("<pre>").nth(1).expect("contract disclosure");
    assert!(
        disclosure.find("alpha").unwrap() < disclosure.find("zebra").unwrap(),
        "contract keys were not sorted"
    );
    assert!(
        html.find("max memory bytes").unwrap() < html.find("max temp bytes").unwrap(),
        "resource keys were not sorted"
    );
}
