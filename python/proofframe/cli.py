"""Streaming ProofFrame command-line interface."""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
import warnings
from collections.abc import Mapping
from pathlib import Path
from typing import Any

import pyarrow as pa
import pyarrow.csv as arrow_csv
from pyarrow import parquet

from .api import check, diff, fingerprint, profile, sign_receipt, verify_receipt
from .errors import ContractError, ProofFrameError, ResourceLimitError

DEFAULT_BATCH_SIZE = 65_536
DEFAULT_MEMORY = 512 * 1024 * 1024
DEFAULT_TEMP = 4 * 1024 * 1024 * 1024
DEFAULT_OUTPUT_RECORDS = 100_000
DEFAULT_SAMPLES = 100


def _open_reader(path: str | Path, batch_size: int = DEFAULT_BATCH_SIZE) -> pa.RecordBatchReader:
    """Open a bounded Arrow stream without constructing a full in-memory table."""
    source = Path(path)
    if batch_size <= 0:
        raise ValueError("batch_size must be positive")
    if source.suffix.lower() == ".csv":
        return arrow_csv.open_csv(
            source,
            read_options=arrow_csv.ReadOptions(block_size=max(batch_size, 1_024)),
        )
    if source.suffix.lower() == ".parquet":
        parquet_file = parquet.ParquetFile(source)
        batches = parquet_file.iter_batches(batch_size=batch_size)
        return pa.RecordBatchReader.from_batches(parquet_file.schema_arrow, batches)
    raise ValueError(f"Unsupported input: {source}. Use .csv or .parquet")


def _read(path: str) -> pa.RecordBatchReader:
    """0.4 private-helper compatibility alias; returns a streaming reader."""
    return _open_reader(path)


def _parse_bytes(value: str) -> int:
    normalized = value.strip().lower().replace(" ", "")
    suffixes = {
        "gib": 1024**3,
        "gb": 1000**3,
        "mib": 1024**2,
        "mb": 1000**2,
        "kib": 1024,
        "kb": 1000,
        "b": 1,
    }
    multiplier = 1
    for suffix, candidate in suffixes.items():
        if normalized.endswith(suffix):
            normalized = normalized[: -len(suffix)]
            multiplier = candidate
            break
    try:
        amount = int(normalized)
    except ValueError as error:
        raise argparse.ArgumentTypeError(f"invalid byte size: {value}") from error
    if amount < 0:
        raise argparse.ArgumentTypeError("byte sizes cannot be negative")
    return amount * multiplier


def _positive_int(value: str) -> int:
    try:
        parsed = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError(f"expected an integer, got {value!r}") from error
    if parsed <= 0:
        raise argparse.ArgumentTypeError("value must be positive")
    return parsed


def _non_negative_int(value: str) -> int:
    try:
        parsed = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError(f"expected an integer, got {value!r}") from error
    if parsed < 0:
        raise argparse.ArgumentTypeError("value cannot be negative")
    return parsed


def _add_stream_options(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--batch-size", type=_positive_int, default=DEFAULT_BATCH_SIZE)


def _add_resource_options(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--max-memory", type=_parse_bytes, default=DEFAULT_MEMORY)
    parser.add_argument("--max-temp", type=_parse_bytes, default=DEFAULT_TEMP)
    parser.add_argument(
        "--max-output-records",
        type=_non_negative_int,
        default=DEFAULT_OUTPUT_RECORDS,
    )
    parser.add_argument("--max-samples", type=_non_negative_int, default=DEFAULT_SAMPLES)


def _add_check_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("path")
    parser.add_argument("--contract", required=True)
    _add_stream_options(parser)
    _add_resource_options(parser)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="proofframe",
        description="Bounded, Arrow-native data contracts and evidence",
    )
    commands = parser.add_subparsers(dest="command", required=True)

    check_parser = commands.add_parser("check", help="compile and enforce a JSON contract")
    _add_check_arguments(check_parser)

    fingerprint_parser = commands.add_parser(
        "fingerprint", help="compute a canonical dataset fingerprint"
    )
    fingerprint_parser.add_argument("path")
    fingerprint_parser.add_argument(
        "--fingerprint-version", choices=("v1", "v2"), default="v2"
    )
    _add_stream_options(fingerprint_parser)

    diff_parser = commands.add_parser("diff", help="stream an exact keyed dataset diff")
    diff_parser.add_argument("before")
    diff_parser.add_argument("after")
    diff_parser.add_argument("--key", action="append", required=True)
    diff_parser.add_argument("--output")
    diff_parser.add_argument("--output-format", choices=("jsonl", "arrow"), default="jsonl")
    _add_stream_options(diff_parser)
    _add_resource_options(diff_parser)

    sign_parser = commands.add_parser("sign", help="sign a JSON report with Ed25519")
    sign_parser.add_argument("report")
    sign_parser.add_argument("--private-key", required=True)
    sign_parser.add_argument("--output")

    verify_parser = commands.add_parser("verify", help="verify a signed JSON receipt")
    verify_parser.add_argument("receipt")

    profile_parser = commands.add_parser("profile", help="deprecated 0.4 profiling alias")
    profile_parser.add_argument("path")
    profile_parser.add_argument("--distinct", choices=("none", "exact"), default="exact")
    _add_stream_options(profile_parser)

    validate_parser = commands.add_parser("validate", help="deprecated alias for check")
    _add_check_arguments(validate_parser)
    return parser


def _load_mapping(path: str | Path, label: str) -> dict[str, Any]:
    value = json.loads(Path(path).read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise TypeError(f"{label} must be a JSON object")
    return value


def _write_json_atomic(path: str | Path, value: Mapping[str, Any]) -> None:
    target = Path(path)
    if not target.parent.is_dir():
        raise ValueError(f"Output directory does not exist: {target.parent}")
    temporary_name: str | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            newline="\n",
            dir=target.parent,
            prefix=f".{target.name}.",
            suffix=".tmp",
            delete=False,
        ) as temporary:
            temporary_name = temporary.name
            json.dump(value, temporary, indent=2, sort_keys=True)
            temporary.write("\n")
            temporary.flush()
            os.fsync(temporary.fileno())
        os.replace(temporary_name, target)
        temporary_name = None
    finally:
        if temporary_name is not None:
            Path(temporary_name).unlink(missing_ok=True)


def _check(args: argparse.Namespace) -> dict[str, Any]:
    contract = _load_mapping(args.contract, "contract")
    return check(
        _open_reader(args.path, args.batch_size),
        contract,
        max_memory=args.max_memory,
        max_temp=args.max_temp,
        max_output_records=args.max_output_records,
        max_samples=args.max_samples,
    )


def _execute(args: argparse.Namespace) -> tuple[dict[str, Any], int]:
    if args.command == "check":
        result = _check(args)
        return result, 0 if result["valid"] else 1
    if args.command == "fingerprint":
        digest = fingerprint(
            _open_reader(args.path, args.batch_size),
            version=args.fingerprint_version,
        )
        return {"fingerprint": digest, "version": args.fingerprint_version}, 0
    if args.command == "diff":
        result = diff(
            _open_reader(args.before, args.batch_size),
            _open_reader(args.after, args.batch_size),
            keys=args.key,
            max_memory=args.max_memory,
            max_temp=args.max_temp,
            max_output_records=args.max_output_records,
            max_samples=args.max_samples,
            output=args.output,
            output_format=args.output_format,
        )
        return result, 0
    if args.command == "sign":
        report = _load_mapping(args.report, "report")
        result = sign_receipt(report, private_key=args.private_key)
        if args.output:
            _write_json_atomic(args.output, result)
        return result, 0
    if args.command == "verify":
        receipt = _load_mapping(args.receipt, "receipt")
        result = verify_receipt(receipt)
        return result, 0 if result["valid"] else 1
    if args.command == "profile":
        warnings.warn(
            "The profile command is retained for 0.5 compatibility; use fingerprint or check.",
            DeprecationWarning,
            stacklevel=2,
        )
        return profile(_open_reader(args.path, args.batch_size), distinct=args.distinct), 0
    if args.command == "validate":
        warnings.warn(
            "The validate command is retained for 0.5 compatibility; use check.",
            DeprecationWarning,
            stacklevel=2,
        )
        result = _check(args)
        return result, 0 if result["valid"] else 1
    raise ValueError(f"Unsupported command: {args.command}")


def _error_payload(error: BaseException) -> str:
    payload: dict[str, Any] = {"error": str(error)}
    code = getattr(error, "code", None)
    if code is not None:
        payload["code"] = code
    return json.dumps(payload, indent=2, sort_keys=True)


def main(argv: list[str] | None = None) -> None:
    try:
        args = _parser().parse_args(argv)
        result, exit_code = _execute(args)
    except SystemExit:
        raise
    except ResourceLimitError as error:
        print(_error_payload(error), file=sys.stderr)
        raise SystemExit(4) from error
    except ContractError as error:
        print(_error_payload(error), file=sys.stderr)
        raise SystemExit(2) from error
    except ProofFrameError as error:
        print(_error_payload(error), file=sys.stderr)
        raise SystemExit(3) from error
    except (OSError, ValueError, json.JSONDecodeError, TypeError) as error:
        print(_error_payload(error), file=sys.stderr)
        raise SystemExit(2) from error

    print(json.dumps(result, indent=2, sort_keys=True))
    if exit_code:
        raise SystemExit(exit_code)


if __name__ == "__main__":
    main()
