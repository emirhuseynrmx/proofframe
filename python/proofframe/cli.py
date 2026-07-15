"""ProofFrame command-line interface."""

from __future__ import annotations

import argparse
import json
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
    raise SystemExit(f"Unsupported input: {source}. Use .csv or .parquet")


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
    args = _parser().parse_args()
    if args.command == "profile":
        result = profile(_read(args.path))
    elif args.command == "validate":
        contract = json.loads(Path(args.contract).read_text(encoding="utf-8"))
        result = validate(_read(args.path), contract)
    else:
        result = diff(_read(args.before), _read(args.after), keys=args.key)
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
