//! Output-format rendering for MCP tool responses.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

use crate::display::format_relative_time;
use crate::mcp::response_handles::{
    note_response_handle_store_skipped_no_project_root, observe_response_truncation,
    store_response_handle, ResponseHandleRecord, RESPONSE_HANDLE_TTL_SECS, RESPONSE_RETRIEVE_TOOL,
};
use crate::path_tree::format_compact_path_list;
use crate::tracedecay::current_timestamp;

use super::MAX_RESPONSE_CHARS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Markdown,
    Json,
}

fn parse_format(args: &Value) -> OutputFormat {
    match args.get("format").and_then(Value::as_str) {
        Some(v) if v.eq_ignore_ascii_case("json") => OutputFormat::Json,
        _ => OutputFormat::Markdown,
    }
}

/// True when the caller explicitly opted into JSON output via `format: "json"`.
pub(super) fn wants_json(args: &Value) -> bool {
    parse_format(args) == OutputFormat::Json
}

pub(super) fn finalize<F>(project_root: Option<&Path>, args: &Value, value: &Value, md: F) -> String
where
    F: FnOnce() -> String,
{
    match parse_format(args) {
        OutputFormat::Json => {
            let json = serde_json::to_string(value).unwrap_or_default();
            truncated_json_envelope_with_handle(project_root, &json)
        }
        OutputFormat::Markdown => {
            let text = md();
            if text.is_empty() {
                return text;
            }
            truncated_markdown_with_handle(project_root, &text)
        }
    }
}

/// Truncates a string to the maximum response character limit, appending
/// a truncation notice if necessary.
///
/// Legacy, irreversible truncation: no retrieval handle is stored. Prefer
/// [`truncate_text_with_handle`] for plain-text tool output.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn truncate_response(s: &str) -> String {
    debug_assert!(!s.is_empty(), "truncate_response called with empty string");
    if s.len() <= MAX_RESPONSE_CHARS {
        s.to_string()
    } else {
        let started = std::time::Instant::now();
        let now = current_timestamp();
        // Find a valid UTF-8 character boundary at or before MAX_RESPONSE_CHARS.
        let mut end = MAX_RESPONSE_CHARS;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        let truncated = format!("{}\n\n[... truncated at {} chars]", &s[..end], end);
        observe_response_truncation(
            s.len(),
            truncated.len(),
            false,
            now,
            "not_available",
            started.elapsed(),
        );
        truncated
    }
}

/// Wraps oversized JSON text in a valid preview envelope. When a project root
/// is available, stores the full original locally and includes a retrieval
/// handle.
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
    let mut end = formatted.len().min(MAX_RESPONSE_CHARS.saturating_sub(1024));
    loop {
        while end > 0 && !formatted.is_char_boundary(end) {
            end -= 1;
        }
        let preview = &formatted[..end];
        let mut envelope = serde_json::json!({
            "truncated": true,
            "original_chars": formatted.len(),
            "preview_chars": preview.len(),
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
                        "This response was truncated: `preview` contains only the first {} of {} characters. The full original response is stored locally in this project and expires at {} (TTL {} seconds). To recover it, call `{RESPONSE_RETRIEVE_TOOL}` with required argument `handle` set to `{}`. If the original tool call used a project selector (`project_id`, `project_path`, or `project_selector`), pass the same selector to `{RESPONSE_RETRIEVE_TOOL}` so the handle is looked up in the same project cache. Only call it if the missing details are needed to answer the user's request.",
                        preview.len(),
                        formatted.len(),
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
        let text = serde_json::to_string_pretty(&envelope).unwrap_or_default();
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

/// Reversible truncation for plain-text tool output. Returns `text` unchanged
/// when it fits within [`MAX_RESPONSE_CHARS`]; otherwise stores the full text
/// via the response-handle machinery and returns the readable markdown
/// truncation envelope (preview plus `rh_` retrieval handle).
pub(super) fn truncate_text_with_handle(project_root: Option<&Path>, text: &str) -> String {
    truncated_markdown_with_handle(project_root, text)
}

fn truncated_markdown_with_handle(project_root: Option<&Path>, text: &str) -> String {
    if text.len() <= MAX_RESPONSE_CHARS {
        return text.to_string();
    }
    let started = std::time::Instant::now();
    let now = current_timestamp();
    let handle = prepare_truncated_response_handle(project_root, text);
    let mut end = text.len().min(MAX_RESPONSE_CHARS.saturating_sub(2048));
    loop {
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        let preview = &text[..end];
        let rendered = render_markdown_truncation(preview, text.len(), &handle);
        if rendered.len() <= MAX_RESPONSE_CHARS || end == 0 {
            observe_response_truncation(
                text.len(),
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
            Err(err) => TruncatedResponseHandle {
                record: None,
                unavailable: Some(serde_json::json!({
                    "reason_code": "handle_store_failed",
                    "message": format!(
                        "The full response could not be cached locally, so no retrieval handle is available: {err}"
                    ),
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
    original_chars: usize,
    handle: &TruncatedResponseHandle,
) -> String {
    let mut rendered = String::new();
    rendered.push_str("# Truncated Response\n\n");
    let _ = writeln!(
        rendered,
        "Showing the first {} of {original_chars} characters.",
        preview.len()
    );
    if let Some(record) = &handle.record {
        let _ = writeln!(
            rendered,
            "Full response stored locally. Retrieve it with `{RESPONSE_RETRIEVE_TOOL}` using handle `{}` before {}.",
            record.handle,
            record.expires_at
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

fn esc_cell(s: &str) -> String {
    s.replace('|', "\\|").replace(['\n', '\r'], " ")
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

    pub(super) fn table(&mut self, headers: &[&str], rows: &[Vec<String>]) -> &mut Self {
        if headers.is_empty() {
            return self;
        }
        let _ = writeln!(self.buf, "| {} |", headers.join(" | "));
        let sep: Vec<&str> = headers.iter().map(|_| "---").collect();
        let _ = writeln!(self.buf, "| {} |", sep.join(" | "));
        for row in rows {
            let cells: Vec<String> = row.iter().map(|c| esc_cell(c)).collect();
            let _ = writeln!(self.buf, "| {} |", cells.join(" | "));
        }
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
            if let Some(f) = n.as_f64() {
                if n.as_i64().is_none() && n.as_u64().is_none() {
                    return format_score(f);
                }
            }
            n.to_string()
        }
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        _ => serde_json::to_string(v).unwrap_or_default(),
    }
}

/// Key-aware scalar rendering: humanizes epoch timestamps for `*_at`/`*_time`
/// keys and otherwise defers to [`scalar_str`] (which rounds floats).
fn scalar_str_keyed(key: &str, v: &Value) -> String {
    if is_timestamp_key(key) {
        if let Some(ts) = v.as_u64() {
            if ts > 100_000_000 {
                return format!("{} ({ts})", format_relative_time(ts));
            }
        }
    }
    scalar_str(v)
}

/// Renders a nested JSON value into a single table cell without dumping raw
/// JSON. Arrays of objects become `name (file:line)`-style summaries; plain
/// objects become compact `k=v` strings; arrays of scalars are comma-joined.
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

/// Compact one-line summary of an object for a table cell. Prefers a
/// `name (file:line)` shape when those keys exist, else `k=v` pairs.
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
        render_object_array_table(md, arr);
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

/// Renders an array of objects as a markdown table with three refinements:
/// columns are ordered (preferred keys first, rest alphabetical), columns that
/// are empty in every row are dropped, and columns whose value is constant
/// across every row are hoisted into a header line (`edge_kind: calls`) rather
/// than repeated in each cell.
fn render_object_array_table(md: &mut Md, arr: &[Value]) {
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
    let mut keep: Vec<usize> = Vec::new();
    for (ci, col) in cols.iter().enumerate() {
        let all_empty = rendered.iter().all(|row| row[ci].is_empty());
        if all_empty {
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
        return;
    }
    let headers: Vec<&str> = keep.iter().map(|&ci| cols[ci].as_str()).collect();
    let rows: Vec<Vec<String>> = rendered
        .iter()
        .map(|row| keep.iter().map(|&ci| row[ci].clone()).collect())
        .collect();
    md.table(&headers, &rows);
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
            md.line(&format!(
                "`{}`",
                serde_json::to_string(v).unwrap_or_default()
            ));
        } else {
            render_value(md, v, depth + 1);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
