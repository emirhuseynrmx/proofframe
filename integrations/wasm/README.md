# ProofFrame in the browser

A `cdylib` that exposes the engine to JavaScript through `wasm-bindgen`. It is a
separate crate on purpose: the published `proofframe` crate keeps its dependency
surface, and this one is free to enable `arrow/csv` and a `getrandom` backend for
the whole tree.

## Build

```sh
cargo build --release --target wasm32-unknown-unknown
wasm-bindgen --target web --no-typescript \
  --out-dir pkg target/wasm32-unknown-unknown/release/proofframe_wasm.wasm
```

`wasm-bindgen-cli` must match the `wasm-bindgen` version pinned in `Cargo.toml`.
`.cargo/config.toml` supplies `--cfg getrandom_backend="wasm_js"`, which
`getrandom` 0.3 requires in addition to its feature flag.

## Surface

| Export             | Returns                                                        |
| ------------------ | -------------------------------------------------------------- |
| `engine_version`   | the engine version this module was compiled from                |
| `suggest_contract` | a review-required draft contract inferred from the CSV          |
| `check_csv`        | the validation report, the fingerprint, and the resolved schema |

## What this target cannot do

`wasm32-unknown-unknown` has no filesystem, and the exact-set path spills to a
temporary directory. `std` panics rather than returning an error there, so
`check_csv` refuses `unique`, `composite_unique`, `distinct_count`,
`distinct_ratio` and `references` before entering the engine, and
`suggest_contract` does not infer uniqueness. Every other rule runs the same
native code the CLI runs.

Signed receipts are also unavailable: they read the wall clock.
