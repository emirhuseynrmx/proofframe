# Acceptance API (0.7.0)

`accept_file` interprets a completed native evidence scan using an explicit acceptance policy. It is not a second validation engine, a data repair service, or a provider of business guarantees.

```python
import proofframe as pf

bundle = pf.accept_file(
    'orders.csv',
    {'version': 'proofframe.contract.v2', 'columns': {'amount': {'min': 0}}},
    policy={'version': 'proofframe.acceptance-policy.v1',
            'max_violations': 0, 'minimum_evaluated': {'amount': 1}},
    csv_options={'delimiter': ';', 'encoding': 'cp1254', 'decimal_point': ',',
                 'column_types': {'amount': 'float64'}, 'null_values': ['', 'NA']},
    output='acceptance.json',
)
print(bundle['payload']['decision'])
```

`python examples/accept_and_verify.py` runs the whole path offline: an accepted
delivery, one rejected for coverage, one that could not be read at all, and a
tampered bundle that fails verification.

## Decision contract

- `accepted`: native scan completed, violations do not exceed `max_violations`, and every named minimum evaluation threshold is met.
- `rejected`: completed scan exceeded one or more acceptance thresholds. A column absent from native evaluation counters also rejects, even if its configured minimum is zero.
- `unknown`: I/O, Arrow parsing, native data/schema failure or resource exhaustion prevented a complete scan. No successful dataset fingerprint or evidence is fabricated for this state.
- Invalid policy fields, contract errors and invalid API arguments raise. They are not accepted or silently ignored.

`minimum_evaluated` counts non-null values offered to column checks, using native `evaluated_indices` and `evaluated_columns`. These are NOT per-rule execution counters. Zero violations alone does not imply useful coverage. Acceptance defaults to zero permitted violations and no coverage requirement; specify minimums for important columns. A positive violation allowance can accept a report whose native `valid` is false, intentionally: policy and native validity answer different questions.

CSV defaults: comma delimiter, UTF-8, decimal point `.`, Arrow default null tokens, strings nullable, 1 MiB block size. Exact resolved defaults and explicit type aliases are included in the bundle. Types use PyArrow aliases such as `int64`, `float64`, `string`. Unspecified types retain Arrow inference; use explicit types for contractual ingestion. No thousands-separator guessing or automatic repair is attempted. Parquet preserves its stored schema and rejects CSV options. Source files must remain unchanged while scanning; the dataset identity in native evidence covers the scanned Arrow values, not original CSV bytes. Native memory counters exclude Python, Arrow reader buffers, serialization, and operating-system RSS.

## Integrity and optional authenticity

The versioned payload contains the exact contract, normalized policy, resolved read settings, their SHA-256 identities, the decision, native report and native Evidence V2 (including dataset identity). SHA-256 covers the entire payload. The encoding is this API's sorted compact ASCII JSON representation, not a claim of RFC 8785 canonicalization.

Install `proofframe[signing]` for Ed25519 signing and verification via `cryptography`. Existing `generate_keypair()` URL-safe base64 keys are compatible:

```python
keys = pf.generate_keypair()
signed = pf.accept_file('orders.parquet', {'columns': {'amount': {'min': 0}}},
                        private_key=keys['private_key'])
assert pf.verify_acceptance(signed, expected_public_key=keys['public_key'])['valid']
```

Verification checks the payload's shape before it checks any digest, because a hash only proves that what is present was not edited. A bundle whose decision or evidence had been removed and whose hash had been recomputed would otherwise be internally consistent and empty. Every field listed above must be present and recognized, the status must be `accepted`, `rejected` or `unknown`, and only `unknown` may carry a null report and evidence: a decision that claims the data was examined has to carry the examination. The report and the evidence are checked field by field against the shape the scanner writes: each field present with its own type, counts non-negative and consistent with the columns they describe, and the Evidence V2 schema this release binds. Python's `bool` is an `int`, so a count of `True` is rejected explicitly. A present field of the wrong type fails at least as hard as a missing one, because a reader who sees `valid` will use it. An unrecognized extra field in the payload also fails, because a reader cannot know what it was meant to change. All of this is a shape check on the envelope: it does not rescan the data and does not re-validate the findings.

Always pin `expected_public_key` when trust matters. Without a pinned signer, a valid self-signature does not establish the signer you intended. An unsigned hash is editable by anyone; it is an integrity checksum, not authentication. Verification performs no dataset rescan, does not establish publisher honesty and does not independently prove the decision. It verifies the envelope's byte binding and optional signature. Unknown decisions can also be signed; an authentic unknown result is still unknown. HTML/review output is separate and not covered by this acceptance signature.

## CLI

```bash
proofframe accept orders.csv --contract contract.json --policy policy.json --csv-options reader.json --output acceptance.json
proofframe accept orders.parquet --contract contract.json --private-key-file signing-key.txt --output signed.json
proofframe verify-acceptance signed.json --expected-public-key YOUR_PUBLIC_KEY
```

Acceptance exit codes: 0 accepted, 1 rejected, 3 unknown, 2 invalid input/configuration. Output-size exhaustion raises ResourceLimitError (CLI 4). Verification exits 0 for valid integrity/signature and 1 for invalid. All results are JSON. Key files should be protected by the operator; private keys are not written into bundles. Existing output paths are never overwritten by cooperative writers. Publication uses a same-filesystem temporary file and exclusive sidecar lock; abandoned locks after process termination require operator review. No power-loss durability promise is added. Output is bounded to 16 MiB by default through `max_output_bytes` (Python); serialization uses memory before checking this bound, so this is an artifact-size limit, not a total memory limit.

Samples default to zero. This is not anonymization: contracts, schema labels, policy column names and any explicitly requested finding samples may be sensitive. No upload occurs. ERP adapters, on-premises deployment services, access control, portals and automatic correction are outside this library release.
