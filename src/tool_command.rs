//! `tracedecay tool <name> [args...]` — invoke any MCP tool from the CLI.
//!
//! The CLI surface is **dynamic**: tool names and parameters come from the MCP
//! tool definitions in [`crate::mcp::tools`]. Each MCP tool's JSON Schema is
//! walked once to convert CLI `--key value` pairs into a `serde_json::Value`,
//! which is then handed to the same dispatch function the MCP server uses.
//!
//! Reserved flags (handled by this module, never forwarded to the tool):
//!
//! - `-h` / `--help` — print the tool's parameters and exit.
//! - `--json` — print the raw JSON-RPC `result.value`; default is the
//!   human-readable text inside `content[0].text`.
//! - `--dry-run` — parse and validate the arguments, print the resolved
//!   arguments object as pretty JSON, and exit without dispatching the tool.
//! - `--project <path>` — project root to target. Defaults to the nearest
//!   initialised project walking up from cwd (falling back to cwd). We use
//!   `--project` (not `-p`) because several MCP tools have a `path` argument
//!   that filters files within the project.
//! - `--args <json|file|->` — escape hatch. Treats the value as the entire
//!   argument object; mutually exclusive with `--key value` flags. Use for
//!   complex shapes like `tracedecay_multi_str_replace`'s array-of-pairs.
//!   As a whole-payload argument it follows the same convention as
//!   `memory curate --llm-ops`: inline JSON, `-` for stdin, or a file path
//!   (`--args payload.json`; a leading `@` also works for symmetry with
//!   per-key values). Reading from a file or stdin sidesteps the kernel's
//!   128 KiB per-argv-string cap for large payloads.
//!
//! For per-`--key` values, a leading `@` opts into file/stdin reading
//! (`--key @path`, `--key @-`) — the sigil is required there because a bare
//! value is a literal. This makes multi-line strings (replacements, ast-grep
//! patterns, decision text) ergonomic. stdin is read once and memoized, so it
//! can be referenced by more than one field in a single invocation.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

use tracedecay::daemon::{DaemonHandshake, call_default_tool};
use tracedecay::errors::{Result, TraceDecayError};
use tracedecay::mcp::tools::{
    RESERVED_FLAGS_FOOTER, ToolDefinition, get_tool_definitions, render_tool_cli_help,
    short_tool_name,
};

mod args;
use args::{ParsedInvocation, canonical_tool_name, nearest_tool_name, parse_invocation};
#[cfg(test)]
use args::{edit_distance, finalize_arrays, parse_invocation_with_stdin};
#[cfg(test)]
use serde_json::Map;

/// Tools allowed to initialize an explicitly targeted project on first touch.
/// Bare invocations from an uninitialized cwd still get the
/// "run tracedecay init" guidance rather than a silent store.
const FIRST_TOUCH_STORE_TOOLS: &[&str] = &[
    "tracedecay_fact_store",
    "tracedecay_fact_feedback",
    "tracedecay_memory_status",
    "tracedecay_message_search",
    "tracedecay_lcm_status",
    "tracedecay_lcm_grep",
    "tracedecay_lcm_load_session",
    "tracedecay_lcm_doctor",
    "tracedecay_lcm_describe",
    "tracedecay_lcm_expand",
    "tracedecay_lcm_expand_query",
    "tracedecay_lcm_preflight",
    "tracedecay_lcm_compress",
    "tracedecay_lcm_session_boundary",
];

/// Entry point for `tracedecay tool ...`.
pub(crate) async fn run(
    project: Option<String>,
    name: Option<String>,
    args: Vec<String>,
) -> Result<()> {
    let defs = get_tool_definitions();

    let Some(raw_name) = name else {
        print_tool_list(&defs);
        return Ok(());
    };

    let canonical = canonical_tool_name(&raw_name);
    let Some(def) = defs.iter().find(|d| d.name == canonical) else {
        let suggestion = nearest_tool_name(&canonical, &defs)
            .map(|name| format!(" Did you mean '{name}'?"))
            .unwrap_or_default();
        return Err(TraceDecayError::Config {
            message: format!(
                "unknown tool: '{raw_name}'.{suggestion} Run `tracedecay tool` to list available tools."
            ),
        });
    };

    let parsed = parse_invocation(def, &args)?;
    if parsed.show_help {
        print_tool_help(def);
        return Ok(());
    }
    let ParsedInvocation {
        tool_args,
        project: parsed_project,
        raw_json,
        dry_run,
        show_help: _,
    } = parsed;

    if dry_run {
        println!(
            "{}",
            serde_json::to_string_pretty(&tool_args).unwrap_or_default()
        );
        return Ok(());
    }

    let explicit_project = project.or(parsed_project);
    dispatch_daemon_tool(
        DaemonToolDispatch::project_scoped(explicit_project, &def.name),
        &def.name,
        tool_args,
        raw_json,
    )
    .await
}

struct DaemonToolDispatch {
    project_path: Option<PathBuf>,
    allow_init: bool,
}

impl DaemonToolDispatch {
    fn project_scoped(explicit_project: Option<String>, tool_name: &str) -> Self {
        // Same resolution as `tracedecay sync`/`status`/`serve`: an explicit
        // --project wins; otherwise walk up from cwd to the nearest initialised
        // project so the command works from subdirectories.
        let explicitly_targeted = explicit_project.is_some();
        let project_path = tracedecay::config::resolve_path_with_discovery(explicit_project);
        let allow_init = explicitly_targeted && FIRST_TOUCH_STORE_TOOLS.contains(&tool_name);

        Self {
            project_path: Some(project_path),
            allow_init,
        }
    }

    fn handshake(&self) -> Result<DaemonHandshake> {
        DaemonHandshake::for_current_client(self.project_path.clone(), None, false, self.allow_init)
    }

    async fn call(&self, tool_name: &str, tool_args: Value) -> Result<Value> {
        let handshake = self.handshake()?;
        call_default_tool(&handshake, tool_name, tool_args).await
    }
}

async fn dispatch_daemon_tool(
    dispatch: DaemonToolDispatch,
    tool_name: &str,
    tool_args: Value,
    raw_json: bool,
) -> Result<()> {
    let result_value = dispatch.call(tool_name, tool_args).await?;
    print_tool_output(&result_value, raw_json);
    Ok(())
}

fn print_tool_output(result_value: &Value, raw_json: bool) {
    if raw_json {
        println!(
            "{}",
            serde_json::to_string_pretty(result_value).unwrap_or_default()
        );
    } else {
        println!("{}", join_content_text(result_value));
    }
}

/// Joins every `content[*].text` block in an MCP tool result, separated by a
/// blank line. Handlers sometimes prepend a warning/notice block ahead of the
/// real payload+metrics block; printing only `content[0].text` would silently
/// drop the payload. Falls back to the empty string when no text blocks exist.
fn join_content_text(result_value: &Value) -> String {
    result_value
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}

/// Print a grouped list of every available tool. Tools annotated as
/// `alwaysLoad` come first since they're the most commonly used; everything
/// else is alphabetized.
fn print_tool_list(defs: &[ToolDefinition]) {
    let mut groups: BTreeMap<&str, Vec<&ToolDefinition>> = BTreeMap::new();
    let mut always = Vec::new();
    for def in defs {
        let is_always = def
            .meta
            .as_ref()
            .and_then(|m| m.get("anthropic/alwaysLoad"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if is_always {
            always.push(def);
            continue;
        }
        let group = group_for(def);
        groups.entry(group).or_default().push(def);
    }

    println!(
        "Available tools ({}; TraceDecay {}) — run `tracedecay tool <name> --help` for parameters, then",
        defs.len(),
        env!("CARGO_PKG_VERSION")
    );
    println!("invoke with `tracedecay tool <name> --args '<json>'` (the same JSON arguments");
    println!("object as the MCP tool; `--args -` reads a heredoc from stdin) or, for quick");
    println!("scalar calls, `--key value` flags.\n");

    if !always.is_empty() {
        println!("[always-loaded]");
        for def in &always {
            println!(
                "  {:<32}  {}",
                short_tool_name(&def.name),
                first_line(&def.description)
            );
        }
        println!();
    }

    for (group, mut list) in groups {
        list.sort_by_key(|d| d.name.clone());
        println!("[{group}]");
        for def in list {
            println!(
                "  {:<32}  {}",
                short_tool_name(&def.name),
                first_line(&def.description)
            );
        }
        println!();
    }

    println!("{RESERVED_FLAGS_FOOTER}");
}

/// First line of a (possibly multi-line) description, truncated for layout.
fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("");
    if line.len() > 90 {
        format!("{}…", &line[..89])
    } else {
        line.to_string()
    }
}

/// Best-effort categorisation by tool-name prefix. Matches how the codebase
/// already groups handlers (`graph`, `info`, `git`, `analysis`, `health`,
/// `edit`, `memory`). Tools that don't match any prefix fall under `other`.
fn group_for(def: &ToolDefinition) -> &'static str {
    let n = def.name.as_str();
    if n.starts_with("tracedecay_branch_")
        || n == "tracedecay_commit_context"
        || n == "tracedecay_pr_context"
        || n == "tracedecay_changelog"
        || n == "tracedecay_diff_context"
        || n == "tracedecay_affected"
    {
        "git & history"
    } else if n == "tracedecay_str_replace"
        || n == "tracedecay_multi_str_replace"
        || n == "tracedecay_insert_at"
        || n == "tracedecay_ast_grep_rewrite"
        || n == "tracedecay_replace_symbol"
        || n == "tracedecay_insert_at_symbol"
        || n == "tracedecay_move_symbol"
    {
        "edit"
    } else if n == "tracedecay_fact_store"
        || n == "tracedecay_fact_feedback"
        || n == "tracedecay_memory_status"
        || n == "tracedecay_session_start"
        || n == "tracedecay_session_end"
    {
        "memory & session"
    } else if n == "tracedecay_health"
        || n == "tracedecay_runtime"
        || n == "tracedecay_dsm"
        || n == "tracedecay_test_risk"
        || n == "tracedecay_test_map"
        || n == "tracedecay_gini"
        || n == "tracedecay_dependency_depth"
        || n == "tracedecay_redundancy"
    {
        "health"
    } else if n == "tracedecay_callers"
        || n == "tracedecay_callees"
        || n == "tracedecay_callers_for"
        || n == "tracedecay_call_chain"
        || n == "tracedecay_impact"
        || n == "tracedecay_file_dependents"
        || n == "tracedecay_by_qualified_name"
        || n == "tracedecay_signature"
        || n == "tracedecay_impls"
        || n == "tracedecay_implementations"
        || n == "tracedecay_derives"
        || n == "tracedecay_similar"
        || n == "tracedecay_rename_preview"
        || n == "tracedecay_find_exact_symbol"
        || n == "tracedecay_type_hierarchy"
    {
        "graph"
    } else if n == "tracedecay_diagnose"
        || n == "tracedecay_diagnostics"
        || n == "tracedecay_run_affected_tests"
    {
        "workflow"
    } else if n == "tracedecay_dead_code"
        || n == "tracedecay_unused_imports"
        || n == "tracedecay_module_api"
        || n == "tracedecay_circular"
        || n == "tracedecay_hotspots"
        || n == "tracedecay_rank"
        || n == "tracedecay_largest"
        || n == "tracedecay_coupling"
        || n == "tracedecay_inheritance_depth"
        || n == "tracedecay_distribution"
        || n == "tracedecay_recursion"
        || n == "tracedecay_complexity"
        || n == "tracedecay_doc_coverage"
        || n == "tracedecay_god_class"
        || n == "tracedecay_unsafe_patterns"
        || n == "tracedecay_constructors"
        || n == "tracedecay_field_sites"
    {
        "analysis"
    } else {
        "info"
    }
}

/// Print one tool's description, usage line, and parameter table.
fn print_tool_help(def: &ToolDefinition) {
    print!("{}", render_tool_cli_help(def));
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
