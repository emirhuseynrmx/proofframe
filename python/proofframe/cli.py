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

from .api import (
    check,
    check_with_evidence,
    diff,
    fingerprint,
    profile,
    sign_receipt,
    suggest_contract,
    verify_receipt,
)
from .errors import ContractError, ProofFrameError, ResourceLimitError

DEFAULT_BATCH_SIZE = 65_536
DEFAULT_MEMORY = 512 * 1024 * 1024
DEFAULT_TEMP = 4 * 1024 * 1024 * 1024
DEFAULT_OUTPUT_RECORDS = 100_000
DEFAULT_SAMPLES = 100
_KEY_OPTIONS = frozenset(("--private-key", "--expected-public-key"))


def _normalize_key_options(argv: list[str] | None) -> list[str]:
    """Bind URL-safe key values to their option before argparse classifies tokens.

    Ed25519 keys use URL-safe base64, whose alphabet includes ``-``. Argparse otherwise
    interprets a key beginning with that character as another option. The ``--name=value``
    form is unambiguous on every supported Python and operating system.
    """
    source = list(sys.argv[1:] if argv is None else argv)
    normalized: list[str] = []
    index = 0
    while index < len(source):
        token = source[index]
        if token in _KEY_OPTIONS and index + 1 < len(source):
            normalized.append(f"{token}={source[index + 1]}")
            index += 2
            continue
        normalized.append(token)
        index += 1
    return normalized


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
        "--spill",
        choices=("auto", "never"),
        default="auto",
        help="spill exact state when needed, or fail instead of writing data partitions",
    )
    parser.add_argument(
        "--max-output-records",
        type=_non_negative_int,
        default=DEFAULT_OUTPUT_RECORDS,
    )
    parser.add_argument("--max-samples", type=_non_negative_int, default=DEFAULT_SAMPLES)


def _reference_binding(value: str) -> tuple[str, str]:
    """Parse one `name=path` binding for a contract's reference rules."""
    name, separator, path = value.partition("=")
    if not separator or not name or not path:
        raise argparse.ArgumentTypeError(
            f"expected a reference binding as name=path, got {value!r}"
        )
    return name, path


def _add_check_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("path")
    parser.add_argument("--contract", required=True)
    parser.add_argument(
        "--reference",
        action="append",
        default=[],
        type=_reference_binding,
        metavar="NAME=PATH",
        help="bind a dataset to a referential integrity rule; repeatable",
    )
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

    evidence_parser = commands.add_parser(
        "evidence", help="check and fingerprint one stream into Evidence V2"
    )
    evidence_parser.add_argument("path")
    evidence_parser.add_argument("--contract", required=True)
    evidence_parser.add_argument(
        "--reference",
        action="append",
        default=[],
        type=_reference_binding,
        metavar="NAME=PATH",
        help="bind a dataset to a referential integrity rule; repeatable",
    )
    evidence_parser.add_argument("--output")
    _add_stream_options(evidence_parser)
    _add_resource_options(evidence_parser)

    sign_parser = commands.add_parser("sign", help="sign a V2 evidence envelope with Ed25519")
    sign_parser.add_argument("report")
    sign_parser.add_argument("--private-key", required=True)
    sign_parser.add_argument("--receipt-version", choices=("v1", "v2"), default="v2")
    sign_parser.add_argument("--output")

    verify_parser = commands.add_parser("verify", help="verify a signed JSON receipt")
    verify_parser.add_argument("receipt")
    verify_parser.add_argument("--expected-public-key")

    profile_parser = commands.add_parser("profile", help="deprecated 0.4 profiling alias")
    profile_parser.add_argument("path")
    profile_parser.add_argument("--distinct", choices=("none", "exact"), default="none")
    _add_stream_options(profile_parser)
    _add_resource_options(profile_parser)

    suggest_parser = commands.add_parser(
        "suggest", help="generate a review-required V2 contract draft"
    )
    suggest_parser.add_argument("path")
    suggest_parser.add_argument("--infer-uniqueness", action="store_true")
    suggest_parser.add_argument("--infer-categories", action="store_true")
    suggest_parser.add_argument("--max-categories", type=int, default=20)
    suggest_parser.add_argument("--infer-required", action="store_true")
    suggest_parser.add_argument("--no-infer-ranges", action="store_false", dest="infer_ranges")
    suggest_parser.add_argument("--range-tolerance", type=float, default=0.0)
    suggest_parser.add_argument(
        "--no-infer-row-count", action="store_false", dest="infer_row_count"
    )
    _add_stream_options(suggest_parser)
    _add_resource_options(suggest_parser)

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


def _references(args: argparse.Namespace) -> dict[str, Any]:
    """Open every bound reference dataset as its own stream.

    A repeated name would silently keep only the last binding, which is the kind of
    typo that makes a foreign key look checked when it was not.
    """
    streams: dict[str, Any] = {}
    for name, path in getattr(args, "reference", []) or []:
        if name in streams:
            raise ValueError(f"reference dataset `{name}` was bound more than once")
        streams[name] = _open_reader(path, args.batch_size)
    return streams


def _check(args: argparse.Namespace) -> dict[str, Any]:
    contract = _load_mapping(args.contract, "contract")
    return check(
        _open_reader(args.path, args.batch_size),
        contract,
        references=_references(args),
        max_memory=args.max_memory,
        max_temp=args.max_temp,
        max_output_records=args.max_output_records,
        max_samples=args.max_samples,
        spill=args.spill,
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
            spill=args.spill,
        )
        return result, 0
    if args.command == "evidence":
        checked = check_with_evidence(
            _open_reader(args.path, args.batch_size),
            _load_mapping(args.contract, "contract"),
            references=_references(args),
            max_memory=args.max_memory,
            max_temp=args.max_temp,
            max_output_records=args.max_output_records,
            max_samples=args.max_samples,
            spill=args.spill,
        )
        result = checked["evidence"]
        if args.output:
            _write_json_atomic(args.output, result)
        return result, 0
    if args.command == "sign":
        report = _load_mapping(args.report, "report")
        result = sign_receipt(
            report,
            private_key=args.private_key,
            receipt_version=args.receipt_version,
        )
        if args.output:
            _write_json_atomic(args.output, result)
        return result, 0
    if args.command == "verify":
        receipt = _load_mapping(args.receipt, "receipt")
        result = verify_receipt(receipt, expected_public_key=args.expected_public_key)
        return result, 0 if result["valid"] else 1
    if args.command == "profile":
        warnings.warn(
            "The profile command is retained for 0.5 compatibility; use fingerprint or check.",
            DeprecationWarning,
            stacklevel=2,
        )
        return profile(
            _open_reader(args.path, args.batch_size),
            distinct=args.distinct,
            max_memory=args.max_memory,
            max_temp=args.max_temp,
            spill=args.spill,
        ), 0
    if args.command == "suggest":
        return suggest_contract(
            _open_reader(args.path, args.batch_size),
            infer_uniqueness=args.infer_uniqueness,
            infer_categories=args.infer_categories,
            max_categories=args.max_categories,
            infer_required=args.infer_required,
            infer_ranges=args.infer_ranges,
            range_tolerance=args.range_tolerance,
            infer_row_count=args.infer_row_count,
            max_memory=args.max_memory,
            max_temp=args.max_temp,
            spill=args.spill,
        ), 0
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
        args = _parser().parse_args(_normalize_key_options(argv))
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
    except (OSError, ValueError, TypeError) as error:
        print(_error_payload(error), file=sys.stderr)
        raise SystemExit(2) from error

    print(json.dumps(result, indent=2, sort_keys=True))
    if exit_code:
        raise SystemExit(exit_code)


if __name__ == "__main__":
    main()
