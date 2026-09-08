# Review parity fixtures

Each `<name>.json` holds a report, evidence, contract, label and column names.
The `.html` and `.md` beside it are what `proofframe/review_render.py` produced
from that input in 0.7.0, before the renderer moved into the crate. They are the
migration guarantee for that move, and from 0.7.1 they are the golden output of
`review_html` and `review_markdown`.

The fixtures exist to cover what could plausibly differ between two languages
rather than what a typical review looks like:

| Fixture      | Covers                                                             |
| ------------ | ------------------------------------------------------------------ |
| `passing`    | a green result, and rules that were never offered a value           |
| `hostile`    | HTML injection, quotes, backslashes, non-ASCII keys and values      |
| `no_samples` | a violation with no sampled findings, and empty contract sections   |
| `long_value` | values past the 1000-character presentation cap                     |

The Python renderer is gone, so these cannot be regenerated from it. Change them
only when the review is meant to change, and say so in the changelog: a diff here
is a diff in every review every user has ever produced.

One difference from those files is deliberate and asserted separately in
`tests/review_parity.rs`: keys in the disclosed contract, and the resource and
metric rows, are ordered by key rather than by insertion. The `hostile` contract
is written in sorted order so the fixtures themselves stay byte-comparable.
