//! MCP tool definitions and dispatch for the code graph.
//!
//! Split into two sub-modules:
//! - `definitions`: JSON Schema tool descriptors (`def_*` functions)
//! - `handlers`: tool call implementations (`handle_*` functions)

mod binding;
mod definitions;
pub mod dispatch;
pub(crate) mod handlers;
pub(crate) mod render;
pub(crate) mod renderers;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::OnceLock;

pub(crate) use binding::tool_dispatches_registered_project_reader;
pub use definitions::{
    ALWAYS_REGISTERED_TOOL_COUNT, ToolRegistryMode, ast_grep_available, ast_grep_diagnostics_json,
    ast_grep_outline_available, context_description, default_catalog_discovery_authority,
    explore_call_budget, format_capable_tool_names,
    get_catalog_filtered_tool_definitions_with_budget,
    get_catalog_filtered_tool_definitions_with_warming_budget, get_tool_definitions,
    get_tool_definitions_with_budget, get_tool_definitions_with_warming_budget,
    internal_daemon_tool_definition, project_catalog_discovery_scope, tool_defaults_to_markdown,
};
pub(crate) use handlers::handle_user_lcm_tool_with_retained_authority;
pub(crate) use handlers::hook_runtime::structured_hook_error_data;
pub(crate) use handlers::memory::handle_user_memory_tool;
pub(crate) use handlers::session::message_search::SessionRetrievalOmissionView;
pub(crate) use handlers::{
    LcmDescribeServiceCommand, LcmDescribeServiceFuture, LcmDescribeServiceOutcome,
    LcmExpandServiceCommand, LcmExpandServiceFuture, LcmExpandServiceOutcome,
    ProjectRegistryContextCommand, ProjectRegistryContextFuture, ProjectRegistryContextOutcome,
    ProjectRegistryContextView, ProjectRegistryListingCommand, ProjectRegistryListingFuture,
    ProjectRegistryListingOutcome, ProjectRegistryListingScope, ProjectRegistryListingView,
    ProjectRegistryReadPort, ProjectRegistrySelector, SessionRefreshAction, SessionRefreshCommand,
    SessionRefreshCoverageView, SessionRefreshFrontierView, SessionRefreshProgressView,
    SessionRefreshReceiptView, SessionRefreshServiceOutcome, SessionRefreshServicePort,
    SessionRetrievalCommand, SessionRetrievalExplanationView, SessionRetrievalPageView,
    SessionRetrievalServiceFuture, SessionRetrievalServiceOutcome, SessionRetrievalServicePort,
    SessionRetrievalStoreScope, SessionRetrievalUnavailable, SessionRetrievalUnavailableReason,
    SessionRetrievalWorkerBlocker, SessionRetrievalWorkerRetryClass,
    SessionRetrievalWorkerStatusView, SessionTemporalMetadataView, SessionTemporalWatermarksView,
    handle_projectless_admin_cli, handle_projectless_hook_runtime,
    replay_projectless_hermes_host_admission, utc_micros_value,
};
pub use handlers::{
    SessionAuthorities, ToolCallRegistryOptions, handle_tool_call,
    handle_tool_call_with_registry_and_implicit_project, handle_user_lcm_tool,
};

/// Maximum character length for a tool response before truncation.
const MAX_RESPONSE_CHARS: usize = 15_000;

/// A tool definition exposed by the MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Unique tool name.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema describing the tool's input parameters.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    /// MCP tool annotations (readOnlyHint, title, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
    /// MCP tool metadata (e.g. anthropic/alwaysLoad).
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

/// Explicit owner for advertised tools awaiting typed application contracts.
///
/// These tools retain their existing root handlers, but they are no longer an
/// unclassified dispatch fallback: definition admission is mandatory, and any
/// application-catalog binding is resolved before this owner is entered.
pub struct LegacyToolCompatibilityOwner;

impl LegacyToolCompatibilityOwner {
    pub const OWNER: &'static str = "root MCP tool-dispatch migration";
    pub const REASON: &'static str =
        "typed ApplicationSurfaceRequest contract has not yet landed for this tool family";

    pub fn admits(tool_name: &str) -> bool {
        static ADVERTISED_TOOLS: OnceLock<BTreeSet<String>> = OnceLock::new();
        ADVERTISED_TOOLS
            .get_or_init(|| {
                get_tool_definitions()
                    .into_iter()
                    .map(|definition| definition.name)
                    .collect()
            })
            .contains(tool_name)
    }
}

/// The result of a tool call, including the JSON response and the file
/// paths that were touched (used to track saved tokens).
#[derive(Clone, Debug)]
pub struct ToolResult {
    /// The JSON-RPC result payload.
    pub value: Value,
    /// Unique file paths referenced in the result.
    pub touched_files: Vec<String>,
    /// Internal analytics metadata for the server runtime. This must never be
    /// serialized into the tool response payload.
    internal_analytics: Option<Value>,
    /// Structural signal that the handler itself determined this call failed
    /// semantically (e.g. an edit whose `success` field is `false`), set by
    /// the handler rather than inferred later from rendered response text.
    /// `None` means the handler did not classify outcome structurally, so
    /// callers should fall back to text-based heuristics; `Some(true)`/
    /// `Some(false)` are authoritative and skip those heuristics.
    semantic_error: Option<bool>,
    /// Handler-provided human-readable reason for a structural semantic
    /// failure (e.g. an edit result's `message`, such as "`old_str` not
    /// found"). Only meaningful when `semantic_error == Some(true)`; used to
    /// populate analytics `failure_reason` without re-deriving it from
    /// rendered response text.
    failure_message: Option<String>,
}

impl ToolResult {
    pub fn new(value: Value, touched_files: Vec<String>) -> Self {
        Self {
            value,
            touched_files,
            internal_analytics: None,
            semantic_error: None,
            failure_message: None,
        }
    }

    #[must_use]
    pub fn with_internal_analytics(mut self, internal_analytics: Value) -> Self {
        self.internal_analytics = Some(internal_analytics);
        self
    }

    pub fn internal_analytics(&self) -> Option<&Value> {
        self.internal_analytics.as_ref()
    }

    /// Record a handler-determined semantic outcome for this call. Pass
    /// `true` when the handler knows the operation failed (e.g. an edit's
    /// `success: false`), `false` when the handler knows it succeeded.
    #[must_use]
    pub fn with_semantic_error(mut self, is_error: bool) -> Self {
        self.semantic_error = Some(is_error);
        self
    }

    /// The handler-determined semantic outcome, if the handler set one.
    pub fn semantic_error(&self) -> Option<bool> {
        self.semantic_error
    }

    /// Attach a human-readable reason for a structural semantic failure
    /// (e.g. an edit result's `message`). No-op unless paired with
    /// `with_semantic_error(true)`.
    #[must_use]
    pub fn with_failure_message(mut self, message: impl Into<String>) -> Self {
        self.failure_message = Some(message.into());
        self
    }

    /// The handler-provided failure reason, if one was set.
    pub fn failure_message(&self) -> Option<&str> {
        self.failure_message.as_deref()
    }
}

/// Render the CLI help shown by `tracedecay tool <name> --help`.
///
/// Kept in the library so tests and generated integration surfaces can
/// validate the dynamic tool help without spawning the binary once per tool.
pub fn render_tool_cli_help(def: &ToolDefinition) -> String {
    let short = short_tool_name(&def.name);
    let mut out = String::new();
    let _ = writeln!(out, "tracedecay tool {short}");
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", def.description);
    let _ = writeln!(out);

    let props = def
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
        .filter(|props| !props.is_empty());
    let required: Vec<&str> = def
        .input_schema
        .get("required")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let Some(props) = props else {
        let _ = writeln!(out, "(no parameters)");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Usage: tracedecay tool {short} [--json] [--project <path>]"
        );
        return out;
    };

    let mut usage_params = String::new();
    for req in &required {
        let _ = write!(usage_params, " --{} <value>", req.replace('_', "-"));
    }
    let _ = writeln!(
        out,
        "Usage: tracedecay tool {short}{usage_params} [--key value]... [--json]"
    );
    let _ = writeln!(out);

    let _ = writeln!(out, "Parameters:");
    let mut entries: Vec<(&String, &Value)> = props.iter().collect();
    entries.sort_by_key(|(k, _)| (*k).clone());
    let mut has_non_scalar = false;
    for (key, schema) in &entries {
        let ty = schema
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("string");
        if matches!(ty, "array" | "object") {
            has_non_scalar = true;
        }
        let req = if required.contains(&key.as_str()) {
            "required"
        } else {
            "optional"
        };
        let mut desc = schema
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if let Some(constraint) = param_shape_note(schema, ty) {
            if desc.is_empty() {
                desc = constraint;
            } else {
                let _ = write!(desc, " ({constraint})");
            }
        }
        let _ = writeln!(
            out,
            "  --{:<26} {:<8} {:<8}  {}",
            key.replace('_', "-"),
            ty,
            req,
            desc
        );
    }
    let _ = writeln!(out);

    // Tools with array/object params can't be driven with scalar flags alone;
    // show the ready-to-copy heredoc form built from the schema itself.
    if has_non_scalar {
        let _ = writeln!(out, "Example (whole MCP arguments object via stdin):");
        let _ = writeln!(out, "  tracedecay tool {short} --args - <<'JSON'");
        let _ = writeln!(out, "  {}", example_args_object(props, &required));
        let _ = writeln!(out, "  JSON");
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "{RESERVED_FLAGS_FOOTER}");
    out
}

/// The reserved-flag footnote shared by per-tool help; the tool list and the
/// `tracedecay tool --help` trailer restate the same two facts.
pub const RESERVED_FLAGS_FOOTER: &str = "\
Reserved flags: --args <json|-|@file|file> (whole MCP arguments object; `-` reads stdin),\n\
  --dry-run (validate + print the resolved arguments, don't invoke), --json (raw payload),\n\
  --project <path>, -h/--help.\n\
Per-key values starting with @ are read from that file; @- reads stdin.";

/// Enum values or array item shapes worth stating inline so the help is
/// sufficient for one-shot construction.
fn param_shape_note(schema: &Value, ty: &str) -> Option<String> {
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        let values: Vec<&str> = allowed.iter().filter_map(Value::as_str).collect();
        if !values.is_empty() {
            return Some(format!("one of: {}", values.join(" | ")));
        }
    }
    if ty == "array" {
        let items_type = schema
            .get("items")
            .and_then(|items| items.get("type"))
            .and_then(Value::as_str)?;
        return Some(match items_type {
            "array" => "array of arrays — pass JSON via --args".to_string(),
            "object" => "array of objects — pass JSON via --args".to_string(),
            other => format!("array of {other}s"),
        });
    }
    None
}

/// A mechanical `--args` example object: every required property plus the
/// non-scalar optional ones, with placeholder values derived from the schema.
fn example_args_object(props: &serde_json::Map<String, Value>, required: &[&str]) -> String {
    let mut example = serde_json::Map::new();
    let mut entries: Vec<(&String, &Value)> = props.iter().collect();
    entries.sort_by_key(|(key, _)| (!required.contains(&key.as_str()), (*key).clone()));
    for (key, schema) in entries {
        let ty = schema
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("string");
        if !required.contains(&key.as_str()) && !matches!(ty, "array" | "object") {
            continue;
        }
        example.insert(key.clone(), placeholder_value(key, schema, ty));
    }
    serde_json::to_string(&Value::Object(example)).unwrap_or_else(|_| "{}".to_string())
}

fn placeholder_value(key: &str, schema: &Value, ty: &str) -> Value {
    match ty {
        "boolean" => Value::Bool(true),
        "integer" | "number" => Value::from(10),
        "array" => {
            let items = schema.get("items");
            let items_type = items
                .and_then(|items| items.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("string");
            let element = match items_type {
                "array" => {
                    let inner = items
                        .and_then(|items| items.get("items"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    let inner_type = inner
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("string");
                    Value::Array(vec![
                        placeholder_value(key, &inner, inner_type),
                        placeholder_value(key, &inner, inner_type),
                    ])
                }
                "object" => Value::Object(serde_json::Map::new()),
                other => placeholder_value(key, items.unwrap_or(&Value::Null), other),
            };
            Value::Array(vec![element])
        }
        "object" => Value::Object(serde_json::Map::new()),
        _ => {
            if let Some(first) = schema
                .get("enum")
                .and_then(Value::as_array)
                .and_then(|values| values.first())
            {
                first.clone()
            } else {
                Value::String(format!("<{key}>"))
            }
        }
    }
}

pub fn short_tool_name(full: &str) -> &str {
    full.strip_prefix("tracedecay_").unwrap_or(full)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_result_constructors_keep_internal_analytics_explicit() {
        let result = ToolResult::new(json!({"content": []}), vec!["src/lib.rs".to_string()]);
        assert_eq!(result.value, json!({"content": []}));
        assert_eq!(result.touched_files, vec!["src/lib.rs"]);
        assert!(result.internal_analytics().is_none());

        let result = result.with_internal_analytics(json!({"context_memory": {"match_count": 1}}));
        assert_eq!(
            result.internal_analytics(),
            Some(&json!({"context_memory": {"match_count": 1}}))
        );
    }
}
