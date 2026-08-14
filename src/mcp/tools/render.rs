//! Output-format rendering for MCP tool responses.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

use crate::context::CONTEXT_PRIORITY_HEADINGS;
use crate::daemon_client::RequestedOutputFormat;
use crate::display::format_relative_time;
use crate::mcp::response_handles::{
    RESPONSE_HANDLE_TTL_SECS, RESPONSE_RETRIEVE_TOOL, ResponseHandleRecord,
    note_response_handle_store_skipped_no_project_root, observe_response_truncation,
    store_response_handle,
};
use crate::path_tree::format_compact_path_list;
use crate::text::utf8_prefix_at_or_before;
use crate::tracedecay::current_timestamp;

use super::MAX_RESPONSE_CHARS;

const MARKDOWN_TRUNCATION_RESERVED_CHARS: usize = 2_048;

fn parse_format(args: &Value) -> RequestedOutputFormat {
    crate::application_surface::requested_output_format(args)
}

/// True when the caller explicitly opted into JSON output via `format: "json"`.
pub(super) fn wants_json(args: &Value) -> bool {
    parse_format(args) == RequestedOutputFormat::Json
}

pub(super) fn finalize<F>(project_root: Option<&Path>, args: &Value, value: &Value, md: F) -> String
where
    F: FnOnce() -> String,
{
    finalize_with_format(project_root, parse_format(args), value, md)
}

pub(super) fn finalize_with_format<F>(
    project_root: Option<&Path>,
    format: RequestedOutputFormat,
    value: &Value,
    md: F,
) -> String
where
    F: FnOnce() -> String,
{
    match format {
        RequestedOutputFormat::Json => {
            let json = value.to_string();
            truncated_json_envelope_with_handle(project_root, &json)
        }
        RequestedOutputFormat::Markdown => {
            let text = md();
            if text.is_empty() {
                return text;
            }
            truncated_markdown_with_handle(project_root, &text)
        }
    }
}

/// Wraps oversized JSON text in a valid preview envelope. With a project root,
/// stores the full original locally and includes a retrieval handle.
///
/// If local handle storage is unavailable or fails, the envelope still carries
/// a preview but also includes explicit recovery metadata so clients can tell
/// why no handle was emitted and what to retry.
pub(super) fn truncated_json_envelope_with_handle(
    project_root: Option<&Path>,
    formatted: &str,
) -> String {
    if formatted.len() <= MAX_RESPONSE_CHARS {
        return formatted.to_string();
    }
    let started = std::time::Instant::now();
    let now = current_timestamp();
    let handle = prepare_truncated_response_handle(project_root, formatted);
    let original_chars = formatted.chars().count();
    let mut end = formatted.len().min(MAX_RESPONSE_CHARS.saturating_sub(1024));
    loop {
        while end > 0 && !formatted.is_char_boundary(end) {
            end -= 1;
        }
        let preview = &formatted[..end];
        let mut envelope = serde_json::json!({
            "truncated": true,
            "original_chars": original_chars,
            "preview_chars": preview.chars().count(),
            "preview": preview,
        });
        if let Some(object) = envelope.as_object_mut() {
            if let Some(record) = &handle.record {
                object.insert("handle".to_string(), serde_json::json!(record.handle));
                object.insert(
                    "retrieve_tool".to_string(),
                    serde_json::json!(RESPONSE_RETRIEVE_TOOL),
                );
                object.insert(
                    "retrieve_ttl_seconds".to_string(),
                    serde_json::json!(RESPONSE_HANDLE_TTL_SECS),
                );
                object.insert(
                    "retrieve_expires_at".to_string(),
                    serde_json::json!(record.expires_at),
                );
                object.insert(
                    "retrieve_instruction".to_string(),
                    serde_json::json!(format!(
                        "This response was truncated: `preview` contains only the first {} of {} characters. The full original response is stored locally in this project and expires at {} (TTL {} seconds). To recover it, call `{RESPONSE_RETRIEVE_TOOL}` with required argument `handle` set to `{}`. If the original call used `project_selector.project_id`, pass the same selector so the handle is read from that project cache. Only call it if the missing details are needed.",
                        preview.chars().count(),
                        original_chars,
                        record.expires_at,
                        RESPONSE_HANDLE_TTL_SECS,
                        record.handle
                    )),
                );
            } else if let Some(status) = &handle.unavailable {
                object.insert("handle_available".to_string(), serde_json::json!(false));
                object.insert("handle_status".to_string(), status.clone());
            }
        }
        let text = envelope.to_string();
        if text.len() <= MAX_RESPONSE_CHARS || end == 0 {
            observe_response_truncation(
                formatted.len(),
                text.len(),
                // Reversible only when the full body was actually stored; a
                // failed/absent handle means the preview is all that survives.
                handle.record.is_some(),
                now,
                truncation_handle_status(project_root, &handle),
                started.elapsed(),
            );
            return text;
        }
        end = end.saturating_sub(1024);
    }
}

pub(super) fn markdown_preview_with_handle(
    project_root: Option<&Path>,
    full_text: &str,
    preview: &str,
) -> String {
    if full_text.len() <= MAX_RESPONSE_CHARS {
        return full_text.to_string();
    }
    if full_text == preview {
        return truncated_markdown_with_handle(project_root, full_text);
    }
    markdown_preview_truncation_with_handle(project_root, full_text, preview)
}

fn truncated_markdown_with_handle(project_root: Option<&Path>, text: &str) -> String {
    if text.len() <= MAX_RESPONSE_CHARS {
        return text.to_string();
    }
    render_markdown_truncation_with_handle(
        project_root,
        text,
        text.len(),
        |end| markdown_truncation_preview(text, end),
        |preview| {
            format!(
                "Showing the first {} of {} characters.",
                preview.chars().count(),
                text.chars().count()
            )
        },
    )
}

fn markdown_preview_truncation_with_handle(
    project_root: Option<&Path>,
    full_text: &str,
    preview: &str,
) -> String {
    render_markdown_truncation_with_handle(
        project_root,
        full_text,
        preview.len(),
        |end| {
            if preview.len() <= end {
                preview.to_string()
            } else {
                markdown_truncation_preview(preview, end)
            }
        },
        |compact_preview| {
            format!(
                "Showing a lane-budgeted preview of {} characters from {} original characters.",
                compact_preview.chars().count(),
                full_text.chars().count()
            )
        },
    )
}

fn render_markdown_truncation_with_handle(
    project_root: Option<&Path>,
    full_text: &str,
    mut end: usize,
    mut preview_for_end: impl FnMut(usize) -> String,
    preview_note: impl Fn(&str) -> String,
) -> String {
    let started = std::time::Instant::now();
    let now = current_timestamp();
    let handle = prepare_truncated_response_handle(project_root, full_text);
    end = end.min(MAX_RESPONSE_CHARS.saturating_sub(MARKDOWN_TRUNCATION_RESERVED_CHARS));
    loop {
        let preview = preview_for_end(end);
        let rendered = render_markdown_truncation(&preview, &handle, &preview_note(&preview));
        if rendered.len() <= MAX_RESPONSE_CHARS || end == 0 {
            observe_response_truncation(
                full_text.len(),
                rendered.len(),
                handle.record.is_some(),
                now,
                truncation_handle_status(project_root, &handle),
                started.elapsed(),
            );
            return rendered;
        }
        end = end.saturating_sub(1024);
    }
}

fn markdown_truncation_preview(text: &str, budget: usize) -> String {
    if text.len() <= budget {
        return text.to_string();
    }
    let has_late_priority = CONTEXT_PRIORITY_HEADINGS
        .iter()
        .any(|heading| text.find(heading).is_some_and(|idx| idx > budget));
    if !has_late_priority {
        return markdown_prefix_preview(text, budget);
    }

    let prefix_budget = budget.saturating_mul(2) / 3;
    let prefix = utf8_prefix_at_or_before(text, prefix_budget);
    let prefix_len = prefix.len();
    let mut preview = prefix.to_string();
    close_open_markdown_fence(&mut preview);
    let mut remaining = budget.saturating_sub(preview.len());
    let mut preserved = String::new();

    for heading in CONTEXT_PRIORITY_HEADINGS {
        let Some(start) = text.find(heading) else {
            continue;
        };
        if start < prefix_len {
            continue;
        }
        let section_header = if preserved.is_empty() {
            "\n\n## Preserved Priority Sections\n\n"
        } else {
            "\n"
        };
        if remaining <= section_header.len() + 96 {
            break;
        }
        preserved.push_str(section_header);
        remaining -= section_header.len();

        let section = markdown_section_at(text, start);
        let section_preview = utf8_prefix_at_or_before(section, remaining);
        preserved.push_str(section_preview);
        let section_truncated = section_preview.len() < section.len();
        close_open_markdown_fence(&mut preserved);
        remaining = budget.saturating_sub(preview.len() + preserved.len());
        if section_truncated && remaining >= 5 {
            preserved.push_str("\n...");
            remaining -= 5;
        }
    }

    if preserved.is_empty() {
        return markdown_prefix_preview(text, budget);
    }
    preview.push_str(&preserved);
    preview
}

fn markdown_prefix_preview(text: &str, budget: usize) -> String {
    let mut preview = utf8_prefix_at_or_before(text, budget).to_string();
    close_open_markdown_fence(&mut preview);
    preview
}

fn markdown_section_at(text: &str, start: usize) -> &str {
    let rest = &text[start..];
    let end = ["\n## ", "\n### "]
        .into_iter()
        .filter_map(|marker| rest[1..].find(marker).map(|idx| idx + 1))
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

fn close_open_markdown_fence(markdown: &mut String) {
    if has_open_markdown_fence(markdown) {
        if !markdown.ends_with('\n') {
            markdown.push('\n');
        }
        markdown.push_str("```");
    }
}

pub(in crate::mcp::tools) fn has_open_markdown_fence(markdown: &str) -> bool {
    markdown
        .lines()
        .filter(|line| line.trim_start().starts_with("```"))
        .count()
        % 2
        == 1
}

struct TruncatedResponseHandle {
    record: Option<ResponseHandleRecord>,
    unavailable: Option<Value>,
}

fn truncation_handle_status(
    project_root: Option<&Path>,
    handle: &TruncatedResponseHandle,
) -> &'static str {
    if handle.record.is_some() {
        "stored"
    } else if project_root.is_none() {
        "no_project_root"
    } else {
        "store_failed"
    }
}

fn prepare_truncated_response_handle(
    project_root: Option<&Path>,
    text: &str,
) -> TruncatedResponseHandle {
    if let Some(root) = project_root {
        match store_response_handle(root, text, current_timestamp()) {
            Ok(record) => TruncatedResponseHandle {
                record: Some(record),
                unavailable: None,
            },
            // The adapter records the full typed error in internal telemetry.
            // Public output must not disclose project-local filesystem paths.
            Err(_) => TruncatedResponseHandle {
                record: None,
                unavailable: Some(serde_json::json!({
                    "reason_code": "handle_store_failed",
                    "message": "The full response could not be cached locally, so no retrieval handle is available.",
                    "retryable": true,
                    "retry_instruction": "Fix the local project cache path or filesystem error, then re-run the original MCP tool to regenerate the full response and a fresh handle."
                })),
            },
        }
    } else {
        note_response_handle_store_skipped_no_project_root();
        TruncatedResponseHandle {
            record: None,
            unavailable: Some(serde_json::json!({
                "reason_code": "handle_storage_unavailable",
                "message": "This response was truncated in a context without a project-local cache path, so no retrieval handle could be created.",
                "retryable": true,
                "retry_instruction": "Re-run the original MCP tool from a project-scoped tracedecay session if you need a retrievable full response."
            })),
        }
    }
}

fn render_markdown_truncation(
    preview: &str,
    handle: &TruncatedResponseHandle,
    preview_note: &str,
) -> String {
    let mut rendered = String::new();
    rendered.push_str("# Truncated Response\n\n");
    let _ = writeln!(rendered, "{preview_note}");
    if let Some(record) = &handle.record {
        let _ = writeln!(
            rendered,
            "Full response stored locally. Retrieve it with `{RESPONSE_RETRIEVE_TOOL}` using handle `{}` before {}.",
            record.handle, record.expires_at
        );
    } else if let Some(status) = &handle.unavailable {
        let message = status
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("No retrieval handle is available.");
        let _ = writeln!(rendered, "{message}");
    }
    rendered.push_str("\n## Preview\n\n");
    rendered.push_str(preview);
    if !preview.ends_with('\n') {
        rendered.push('\n');
    }
    rendered
}

pub(super) fn field_str<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

pub(super) fn field_i64(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

#[derive(Default)]
pub(super) struct Md {
    buf: String,
}

impl Md {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn heading(&mut self, level: u8, text: &str) -> &mut Self {
        let hashes = "#".repeat(level.clamp(1, 6) as usize);
        let _ = writeln!(self.buf, "{hashes} {text}");
        self
    }

    pub(super) fn field(&mut self, key: &str, value: &str) -> &mut Self {
        let _ = writeln!(self.buf, "**{key}:** {value}");
        self
    }

    pub(super) fn line(&mut self, text: &str) -> &mut Self {
        let _ = writeln!(self.buf, "{text}");
        self
    }

    pub(super) fn bullet(&mut self, text: &str) -> &mut Self {
        let _ = writeln!(self.buf, "- {text}");
        self
    }

    pub(super) fn empty_note(&mut self, text: &str) -> &mut Self {
        let _ = writeln!(self.buf, "_{text}_");
        self
    }

    pub(super) fn blank(&mut self) -> &mut Self {
        self.buf.push('\n');
        self
    }

    pub(super) fn code(&mut self, lang: &str, body: &str) -> &mut Self {
        let _ = writeln!(self.buf, "```{lang}");
        self.buf.push_str(body);
        if !body.ends_with('\n') {
            self.buf.push('\n');
        }
        self.buf.push_str("```\n");
        self
    }

    pub(super) fn render(self) -> String {
        self.buf
    }
}

const GENERIC_MAX_DEPTH: u8 = 4;

pub(super) fn generic_md(value: &Value) -> String {
    let mut md = Md::new();
    render_value(&mut md, value, 2);
    let out = md.render();
    if out.trim().is_empty() {
        "_No results._\n".to_string()
    } else {
        out
    }
}

pub(super) fn diagnostics_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Diagnostics");
    for key in [
        "status",
        "scope",
        "diagnostics_parsed",
        "diagnostics_returned",
        "diagnostic_count",
        "error_count",
        "warning_count",
        "mapped_to_node",
        "unmapped",
        "truncated",
        "target_dir",
    ] {
        if let Some(v) = value.get(key).filter(|v| is_scalar(v)) {
            md.field(title_label(key), &cell_str(key, v));
        }
    }
    if let Some(message) = value.get("message").and_then(Value::as_str) {
        md.field("Message", &indent_multiline_value(message));
    }
    md.blank();

    let diagnostics = value
        .get("diagnostics")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    if diagnostics.is_empty() {
        md.empty_note("No diagnostics.");
        return md.render();
    }

    md.heading(3, "Findings");
    for diagnostic in diagnostics {
        render_diagnostic_record(&mut md, diagnostic);
    }
    md.render()
}

/// Renders the `tracedecay_unsafe_patterns` payload
/// (`{ match_count, by_kind, matches: [...] }`).
///
/// This shape is distinct from the compiler-`diagnostics` shape rendered by
/// [`diagnostics_md`]: each match carries a `kind`, `file`, `line`, `snippet`,
/// `enclosing`, and `in_test` field rather than a `level`/`code`/`message`.
/// Feeding it through `diagnostics_md` silently dropped every finding (the
/// `diagnostics` key is absent, so it always printed "No diagnostics."), which
/// is why the tool appeared to return nothing even when matches existed.
pub(super) fn risky_patterns_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Risky Patterns");

    let matches = value
        .get("matches")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);

    let match_count = value
        .get("match_count")
        .and_then(Value::as_u64)
        .unwrap_or(matches.len() as u64);
    md.field("Match count", &match_count.to_string());

    if let Some(by_kind) = value.get("by_kind").and_then(Value::as_object)
        && !by_kind.is_empty()
    {
        let mut entries: Vec<(String, u64)> = by_kind
            .iter()
            .map(|(k, v)| (k.clone(), v.as_u64().unwrap_or(0)))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let summary = entries
            .iter()
            .map(|(kind, count)| format!("{kind}: {count}"))
            .collect::<Vec<_>>()
            .join(", ");
        md.field("By kind", &summary);
    }
    md.blank();

    if matches.is_empty() {
        md.empty_note("No risky patterns found.");
        return md.render();
    }

    md.heading(3, "Findings");
    for m in matches {
        let kind = m.get("kind").and_then(Value::as_str).unwrap_or("pattern");
        let file = m.get("file").and_then(Value::as_str).unwrap_or("<unknown>");
        let line = m.get("line").and_then(Value::as_u64).unwrap_or(0);
        md.bullet(&format!("**{} at {file}:{line}**", kind.to_uppercase()));
        if let Some(snippet) = m.get("snippet").and_then(Value::as_str)
            && !snippet.is_empty()
        {
            md.line(&format!("  **Snippet:** {snippet}"));
        }
        if let Some(enclosing) = m.get("enclosing").and_then(Value::as_str)
            && !enclosing.is_empty()
        {
            md.line(&format!("  **Enclosing:** {enclosing}"));
        }
        if m.get("in_test").and_then(Value::as_bool).unwrap_or(false) {
            md.line("  **In test:** true");
        }
    }
    md.render()
}

/// Dedicated markdown renderer for `tracedecay_unused_imports`.
///
/// The generic renderer dumped the `imports` array as anonymous records; this
/// spells each finding as `NAME unused in file:line` so agents (and tests) get
/// a stable, greppable location — the same shape as [`risky_patterns_md`].
pub(super) fn unused_imports_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Unused Imports");

    let imports = value
        .get("imports")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let count = value
        .get("unused_import_count")
        .and_then(Value::as_u64)
        .unwrap_or(imports.len() as u64);
    md.field("Unused import count", &count.to_string());
    // A paged walk must never read as a whole-repository verdict: state the
    // scanned scope and how to resume before listing findings.
    let complete = value.get("complete").and_then(Value::as_bool);
    if let Some(scanned) = value.get("scanned_files").and_then(Value::as_u64) {
        md.field("Files scanned", &scanned.to_string());
    }
    if complete == Some(false) {
        md.field("Coverage", "partial");
        if let Some(reason) = value.get("partial_reason").and_then(Value::as_str) {
            md.field("Partial reason", reason);
        }
        if let Some(cursor) = value.get("next_cursor").and_then(Value::as_str) {
            md.field("Resume with cursor", cursor);
        }
    }
    md.blank();

    if imports.is_empty() {
        if complete == Some(false) {
            md.empty_note("No unused imports in the scanned page; the walk is incomplete.");
        } else {
            md.empty_note("No unused imports found.");
        }
        return md.render();
    }

    md.heading(3, "Findings");
    for imp in imports {
        let name = imp
            .get("unused")
            .and_then(Value::as_str)
            .or_else(|| imp.get("name").and_then(Value::as_str));
        let file = imp
            .get("file")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let line = imp.get("line").and_then(Value::as_u64).unwrap_or(0);
        let label = name.unwrap_or("<unknown>");
        md.bullet(&format!("**{label} unused in {file}:{line}**"));
        // Show the full import path when it differs from the flagged identifier
        // (grouped/aliased imports like `foo::{a, b as c}`).
        if let Some(full) = imp.get("name").and_then(Value::as_str)
            && Some(full) != name
        {
            md.line(&format!("  **Import:** {full}"));
        }
    }
    md.render()
}

/// Dedicated markdown renderer for `tracedecay_unmounted_files`.
///
/// An empty answer here is a real and welcome verdict — "every source file is
/// reachable" — so it is spelled out rather than left as the generic renderer's
/// silence. The per-ecosystem section is not decoration: "unmounted" means
/// something stronger for cargo than for a bundler, and a language nobody
/// modelled must say so out loud rather than let a clean report imply coverage
/// it never had. When findings exist, each one leads with the repair (`add
/// `mod foo;` to src/daemon.rs`) because the reader's next action is an edit,
/// not further investigation.
pub(super) fn unmounted_files_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Unmounted Files");

    let unmounted = value
        .get("unmounted")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let count = value
        .get("unmounted_file_count")
        .and_then(Value::as_u64)
        .unwrap_or(unmounted.len() as u64);
    md.field("Unmounted file count", &count.to_string());
    // A truncated list must never read as the whole finding.
    if value.get("complete").and_then(Value::as_bool) == Some(false)
        && let Some(omitted) = value.get("omitted_count").and_then(Value::as_u64)
    {
        md.field("Coverage", "partial");
        md.field("Omitted", &format!("{omitted} (raise `limit` to see them)"));
    }
    md.blank();

    let ecosystems = value
        .get("ecosystems")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    if !ecosystems.is_empty() {
        md.heading(3, "Ecosystems");
        for ecosystem in ecosystems {
            render_ecosystem(&mut md, ecosystem);
        }
        md.blank();
    }

    if unmounted.is_empty() {
        let audited = ecosystems.iter().any(|ecosystem| {
            ecosystem.get("status").and_then(Value::as_str) == Some("audited")
        });
        if audited {
            md.empty_note(
                "Every scanned source file is reachable from a declared entry point in the \
                 ecosystems listed above.",
            );
        } else {
            md.empty_note(
                "No package of a modelled ecosystem (cargo, npm) was found; nothing was audited.",
            );
        }
        return md.render();
    }

    md.heading(3, "Findings");
    for entry in unmounted {
        let file = entry
            .get("file")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let ecosystem = entry.get("ecosystem").and_then(Value::as_str).unwrap_or("");
        let package = entry.get("package").and_then(Value::as_str).unwrap_or("");
        md.bullet(&format!("**{file}** ({ecosystem} package `{package}`)"));
        match (
            entry.get("suggested_declaration").and_then(Value::as_str),
            entry.get("nearest_mounted_parent").and_then(Value::as_str),
        ) {
            (Some(declaration), Some(parent)) => {
                md.line(&format!("  **Fix:** add `{declaration}` to {parent}"))
            }
            (Some(declaration), None) => md.line(&format!(
                "  **Fix:** no mounted ancestor exists; the whole branch needs a root, then `{declaration}`"
            )),
            // No canonical repair: the file is either dead or reached through
            // a blind spot, and naming an importer would invent one.
            (None, _) => md.line(
                "  **Next:** delete it, or confirm it is reached through a blind spot listed above",
            ),
        };
    }
    md.render()
}

/// One ecosystem's line in the report, including what its verdict claims.
fn render_ecosystem(md: &mut Md, ecosystem: &Value) {
    let name = ecosystem
        .get("ecosystem")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    let status = ecosystem
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let number = |key: &str| ecosystem.get(key).and_then(Value::as_u64).unwrap_or(0);
    let findings = number("unmounted_file_count");
    md.bullet(&format!(
        "**{name}** — {status} · {} package(s) · {} entry point(s) · {} file(s) scanned · {findings} unmounted",
        number("package_count"),
        number("entry_point_count"),
        number("scanned_file_count"),
    ));
    if let Some(note) = ecosystem.get("note").and_then(Value::as_str) {
        md.line(&format!("  {note}"));
    }
    if status != "audited" {
        return;
    }
    if let Some(verdict) = ecosystem.get("verdict").and_then(Value::as_str) {
        md.line(&format!("  **Unmounted here means:** {verdict}"));
    }
    // Blind spots are what turns a finding into a judgement, so they ride with
    // the findings rather than with the clean runs.
    if findings == 0 {
        return;
    }
    for blind_spot in ecosystem
        .get("blind_spots")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(Value::as_str)
    {
        md.line(&format!("  **Blind spot:** {blind_spot}"));
    }
}

fn render_diagnostic_record(md: &mut Md, diagnostic: &Value) {
    let level = diagnostic
        .get("level")
        .or_else(|| diagnostic.get("severity"))
        .and_then(Value::as_str)
        .unwrap_or("diagnostic");
    let location = diagnostic_location(diagnostic);
    let code = diagnostic.get("code").and_then(Value::as_str).unwrap_or("");
    let title = if code.is_empty() {
        format!("{} at {location}", level.to_uppercase())
    } else {
        format!("{} {code} at {location}", level.to_uppercase())
    };
    md.bullet(&format!("**{title}**"));
    if let Some(message) = diagnostic.get("message").and_then(Value::as_str) {
        md.line(&format!(
            "  **Message:** {}",
            indent_multiline_value(message)
        ));
    }
    if let Some(driver) = diagnostic.get("driver").and_then(Value::as_str) {
        md.line(&format!("  **Driver:** {driver}"));
    }
    if let Some(enclosing) = diagnostic.get("enclosing").and_then(Value::as_str)
        && !enclosing.is_empty()
    {
        md.line(&format!("  **Enclosing:** {enclosing}"));
    }
    if let Some(node) = diagnostic.get("node").filter(|v| !v.is_null()) {
        let name = node
            .get("qualified_name")
            .or_else(|| node.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !name.is_empty() {
            md.line(&format!("  **Node:** {name}"));
        }
    }
    if let Some(callers) = diagnostic.get("callers").and_then(Value::as_array) {
        let callers = callers
            .iter()
            .filter_map(|caller| {
                let name = caller.get("name").and_then(Value::as_str)?;
                let file = caller.get("file").and_then(Value::as_str).unwrap_or("");
                let line = caller.get("line").and_then(Value::as_u64).unwrap_or(0);
                Some(if file.is_empty() {
                    name.to_string()
                } else {
                    format!("{name} ({file}:{line})")
                })
            })
            .collect::<Vec<_>>();
        if !callers.is_empty() {
            md.line(&format!("  **Callers:** {}", callers.join(", ")));
        }
    }
    if let Some(dupes) = diagnostic.get("near_duplicates").and_then(Value::as_array) {
        let dupes = dupes
            .iter()
            .filter_map(|dupe| {
                let name = dupe.get("name").and_then(Value::as_str)?;
                let file = dupe.get("file").and_then(Value::as_str).unwrap_or("");
                let line = dupe.get("line").and_then(Value::as_u64).unwrap_or(0);
                let kind = dupe
                    .get("overlap_kind")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                Some(if file.is_empty() {
                    format!("{name} [{kind}]")
                } else {
                    format!("{name} ({file}:{line}) [{kind}]")
                })
            })
            .collect::<Vec<_>>();
        if !dupes.is_empty() {
            md.line(&format!("  **Near-duplicates:** {}", dupes.join(", ")));
        }
    }
}

fn diagnostic_location(diagnostic: &Value) -> String {
    let file = diagnostic
        .get("file")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    let line = diagnostic
        .get("line_start")
        .or_else(|| diagnostic.get("line"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let column = diagnostic.get("column").and_then(Value::as_u64);
    match column {
        Some(column) if column > 0 => format!("{file}:{line}:{column}"),
        _ => format!("{file}:{line}"),
    }
}

fn title_label(key: &str) -> &str {
    match key {
        "diagnostics_parsed" => "Diagnostics parsed",
        "diagnostics_returned" => "Diagnostics returned",
        "diagnostic_count" => "Diagnostic count",
        "error_count" => "Error count",
        "warning_count" => "Warning count",
        "mapped_to_node" => "Mapped to node",
        "target_dir" => "Target dir",
        "status" => "Status",
        "scope" => "Scope",
        "unmapped" => "Unmapped",
        "truncated" => "Truncated",
        _ => key,
    }
}

fn indent_multiline_value(value: &str) -> String {
    value.replace('\n', "\n    ")
}

fn is_id_key(k: &str) -> bool {
    matches!(k, "id" | "node_id" | "qualified_name" | "signature") || k.ends_with("_id")
}

/// True for keys that carry a UNIX epoch timestamp worth humanizing
/// (e.g. `created_at`, `last_sync_time`, `expires_at`).
fn is_timestamp_key(k: &str) -> bool {
    k.ends_with("_at") || k.ends_with("_time") || k == "timestamp"
}

fn is_scalar(v: &Value) -> bool {
    !v.is_array() && !v.is_object()
}

/// Rounds a float to at most two decimals, trimming trailing zeros so
/// integers stay integer-looking. Fixes 16-digit score noise
/// (`similar/branch_search`).
fn format_score(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        return format!("{}", f as i64);
    }
    let s = format!("{f:.2}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    trimmed.to_string()
}

fn scalar_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => {
            if let Some(f) = n.as_f64()
                && n.as_i64().is_none()
                && n.as_u64().is_none()
            {
                return format_score(f);
            }
            n.to_string()
        }
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        _ => v.to_string(),
    }
}

/// Key-aware scalar rendering: humanizes epoch timestamps for `*_at`/`*_time`
/// keys and otherwise defers to [`scalar_str`] (which rounds floats).
fn scalar_str_keyed(key: &str, v: &Value) -> String {
    if is_timestamp_key(key)
        && let Some(ts) = v.as_u64()
        && ts > 100_000_000
    {
        return format!("{} ({ts})", format_relative_time(ts));
    }
    scalar_str(v)
}

fn nested_cell_str(v: &Value) -> String {
    const MAX_ITEMS: usize = 3;

    match v {
        Value::Array(arr) => {
            if arr.is_empty() {
                return "none".to_string();
            }
            let shown: Vec<String> = arr
                .iter()
                .take(MAX_ITEMS)
                .map(|e| match e {
                    Value::Object(obj) => summarize_object(obj),
                    _ => scalar_str(e),
                })
                .collect();
            if arr.len() > MAX_ITEMS {
                format!(
                    "{} … (+{} more, {} total)",
                    shown.join("; "),
                    arr.len() - MAX_ITEMS,
                    arr.len()
                )
            } else {
                shown.join("; ")
            }
        }
        Value::Object(obj) => summarize_object(obj),
        _ => scalar_str(v),
    }
}

fn summarize_object(obj: &serde_json::Map<String, Value>) -> String {
    let name = obj
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| obj.get("id").and_then(Value::as_str));
    let file = obj.get("file").and_then(Value::as_str);
    let line = obj.get("line").and_then(Value::as_i64);
    if let Some(name) = name {
        return match (file, line) {
            (Some(f), Some(l)) => format!("{name} ({f}:{l})"),
            (Some(f), None) => format!("{name} ({f})"),
            _ => name.to_string(),
        };
    }
    let pairs: Vec<String> = obj
        .iter()
        .filter(|(_, v)| is_scalar(v))
        .map(|(k, v)| format!("{k}={}", scalar_str(v)))
        .collect();
    if pairs.is_empty() {
        "{…}".to_string()
    } else {
        pairs.join(", ")
    }
}

fn cell_str(key: &str, v: &Value) -> String {
    let s = if is_scalar(v) {
        scalar_str_keyed(key, v)
    } else {
        nested_cell_str(v)
    };
    if is_id_key(key) && !s.is_empty() {
        format!("`{s}`")
    } else {
        s
    }
}

fn render_value(md: &mut Md, value: &Value, depth: u8) {
    match value {
        Value::Array(arr) => render_array(md, arr, depth),
        Value::Object(map) => render_object(md, map, depth),
        other => {
            md.line(&scalar_str(other));
        }
    }
}

fn render_array(md: &mut Md, arr: &[Value], depth: u8) {
    if arr.is_empty() {
        md.empty_note("None.");
        return;
    }
    if let Some(paths) = compact_path_array(arr) {
        md.line(&paths);
        return;
    }
    if arr.iter().all(Value::is_object) {
        render_object_array_records(md, arr);
    } else {
        for e in arr {
            if is_scalar(e) {
                md.bullet(&scalar_str(e));
            } else {
                md.bullet("");
                render_value(md, e, depth + 1);
            }
        }
    }
}

/// Preferred left-to-right ordering for well-known columns; everything else
/// sorts alphabetically after these.
const PREFERRED_COLUMNS: &[&str] = &["name", "kind", "file", "line", "id", "signature"];

fn column_rank(col: &str) -> (usize, &str) {
    match PREFERRED_COLUMNS.iter().position(|c| *c == col) {
        Some(i) => (i, col),
        None => (PREFERRED_COLUMNS.len(), col),
    }
}

fn render_object_array_records(md: &mut Md, arr: &[Value]) {
    // Collect the union of keys.
    let mut cols: Vec<String> = Vec::new();
    for e in arr {
        if let Some(obj) = e.as_object() {
            for k in obj.keys() {
                if !cols.contains(k) {
                    cols.push(k.clone());
                }
            }
        }
    }
    cols.sort_by(|a, b| column_rank(a).cmp(&column_rank(b)));

    // Precompute each cell's rendered string once.
    let rendered: Vec<Vec<String>> = arr
        .iter()
        .map(|e| {
            cols.iter()
                .map(|c| cell_str(c, e.get(c).unwrap_or(&Value::Null)))
                .collect()
        })
        .collect();

    // Drop columns empty in every row; hoist columns constant across all rows.
    let mut hoisted: Vec<(String, String)> = Vec::new();
    let mut dropped_empty: Vec<String> = Vec::new();
    let mut keep: Vec<usize> = Vec::new();
    for (ci, col) in cols.iter().enumerate() {
        let all_empty = rendered.iter().all(|row| row[ci].is_empty());
        if all_empty {
            dropped_empty.push(col.clone());
            continue;
        }
        let first = &rendered[0][ci];
        let constant = rendered.len() > 1 && rendered.iter().all(|row| &row[ci] == first);
        if constant {
            hoisted.push((col.clone(), first.clone()));
        } else {
            keep.push(ci);
        }
    }

    for (col, val) in &hoisted {
        md.field(col, val);
    }
    if !hoisted.is_empty() && !keep.is_empty() {
        md.blank();
    }

    if keep.is_empty() {
        if hoisted.is_empty() {
            let dropped = if dropped_empty.is_empty() {
                "none".to_string()
            } else {
                dropped_empty.join(", ")
            };
            md.empty_note(&format!(
                "No visible fields across {} rows; dropped empty keys: {dropped}.",
                arr.len()
            ));
        }
        return;
    }
    let title_ci = keep
        .iter()
        .copied()
        .find(|ci| matches!(cols[*ci].as_str(), "name" | "symbol" | "file" | "path"))
        .unwrap_or(keep[0]);
    for (idx, row) in rendered.iter().enumerate() {
        let title = if row[title_ci].is_empty() {
            format!("Item {}", idx + 1)
        } else {
            row[title_ci].clone()
        };
        md.bullet(&format!("**{title}**"));
        for &ci in &keep {
            if ci == title_ci || row[ci].is_empty() {
                continue;
            }
            let value = indent_multiline_value(&row[ci]);
            md.line(&format!("  **{}:** {}", cols[ci], value));
        }
    }
}

fn compact_path_array(arr: &[Value]) -> Option<String> {
    if arr.len() < 2 || !arr.iter().all(Value::is_string) {
        return None;
    }
    let paths = arr.iter().filter_map(Value::as_str).collect::<Vec<_>>();
    if !paths.iter().all(|path| looks_like_path(path)) {
        return None;
    }
    let bullets = paths
        .iter()
        .map(|path| format!("- {path}"))
        .collect::<Vec<_>>()
        .join("\n");
    let compact = format_compact_path_list(paths.iter().copied(), "- ", "");
    if compact == bullets {
        None
    } else {
        Some(compact)
    }
}

fn looks_like_path(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && !trimmed.contains('\n')
        && !trimmed.contains("://")
        && (trimmed.contains('/') || trimmed.contains('\\'))
}

fn is_empty_collection(v: &Value) -> bool {
    matches!(v, Value::Array(a) if a.is_empty()) || matches!(v, Value::Object(o) if o.is_empty())
}

fn render_object(md: &mut Md, map: &serde_json::Map<String, Value>, depth: u8) {
    for (k, v) in map {
        if is_scalar(v) {
            let cell = cell_str(k, v);
            // Omit empty scalar fields entirely (no bare "**docstring:** ").
            if cell.is_empty() {
                continue;
            }
            md.field(k, &cell);
        }
    }
    for (k, v) in map {
        if is_scalar(v) {
            continue;
        }
        // Empty collections collapse to a single "section: none" line rather
        // than a bare heading with no body.
        if is_empty_collection(v) {
            md.line(&format!("{k}: none"));
            continue;
        }
        md.blank().heading(depth.min(6), k);
        if depth >= GENERIC_MAX_DEPTH {
            md.line(&format!("`{}`", v));
        } else {
            render_value(md, v, depth + 1);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
