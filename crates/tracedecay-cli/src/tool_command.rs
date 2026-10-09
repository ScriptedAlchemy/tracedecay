//! `tracedecay tool <name> [args...]`. Invoke any MCP tool from the CLI.
//!
//! The CLI surface is **dynamic**: tool names and parameters come from the MCP
//! tool definitions in [`crate::mcp::tools`]. Each MCP tool's JSON Schema is
//! walked once to convert CLI `--key value` pairs into a `serde_json::Value`,
//! which is then handed to the same dispatch function the MCP server uses.
//!
//! Reserved flags (handled by this module, never forwarded to the tool):
//!
//! - `-h` / `--help`, print the tool's parameters and exit.
//! - `--json`, print the raw JSON-RPC `result.value`; default is the
//!   human-readable text inside `content[0].text`.
//! - `--dry-run`, for tools without their own `dry_run` property, parse and
//!   validate the arguments, print the resolved arguments object as pretty
//!   JSON, and exit without dispatching the tool. Otherwise it is forwarded as
//!   the tool's boolean argument.
//! - `--project <path>`, project root to target. Defaults to the nearest
//!   initialised project walking up from cwd. We use
//!   `--project` (not `-p`) because several MCP tools have a `path` argument
//!   that filters files within the project.
//! - `--args <json|file|->`, escape hatch. Treats the value as the entire
//!   argument object; mutually exclusive with `--key value` flags. Use for
//!   complex shapes like `tracedecay_multi_str_replace`'s array-of-pairs.
//!   A whole payload accepts inline JSON, `-` for stdin, or a file path
//!   (`--args payload.json`; a leading `@` also works for symmetry with per-key
//!   values). Reading from a file or stdin sidesteps the kernel's 128 KiB
//!   per-argv-string cap for large payloads.
//!
//! For per-`--key` values, a leading `@` opts into file/stdin reading
//! (`--key @path`, `--key @-`), the sigil is required there because a bare
//! value is a literal. This makes multi-line strings (replacements, ast-grep
//! patterns, decision text) ergonomic. stdin is read once and memoized, so it
//! can be referenced by more than one field in a single invocation.
//!
//! Memory curation uses the same public MCP interfaces through this dynamic
//! command: `tracedecay tool fact_store_curate` launches the daemon-owned
//! curator, while `automation_run_list`, `automation_run_view`, and
//! `automation_run_artifact_view` inspect its durable result. The launch tool
//! accepts only review bounds; direct fact add, update, and remove remain
//! separate exact administrative tools.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracedecay_runtime_core::config::ProfileRoot;

use serde_json::Value;
use tokio::time::Instant;

use tracedecay::mcp::tools::{registered_project_not_found, registered_project_selector_id};
use tracedecay_contracts::code_index_freshness::{
    CODE_INDEX_READINESS_WAIT_TIMED_OUT, CODE_INDEX_READINESS_WAIT_UNAVAILABLE,
    CodeIndexReadinessWaitOutcomeV1,
};
use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_contracts::retrieval::{
    AdminCliRegistryContextV1, AdminCliResultV1, AdminCliSurfaceRequestV1,
};
use tracedecay_contracts::{
    CancellationSignal, Deadline, RetainedSurfaceOperation, retained_surface_operation_is_effect,
};
use tracedecay_daemon_protocol::{
    ApplicationSurfaceAdapterError, ApplicationSurfaceInvocationResult,
    adapt_application_tool_request, parse_application_surface_request,
};
use tracedecay_daemon_protocol::{
    DaemonHandshake, RequestedOutputFormat, TOOL_REQUEST_DEADLINE_ENV, requested_output_format,
    tool_request_deadline,
};
use tracedecay_daemon_service::application_surface::observe_surface_argument_rejection;
use tracedecay_domain::UtcMicros;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_mcp::tool_errors::{mark_semantic_tool_error, tool_result_problem};
use tracedecay_mcp::tools::binding::tool_dispatches_registered_project_reader;
use tracedecay_mcp::tools::response_trailers::{
    CODE_GRAPH_FRESHNESS_TRAILER_PREFIX, REQUEST_COST_TRAILER_PREFIX,
    TOKEN_ACCOUNTING_FOOTER_PREFIX, account_tool_result,
};
use tracedecay_mcp::{
    RESERVED_FLAGS_FOOTER, ToolDefinition, ToolResult, cli_tool_definition,
    get_tool_definitions_ref, render_tool_cli_help, short_tool_name,
};
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingSurface};

use crate::cli::dispatch::resolve_cli_application_surface;

mod application_family;
mod args;
use application_family::{FamilyTool, dispatch_cli_family_tool};
use args::{
    ParsedInvocation, canonical_tool_name, nearest_tool_name, parse_invocation,
    parse_whole_payload_invocation,
};
#[cfg(test)]
use args::{
    finalize_arrays, parse_invocation_with_stdin, parse_whole_payload_invocation_with_stdin,
};
#[cfg(test)]
use serde_json::Map;

/// Tools allowed to initialize an explicitly targeted project on first touch.
/// Bare invocations from an uninitialized cwd still get the
/// "run tracedecay init" guidance rather than a silent store.
const FIRST_TOUCH_STORE_TOOLS: &[&str] = &[
    "tracedecay_fact_store_add",
    "tracedecay_fact_store_curate",
    "tracedecay_fact_store_search",
    "tracedecay_fact_store_probe",
    "tracedecay_fact_store_related",
    "tracedecay_fact_store_reason",
    "tracedecay_fact_store_contradict",
    "tracedecay_fact_store_get",
    "tracedecay_fact_store_update",
    "tracedecay_fact_store_remove",
    "tracedecay_fact_store_supersede",
    "tracedecay_fact_store_list",
    "tracedecay_fact_feedback",
    "tracedecay_memory_status",
    "tracedecay_message_search",
    "tracedecay_lcm_status",
    "tracedecay_lcm_grep",
    "tracedecay_lcm_load_session",
    "tracedecay_lcm_describe",
    "tracedecay_lcm_expand",
    "tracedecay_lcm_expand_query",
];

fn tool_deadline_range_error() -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "{} exceeds the supported monotonic deadline range",
            TOOL_REQUEST_DEADLINE_ENV
        ),
    }
}

pub(crate) fn tool_command_deadline() -> Result<Duration> {
    tool_request_deadline()
}

fn catalog_discovery_unavailable(error: tracedecay_mcp::McpCatalogError) -> TraceDecayError {
    TraceDecayError::project_route(
        "mcp.catalog_discovery_unavailable",
        false,
        format!("MCP tool discovery is unavailable: {error}"),
    )
}

fn advertised_cli_definition(operation: ApplicationSurfaceOperation) -> Result<ToolDefinition> {
    cli_tool_definition(operation.mcp_tool_name()).map_err(catalog_discovery_unavailable)
}

/// Entry point for `tracedecay tool ...`.
#[tracing::instrument(name = "cli.tool.dispatch", level = "trace", skip_all)]
pub(crate) async fn run(
    profile: &ProfileRoot,
    project: Option<String>,
    name: Option<String>,
    args: Vec<String>,
) -> Result<()> {
    let json_requested = args.iter().any(|arg| arg == "--json");
    let tool_name = name.as_deref().map(canonical_tool_name);
    let result = run_inner(profile, project, name, args).await;
    match (&result, tool_name.as_deref()) {
        (Err(error), Some(tool_name)) if json_requested => {
            print_settled_route_refusal(tool_name, error).map_err(|print_error| {
                TraceDecayError::Config {
                    message: format!(
                        "{error}; the --json refusal could not be rendered: {print_error}"
                    ),
                }
            })?;
            result
        }
        _ => result,
    }
}

/// A `--json` call refused before any owner answered it prints the same
/// tool-result refusal an answered call does; any other unrendered refusal
/// prints its problem document.
fn print_settled_route_refusal(tool_name: &str, error: &TraceDecayError) -> Result<()> {
    let request_id =
        mint_global_request_id(GlobalRequestSurface::Cli).map_err(|_| TraceDecayError::Config {
            message: "could not allocate a refusal request id".to_owned(),
        })?;
    match tracedecay::mcp::tools::render_settled_route_refusal(
        BindingSurface::Cli,
        tool_name,
        request_id,
        error,
        &serde_json::json!({ "format": "json" }),
    ) {
        Some(rendered) => print_tool_output(&rendered?, CliToolOutput::Document)?,
        None if !matches!(error, TraceDecayError::ToolRefused(_)) => println!(
            "{}",
            tracedecay::mcp::tools::command_refusal_document(error)?
        ),
        None => {}
    }
    Ok(())
}

fn run_inner(
    profile: &ProfileRoot,
    project: Option<String>,
    name: Option<String>,
    args: Vec<String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'static>> {
    // Erase the deeply nested tool-dispatch future before it reaches the
    // measured wrapper so every profiling feature can compute its layout.
    let profile = profile.clone();
    Box::pin(async move {
        let profile = &profile;

        {
            let requested_name = name.as_deref().map(canonical_tool_name);
            tracing::trace!(name: "cli.tool.name", value = ?requested_name.as_deref().unwrap_or("list"));
        }
        let requested_operation = name
            .as_deref()
            .map(canonical_tool_name)
            .and_then(|canonical| ApplicationSurfaceOperation::from_tool_name(&canonical));
        if let Some(operation) = requested_operation {
            let parsed = if let Some(parsed) = parse_whole_payload_invocation(&args)? {
                parsed
            } else {
                let def = advertised_cli_definition(operation)?;
                parse_invocation(&def, &args)?
            };
            if parsed.show_help {
                print_tool_help(&advertised_cli_definition(operation)?);
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
                println!("{}", serde_json::to_string_pretty(&tool_args)?);
                return Ok(());
            }
            let explicit_project = project.or(parsed_project);
            let deadline = Instant::now()
                .checked_add(tool_command_deadline()?)
                .ok_or_else(tool_deadline_range_error)?;
            let tool_name = operation.mcp_tool_name();
            if RetainedSurfaceOperation::from_application(operation).is_some() {
                let mut tool_args = tool_args;
                let dispatch = DaemonToolDispatch::for_retained(
                    profile,
                    explicit_project,
                    tool_name,
                    &mut tool_args,
                )
                .await?;
                return dispatch_cli_retained(
                    profile, operation, tool_args, dispatch, raw_json, deadline,
                )
                .await;
            }
            if operation.is_profile_registry_read() {
                let mut tool_args = tool_args;
                let dispatch = DaemonToolDispatch::for_tool(
                    profile,
                    explicit_project,
                    tool_name,
                    &mut tool_args,
                );
                return dispatch_cli_profile_registry(
                    profile, operation, tool_args, dispatch, raw_json, deadline,
                )
                .await;
            }
            if operation.is_graph_tool() {
                let project_path =
                    selected_project_path(profile, explicit_project, tool_name, &tool_args).await?;
                return dispatch_cli_graph_tool(
                    profile,
                    operation,
                    tool_args,
                    project_path,
                    raw_json,
                    deadline,
                )
                .await;
            }
            if tracedecay_daemon_protocol::is_source_edit_operation(operation) {
                let project_path =
                    DaemonToolDispatch::project_scoped(profile, explicit_project, tool_name)
                        .project_path;
                return dispatch_cli_source_edit(
                    profile,
                    operation,
                    tool_args,
                    project_path,
                    raw_json,
                    deadline,
                )
                .await;
            }
            let (request, requested_format, output) =
                cli_surface_invocation(tool_name, tool_args, raw_json)
                    .map_err(ApplicationSurfaceAdapterError::into_trace_decay_error)?;
            return dispatch_cli_application_surface(
                profile,
                operation,
                request,
                DaemonToolDispatch::project_scoped(profile, explicit_project, tool_name)
                    .project_path,
                requested_format,
                output,
                deadline,
            )
            .await;
        }
        let defs = get_tool_definitions_ref().map_err(catalog_discovery_unavailable)?;

        let Some(raw_name) = name else {
            print_tool_list(defs);
            return Ok(());
        };

        let canonical = canonical_tool_name(&raw_name);
        // An application operation advertises one MCP definition under its
        // transport spelling; a canonical-identity request selects that same
        // definition instead of a second, unadvertised one.
        let advertised_name: &str = match requested_operation {
            Some(operation) => operation.mcp_tool_name(),
            None => canonical.as_str(),
        };
        let Some(def) = defs
            .iter()
            .find(|definition| definition.name == advertised_name)
        else {
            let suggestion = nearest_tool_name(&canonical, defs)
                .map(|name| format!(" Did you mean '{name}'?"))
                .unwrap_or_default();
            return Err(TraceDecayError::project_route(
                "unknown_tool",
                false,
                format!(
                    "unknown tool: '{raw_name}'.{suggestion} Run `tracedecay tool` to list available tools."
                ),
            ));
        };

        let parsed = parse_invocation(def, &args)?;
        if parsed.show_help {
            print_tool_help(def);
            return Ok(());
        }
        let ParsedInvocation {
            mut tool_args,
            project: parsed_project,
            raw_json,
            dry_run,
            show_help: _,
        } = parsed;

        if dry_run {
            println!("{}", serde_json::to_string_pretty(&tool_args)?);
            return Ok(());
        }

        let explicit_project = project.or(parsed_project);
        let deadline = Instant::now()
            .checked_add(tool_command_deadline()?)
            .ok_or_else(tool_deadline_range_error)?;
        if let Some(operation) = ApplicationSurfaceOperation::from_tool_name(&def.name)
            && RetainedSurfaceOperation::from_application(operation).is_some()
        {
            let dispatch = DaemonToolDispatch::for_retained(
                profile,
                explicit_project,
                &def.name,
                &mut tool_args,
            )
            .await?;
            return dispatch_cli_retained(
                profile, operation, tool_args, dispatch, raw_json, deadline,
            )
            .await;
        }
        if let Some(operation) = ApplicationSurfaceOperation::from_tool_name(&def.name)
            && operation.is_profile_registry_read()
        {
            let dispatch =
                DaemonToolDispatch::for_tool(profile, explicit_project, &def.name, &mut tool_args);
            return dispatch_cli_profile_registry(
                profile, operation, tool_args, dispatch, raw_json, deadline,
            )
            .await;
        }
        if let Some(operation) = ApplicationSurfaceOperation::from_tool_name(&def.name)
            && operation.is_graph_tool()
        {
            let project_path =
                selected_project_path(profile, explicit_project, &def.name, &tool_args).await?;
            return dispatch_cli_graph_tool(
                profile,
                operation,
                tool_args,
                project_path,
                raw_json,
                deadline,
            )
            .await;
        }
        if let Some(operation) = ApplicationSurfaceOperation::from_tool_name(&def.name)
            && tracedecay_daemon_protocol::is_source_edit_operation(operation)
        {
            let project_path =
                DaemonToolDispatch::project_scoped(profile, explicit_project, &def.name)
                    .project_path;
            return dispatch_cli_source_edit(
                profile,
                operation,
                tool_args,
                project_path,
                raw_json,
                deadline,
            )
            .await;
        }
        if let Some(operation) = ApplicationSurfaceOperation::from_tool_name(&def.name)
            && RetainedSurfaceOperation::from_application(operation).is_none()
        {
            let (request, requested_format, output) =
                cli_surface_invocation(&def.name, tool_args, raw_json)
                    .map_err(ApplicationSurfaceAdapterError::into_trace_decay_error)?;
            return dispatch_cli_application_surface(
                profile,
                operation,
                request,
                DaemonToolDispatch::project_scoped(profile, explicit_project, &def.name)
                    .project_path,
                requested_format,
                output,
                deadline,
            )
            .await;
        }
        if let Some(tool) = FamilyTool::from_tool_name(&def.name) {
            let project_path =
                DaemonToolDispatch::project_scoped(profile, explicit_project, &def.name)
                    .project_path;
            return dispatch_cli_family_tool(
                profile,
                tool,
                &def.name,
                tool_args,
                project_path,
                raw_json,
                deadline,
            )
            .await;
        }
        Err(TraceDecayError::Config {
            message: format!("{} has no typed CLI route", def.name),
        })
    })
}

/// Dispatch one catalogued application-surface operation on behalf of a
/// first-class CLI command (e.g. `tracedecay git status`).
///
/// This is the same normalized-argument, deadline, and warm-up-retry path the
/// `tracedecay tool` fallback uses, so first-class commands cannot drift from
/// the typed surface's transport behavior.
#[tracing::instrument(name = "cli.tool.catalog", level = "trace", skip_all)]
pub(crate) async fn dispatch_catalogued_cli_operation(
    profile: &ProfileRoot,
    operation: ApplicationSurfaceOperation,
    tool_args: Value,
    project: Option<PathBuf>,
    raw_json: bool,
) -> Result<()> {
    let deadline = Instant::now()
        .checked_add(tool_command_deadline()?)
        .ok_or_else(tool_deadline_range_error)?;
    let (request, requested_format, output) =
        cli_surface_invocation(operation.mcp_tool_name(), tool_args, raw_json)
            .map_err(ApplicationSurfaceAdapterError::into_trace_decay_error)?;
    dispatch_cli_application_surface(
        profile,
        operation,
        request,
        project,
        requested_format,
        output,
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
) -> std::result::Result<
    (Value, RequestedOutputFormat, CliToolOutput),
    ApplicationSurfaceAdapterError,
> {
    let normalized = adapt_application_tool_request(tool_name, tool_args)?;
    let output = CliToolOutput::new(raw_json, normalized.requested_format);
    let requested_format = if raw_json {
        RequestedOutputFormat::Json
    } else {
        normalized.requested_format
    };
    Ok((normalized.request, requested_format, output))
}

/// Every application-surface operation is project-scoped on the daemon side
/// (`DaemonInvocationRequest::requires_project`), so `project` must already be
/// the resolved project route, not just an explicit `--project`. A handshake
/// without a project reaches the profile-scoped projectless route, where those
/// operations can only answer `application.surface.unavailable` /
/// `not_found_or_not_authorized`.
#[tracing::instrument(name = "cli.tool.application", level = "trace", skip_all)]
async fn dispatch_cli_application_surface(
    profile: &ProfileRoot,
    operation: ApplicationSurfaceOperation,
    tool_args: Value,
    project: Option<PathBuf>,
    requested_format: RequestedOutputFormat,
    output: CliToolOutput,
    deadline: Instant,
) -> Result<()> {
    dispatch_cli_application_surface_inner(
        profile,
        operation,
        tool_args,
        project,
        requested_format,
        output,
        deadline,
    )
    .await
}

fn dispatch_cli_application_surface_inner(
    profile: &ProfileRoot,
    operation: ApplicationSurfaceOperation,
    tool_args: Value,
    project: Option<PathBuf>,
    requested_format: RequestedOutputFormat,
    output: CliToolOutput,
    deadline: Instant,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'static>> {
    // Erase the deeply nested application-surface future before it reaches
    // the measured wrapper so every profiling feature can compute its layout.
    let profile = profile.clone();
    Box::pin(async move {
        let profile = &profile;

        tracing::trace!(name: "cli.application.operation", value = ?operation.as_str());
        let request_id = mint_global_request_id(GlobalRequestSurface::Cli).map_err(|_| {
            TraceDecayError::Config {
                message: "could not allocate an application surface request id".to_owned(),
            }
        })?;
        let request = match parse_application_surface_request(operation, tool_args.clone()) {
            Ok(request) => request,
            Err(error) => {
                if let Ok(handshake) =
                    crate::commands::client_handshake(profile, project.as_deref())
                    && let Ok(client) =
                        tracedecay::daemon::invocation_client_for_current_client(profile, handshake)
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
                return Err(error.into_trace_decay_error());
            }
        };
        let handshake = tracedecay::daemon::handshake_for_current_client(
            profile,
            project.clone(),
            None,
            false,
            false,
        )?;
        let client = tracedecay::daemon::invocation_client_for_current_client(profile, handshake)?;
        // A cold daemon answers the mounting refusal while the project open
        // still warms in the background. The compatibility tool path rides
        // that state out through its project-open retry loop; the typed
        // surface path re-sends only that same refusal, until the CLI
        // deadline. Every other completed problem is the answer.
        let mut next_request = Some(request);
        let result = loop {
            let request = match next_request.take() {
                Some(request) => request,
                None => parse_application_surface_request(operation, tool_args.clone())
                    .map_err(ApplicationSurfaceAdapterError::into_trace_decay_error)?,
            };
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| i64::try_from(duration.as_micros()).unwrap_or(i64::MAX))
                .unwrap_or(i64::MAX);
            let remaining = deadline.saturating_duration_since(Instant::now());
            let request_deadline = Deadline::new(UtcMicros(
                now.saturating_add(i64::try_from(remaining.as_micros()).unwrap_or(i64::MAX)),
            ))
            .map_err(|error| TraceDecayError::Config {
                message: error.to_string(),
            })?;
            let cancellation =
                CancellationSignal::active(format!("cancellation.cli.{}", request_id.as_str()))
                    .map_err(|error| TraceDecayError::Config {
                        message: error.to_string(),
                    })?;
            let result = resolve_cli_application_surface(
                operation,
                request_id.clone(),
                request,
                requested_format,
                request_deadline,
                cancellation,
                Some(&client),
            )
            .await
            .map_err(ApplicationSurfaceAdapterError::into_trace_decay_error)?;
            let Some(delay) = crate::cli::dispatch::surface_retry_delay(&result) else {
                break result;
            };
            if deadline.saturating_duration_since(Instant::now()) <= delay {
                break result;
            }
            tokio::time::sleep(delay).await;
        };
        print_cli_application_surface(profile, project.as_deref(), result, output)
    })
}

/// The request deadline and cancellation for one CLI application attempt.
fn cli_request_controls(
    request_id: &tracedecay_contracts::RequestId,
    deadline: Instant,
) -> Result<(Deadline, CancellationSignal)> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_micros()).unwrap_or(i64::MAX))
        .unwrap_or(i64::MAX);
    let request_deadline = Deadline::new(UtcMicros(
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
    Ok((request_deadline, cancellation))
}

/// Run one retained memory, session, or workflow tool through the application
/// surface and print the same tool result its MCP call returns.
#[tracing::instrument(name = "cli.tool.retained", level = "trace", skip_all)]
async fn dispatch_cli_retained(
    profile: &ProfileRoot,
    operation: ApplicationSurfaceOperation,
    tool_args: Value,
    dispatch: DaemonToolDispatch,
    raw_json: bool,
    deadline: Instant,
) -> Result<()> {
    let tool_name = operation.mcp_tool_name();
    let request_id =
        mint_global_request_id(GlobalRequestSurface::Cli).map_err(|_| TraceDecayError::Config {
            message: "could not allocate an application surface request id".to_owned(),
        })?;
    let client = tracedecay::daemon::invocation_client_for_current_client(
        profile,
        dispatch.handshake(profile)?,
    )?;
    // The mounting refusal precedes admission; re-send it until the deadline.
    let execution = loop {
        let (request_deadline, cancellation) = cli_request_controls(&request_id, deadline)?;
        let execution = tracedecay::mcp::tools::execute_retained_surface_tool(
            tracedecay_tool_catalog::BindingSurface::Cli,
            operation,
            tool_args.clone(),
            Some(&client),
            Some(request_id.clone()),
            Some(request_deadline),
            Some(cancellation),
        )
        .await?;
        let Some(delay) = execution
            .result
            .as_ref()
            .err()
            .and_then(|problem| problem.problem.owner_mount_resend_delay())
        else {
            break execution;
        };
        if deadline.saturating_duration_since(Instant::now()) <= delay {
            break execution;
        }
        tokio::time::sleep(delay).await;
    };
    let response_handle_root = cli_response_handle_root(profile, dispatch.project_path.as_deref())?;
    let mut result = tracedecay::mcp::tools::render_retained_execution(
        response_handle_root.as_deref(),
        &execution,
    )?;
    account_tool_result(dispatch.project_path.as_deref(), &mut result);
    tracedecay_mcp::tool_errors::mark_semantic_tool_error(&mut result);
    print_tool_output(&result, CliToolOutput::for_args(raw_json, &tool_args))?;
    tool_result_process_outcome(&result.value, tool_name)
}

/// Run one source-edit tool through the application surface and print the
/// same tool result its MCP call returns.
#[tracing::instrument(name = "cli.tool.source_edit", level = "trace", skip_all)]
async fn dispatch_cli_source_edit(
    profile: &ProfileRoot,
    operation: ApplicationSurfaceOperation,
    tool_args: Value,
    project: Option<PathBuf>,
    raw_json: bool,
    deadline: Instant,
) -> Result<()> {
    let tool_name = operation.mcp_tool_name();
    let request_id =
        mint_global_request_id(GlobalRequestSurface::Cli).map_err(|_| TraceDecayError::Config {
            message: "could not allocate an application surface request id".to_owned(),
        })?;
    let handshake = tracedecay::daemon::handshake_for_current_client(
        profile,
        project.clone(),
        None,
        false,
        false,
    )?;
    let client = tracedecay::daemon::invocation_client_for_current_client(profile, handshake)?;
    // A cold daemon refuses with the mounting problem while the project open
    // warms; that refusal precedes admission, so it is re-sent until the CLI
    // deadline like every other surface.
    let outcome = loop {
        let (request_deadline, cancellation) = cli_request_controls(&request_id, deadline)?;
        let outcome = tracedecay_mcp::handlers::edit::run_source_edit(
            tracedecay_tool_catalog::BindingSurface::Cli,
            operation,
            &tool_args,
            tracedecay_mcp::handlers::edit::SourceEditInvocationContext {
                executor: Some(&client),
                target: tracedecay_contracts::InvocationTarget::CurrentProject,
                request_id: Some(request_id.clone()),
                deadline: Some(request_deadline),
                cancellation: Some(cancellation),
            },
        )
        .await?;
        let Some(delay) = outcome
            .as_ref()
            .err()
            .and_then(|refusal| refusal.problem.problem.owner_mount_resend_delay())
        else {
            break outcome;
        };
        if deadline.saturating_duration_since(Instant::now()) <= delay {
            break outcome;
        }
        tokio::time::sleep(delay).await;
    };
    let response_handle_root = cli_response_handle_root(profile, project.as_deref())?;
    let mut result = tracedecay_mcp::handlers::edit::render_source_edit_outcome(
        response_handle_root.as_deref(),
        operation,
        &tool_args,
        outcome,
    )?;
    account_tool_result(project.as_deref(), &mut result);
    tracedecay_mcp::tool_errors::mark_semantic_tool_error(&mut result);
    print_tool_output(&result, CliToolOutput::for_args(raw_json, &tool_args))?;
    tool_result_process_outcome(&result.value, tool_name)
}

/// Run one graph or port read through its project owner and print the same
/// tool result its MCP call returns.
#[tracing::instrument(name = "cli.tool.graph_tool", level = "trace", skip_all)]
async fn dispatch_cli_graph_tool(
    profile: &ProfileRoot,
    operation: ApplicationSurfaceOperation,
    tool_args: Value,
    project: Option<PathBuf>,
    raw_json: bool,
    deadline: Instant,
) -> Result<()> {
    let tool_name = operation.mcp_tool_name();
    let handshake = tracedecay::daemon::handshake_for_current_client(
        profile,
        project.clone(),
        None,
        false,
        false,
    )?;
    let outcome =
        invoke_cli_graph_tool(profile, handshake, operation, &tool_args, deadline).await?;
    let response_handle_root = cli_response_handle_root(profile, project.as_deref())?;
    let mut result = match outcome {
        Ok(completion) => tracedecay_mcp::handlers::graph_tool::render_graph_tool(
            response_handle_root.as_deref(),
            &tool_args,
            completion,
        )?,
        Err(refusal) => refusal.render(response_handle_root.as_deref(), &tool_args)?,
    };
    account_tool_result(project.as_deref(), &mut result);
    tracedecay_mcp::tool_errors::mark_semantic_tool_error(&mut result);
    print_tool_output(&result, CliToolOutput::for_args(raw_json, &tool_args))?;
    tool_result_process_outcome(&result.value, tool_name)
}

/// The project a graph-tool read or a selected retained effect answers for.
/// Those owners answer for the handshake's project, so a registered-project
/// reader's `project_selector` is resolved here, through the daemon's
/// registry, to that exact registered project's root; it never falls back to
/// the cwd project.
async fn selected_project_path(
    profile: &ProfileRoot,
    explicit_project: Option<String>,
    tool_name: &str,
    tool_args: &Value,
) -> Result<Option<PathBuf>> {
    let invalid_selector = |detail: String| {
        TraceDecayError::project_route("project_route_invalid_selector", false, detail)
    };
    if !tool_dispatches_registered_project_reader(tool_name) {
        if tool_args.get("project_selector").is_some() {
            return Err(invalid_selector(format!(
                "{tool_name} answers only for the connected project and does not accept project_selector"
            )));
        }
        return Ok(
            DaemonToolDispatch::project_scoped(profile, explicit_project, tool_name).project_path,
        );
    }
    let Some(project_id) = registered_project_selector_id(tool_args)? else {
        return Ok(
            DaemonToolDispatch::project_scoped(profile, explicit_project, tool_name).project_path,
        );
    };
    if explicit_project.is_some() {
        return Err(invalid_selector(
            "--project and project_selector both name a project; pass only one".to_owned(),
        ));
    }
    let request = AdminCliSurfaceRequestV1::RegistryContext {
        project_arg: Some(PathBuf::from(project_id)),
    };
    // The registry also matches aliases and paths; only the exact project id
    // is this selector's project.
    match crate::commands::admin_cli_result(profile, None, request).await? {
        AdminCliResultV1::RegistryContext(AdminCliRegistryContextV1::Ok { project, .. })
            if project.project_id == project_id =>
        {
            Ok(Some(PathBuf::from(project.canonical_root)))
        }
        AdminCliResultV1::RegistryContext(_) => Err(registered_project_not_found(project_id)),
        _ => Err(crate::commands::admin_cli_result_mismatch(
            "registry_context",
        )),
    }
}

/// Run one graph-tool owner operation for `project` through the daemon and
/// return its settled outcome.
async fn invoke_cli_graph_tool(
    profile: &ProfileRoot,
    handshake: DaemonHandshake,
    operation: ApplicationSurfaceOperation,
    tool_args: &Value,
    deadline: Instant,
) -> Result<tracedecay::mcp::tools::GraphToolOutcome> {
    let request_id =
        mint_global_request_id(GlobalRequestSurface::Cli).map_err(|_| TraceDecayError::Config {
            message: "could not allocate an application surface request id".to_owned(),
        })?;
    let client = tracedecay::daemon::invocation_client_for_current_client(profile, handshake)?;
    // A cold daemon refuses with the mounting problem while the project open
    // warms; that refusal precedes admission, so it is re-sent until the CLI
    // deadline like every other surface.
    loop {
        let (request_deadline, cancellation) = cli_request_controls(&request_id, deadline)?;
        let outcome = tracedecay::mcp::tools::execute_graph_tool_surface(
            tracedecay_tool_catalog::BindingSurface::Cli,
            operation,
            tool_args.clone(),
            Some(&client),
            Some(request_id.clone()),
            Some(request_deadline),
            Some(cancellation),
        )
        .await?;
        let Some(delay) = outcome
            .as_ref()
            .err()
            .and_then(|refusal| refusal.problem.problem.owner_mount_resend_delay())
        else {
            return Ok(outcome);
        };
        if deadline.saturating_duration_since(Instant::now()) <= delay {
            return Ok(outcome);
        }
        tokio::time::sleep(delay).await;
    }
}

/// The typed result a first-party command asks the project's owner for; a
/// refusal is the command's error.
pub(crate) async fn owner_operation_result(
    profile: &ProfileRoot,
    handshake: DaemonHandshake,
    operation: ApplicationSurfaceOperation,
    arguments: Value,
    deadline: Instant,
) -> Result<tracedecay_contracts::graph_tool::GraphToolResultV1> {
    match invoke_cli_graph_tool(profile, handshake, operation, &arguments, deadline).await? {
        Ok(completion) => Ok(completion.result),
        Err(refusal) => Err(refusal.into_error()),
    }
}

/// Run one profile registry read through the daemon's profile owner and print
/// the same tool result its MCP call returns. The handshake's project, when
/// the dispatch names one, only marks that project active.
#[tracing::instrument(name = "cli.tool.profile_registry", level = "trace", skip_all)]
async fn dispatch_cli_profile_registry(
    profile: &ProfileRoot,
    operation: ApplicationSurfaceOperation,
    tool_args: Value,
    dispatch: DaemonToolDispatch,
    raw_json: bool,
    deadline: Instant,
) -> Result<()> {
    let tool_name = operation.mcp_tool_name();
    let request_id =
        mint_global_request_id(GlobalRequestSurface::Cli).map_err(|_| TraceDecayError::Config {
            message: "could not allocate an application surface request id".to_owned(),
        })?;
    let client = tracedecay::daemon::invocation_client_for_current_client(
        profile,
        dispatch.handshake(profile)?,
    )?;
    let (request_deadline, cancellation) = cli_request_controls(&request_id, deadline)?;
    let outcome = tracedecay::mcp::tools::execute_graph_tool_surface(
        tracedecay_tool_catalog::BindingSurface::Cli,
        operation,
        tool_args.clone(),
        Some(&client),
        Some(request_id),
        Some(request_deadline),
        Some(cancellation),
    )
    .await?;
    let response_handle_root = cli_response_handle_root(profile, dispatch.project_path.as_deref())?;
    let mut result = match outcome {
        Ok(completion) => tracedecay_mcp::handlers::graph_tool::render_graph_tool(
            response_handle_root.as_deref(),
            &tool_args,
            completion,
        )?,
        Err(refusal) => refusal.render(response_handle_root.as_deref(), &tool_args)?,
    };
    account_tool_result(dispatch.project_path.as_deref(), &mut result);
    tracedecay_mcp::tool_errors::mark_semantic_tool_error(&mut result);
    print_tool_output(&result, CliToolOutput::for_args(raw_json, &tool_args))?;
    tool_result_process_outcome(&result.value, tool_name)
}

/// Enrolled project's handle root, or none when that path has no store.
fn cli_response_handle_root(
    profile: &ProfileRoot,
    project: Option<&Path>,
) -> Result<Option<PathBuf>> {
    let Some(project) = project else {
        return Ok(None);
    };
    Ok(
        tracedecay_runtime_core::storage::resolve_persisted_layout(project, profile.data_dir())?
            .map(|layout| layout.response_handle_root),
    )
}

const OWNER_MOUNT_RESEND_DELAY: Duration = Duration::from_millis(250);

/// Prints one settled application-surface call through the renderer its MCP
/// call uses, as every other tool call prints.
fn print_cli_application_surface(
    profile: &ProfileRoot,
    project: Option<&Path>,
    result: ApplicationSurfaceInvocationResult,
    output: CliToolOutput,
) -> Result<()> {
    let application_problem = result.result.as_ref().err().map(|problem| {
        TraceDecayError::tool_refused(
            result.operation.mcp_tool_name(),
            Some(problem.problem.code.clone()),
            Some(problem.problem.message.clone()),
        )
    });
    let response_handle_root = cli_response_handle_root(profile, project)?;
    let mut rendered = tracedecay::mcp::tools::render_application_surface_result(
        response_handle_root.as_deref(),
        &result,
    )?;
    account_tool_result(project, &mut rendered);
    mark_semantic_tool_error(&mut rendered);
    print_tool_output(&rendered, output)?;
    if application_problem.is_some() {
        std::io::stdout().flush()?;
    }
    match application_problem {
        Some(refusal) => Err(refusal),
        None => Ok(()),
    }
}

struct DaemonToolDispatch {
    project_path: Option<PathBuf>,
    allow_init: bool,
}

impl DaemonToolDispatch {
    /// `tool_args` may be seeded: a registry read that names an uninitialised
    /// `--project` but no `path` inherits that project as its `path`.
    fn for_tool(
        profile: &ProfileRoot,
        explicit_project: Option<String>,
        tool_name: &str,
        tool_args: &mut Value,
    ) -> Self {
        // Profile-targeted calls (Hermes user LCM/memory) must never invent a
        // project from cwd. Hermes intentionally runs those calls with cwd=/ so
        // Hermes home is never mistaken for a TraceDecay project.
        if targets_profile(tool_name, tool_args) {
            return Self {
                project_path: None,
                allow_init: false,
            };
        }
        // Registry reads need no mounted project: a project, when one is
        // connected, only marks the active listing entry. An explicit
        // `--project` that is not an initialised project therefore routes
        // projectless instead of being refused for a project the read never
        // depended on.
        if ApplicationSurfaceOperation::from_tool_name(tool_name)
            .is_some_and(ApplicationSurfaceOperation::is_profile_registry_read)
        {
            return Self::registry_scoped(profile, explicit_project, tool_name, tool_args);
        }
        Self::project_scoped(profile, explicit_project, tool_name)
    }

    /// A retained effect with a `project_selector` connects to the selected
    /// registered project, so the write lands in that project's store. Retained
    /// reads keep the connected project and read the selected one read-only.
    async fn for_retained(
        profile: &ProfileRoot,
        explicit_project: Option<String>,
        tool_name: &str,
        tool_args: &mut Value,
    ) -> Result<Self> {
        let selected_effect = tool_args.get("project_selector").is_some()
            && !targets_profile(tool_name, tool_args)
            && tool_dispatches_registered_project_reader(tool_name)
            && RetainedSurfaceOperation::from_tool_name(tool_name)
                .is_some_and(retained_surface_operation_is_effect);
        if !selected_effect {
            return Ok(Self::for_tool(
                profile,
                explicit_project,
                tool_name,
                tool_args,
            ));
        }
        Ok(Self {
            project_path: selected_project_path(profile, explicit_project, tool_name, tool_args)
                .await?,
            allow_init: false,
        })
    }

    /// Registry reads never initialise anything, and follow the canonical
    /// resolution order (`ProfileRoot::discover_project_root`):
    ///
    /// * An explicit `--project` is honoured verbatim, with no discovery and
    ///   no ambient-root filter. An initialised root becomes the project
    ///   connection so the daemon can mark it active. An existing directory
    ///   that is not an initialised root routes projectless, because the read
    ///   never depended on that project; `project_context` then inherits the
    ///   named directory as its `path` unless the caller already gave a
    ///   `path` or `project_selector`. Anything else (a path that does not
    ///   exist, or is not a directory) is still handed to the daemon so the
    ///   caller receives the same typed project-route refusal every
    ///   project-scoped tool reports, never a silently unscoped success.
    /// * Without `--project`, cwd walks up to the nearest initialised
    ///   ancestor, and stays projectless when there is none.
    fn registry_scoped(
        profile: &ProfileRoot,
        explicit_project: Option<String>,
        tool_name: &str,
        tool_args: &mut Value,
    ) -> Self {
        let project_path = match explicit_project {
            Some(path) => {
                let explicit = tracedecay_configuration::resolve_path(Some(path));
                if explicit.is_dir() && !profile.is_initialized_project_root(&explicit) {
                    if tool_name == "tracedecay_project_context" {
                        seed_registry_context_path(tool_args, &explicit);
                    }
                    None
                } else {
                    Some(explicit)
                }
            }
            None => std::env::current_dir()
                .ok()
                .and_then(|cwd| implicit_tool_project_path(profile, &cwd)),
        };
        Self {
            project_path,
            allow_init: false,
        }
    }

    fn project_scoped(
        profile: &ProfileRoot,
        explicit_project: Option<String>,
        tool_name: &str,
    ) -> Self {
        // An explicit --project wins. Otherwise only route to the nearest
        // initialised ancestor. Keeping an unscoped invocation projectless is
        // important: falling back to cwd can turn a broad directory such as
        // the user profile into an accidental project handshake.
        let explicitly_targeted = explicit_project.is_some();
        let project_path = match explicit_project {
            Some(path) => Some(tracedecay_configuration::resolve_path(Some(path))),
            None => std::env::current_dir()
                .ok()
                .and_then(|cwd| implicit_tool_project_path(profile, &cwd)),
        };
        let allow_init = explicitly_targeted && FIRST_TOUCH_STORE_TOOLS.contains(&tool_name);

        Self {
            project_path,
            allow_init,
        }
    }

    fn handshake(&self, profile: &ProfileRoot) -> Result<DaemonHandshake> {
        tracedecay::daemon::handshake_for_current_client(
            profile,
            self.project_path.clone(),
            None,
            false,
            self.allow_init,
        )
    }
}

/// Whether a retained call addresses the authenticated profile's own stores.
fn targets_profile(tool_name: &str, tool_args: &Value) -> bool {
    RetainedSurfaceOperation::from_tool_name(tool_name).is_some_and(|operation| {
        tracedecay::mcp::tools::retained_tool_target(operation, tool_args)
            .is_ok_and(|target| target == tracedecay_contracts::InvocationTarget::Profile)
    })
}

fn implicit_tool_project_path(profile: &ProfileRoot, cwd: &Path) -> Option<PathBuf> {
    profile.discover_project_root(cwd)
}

/// `project_context` with an explicit uninitialised `--project` and no
/// selector asks about that directory: seed `path` from it rather than
/// refusing for a missing parameter the caller did supply.
fn seed_registry_context_path(tool_args: &mut Value, explicit_project: &Path) {
    let names_a_project = tool_args.get("path").is_some_and(|path| !path.is_null())
        || tool_args
            .get("project_selector")
            .is_some_and(|selector| !selector.is_null());
    if names_a_project {
        return;
    }
    if let Some(args) = tool_args.as_object_mut() {
        args.insert(
            "path".to_owned(),
            Value::String(explicit_project.to_string_lossy().into_owned()),
        );
    }
}

pub(crate) const TEST_GATE_REFUSAL: &str = "test_gate";

pub(crate) const TEST_GATE_EXIT_CODE: u8 = 4;

fn tool_result_process_outcome(result_value: &Value, tool_name: &str) -> Result<()> {
    if let Some(wait) = result_value.pointer("/structuredContent/wait") {
        let wait: CodeIndexReadinessWaitOutcomeV1 = serde_json::from_value(wait.clone())?;
        let refusal = match wait {
            CodeIndexReadinessWaitOutcomeV1::Reached => None,
            CodeIndexReadinessWaitOutcomeV1::TimedOut { last_state } => Some((
                CODE_INDEX_READINESS_WAIT_TIMED_OUT,
                format!(
                    "wait_for timed out before the index reached the requested state; last \
                     state: {last_state}"
                ),
            )),
            CodeIndexReadinessWaitOutcomeV1::Unavailable { reason } => Some((
                CODE_INDEX_READINESS_WAIT_UNAVAILABLE,
                format!("wait_for cannot reach the requested state: {reason}"),
            )),
        };
        if let Some((code, reason)) = refusal {
            std::io::stdout().flush()?;
            return Err(TraceDecayError::tool_refused(
                tool_name,
                Some(code.to_owned()),
                Some(reason),
            ));
        }
    }
    if result_value
        .pointer("/structuredContent/test_gate/verdict")
        .and_then(Value::as_str)
        == Some("fail")
    {
        let untested = result_value
            .pointer("/structuredContent/test_gate/untested")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        std::io::stdout().flush()?;
        return Err(TraceDecayError::tool_refused(
            tool_name,
            Some(TEST_GATE_REFUSAL.to_owned()),
            Some(format!("untested blast radius: {untested}")),
        ));
    }
    if result_value.get("isError").and_then(Value::as_bool) != Some(true) {
        return Ok(());
    }
    // `print_tool_output` already wrote the exact daemon payload. Flush before
    // returning the status-only error so the process boundary can drop its
    // profiling guard and then return the nonzero `ExitCode`.
    std::io::stdout().flush()?;
    let problem = tool_result_problem(result_value);
    let problem_text = |key: &str| {
        problem
            .and_then(|problem| problem.get(key))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    Err(TraceDecayError::tool_refused(
        tool_name,
        problem_text("code"),
        problem_text("message"),
    ))
}

/// How `tracedecay tool` prints a completed call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CliToolOutput {
    /// `--json`: the one tool-result document.
    Document,
    /// `format: "json"`: an answer's whole typed result, or a refusal's
    /// rendered envelope.
    TypedResult,
    /// The rendered text body.
    Text,
}

impl CliToolOutput {
    fn new(raw_json: bool, requested_format: RequestedOutputFormat) -> Self {
        match (raw_json, requested_format) {
            (true, _) => Self::Document,
            (false, RequestedOutputFormat::Json) => Self::TypedResult,
            (false, RequestedOutputFormat::Markdown) => Self::Text,
        }
    }

    fn for_args(raw_json: bool, tool_args: &Value) -> Self {
        Self::new(raw_json, requested_output_format(tool_args))
    }
}

/// Prints one completed tool call; the beside-result blocks go to stderr
/// unless the document on stdout already carries them.
fn print_tool_output(result: &ToolResult, output: CliToolOutput) -> Result<()> {
    println!("{}", rendered_tool_output(result, output)?);
    if output != CliToolOutput::Document {
        print_beside_result_blocks(&result.value);
    }
    Ok(())
}

fn print_beside_result_blocks(result_value: &Value) {
    for block in beside_result_blocks(result_value) {
        eprintln!("{block}");
    }
}

/// The bytes `tracedecay tool` writes to stdout for a completed call. Status
/// is decided separately from `isError`.
fn rendered_tool_output(result: &ToolResult, output: CliToolOutput) -> Result<String> {
    match (output, result.structured_result()) {
        (CliToolOutput::Document, _) => {
            Ok(serde_json::to_string_pretty(&json_tool_document(result)?)?)
        }
        (CliToolOutput::TypedResult, Some(structured)) if !is_error_result(&result.value) => {
            Ok(structured.to_string())
        }
        (CliToolOutput::TypedResult | CliToolOutput::Text, _) => {
            Ok(join_content_text(&result.value))
        }
    }
}

fn is_error_result(result_value: &Value) -> bool {
    result_value.get("isError").and_then(Value::as_bool) == Some(true)
}

/// The one `tracedecay tool --json` document for every tool: the MCP tool
/// result's `content`, its `isError`, and `structuredContent` holding the
/// refusal's typed problem record or the answer's whole typed result.
fn json_tool_document(result: &ToolResult) -> Result<Value> {
    let mut document = result.value.clone();
    let is_error = is_error_result(&document);
    let Some(object) = document.as_object_mut() else {
        return Err(TraceDecayError::Config {
            message: "the tool rendered a result that is not a JSON object".to_owned(),
        });
    };
    object.insert("isError".to_owned(), serde_json::json!(is_error));
    if !is_error {
        let structured = result
            .structured_result()
            .ok_or_else(|| TraceDecayError::Config {
                message: "the tool rendered its answer without its typed result".to_owned(),
            })?;
        object.insert("structuredContent".to_owned(), structured.clone());
    }
    Ok(document)
}

/// Joins every payload `content[*].text` block in an MCP tool result,
/// separated by a blank line. Handlers sometimes prepend a warning/notice block
/// ahead of the real payload; printing only `content[0].text` would silently
/// drop the payload. The beside-result stale-graph and cost trailers and the
/// token-accounting footer blocks are excluded (see [`beside_result_blocks`]): with
/// `--format json` the payload block is the whole stdout document and a
/// trailing block would make it unparseable. Falls back to the empty string
/// when no text blocks exist.
fn join_content_text(result_value: &Value) -> String {
    content_text_blocks(result_value)
        .filter(|text| !is_beside_result_block(text))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The stale-graph and cost trailers and token-accounting footer blocks, printed to
/// stderr.
fn beside_result_blocks(result_value: &Value) -> Vec<String> {
    content_text_blocks(result_value)
        .filter(|text| is_beside_result_block(text))
        .map(|text| text.trim().to_owned())
        .collect()
}

fn content_text_blocks(result_value: &Value) -> impl Iterator<Item = &str> {
    result_value
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .filter(|text| !text.is_empty())
}

fn is_beside_result_block(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with(TOKEN_ACCOUNTING_FOOTER_PREFIX)
        || text.starts_with(CODE_GRAPH_FRESHNESS_TRAILER_PREFIX)
        || text.starts_with(REQUEST_COST_TRAILER_PREFIX)
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
        "Available tools ({}; TraceDecay {}), run `tracedecay tool <name> --help` for parameters, then",
        defs.len(),
        crate::product_runtime::PRODUCT_BUILD_VERSION
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
/// already groups handlers (`graph`, `info`, `git`, `health`, `edit`,
/// `memory`). Tools that don't match any prefix fall under `other`.
fn group_for(def: &ToolDefinition) -> &'static str {
    let n = def.name.as_str();
    if ApplicationSurfaceOperation::from_tool_name(n).is_some() {
        "application"
    } else if n == "tracedecay_str_replace"
        || n == "tracedecay_multi_str_replace"
        || n == "tracedecay_insert_at"
        || n == "tracedecay_ast_grep_rewrite"
        || n == "tracedecay_replace_symbol"
        || n == "tracedecay_insert_at_symbol"
        || n == "tracedecay_move_symbol"
        || n == "tracedecay_rename_symbol"
    {
        "edit"
    } else if n.starts_with("tracedecay_fact_store_")
        || n == "tracedecay_fact_feedback"
        || n == "tracedecay_memory_status"
    {
        "memory & session"
    } else if n == "tracedecay_runtime" {
        "health"
    } else if n == "tracedecay_call_chain"
        || n == "tracedecay_impact"
        || n == "tracedecay_file_dependents"
        || n == "tracedecay_similar"
        || n == "tracedecay_rename_preview"
    {
        "graph"
    } else if n == "tracedecay_run_affected_tests" {
        "workflow"
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
