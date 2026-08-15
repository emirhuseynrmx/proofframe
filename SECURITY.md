# Security policy and invariants

Report suspected vulnerabilities privately to the maintainer before public disclosure.

ProofFrame processes untrusted tabular values without executing them. The Rust crate uses
`#![forbid(unsafe_code)]`; Arrow, PyO3, and the Arrow C Stream FFI remain dependency trust
boundaries. CI runs tests, Clippy, Miri-compatible core tests, fuzz targets, package checks, and
cross-platform wheel tests.

## Fingerprint protocols

`pf-fp-v1` is frozen for compatibility. `pf-fp-v2` is a separate, versioned canonical protocol.
Both bind ordered Arrow schema and cell content with typed, length-prefixed encodings; V2 uses the
prepared segmented encoder. Callers must persist the tag and must never compare digests from
different versions.

Supported scalar and nested types use canonical binary encodings rather than Arrow display text.
An unsupported type fails closed. Text rendering is used only for text-rule evaluation and human
diagnostics, never as proof-critical fingerprint input.

## Contract, evidence, and receipt protocols

Missing required columns, unknown contract fields, invalid bounds, and incompatible rule/type
pairs fail during compilation before a row is scanned. Timestamp bounds are signed integer ticks in
the Arrow field's declared unit, expressed as JSON numbers or decimal strings; ISO-8601 strings are
not accepted by contract V1.

Evidence V2 binds three independent contract identities:

- `pf-contract-v1` over validated RFC 8785-canonical source JSON;
- `pf-plan-v1` over the schema-resolved typed execution plan;
- `pf-schema-v1` over the Arrow schema used for compilation.

It also binds the V2 dataset fingerprint, row count, engine version, operation, effective resource
limits, exact violation count, and retained output count. Receipt V2 signs the entire strict evidence
envelope with an Ed25519 domain-separated message. V1 report receipts are migration-only and must
be requested explicitly.

Signature integrity and signer authorization are different properties. `SignatureOnly` proves only
that the embedded key signed the receipt. Production authorization should supply an expected public
key or a `TrustStore`. A valid receipt does not prove honest data collection or contract sufficiency.

## Redacted PII fingerprints

PII findings never return matched values. V2 findings use full 256-bit keyed BLAKE3 digests. The
default `unlinkable` mode generates a new random 256-bit key for every scan, so equal values cannot
be correlated across runs. `stable` mode requires a caller-managed 32-byte secret and a non-secret
`key_id` for rotation. Keys and salts are never serialized; reports contain only mode and key ID.

PII scanning is a high-signal helper, not a legal-compliance guarantee. Email, IPv4, phone, Luhn
payment-card, and IBAN patterns are recognized. Numeric-only matches are downgraded because business
identifiers can match checksums by chance.

## Resource and corruption invariants

For compiled validation, retained findings are bounded by
`min(contract.max_findings, resources.max_samples)` while `violation_count` remains exact. Finding
strings and vector growth are charged to the operation memory account. Every yielded record batch
must exactly match the schema used to compile the plan or execution returns `PF_SCHEMA_MISMATCH`.

Exact profile distinct state uses the bounded spill engine and is opt-in; `profile()` defaults to
`distinct="none"`. Exact uniqueness, leakage, and diff use hierarchical memory and temporary-storage
accounts. Diff partition record sizes are checked before scratch growth, scratch and retained samples
are charged, persisted lengths are capped before allocation, and partition payloads are schema-bound
and checksummed. Complete diff output is capped by `max_output_records` and published atomically.

Contracts and regular expressions are trusted configuration. Resource exhaustion, I/O failures,
Arrow failures, corrupt persisted state, schema mismatch, and contract violations have distinct
stable error codes and Python exception classes.

## Release provenance

A version tag is not sufficient to publish. CI on the exact tag commit must complete every required
Rust, Python, quality, Miri, and fuzz job and emit `proofframe.release-evidence.v1` containing the tag
ref, commit SHA, and CI run ID. The publish workflow downloads and verifies that artifact, rebuilds
from the same SHA, checks the package version, then uses trusted publishing.
