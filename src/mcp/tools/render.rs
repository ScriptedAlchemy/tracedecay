//! Output-format rendering for MCP tool responses.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

use super::handlers::{truncate_response, truncated_json_envelope_with_handle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Markdown,
    Json,
}

pub fn parse_format(args: &Value) -> OutputFormat {
    match args.get("format").and_then(Value::as_str) {
        Some(v) if v.eq_ignore_ascii_case("json") => OutputFormat::Json,
        _ => OutputFormat::Markdown,
    }
}

pub fn finalize<F>(project_root: Option<&Path>, args: &Value, value: &Value, md: F) -> String
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
            truncate_response(&text)
        }
    }
}

pub fn esc_cell(s: &str) -> String {
    s.replace('|', "\\|").replace(['\n', '\r'], " ")
}

pub fn field_str<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

pub fn field_i64(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

#[derive(Default)]
pub struct Md {
    buf: String,
}

impl Md {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn heading(&mut self, level: u8, text: &str) -> &mut Self {
        let hashes = "#".repeat(level.clamp(1, 6) as usize);
        let _ = writeln!(self.buf, "{hashes} {text}");
        self
    }

    pub fn field(&mut self, key: &str, value: &str) -> &mut Self {
        let _ = writeln!(self.buf, "**{key}:** {value}");
        self
    }

    pub fn line(&mut self, text: &str) -> &mut Self {
        let _ = writeln!(self.buf, "{text}");
        self
    }

    pub fn bullet(&mut self, text: &str) -> &mut Self {
        let _ = writeln!(self.buf, "- {text}");
        self
    }

    pub fn empty_note(&mut self, text: &str) -> &mut Self {
        let _ = writeln!(self.buf, "_{text}_");
        self
    }

    pub fn blank(&mut self) -> &mut Self {
        self.buf.push('\n');
        self
    }

    pub fn table(&mut self, headers: &[&str], rows: &[Vec<String>]) -> &mut Self {
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

    pub fn code(&mut self, lang: &str, body: &str) -> &mut Self {
        let _ = writeln!(self.buf, "```{lang}");
        self.buf.push_str(body);
        if !body.ends_with('\n') {
            self.buf.push('\n');
        }
        self.buf.push_str("```\n");
        self
    }

    pub fn render(self) -> String {
        self.buf
    }
}

const GENERIC_MAX_DEPTH: u8 = 4;

pub fn generic_md(value: &Value) -> String {
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
