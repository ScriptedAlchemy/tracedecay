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
//! - `--project <path>` — project root to open. Defaults to the nearest
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

#[cfg(unix)]
use tracedecay::daemon::call_default_tool;
use tracedecay::daemon::DaemonHandshake;
use tracedecay::errors::{Result, TraceDecayError};
use tracedecay::mcp::tools::{
    get_tool_definitions, handle_profile_scoped_lcm_tool_call, render_tool_cli_help,
    short_tool_name, ToolDefinition, RESERVED_FLAGS_FOOTER,
};

mod args;
use args::{canonical_tool_name, nearest_tool_name, parse_invocation, ParsedInvocation};
#[cfg(test)]
use args::{edit_distance, finalize_arrays, parse_invocation_with_stdin};
#[cfg(test)]
use serde_json::Map;

const PROFILE_SCOPED_LCM_TOOLS: &[&str] = &[
    "tracedecay_lcm_status",
    "tracedecay_lcm_doctor",
    "tracedecay_lcm_load_session",
    "tracedecay_lcm_grep",
    "tracedecay_lcm_describe",
    "tracedecay_lcm_expand",
    "tracedecay_lcm_expand_query",
    "tracedecay_lcm_preflight",
    "tracedecay_lcm_compress",
    "tracedecay_lcm_session_boundary",
];
// Maintenance note: this CLI allowlist must match the MCP registry's
// profile-scoped LCM schemas (tools with `storage_scope` including
// `hermes_profile`) and the daemon's projectless dispatch path; update it
// alongside the handler lockstep tests so profile-scoped calls do not silently
// route through project initialization.
/// Profile-store tools the generated Hermes plugin anchors at the Hermes
/// home (`--project <hermes_home>`). The store is created on first touch —
/// a fresh profile has no `.tracedecay` until the first fact lands — instead
/// of demanding a manual `tracedecay init` of the profile directory. Gated on
/// an explicit `--project` so a bare invocation from an uninitialised cwd
/// still gets the "run tracedecay init" guidance rather than a silent store.
const FIRST_TOUCH_STORE_TOOLS: &[&str] = &[
    "tracedecay_fact_store",
    "tracedecay_fact_feedback",
    "tracedecay_memory_status",
    "tracedecay_message_search",
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

    if is_profile_scoped_lcm_dispatch(&def.name, &tool_args) {
        return dispatch_daemon_tool(
            DaemonToolDispatch::profile_scoped(),
            &def.name,
            tool_args,
            raw_json,
        )
        .await;
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

fn is_profile_scoped_lcm_dispatch(tool_name: &str, tool_args: &Value) -> bool {
    PROFILE_SCOPED_LCM_TOOLS.contains(&tool_name)
        && tool_args
            .get("storage_scope")
            .and_then(Value::as_str)
            .is_some_and(|scope| scope == "hermes_profile")
}

struct DaemonToolDispatch {
    project_path: Option<PathBuf>,
    allow_init: bool,
    allow_profile_scoped_fallback: bool,
}

impl DaemonToolDispatch {
    fn profile_scoped() -> Self {
        Self {
            project_path: None,
            allow_init: false,
            allow_profile_scoped_fallback: true,
        }
    }

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
            allow_profile_scoped_fallback: false,
        }
    }

    fn handshake(&self) -> Result<DaemonHandshake> {
        DaemonHandshake::for_current_client(self.project_path.clone(), None, false, self.allow_init)
    }

    async fn call(&self, tool_name: &str, tool_args: Value) -> Result<Value> {
        let handshake = self.handshake()?;
        #[cfg(unix)]
        {
            call_default_tool(&handshake, tool_name, tool_args).await
        }
        #[cfg(not(unix))]
        {
            call_in_process_tool(&handshake, tool_name, tool_args).await
        }
    }

    async fn fallback(&self, tool_name: &str, tool_args: Value) -> Result<Option<Value>> {
        if !self.allow_profile_scoped_fallback {
            let handshake = self.handshake()?;
            if handshake.project_path.is_none() {
                return Ok(None);
            }
            return Ok(Some(
                call_in_process_tool(&handshake, tool_name, tool_args).await?,
            ));
        }
        let result = handle_profile_scoped_lcm_tool_call(tool_name, tool_args).await?;
        Ok(Some(result.value))
    }
}

async fn call_in_process_tool(
    handshake: &DaemonHandshake,
    tool_name: &str,
    tool_args: Value,
) -> Result<Value> {
    let project_path = handshake
        .project_path
        .as_ref()
        .ok_or_else(|| TraceDecayError::Config {
            message: "profile-scoped daemon tool dispatch requires daemon socket support"
                .to_string(),
        })?;
    let open_options = tracedecay::tracedecay::TraceDecayOpenOptions {
        profile_root: Some(handshake.client_identity.profile_root.clone()),
        global_db_path: Some(handshake.client_identity.global_db_path.clone()),
    };
    let cg = if handshake.allow_init
        && !tracedecay::tracedecay::TraceDecay::has_initialized_store_with_options(
            project_path,
            &open_options,
        )
        .await
    {
        tracedecay::tracedecay::TraceDecay::init_with_options(project_path, open_options).await?
    } else {
        tracedecay::tracedecay::TraceDecay::open_with_options(project_path, open_options).await?
    };
    let global_db =
        tracedecay::global_db::GlobalDb::open_at(&handshake.client_identity.global_db_path).await;
    let result = tracedecay::mcp::tools::handle_tool_call_with_registry(
        &cg,
        tool_name,
        tool_args,
        None,
        handshake.scope_prefix.as_deref(),
        global_db.as_ref(),
        false,
    )
    .await?;
    Ok(result.value)
}

async fn dispatch_daemon_tool(
    dispatch: DaemonToolDispatch,
    tool_name: &str,
    tool_args: Value,
    raw_json: bool,
) -> Result<()> {
    let result_value = match dispatch.call(tool_name, tool_args.clone()).await {
        Ok(value) => value,
        Err(error) if is_daemon_unavailable(&error) => {
            match dispatch.fallback(tool_name, tool_args).await? {
                Some(value) => value,
                None => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    print_tool_output(&result_value, raw_json);
    Ok(())
}

fn is_daemon_unavailable(error: &TraceDecayError) -> bool {
    matches!(
        error,
        TraceDecayError::Config { message }
            if message.contains("TraceDecay daemon socket")
                && message.contains("is not available")
    )
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

    println!("Available tools — run `tracedecay tool <name> --help` for parameters, then");
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
