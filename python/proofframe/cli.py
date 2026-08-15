"""ProofFrame command-line interface."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

import pyarrow.csv as arrow_csv
import pyarrow.parquet as parquet

from .api import diff, profile, validate


def _read(path: str) -> Any:
    source = Path(path)
    if source.suffix.lower() == ".parquet":
        return parquet.read_table(source)
    if source.suffix.lower() == ".csv":
        return arrow_csv.read_csv(source)
    raise ValueError(f"Unsupported input: {source}. Use .csv or .parquet")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="proofframe", description="Ruff for data")
    commands = parser.add_subparsers(dest="command", required=True)
    profile_parser = commands.add_parser("profile", help="profile and fingerprint a dataset")
    profile_parser.add_argument("path")
    validate_parser = commands.add_parser("validate", help="enforce a JSON data contract")
    validate_parser.add_argument("path")
    validate_parser.add_argument("--contract", required=True)
    diff_parser = commands.add_parser("diff", help="diff two datasets by key")
    diff_parser.add_argument("before")
    diff_parser.add_argument("after")
    diff_parser.add_argument("--key", action="append", required=True)
    return parser


def main() -> None:
    try:
        args = _parser().parse_args()
        if args.command == "profile":
            result = profile(_read(args.path))
        elif args.command == "validate":
            contract = json.loads(Path(args.contract).read_text(encoding="utf-8"))
            result = validate(_read(args.path), contract)
        else:
            result = diff(_read(args.before), _read(args.after), keys=args.key)
    except SystemExit:
        raise
    except (OSError, ValueError, json.JSONDecodeError, TypeError) as error:
        print(json.dumps({"error": str(error)}, indent=2, sort_keys=True), file=sys.stderr)
        raise SystemExit(2) from error
    except Exception as error:
        print(json.dumps({"error": str(error)}, indent=2, sort_keys=True), file=sys.stderr)
        raise SystemExit(3) from error
    print(json.dumps(result, indent=2, sort_keys=True))
    if args.command == "validate" and not result["valid"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
