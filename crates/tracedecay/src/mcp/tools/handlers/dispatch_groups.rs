use serde_json::Value;
use tracedecay_contracts::{
    ApplicationOperation, ApplicationProblem, ResultContractRef, RetainedSurfaceOperation,
};
use tracedecay_graph_query::VerifiedGraphQueryRequest;
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingSurface};

use crate::project::TraceDecay;
use tracedecay_daemon_protocol::InvocationCancellationPolicy;
use tracedecay_daemon_service::application_surface::resolve_catalog_tool_binding;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;

use tracedecay_contracts::code_index_freshness::CodeIndexFreshnessReader;
use tracedecay_contracts::doctor::SemanticOwnerStateV1;
use tracedecay_dashboard_api::AdmittedDoctorReportV1;
use tracedecay_mcp::handlers::analysis as portable_analysis;
use tracedecay_mcp::handlers::git;
use tracedecay_mcp::handlers::graph as portable_graph;
use tracedecay_mcp::handlers::info as portable_info;
use tracedecay_mcp::handlers::{
    VerifiedGraphOpenFuture, unknown_tool_error, verified_read_operation,
};
use tracedecay_mcp::{
    AdmittedCodeIndex, McpAdmittedProjectV1, McpDoctorReportV1, McpProjectIdentityV1,
    McpRequestAuthoritiesV1, McpSemanticOwnerV1, McpToolBinding, McpToolContext, RequestControls,
    ToolResult,
};
use tracedecay_runtime_core::runtime_telemetry::GenerationCensusSnapshot;

use super::ToolCallRegistryOptions;
use super::tool_call_support::handle_retrieve;
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

async fn admitted_graph_query(
    options: &ToolCallRegistryOptions<'_>,
    operation_name: &str,
) -> Result<tracedecay_graph_query::VerifiedGraphQuery> {
    admitted_graph_query_for_operation(options, verified_read_operation(operation_name)?).await
}

/// Lends the root's verified-graph admission funnel to a portable dispatch
/// table for one tool call. The borrow of `options` is the whole lifetime of
/// the table's dispatch, so every lazy open it issues reports back through
/// the same `served_stale_graph_generation` slot.
fn verified_graph_open<'o>(
    options: &'o ToolCallRegistryOptions<'_>,
) -> impl Fn(ApplicationOperation) -> VerifiedGraphOpenFuture<'o> + Sync + 'o {
    move |operation| -> VerifiedGraphOpenFuture<'o> {
        Box::pin(admitted_graph_query_for_operation(options, operation))
    }
}

async fn admitted_graph_query_for_operation(
    options: &ToolCallRegistryOptions<'_>,
    operation: ApplicationOperation,
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
            // Retrieval reads the live `TraceDecay` store directly rather than
            // a verified graph open, so it stays with the composition root.
            "tracedecay_retrieve" => handle_retrieve(cg, &args).await,
            _ => {
                portable_graph::dispatch_tool(
                    &ctx,
                    &verified_graph_open(&options),
                    tool_name,
                    args,
                    selected_scope_prefix,
                    options.code_index_ignored_dependency_admission.as_deref(),
                )
                .await
            }
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
        // Registry, status, and remote-status reads bind daemon authorities
        // only the root holds (registry port, status snapshots, remote
        // status reader, reconcile sink); the graph-backed file inspections
        // dispatch through the portable table.
        match tool_name {
            "tracedecay_remote_status" => portable_info::handle_remote_status(
                cg.project_root(),
                &args,
                options.remote_operational_status.as_ref(),
            ),
            "tracedecay_status" => {
                let project = admitted_project_authorities(cg, &options)?;
                let snapshots = admitted_status_snapshots(cg, &options).await;
                let ctx = admitted_tool_context_for(&options, &project, &snapshots)?;
                portable_info::handle_status(&ctx, args, server_stats, scope_prefix).await
            }
            "tracedecay_active_project" => {
                let project = admitted_project_authorities(cg, &options)?;
                let snapshots = AdmittedRequestSnapshotsV1::default();
                let ctx = admitted_tool_context_for(&options, &project, &snapshots)?;
                portable_info::handle_active_project(&ctx, &args, server_stats, scope_prefix).await
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
            "tracedecay_admin_sync" => {
                info::handle_admin_sync(cg, args, options.code_index_reconcile_sink.as_ref()).await
            }
            _ => {
                portable_info::dispatch_tool(
                    cg.project_root(),
                    &verified_graph_open(&options),
                    tool_name,
                    args,
                    selected_scope_prefix,
                )
                .await
            }
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
    // Erase the portable dispatch future before it reaches the measured
    // wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        portable_analysis::dispatch_tool(
            cg.project_root(),
            &verified_graph_open(&options),
            tool_name,
            args,
            scope_prefix,
        )
        .await
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
    // Erase the portable dispatch future before it reaches the measured
    // wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        let project = admitted_project_authorities(cg, &options)?;
        let snapshots = AdmittedRequestSnapshotsV1::default();
        let ctx = admitted_tool_context(&options, &project, &snapshots, None)?;
        git::dispatch_tool(&ctx, &verified_graph_open(&options), tool_name, args).await
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
/// [`admitted_tool_context`] with the registry's own freshness reader.
fn admitted_tool_context_for<'a>(
    options: &'a ToolCallRegistryOptions<'a>,
    project: &'a McpAdmittedProjectV1,
    snapshots: &'a AdmittedRequestSnapshotsV1,
) -> Result<McpToolContext<'a>> {
    admitted_tool_context(
        options,
        project,
        snapshots,
        options.code_index_freshness_reader.as_ref(),
    )
}

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

#[expect(
    clippy::too_many_lines,
    reason = "Retained-application dispatch is one name match onto the application surface."
)]
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
                let graph = admitted_graph_query(&options, "diagnostics_read").await?;
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
                    admitted_graph_query(&options, "file_dependents"),
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
