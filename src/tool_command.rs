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
//! - `--dry-run` — for tools without their own `dry_run` property, parse and
//!   validate the arguments, print the resolved arguments object as pretty
//!   JSON, and exit without dispatching the tool. Otherwise it is forwarded as
//!   the tool's boolean argument.
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
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::time::{Instant, timeout_at};

use tracedecay::application_surface::{
    ApplicationSurfaceAdapterError, ApplicationSurfaceInvocationResult,
    ApplicationSurfaceOperation, normalize_application_tool_args,
    observe_surface_argument_rejection, parse_application_surface_request,
    resolve_catalog_tool_binding,
};
use tracedecay::daemon::{DaemonHandshake, call_default_tool_within};
use tracedecay::daemon_client::{DaemonInvocationClient, RequestedOutputFormat};
use tracedecay::errors::{Result, TraceDecayError};
use tracedecay::mcp::tools::internal_daemon_tool_definition;
use tracedecay::mcp::tools::{
    LegacyToolCompatibilityOwner, RESERVED_FLAGS_FOOTER, ToolDefinition, get_tool_definitions,
    render_tool_cli_help, short_tool_name,
};
use tracedecay::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_application::{CancellationSignal, Deadline};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::BindingSurface;

use crate::cli::dispatch::resolve_cli_application_surface;

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

const DEFAULT_TOOL_DEADLINE: Duration = Duration::from_secs(120);
const MAX_TOOL_DEADLINE: Duration = Duration::from_secs(24 * 60 * 60);
const TOOL_DEADLINE_ENV: &str = "TRACEDECAY_TOOL_DEADLINE_MS";

fn tool_deadline_range_error() -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("{TOOL_DEADLINE_ENV} exceeds the supported monotonic deadline range"),
    }
}

fn tool_command_deadline() -> Result<Duration> {
    let deadline = std::env::var(TOOL_DEADLINE_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TOOL_DEADLINE);
    if deadline > MAX_TOOL_DEADLINE {
        return Err(tool_deadline_range_error());
    }
    Ok(deadline)
}

fn tool_timeout_error(tool_name: &str) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "tool request timed out before deadline: {tool_name}; request outcome may be unknown"
        ),
    }
}

fn is_truncation_envelope(value: &Value) -> bool {
    value.get("truncated").and_then(Value::as_bool) == Some(true)
        && value
            .get("original_chars")
            .and_then(Value::as_u64)
            .is_some()
        && value.get("preview").and_then(Value::as_str).is_some()
}

fn reject_truncation_envelope(value: &Value, tool_name: &str) -> Result<()> {
    if !is_truncation_envelope(value) {
        return Ok(());
    }
    let original_chars = value.get("original_chars").and_then(Value::as_u64);
    let handle = value.get("handle").and_then(Value::as_str);
    let message = match (original_chars, handle) {
        (Some(chars), Some(handle)) => format!(
            "daemon tool {tool_name} returned truncated JSON ({chars} chars); \
             recover with tracedecay_retrieve handle={handle}"
        ),
        (Some(chars), None) => format!(
            "daemon tool {tool_name} returned truncated JSON ({chars} chars) \
             without a retrieval handle"
        ),
        _ => format!("daemon tool {tool_name} returned truncated JSON"),
    };
    Err(TraceDecayError::Config { message })
}

fn reject_tool_result_truncation(result_value: &Value, tool_name: &str) -> Result<()> {
    reject_truncation_envelope(result_value, tool_name)?;
    let Some(blocks) = result_value.get("content").and_then(Value::as_array) else {
        return Ok(());
    };
    for block in blocks {
        let Some(text) = block.get("text").and_then(Value::as_str) else {
            continue;
        };
        if let Ok(payload) = serde_json::from_str::<Value>(text) {
            reject_truncation_envelope(&payload, tool_name)?;
        }
    }
    Ok(())
}

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
    let internal_def = internal_daemon_tool_definition(&canonical);
    let Some(def) = defs
        .iter()
        .find(|definition| definition.name == canonical)
        .or(internal_def.as_ref())
    else {
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
    let deadline = Instant::now()
        .checked_add(tool_command_deadline()?)
        .ok_or_else(tool_deadline_range_error)?;
    if let Some(operation) = ApplicationSurfaceOperation::from_tool_name(&def.name) {
        let (request, requested_format) = cli_surface_invocation(&def.name, tool_args, raw_json)
            .map_err(|error| TraceDecayError::Config {
                message: error.to_string(),
            })?;
        return dispatch_cli_application_surface(
            operation,
            request,
            DaemonToolDispatch::project_scoped(explicit_project, &def.name).project_path,
            requested_format,
            deadline,
        )
        .await;
    }
    // Catalog-declared operations must pass the same binding resolver as the
    // typed application surfaces before entering the retained compatibility
    // owner. Operations with no catalog contract remain explicitly owned by
    // the root MCP handler migration, rather than an unclassified fallback.
    let _catalog_binding =
        resolve_catalog_tool_binding(BindingSurface::Cli, &def.name).map_err(|error| {
            TraceDecayError::Config {
                message: error.to_string(),
            }
        })?;
    if internal_def.is_none() && !LegacyToolCompatibilityOwner::admits(&def.name) {
        return Err(TraceDecayError::Config {
            message: format!(
                "{} does not own {}: {}",
                LegacyToolCompatibilityOwner::OWNER,
                def.name,
                LegacyToolCompatibilityOwner::REASON
            ),
        });
    }
    dispatch_compatibility_tool(
        DaemonToolDispatch::for_tool(explicit_project, &def.name, &tool_args),
        &def.name,
        tool_args,
        raw_json,
        deadline,
    )
    .await
}

/// Splits a CLI `--args` object into the reviewed application request body and
/// the requested output format, through the same adapter every other transport
/// uses. `--json` and `format: "json"` are the same request for JSON output.
fn cli_surface_invocation(
    tool_name: &str,
    tool_args: Value,
    raw_json: bool,
) -> std::result::Result<(Value, RequestedOutputFormat), ApplicationSurfaceAdapterError> {
    let normalized = normalize_application_tool_args(tool_name, tool_args)?;
    let requested_format = if raw_json {
        RequestedOutputFormat::Json
    } else {
        normalized.requested_format
    };
    Ok((normalized.request, requested_format))
}

/// Every application-surface operation is project-scoped on the daemon side
/// (`DaemonInvocationRequest::requires_project`), so `project` must already be
/// the resolved project route — not just an explicit `--project`. A handshake
/// without a project reaches the profile-scoped projectless route, where those
/// operations can only answer `application.surface.unavailable` /
/// `not_found_or_not_authorized`.
async fn dispatch_cli_application_surface(
    operation: ApplicationSurfaceOperation,
    tool_args: Value,
    project: Option<PathBuf>,
    requested_format: RequestedOutputFormat,
    deadline: Instant,
) -> Result<()> {
    let request_id =
        mint_global_request_id(GlobalRequestSurface::Cli).map_err(|_| TraceDecayError::Config {
            message: "could not allocate an application surface request id".to_owned(),
        })?;
    let request = match parse_application_surface_request(operation, tool_args) {
        Ok(request) => request,
        Err(error) => {
            if let Ok(handshake) =
                DaemonHandshake::for_current_client(project.clone(), None, false, false)
                && let Ok(client) = DaemonInvocationClient::for_current(handshake)
            {
                observe_surface_argument_rejection(
                    Some(&client),
                    tracedecay_tool_catalog::BindingSurface::Cli,
                    operation,
                    &request_id,
                    &error,
                )
                .await;
            }
            return Err(TraceDecayError::Config {
                message: error.to_string(),
            });
        }
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_micros()).unwrap_or(i64::MAX))
        .unwrap_or(i64::MAX);
    let remaining = deadline.saturating_duration_since(Instant::now());
    let deadline = Deadline::new(UtcMicros(
        now.saturating_add(i64::try_from(remaining.as_micros()).unwrap_or(i64::MAX)),
    ))
    .map_err(|error| TraceDecayError::Config {
        message: error.to_string(),
    })?;
    let cancellation =
        CancellationSignal::active(format!("cancellation.cli.{}", request_id.as_str())).map_err(
            |error| TraceDecayError::Config {
                message: error.to_string(),
            },
        )?;
    let handshake = DaemonHandshake::for_current_client(project, None, false, false)?;
    let client = DaemonInvocationClient::for_current(handshake)?;
    let result = resolve_cli_application_surface(
        operation,
        request_id,
        request,
        requested_format,
        deadline,
        cancellation,
        Some(&client),
    )
    .await
    .map_err(|error| TraceDecayError::Config {
        message: error.to_string(),
    })?;
    print_cli_application_surface(result, requested_format == RequestedOutputFormat::Json)
}

fn print_cli_application_surface(
    result: ApplicationSurfaceInvocationResult,
    raw_json: bool,
) -> Result<()> {
    let application_problem = result
        .result
        .as_ref()
        .err()
        .map(|problem| format!("{}: {}", problem.problem.code, problem.problem.message));
    if raw_json {
        print!("{}", crate::cli::output::json::json_line(&result.result)?);
    } else {
        let view = crate::cli::output::view::CanonicalHumanView::from_application_result(
            result.operation.as_str(),
            &result.binding_id,
            &result.result,
        )?;
        let rendered = crate::cli::output::markdown::render(view);
        println!("{}", rendered.as_str());
    }
    if application_problem.is_some() {
        std::io::stdout().flush()?;
    }
    match application_problem {
        Some(message) => Err(TraceDecayError::Config { message }),
        None => Ok(()),
    }
}

struct DaemonToolDispatch {
    project_path: Option<PathBuf>,
    allow_init: bool,
}

impl DaemonToolDispatch {
    fn for_tool(explicit_project: Option<String>, tool_name: &str, tool_args: &Value) -> Self {
        // Profile-authority tools (Hermes user LCM/memory) must never invent a
        // project from cwd. Hermes intentionally runs those calls with cwd=/ so
        // Hermes home is never mistaken for a TraceDecay project.
        if requests_profile_authority(tool_args) {
            return Self {
                project_path: None,
                allow_init: false,
            };
        }
        Self::project_scoped(explicit_project, tool_name)
    }

    fn project_scoped(explicit_project: Option<String>, tool_name: &str) -> Self {
        // Same resolution as `tracedecay sync`/`status`/`serve`: an explicit
        // --project wins; otherwise walk up from cwd to the nearest initialised
        // project so the command works from subdirectories.
        let explicitly_targeted = explicit_project.is_some();
        let project_path = tracedecay::config::resolve_path_with_discovery(explicit_project);
        // Never treat the filesystem root as a discovered project fallback.
        // Callers that need a project must pass --project; otherwise the daemon
        // serves the profile-scoped projectless route.
        if !explicitly_targeted && is_filesystem_root(&project_path) {
            return Self {
                project_path: None,
                allow_init: false,
            };
        }
        let allow_init = explicitly_targeted && FIRST_TOUCH_STORE_TOOLS.contains(&tool_name);

        Self {
            project_path: Some(project_path),
            allow_init,
        }
    }

    fn handshake(&self) -> Result<DaemonHandshake> {
        DaemonHandshake::for_current_client(self.project_path.clone(), None, false, self.allow_init)
    }

    async fn call(&self, tool_name: &str, tool_args: Value, deadline: Instant) -> Result<Value> {
        let handshake = self.handshake()?;
        call_default_tool_within(&handshake, tool_name, tool_args, deadline).await
    }
}

fn requests_profile_authority(tool_args: &Value) -> bool {
    matches!(
        tool_args.get("storage_scope").and_then(Value::as_str),
        Some("user")
    ) || matches!(
        tool_args.get("memory_scope").and_then(Value::as_str),
        Some("user")
    )
}

fn is_filesystem_root(path: &Path) -> bool {
    let mut saw_root = false;
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => saw_root = true,
            Component::CurDir => {}
            Component::ParentDir | Component::Normal(_) => return false,
        }
    }
    saw_root
}

fn map_tool_deadline_error(tool_name: &str, error: TraceDecayError) -> TraceDecayError {
    if tracedecay::daemon::error_message_is_read_deadline(&error.to_string()) {
        tool_timeout_error(tool_name)
    } else {
        error
    }
}

/// Compatibility owner for advertised tools that do not yet have a typed
/// `ApplicationSurfaceRequest`.
///
/// Owner: root MCP tool-dispatch migration. The operation has already passed
/// definition admission and, when declared, catalog binding resolution.
async fn dispatch_compatibility_tool(
    dispatch: DaemonToolDispatch,
    tool_name: &str,
    tool_args: Value,
    raw_json: bool,
    deadline: Instant,
) -> Result<()> {
    let result_value =
        match timeout_at(deadline, dispatch.call(tool_name, tool_args, deadline)).await {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => return Err(map_tool_deadline_error(tool_name, error)),
            Err(_) => return Err(tool_timeout_error(tool_name)),
        };
    if Instant::now() >= deadline {
        return Err(tool_timeout_error(tool_name));
    }
    reject_tool_result_truncation(&result_value, tool_name)?;
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
        tracedecay::version::build_version()
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
    if ApplicationSurfaceOperation::from_tool_name(n).is_some() {
        "application"
    } else if n.starts_with("tracedecay_branch_")
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
        || n == "tracedecay_api_migration_plan"
        || n == "tracedecay_api_migration_apply"
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
