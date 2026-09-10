use serde_json::Value;
use tracedecay_code_index::intake::content_digest;
use tracedecay_contracts::{ApplicationProblem, ResultContractRef, RetainedSurfaceOperation};
use tracedecay_graph_query::VerifiedGraphQueryRequest;
use tracedecay_privacy::{CodeSourceShapeV1, sanitize_code_source_bytes};
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingSurface};

use crate::tracedecay::TraceDecay;
use tracedecay_daemon_protocol::InvocationCancellationPolicy;
use tracedecay_daemon_service::application_surface::resolve_catalog_tool_binding;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;

use tracedecay_contracts::doctor::SemanticOwnerStateV1;
use tracedecay_dashboard_api::AdmittedDoctorReportV1;
use tracedecay_dashboard_api::code_index_freshness_api::CodeIndexFreshnessReader;
use tracedecay_mcp::handlers::analysis as portable_analysis;
use tracedecay_mcp::handlers::ast_grep as portable_ast_grep;
use tracedecay_mcp::handlers::git;
use tracedecay_mcp::handlers::graph as portable_graph;
use tracedecay_mcp::handlers::grep as portable_grep;
use tracedecay_mcp::handlers::info as portable_info;
use tracedecay_mcp::{
    AdmittedCodeIndex, McpAdmittedProjectV1, McpDoctorReportV1, McpProjectIdentityV1,
    McpRequestAuthoritiesV1, McpSemanticOwnerV1, McpToolBinding, McpToolContext, RequestControls,
    ToolResult,
};
use tracedecay_session_memory::runtime_telemetry::GenerationCensusSnapshot;

use super::ToolCallRegistryOptions;
use super::support::effective_path;
use super::tool_call_support::handle_retrieve;
use super::unknown_tool_error;
use super::{
    admin_cli, admin_project, application_surface, automation_runs, dashboard, dispatch_controls,
    edit, hook_runtime, info, skills, workflow,
};

mod health_dispatch;
pub(super) use health_dispatch::dispatch_health_tools;
use tracedecay_mcp::{
    retained_problem_envelope, retained_safe_diagnostic, validated_retained_response,
};

fn graph_read_unavailable(detail: &str) -> TraceDecayError {
    TraceDecayError::ProjectRoute {
        reason_code: "verified-code-graph-read-unavailable".to_owned(),
        retryable: false,
        detail: detail.to_owned(),
    }
}

const DOC_COVERAGE_SYMBOL_BUDGET: usize = 500_000;

fn doc_coverage_unavailable(detail: impl Into<String>) -> TraceDecayError {
    TraceDecayError::project_route("verified-doc-coverage-unavailable", false, detail.into())
}

fn admitted_doc_source(project_root: &std::path::Path, path: &str) -> Result<Vec<u8>> {
    let raw = std::fs::read(project_root.join(path)).map_err(|error| {
        doc_coverage_unavailable(format!(
            "verified documentation source `{path}` could not be read: {error}"
        ))
    })?;
    let shape = match path.rsplit('.').next() {
        Some("json" | "toml" | "yaml" | "yml") => CodeSourceShapeV1::StructuredData,
        _ => CodeSourceShapeV1::CodeOrProse,
    };
    let sanitized = sanitize_code_source_bytes(&raw, shape).map_err(|error| {
        doc_coverage_unavailable(format!(
            "verified documentation source `{path}` could not be admitted through the code sanitizer: {error}"
        ))
    })?;
    Ok(sanitized.into_parts().0)
}

fn verify_doc_coverage_sources_current(
    cg: &TraceDecay,
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: &Value,
    scope_prefix: Option<&str>,
) -> Result<()> {
    let path_prefix = effective_path(args, scope_prefix);
    let page = graph.symbols_page(None, DOC_COVERAGE_SYMBOL_BUDGET)?;
    if page.has_more {
        return Err(doc_coverage_unavailable(
            "verified documentation census exceeded its declared symbol budget",
        ));
    }
    let mut candidates = Vec::new();
    for symbol in page.symbols {
        let metadata = symbol.metadata.as_ref().ok_or_else(|| {
            doc_coverage_unavailable(format!(
                "symbol {} has no admitted documentation metadata",
                symbol.occurrence.as_str()
            ))
        })?;
        let path = symbol
            .binding
            .as_ref()
            .and_then(|binding| binding.logical_path.as_deref())
            .ok_or_else(|| {
                doc_coverage_unavailable(format!(
                    "symbol {} has no admitted logical file binding",
                    symbol.occurrence.as_str()
                ))
            })?;
        if metadata.visibility == "public"
            && portable_analysis::is_documentable_kind(&metadata.kind)
            && tracedecay_runtime_core::path_scope::path_matches_scope(path, path_prefix)
        {
            candidates.push(symbol);
        }
    }
    candidates.sort_by(|left, right| {
        left.binding
            .as_ref()
            .and_then(|binding| binding.logical_path.as_deref())
            .cmp(
                &right
                    .binding
                    .as_ref()
                    .and_then(|binding| binding.logical_path.as_deref()),
            )
            .then_with(|| left.occurrence.cmp(&right.occurrence))
    });

    let mut admitted_path = None::<String>;
    let mut admitted_bytes = Vec::new();
    for symbol in candidates {
        let metadata = symbol.metadata.as_ref().ok_or_else(|| {
            doc_coverage_unavailable("documentation candidate metadata disappeared")
        })?;
        let binding = symbol.binding.as_ref().ok_or_else(|| {
            doc_coverage_unavailable("documentation candidate file binding disappeared")
        })?;
        let path = binding.logical_path.as_deref().ok_or_else(|| {
            doc_coverage_unavailable("documentation candidate logical path disappeared")
        })?;
        if admitted_path.as_deref() != Some(path) {
            admitted_bytes = admitted_doc_source(cg.project_root(), path)?;
            admitted_path = Some(path.to_owned());
        }
        let source_span = binding.source_span.ok_or_else(|| {
            doc_coverage_unavailable(format!(
                "public symbol {} has no admitted source span",
                symbol.occurrence.as_str()
            ))
        })?;
        let start = usize::try_from(source_span.start_byte).map_err(|error| {
            doc_coverage_unavailable(format!(
                "public symbol {} source start does not fit this host: {error}",
                symbol.occurrence.as_str()
            ))
        })?;
        let end = usize::try_from(source_span.end_byte).map_err(|error| {
            doc_coverage_unavailable(format!(
                "public symbol {} source end does not fit this host: {error}",
                symbol.occurrence.as_str()
            ))
        })?;
        let source = admitted_bytes.get(start..end).ok_or_else(|| {
            doc_coverage_unavailable(format!(
                "public symbol {} source span is outside `{path}`",
                symbol.occurrence.as_str()
            ))
        })?;
        if content_digest(source) != metadata.content_digest {
            return Err(doc_coverage_unavailable(format!(
                "documentation source for symbol {} no longer matches the admitted graph generation",
                symbol.occurrence.as_str()
            )));
        }
    }
    Ok(())
}

async fn admitted_graph_query(
    _cg: &TraceDecay,
    options: &ToolCallRegistryOptions<'_>,
    operation_name: &str,
) -> Result<tracedecay_graph_query::VerifiedGraphQuery> {
    let Some(port) = options.verified_graph_query_port.as_deref() else {
        return Err(graph_read_unavailable(
            "the exact project verified graph query is not mounted",
        ));
    };
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
        tracedecay_contracts::retrieval::catalog::primitive_read_operation(operation_name)
            .map_err(|error| TraceDecayError::Config {
                message: format!("invalid graph read operation: {error}"),
            })?
            .ok_or_else(|| TraceDecayError::Config {
                message: format!("unregistered graph read operation: {operation_name}"),
            })?;
    // Admission wait is measured apart from handler execution: every
    // graph-backed tool in the graph/info/analysis/git/health groups funnels
    // through this one open, so a slow span here is admission contention or a
    // stale generation, never handler work.
    let query = hotpath::future!(
        port.open(VerifiedGraphQueryRequest::new(
            &operation,
            request_id,
            deadline,
            cancellation,
        )),
        label = "mcp.dispatch.graph_query_admission"
    )
    .await?;
    if let tracedecay_graph_query::CodeGraphReadFreshnessV1::LastCompleteStale {
        sealed_at,
        rebuild_in_flight,
    } = query.freshness()
    {
        // Every graph-backed tool funnels through this open, so this is the
        // single point that reports serve-old-while-rebuilding back to the
        // dispatch boundary for the typed response trailer.
        let _ = options
            .served_stale_graph_generation
            .set(super::ServedStaleCodeGraphReadV1 {
                generation: query.generation().as_str().to_owned(),
                sealed_at,
                rebuild_in_flight,
            });
    }
    Ok(query)
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
    deadline: Option<&tracedecay_contracts::Deadline>,
) -> Option<std::time::Duration> {
    let ceiling = tool_dispatch_ceiling(tool_name);
    match deadline {
        // A carried deadline is preferred whenever it is shorter; the ceiling
        // still clamps a pathologically distant one so it can never be a way
        // out of the bound.
        Some(deadline) => tracedecay_daemon_protocol::deadline_remaining(deadline)
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
    // A firing ceiling is a defect signal upstream; count every occurrence so
    // profiling sees the refusals, not only the successful dispatches.
    hotpath::gauge!("mcp.tool_call.dispatch_deadline_total").inc(1_u64);
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
#[hotpath::measure(future = true, label = "mcp.dispatch.graph")]
pub(super) async fn dispatch_graph_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    selected_scope_prefix: Option<&str>,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    dispatch_graph_tools_inner(tool_name, cg, args, selected_scope_prefix, options).await
}

fn dispatch_graph_tools_inner<'a>(
    tool_name: &'a str,
    cg: &'a TraceDecay,
    args: Value,
    selected_scope_prefix: Option<&'a str>,
    options: ToolCallRegistryOptions<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + 'a>> {
    // Erase the deeply nested match-arm futures before they reach the
    // measured wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        let project = admitted_project_authorities(cg, &options)?;
        let snapshots = AdmittedRequestSnapshotsV1::default();
        let freshness = graph_freshness_reader(tool_name, &options);
        let ctx = admitted_tool_context(&options, &project, &snapshots, freshness)?;
        match tool_name {
            "tracedecay_search" => {
                portable_graph::handle_search(
                    &ctx,
                    admitted_graph_query(cg, &options, "code_symbol_search"),
                    args,
                    selected_scope_prefix,
                    options.code_index_ignored_dependency_admission.as_deref(),
                )
                .await
            }
            "tracedecay_grep" => {
                let graph = admitted_graph_query(cg, &options, "source_lines").await;
                portable_grep::handle_grep(
                    cg.project_root(),
                    graph.as_ref(),
                    args,
                    selected_scope_prefix,
                    options.application_deadline.clone(),
                    options.application_cancellation.clone(),
                )
                .await
            }
            "tracedecay_ast_grep_search" => {
                portable_ast_grep::handle_ast_grep_search(
                    cg.project_root(),
                    args,
                    selected_scope_prefix,
                    options.application_deadline.clone(),
                    options.application_cancellation.clone(),
                )
                .await
            }
            "tracedecay_retrieve" => handle_retrieve(cg, &args).await,
            "tracedecay_context" => {
                portable_graph::handle_context(
                    &ctx,
                    admitted_graph_query(cg, &options, "context"),
                    args,
                    selected_scope_prefix,
                )
                .await
            }
            "tracedecay_callers" => {
                let graph_query = admitted_graph_query(cg, &options, "code_callers").await?;
                portable_graph::handle_callers(&graph_query, args).await
            }
            "tracedecay_callees" => {
                let graph_query = admitted_graph_query(cg, &options, "callees").await?;
                portable_graph::handle_callees(&graph_query, args).await
            }
            "tracedecay_impact" => {
                let graph_query = admitted_graph_query(cg, &options, "impact").await?;
                portable_graph::handle_impact(&graph_query, args).await
            }
            "tracedecay_node" => {
                let graph_query = admitted_graph_query(cg, &options, "node").await?;
                portable_graph::handle_node(&graph_query, args).await
            }
            "tracedecay_similar" => {
                let graph_query = admitted_graph_query(cg, &options, "similar").await?;
                portable_graph::handle_similar(&ctx, &graph_query, args).await
            }
            "tracedecay_rename_preview" => {
                let graph_query = admitted_graph_query(cg, &options, "rename_preview").await?;
                portable_graph::handle_rename_preview(&ctx, &graph_query, args).await
            }
            "tracedecay_implementations" => {
                let graph_query =
                    admitted_graph_query(cg, &options, "code_implementations").await?;
                portable_graph::handle_implementations(&graph_query, args, selected_scope_prefix)
                    .await
            }
            "tracedecay_callers_for" => {
                let graph_query = admitted_graph_query(cg, &options, "code_callers").await?;
                portable_graph::handle_callers_for(&graph_query, args).await
            }
            "tracedecay_find_exact_symbol" => {
                let graph_query = admitted_graph_query(cg, &options, "qualified_name").await?;
                portable_graph::handle_find_exact_symbol(
                    &ctx,
                    &graph_query,
                    args,
                    selected_scope_prefix,
                    options.code_index_ignored_dependency_admission.as_deref(),
                )
                .await
            }
            "tracedecay_by_qualified_name" => {
                let graph_query = admitted_graph_query(cg, &options, "qualified_name").await?;
                portable_graph::handle_by_qualified_name(&graph_query, args).await
            }
            "tracedecay_signature" => {
                let graph_query =
                    admitted_graph_query(cg, &options, "code_signature_search").await?;
                portable_graph::handle_signature(&graph_query, args).await
            }
            "tracedecay_impls" => {
                let graph_query =
                    admitted_graph_query(cg, &options, "code_implementations").await?;
                portable_graph::handle_impls(&graph_query, args).await
            }
            "tracedecay_derives" => {
                let graph_query = admitted_graph_query(cg, &options, "code_type_hierarchy").await?;
                portable_graph::handle_derives(&graph_query, args).await
            }
            _ => Err(unknown_tool_error(tool_name)),
        }
    })
}

/// Dispatch project-info, registry, and file-inspection tools
/// (`tracedecay_status`, `tracedecay_project_list`, `tracedecay_read`, ...).
#[allow(clippy::too_many_arguments)]
#[hotpath::measure(future = true, label = "mcp.dispatch.info")]
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
    dispatch_info_tools_inner(
        tool_name,
        cg,
        args,
        server_stats,
        scope_prefix,
        selected_scope_prefix,
        active_project_session_db,
        options,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
fn dispatch_info_tools_inner<'a>(
    tool_name: &'a str,
    cg: &'a TraceDecay,
    args: Value,
    server_stats: Option<Value>,
    scope_prefix: Option<&'a str>,
    selected_scope_prefix: Option<&'a str>,
    _active_project_session_db: Option<&'a RegisteredGlobalDbLeaseV1>,
    options: ToolCallRegistryOptions<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + 'a>> {
    // Erase the deeply nested match-arm futures before they reach the
    // measured wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        match tool_name {
            "tracedecay_remote_status" => portable_info::handle_remote_status(
                cg.project_root(),
                &args,
                options.remote_operational_status.as_ref(),
            ),
            "tracedecay_status" => {
                let project = admitted_project_authorities(cg, &options)?;
                let snapshots = admitted_status_snapshots(cg, &options).await;
                let ctx = admitted_tool_context(
                    &options,
                    &project,
                    &snapshots,
                    options.code_index_freshness_reader.as_ref(),
                )?;
                portable_info::handle_status(&ctx, args, server_stats, scope_prefix).await
            }
            "tracedecay_active_project" => {
                let project = admitted_project_authorities(cg, &options)?;
                let snapshots = AdmittedRequestSnapshotsV1::default();
                let ctx = admitted_tool_context(&options, &project, &snapshots, None)?;
                portable_info::handle_active_project(&ctx, &args, server_stats, scope_prefix)
            }
            "tracedecay_project_list" => {
                portable_info::handle_project_list(
                    cg.project_root(),
                    args,
                    options.project_registry_reads,
                )
                .await
            }
            "tracedecay_project_search" => {
                portable_info::handle_project_search(
                    cg.project_root(),
                    args,
                    options.project_registry_reads,
                )
                .await
            }
            "tracedecay_project_context" => {
                portable_info::handle_project_context(
                    cg.project_root(),
                    args,
                    options.project_registry_reads,
                )
                .await
            }
            "tracedecay_files" => {
                let graph = admitted_graph_query(cg, &options, "file_metadata").await?;
                portable_info::handle_files(&graph, args, selected_scope_prefix).await
            }
            "tracedecay_admin_sync" => {
                info::handle_admin_sync(cg, args, options.code_index_reconcile_sink.as_ref()).await
            }
            "tracedecay_port_status" => {
                let graph = admitted_graph_query(cg, &options, "port_status").await?;
                portable_info::handle_port_status(&graph, args).await
            }
            "tracedecay_port_order" => {
                let graph = admitted_graph_query(cg, &options, "port_order").await?;
                portable_info::handle_port_order(&graph, args).await
            }
            "tracedecay_simplify_scan" => portable_info::handle_simplify_scan().await,
            "tracedecay_type_hierarchy" => {
                let graph = admitted_graph_query(cg, &options, "code_type_hierarchy").await?;
                portable_info::handle_type_hierarchy(&graph, args).await
            }
            "tracedecay_body" => {
                let graph = admitted_graph_query(cg, &options, "source_body").await?;
                portable_info::handle_body(&graph, args, selected_scope_prefix).await
            }
            "tracedecay_todos" => {
                let graph = admitted_graph_query(cg, &options, "todos").await?;
                portable_info::handle_todos(&graph, args, scope_prefix).await
            }
            "tracedecay_read" => {
                let operation = match args.get("mode").and_then(Value::as_str).unwrap_or("full") {
                    "map" => "source_outline",
                    "signatures" => "code_signature_search",
                    _ => "source_lines",
                };
                let graph = admitted_graph_query(cg, &options, operation).await?;
                portable_info::handle_read(&graph, args).await
            }
            "tracedecay_outline" => {
                let graph = admitted_graph_query(cg, &options, "source_outline").await?;
                portable_info::handle_outline(&graph, args).await
            }
            "tracedecay_config" => portable_info::handle_config(cg.project_root(), &args).await,
            "tracedecay_signature_search" => {
                let graph = admitted_graph_query(cg, &options, "code_signature_search").await?;
                portable_info::handle_signature_search(&graph, args, selected_scope_prefix).await
            }
            _ => Err(unknown_tool_error(tool_name)),
        }
    })
}

/// Dispatch administrative tools (`tracedecay_hook_runtime`,
/// `tracedecay_admin_cli`, `tracedecay_admin_project`).
#[hotpath::measure(future = true, label = "mcp.dispatch.admin")]
pub(super) async fn dispatch_admin_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    dispatch_admin_tools_inner(tool_name, cg, args, options).await
}

fn dispatch_admin_tools_inner<'a>(
    tool_name: &'a str,
    cg: &'a TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + 'a>> {
    // Erase the deeply nested match-arm futures before they reach the
    // measured wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
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
                let deadline = options.application_deadline.clone().ok_or_else(|| {
                    TraceDecayError::Config {
                        message: "admin project request deadline is unavailable".to_owned(),
                    }
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
    })
}

/// Dispatch catalog-owned application surfaces.
#[hotpath::measure(future = true, label = "mcp.dispatch.application")]
pub(super) async fn dispatch_application_surface_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    dispatch_application_surface_tools_inner(tool_name, cg, args, options).await
}

fn dispatch_application_surface_tools_inner<'a>(
    tool_name: &'a str,
    cg: &'a TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + 'a>> {
    // Erase the deeply nested application-surface future before it reaches
    // the measured wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        let Some(operation) = ApplicationSurfaceOperation::from_tool_name(tool_name) else {
            return Err(unknown_tool_error(tool_name));
        };
        let normalized_args =
            match tracedecay_daemon_protocol::adapt_application_tool_request(tool_name, args) {
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
            RequestControls {
                deadline: options.application_deadline.as_ref(),
                cancellation: options.application_cancellation.as_ref(),
            },
        )
        .await
    })
}

/// Dispatch static-analysis report tools such as `tracedecay_dead_code` and
/// `tracedecay_complexity`.
#[hotpath::measure(future = true, label = "mcp.dispatch.analysis")]
pub(super) async fn dispatch_analysis_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    dispatch_analysis_tools_inner(tool_name, cg, args, scope_prefix, options).await
}

fn dispatch_analysis_tools_inner<'a>(
    tool_name: &'a str,
    cg: &'a TraceDecay,
    args: Value,
    scope_prefix: Option<&'a str>,
    options: ToolCallRegistryOptions<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + 'a>> {
    // Erase the deeply nested match-arm futures before they reach the
    // measured wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        match tool_name {
            "tracedecay_dead_code" => {
                let graph = admitted_graph_query(cg, &options, "health_read").await?;
                portable_analysis::handle_dead_code(&graph, args, scope_prefix).await
            }
            "tracedecay_circular" => {
                let graph = admitted_graph_query(cg, &options, "health_read").await?;
                portable_analysis::handle_circular(&graph, args).await
            }
            "tracedecay_hotspots" => {
                let graph = admitted_graph_query(cg, &options, "health_read").await?;
                portable_analysis::handle_hotspots(&graph, args, scope_prefix).await
            }
            "tracedecay_unused_imports" => {
                let graph = admitted_graph_query(cg, &options, "health_read").await?;
                portable_analysis::handle_unused_imports(
                    cg.project_root(),
                    &graph,
                    args,
                    scope_prefix,
                )
                .await
            }
            // The one analysis tool that opens no graph query: its whole finding is
            // that the graph and the compiler disagree, so taking the graph's file
            // set as input would answer the question with the very source that is
            // under suspicion.
            "tracedecay_unmounted_files" => {
                portable_analysis::handle_unmounted_files(cg.project_root(), args, scope_prefix)
                    .await
            }
            "tracedecay_rank" => {
                let graph = admitted_graph_query(cg, &options, "health_read").await?;
                portable_analysis::handle_rank(&graph, args, scope_prefix).await
            }
            "tracedecay_largest" => {
                let graph = admitted_graph_query(cg, &options, "health_read").await?;
                portable_analysis::handle_largest(&graph, args, scope_prefix).await
            }
            "tracedecay_coupling" => {
                let graph = admitted_graph_query(cg, &options, "health_read").await?;
                portable_analysis::handle_coupling(&graph, args, scope_prefix).await
            }
            "tracedecay_inheritance_depth" => {
                let graph = admitted_graph_query(cg, &options, "health_read").await?;
                portable_analysis::handle_inheritance_depth(&graph, args, scope_prefix).await
            }
            "tracedecay_distribution" => {
                let graph = admitted_graph_query(cg, &options, "health_read").await?;
                portable_analysis::handle_distribution(&graph, args, scope_prefix).await
            }
            "tracedecay_recursion" => {
                let graph = admitted_graph_query(cg, &options, "health_read").await?;
                portable_analysis::handle_recursion(&graph, args, scope_prefix).await
            }
            "tracedecay_complexity" => {
                let graph = admitted_graph_query(cg, &options, "health_read").await?;
                portable_analysis::handle_complexity(&graph, args, scope_prefix).await
            }
            "tracedecay_doc_coverage" => {
                let graph = admitted_graph_query(cg, &options, "health_read").await?;
                verify_doc_coverage_sources_current(cg, &graph, &args, scope_prefix)?;
                portable_analysis::handle_doc_coverage(&graph, args, scope_prefix).await
            }
            "tracedecay_god_class" => {
                let graph = admitted_graph_query(cg, &options, "health_read").await?;
                portable_analysis::handle_god_class(&graph, args, scope_prefix).await
            }
            "tracedecay_unsafe_patterns" => {
                let graph = admitted_graph_query(cg, &options, "health_read").await?;
                portable_analysis::handle_unsafe_patterns(
                    cg.project_root(),
                    &graph,
                    args,
                    scope_prefix,
                )
                .await
            }
            "tracedecay_constructors" => {
                let graph = admitted_graph_query(cg, &options, "health_read").await?;
                portable_analysis::handle_constructors(&graph, args, scope_prefix).await
            }
            "tracedecay_field_sites" => {
                let graph = admitted_graph_query(cg, &options, "health_read").await?;
                portable_analysis::handle_field_sites(&graph, args, scope_prefix).await
            }
            _ => Err(unknown_tool_error(tool_name)),
        }
    })
}

/// Dispatch git-aware tools (`tracedecay_affected`, `tracedecay_changelog`,
/// branch and PR context helpers).
#[hotpath::measure(future = true, label = "mcp.dispatch.git")]
pub(super) async fn dispatch_git_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    dispatch_git_tools_inner(tool_name, cg, args, options).await
}

fn dispatch_git_tools_inner<'a>(
    tool_name: &'a str,
    cg: &'a TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + 'a>> {
    // Erase the deeply nested match-arm futures before they reach the
    // measured wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        // Tree walks and revwalks still need a uniform dispatch deadline. Branch
        // generation reads additionally carry this deadline into their bounded
        // blocking/ref and daemon-generation executors, so timing out this future
        // also tells the underlying operation to stop at its next checkpoint.
        let carried_deadline = options.application_deadline.as_ref();
        let remaining = carried_deadline.and_then(tracedecay_daemon_protocol::deadline_remaining);
        let project = admitted_project_authorities(cg, &options)?;
        let snapshots = AdmittedRequestSnapshotsV1::default();
        let ctx = admitted_tool_context(&options, &project, &snapshots, None)?;

        let handler = async {
            match tool_name {
                "tracedecay_affected" => {
                    let graph = admitted_graph_query(cg, &options, "file_dependents").await?;
                    git::handle_affected(&ctx, &graph, args).await
                }
                "tracedecay_diff_context" => {
                    let graph = admitted_graph_query(cg, &options, "file_dependents").await?;
                    git::handle_diff_context(&ctx, &graph, args).await
                }
                "tracedecay_changelog" => {
                    git::handle_changelog(
                        &ctx,
                        admitted_graph_query(cg, &options, "file_dependents"),
                        args,
                    )
                    .await
                }
                "tracedecay_commit_context" => {
                    let graph = admitted_graph_query(cg, &options, "file_dependents").await?;
                    git::handle_commit_context(&ctx, &graph, args).await
                }
                "tracedecay_pr_context" => {
                    git::handle_pr_context(
                        &ctx,
                        admitted_graph_query(cg, &options, "file_dependents"),
                        args,
                    )
                    .await
                }
                "tracedecay_branch_search" => git::handle_branch_search(&ctx, args).await,
                "tracedecay_branch_diff" => git::handle_branch_diff(&ctx, args).await,
                "tracedecay_branch_list" => git::handle_branch_list(&ctx, args).await,
                _ => Err(unknown_tool_error(tool_name)),
            }
        };

        match (carried_deadline.is_some(), remaining) {
            (_, Some(remaining)) => match tokio::time::timeout(remaining, handler).await {
                Ok(result) => result,
                Err(_elapsed) => Ok(git::git_dispatch_deadline_result(&ctx, tool_name)),
            },
            // `deadline_remaining` yields `None` for a non-positive budget, so a
            // carried deadline that already elapsed must be rejected rather than
            // dispatched unbounded.
            (true, None) => Ok(git::git_dispatch_deadline_result(&ctx, tool_name)),
            // Standalone / non-admission callers carry no deadline and stay
            // unbounded.
            (false, None) => handler.await,
        }
    })
}

/// Builds the request-scoped admitted project snapshot from the live
/// `TraceDecay` this call already holds. It must not be cached: a later
/// branch reopen swaps the served instance.
///
/// Every served route publishes a checkout at project-open. Absence is a
/// typed root failure, not a second binding shape. The session store is the
/// canonical `registered_project_session_db` lease only — never a silent
/// fallback to `session_authorities.project`. Attached means admitted;
/// absent is the typed unavailable/denied state.
fn admitted_project_authorities(
    cg: &TraceDecay,
    options: &ToolCallRegistryOptions<'_>,
) -> Result<McpAdmittedProjectV1> {
    let Some(scope) = options.admitted_project_scope.clone() else {
        return Err(TraceDecayError::project_route(
            "admitted_project_scope_unresolved",
            false,
            "every served MCP tool call requires the checkout the route published at project-open",
        ));
    };
    McpAdmittedProjectV1::new(
        McpProjectIdentityV1 {
            project_root: cg.project_root().to_path_buf(),
            scope,
            active_branch: cg.active_branch().map(str::to_owned),
            serving_branch: cg.serving_branch().map(str::to_owned),
            fallback_warning: cg.fallback_warning().map(str::to_owned),
        },
        cg.store_layout().clone(),
        cg.db().clone(),
        cg.db_path(),
        cg.store_runtime_registry.clone(),
        cg.configuration_runtime().clone(),
        options.registered_project_session_db.clone(),
    )
    .map_err(Into::into)
}

#[derive(Default)]
enum SemanticOwnerSnapshotV1 {
    #[default]
    NotAttached,
    AttachedAbsent,
    Attached(SemanticOwnerStateV1),
}

#[derive(Default)]
enum DoctorReportSnapshotV1 {
    #[default]
    NotAttached,
    ReadFailed,
    Read(AdmittedDoctorReportV1),
}

#[derive(Default)]
struct AdmittedRequestSnapshotsV1 {
    generation_census: Option<GenerationCensusSnapshot>,
    semantic_owner: SemanticOwnerSnapshotV1,
    doctor_report: DoctorReportSnapshotV1,
}

async fn admitted_generation_census(
    options: &ToolCallRegistryOptions<'_>,
) -> Option<GenerationCensusSnapshot> {
    match options.generation_census_reader.as_ref() {
        Some(reader) => Some(reader().await),
        None => None,
    }
}

async fn admitted_semantic_owner(
    cg: &TraceDecay,
    options: &ToolCallRegistryOptions<'_>,
) -> SemanticOwnerSnapshotV1 {
    match options.daemon_invocation_service {
        Some(service) => {
            match tracedecay_daemon_service::DaemonSemanticOwnerRuntimeRegistrar::new(service)
                .state(cg.project_root())
                .await
            {
                Some(state) => SemanticOwnerSnapshotV1::Attached(state),
                None => SemanticOwnerSnapshotV1::AttachedAbsent,
            }
        }
        None => SemanticOwnerSnapshotV1::NotAttached,
    }
}

async fn admitted_doctor_report(options: &ToolCallRegistryOptions<'_>) -> DoctorReportSnapshotV1 {
    match options.doctor_report_reader.as_ref() {
        Some(reader) => {
            match hotpath::future!(reader(), label = "mcp.health.runtime.doctor_report").await {
                Ok(report) => DoctorReportSnapshotV1::Read(report),
                Err(_) => DoctorReportSnapshotV1::ReadFailed,
            }
        }
        None => DoctorReportSnapshotV1::NotAttached,
    }
}

/// Status needs census and the semantic-owner snapshot. Freshness is a
/// lazy reader on the binding so the handler reads it when it renders.
/// It does not run the doctor reader — that report is runtime-only.
async fn admitted_status_snapshots(
    cg: &TraceDecay,
    options: &ToolCallRegistryOptions<'_>,
) -> AdmittedRequestSnapshotsV1 {
    AdmittedRequestSnapshotsV1 {
        generation_census: hotpath::future!(
            admitted_generation_census(options),
            label = "mcp.info.status.generation_census"
        )
        .await,
        semantic_owner: admitted_semantic_owner(cg, options).await,
        ..AdmittedRequestSnapshotsV1::default()
    }
}

/// Runtime always needs the census. The doctor report is snapshotted only
/// when the caller asked for it, matching the previous handler's cost.
async fn admitted_runtime_snapshots(
    options: &ToolCallRegistryOptions<'_>,
    include_doctor: bool,
) -> AdmittedRequestSnapshotsV1 {
    AdmittedRequestSnapshotsV1 {
        generation_census: hotpath::future!(
            admitted_generation_census(options),
            label = "runtime_ports.generation_census"
        )
        .await,
        doctor_report: if include_doctor {
            admitted_doctor_report(options).await
        } else {
            DoctorReportSnapshotV1::NotAttached
        },
        ..AdmittedRequestSnapshotsV1::default()
    }
}

fn graph_freshness_reader<'a>(
    tool_name: &str,
    options: &'a ToolCallRegistryOptions<'a>,
) -> Option<&'a CodeIndexFreshnessReader> {
    matches!(tool_name, "tracedecay_search" | "tracedecay_context")
        .then_some(options.code_index_freshness_reader.as_ref())
        .flatten()
}

/// Binds the admitted authorities a moved handler family reads.
///
/// Everything the family may touch — the resolved project scope, the caller's
/// deadline and cancellation, the registered project session store that
/// authenticates PR-context cursors, and the daemon-owned code-index executors
/// with the authorization proved for them — crosses into `tracedecay-mcp` as
/// one validated binding, under the single checkout the serving route was
/// admitted for. An authority the daemon did not admit stays absent and the
/// handler reports its own typed unavailable state; an authority that
/// contradicts the admitted checkout refuses the whole call.
///
/// The snapshot comes from [`admitted_project_authorities`]; this function
/// is the binding constructor, not a second admission.
fn admitted_tool_context<'a>(
    options: &'a ToolCallRegistryOptions<'a>,
    project: &'a McpAdmittedProjectV1,
    snapshots: &'a AdmittedRequestSnapshotsV1,
    freshness: Option<&'a CodeIndexFreshnessReader>,
) -> Result<McpToolContext<'a>> {
    // Project open resolves one checkout per served route and publishes it
    // alongside the authorities that mount behind it, so this is the checkout
    // every scoped authority below belongs to.
    let scope = Some(&project.identity().scope);
    // Executors and their admission envelope are published together by the
    // route. Presenting executors without the envelope is a wiring fault, not
    // a capability to report: they would authenticate nothing.
    let code_index = match (
        scope.and(options.code_index_search_authority.as_ref()),
        options.code_index_search_executor.as_ref(),
        options.code_index_branch_diff_executor.as_ref(),
    ) {
        (Some(authority), search, branch_diff) => {
            Some(AdmittedCodeIndex::new(authority, search, branch_diff)?)
        }
        (None, None, None) => None,
        (None, _, _) => {
            return Err(TraceDecayError::project_route(
                "mcp_tool_binding_code_index_without_authority",
                false,
                "code-index executors were admitted without the admitted scope and read admission envelope they authenticate against",
            ));
        }
    };
    let request = McpRequestAuthoritiesV1 {
        controls: RequestControls {
            deadline: options.application_deadline.as_ref(),
            cancellation: options.application_cancellation.as_ref(),
        },
        code_index,
        freshness,
        generation_census: snapshots.generation_census.as_ref(),
        semantic_owner: match &snapshots.semantic_owner {
            SemanticOwnerSnapshotV1::Attached(state) => McpSemanticOwnerV1::Attached(state),
            SemanticOwnerSnapshotV1::AttachedAbsent => McpSemanticOwnerV1::AttachedAbsent,
            SemanticOwnerSnapshotV1::NotAttached => McpSemanticOwnerV1::NotAttached,
        },
        doctor_report: match &snapshots.doctor_report {
            DoctorReportSnapshotV1::Read(report) => McpDoctorReportV1::Read(report),
            DoctorReportSnapshotV1::ReadFailed => McpDoctorReportV1::ReadFailed,
            DoctorReportSnapshotV1::NotAttached => McpDoctorReportV1::NotAttached,
        },
    };
    Ok(McpToolContext::bind(McpToolBinding { project, request })?)
}

/// Dispatch source-editing tools (`tracedecay_str_replace`,
/// `tracedecay_move_symbol`, ...).
#[hotpath::measure(future = true, label = "mcp.dispatch.edit")]
pub(super) async fn dispatch_edit_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    dispatch_edit_tools_inner(tool_name, cg, args, options).await
}

fn dispatch_edit_tools_inner<'a>(
    tool_name: &'a str,
    cg: &'a TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + 'a>> {
    // Erase the deeply nested match-arm futures before they reach the
    // measured wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        let invocation = edit::SourceEditInvocationContext {
            executor: options.application_invocation_executor,
            request_id: options.application_request_id.clone(),
            deadline: options.application_deadline.clone(),
            cancellation: options.application_cancellation.clone(),
        };
        match tool_name {
            "tracedecay_str_replace" => {
                edit::handle_str_replace(cg, args, invocation.clone()).await
            }
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
            "tracedecay_move_symbol" => {
                edit::handle_move_symbol(cg, args, invocation.clone()).await
            }
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
    })
}

/// Dispatch retained memory, session, and workflow operations only after the
/// application-owned catalog has resolved their stable operation identity.
#[hotpath::measure(future = true, label = "mcp.dispatch.retained_application")]
pub(super) async fn dispatch_retained_application_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    _scope_prefix: Option<&str>,
    _active_project_session_db: Option<&RegisteredGlobalDbLeaseV1>,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    dispatch_retained_application_tools_inner(
        tool_name,
        cg,
        args,
        _scope_prefix,
        _active_project_session_db,
        options,
    )
    .await
}

fn dispatch_retained_application_tools_inner<'a>(
    tool_name: &'a str,
    cg: &'a TraceDecay,
    args: Value,
    _scope_prefix: Option<&'a str>,
    _active_project_session_db: Option<&'a RegisteredGlobalDbLeaseV1>,
    options: ToolCallRegistryOptions<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + 'a>> {
    // Erase the deeply nested retained-application future before it reaches
    // the measured wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        let retained_operation = RetainedSurfaceOperation::from_tool_name(tool_name)
            .ok_or_else(|| unknown_tool_error(tool_name))?;
        let binding = resolve_catalog_tool_binding(BindingSurface::Mcp, tool_name)
            .map_err(|error| TraceDecayError::Config {
                message: error.to_string(),
            })?
            .ok_or_else(|| unknown_tool_error(tool_name))?;
        // Normalization strips `project_selector`, so the selected project is
        // read here: a selector-bound retained route is served by the calling
        // session's own runtime, and only the selector names the project the
        // retained owner actually opened.
        let selected_project_id = super::tool_call_support::selected_project_id_argument(&args);
        let normalized = tracedecay_daemon_protocol::separate_application_tool_request(args)
            .map_err(|error| TraceDecayError::Config {
                message: error.to_string(),
            })?;
        let requested_format = normalized.requested_format;
        let request = hotpath::measure_block!(
            "mcp.retained.decode",
            tracedecay_daemon_service::application_surface::retained::decode_request(
                retained_operation,
                normalized.request,
            )
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid retained application request for {tool_name}: {error}"),
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
        let selected_scope_contract = result_contract.clone();
        let selected_scope_request_id = request_id.clone();
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
                let invocation =
                    tracedecay_daemon_protocol::DaemonInvocationRequest::retained_application(
                        request_id.as_str(),
                        request,
                        tracedecay_contracts::now_micros(),
                        deadline.clone(),
                        cancellation.context(),
                    );
                let policy =
                    if tracedecay_contracts::retained_surfaces::retained_surface_operation_is_effect(
                        retained_operation,
                    ) {
                        InvocationCancellationPolicy::AuthoritativeEffect
                    } else {
                        InvocationCancellationPolicy::ReadOnly
                    };
                match hotpath::future!(
                    executor.invoke_controlled(invocation, deadline, cancellation, policy),
                    label = "mcp.retained.invoke"
                )
                .await
                {
                    Ok(response)
                        if response.protocol
                            == tracedecay_daemon_protocol::DAEMON_INVOCATION_PROTOCOL
                            && response.revision
                                == tracedecay_daemon_protocol::DAEMON_INVOCATION_REVISION
                            && response.request_id == request_id.as_str() =>
                    {
                        validated_retained_response(
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
        let result = match selected_project_id {
            Some(selected_project_id) => {
                restate_selected_project_scope(
                    result,
                    &selected_project_id,
                    options.global_db.map(RegisteredGlobalDbLeaseV1::as_ref),
                    selected_scope_contract,
                    selected_scope_request_id,
                )
                .await?
            }
            None => result,
        };
        hotpath::measure_block!(
            "mcp.retained.render",
            application_surface::render_retained_result(
                Some(cg.project_root()),
                retained_operation,
                &binding.binding_id,
                result,
                requested_format,
            )
        )
    })
}

/// Report the exact project a selector-bound retained route was served from.
///
/// A selector-bound route stays on the calling session's admitted runtime, so
/// the daemon resolves the response scope from that session — the admitted
/// project — even when the retained owner opened the selected project's store
/// instead. Restating the scope here keeps the envelope truthful about which
/// project answered.
///
/// Only an evidence (read) outcome can be restated: an effect's receipt is
/// signed over the admitted scope, so a committed effect that somehow named
/// another project is refused rather than reported under either scope. Any
/// selector that cannot be resolved to an exact registered scope is refused
/// the same way, with the indistinguishable disclosure the retained surface
/// already uses for a foreign selector.
async fn restate_selected_project_scope(
    result: tracedecay_contracts::ApplicationResult<
        tracedecay_contracts::retained_surfaces::RetainedSurfaceResultV1,
    >,
    selected_project_id: &str,
    global_db: Option<&tracedecay_global_db::RegisteredGlobalDb>,
    contract: ResultContractRef,
    request_id: tracedecay_contracts::RequestId,
) -> Result<
    tracedecay_contracts::ApplicationResult<
        tracedecay_contracts::retained_surfaces::RetainedSurfaceResultV1,
    >,
> {
    use super::tool_call_support::SelectedProjectScopeV1;

    let mut envelope = match result {
        Ok(envelope) => envelope,
        Err(problem) => return Ok(Err(problem)),
    };
    let refused = || {
        retained_problem_envelope(
            contract.clone(),
            request_id.clone(),
            ApplicationProblem::not_found_or_not_authorized(
                tracedecay_contracts::RetryDirective::Never,
            ),
        )
    };
    match super::tool_call_support::selected_project_scope(
        selected_project_id,
        &envelope.scope,
        global_db,
    )
    .await
    {
        SelectedProjectScopeV1::Unchanged => Ok(Ok(envelope)),
        SelectedProjectScopeV1::Restated(scope) => {
            if matches!(
                envelope.outcome,
                tracedecay_contracts::ApplicationOutcome::Evidence(_)
            ) {
                envelope.scope = *scope;
                Ok(Ok(envelope))
            } else {
                Ok(Err(refused()?))
            }
        }
        SelectedProjectScopeV1::Refused => Ok(Err(refused()?)),
    }
}

/// Dispatch memory, skill, and analytics tools (`tracedecay_fact_store_add`,
/// `tracedecay_skill_list`, `tracedecay_analytics`, ...).
#[hotpath::measure(future = true, label = "mcp.dispatch.memory")]
pub(super) async fn dispatch_memory_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    dispatch_memory_tools_inner(tool_name, cg, args, options).await
}

fn dispatch_memory_tools_inner<'a>(
    tool_name: &'a str,
    cg: &'a TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + 'a>> {
    // Erase the deeply nested match-arm futures before they reach the
    // measured wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        match tool_name {
            "tracedecay_automation_run_list" => automation_runs::handle_list(cg, args).await,
            "tracedecay_automation_run_view" => automation_runs::handle_view(cg, args).await,
            "tracedecay_automation_run_artifact_view" => {
                skills::handle_automation_run_artifact_view(cg, args).await
            }
            "tracedecay_analytics" => {
                dispatch_controls::dispatch_analytics(cg, args, options).await
            }
            "tracedecay_skill_list" => {
                skills::handle_skill_list(cg, args, options.accounting_db).await
            }
            "tracedecay_skill_view" => {
                skills::handle_skill_view(cg, args, options.accounting_db).await
            }
            "tracedecay_hermes_skill_bridge" => skills::handle_hermes_skill_bridge(cg, &args),
            _ => Err(unknown_tool_error(tool_name)),
        }
    })
}

/// Dispatch dashboard and workflow tools that have not moved to a dedicated
/// application family.
#[hotpath::measure(future = true, label = "mcp.dispatch.session_workflow")]
pub(super) async fn dispatch_session_workflow_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    dispatch_session_workflow_tools_inner(tool_name, cg, args, options).await
}

fn dispatch_session_workflow_tools_inner<'a>(
    tool_name: &'a str,
    cg: &'a TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + 'a>> {
    // Erase the deeply nested match-arm futures before they reach the
    // measured wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
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
                workflow::handle_run_affected_tests(
                    cg,
                    admitted_graph_query(cg, &options, "file_dependents"),
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
                    options.registered_profile_session_db.clone(),
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
                    options.pr_autotrack_reader.clone(),
                    options.diagnostics_lsp.clone(),
                    options.dashboard_application_invocation_executor.clone(),
                    options.dashboard_delivery_settlement_authority.clone(),
                    options.daemon_invocation_service.cloned(),
                )
                .await
            }
            _ => Err(unknown_tool_error(tool_name)),
        }
    })
}
