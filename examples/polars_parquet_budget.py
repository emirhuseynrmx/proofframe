"""Read a Parquet file in batches and validate it with explicit resource limits."""

from tempfile import TemporaryDirectory

import polars as pl
import proofframe as pf
import pyarrow as pa
from pyarrow import parquet

with TemporaryDirectory() as directory:
    path = f"{directory}/orders.parquet"
    pl.DataFrame({"order_id": [101, 102, 103], "amount": [12.5, 8.0, 10.0]}).write_parquet(path)

    with parquet.ParquetFile(path) as source:
        reader = pa.RecordBatchReader.from_batches(
            source.schema_arrow, source.iter_batches(batch_size=2)
        )
        report = pf.check(
            reader,
            {
                "version": "proofframe.contract.v1",
                "columns": {"order_id": {"unique": True}, "amount": {"min": 0}},
            },
            max_memory=64 * 1024 * 1024,
            max_temp=0,
            spill="never",
        )

print(f"valid={report['valid']} rows={report['rows']}")
