# Concepts

## Contract, plan, and source identity

A contract is the human-authored JSON document. ProofFrame compiles it against the
input Arrow schema into a typed plan before consuming data. This is why an unknown
rule, missing required column, or incompatible literal fails early.

The compiled-plan digest answers: **which executable rules ran?** It excludes
operational metadata such as `status` and `suggested_from`. The contract-source digest
answers: **which exact source document was approved?** It includes that metadata.
Both are placed in reports and evidence. Changing a suggested contract from `draft` to
`active` therefore preserves the plan digest while changing the source digest.

The pre-0.6 source canonicalization is frozen: a 0.5.1 source document retains its
byte-identical source digest in 0.6.0. This keeps earlier evidence comparable.

## Fingerprints

A fingerprint is a versioned BLAKE3 identity for ordered Arrow data. It binds schema,
column order, row order, nulls, type tags, and canonical values. It is stable across
record-batch segmentation. V1 remains available for existing proofs; V2 is a distinct
protocol, not a silent replacement.

## Evidence and receipts

`check_with_evidence()` binds a validation result, dataset fingerprint, Arrow schema,
resource settings, engine version, source digest, and compiled-plan digest in one
execution. Evidence is useful when a pass/fail result must be retained or compared
later.

A receipt is an Ed25519 signature over evidence. `verify_receipt()` reports
cryptographic integrity separately from whether an expected public key was supplied
and matched. A valid signature does not independently establish that the signer was
authorized; obtain trusted public keys through a separate channel.

## Findings and bounded output

ProofFrame keeps exact violation counts but bounds retained examples. `max_samples`
limits findings in a report; `max_output_records` limits full diff output. A report
with `truncated: true` still has an exact `violation_count`, so an empty or short
finding list is never proof that all failures were retained.

## Partitions and spill

Partition validation treats inputs as one ordered logical dataset. Dataset-wide rules,
such as distinct ratios and composite uniqueness, retain exact state across partition
boundaries. Partition evidence adds a manifest that binds each partition's fingerprint,
schema, row count, contract, plan, resource settings, and contribution to the global
result.

Exact state can exceed memory. `max_memory` bounds in-memory state and `max_temp`
bounds temporary storage. With `spill="auto"`, ProofFrame may create sorted,
checksummed temporary runs; with `spill="never"`, it fails rather than using disk.
Resource exhaustion and corrupt temporary state fail closed.

## Draft suggestions

`suggest_contract()` observes one dataset and produces a reviewable hypothesis, not a
guarantee about future data. Its `suggested_from` record contains the observed rows,
fingerprint, engine version, and review notices. Drafts are rejected mechanically by
every validation entry point. Only an explicit review and `status: "active"` makes a
contract executable.
