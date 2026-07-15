# ProofFrame test strategy

## What runs now

- Rust unit tests and Proptest properties cover PII primitives and signed-receipt tampering.
- Python integration tests cross the PyO3/Arrow C Stream boundary.
- Clippy with warnings denied, rustfmt, Ruff, and a three-OS CI matrix are release gates.
- The comparative benchmark is standalone and records inputs, package versions, raw samples,
  median time, and throughput.
- Rules-only validation uses typed Arrow buffers, exact Roaring bitmaps for 64-bit integer
  uniqueness, and exact hash sets for floating-point/string uniqueness. It still returns bounded
  row evidence; it skips only the separate profile and fingerprint products.

## Miri and Loom decision

Miri is valuable for pure Rust unsafe-code boundaries, but the current native module crosses PyO3 and
Arrow FFI, which Miri cannot exercise as a normal Python extension. A Miri job would therefore be a
misleading green badge or a permanently broken gate. Before 0.4 stable, pure logic should be moved
into a `proofframe-core` crate and that crate should run under Miri.

Loom is not warranted yet. ProofFrame has no custom synchronization primitive, lock-free algorithm,
or concurrent state machine. Add Loom when shared caches, a parallel streaming coordinator, or other
interleaving-sensitive state is introduced.

## Next hardening gates for 0.4 stable

1. Extract `proofframe-core` and run Miri on it.
2. Add cargo-fuzz targets for detector inputs, canonical receipt parsing, and Arrow batch boundaries.
3. Add differential properties proving fingerprints are invariant to batch segmentation.
4. Run the benchmark on a pinned dedicated runner; publish all raw samples, not only a speedup ratio.

No benchmark result is a universal performance claim. Data shape, null density, rule selection,
framework versions, allocator, CPU, and cache state can change the ranking.
