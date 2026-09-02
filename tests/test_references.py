import json

import proofframe as pf
import pyarrow as pa
import pytest
from proofframe.cli import main
from proofframe.errors import ContractError

CONTRACT = {
    "version": "proofframe.contract.v2",
    "dataset_rules": {
        "references": [
            {
                "name": "orders_customer_fk",
                "columns": ["customer_id"],
                "reference": "customers",
                "reference_columns": ["id"],
            }
        ]
    },
}


def test_python_reference_rule_reports_missing_keys_and_the_reference_identity():
    orders = pa.table({"customer_id": [1, 99, 2, 99]})
    customers = pa.table({"id": [1, 2]})

    report = pf.check(orders, CONTRACT, references={"customers": customers})

    assert report["valid"] is False
    # One distinct key is absent, not the two rows that carried it.
    assert report["violation_count"] == 1
    assert report["findings"][0]["rule"] == "references"
    assert report["findings"][0]["row"] == 1
    outcome = report["references"][0]
    assert outcome["name"] == "orders_customer_fk"
    assert outcome["reference"] == "customers"
    assert outcome["reference_rows"] == 2
    assert outcome["missing_distinct_keys"] == 1
    assert outcome["reference_fingerprint"].startswith("pf-fp-v2:")


def test_python_reference_rule_passes_when_every_key_resolves():
    report = pf.check(
        pa.table({"customer_id": [1, 2, 2]}),
        CONTRACT,
        references={"customers": pa.table({"id": [1, 2, 3]})},
    )

    assert report["valid"] is True
    assert report["references"][0]["missing_distinct_keys"] == 0
    assert report["references"][0]["checked_distinct_keys"] == 2


@pytest.mark.parametrize(
    "operation",
    [
        pf.check,
        pf.check_with_evidence,
        # The partition entry points take a sequence, so these two wrap the table.
        lambda data, contract, **options: pf.check_partitions([data], contract, **options),
        lambda data, contract, **options: pf.check_partitions_with_evidence(
            [data], contract, **options
        ),
    ],
)
def test_an_unbound_reference_fails_closed_at_every_entry_point(operation):
    with pytest.raises(ContractError) as error:
        operation(pa.table({"customer_id": [1]}), CONTRACT)

    assert error.value.code == "PF_REFERENCE_UNBOUND"


def test_a_bound_dataset_no_rule_uses_fails_closed():
    customers = pa.table({"id": [1]})

    with pytest.raises(ContractError) as error:
        pf.check(
            pa.table({"customer_id": [1]}),
            CONTRACT,
            references={"customers": customers, "custmoers": pa.table({"id": [1]})},
        )

    assert error.value.code == "PF_REFERENCE_UNBOUND"


def test_evidence_carries_the_reference_the_keys_were_resolved_against():
    checked = pf.check_with_evidence(
        pa.table({"customer_id": [1]}),
        CONTRACT,
        references={"customers": pa.table({"id": [1]})},
    )

    outcome = checked["report"]["references"][0]
    assert outcome["reference_fingerprint"].startswith("pf-fp-v2:")
    # The report the evidence digests is the one carrying the reference identity.
    assert checked["evidence"]["result"]["valid"] is True


def test_partitions_resolve_keys_against_the_whole_reference():
    # 99 is absent; splitting the subject must not change that verdict.
    report = pf.check_partitions(
        [pa.table({"customer_id": [1]}), pa.table({"customer_id": [99]})],
        CONTRACT,
        references={"customers": pa.table({"id": [1, 2]})},
    )

    assert report["valid"] is False
    assert report["references"][0]["missing_distinct_keys"] == 1


def test_a_contract_without_reference_rules_still_reports_an_empty_list():
    report = pf.check(pa.table({"customer_id": [1]}), {"columns": {}})

    assert report["references"] == []


def test_cli_binds_reference_datasets_by_name(tmp_path, capsys):
    orders = tmp_path / "orders.csv"
    orders.write_text("customer_id\n1\n99\n", encoding="utf-8")
    customers = tmp_path / "customers.csv"
    customers.write_text("id\n1\n2\n", encoding="utf-8")
    contract = tmp_path / "contract.json"
    contract.write_text(json.dumps(CONTRACT), encoding="utf-8")

    with pytest.raises(SystemExit) as exit_code:
        main(
            [
                "check",
                str(orders),
                "--contract",
                str(contract),
                "--reference",
                f"customers={customers}",
            ]
        )

    # A contract violation is exit code 1, not an error.
    assert exit_code.value.code == 1
    report = json.loads(capsys.readouterr().out)
    assert report["references"][0]["missing_distinct_keys"] == 1


def test_cli_rejects_a_reference_binding_without_a_name(tmp_path):
    contract = tmp_path / "contract.json"
    contract.write_text(json.dumps(CONTRACT), encoding="utf-8")

    with pytest.raises(SystemExit):
        main(
            [
                "check",
                str(tmp_path / "orders.csv"),
                "--contract",
                str(contract),
                "--reference",
                "customers.csv",
            ]
        )
