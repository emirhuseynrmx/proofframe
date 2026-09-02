# dbt integration

This package supplies a `run-operation` macro that prints a ProofFrame command for a model artifact and contract. Core dbt macros do not execute local shell commands, so an orchestrator or CI step must run the emitted command after dbt materializes the file.

Copy `macros/proofframe_validate.sql` into your dbt project. Then point it at a file your dbt workflow creates, such as a Parquet export:

```bash
dbt run-operation proofframe_validate --args '{"model_path":"target/orders.parquet","contract_path":"contracts/orders.json"}'
```

The operation logs:

```text
proofframe check target/orders.parquet --contract contracts/orders.json
```

Run that command in the step that owns the artifact. A non-zero exit status means the contract was not satisfied. The paths are examples: configure an export/materialization and an execution environment that make both files available before relying on this check.
