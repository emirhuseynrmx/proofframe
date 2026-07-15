"""Reproducible valid-data benchmark against Pandera and Great Expectations.

Setup and imports are excluded. Every framework checks the same three predicates:
`id` is non-null and unique, and `score` is between zero and one inclusive.
"""

from __future__ import annotations

import argparse
import json
import platform
import statistics
import time
from importlib.metadata import version
from pathlib import Path
from typing import Callable

import great_expectations as gx
import numpy as np
import pandas as pd
import pandera.pandas as pa
import pyarrow as arrow

import proofframe


def timed(run: Callable[[], object], *, warmups: int, repeats: int) -> list[float]:
    for _ in range(warmups):
        run()
    samples = []
    for _ in range(repeats):
        started = time.perf_counter()
        run()
        samples.append(time.perf_counter() - started)
    return samples


def build_gx_validator(dataframe: pd.DataFrame) -> Callable[[], object]:
    context = gx.get_context(mode="ephemeral")
    source = context.data_sources.add_pandas("benchmark")
    asset = source.add_dataframe_asset(name="frame")
    batch_definition = asset.add_batch_definition_whole_dataframe("whole")
    suite = gx.ExpectationSuite(name="same_rules")
    suite.add_expectation(gx.expectations.ExpectColumnValuesToNotBeNull(column="id"))
    suite.add_expectation(gx.expectations.ExpectColumnValuesToBeUnique(column="id"))
    suite.add_expectation(
        gx.expectations.ExpectColumnValuesToBeBetween(column="score", min_value=0, max_value=1)
    )
    suite = context.suites.add(suite)
    definition = gx.ValidationDefinition(name="same_rules", data=batch_definition, suite=suite)
    definition = context.validation_definitions.add(definition)

    def validate() -> object:
        result = definition.run(batch_parameters={"dataframe": dataframe})
        if not result.success:
            raise RuntimeError("Great Expectations rejected valid benchmark data")
        return result

    return validate


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rows", type=int, default=1_000_000)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    dataframe = pd.DataFrame(
        {
            "id": np.arange(args.rows, dtype=np.int64),
            "score": np.linspace(0.0, 1.0, args.rows, dtype=np.float64),
        }
    )
    table = arrow.Table.from_pandas(dataframe, preserve_index=False)
    contract = {
        "columns": {
            "id": {"required": True, "not_null": True, "unique": True},
            "score": {"min": 0, "max": 1},
        }
    }
    pandera_schema = pa.DataFrameSchema(
        {
            "id": pa.Column("int64", nullable=False, unique=True),
            "score": pa.Column("float64", checks=pa.Check.in_range(0, 1)),
        },
        strict=True,
    )

    def proof_run() -> object:
        result = proofframe.validate(table, contract)
        if not result["valid"]:
            raise RuntimeError("ProofFrame rejected valid benchmark data")
        return result

    def pandera_run() -> object:
        return pandera_schema.validate(dataframe, lazy=True)

    runners = {
        "proofframe": proof_run,
        "pandera": pandera_run,
        "great_expectations": build_gx_validator(dataframe),
    }
    results = {}
    for name, runner in runners.items():
        samples = timed(runner, warmups=args.warmups, repeats=args.repeats)
        median = statistics.median(samples)
        results[name] = {
            "samples_seconds": samples,
            "median_seconds": median,
            "rows_per_second": args.rows / median,
        }

    payload = {
        "methodology": {
            "dataset": "in-memory pandas DataFrame; int64 id and float64 score",
            "rows": args.rows,
            "rules": ["id not null", "id unique", "0 <= score <= 1"],
            "warmups": args.warmups,
            "repeats": args.repeats,
            "timing_scope": "validation only; imports, setup, and conversion excluded",
        },
        "environment": {
            "python": platform.python_version(),
            "platform": platform.platform(),
            "versions": {
                name: version(package)
                for name, package in {
                    "proofframe": "proofframe",
                    "pandera": "pandera",
                    "great_expectations": "great-expectations",
                    "pandas": "pandas",
                    "pyarrow": "pyarrow",
                }.items()
            },
        },
        "results": results,
    }
    rendered = json.dumps(payload, indent=2)
    print(rendered)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
