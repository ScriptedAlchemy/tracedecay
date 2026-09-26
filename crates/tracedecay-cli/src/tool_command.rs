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

use serde_json::Value;
use tokio::time::{Instant, timeout_at};

use tracedecay::daemon::call_default_tool_awaiting_project_open;
use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_contracts::{CancellationSignal, Deadline, RetainedSurfaceOperation};
use tracedecay_daemon_protocol::{
    ApplicationSurfaceAdapterError, ApplicationSurfaceInvocationResult,
    adapt_application_tool_request, parse_application_surface_request,
};
use tracedecay_daemon_protocol::{
    DaemonHandshake, RequestedOutputFormat, TOOL_REQUEST_DEADLINE_ENV, tool_request_deadline,
};
use tracedecay_daemon_service::application_surface::observe_surface_argument_rejection;
use tracedecay_domain::UtcMicros;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_mcp::tools::response_trailers::{
    CODE_GRAPH_FRESHNESS_TRAILER_PREFIX, TOKEN_ACCOUNTING_FOOTER_PREFIX, account_tool_result,
};
use tracedecay_mcp::{
    RESERVED_FLAGS_FOOTER, ToolDefinition, get_tool_definitions, internal_daemon_tool_definition,
    render_tool_cli_help, short_tool_name,
};
use tracedecay_runtime_core::storage::resolve_enrolled_layout_for_current_profile;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use crate::cli::dispatch::resolve_cli_application_surface;
use crate::commands::{recover_truncated_mcp_result, reject_truncation_envelope};

mod application_family;
mod args;
use application_family::{FamilyTool, dispatch_cli_family_tool};
use args::{
    ParsedInvocation, canonical_tool_name, nearest_tool_name, parse_invocation,
    parse_whole_payload_invocation,
};
#[cfg(test)]
use args::{
    edit_distance, finalize_arrays, parse_invocation_with_stdin,
    parse_whole_payload_invocation_with_stdin,
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

/// Tools that read the profile's project registry. They need no mounted
/// project: a project, when one is connected, only marks the active listing
/// entry. An explicit `--project` that is not an initialised project therefore
/// routes projectless instead of being refused for a project the read never
/// depended on.
const PROFILE_REGISTRY_TOOLS: &[&str] = &[
    "tracedecay_project_list",
    "tracedecay_project_search",
    "tracedecay_project_context",
];

fn tool_deadline_range_error() -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "{} exceeds the supported monotonic deadline range",
            TOOL_REQUEST_DEADLINE_ENV
        ),
    }
}

fn tool_command_deadline() -> Result<Duration> {
    tool_request_deadline()
}

fn tool_timeout_error(tool_name: &str) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "tool request timed out before deadline: {tool_name}; request outcome may be unknown"
        ),
    }
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
#[hotpath::measure(label = "cli.tool.dispatch", future = true)]
pub(crate) async fn run(
    project: Option<String>,
    name: Option<String>,
    args: Vec<String>,
) -> Result<()> {
    run_inner(project, name, args).await
}

fn run_inner(
    project: Option<String>,
    name: Option<String>,
    args: Vec<String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'static>> {
    // Erase the deeply nested tool-dispatch future before it reaches the
    // measured wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        #[cfg(feature = "hotpath")]
        {
            let requested_name = name.as_deref().map(canonical_tool_name);
            hotpath::val!("cli.tool.name").set(&requested_name.as_deref().unwrap_or("list"));
        }
        let requested_operation = name
            .as_deref()
            .map(canonical_tool_name)
            .and_then(|canonical| ApplicationSurfaceOperation::from_tool_name(&canonical));
        if let Some(operation) = requested_operation
            && let Some(parsed) = parse_whole_payload_invocation(&args)?
        {
            let ParsedInvocation {
                tool_args,
                project: parsed_project,
                raw_json,
                dry_run: _,
                show_help: _,
            } = parsed;
            let explicit_project = project.or(parsed_project);
            let deadline = Instant::now()
                .checked_add(tool_command_deadline()?)
                .ok_or_else(tool_deadline_range_error)?;
            let tool_name = operation.mcp_tool_name();
            if RetainedSurfaceOperation::from_application(operation).is_some() {
                let mut tool_args = tool_args;
                let dispatch =
                    DaemonToolDispatch::for_tool(explicit_project, tool_name, &mut tool_args);
                return dispatch_cli_retained(operation, tool_args, dispatch, raw_json, deadline)
                    .await;
            }
            if operation.is_graph_tool() {
                let project_path =
                    DaemonToolDispatch::project_scoped(explicit_project, tool_name).project_path;
                return dispatch_cli_graph_tool(
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
                    DaemonToolDispatch::project_scoped(explicit_project, tool_name).project_path;
                return dispatch_cli_source_edit(
                    operation,
                    tool_args,
                    project_path,
                    raw_json,
                    deadline,
                )
                .await;
            }
            let (request, requested_format) =
                cli_surface_invocation(tool_name, tool_args, raw_json).map_err(|error| {
                    TraceDecayError::Config {
                        message: error.to_string(),
                    }
                })?;
            return dispatch_cli_application_surface(
                operation,
                request,
                DaemonToolDispatch::project_scoped(explicit_project, tool_name).project_path,
                requested_format,
                deadline,
            )
            .await;
        }
        let defs = get_tool_definitions().map_err(|error| {
            TraceDecayError::project_route(
                "mcp.catalog_discovery_unavailable",
                false,
                format!("MCP tool discovery is unavailable: {error}"),
            )
        })?;

        let Some(raw_name) = name else {
            print_tool_list(&defs);
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
        let internal_def = internal_daemon_tool_definition(advertised_name);
        let Some(def) = defs
            .iter()
            .find(|definition| definition.name == advertised_name)
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
            mut tool_args,
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
        if let Some(operation) = ApplicationSurfaceOperation::from_tool_name(&def.name)
            && RetainedSurfaceOperation::from_application(operation).is_some()
        {
            let dispatch =
                DaemonToolDispatch::for_tool(explicit_project, &def.name, &mut tool_args);
            return dispatch_cli_retained(operation, tool_args, dispatch, raw_json, deadline).await;
        }
        if let Some(operation) = ApplicationSurfaceOperation::from_tool_name(&def.name)
            && operation.is_graph_tool()
        {
            let project_path =
                DaemonToolDispatch::project_scoped(explicit_project, &def.name).project_path;
            return dispatch_cli_graph_tool(operation, tool_args, project_path, raw_json, deadline)
                .await;
        }
        if let Some(operation) = ApplicationSurfaceOperation::from_tool_name(&def.name)
            && tracedecay_daemon_protocol::is_source_edit_operation(operation)
        {
            let project_path =
                DaemonToolDispatch::project_scoped(explicit_project, &def.name).project_path;
            return dispatch_cli_source_edit(
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
            let (request, requested_format) =
                cli_surface_invocation(&def.name, tool_args, raw_json).map_err(|error| {
                    TraceDecayError::Config {
                        message: error.to_string(),
                    }
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
        if let Some(tool) = FamilyTool::from_tool_name(&def.name) {
            let project_path =
                DaemonToolDispatch::project_scoped(explicit_project, &def.name).project_path;
            return dispatch_cli_family_tool(
                tool,
                &def.name,
                tool_args,
                project_path,
                raw_json,
                deadline,
            )
            .await;
        }
        // Finding `def` in the host-filtered MCP definitions is the retained
        // compatibility owner's admission authority. This point is reachable
        // only after every typed branch above rejected the name, so composing
        // the application catalog again can only return `None`; rebuilding a
        // second advertised-name set likewise repeats the exact membership
        // check that selected `def`.
        let dispatch = DaemonToolDispatch::for_tool(explicit_project, &def.name, &mut tool_args);
        dispatch_compatibility_tool(dispatch, &def.name, tool_args, raw_json, deadline).await
    })
}

/// Dispatch one catalogued application-surface operation on behalf of a
/// first-class CLI command (e.g. `tracedecay git status`).
///
/// This is the same normalized-argument, deadline, and warm-up-retry path the
/// `tracedecay tool` fallback uses, so first-class commands cannot drift from
/// the typed surface's transport behavior.
#[hotpath::measure(label = "cli.tool.catalog", future = true)]
pub(crate) async fn dispatch_catalogued_cli_operation(
    operation: ApplicationSurfaceOperation,
    tool_args: Value,
    project: Option<PathBuf>,
    raw_json: bool,
) -> Result<()> {
    let deadline = Instant::now()
        .checked_add(tool_command_deadline()?)
        .ok_or_else(tool_deadline_range_error)?;
    let (request, requested_format) =
        cli_surface_invocation(operation.mcp_tool_name(), tool_args, raw_json).map_err(
            |error| TraceDecayError::Config {
                message: error.to_string(),
            },
        )?;
    dispatch_cli_application_surface(operation, request, project, requested_format, deadline).await
}

/// Splits a CLI `--args` object into the reviewed application request body and
/// the requested output format, through the same adapter every other transport
/// uses. `--json` and `format: "json"` are the same request for JSON output.
fn cli_surface_invocation(
    tool_name: &str,
    tool_args: Value,
    raw_json: bool,
) -> std::result::Result<(Value, RequestedOutputFormat), ApplicationSurfaceAdapterError> {
    let normalized = adapt_application_tool_request(tool_name, tool_args)?;
    let requested_format = if raw_json {
        RequestedOutputFormat::Json
    } else {
        normalized.requested_format
    };
    Ok((normalized.request, requested_format))
}

/// Every application-surface operation is project-scoped on the daemon side
/// (`DaemonInvocationRequest::requires_project`), so `project` must already be
/// the resolved project route, not just an explicit `--project`. A handshake
/// without a project reaches the profile-scoped projectless route, where those
/// operations can only answer `application.surface.unavailable` /
/// `not_found_or_not_authorized`.
#[hotpath::measure(label = "cli.tool.application", future = true)]
async fn dispatch_cli_application_surface(
    operation: ApplicationSurfaceOperation,
    tool_args: Value,
    project: Option<PathBuf>,
    requested_format: RequestedOutputFormat,
    deadline: Instant,
) -> Result<()> {
    dispatch_cli_application_surface_inner(
        operation,
        tool_args,
        project,
        requested_format,
        deadline,
    )
    .await
}

fn dispatch_cli_application_surface_inner(
    operation: ApplicationSurfaceOperation,
    tool_args: Value,
    project: Option<PathBuf>,
    requested_format: RequestedOutputFormat,
    deadline: Instant,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'static>> {
    // Erase the deeply nested application-surface future before it reaches
    // the measured wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        #[cfg(feature = "hotpath")]
        hotpath::val!("cli.application.operation").set(&operation.as_str());
        let request_id = mint_global_request_id(GlobalRequestSurface::Cli).map_err(|_| {
            TraceDecayError::Config {
                message: "could not allocate an application surface request id".to_owned(),
            }
        })?;
        let request = match parse_application_surface_request(operation, tool_args.clone()) {
            Ok(request) => request,
            Err(error) => {
                if let Ok(handshake) = crate::commands::client_handshake(project.as_deref())
                    && let Ok(client) =
                        tracedecay_daemon_identity::invocation_client_for_current(handshake)
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
        let handshake =
            tracedecay::daemon::handshake_for_current_client(project.clone(), None, false, false)?;
        let client = tracedecay_daemon_identity::invocation_client_for_current(handshake)?;
        // A cold daemon answers the mounting refusal while the project open
        // still warms in the background. The compatibility tool path rides
        // that state out through its project-open retry loop; the typed
        // surface path re-sends only that same refusal, until the CLI
        // deadline. Every other completed problem is the answer.
        let mut next_request = Some(request);
        let result = loop {
            let request = match next_request.take() {
                Some(request) => request,
                None => parse_application_surface_request(operation, tool_args.clone()).map_err(
                    |error| TraceDecayError::Config {
                        message: error.to_string(),
                    },
                )?,
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
            .map_err(|error| match error {
                // The same typed connect failure the compatibility tool path
                // returns: one restart grace, then fail fast, never another
                // dispatch attempt against a dead socket.
                ApplicationSurfaceAdapterError::DaemonUnreachable {
                    reason_code,
                    detail,
                } => TraceDecayError::project_route(reason_code, true, detail),
                error => TraceDecayError::Config {
                    message: error.to_string(),
                },
            })?;
            let Some(delay) = crate::cli::dispatch::surface_retry_delay(&result) else {
                break result;
            };
            if deadline.saturating_duration_since(Instant::now()) <= delay {
                break result;
            }
            tokio::time::sleep(delay).await;
        };
        print_cli_application_surface(
            project.as_deref(),
            result,
            requested_format == RequestedOutputFormat::Json,
        )
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
#[hotpath::measure(label = "cli.tool.retained", future = true)]
async fn dispatch_cli_retained(
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
    let client = tracedecay_daemon_identity::invocation_client_for_current(dispatch.handshake()?)?;
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
    let response_handle_root = cli_response_handle_root(dispatch.project_path.as_deref())?;
    let mut result = tracedecay::mcp::tools::render_retained_execution(
        response_handle_root.as_deref(),
        &execution,
    )?;
    account_tool_result(dispatch.project_path.as_deref(), &mut result);
    tracedecay_mcp::tool_errors::mark_semantic_tool_error(&mut result);
    print_tool_output(&result.value, raw_json);
    tool_result_process_outcome(&result.value, tool_name)
}

/// Run one source-edit tool through the application surface and print the
/// same tool result its MCP call returns.
#[hotpath::measure(label = "cli.tool.source_edit", future = true)]
async fn dispatch_cli_source_edit(
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
    let handshake =
        tracedecay::daemon::handshake_for_current_client(project.clone(), None, false, false)?;
    let client = tracedecay_daemon_identity::invocation_client_for_current(handshake)?;
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
            .and_then(tracedecay_contracts::ApplicationProblemRecord::owner_mount_resend_delay)
        else {
            break outcome;
        };
        if deadline.saturating_duration_since(Instant::now()) <= delay {
            break outcome;
        }
        tokio::time::sleep(delay).await;
    };
    let response_handle_root = cli_response_handle_root(project.as_deref())?;
    let mut result = tracedecay_mcp::handlers::edit::render_source_edit_outcome(
        response_handle_root.as_deref(),
        operation,
        &tool_args,
        outcome,
    )?;
    account_tool_result(project.as_deref(), &mut result);
    tracedecay_mcp::tool_errors::mark_semantic_tool_error(&mut result);
    print_tool_output(&result.value, raw_json);
    tool_result_process_outcome(&result.value, tool_name)
}

/// Run one graph or port read through its project owner and print the same
/// tool result its MCP call returns.
#[hotpath::measure(label = "cli.tool.graph_tool", future = true)]
async fn dispatch_cli_graph_tool(
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
    let handshake =
        tracedecay::daemon::handshake_for_current_client(project.clone(), None, false, false)?;
    let client = tracedecay_daemon_identity::invocation_client_for_current(handshake)?;
    // A cold daemon refuses with the mounting problem while the project open
    // warms; that refusal precedes admission, so it is re-sent until the CLI
    // deadline like every other surface.
    let completion = loop {
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
        .await;
        let mounting = outcome.as_ref().err().is_some_and(|error| {
            error.project_route_context().is_some_and(|(code, _, _)| {
                code == tracedecay_contracts::RUNTIME_MOUNTING_REASON_CODE
            })
        });
        if !mounting
            || deadline.saturating_duration_since(Instant::now()) <= OWNER_MOUNT_RESEND_DELAY
        {
            break outcome?;
        }
        tokio::time::sleep(OWNER_MOUNT_RESEND_DELAY).await;
    };
    let response_handle_root = cli_response_handle_root(project.as_deref())?;
    let mut result = tracedecay_mcp::handlers::graph_tool::render_graph_tool(
        response_handle_root.as_deref(),
        &tool_args,
        completion,
    )?;
    account_tool_result(project.as_deref(), &mut result);
    tracedecay_mcp::tool_errors::mark_semantic_tool_error(&mut result);
    print_tool_output(&result.value, raw_json);
    tool_result_process_outcome(&result.value, tool_name)
}

/// Enrolled project's handle root, or none when that path has no store.
fn cli_response_handle_root(project: Option<&Path>) -> Result<Option<PathBuf>> {
    let Some(project) = project else {
        return Ok(None);
    };
    Ok(resolve_enrolled_layout_for_current_profile(project)?
        .map(|layout| layout.response_handle_root))
}

const OWNER_MOUNT_RESEND_DELAY: Duration = Duration::from_millis(250);

/// Prints one settled application-surface call through the renderer its MCP
/// call uses. `--json` keeps the whole canonical envelope on stdout; the
/// beside-result trailer and footer go to stderr on every format.
fn print_cli_application_surface(
    project: Option<&Path>,
    result: ApplicationSurfaceInvocationResult,
    raw_json: bool,
) -> Result<()> {
    let application_problem = result
        .result
        .as_ref()
        .err()
        .map(|problem| format!("{}: {}", problem.problem.code, problem.problem.message));
    let response_handle_root = cli_response_handle_root(project)?;
    let mut rendered = tracedecay::mcp::tools::render_application_surface_result(
        response_handle_root.as_deref(),
        &result,
    )?;
    account_tool_result(project, &mut rendered);
    if raw_json {
        print!("{}", crate::cli::output::json::json_line(&result.result)?);
        print_beside_result_blocks(&rendered.value);
    } else {
        print_tool_output(&rendered.value, false);
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
    /// `tool_args` may be seeded: a registry read that names an uninitialised
    /// `--project` but no `path` inherits that project as its `path`.
    fn for_tool(explicit_project: Option<String>, tool_name: &str, tool_args: &mut Value) -> Self {
        // Profile-targeted calls (Hermes user LCM/memory) must never invent a
        // project from cwd. Hermes intentionally runs those calls with cwd=/ so
        // Hermes home is never mistaken for a TraceDecay project.
        if targets_profile(tool_name, tool_args) {
            return Self {
                project_path: None,
                allow_init: false,
            };
        }
        if PROFILE_REGISTRY_TOOLS.contains(&tool_name) {
            return Self::registry_scoped(explicit_project, tool_name, tool_args);
        }
        Self::project_scoped(explicit_project, tool_name)
    }

    /// Registry reads never initialise anything, and follow the canonical
    /// resolution order (`tracedecay_runtime_core::config::discover_project_root`):
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
        explicit_project: Option<String>,
        tool_name: &str,
        tool_args: &mut Value,
    ) -> Self {
        let project_path = match explicit_project {
            Some(path) => {
                let explicit = tracedecay_configuration::resolve_path(Some(path));
                if explicit.is_dir()
                    && !tracedecay_runtime_core::config::is_initialized_project_root(&explicit)
                {
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
                .and_then(|cwd| implicit_tool_project_path(&cwd)),
        };
        Self {
            project_path,
            allow_init: false,
        }
    }

    fn project_scoped(explicit_project: Option<String>, tool_name: &str) -> Self {
        // An explicit --project wins. Otherwise only route to the nearest
        // initialised ancestor. Keeping an unscoped invocation projectless is
        // important: falling back to cwd can turn a broad directory such as
        // the user profile into an accidental project handshake.
        let explicitly_targeted = explicit_project.is_some();
        let project_path = match explicit_project {
            Some(path) => Some(tracedecay_configuration::resolve_path(Some(path))),
            None => std::env::current_dir()
                .ok()
                .and_then(|cwd| implicit_tool_project_path(&cwd)),
        };
        let allow_init = explicitly_targeted && FIRST_TOUCH_STORE_TOOLS.contains(&tool_name);

        Self {
            project_path,
            allow_init,
        }
    }

    fn handshake(&self) -> Result<DaemonHandshake> {
        tracedecay::daemon::handshake_for_current_client(
            self.project_path.clone(),
            None,
            false,
            self.allow_init,
        )
    }

    /// `deadline` is the caller's request deadline. It is sent to the daemon and
    /// enforced there; the transport reads for a bounded grace beyond it.
    #[hotpath::skip]
    async fn call(&self, tool_name: &str, tool_args: Value, deadline: Instant) -> Result<Value> {
        let handshake = self.handshake()?;
        // The interactive CLI wants the tool's answer, not the daemon's typed
        // warming state: ride out a cold project open until the CLI deadline,
        // the same transport behavior as the typed application-surface path.
        let result =
            call_default_tool_awaiting_project_open(&handshake, tool_name, tool_args, deadline)
                .await?;
        recover_truncated_mcp_result(&handshake, tool_name, result, Some(deadline)).await
    }
}

/// Whether a retained call addresses the authenticated profile's own stores.
fn targets_profile(tool_name: &str, tool_args: &Value) -> bool {
    RetainedSurfaceOperation::from_tool_name(tool_name).is_some_and(|operation| {
        tracedecay::mcp::tools::retained_tool_target(operation, tool_args)
            .is_ok_and(|target| target == tracedecay_contracts::InvocationTarget::Profile)
    })
}

fn implicit_tool_project_path(cwd: &Path) -> Option<PathBuf> {
    tracedecay_runtime_core::config::discover_project_root(cwd)
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

fn map_tool_deadline_error(tool_name: &str, error: TraceDecayError) -> TraceDecayError {
    if tracedecay::daemon::error_is_read_deadline(&error) {
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
#[hotpath::measure(label = "cli.tool.compatibility", future = true)]
async fn dispatch_compatibility_tool(
    dispatch: DaemonToolDispatch,
    tool_name: &str,
    tool_args: Value,
    raw_json: bool,
    deadline: Instant,
) -> Result<()> {
    #[cfg(feature = "hotpath")]
    hotpath::val!("cli.compatibility_tool.name").set(&tool_name);
    // `deadline` is the caller's *request* deadline: it now travels to the
    // daemon, which enforces it. The local wait exists only to bound a dead or
    // wedged daemon, so it runs on the transport's response bound, that same
    // deadline plus a bounded grace. Waiting strictly to the request deadline
    // made every deadline-elapsed typed terminal unobservable through this
    // transport: the daemon's PartialEffect (committed receipt, Reconcile-only
    // legal action) or typed timeout envelope arrived moments after the local
    // abort had already printed "outcome may be unknown", untruthful, since
    // the outcome was in flight. Never discard an envelope that was received.
    let response_bound = tracedecay::daemon::daemon_tool_response_bound(deadline)?;
    let result_value = match timeout_at(
        response_bound,
        dispatch.call(tool_name, tool_args, deadline),
    )
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => return Err(map_tool_deadline_error(tool_name, error)),
        Err(_) => return Err(tool_timeout_error(tool_name)),
    };
    reject_tool_result_truncation(&result_value, tool_name)?;
    print_tool_output(&result_value, raw_json);
    // The payload above is the tool's answer and callers parse it, so it is
    // printed byte-for-byte either way; only the process status changes here.
    // A tool result the daemon classified as an application failure must not
    // exit 0, that made every script and CI gate shelling out to
    // `tracedecay tool` silently blind to a failing tool.
    tool_result_process_outcome(&result_value, tool_name)
}

/// The process outcome for a completed MCP tool result: `Ok` (exit 0) for a
/// successful call, `Err` (nonzero exit) for one the daemon classified as an
/// application failure.
///
/// `isError` is the daemon's own authoritative classification, set by
/// `mark_semantic_tool_error` from either a handler's structural
/// `with_semantic_error` marker or the rendered-payload failure heuristic, and
/// is the same field an MCP client reads, so the CLI and MCP transports agree
/// on what "this tool failed" means.
///
/// A *degraded but truthful* answer is deliberately not a failure: a partial
/// coverage report, a warming generation, or an `unavailable` retrieval lane
/// described inside an otherwise successful payload never carries `isError`, so
/// it keeps exit 0. Only an outcome the daemon itself marked as failed changes
/// the status, which mirrors what the typed application-surface path already
/// does in [`print_cli_application_surface`].
fn tool_result_process_outcome(result_value: &Value, tool_name: &str) -> Result<()> {
    if result_value.get("isError").and_then(Value::as_bool) != Some(true) {
        return Ok(());
    }
    // `print_tool_output` already wrote the exact daemon payload. Flush before
    // returning the status-only error so the process boundary can drop its
    // profiling guard and then return the nonzero `ExitCode`.
    std::io::stdout().flush()?;
    Err(TraceDecayError::Config {
        message: format!("{tool_name} reported an application failure."),
    })
}

fn print_tool_output(result_value: &Value, raw_json: bool) {
    println!("{}", rendered_tool_output(result_value, raw_json));
    if !raw_json {
        print_beside_result_blocks(result_value);
    }
}

fn print_beside_result_blocks(result_value: &Value) {
    for block in beside_result_blocks(result_value) {
        eprintln!("{block}");
    }
}

/// The bytes `tracedecay tool` writes to stdout for a completed compatibility
/// result: the exact daemon JSON object when `--json` is set, otherwise the
/// joined `content[*].text` markdown. Status is decided separately from
/// top-level `isError`.
fn rendered_tool_output(result_value: &Value, raw_json: bool) -> String {
    if raw_json {
        serde_json::to_string_pretty(result_value).unwrap_or_default()
    } else {
        join_content_text(result_value)
    }
}

/// Joins every payload `content[*].text` block in an MCP tool result,
/// separated by a blank line. Handlers sometimes prepend a warning/notice block
/// ahead of the real payload; printing only `content[0].text` would silently
/// drop the payload. The beside-result stale-graph trailer and token-accounting
/// footer blocks are excluded (see [`beside_result_blocks`]): with
/// `--format json` the payload block is the whole stdout document and a
/// trailing block would make it unparseable. Falls back to the empty string
/// when no text blocks exist.
fn join_content_text(result_value: &Value) -> String {
    content_text_blocks(result_value)
        .filter(|text| !is_beside_result_block(text))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The stale-graph trailer and token-accounting footer blocks, printed to
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
        || n == "tracedecay_by_qualified_name"
        || n == "tracedecay_signature"
        || n == "tracedecay_derives"
        || n == "tracedecay_similar"
        || n == "tracedecay_rename_preview"
        || n == "tracedecay_find_exact_symbol"
    {
        "graph"
    } else if n == "tracedecay_run_affected_tests" {
        "workflow"
    } else if n == "tracedecay_dead_code"
        || n == "tracedecay_unmounted_files"
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
