"""Offline, escaped views of one native validation/evidence result."""

from __future__ import annotations

import html
import json
import re
from collections.abc import Iterator, Mapping, Sequence
from typing import Any

CSS = """
:root{color-scheme:light;--paper:#fff;--ground:#f5f7fb;--ink:#15253d;
--blue:#2458a6;--warning:#8a4518;--green:#126044;--line:#d6dfea}
*{box-sizing:border-box}body{margin:0;background:var(--ground);color:var(--ink);
font-family:Geist,'Segoe UI',system-ui,sans-serif;font-size:16px;line-height:24px}
main{max-width:1120px;margin:0 auto;padding:40px 24px 64px}
header{display:flex;justify-content:space-between;gap:16px;align-items:center;margin-bottom:32px}
.brand{font-weight:700;letter-spacing:.04em}.badge{font-size:14px;line-height:20px;
padding:4px 12px;border-radius:9999px;background:#fff0e4;color:var(--warning)}
.pass{background:#e6f3ed;color:var(--green)}h1{font-size:36px;line-height:40px;
margin:8px 0 16px;font-weight:600;overflow-wrap:anywhere;text-wrap:balance}
h2{font-size:20px;line-height:28px;margin:0 0 16px;font-weight:600}
h3{font-size:16px;line-height:24px;margin:0}p{margin:8px 0 16px;text-wrap:pretty}
.action{padding:32px;border:1px solid var(--line);border-radius:16px;background:var(--paper)}
.eyebrow{color:var(--blue);font-size:14px;line-height:20px;font-weight:600}
.muted{color:#53647b}.metrics{display:grid;grid-template-columns:repeat(3,1fr);
gap:24px;margin:24px 0 32px}.metric{padding:16px 0}.value{display:block;
font-size:24px;line-height:32px;font-weight:600}.label{font-size:14px;line-height:20px}
section{margin-top:32px}a{color:var(--blue);text-underline-offset:4px}a:hover{color:var(--ink)}
a:focus-visible,summary:focus-visible{outline:3px solid var(--blue);outline-offset:4px}
nav{display:flex;gap:24px;flex-wrap:wrap}code,pre{font-family:'Geist Mono',Consolas,monospace;
font-size:14px;line-height:20px;overflow-wrap:anywhere}pre{white-space:pre-wrap;
background:#eaf0f8;border-radius:8px;padding:16px;margin:16px 0;max-height:320px;overflow:auto}
.finding{padding:24px;border:1px solid var(--line);border-radius:8px;background:white;margin:16px 0;
overflow-wrap:anywhere}.location{display:flex;gap:16px;flex-wrap:wrap}.location code{font-weight:600}
dl{display:grid;grid-template-columns:200px minmax(0,1fr);gap:12px 24px;margin:0}
dt{color:#53647b}dd{margin:0;overflow-wrap:anywhere}details{background:white;border:1px solid var(--line);
border-radius:8px;padding:16px;margin:16px 0}summary{cursor:pointer;font-weight:600}
footer{margin-top:40px;color:#53647b;font-size:14px;line-height:20px}
.skip{position:absolute;left:-9999px}.skip:focus{position:static}
@media(max-width:600px){main{padding:24px 16px 40px}.action{padding:24px 16px}
h1{font-size:24px;line-height:32px}.metrics{gap:12px}.value{font-size:20px;line-height:28px}
dl{grid-template-columns:1fr;gap:4px}dd{margin-bottom:12px}.finding{padding:16px}}
@media print{body{background:white}main{padding:0}nav,.skip{display:none}pre{max-height:none}}
"""


def text(value: Any) -> str:
    """Bound presentation length; the machine-readable artifacts stay unmodified."""
    value = str(value)
    return value if len(value) <= 1000 else value[:1000] + "… (see JSON)"


def escape(value: Any) -> str:
    return html.escape(text(value), quote=True)


def markdown_escape(value: Any) -> str:
    """Neutralises markup in a value that came from the data or the contract.

    `.` and `-` are left alone: they only carry meaning at the start of a line, and a
    value cannot reach one because `text` collapses every run of whitespace. Escaping
    them turned prose the reader is meant to copy, such as a command line, into
    something that no longer runs.
    """
    value = html.escape(" ".join(text(value).split()), quote=True)
    return re.sub(r"([\\`*_{}\[\]()#+!|>~])", r"\\\1", value)


# A rendered sentence is the tool's own words with the data's words placed inside it.
# Only the second kind is escaped, so a command stays runnable and a filename stays
# readable while a hostile column name still cannot introduce markup.
Segment = tuple[str, bool]


def _own(text_: str) -> Segment:
    return (text_, False)


def _data(value: Any) -> Segment:
    return (text(value), True)


def render_segments(segments: Sequence[Segment], escaper: Any) -> str:
    return "".join(escaper(value) if untrusted else value for value, untrusted in segments)


def action(report: Mapping[str, Any]) -> tuple[list[Segment], list[Segment]]:
    findings = report["findings"]
    if report["valid"]:
        return (
            [_own("Keep this result with your data")],
            [_own("The declared contract passed on this dataset.")],
        )
    if findings:
        first = findings[0]
        return (
            [_data(first.get("column", "Dataset")), _own(" · row "), _data(first.get("row", "—"))],
            [
                _own("Inspect the "),
                _data(first["rule"]),
                _own(" rule in your contract. Rows are zero indexed."),
            ],
        )
    return (
        [_own("Inspect the violations")],
        [_own("Rerun with --max-samples 20 to include finding details.")],
    )


def unexercised(report: Mapping[str, Any], names: Sequence[str]) -> list[str]:
    """Columns whose row rules were never offered a value.

    A contract can pass because nothing was wrong or because nothing was asked. These
    are the second kind, and they are the only part of a green result worth reading.

    The engine reports schema indices rather than names so its scan loop stays inside
    its allocation contract; the names are resolved here, from the schema the caller
    already had.
    """
    counts = report.get("evaluated_columns") or []
    indices = report.get("evaluated_indices") or []
    return [
        names[index] if index < len(names) else f"column {index}"
        for index, seen in zip(indices, counts)
        if seen == 0
    ]


def fingerprint(evidence: Mapping[str, Any]) -> str:
    dataset = evidence["dataset"]
    return f"pf-fp-{dataset['fingerprint_version']}:" + bytes(dataset["fingerprint_digest"]).hex()


def markdown(
    report: Mapping[str, Any],
    evidence: Mapping[str, Any],
    label: str,
    column_names: Sequence[str] = (),
) -> Iterator[str]:
    # Escape all markup-bearing user input; no provider-supplied URLs or raw HTML.
    title, instruction = action(report)
    heading = render_segments(title, markdown_escape)
    guidance = render_segments(instruction, markdown_escape)
    yield f"# ProofFrame data review\n\n**Start here:** {heading}\n\n{guidance}\n\n"
    yield f"Dataset: {markdown_escape(label)}\n\n"
    if not report["valid"] and not report["findings"]:
        yield "Finding details are off. Rerun with `--max-samples 20`.\n\n"
    yield f"**{'Passed' if report['valid'] else 'Contract violations'}** · "
    yield f"{report['violation_count']} violations · {report['rows']} rows\n\n"
    yield (
        f"{len(report['findings'])} sampled finding(s); "
        "not a complete list when truncated. Violations are not a count of distinct failing rows.\n\n"
    )
    for finding in report["findings"]:
        yield (
            f"- {markdown_escape(finding.get('column', 'Dataset'))}, row {markdown_escape(finding.get('row'))}: "
            f"{markdown_escape(finding['rule'])} — {markdown_escape(finding.get('message', ''))}\n"
        )
    idle = unexercised(report, column_names)
    if idle:
        yield (
            f"\n**{len(idle)} column(s) had no value for their rules to check.** "
            "A rule that was never asked did not pass.\n\n"
        )
        for name in idle:
            yield f"- {markdown_escape(name)}: 0 values evaluated\n"
    yield f"\nDataset fingerprint: {fingerprint(evidence)}\n\n"
    yield "This review is local. Evidence is unsigned until signed separately.\n"


def _findings_section(report: Mapping[str, Any]) -> Iterator[str]:
    """The sampled findings, with the sampling stated rather than implied."""
    yield '<section><h2>Where to investigate</h2><p class="muted">'
    yield f"{len(report['findings'])} sampled finding(s). "
    yield ("This is not a complete list. " if report.get("truncated") else "")
    yield "A violation count is not a count of distinct failing rows. Unlisted rules are not marked as passed.</p>"
    for finding in report["findings"]:
        yield '<article class="finding"><div class="location">'
        yield f"<code>{escape(finding.get('column', 'Dataset'))}</code>"
        yield f"<span>row {escape(finding.get('row', '—'))}</span><span>{escape(finding['rule'])}</span></div>"
        yield f"<p>{escape(finding.get('message', ''))}</p></article>"
    yield "</section>"


def _idle_section(report: Mapping[str, Any], column_names: Sequence[str]) -> Iterator[str]:
    """Columns whose rules had nothing to check. Silence here would read as a pass."""
    idle = unexercised(report, column_names)
    if not idle:
        return
    yield "<section><h2>Rules that were never asked</h2>"
    yield (
        f"<p>{len(idle)} column(s) had no value for their rules to check. "
        "A rule that was never asked did not pass.</p>"
    )
    for name in idle:
        yield '<article class="finding"><div class="location">'
        yield f"<code>{escape(name)}</code><span>0 values evaluated</span></div></article>"
    yield "</section>"


def _contract_section(contract: Mapping[str, Any]) -> Iterator[str]:
    """The declared rules, disclosed verbatim rather than summarized."""
    yield "<section><h2>What was checked</h2>"
    yield "<p>These are the declared rules, not individual pass verdicts.</p>"
    for name in ("columns", "row_rules", "dataset_rules"):
        value = contract.get(name)
        if value:
            yield f"<details><summary>{escape(name.replace('_', ' ').capitalize())}</summary><pre>"
            # Rules may contain sensitive literal values: disclose rather than claim redaction.
            for chunk in json.JSONEncoder(indent=2, ensure_ascii=True).iterencode(value):
                yield html.escape(chunk, quote=True)
            yield "</pre></details>"
    yield "</section>"


def document(
    report: Mapping[str, Any],
    evidence: Mapping[str, Any],
    contract: Mapping[str, Any],
    label: str,
    column_names: Sequence[str] = (),
) -> Iterator[str]:
    title, instruction = action(report)
    yield '<!doctype html><html lang="en"><head><meta charset="utf-8">'
    yield '<meta name="viewport" content="width=device-width,initial-scale=1">'
    yield (
        '<meta http-equiv="Content-Security-Policy" content="default-src &#39;none&#39;; '
        'style-src &#39;unsafe-inline&#39;; base-uri &#39;none&#39;; form-action &#39;none&#39;">'
    )
    yield f"<title>ProofFrame review · {escape(label)}</title><style>{CSS}</style></head><body>"
    yield '<a class="skip" href="#main">Skip to review</a><main id="main">'
    badge = "Contract passed" if report["valid"] else "Contract violations"
    yield '<header><div class="brand">ProofFrame / Data review</div><span class="badge '
    yield f'{"pass" if report["valid"] else ""}">{badge}</span></header>'
    yield '<article class="action"><div class="eyebrow">Start here</div><h1>'
    yield f"{render_segments(title, escape)}</h1>"
    yield f"<p>{render_segments(instruction, escape)}</p>"
    yield f'<p class="muted">{escape(label)}</p>'
    if not report["valid"] and not report["findings"]:
        yield '<pre>--max-samples 20</pre><p class="muted">Finding details are off by default. '
        yield "Enabling samples can include sensitive content in every artifact.</p>"
    yield '<nav aria-label="Review files"><a href="report.json">Validation JSON</a>'
    yield '<a href="evidence.json">Evidence JSON</a><a href="summary.md">CI summary</a></nav></article>'
    yield '<div class="metrics">'
    for value, name in [
        (f"{report['violation_count']} violations", "Exact total"),
        (report["rows"], "Rows scanned"),
        (len(report["findings"]), "Sampled findings"),
    ]:
        yield f'<div class="metric"><span class="value">{escape(value)}</span><span class="label">{name}</span></div>'
    yield "</div>"
    yield from _findings_section(report)
    yield from _idle_section(report, column_names)
    yield from _contract_section(contract)
    yield "<section><h2>Data identity and execution</h2><dl>"
    pairs = [
        ("Dataset fingerprint", fingerprint(evidence)),
        ("Contract digest", report.get("contract_source_digest", "")),
        ("Schema digest", report.get("schema_digest", "")),
        ("Plan digest", report.get("compiled_plan_digest", "")),
        ("Engine version", evidence["engine"]["version"]),
    ]
    pairs += [(key.replace("_", " "), value) for key, value in report.get("resources", {}).items()]
    pairs += [(key.replace("_", " "), value) for key, value in report.get("metrics", {}).items()]
    for key, value in pairs:
        yield f"<dt>{escape(key)}</dt><dd><code>{escape(value)}</code></dd>"
    yield '</dl><p class="muted">Memory counters describe the engine, not total process memory.</p></section>'
    yield "<footer>Generated from one validation and fingerprint scan. No external assets or telemetry.<br>"
    yield "This HTML is not signed. Sign evidence.json separately to attest the engine result.<br>"
    yield "Review contract literals, column names and enabled samples before sharing.</footer></main></body></html>"
