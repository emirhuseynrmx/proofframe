# ProofFrame test strategy

## What runs now

- Rust unit, integration, allocation, and Proptest suites cover typed contracts, exact state,
  partition ordering, PII primitives, and signed-receipt tampering.
- Python integration tests cross the PyO3/Arrow C Stream boundary.
- Clippy with warnings denied, rustfmt, Ruff, and a three-OS CI matrix are release gates.
- `benchmarks/release_gate.py` runs each case in a dedicated worker process. The parent samples RSS
  every 5 ms, so a Python thread blocked by the GIL cannot hide the native peak. Artifacts retain
  seven raw Python and native-scan samples, median, IQR, throughput, native resource counters,
  spill bytes, hardware, compiler, algorithm version, and the dataset SHA-256.
- Rules-only validation uses typed Arrow buffers and type-specific hash sets for integer,
  timestamp, floating-point, and string uniqueness. It still returns bounded row evidence; it skips
  profile, fingerprint, exact distinct, and min/max profile state.
- Contract validation tracks `violation_count` independently from stored finding examples, so
  `max_findings=0` cannot hide invalid data.

## Miri, fuzzing, and Loom

The crate forbids unsafe code, but Miri still checks the pure-Rust ownership and state transitions
that protect hierarchical resource reservations and strict contract parsing. CI runs only those
explicitly compatible tests with `cargo +nightly miri test --lib miri_tests::`. It does not claim to
exercise PyO3 or Arrow FFI; those boundaries are covered by built-wheel tests on three operating
systems and Python 3.10–3.13.

Three 60-second libFuzzer jobs feed bounded arbitrary input into the strict contract parser, the
V1/V2 receipt dispatcher, and the production checksummed partition decoder. The partition target
caps input at one MiB before writing or decoding it, and the decoder checks declared lengths before
allocation.

Partition workers use scoped standard threads, a mutex-protected bounded work queue, ordered result
slots, and the existing hierarchical resource account. No custom synchronization primitive or
lock-free state machine is present, so Loom is not used. The Miri-compatible coordinator tests cover
the pure state transitions; built-wheel tests cover the Arrow/PyO3 boundary.

## Release benchmark contract

The checked-in Bitcoin manifest pins `btcusd_1-min_data.csv` at 7,645,034 rows and SHA-256
`bf38ee982b0486a08e2c2ceddb2ad588725aafc26c3dd6f043edb4ce4913231a`. File ingestion and schema
normalization are outside the timed region. A local smoke run uses a deterministic Bitcoin-shaped
synthetic table and is never presented as the real-data result:

```bash
python benchmarks/release_gate.py \
  --rows 100000 --runs 7 --warmups 1 \
  --output target/release-gate-smoke.json
```

The v2 matrix covers frozen V1 numeric validation, cross-column comparisons, conditional
assertions, dataset ratios, composite identity in memory and under forced spill, and fingerprints.
`tests/test_release_gate.py` rejects artifacts with fewer than seven runs, a different dataset hash,
missing correctness or native/Python timing fields, mixed fingerprint versions, allocation growth,
or different CPU/compiler identity. Linux `perf` counters are optional diagnostics: unavailable
permissions are recorded as `null` with the OS error and never replaced by estimates.

The allocation fields are backed by the release-mode counting allocator in
`tests/allocation_contract.rs`; production RSS and engine-accounted bytes are separate measures.
Cross-version speed gates run only against a baseline captured on the same dataset, hardware,
compiler, and fingerprint version. The 100k run is a correctness/resource smoke. The 95% installed
Python/native throughput gate applies at one million rows or more, where fixed reader and FFI setup
does not dominate sub-millisecond kernels.

## Remaining dedicated-runner work

1. Capture the 0.5 result on the pinned real Bitcoin file and archive the complete artifact.
2. Capture a comparable 0.4 baseline on the same dedicated runner before enforcing ratio gates.
3. Keep raw samples and unavailable hardware counters visible; never publish only a speedup ratio.

No benchmark result is a universal performance claim. Data shape, null density, rule selection,
framework versions, allocator, CPU, and cache state can change the ranking.
