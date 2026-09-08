# Data Review (0.7.1)

## CI migration: review fails on violations; evidence does not

`proofframe review` exits **1** when the contract is violated, while still writing
the completed review package. It exits0 on a valid result. Existing `evidence`
continues to exit 0 when evidence generation succeeds, even for invalid data.
No existing command's success semantics or default JSON output changed. Switching
a CI step from evidence to review intentionally turns violations into failed steps.
Upload review artifacts in an `if: always()` step after validation if you need to
inspect a failed check. Do not treat invalid input or resource failure as a passed check.

## First run

```console
proofframe review orders.parquet --contract contract.json --out review-run
```

Open `review-run/index.html` locally. It works without a server, scripts, web
fonts or network access. The folder also contains `summary.md`, `report.json`,
and `evidence.json`. Keep the files together so the links work.

No contract yet? Use `proofframe suggest orders.parquet > contract.json`, review
the proposed rules, and set the V2 draft's status to active. Suggestions are not
automatically activated. Errors show the appropriate envelope/type names; no
automatic conversion from `string` to `utf8` or from V1 to V2 takes place.

The first report defaults to **zero sampled findings**. For invalid data it says:
“Rerun with --max-samples 20 to include finding details.” Choose a new output folder:

```console
proofframe review orders.parquet --contract contract.json --out review-details --max-samples 20
```

With samples, “Start here” identifies the first retained finding's column, zero
indexed row and rule. It is an investigation starting point, not a claim that this
is the root cause or the most severe violation. Total violations are exact; samples
may be truncated. Multiple violations can refer to the same row. No per-rule pass
verdicts or invented percentages are derived from the samples.

Python callers can supply a PyArrow/Pandas/Polars container or Arrow reader:

```python
import proofframe as pf

# result = pf.review(table, contract, 'review-run', max_samples=20)
# result['valid'] is the contract verdict; Python does not exit on violations.
```

`python examples/review_demo.py` creates credential-free synthetic examples for
valid data, invalid data without samples, and invalid data with samples.

## Resource and output contract

The source data is checked and fingerprinted in one `check_with_evidence` native
execution. Rendering never scans the data again. Reference datasets are supported
with the existing repeatable `--reference NAME=PATH` option.

Engine defaults remain 512 MiB memory, 4 GiB temporary storage, 100000 output records.
Review samples default 0. `--max-output-bytes` defaults to 16 MiB for the combined four
UTF-8 output files and is separate from engine memory. Files are streamed through
a size-limited writer. Rules, source names and native result structures themselves
still occupy memory: this is not a total process RSS limit. Report size does not
grow with every scanned row, but can grow with schema, contract and sample size.

The output directory must be new; an existing path is an error. The directory is
built in a sibling staging folder, file contents are flushed and fsynced, and the
completed folder is renamed into place. Cooperating publishers serialize through
an exclusive sidecar lock. Validation/render/write failures remove this run's
staging output. A process crash may leave a staging directory/lock; remove an
abandoned lock only after confirming no publisher is running. There is no new
power-loss guarantee or defense against hostile external directory mutations.

Invalid contracts/CLI options use exit 2; engine/schema/data/I/O errors use the
existing error paths; resource errors use exit 4. There is no final success package
on incomplete validation or failed publication. Review filesystem errors currently
follow the existing CLI OSError classification (exit 2), distinct from native I/O
errors (exit 3). Callers should treat every nonzero code other than a completed
violation package as an incomplete run, not as evidence about data quality.

## Sharing and trust

- Nothing is uploaded or sent automatically. The browser does not open itself.
- Zero samples prevents native finding messages/values from entering the package.
  **Column names, labels and contract literals are not redacted.** A suggested
  contract may itself contain observed categories/ranges. Review all artifacts
  before sharing; zero samples is not an anonymization guarantee.
- With samples enabled, native messages may include sensitive values. They appear
  consistently in the bounded JSON/evidence and escaped human views. Long display
  strings are truncated; machine-readable content stays unchanged.
- The HTML has a restrictive CSP, no JavaScript, and HTML-escapes all untrusted
  text. Markdown escapes untrusted markup as well. The original JSON is still data;
  consumers must escape it if they render it elsewhere.
- HTML and Markdown are not signed. `evidence.json` retains the existing Evidence V2
  format and can be passed to `sign`/`verify` with a separately trusted key. Signing
  evidence does not automatically attest that someone has not edited the HTML.
- The contract is copied before scanning, so a caller mutation during data
  iteration cannot silently change which rules the report displays.
