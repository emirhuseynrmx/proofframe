"""Validate a Parquet artifact with ProofFrame in Apache Airflow.

Copy this file into an Airflow DAG folder, then change ``data_path`` and
``contract_path`` to locations available to the Airflow worker. The task exits
non-zero when the contract is invalid, so normal Airflow retry and alerting
policies apply.
"""

from __future__ import annotations

from datetime import datetime, timezone

from airflow.models import DAG
from airflow.operators.bash import BashOperator

with DAG(
    dag_id="proofframe_validation",
    description="Validate a produced Parquet artifact with a ProofFrame contract.",
    start_date=datetime(2025, 1, 1, tzinfo=timezone.utc),
    schedule=None,
    catchup=False,
    params={
        "data_path": "/opt/airflow/data/orders.parquet",
        "contract_path": "/opt/airflow/contracts/orders.json",
    },
    tags=["data-quality", "proofframe"],
) as dag:
    validate_contract = BashOperator(
        task_id="validate_contract",
        bash_command=(
            "proofframe check '{{ params.data_path }}' "
            "--contract '{{ params.contract_path }}'"
        ),
    )
