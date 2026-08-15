//! Section preview, retrieval handle, and structure for markdown symbols.
//!
//! A markdown heading is one symbol in the code graph, and the graph stays
//! heading-level on purpose. But a heading alone is not a useful retrieval
//! result: a reader scanning `tracedecay_outline docs/plans/.../NEXT.md` wants
//! to know *what is under* "Remaining work" without pulling the whole file, and
//! then wants exactly one section's complete body.
//!
//! So each markdown section symbol carries a `section` lane with:
//!
//! - the **title** and the section's **1-based line span**, so
//!   `tracedecay_read mode=lines` is always the zero-magic alternative;
//! - a **truncated body preview** bounded by [`SECTION_PREVIEW_CHARS`];
//! - a **retrieval handle** for the *full* body, minted through the same
//!   response-handle cache that reversible MCP truncation uses
//!   ([`crate::response_handles::store_response_handle`]), so the reader
//!   dereferences it with the existing `tracedecay_retrieve` tool and no
//!   parallel mechanism exists;
//! - the section's load-bearing **structure** — task-list checkboxes with their
//!   checked state, nested bullets, ordered steps, tables, block quotes and
//!   fenced code — parsed by
//!   [`tracedecay_code_extraction::markdown_structure`], so "which checklist
//!   items under 'Remaining work' are unchecked" is answerable from the
//!   retrieval payload instead of by re-reading the file.
//!
//! A handle is minted only when the preview actually truncates: when the whole
//! body already fits in the preview, the reader is holding the full body and a
//! handle would be a pointless durable write. [`MAX_SECTION_HANDLES`] bounds how
//! many one response may mint, so a 300-heading document cannot turn one outline
//! call into 300 fsyncs; sections past the cap keep preview and line span.

use std::path::Path;

use serde_json::{Value, json};
use tracedecay_code_extraction::markdown_structure::{
    MarkdownSectionStructure, parse_section_structure,
};

use crate::response_handles::store_response_handle;

/// Characters of section body carried inline before the preview truncates.
pub const SECTION_PREVIEW_CHARS: usize = 320;

/// The existing tool that dereferences a minted handle. Never a new tool.
pub const SECTION_RETRIEVE_TOOL: &str = "tracedecay_retrieve";

/// Handles minted per response. Past this, sections keep preview + line span.
pub const MAX_SECTION_HANDLES: usize = 64;

/// The graph kind markdown headings are published as.
const MARKDOWN_SECTION_KIND: &str = "module";

/// `true` when `path` is a file the markdown extractor claims.
pub fn is_markdown_file(path: &str) -> bool {
    let extension = path.rsplit_once('.').map(|(_, extension)| extension);
    matches!(
        extension.map(str::to_ascii_lowercase).as_deref(),
        Some("md" | "markdown")
    )
}

/// Budgeted minting across one response's sections.
pub struct SectionEnrichment<'a> {
    project_root: Option<&'a Path>,
    now: i64,
    handles_minted: usize,
}

impl<'a> SectionEnrichment<'a> {
    pub fn new(project_root: Option<&'a Path>, now: i64) -> Self {
        Self {
            project_root,
            now,
            handles_minted: 0,
        }
    }

    /// Enriches every markdown section symbol in a `{"symbols": [...]}` payload
    /// in place, using `source` as the file's current text.
    ///
    /// This is an *enrichment*: a symbol the source cannot explain (no span, a
    /// span past the end of the file, a non-section kind) is left untouched
    /// rather than failing the surface that carries it.
    pub fn enrich_symbol_array(&mut self, symbols: &mut [Value], source: &str) {
        for symbol in symbols {
            if let Some(section) = self.section_for_symbol(symbol, source) {
                symbol["section"] = section;
            }
        }
    }

    fn section_for_symbol(&mut self, symbol: &Value, source: &str) -> Option<Value> {
        let kind = symbol.get("kind").and_then(Value::as_str)?;
        if !kind.eq_ignore_ascii_case(MARKDOWN_SECTION_KIND) {
            return None;
        }
        let title = symbol.get("name").and_then(Value::as_str)?;
        let start_line = symbol.get("line").and_then(Value::as_u64)? as u32;
        let end_line = symbol
            .get("end_line")
            .and_then(Value::as_u64)
            .unwrap_or(u64::from(start_line)) as u32;
        Some(self.section_value(source, title, start_line, end_line))
    }

    /// Builds one section's `section` lane. `start_line` is the 1-based heading
    /// line and `end_line` the 1-based inclusive last line of the section.
    pub fn section_value(
        &mut self,
        source: &str,
        title: &str,
        start_line: u32,
        end_line: u32,
    ) -> Value {
        let body_start = start_line.saturating_add(1);
        let body = section_body(source, body_start, end_line);
        let body_chars = body.chars().count();
        let (preview, preview_truncated) = preview_of(body);

        let mut value = json!({
            "title": title,
            "heading_line": start_line,
            "body_start_line": body_start,
            "body_end_line": end_line.max(start_line),
            "body_chars": body_chars,
            "preview": preview,
            "preview_truncated": preview_truncated,
        });
        // The span is published whether or not a handle exists, so the reader
        // always has the zero-magic route into the full body.
        if end_line >= body_start {
            value["read_lines"] = json!(format!("{body_start}-{end_line}"));
        }

        if preview_truncated {
            self.attach_handle(&mut value, body);
        } else {
            value["body_handle"] = Value::Null;
        }

        let structure = parse_section_structure(body, body_start);
        if !structure.is_empty() {
            value["structure"] = structure_value(&structure);
        }
        value
    }

    /// Mints the full-body handle through the shared response-handle cache.
    fn attach_handle(&mut self, value: &mut Value, body: &str) {
        let Some(root) = self.project_root else {
            value["body_handle"] = Value::Null;
            value["body_handle_unavailable"] = json!({
                "reason_code": "handle_storage_unavailable",
                "message": "This section preview was produced without a project-local cache path, so no retrieval handle could be created.",
                "retryable": true,
                "retry_instruction": "Re-run from a project-scoped tracedecay session, or read the section directly with tracedecay_read mode=lines.",
            });
            return;
        };
        if self.handles_minted >= MAX_SECTION_HANDLES {
            value["body_handle"] = Value::Null;
            value["body_handle_unavailable"] = json!({
                "reason_code": "handle_budget_exhausted",
                "message": format!(
                    "This response already minted {MAX_SECTION_HANDLES} section handles; read this section with tracedecay_read mode=lines instead."
                ),
                "retryable": true,
                "retry_instruction": "Narrow the request (for example with the outline `kinds` filter) so fewer sections compete for the handle budget.",
            });
            return;
        }
        match store_response_handle(root, body, self.now) {
            Ok(record) => {
                self.handles_minted += 1;
                value["body_handle"] = json!(record.handle);
                value["body_handle_expires_at"] = json!(record.expires_at);
                value["retrieve_with"] = json!(SECTION_RETRIEVE_TOOL);
            }
            // The handle cache records the typed error in its own telemetry.
            // Public output must not disclose project-local filesystem paths.
            Err(_) => {
                value["body_handle"] = Value::Null;
                value["body_handle_unavailable"] = json!({
                    "reason_code": "handle_store_failed",
                    "message": "The full section body could not be cached locally, so no retrieval handle is available.",
                    "retryable": true,
                    "retry_instruction": "Read the section with tracedecay_read mode=lines, or fix the local project cache path and re-run.",
                });
            }
        }
    }
}

/// Unchecked checklist items named inline before the summary elides the rest.
const SUMMARY_UNCHECKED_LIMIT: usize = 8;

/// Renders one section lane as human-facing summary lines, for a surface that
/// lists symbols as bullets.
///
/// The lines are returned rather than written so the transport layer keeps
/// ownership of its own markdown builder: an adapter emits each line under the
/// symbol's bullet. The preview is collapsed to a single line here because the
/// surrounding surface is a list; the verbatim preview stays in the JSON.
pub fn section_summary_lines(section: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    let preview = collapse_whitespace(field_str(section, "preview"));
    if !preview.is_empty() {
        let chars = section
            .get("body_chars")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        lines.push(format!("preview ({chars} chars): {preview}"));
    }
    let read_lines = field_str(section, "read_lines");
    match section.get("body_handle").and_then(Value::as_str) {
        Some(handle) => lines.push(format!(
            "full body: `{SECTION_RETRIEVE_TOOL}` handle `{handle}` (or `tracedecay_read mode=lines lines={read_lines}`)"
        )),
        None if section
            .get("preview_truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false) =>
        {
            let reason = section
                .get("body_handle_unavailable")
                .and_then(|value| value.get("reason_code"))
                .and_then(Value::as_str)
                .unwrap_or("handle_unavailable");
            lines.push(format!(
                "full body: no handle ({reason}); read `mode=lines lines={read_lines}`"
            ));
        }
        None => {}
    }
    if let Some(structure) = section.get("structure") {
        push_structure_lines(&mut lines, structure);
    }
    lines
}

fn push_structure_lines(lines: &mut Vec<String>, structure: &Value) {
    if let Some(checklist) = structure.get("checklist") {
        let total = checklist
            .get("total")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let checked = checklist
            .get("checked")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let mut line = format!("checklist: {checked}/{total} checked");
        let unchecked = checklist
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter(|item| {
                        !item
                            .get("checked")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                    })
                    .take(SUMMARY_UNCHECKED_LIMIT)
                    .map(|item| {
                        format!(
                            "L{} {}",
                            item.get("line").and_then(Value::as_u64).unwrap_or_default(),
                            collapse_whitespace(field_str(item, "text"))
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !unchecked.is_empty() {
            line.push_str(" · unchecked: ");
            line.push_str(&unchecked.join("; "));
        }
        lines.push(line);
    }
    let counts = [
        ("bullets", "bullets"),
        ("ordered", "ordered items"),
        ("tables", "tables"),
        ("block_quotes", "block quotes"),
        ("code_blocks", "code blocks"),
    ]
    .into_iter()
    .filter_map(|(key, label)| {
        let count = structure.get(key).and_then(Value::as_array)?.len();
        Some(format!("{count} {label}"))
    })
    .collect::<Vec<_>>();
    if !counts.is_empty() {
        lines.push(format!("structure: {}", counts.join(" · ")));
    }
}

fn field_str<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// Squashes newlines and runs of blanks so a multi-line preview stays on one
/// summary line.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The section body: 1-based inclusive lines `start ..= end`, empty when the
/// heading carries no body or the span points past the end of the file.
pub fn section_body(source: &str, start: u32, end: u32) -> &str {
    if end < start || start == 0 {
        return "";
    }
    let mut offset = 0usize;
    let mut body_start = None;
    for (index, line) in source.split_inclusive('\n').enumerate() {
        let line_number = index as u32 + 1;
        if line_number == start {
            body_start = Some(offset);
        }
        if line_number == end {
            let from = match body_start {
                Some(from) => from,
                None => return "",
            };
            return &source[from..offset + line.len()];
        }
        offset += line.len();
    }
    // A span that runs past the last line still describes real content: return
    // the tail rather than dropping the body.
    match body_start {
        Some(from) => &source[from..],
        None => "",
    }
}

/// `(preview, truncated)` for a section body.
fn preview_of(body: &str) -> (String, bool) {
    let trimmed = body.trim();
    let mut preview = String::new();
    let mut chars = trimmed.chars();
    for _ in 0..SECTION_PREVIEW_CHARS {
        match chars.next() {
            Some(ch) => preview.push(ch),
            None => return (preview, false),
        }
    }
    if chars.next().is_none() {
        return (preview, false);
    }
    // The body outran the preview budget. Truncation is reported against the
    // *whole* body, not the trimmed prefix, so `body_handle` is the only way to
    // see the rest.
    preview.push('…');
    (preview, true)
}

/// Publishes the parsed structure as JSON, with the counts a reader needs to
/// decide whether to pull the full body.
fn structure_value(structure: &MarkdownSectionStructure) -> Value {
    let mut value = json!({});
    if !structure.checklist.is_empty() {
        let checked = structure
            .checklist
            .iter()
            .filter(|item| item.checked)
            .count();
        value["checklist"] = json!({
            "total": structure.checklist.len(),
            "checked": checked,
            "unchecked": structure.checklist.len() - checked,
            "items": structure.checklist,
        });
    }
    if !structure.bullets.is_empty() {
        value["bullets"] = json!(structure.bullets);
    }
    if !structure.ordered.is_empty() {
        value["ordered"] = json!(structure.ordered);
    }
    if !structure.tables.is_empty() {
        value["tables"] = json!(structure.tables);
    }
    if !structure.block_quotes.is_empty() {
        value["block_quotes"] = json!(structure.block_quotes);
    }
    if !structure.code_blocks.is_empty() {
        value["code_blocks"] = json!(structure.code_blocks);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "\
# Plan

Intro line.

## Remaining work

- [x] wire the extractor
- [ ] mint the handle
  - [ ] nested follow-up
- prose bullet

| lane | owner |
| ---- | ----- |
| index | zack |

```rust
fn probe() {}
```

## Done

Nothing left.
";

    fn enrichment() -> SectionEnrichment<'static> {
        SectionEnrichment::new(None, 0)
    }

    #[test]
    fn markdown_extension_gate_matches_the_extractor() {
        assert!(is_markdown_file("docs/plans/NEXT.md"));
        assert!(is_markdown_file("README.MARKDOWN"));
        assert!(!is_markdown_file("src/main.rs"));
        assert!(!is_markdown_file("NEXT"));
    }

    #[test]
    fn section_body_is_the_lines_after_the_heading() {
        // "## Remaining work" is 1-based line 5, section ends at line 19.
        let body = section_body(DOC, 6, 19);
        assert!(body.starts_with("\n- [x] wire the extractor"));
        assert!(body.contains("fn probe()"));
        assert!(!body.contains("## Done"));
    }

    #[test]
    fn section_publishes_span_preview_and_structure() {
        let value = enrichment().section_value(DOC, "Remaining work", 5, 19);

        assert_eq!(value["title"], "Remaining work");
        assert_eq!(value["heading_line"], 5);
        assert_eq!(value["body_start_line"], 6);
        assert_eq!(value["read_lines"], "6-19");

        let checklist = &value["structure"]["checklist"];
        assert_eq!(checklist["total"], 3);
        assert_eq!(checklist["checked"], 1);
        assert_eq!(checklist["unchecked"], 2);
        // Checklist lines are absolute and 1-based, so they address the same
        // rows `tracedecay_read mode=lines` does.
        assert_eq!(checklist["items"][0]["line"], 7);
        assert_eq!(checklist["items"][0]["checked"], true);
        assert_eq!(checklist["items"][1]["text"], "mint the handle");
        assert_eq!(checklist["items"][2]["depth"], 1);

        assert_eq!(value["structure"]["bullets"][0]["text"], "prose bullet");
        assert_eq!(value["structure"]["tables"][0]["rows"], 1);
        assert_eq!(
            value["structure"]["code_blocks"][0]["language"].as_str(),
            Some("rust")
        );
    }

    #[test]
    fn short_sections_carry_the_whole_body_and_no_handle() {
        // "## Done" is 1-based line 20; its body is lines 21-22.
        let value = enrichment().section_value(DOC, "Done", 20, 22);

        assert_eq!(value["preview"], "Nothing left.");
        assert_eq!(value["preview_truncated"], false);
        assert_eq!(value["read_lines"], "21-22");
        // A body that already fits the preview needs no durable handle: the
        // reader is holding the whole section.
        assert_eq!(value["body_handle"], Value::Null);
        assert!(value.get("body_handle_unavailable").is_none());
    }

    #[test]
    fn oversized_sections_report_why_no_handle_exists_without_a_project_root() {
        let long = format!("# Big\n\n{}\n", "word ".repeat(400));
        let value = enrichment().section_value(&long, "Big", 1, 3);

        assert_eq!(value["preview_truncated"], true);
        assert_eq!(value["body_handle"], Value::Null);
        assert_eq!(
            value["body_handle_unavailable"]["reason_code"],
            "handle_storage_unavailable"
        );
    }

    #[test]
    fn only_markdown_section_symbols_are_enriched() {
        let mut symbols = vec![
            json!({"kind": "module", "name": "Remaining work", "line": 5, "end_line": 19}),
            json!({"kind": "function", "name": "probe", "line": 5, "end_line": 19}),
        ];
        enrichment().enrich_symbol_array(&mut symbols, DOC);

        assert_eq!(symbols[0]["section"]["title"], "Remaining work");
        assert!(symbols[1].get("section").is_none());
    }

    #[test]
    fn summary_lines_name_the_open_checklist_items_and_the_read_route() {
        let value = enrichment().section_value(DOC, "Remaining work", 5, 19);
        let lines = section_summary_lines(&value);
        let joined = lines.join("\n");

        assert!(joined.contains("checklist: 1/3 checked"), "{joined}");
        assert!(joined.contains("L8 mint the handle"), "{joined}");
        assert!(joined.contains("L9 nested follow-up"), "{joined}");
        assert!(joined.contains("1 code blocks"), "{joined}");
        // Every line must stay a single line: these are bullet continuations.
        assert!(lines.iter().all(|line| !line.contains('\n')), "{joined}");
    }

    #[test]
    fn symbols_without_a_published_span_are_left_untouched() {
        let mut symbols = vec![json!({"kind": "module", "name": "Orphan", "line": Value::Null})];
        enrichment().enrich_symbol_array(&mut symbols, DOC);

        assert!(symbols[0].get("section").is_none());
    }
}
