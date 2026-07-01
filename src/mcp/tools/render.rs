//! Output-format rendering for MCP tool responses.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

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
                true,
                now,
                truncation_handle_status(project_root, &handle),
                started.elapsed(),
            );
            return text;
        }
        end = end.saturating_sub(1024);
    }
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

fn is_scalar(v: &Value) -> bool {
    !v.is_array() && !v.is_object()
}

fn scalar_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        _ => serde_json::to_string(v).unwrap_or_default(),
    }
}

fn cell_str(key: &str, v: &Value) -> String {
    let s = if is_scalar(v) {
        scalar_str(v)
    } else {
        serde_json::to_string(v).unwrap_or_default()
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
        let headers: Vec<&str> = cols.iter().map(String::as_str).collect();
        let rows: Vec<Vec<String>> = arr
            .iter()
            .map(|e| {
                cols.iter()
                    .map(|c| cell_str(c, e.get(c).unwrap_or(&Value::Null)))
                    .collect()
            })
            .collect();
        md.table(&headers, &rows);
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

fn render_object(md: &mut Md, map: &serde_json::Map<String, Value>, depth: u8) {
    for (k, v) in map {
        if is_scalar(v) {
            md.field(k, &cell_str(k, v));
        }
    }
    for (k, v) in map {
        if is_scalar(v) {
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
mod tests {
    use super::*;
    use crate::mcp::response_handles::{retrieve_response_handle_from_root, ResponseHandleLookup};
    use crate::tracedecay::current_timestamp;
    use serde_json::json;

    #[test]
    fn default_format_is_markdown() {
        assert_eq!(parse_format(&json!({})), OutputFormat::Markdown);
        assert_eq!(
            parse_format(&json!({"format": "markdown"})),
            OutputFormat::Markdown
        );
        assert_eq!(
            parse_format(&json!({"format": "md"})),
            OutputFormat::Markdown
        );
        assert_eq!(parse_format(&json!({"format": "json"})), OutputFormat::Json);
        assert_eq!(parse_format(&json!({"format": "JSON"})), OutputFormat::Json);
        assert_eq!(
            parse_format(&json!({"format": "yaml"})),
            OutputFormat::Markdown
        );
    }

    #[test]
    fn json_format_is_compact() {
        let value = json!({"a": 1, "b": [1, 2]});
        let out = finalize(None, &json!({"format": "json"}), &value, || {
            "unused".to_string()
        });
        assert_eq!(out, "{\"a\":1,\"b\":[1,2]}");
        assert!(
            !out.contains('\n'),
            "compact json must not be pretty-printed"
        );
    }

    #[test]
    fn markdown_format_uses_closure() {
        let value = json!({"a": 1});
        let out = finalize(None, &json!({}), &value, || "## Hi\n".to_string());
        assert_eq!(out, "## Hi\n");
    }

    #[test]
    fn truncate_short_response() {
        let short = "hello world";
        assert_eq!(truncate_response(short), short);
    }

    #[test]
    fn truncate_long_response() {
        let long = "x".repeat(20_000);
        let result = truncate_response(&long);
        assert!(result.len() < 20_000);
        assert!(result.contains("[... truncated at 15000 chars]"));
    }

    #[test]
    fn truncated_json_envelope_includes_handle() {
        let dir = tempfile::TempDir::new().unwrap();
        let long = format!(
            "{{\"items\":[{}]}}",
            (0..3_000)
                .map(|i| format!("{{\"id\":{i},\"name\":\"item-{i}\"}}"))
                .collect::<Vec<_>>()
                .join(",")
        );

        let result = truncated_json_envelope_with_handle(Some(dir.path()), &long);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();

        assert_eq!(parsed["truncated"], true);
        assert_eq!(parsed["retrieve_tool"], "tracedecay_retrieve");
        assert!(parsed.get("retrieve_handle").is_none());
        let handle = parsed["handle"].as_str().unwrap();
        assert!(handle.starts_with("rh_"));

        let prepared = prepare_truncated_response_handle(Some(dir.path()), &long);
        let record = prepared.record.as_ref().unwrap();
        assert_eq!(record.handle, handle);
        let stored = retrieve_response_handle_from_root(
            &record.response_handle_root,
            handle,
            current_timestamp(),
        )
        .unwrap();
        match stored {
            ResponseHandleLookup::Found(record) => assert_eq!(record.content, long),
            other => panic!("stored response should be retrievable, got {other:?}"),
        }
    }

    #[test]
    fn truncated_markdown_includes_readable_handle_guidance() {
        let dir = tempfile::TempDir::new().unwrap();
        let long = format!("# Scan\n\n{}", "- repeated finding\n".repeat(3_000));

        let result = truncated_markdown_with_handle(Some(dir.path()), &long);

        assert!(result.starts_with("# Truncated Response"));
        assert!(result.contains("## Preview"));
        assert!(result.contains("Full response stored locally"));
        assert!(result.contains("tracedecay_retrieve"));
        assert!(
            serde_json::from_str::<serde_json::Value>(&result).is_err(),
            "markdown truncation should not render as a JSON envelope"
        );
        let Some(handle) = result
            .split("handle `")
            .nth(1)
            .and_then(|tail| tail.split('`').next())
        else {
            panic!("markdown guidance should include handle");
        };
        assert!(handle.starts_with("rh_"));

        let prepared = prepare_truncated_response_handle(Some(dir.path()), &long);
        let record = prepared.record.as_ref().unwrap();
        assert_eq!(record.handle, handle);
        let stored = retrieve_response_handle_from_root(
            &record.response_handle_root,
            handle,
            current_timestamp(),
        )
        .unwrap();
        match stored {
            ResponseHandleLookup::Found(record) => assert_eq!(record.content, long),
            other => panic!("stored markdown response should be retrievable, got {other:?}"),
        }
    }

    #[test]
    fn truncated_json_envelope_reports_store_failure() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".tracedecay")).unwrap();
        std::fs::write(
            dir.path().join(".tracedecay/enrollment.json"),
            r#"{"project_id":"../invalid","storage_mode":"profile_sharded"}"#,
        )
        .unwrap();
        let long = format!(
            "{{\"items\":[{}]}}",
            (0..3_000)
                .map(|i| format!("{{\"id\":{i},\"name\":\"item-{i}\"}}"))
                .collect::<Vec<_>>()
                .join(",")
        );

        let result = truncated_json_envelope_with_handle(Some(dir.path()), &long);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();

        assert_eq!(parsed["truncated"], true);
        assert_eq!(parsed["handle_available"], false);
        assert!(parsed.get("handle").is_none());
        assert_eq!(
            parsed["handle_status"]["reason_code"],
            "handle_store_failed"
        );
        assert!(parsed["handle_status"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("could not be cached locally"));
    }

    #[test]
    fn generic_md_renders_array_of_objects_as_table() {
        let v = json!([
            {"id": "function:abc", "name": "foo", "line": 10},
            {"id": "function:def", "name": "bar", "line": 20}
        ]);
        let out = generic_md(&v);
        assert!(out.contains("| id | line | name |"));
        assert!(out.contains("`function:abc`"), "id should be backticked");
        assert!(out.contains("foo"));
    }

    #[test]
    fn generic_md_renders_object_fields_and_sections() {
        let v = json!({
            "total": 3,
            "name": "demo",
            "items": [{"file": "a.rs", "count": 1}]
        });
        let out = generic_md(&v);
        assert!(out.contains("**total:** 3"));
        assert!(out.contains("**name:** demo"));
        assert!(out.contains("## items"));
        assert!(out.contains("| count | file |"));
    }

    #[test]
    fn generic_md_empty_is_noted() {
        assert!(generic_md(&json!([])).contains("None."));
        assert!(generic_md(&json!({})).contains("No results."));
    }

    #[test]
    fn generic_md_compacts_scalar_path_arrays() {
        let out = generic_md(&json!({
            "changed_files": [
                "tests/gateway/test_gateway_shutdown.py",
                "tests/gateway/test_goal_verdict_send.py",
                "tests/gateway/test_homeassistant.py"
            ]
        }));

        assert!(out.contains("## changed_files"));
        assert!(out.contains("tests/gateway/"));
        assert!(out.contains("  test_gateway_shutdown.py"));
        assert!(!out.contains("- tests/gateway/test_gateway_shutdown.py"));
    }

    #[test]
    fn generic_md_keeps_non_path_scalar_arrays_as_bullets() {
        let out = generic_md(&json!({
            "warnings": ["first warning", "second warning"]
        }));

        assert!(out.contains("- first warning"));
        assert!(out.contains("- second warning"));
    }

    #[test]
    fn table_escapes_pipes() {
        let mut md = Md::new();
        md.table(
            &["name", "sig"],
            &[vec!["foo".to_string(), "fn foo(a|b)".to_string()]],
        );
        let out = md.render();
        assert!(out.contains("fn foo(a\\|b)"));
        assert!(out.contains("| name | sig |"));
    }
}
