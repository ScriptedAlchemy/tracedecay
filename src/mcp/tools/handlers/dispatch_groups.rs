use serde_json::Value;
use tracedecay_application::{
    ApplicationProblem, ApplicationProblemEnvelope, ResultContractRef, SafeDiagnostic,
};
use tracedecay_tool_catalog::BindingSurface;

use crate::application_surface::{ApplicationSurfaceOperation, resolve_catalog_tool_binding};
use crate::daemon_client::InvocationCancellationPolicy;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDbLeaseV1;
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;

use super::ToolCallRegistryOptions;
use super::tool_call_support::handle_retrieve;
use super::unknown_tool_error;
use super::{
    admin_cli, admin_project, analysis, application_surface, ast_grep_search, automation_runs,
    dashboard, dispatch_controls, edit, git, graph, grep, hook_runtime, info, skills, workflow,
};

mod health_dispatch;
mod retained_response;
pub(super) use health_dispatch::dispatch_health_tools;

fn graph_read_unavailable(detail: &str) -> TraceDecayError {
    TraceDecayError::ProjectRoute {
        reason_code: "verified-code-graph-read-unavailable".to_owned(),
        retryable: false,
        detail: detail.to_owned(),
    }
}

fn retained_contract_error(
    context: &'static str,
    error: tracedecay_application::ApplicationContractError,
) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("{context}: {error}"),
    }
}

fn retained_safe_diagnostic(code: &'static str, message: &'static str) -> Result<SafeDiagnostic> {
    SafeDiagnostic::new(code, message)
        .map_err(|error| retained_contract_error("invalid retained application diagnostic", error))
}

fn retained_problem_envelope(
    contract: ResultContractRef,
    request_id: tracedecay_application::RequestId,
    problem: ApplicationProblem,
) -> Result<ApplicationProblemEnvelope> {
    ApplicationProblemEnvelope::new(contract, request_id, problem).map_err(|error| {
        retained_contract_error("invalid retained application problem envelope", error)
    })
}

async fn admitted_graph_query(
    cg: &TraceDecay,
    options: &ToolCallRegistryOptions<'_>,
    operation_name: &str,
) -> Result<crate::tracedecay::queries::graph::VerifiedGraphQuery> {
    let projection = options
        .code_graph_projection_read_port
        .as_deref()
        .ok_or_else(|| {
            graph_read_unavailable("the exact project graph projection is not mounted")
        })?;
    let admission = options
        .code_graph_read_admission_port
        .as_deref()
        .ok_or_else(|| {
            graph_read_unavailable("the exact project graph admission is not mounted")
        })?;
    let request_id = options
        .application_request_id
        .clone()
        .ok_or_else(|| graph_read_unavailable("the caller request identity is unavailable"))?;
    let deadline = options
        .application_deadline
        .clone()
        .ok_or_else(|| graph_read_unavailable("the caller deadline is unavailable"))?;
    let cancellation = options
        .application_cancellation
        .as_ref()
        .ok_or_else(|| graph_read_unavailable("the caller cancellation signal is unavailable"))?;
    let operation =
        tracedecay_application::retrieval::catalog::primitive_read_operation(operation_name)
            .map_err(|error| TraceDecayError::Config {
                message: format!("invalid graph read operation: {error}"),
            })?
            .ok_or_else(|| TraceDecayError::Config {
                message: format!("unregistered graph read operation: {operation_name}"),
            })?;
    cg.open_verified_graph_query(
        projection,
        admission,
        &operation,
        request_id,
        deadline,
        cancellation,
    )
    .await
}

/// The hard ceiling every MCP tool call is bounded by, regardless of dispatch
/// group, when admission carried no client deadline.
///
/// Principle 6 of `docs/SERVING-PATH-PERFORMANCE.md`: deadlines bound failure,
/// not work. Before this existed only the git and memory groups were wrapped,
/// so `dispatch_deadline_horizon_micros` returning `None` for a graph tool meant
/// `tracedecay_context` dispatched with no bound at all — a live Codex call once
/// hung for 900 seconds against a daemon grinding a failing publish loop, and
/// only the client's own timeout ended it. A firing ceiling is always a bug
/// somewhere above it; the fix is that bug, never a larger ceiling.
pub(crate) const TOOL_DISPATCH_CEILING: std::time::Duration = std::time::Duration::from_mins(2);

/// The ceiling for the few tools whose *requested work* is itself a long job —
/// running a test suite, an admin index/sync — rather than an interactive read.
///
/// These are still bounded: nothing may run unbounded, and nothing may reach the
/// 900 seconds that motivated this wrap. They simply cannot share the
/// interactive ceiling without failing correct, user-requested work.
pub(crate) const LONG_RUNNING_TOOL_DISPATCH_CEILING: std::time::Duration =
    std::time::Duration::from_mins(10);

/// Tools whose ceiling is [`LONG_RUNNING_TOOL_DISPATCH_CEILING`].
///
/// Deliberately tiny and explicit: membership is a statement that the tool's
/// duration is the caller's own job, not a serving-path stall. Everything not
/// listed here — every graph, info, analysis, health, session, and memory read —
/// inherits [`TOOL_DISPATCH_CEILING`] automatically, so a tool added tomorrow is
/// bounded without touching this file.
const LONG_RUNNING_DISPATCH_TOOLS: &[&str] = &[
    "tracedecay_run_affected_tests",
    "tracedecay_fact_store_curate",
    "tracedecay_admin_cli",
    "tracedecay_admin_project",
    "tracedecay_admin_sync",
    "tracedecay_admin_branch_add",
];

/// The ceiling that applies to `tool_name` in the absence of a shorter carried
/// deadline.
pub(crate) fn tool_dispatch_ceiling(tool_name: &str) -> std::time::Duration {
    if LONG_RUNNING_DISPATCH_TOOLS.contains(&tool_name) {
        LONG_RUNNING_TOOL_DISPATCH_CEILING
    } else {
        TOOL_DISPATCH_CEILING
    }
}

/// The bound one tool call dispatches under: the admission-carried client
/// deadline when it is present and shorter, otherwise the tool's own ceiling.
///
/// `None` means the carried deadline has already elapsed, which must be
/// rejected rather than dispatched — the same rule the git and memory wraps
/// apply to a non-positive budget.
pub(crate) fn tool_dispatch_budget(
    tool_name: &str,
    deadline: Option<&tracedecay_application::Deadline>,
) -> Option<std::time::Duration> {
    let ceiling = tool_dispatch_ceiling(tool_name);
    match deadline {
        // A carried deadline is preferred whenever it is shorter; the ceiling
        // still clamps a pathologically distant one so it can never be a way
        // out of the bound.
        Some(deadline) => crate::daemon_client::deadline_remaining(deadline)
            .map(|remaining| remaining.min(ceiling)),
        None => Some(ceiling),
    }
}

/// The typed, retryable problem a tool call reports when it exhausts the
/// universal dispatch ceiling.
///
/// Its stable `reason_code`, retryability bit, and human detail let the MCP
/// boundary surface a structured error instead of holding the transport open.
/// Retry is safe: the ceiling is a
/// backstop over work that was already admitted, never a commit signal.
pub(crate) fn tool_dispatch_deadline_error(
    tool_name: &str,
    budget: std::time::Duration,
) -> TraceDecayError {
    TraceDecayError::project_route(
        "tool_dispatch_deadline_exceeded",
        true,
        format!(
            "tool '{tool_name}' exceeded its {}s dispatch ceiling and was cancelled",
            budget.as_secs()
        ),
    )
}

/// Dispatch code-graph navigation and lookup tools (`tracedecay_search`,
/// `tracedecay_callers`, ...). Returns `None` when `tool_name` belongs to a
/// different domain so the caller can try the next dispatch group.
pub(super) async fn dispatch_graph_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    selected_scope_prefix: Option<&str>,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_search" => {
            graph::handle_search(
                cg,
                admitted_graph_query(cg, &options, "code_symbol_search"),
                args,
                selected_scope_prefix,
                options.code_index_search_executor.as_ref(),
                options.code_index_search_authority.as_ref(),
                options.code_index_ignored_dependency_admission.as_deref(),
                options.application_deadline.clone(),
                options.application_cancellation.clone(),
            )
            .await
        }
        "tracedecay_grep" => {
            grep::handle_grep(
                cg,
                args,
                selected_scope_prefix,
                options.application_deadline.clone(),
                options.application_cancellation.clone(),
            )
            .await
        }
        "tracedecay_ast_grep_search" => {
            ast_grep_search::handle_ast_grep_search(
                cg,
                args,
                selected_scope_prefix,
                options.application_deadline.clone(),
                options.application_cancellation.clone(),
            )
            .await
        }
        "tracedecay_retrieve" => handle_retrieve(cg, &args),
        "tracedecay_context" => {
            graph::handle_context(
                cg,
                admitted_graph_query(cg, &options, "context"),
                args,
                selected_scope_prefix,
                options.code_index_search_executor.as_ref(),
                options.code_index_search_authority.as_ref(),
                options.application_deadline.clone(),
                options.application_cancellation.clone(),
            )
            .await
        }
        "tracedecay_callers" => {
            let graph_query = admitted_graph_query(cg, &options, "code_callers").await?;
            graph::handle_callers(cg, &graph_query, args).await
        }
        "tracedecay_callees" => {
            let graph_query = admitted_graph_query(cg, &options, "callees").await?;
            graph::handle_callees(cg, &graph_query, args).await
        }
        "tracedecay_impact" => {
            let graph_query = admitted_graph_query(cg, &options, "impact").await?;
            graph::handle_impact(cg, &graph_query, args).await
        }
        "tracedecay_node" => {
            let graph_query = admitted_graph_query(cg, &options, "node").await?;
            graph::handle_node(cg, &graph_query, args).await
        }
        "tracedecay_similar" => {
            let graph_query = admitted_graph_query(cg, &options, "similar").await?;
            graph::handle_similar(
                cg,
                &graph_query,
                args,
                options.code_index_search_executor.as_ref(),
                options.code_index_search_authority.as_ref(),
                options.application_deadline.clone(),
                options.application_cancellation.clone(),
            )
            .await
        }
        "tracedecay_rename_preview" => {
            let graph_query = admitted_graph_query(cg, &options, "rename_preview").await?;
            graph::handle_rename_preview(cg, &graph_query, args).await
        }
        "tracedecay_implementations" => {
            let graph_query = admitted_graph_query(cg, &options, "code_implementations").await?;
            graph::handle_implementations(cg, &graph_query, args, selected_scope_prefix).await
        }
        "tracedecay_callers_for" => {
            let graph_query = admitted_graph_query(cg, &options, "code_callers").await?;
            graph::handle_callers_for(cg, &graph_query, args).await
        }
        "tracedecay_find_exact_symbol" => {
            let graph_query = admitted_graph_query(cg, &options, "qualified_name").await?;
            graph::handle_find_exact_symbol(
                cg,
                &graph_query,
                args,
                selected_scope_prefix,
                options.code_index_ignored_dependency_admission.as_deref(),
                options.application_deadline.as_ref(),
                options.application_cancellation.as_ref(),
            )
            .await
        }
        "tracedecay_by_qualified_name" => {
            let graph_query = admitted_graph_query(cg, &options, "qualified_name").await?;
            graph::handle_by_qualified_name(cg, &graph_query, args).await
        }
        "tracedecay_signature" => {
            let graph_query = admitted_graph_query(cg, &options, "code_signature_search").await?;
            graph::handle_signature(cg, &graph_query, args).await
        }
        "tracedecay_impls" => {
            let graph_query = admitted_graph_query(cg, &options, "code_implementations").await?;
            graph::handle_impls(cg, &graph_query, args).await
        }
        "tracedecay_derives" => {
            let graph_query = admitted_graph_query(cg, &options, "code_type_hierarchy").await?;
            graph::handle_derives(cg, &graph_query, args).await
        }
        _ => Err(unknown_tool_error(tool_name)),
    }
}

/// Dispatch project-info, registry, and file-inspection tools
/// (`tracedecay_status`, `tracedecay_project_list`, `tracedecay_read`, ...).
#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_info_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    server_stats: Option<Value>,
    scope_prefix: Option<&str>,
    selected_scope_prefix: Option<&str>,
    active_project_session_db: Option<&RegisteredGlobalDbLeaseV1>,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_remote_status" => info::handle_remote_status(
            cg.project_root(),
            &args,
            options.remote_operational_status.as_ref(),
        ),
        "tracedecay_status" => {
            info::handle_status(
                cg,
                args,
                server_stats,
                scope_prefix,
                active_project_session_db.map(RegisteredGlobalDbLeaseV1::as_ref),
                options.code_index_freshness_reader.as_ref(),
                options.generation_census_reader.as_ref(),
            )
            .await
        }
        "tracedecay_active_project" => Ok(info::handle_active_project(
            cg,
            &args,
            server_stats,
            scope_prefix,
        )),
        "tracedecay_project_list" => {
            info::handle_project_list(cg, args, options.project_registry_reads).await
        }
        "tracedecay_project_search" => {
            info::handle_project_search(cg, args, options.project_registry_reads).await
        }
        "tracedecay_project_context" => {
            info::handle_project_context(cg, args, options.project_registry_reads).await
        }
        "tracedecay_files" => {
            let graph = admitted_graph_query(cg, &options, "file_metadata").await?;
            info::handle_files(cg, &graph, args, selected_scope_prefix).await
        }
        "tracedecay_admin_sync" => {
            info::handle_admin_sync(cg, args, options.code_index_reconcile_sink.as_ref()).await
        }
        "tracedecay_port_status" => {
            let graph = admitted_graph_query(cg, &options, "port_status").await?;
            info::handle_port_status(cg, &graph, args).await
        }
        "tracedecay_port_order" => {
            let graph = admitted_graph_query(cg, &options, "port_order").await?;
            info::handle_port_order(cg, &graph, args).await
        }
        "tracedecay_simplify_scan" => info::handle_simplify_scan(cg, args, scope_prefix).await,
        "tracedecay_type_hierarchy" => {
            let graph = admitted_graph_query(cg, &options, "code_type_hierarchy").await?;
            info::handle_type_hierarchy(cg, &graph, args).await
        }
        "tracedecay_body" => {
            let graph = admitted_graph_query(cg, &options, "source_body").await?;
            info::handle_body(cg, &graph, args, selected_scope_prefix).await
        }
        "tracedecay_todos" => {
            let graph = admitted_graph_query(cg, &options, "todos").await?;
            info::handle_todos(cg, &graph, args, scope_prefix).await
        }
        "tracedecay_read" => {
            let operation = match args.get("mode").and_then(Value::as_str).unwrap_or("full") {
                "map" => "source_outline",
                "signatures" => "code_signature_search",
                _ => "source_lines",
            };
            let graph = admitted_graph_query(cg, &options, operation).await?;
            info::handle_read(cg, &graph, args).await
        }
        "tracedecay_outline" => {
            let graph = admitted_graph_query(cg, &options, "source_outline").await?;
            info::handle_outline(cg, &graph, args).await
        }
        "tracedecay_config" => info::handle_config(cg, &args),
        "tracedecay_signature_search" => {
            let graph = admitted_graph_query(cg, &options, "code_signature_search").await?;
            info::handle_signature_search(cg, &graph, args, selected_scope_prefix).await
        }
        _ => Err(unknown_tool_error(tool_name)),
    }
}

/// Dispatch administrative tools (`tracedecay_hook_runtime`,
/// `tracedecay_admin_cli`, `tracedecay_admin_project`).
pub(super) async fn dispatch_admin_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_hook_runtime" => {
            hook_runtime::handle_hook_runtime(
                cg,
                args,
                options.global_db.map(RegisteredGlobalDbLeaseV1::as_ref),
                options.accounting_db,
                options.session_authorities,
            )
            .await
        }
        "tracedecay_admin_cli" => {
            admin_cli::handle_admin_cli(
                cg,
                args,
                options.global_db,
                options.accounting_db,
                options.profile_root,
                options.session_authorities,
                options.session_sync_service,
                options.application_request_id.clone(),
                options.application_deadline.clone(),
                options.application_cancellation.clone(),
            )
            .await
        }
        "tracedecay_admin_project" => {
            let deadline =
                options
                    .application_deadline
                    .clone()
                    .ok_or_else(|| TraceDecayError::Config {
                        message: "admin project request deadline is unavailable".to_owned(),
                    })?;
            let cancellation = options.application_cancellation.clone().ok_or_else(|| {
                TraceDecayError::Config {
                    message: "admin project cancellation authority is unavailable".to_owned(),
                }
            })?;
            admin_project::handle_admin_project(
                cg,
                args,
                options.global_db.map(RegisteredGlobalDbLeaseV1::as_ref),
                options.automation_scheduler_reconciler,
                deadline,
                cancellation,
            )
            .await
        }
        _ => Err(unknown_tool_error(tool_name)),
    }
}

/// Dispatch catalog-owned application surfaces.
pub(super) async fn dispatch_application_surface_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    let Some(operation) = ApplicationSurfaceOperation::from_tool_name(tool_name) else {
        return Err(unknown_tool_error(tool_name));
    };
    let normalized_args =
        match crate::application_surface::normalize_application_tool_args(tool_name, args) {
            Ok(args) => args,
            Err(error) => {
                return Err(TraceDecayError::Config {
                    message: error.to_string(),
                });
            }
        };
    application_surface::handle_application_surface(
        cg,
        operation,
        normalized_args,
        options.application_invocation_executor,
        options.application_invocation_target,
        options.application_request_id.clone(),
        options.application_deadline.clone(),
        options.application_cancellation.clone(),
    )
    .await
}

/// Dispatch static-analysis report tools (`tracedecay_dead_code`,
/// `tracedecay_complexity`, `tracedecay_diagnostics`, ...).
pub(super) async fn dispatch_analysis_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
    active_project_session_db: Option<&RegisteredGlobalDbLeaseV1>,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_dead_code" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            analysis::handle_dead_code(cg, &graph, args, scope_prefix).await
        }
        "tracedecay_circular" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            analysis::handle_circular(cg, &graph, args).await
        }
        "tracedecay_hotspots" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            analysis::handle_hotspots(cg, &graph, args, scope_prefix).await
        }
        "tracedecay_unused_imports" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            analysis::handle_unused_imports(cg, &graph, args, scope_prefix).await
        }
        // The one analysis tool that opens no graph query: its whole finding is
        // that the graph and the compiler disagree, so taking the graph's file
        // set as input would answer the question with the very source that is
        // under suspicion.
        "tracedecay_unmounted_files" => {
            analysis::handle_unmounted_files(cg, args, scope_prefix).await
        }
        "tracedecay_rank" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            analysis::handle_rank(cg, &graph, args, scope_prefix).await
        }
        "tracedecay_largest" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            analysis::handle_largest(cg, &graph, args, scope_prefix).await
        }
        "tracedecay_coupling" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            analysis::handle_coupling(cg, &graph, args, scope_prefix).await
        }
        "tracedecay_inheritance_depth" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            analysis::handle_inheritance_depth(cg, &graph, args, scope_prefix).await
        }
        "tracedecay_distribution" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            analysis::handle_distribution(cg, &graph, args, scope_prefix).await
        }
        "tracedecay_recursion" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            analysis::handle_recursion(cg, &graph, args, scope_prefix).await
        }
        "tracedecay_complexity" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            analysis::handle_complexity(cg, &graph, args, scope_prefix).await
        }
        "tracedecay_doc_coverage" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            analysis::handle_doc_coverage(cg, &graph, args, scope_prefix).await
        }
        "tracedecay_god_class" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            analysis::handle_god_class(cg, &graph, args, scope_prefix).await
        }
        "tracedecay_unsafe_patterns" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            analysis::handle_unsafe_patterns(cg, &graph, args, scope_prefix).await
        }
        "tracedecay_constructors" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            analysis::handle_constructors(cg, &graph, args, scope_prefix).await
        }
        "tracedecay_field_sites" => {
            let graph = admitted_graph_query(cg, &options, "health_read").await?;
            analysis::handle_field_sites(cg, &graph, args, scope_prefix).await
        }
        "tracedecay_diagnostics" => {
            let graph = admitted_graph_query(cg, &options, "diagnostics_read").await?;
            analysis::handle_diagnostics(
                cg,
                &graph,
                args,
                options.diagnostics_cache,
                options.diagnostics_lsp.as_deref(),
                active_project_session_db.map(RegisteredGlobalDbLeaseV1::as_ref),
            )
            .await
        }
        _ => Err(unknown_tool_error(tool_name)),
    }
}

/// Dispatch git-aware tools (`tracedecay_affected`, `tracedecay_changelog`,
/// branch and PR context helpers).
pub(super) async fn dispatch_git_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    // Tree walks and revwalks still need a uniform dispatch deadline. Branch
    // generation reads additionally carry this deadline into their bounded
    // blocking/ref and daemon-generation executors, so timing out this future
    // also tells the underlying operation to stop at its next checkpoint.
    let carried_deadline = options.application_deadline.as_ref();
    let remaining = carried_deadline.and_then(crate::daemon_client::deadline_remaining);

    let handler = async {
        match tool_name {
            "tracedecay_affected" => {
                let graph = admitted_graph_query(cg, &options, "file_dependents").await?;
                git::handle_affected(cg, &graph, args).await
            }
            "tracedecay_diff_context" => {
                let graph = admitted_graph_query(cg, &options, "file_dependents").await?;
                git::handle_diff_context(cg, &graph, args).await
            }
            "tracedecay_changelog" => {
                let graph = admitted_graph_query(cg, &options, "file_dependents").await?;
                git::handle_changelog(cg, &graph, args).await
            }
            "tracedecay_commit_context" => {
                let graph = admitted_graph_query(cg, &options, "file_dependents").await?;
                git::handle_commit_context(cg, &graph, args).await
            }
            "tracedecay_pr_context" => {
                let deadline = options.application_deadline.clone();
                let cancellation = options.application_cancellation.clone();
                let registered_project_session_db = options.registered_project_session_db.clone();
                git::handle_pr_context(
                    cg,
                    admitted_graph_query(cg, &options, "file_dependents"),
                    args,
                    deadline,
                    cancellation,
                    registered_project_session_db,
                )
                .await
            }
            "tracedecay_branch_search" => {
                git::handle_branch_search(
                    cg,
                    args,
                    options.code_index_search_executor.as_ref(),
                    options.code_index_search_authority.as_ref(),
                    options.application_deadline.clone(),
                    options.application_cancellation.clone(),
                )
                .await
            }
            "tracedecay_branch_diff" => {
                git::handle_branch_diff(
                    cg,
                    args,
                    options.code_index_branch_diff_executor.as_ref(),
                    options.code_index_search_authority.as_ref(),
                    options.application_deadline.clone(),
                    options.application_cancellation.clone(),
                )
                .await
            }
            "tracedecay_branch_list" => {
                git::handle_branch_list(
                    cg,
                    args,
                    options.application_deadline.clone(),
                    options.application_cancellation.clone(),
                )
                .await
            }
            _ => Err(unknown_tool_error(tool_name)),
        }
    };

    match (carried_deadline.is_some(), remaining) {
        (_, Some(remaining)) => match tokio::time::timeout(remaining, handler).await {
            Ok(result) => result,
            Err(_elapsed) => Ok(git::git_dispatch_deadline_result(cg, tool_name)),
        },
        // `deadline_remaining` yields `None` for a non-positive budget, so a
        // carried deadline that already elapsed must be rejected rather than
        // dispatched unbounded.
        (true, None) => Ok(git::git_dispatch_deadline_result(cg, tool_name)),
        // Standalone / non-admission callers carry no deadline and stay
        // unbounded.
        (false, None) => handler.await,
    }
}

/// Dispatch source-editing tools (`tracedecay_str_replace`,
/// `tracedecay_move_symbol`, ...).
pub(super) async fn dispatch_edit_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    let invocation = edit::SourceEditInvocationContext {
        executor: options.source_edit_executor.clone(),
        reconciliation_executor: options.source_edit_reconciliation_executor.clone(),
        rollback_executor: options.source_edit_rollback_executor.clone(),
        request_id: options.application_request_id.clone(),
        deadline: options.application_deadline.clone(),
        cancellation: options.application_cancellation.clone(),
    };
    match tool_name {
        "tracedecay_str_replace" => edit::handle_str_replace(cg, args, invocation.clone()).await,
        "tracedecay_multi_str_replace" => {
            edit::handle_multi_str_replace(cg, args, invocation.clone()).await
        }
        "tracedecay_insert_at" => edit::handle_insert_at(cg, args, invocation.clone()).await,
        "tracedecay_ast_grep_rewrite" => {
            edit::handle_ast_grep_rewrite(cg, args, invocation.clone()).await
        }
        "tracedecay_replace_symbol" => {
            edit::handle_replace_symbol(cg, args, invocation.clone()).await
        }
        "tracedecay_insert_at_symbol" => {
            edit::handle_insert_at_symbol(cg, args, invocation.clone()).await
        }
        "tracedecay_move_symbol" => edit::handle_move_symbol(cg, args, invocation.clone()).await,
        "tracedecay_rename_symbol" => {
            edit::handle_rename_symbol(cg, args, invocation.clone()).await
        }
        "tracedecay_source_edit_rollback" => {
            edit::handle_source_edit_rollback(cg, args, invocation.clone()).await
        }
        "tracedecay_source_edit_reconcile" => {
            edit::handle_source_edit_reconcile(cg, args, invocation).await
        }
        _ => Err(unknown_tool_error(tool_name)),
    }
}

/// Dispatch retained memory, session, and workflow operations only after the
/// application-owned catalog has resolved their stable operation identity.
pub(super) async fn dispatch_retained_application_tools(
    tool_name: &str,
    cg: &TraceDecay,
    mut args: Value,
    _scope_prefix: Option<&str>,
    _active_project_session_db: Option<&RegisteredGlobalDbLeaseV1>,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    let retained_operation = super::retained_catalog::retained_mcp_operation(tool_name, &args)
        .ok_or_else(|| unknown_tool_error(tool_name))?;
    let canonical_tool_name = format!("tracedecay_{}", retained_operation.as_str());
    let binding = resolve_catalog_tool_binding(BindingSurface::Mcp, &canonical_tool_name)
        .map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?
        .ok_or_else(|| unknown_tool_error(tool_name))?;
    if tool_name == "tracedecay_session_refresh"
        && let Some(arguments) = args.as_object_mut()
    {
        arguments.remove("action");
    }
    let normalized = crate::application_surface::normalize_application_tool_args(tool_name, args)
        .map_err(|error| TraceDecayError::Config {
        message: error.to_string(),
    })?;
    let requested_format = normalized.requested_format;
    let request = crate::application_surface::retained::decode_request(
        retained_operation,
        normalized.request,
    )
    .ok_or_else(|| TraceDecayError::Config {
        message: format!("invalid retained application request for {tool_name}"),
    })?;
    if request.operation() != retained_operation {
        return Err(TraceDecayError::Config {
            message: format!("retained application request does not match {tool_name}"),
        });
    }
    let request_id = match options.application_request_id {
        Some(request_id) => request_id,
        None => application_surface::request_id()?,
    };
    let result_contract = ResultContractRef::from_schema(&binding.result_schema);
    let result = match options.application_invocation_executor {
        Some(executor) => {
            let (deadline, cancellation) =
                application_surface::complete_retained_protocol_controls(
                    retained_operation,
                    &request_id,
                    options.application_deadline,
                    options.application_cancellation,
                )?
                .ok_or_else(|| {
                    TraceDecayError::project_route(
                        "retained_application_controls_unavailable",
                        true,
                        "retained application protocol controls are unavailable",
                    )
                })?;
            let invocation = crate::daemon_contract::DaemonInvocationRequest::retained_application(
                request_id.as_str(),
                request,
                tracedecay_application::now_micros(),
                deadline.clone(),
                cancellation.context(),
            );
            let policy =
                if tracedecay_application::retained_surfaces::retained_surface_operation_is_effect(
                    retained_operation,
                ) {
                    InvocationCancellationPolicy::AuthoritativeEffect
                } else {
                    InvocationCancellationPolicy::ReadOnly
                };
            match executor
                .invoke_controlled(invocation, deadline, cancellation, policy)
                .await
            {
                Ok(response)
                    if response.protocol == crate::daemon_contract::DAEMON_INVOCATION_PROTOCOL
                        && response.revision
                            == crate::daemon_contract::DAEMON_INVOCATION_REVISION
                        && response.request_id == request_id.as_str() =>
                {
                    retained_response::validated_retained_response(
                        response.outcome,
                        retained_operation,
                        &request_id,
                        &result_contract,
                    )?
                }
                Ok(_) => Err(retained_problem_envelope(
                    result_contract.clone(),
                    request_id.clone(),
                    ApplicationProblem::unavailable(retained_safe_diagnostic(
                        "application.surface.invalid_response",
                        "The daemon returned an invalid retained application envelope",
                    )?),
                )?),
                Err(error) => Err(retained_problem_envelope(
                    result_contract.clone(),
                    request_id.clone(),
                    error.into_application_problem(),
                )?),
            }
        }
        None => Err(retained_problem_envelope(
            result_contract,
            request_id,
            ApplicationProblem::unavailable(retained_safe_diagnostic(
                "application.transport.unavailable",
                "The daemon retained application transport is unavailable",
            )?),
        )?),
    };
    application_surface::render_retained_result(
        Some(cg.project_root()),
        retained_operation,
        binding.binding_id,
        result,
        requested_format,
    )
}

/// Dispatch memory, skill, and analytics tools (`tracedecay_fact_store_add`,
/// `tracedecay_skill_list`, `tracedecay_analytics`, ...).
pub(super) async fn dispatch_memory_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_automation_run_list" => automation_runs::handle_list(cg, args).await,
        "tracedecay_automation_run_view" => automation_runs::handle_view(cg, args).await,
        "tracedecay_automation_run_artifact_view" => {
            skills::handle_automation_run_artifact_view(cg, args).await
        }
        "tracedecay_analytics" => dispatch_controls::dispatch_analytics(cg, args, options).await,
        "tracedecay_skill_list" => skills::handle_skill_list(cg, args, options.accounting_db).await,
        "tracedecay_skill_view" => skills::handle_skill_view(cg, args, options.accounting_db).await,
        "tracedecay_hermes_skill_bridge" => skills::handle_hermes_skill_bridge(cg, &args),
        _ => Err(unknown_tool_error(tool_name)),
    }
}

/// Dispatch dashboard and workflow tools that have not moved to a dedicated
/// application family.
pub(super) async fn dispatch_session_workflow_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_diagnose" => {
            let graph = admitted_graph_query(cg, &options, "diagnostics_read").await?;
            workflow::handle_diagnose(
                cg,
                &graph,
                args,
                options.code_index_publication_identity.as_deref(),
            )
            .await
        }
        "tracedecay_run_affected_tests" => {
            let graph = admitted_graph_query(cg, &options, "file_dependents").await?;
            workflow::handle_run_affected_tests(
                cg,
                &graph,
                args,
                options.application_cancellation.clone(),
                options.code_index_publication_identity.as_deref(),
            )
            .await
        }
        "tracedecay_dashboard" => {
            dashboard::handle_dashboard(
                cg,
                args,
                options.retained_project_server_resolver.clone(),
                options.code_graph_read_admission_port.clone(),
                options.code_graph_projection_read_port.clone(),
                options.registered_project_session_db.clone(),
                options.daemon_user_profile_id.clone(),
                options.profile_root.map(std::path::Path::to_path_buf),
                options.dashboard_session_retrieval_service.clone(),
                options.dashboard_session_retrieval_identity.clone(),
                options.registered_savings_db.clone(),
                options.automation_scheduler_reconciler.clone(),
                options.automation_writer.clone(),
                options.doctor_report_reader.clone(),
                options.remote_operational_status.clone(),
                options.code_index_freshness_reader.clone(),
                options.explorer_semantic_reader.clone(),
                options.feedback_status_reader.clone(),
                options.diagnostics_lsp.clone(),
                options.dashboard_application_invocation_executor.clone(),
                options.dashboard_delivery_settlement_authority.clone(),
                options.daemon_invocation_service.cloned(),
            )
            .await
        }
        _ => Err(unknown_tool_error(tool_name)),
    }
}
