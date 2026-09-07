"""Accept one delivered file under an explicit policy, then verify the bundle offline.

Nothing is uploaded and nothing is repaired. The file is read once with settings the
caller states, and the answer is a decision an application can act on.
"""

import json
import tempfile
from pathlib import Path

import proofframe as pf

CONTRACT = {
    "version": "proofframe.contract.v1",
    "columns": {
        "order_id": {"required": True, "unique": True, "not_null": True},
        "amount": {"not_null": True, "min": 0.0},
    },
}

# A European export: semicolon separated, comma as the decimal mark, empty cells
# spelled "NULL". None of this is guessed; it is declared and recorded.
CSV_OPTIONS = {
    "delimiter": ";",
    "decimal_point": ",",
    "encoding": "utf-8",
    "null_values": ["NULL"],
    "column_types": {"order_id": "int64", "amount": "float64"},
}

# Zero violations is not enough on its own. If the amount column carried no value
# for its rules to check, the file has not been shown to be right about amounts.
POLICY = {"max_violations": 0, "minimum_evaluated": {"amount": 3}}


def main() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        delivery = root / "orders.csv"
        delivery.write_text(
            "order_id;amount\n1001;1234,50\n1002;99,00\n1003;7,25\n",
            encoding="utf-8",
        )

        bundle = pf.accept_file(
            delivery,
            CONTRACT,
            policy=POLICY,
            csv_options=CSV_OPTIONS,
            output=root / "acceptance.json",
        )
        decision = bundle["payload"]["decision"]
        print(json.dumps(decision, indent=2))
        assert decision["status"] == "accepted"
        assert decision["evaluated"]["amount"] == 3

        # The bundle verifies without the data, the contract file, or this script.
        # Verification answers whether the bundle is intact, not what the decision
        # was: the decision is read from the payload it just vouched for.
        saved = json.loads((root / "acceptance.json").read_text(encoding="utf-8"))
        assert pf.verify_acceptance(saved)["valid"]
        assert saved["payload"]["decision"]["status"] == "accepted"
        for identity in ("contract_id", "policy_id", "read_settings_id"):
            assert bundle["payload"][identity]

        # Editing any bound part invalidates the bundle, including the settings
        # the file was read with.
        tampered = json.loads(json.dumps(saved))
        tampered["payload"]["read_settings"]["csv"]["decimal_point"] = "."
        assert not pf.verify_acceptance(tampered)["valid"]

        # A file the policy does not accept says so, and says why.
        short = root / "short.csv"
        short.write_text("order_id;amount\n2001;10,00\n", encoding="utf-8")
        rejected = pf.accept_file(short, CONTRACT, policy=POLICY, csv_options=CSV_OPTIONS)
        assert rejected["payload"]["decision"]["status"] == "rejected"
        assert rejected["payload"]["decision"]["reasons"] == ["minimum_evaluated:amount"]

        # A file that cannot be read is unknown. It is never quietly accepted.
        unreadable = pf.accept_file(
            root / "never-delivered.csv", CONTRACT, policy=POLICY, csv_options=CSV_OPTIONS
        )
        assert unreadable["payload"]["decision"]["status"] == "unknown"
        print("rejected:", rejected["payload"]["decision"]["reasons"])
        print("unknown:", unreadable["payload"]["decision"]["reasons"])


if __name__ == "__main__":
    main()
