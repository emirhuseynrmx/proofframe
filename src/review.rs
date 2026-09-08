//! Offline, escaped views of one native validation/evidence result.
//!
//! This is the only implementation. Python calls it, and so does the WebAssembly
//! build, because a review rendered in a browser that disagreed with the review
//! rendered by the CLI would be worse than no browser review at all.

use serde_json::Value;

use crate::error::ProofFrameError;

const CSS: &str = r#"
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
"#;

/// Presentation cap, in characters. The machine-readable artifacts stay unmodified.
const MAX_DISPLAY_CHARS: usize = 1000;

fn missing(what: &str) -> ProofFrameError {
    ProofFrameError::Review(format!("Review input is missing `{what}`"))
}

/// Render a value the way Python's `str()` would, so both surfaces agree.
fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Null => "None".to_owned(),
        other => other.to_string(),
    }
}

/// Bound presentation length; the machine-readable artifacts stay unmodified.
fn text(value: &Value) -> String {
    bound(scalar(value))
}

fn bound(value: String) -> String {
    if value.chars().count() <= MAX_DISPLAY_CHARS {
        return value;
    }
    let head: String = value.chars().take(MAX_DISPLAY_CHARS).collect();
    format!("{head}… (see JSON)")
}

/// HTML-escape with quoting, matching `html.escape(value, quote=True)`.
fn escape_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(character),
        }
    }
    out
}

fn escape(value: &Value) -> String {
    escape_str(&text(value))
}

fn escape_text(value: &str) -> String {
    escape_str(&bound(value.to_owned()))
}

/// Neutralise markup in a value that came from the data or the contract.
///
/// `.` and `-` are left alone: they only carry meaning at the start of a line, and a
/// value cannot reach one because whitespace is collapsed first. Escaping them turned
/// prose the reader is meant to copy, such as a command line, into something that no
/// longer runs.
fn markdown_escape_str(value: &str) -> String {
    let collapsed = bound(value.split_whitespace().collect::<Vec<_>>().join(" "));
    let escaped = escape_str(&collapsed);
    let mut out = String::with_capacity(escaped.len());
    for character in escaped.chars() {
        if matches!(
            character,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '('
                | ')'
                | '#'
                | '+'
                | '!'
                | '|'
                | '>'
                | '~'
        ) {
            out.push('\\');
        }
        out.push(character);
    }
    out
}

fn markdown_escape(value: &Value) -> String {
    markdown_escape_str(&scalar(value))
}

/// A rendered sentence is the tool's own words with the data's words placed inside it.
///
/// Only the second kind is escaped, so a command stays runnable and a filename stays
/// readable while a hostile column name still cannot introduce markup.
enum Segment {
    Own(String),
    Data(String),
}

fn own(value: &str) -> Segment {
    Segment::Own(value.to_owned())
}

fn data(value: &Value) -> Segment {
    Segment::Data(text(value))
}

fn data_text(value: &str) -> Segment {
    Segment::Data(bound(value.to_owned()))
}

fn render_html(segments: &[Segment]) -> String {
    segments
        .iter()
        .map(|segment| match segment {
            Segment::Own(value) => value.clone(),
            Segment::Data(value) => escape_str(value),
        })
        .collect()
}

fn render_markdown(segments: &[Segment]) -> String {
    segments
        .iter()
        .map(|segment| match segment {
            Segment::Own(value) => value.clone(),
            Segment::Data(value) => markdown_escape_str(value),
        })
        .collect()
}

/// `mapping.get(key, default)`: a key that is present but null keeps its value,
/// which is not the same thing as an absent key.
fn get_or(value: &Value, key: &str, default: &str) -> String {
    value.get(key).map_or_else(|| default.to_owned(), text)
}

fn findings(report: &Value) -> Result<&Vec<Value>, ProofFrameError> {
    report
        .get("findings")
        .and_then(Value::as_array)
        .ok_or_else(|| missing("findings"))
}

fn valid(report: &Value) -> Result<bool, ProofFrameError> {
    report
        .get("valid")
        .and_then(Value::as_bool)
        .ok_or_else(|| missing("valid"))
}

/// The one instruction the reader should act on, and the sentence explaining it.
fn action(report: &Value) -> Result<(Vec<Segment>, Vec<Segment>), ProofFrameError> {
    let found = findings(report)?;
    if valid(report)? {
        return Ok((
            vec![own("Keep this result with your data")],
            vec![own("The declared contract passed on this dataset.")],
        ));
    }
    let Some(first) = found.first() else {
        return Ok((
            vec![own("Inspect the violations")],
            vec![own(
                "Rerun with --max-samples 20 to include finding details.",
            )],
        ));
    };
    let column = data_text(&get_or(first, "column", "Dataset"));
    let row = data_text(&get_or(first, "row", "—"));
    let rule = first
        .get("rule")
        .ok_or_else(|| missing("findings[].rule"))?;
    Ok((
        vec![column, own(" · row "), row],
        vec![
            own("Inspect the "),
            data(rule),
            own(" rule in your contract. Rows are zero indexed."),
        ],
    ))
}

/// Columns whose row rules were never offered a value.
///
/// A contract can pass because nothing was wrong or because nothing was asked. These
/// are the second kind, and they are the only part of a green result worth reading.
///
/// The engine reports schema indices rather than names so its scan loop stays inside
/// its allocation contract; the names are resolved here, from the schema the caller
/// already had.
fn unexercised(report: &Value, names: &[String]) -> Vec<String> {
    let counts = report
        .get("evaluated_columns")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let indices = report
        .get("evaluated_indices")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    indices
        .iter()
        .zip(counts)
        .filter(|(_, seen)| seen.as_u64() == Some(0))
        .map(|(index, _)| {
            let position = index.as_u64().unwrap_or(u64::MAX);
            usize::try_from(position)
                .ok()
                .and_then(|position| names.get(position))
                .cloned()
                .unwrap_or_else(|| format!("column {position}"))
        })
        .collect()
}

fn fingerprint(evidence: &Value) -> Result<String, ProofFrameError> {
    let dataset = evidence.get("dataset").ok_or_else(|| missing("dataset"))?;
    let version = dataset
        .get("fingerprint_version")
        .ok_or_else(|| missing("dataset.fingerprint_version"))?;
    let digest = dataset
        .get("fingerprint_digest")
        .and_then(Value::as_array)
        .ok_or_else(|| missing("dataset.fingerprint_digest"))?;
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let byte = byte
            .as_u64()
            .and_then(|value| u8::try_from(value).ok())
            .ok_or_else(|| missing("dataset.fingerprint_digest"))?;
        hex.push_str(&format!("{byte:02x}"));
    }
    Ok(format!("pf-fp-{}:{hex}", scalar(version)))
}

fn engine_version(evidence: &Value) -> Result<&Value, ProofFrameError> {
    evidence
        .get("engine")
        .and_then(|engine| engine.get("version"))
        .ok_or_else(|| missing("engine.version"))
}

/// Serialize the way Python's `json.dumps(indent=2, ensure_ascii=True)` does.
///
/// The disclosed contract is copied verbatim into the report, so the two surfaces
/// have to agree on its bytes and not merely on its meaning.
fn python_json(value: &Value, depth: usize, out: &mut String) {
    let pad = |level: usize| "  ".repeat(level);
    match value {
        Value::Object(entries) if entries.is_empty() => out.push_str("{}"),
        Value::Object(entries) => {
            out.push_str("{\n");
            for (position, (key, item)) in entries.iter().enumerate() {
                if position > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&pad(depth + 1));
                python_string(key, out);
                out.push_str(": ");
                python_json(item, depth + 1, out);
            }
            out.push('\n');
            out.push_str(&pad(depth));
            out.push('}');
        }
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Array(items) => {
            out.push_str("[\n");
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&pad(depth + 1));
                python_json(item, depth + 1, out);
            }
            out.push('\n');
            out.push_str(&pad(depth));
            out.push(']');
        }
        Value::String(item) => python_string(item, out),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Null => out.push_str("null"),
        Value::Number(number) => out.push_str(&number.to_string()),
    }
}

/// A JSON string literal with every non-ASCII code point escaped.
fn python_string(value: &str, out: &mut String) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            ascii if (ascii as u32) < 0x7f => out.push(ascii),
            wide => {
                let mut units = [0u16; 2];
                for unit in wide.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

/// Python's `str.capitalize()`: first character upper, the rest lower.
fn capitalize(value: &str) -> String {
    let mut characters = value.chars();
    match characters.next() {
        None => String::new(),
        Some(first) => {
            first.to_uppercase().collect::<String>() + &characters.as_str().to_lowercase()
        }
    }
}

fn is_disclosable(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Object(entries)) => !entries.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(_) => true,
    }
}

/// The sampled findings, with the sampling stated rather than implied.
fn findings_section(report: &Value, out: &mut String) -> Result<(), ProofFrameError> {
    let found = findings(report)?;
    out.push_str("<section><h2>Where to investigate</h2><p class=\"muted\">");
    out.push_str(&format!("{} sampled finding(s). ", found.len()));
    if report.get("truncated").and_then(Value::as_bool) == Some(true) {
        out.push_str("This is not a complete list. ");
    }
    out.push_str(
        "A violation count is not a count of distinct failing rows. \
         Unlisted rules are not marked as passed.</p>",
    );
    for finding in found {
        let column = escape_str(&get_or(finding, "column", "Dataset"));
        let row = escape_str(&get_or(finding, "row", "—"));
        let rule = finding
            .get("rule")
            .ok_or_else(|| missing("findings[].rule"))?;
        let message = escape_str(&get_or(finding, "message", ""));
        out.push_str("<article class=\"finding\"><div class=\"location\">");
        out.push_str(&format!("<code>{column}</code>"));
        out.push_str(&format!(
            "<span>row {row}</span><span>{}</span></div>",
            escape(rule)
        ));
        out.push_str(&format!("<p>{message}</p></article>"));
    }
    out.push_str("</section>");
    Ok(())
}

/// Columns whose rules had nothing to check. Silence here would read as a pass.
fn idle_section(report: &Value, names: &[String], out: &mut String) {
    let idle = unexercised(report, names);
    if idle.is_empty() {
        return;
    }
    out.push_str("<section><h2>Rules that were never asked</h2>");
    out.push_str(&format!(
        "<p>{} column(s) had no value for their rules to check. \
         A rule that was never asked did not pass.</p>",
        idle.len()
    ));
    for name in &idle {
        out.push_str("<article class=\"finding\"><div class=\"location\">");
        out.push_str(&format!(
            "<code>{}</code><span>0 values evaluated</span></div></article>",
            escape_text(name)
        ));
    }
    out.push_str("</section>");
}

/// The declared rules, disclosed verbatim rather than summarized.
fn contract_section(contract: &Value, out: &mut String) {
    out.push_str("<section><h2>What was checked</h2>");
    out.push_str("<p>These are the declared rules, not individual pass verdicts.</p>");
    for name in ["columns", "row_rules", "dataset_rules"] {
        let value = contract.get(name);
        if !is_disclosable(value) {
            continue;
        }
        let heading = escape_text(&capitalize(&name.replace('_', " ")));
        out.push_str(&format!("<details><summary>{heading}</summary><pre>"));
        // Rules may contain sensitive literal values: disclose rather than claim redaction.
        let mut rendered = String::new();
        python_json(value.unwrap_or(&Value::Null), 0, &mut rendered);
        out.push_str(&escape_str(&rendered));
        out.push_str("</pre></details>");
    }
    out.push_str("</section>");
}

/// Everything above the metrics: the document head, the verdict badge, and the
/// one instruction the reader is meant to act on.
fn action_section(report: &Value, label: &str, out: &mut String) -> Result<(), ProofFrameError> {
    let (title, instruction) = action(report)?;
    let is_valid = valid(report)?;

    out.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">");
    out.push_str(
        "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src &#39;none&#39;; style-src &#39;unsafe-inline&#39;; base-uri &#39;none&#39;; form-action &#39;none&#39;\">",
    );
    out.push_str(&format!(
        "<title>ProofFrame review · {}</title><style>{CSS}</style></head><body>",
        escape_text(label)
    ));
    out.push_str("<a class=\"skip\" href=\"#main\">Skip to review</a><main id=\"main\">");
    out.push_str("<header><div class=\"brand\">ProofFrame / Data review</div><span class=\"badge ");
    out.push_str(&format!(
        "{}\">{}</span></header>",
        if is_valid { "pass" } else { "" },
        if is_valid {
            "Contract passed"
        } else {
            "Contract violations"
        }
    ));
    out.push_str("<article class=\"action\"><div class=\"eyebrow\">Start here</div><h1>");
    out.push_str(&format!("{}</h1>", render_html(&title)));
    out.push_str(&format!("<p>{}</p>", render_html(&instruction)));
    out.push_str(&format!("<p class=\"muted\">{}</p>", escape_text(label)));
    if !is_valid && findings(report)?.is_empty() {
        out.push_str(
            "<pre>--max-samples 20</pre><p class=\"muted\">Finding details are off by default. ",
        );
        out.push_str("Enabling samples can include sensitive content in every artifact.</p>");
    }
    out.push_str("<nav aria-label=\"Review files\"><a href=\"report.json\">Validation JSON</a>");
    out.push_str(
        "<a href=\"evidence.json\">Evidence JSON</a><a href=\"summary.md\">CI summary</a></nav></article>",
    );
    Ok(())
}

/// The three headline numbers, stated as counts rather than as a verdict.
fn metrics_section(report: &Value, out: &mut String) -> Result<(), ProofFrameError> {
    let rows = report.get("rows").ok_or_else(|| missing("rows"))?;
    let violations = report
        .get("violation_count")
        .ok_or_else(|| missing("violation_count"))?;
    let sampled = findings(report)?.len().to_string();

    out.push_str("<div class=\"metrics\">");
    for (value, name) in [
        (
            escape_text(&format!("{} violations", scalar(violations))),
            "Exact total",
        ),
        (escape(rows), "Rows scanned"),
        (escape_text(&sampled), "Sampled findings"),
    ] {
        out.push_str(&format!(
            "<div class=\"metric\"><span class=\"value\">{value}</span><span class=\"label\">{name}</span></div>"
        ));
    }
    out.push_str("</div>");
    Ok(())
}

/// What was scanned and under which limits, so the result can be reproduced.
fn identity_section(
    report: &Value,
    evidence: &Value,
    out: &mut String,
) -> Result<(), ProofFrameError> {
    let digest = |key: &str| report.get(key).map_or_else(String::new, scalar);
    let mut pairs = vec![
        ("Dataset fingerprint".to_owned(), fingerprint(evidence)?),
        (
            "Contract digest".to_owned(),
            digest("contract_source_digest"),
        ),
        ("Schema digest".to_owned(), digest("schema_digest")),
        ("Plan digest".to_owned(), digest("compiled_plan_digest")),
        (
            "Engine version".to_owned(),
            scalar(engine_version(evidence)?),
        ),
    ];
    for group in ["resources", "metrics"] {
        let Some(entries) = report.get(group).and_then(Value::as_object) else {
            continue;
        };
        pairs.extend(
            entries
                .iter()
                .map(|(key, value)| (key.replace('_', " "), scalar(value))),
        );
    }

    out.push_str("<section><h2>Data identity and execution</h2><dl>");
    for (key, value) in pairs {
        out.push_str(&format!(
            "<dt>{}</dt><dd><code>{}</code></dd>",
            escape_text(&key),
            escape_text(&value)
        ));
    }
    out.push_str(
        "</dl><p class=\"muted\">Memory counters describe the engine, not total process memory.</p></section>",
    );
    Ok(())
}

/// The offline HTML review for one validation and fingerprint scan.
pub fn review_html(
    report: &Value,
    evidence: &Value,
    contract: &Value,
    label: &str,
    column_names: &[String],
) -> Result<String, ProofFrameError> {
    let mut out = String::new();
    action_section(report, label, &mut out)?;
    metrics_section(report, &mut out)?;
    findings_section(report, &mut out)?;
    idle_section(report, column_names, &mut out);
    contract_section(contract, &mut out);
    identity_section(report, evidence, &mut out)?;
    out.push_str(
        "<footer>Generated from one validation and fingerprint scan. No external assets or telemetry.<br>",
    );
    out.push_str(
        "This HTML is not signed. Sign evidence.json separately to attest the engine result.<br>",
    );
    out.push_str(
        "Review contract literals, column names and enabled samples before sharing.</footer></main></body></html>",
    );
    Ok(out)
}

/// One bullet per sampled finding, with every value from the data escaped.
fn markdown_findings(found: &[Value], out: &mut String) -> Result<(), ProofFrameError> {
    for finding in found {
        let column = markdown_escape_str(&get_or(finding, "column", "Dataset"));
        let row = markdown_escape_str(&get_or(finding, "row", "None"));
        let rule = finding
            .get("rule")
            .ok_or_else(|| missing("findings[].rule"))?;
        let message = markdown_escape_str(&get_or(finding, "message", ""));
        out.push_str(&format!(
            "- {column}, row {row}: {} — {message}\n",
            markdown_escape(rule)
        ));
    }
    Ok(())
}

/// Columns whose rules had nothing to check, stated rather than left as silence.
fn markdown_idle(idle: &[String], out: &mut String) {
    if idle.is_empty() {
        return;
    }
    out.push_str(&format!(
        "\n**{} column(s) had no value for their rules to check.** \
         A rule that was never asked did not pass.\n\n",
        idle.len()
    ));
    for name in idle {
        out.push_str(&format!(
            "- {}: 0 values evaluated\n",
            markdown_escape_str(name)
        ));
    }
}

/// The Markdown summary written for a CI job summary or a pull request.
pub fn review_markdown(
    report: &Value,
    evidence: &Value,
    label: &str,
    column_names: &[String],
) -> Result<String, ProofFrameError> {
    let (title, instruction) = action(report)?;
    let is_valid = valid(report)?;
    let found = findings(report)?;
    let violations = report
        .get("violation_count")
        .ok_or_else(|| missing("violation_count"))?;
    let rows = report.get("rows").ok_or_else(|| missing("rows"))?;

    let mut out = String::new();
    out.push_str(&format!(
        "# ProofFrame data review\n\n**Start here:** {}\n\n{}\n\n",
        render_markdown(&title),
        render_markdown(&instruction)
    ));
    out.push_str(&format!("Dataset: {}\n\n", markdown_escape_str(label)));
    if !is_valid && found.is_empty() {
        out.push_str("Finding details are off. Rerun with `--max-samples 20`.\n\n");
    }
    out.push_str(&format!(
        "**{}** · ",
        if is_valid {
            "Passed"
        } else {
            "Contract violations"
        }
    ));
    out.push_str(&format!(
        "{} violations · {} rows\n\n",
        scalar(violations),
        scalar(rows)
    ));
    out.push_str(&format!(
        "{} sampled finding(s); not a complete list when truncated. \
         Violations are not a count of distinct failing rows.\n\n",
        found.len()
    ));
    markdown_findings(found, &mut out)?;
    markdown_idle(&unexercised(report, column_names), &mut out);
    out.push_str(&format!(
        "\nDataset fingerprint: {}\n\n",
        fingerprint(evidence)?
    ));
    out.push_str("This review is local. Evidence is unsigned until signed separately.\n");
    Ok(out)
}
