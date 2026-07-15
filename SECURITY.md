# Security policy and invariants

Please report suspected vulnerabilities privately to the maintainer before public disclosure.

ProofFrame processes untrusted tabular values without executing them. The Rust crate is compiled
with `#![forbid(unsafe_code)]`; unsafe behavior can still exist in dependencies or the Python/Arrow
FFI boundary, so CI also runs the native test and Clippy gates.

## Fingerprint invariant

Dataset fingerprints are tagged as `pf-fp-v1:<blake3>`. The hash input is a versioned canonical
encoding of the Arrow schema and ordered cell values. It does not depend on Arrow's human-readable
`Display` formatting. Nulls, column indexes, value lengths, schema fields, and body cells are
domain-separated before hashing.

The current canonical encoder covers booleans, signed and unsigned integer widths, float32/float64,
decimal128, UTF-8 strings, binary values, date32/date64, and
second/millisecond/microsecond/nanosecond timestamps. Unsupported types fail closed instead of
producing a fingerprint with unstable formatting.

## Receipt invariant

Signed proof receipts protect the canonical JSON report they contain. A valid receipt proves that
the holder of the private key signed that exact report payload. It does not prove that the source
data was collected honestly, that the contract was sufficient, or that future ProofFrame versions
will classify PII the same way.

## Detector limits

PII scanning is a high-signal helper, not a legal-compliance guarantee. Email, IPv4, phone, Luhn
payment-card, and IBAN patterns are recognized. Bare digit matches in numeric columns are downgraded
to low confidence because order IDs and account-like identifiers can pass Luhn by chance.

## Operational limits

`max_findings` bounds report size while profile/fingerprint scans still cover the full stream. Keyed
diff currently materializes both keyed datasets in memory, so use normal input size limits for large
tables or network-facing services. Regex patterns and contracts are trusted configuration in the
0.4 alpha line.
