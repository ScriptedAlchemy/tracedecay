use serde_json::Value;
use tracedecay_contracts::graph_tool::GraphToolResultV1;
use tracedecay_contracts::{ApplicationOperation, RetainedSurfaceOperation};
use tracedecay_graph_query::VerifiedGraphQueryRequest;
use tracedecay_runtime_core::config::ProfileRoot;
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingSurface};

use tracedecay_daemon_protocol::ApplicationSurfaceAdapterError;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_project::project::TraceDecay;

use tracedecay_contracts::code_index_freshness::{
    CodeIndexFreshnessReader, CodeIndexReadinessWaitOutcomeV1, CodeIndexReadinessWaitReadV1,
    CodeIndexReadinessWaitV1, CodeIndexWorktreeFreshnessV1,
};
use tracedecay_contracts::retrieval::{
    ActiveProjectSurfaceRequestV1, AdminCliResultV1, AdminProjectSurfaceRequestV1,
    AdminSyncSurfaceRequestV1, RemoteStatusSurfaceRequestV1, RuntimeSurfaceRequestV1,
    StatusSurfaceRequestV1,
};
use tracedecay_dashboard_api::AdmittedDoctorReportV1;
use tracedecay_mcp::handlers::health as portable_health;
use tracedecay_mcp::handlers::info as portable_info;
use tracedecay_mcp::handlers::{
    VerifiedGraphOpenFuture, decode_primitive_request, unknown_tool_error, verified_read_operation,
};
use tracedecay_mcp::tools::binding::tool_dispatches_registered_project_reader;
use tracedecay_mcp::tools::dispatch_ceiling::{tool_dispatch_budget, tool_dispatch_deadline_error};
use tracedecay_mcp::{
    AdmittedCodeIndex, McpAdmittedProjectV1, McpDoctorReportV1, McpProjectIdentityV1,
    McpRequestAuthoritiesV1, McpToolBinding, McpToolContext, RequestControls, ToolResult,
};
use tracedecay_runtime_core::runtime_telemetry::GenerationCensusSnapshot;
use tracedecay_sessions::serving::{RefreshWorkerMissing, SessionProjectionServingStatusPort};

use super::ToolCallRegistryOptions;
use super::{application_surface, dashboard, dispatch_controls, info};
use crate::mcp::project_route::mcp_analytics_session_id;
use tracedecay_mcp::handlers::{admin_cli, admin_project, edit, hook_runtime, workflow};

fn graph_read_unavailable(detail: &str) -> TraceDecayError {
    TraceDecayError::ProjectRoute {
        reason_code: "verified-code-graph-read-unavailable".to_owned(),
        retryable: false,
        detail: detail.to_owned(),
        typed_detail: None,
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
/// the same `served_code_graph` slot.
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
    // graph-backed tool in the graph/info/git groups and the graph-tool owner funnels
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
    // Every graph-backed tool funnels through this open, so this is the
    // single point that reports the served generation, and a
    // serve-old-while-rebuilding seat, back to the dispatch boundary.
    options.served_code_graph.record(
        tracedecay_contracts::retrieval::ServedCodeGraphGenerationV1 {
            generation: query.generation().as_str().to_owned(),
            freshness: query.freshness(),
        },
    );
    Ok(query)
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
    mut args: Value,
    options: ToolCallRegistryOptions<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + 'a>> {
    // Erase the deeply nested application-surface future before it reaches
    // the measured wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        let Some(operation) = ApplicationSurfaceOperation::from_tool_name(tool_name) else {
            return Err(unknown_tool_error(tool_name));
        };
        let retained = RetainedSurfaceOperation::from_application(operation).is_some();
        let source_edit = tracedecay_daemon_protocol::is_source_edit_operation(operation);
        let graph_tool = operation.is_graph_tool() || operation.is_profile_registry_read();
        // An already-elapsed carried deadline is refused before these tools
        // dispatch, exactly as their retained handlers always refused it.
        if (retained || source_edit || graph_tool)
            && tool_dispatch_budget(tool_name, options.application_deadline.as_ref()).is_none()
        {
            return Err(tool_dispatch_deadline_error(
                tool_name,
                std::time::Duration::ZERO,
            ));
        }
        if retained {
            return application_surface::run_retained_surface_tool(
                Some(&cg.store_layout().response_handle_root),
                BindingSurface::Mcp,
                operation,
                args,
                options.application_invocation_executor,
                options.application_request_id.clone(),
                options.application_deadline.clone(),
                options.application_cancellation.clone(),
            )
            .await;
        }
        if graph_tool {
            let execution = application_surface::execute_graph_tool_surface(
                BindingSurface::Mcp,
                operation,
                args.clone(),
                options.application_invocation_executor,
                options.application_request_id.clone(),
                options.application_deadline.clone(),
                options.application_cancellation.clone(),
            )
            .await?;
            let response_handle_root = Some(cg.store_layout().response_handle_root.as_path());
            return match execution {
                Ok(completion) => tracedecay_mcp::handlers::graph_tool::render_graph_tool(
                    response_handle_root,
                    &args,
                    completion,
                ),
                Err(refusal) => refusal.render(response_handle_root, &args),
            };
        }
        if source_edit {
            return edit::source_edit_tool(
                Some(&cg.store_layout().response_handle_root),
                BindingSurface::Mcp,
                operation,
                args,
                edit::SourceEditInvocationContext {
                    executor: options.application_invocation_executor,
                    target: options.application_invocation_target,
                    request_id: options.application_request_id.clone(),
                    deadline: options.application_deadline.clone(),
                    cancellation: options.application_cancellation.clone(),
                },
            )
            .await;
        }
        // Routing resolved the registered-project selector into the invocation
        // target before dispatch; the canonical request schema does not carry it.
        if tool_dispatches_registered_project_reader(tool_name)
            && let Some(arguments) = args.as_object_mut()
        {
            arguments.remove("project_selector");
        }
        let normalized_args =
            tracedecay_daemon_protocol::adapt_application_tool_request(tool_name, args)
                .map_err(ApplicationSurfaceAdapterError::into_trace_decay_error)?;
        application_surface::handle_application_surface(
            Some(&cg.store_layout().response_handle_root),
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

/// Computes one graph-tool operation for the project's graph-tool owner,
/// under the same admitted authorities and dispatch ceiling as every other
/// graph read.
pub(crate) fn compute_graph_tool_for_owner<'a>(
    cg: &'a TraceDecay,
    operation: ApplicationSurfaceOperation,
    args: Value,
    scope_prefix: Option<&'a str>,
    options: ToolCallRegistryOptions<'a>,
) -> std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<tracedecay_contracts::graph_tool::GraphToolCompletionV1>,
            > + Send
            + 'a,
    >,
> {
    Box::pin(async move {
        let tool_name = operation.mcp_tool_name();
        let Some(budget) = tool_dispatch_budget(tool_name, options.application_deadline.as_ref())
        else {
            return Err(tool_dispatch_deadline_error(
                tool_name,
                std::time::Duration::ZERO,
            ));
        };
        if operation.owner_side_effect().is_some() {
            return match tokio::time::timeout(
                budget,
                compute_owner_side_effect(cg, operation, args, &options),
            )
            .await
            {
                Ok(result) => result,
                Err(_elapsed) => Err(tool_dispatch_deadline_error(tool_name, budget)),
            };
        }
        if dispatch_controls::is_automation_read(operation) {
            return match tokio::time::timeout(
                budget,
                dispatch_controls::compute_automation_read(cg, operation, &args, &options),
            )
            .await
            {
                Ok(result) => result,
                Err(_elapsed) => Err(tool_dispatch_deadline_error(tool_name, budget)),
            };
        }
        if matches!(
            operation,
            ApplicationSurfaceOperation::Status
                | ApplicationSurfaceOperation::ActiveProject
                | ApplicationSurfaceOperation::RemoteStatus
                | ApplicationSurfaceOperation::Runtime
        ) {
            let computed = compute_project_info(cg, operation, &args, scope_prefix, &options);
            return match tokio::time::timeout(budget, computed).await {
                Ok(result) => {
                    result.map(
                        |result| tracedecay_contracts::graph_tool::GraphToolCompletionV1 {
                            result,
                            touched_files: Vec::new(),
                            code_graph: None,
                            analytics: None,
                            cost: None,
                        },
                    )
                }
                Err(_elapsed) => Err(tool_dispatch_deadline_error(tool_name, budget)),
            };
        }
        let project = admitted_project_authorities(cg, &options)?;
        let snapshots = AdmittedRequestSnapshotsV1::default();
        let freshness = graph_freshness_reader(tool_name, &options);
        let ctx = admitted_tool_context(&options, &project, &snapshots, freshness)?;
        let open = verified_graph_open(&options);
        let computed = async {
            if operation == ApplicationSurfaceOperation::Diagnose {
                let graph = admitted_graph_query(&options, "diagnostics_read").await?;
                return workflow::compute_diagnose(
                    cg,
                    &graph,
                    args,
                    options.code_index_publication_identity.as_deref(),
                )
                .await;
            }
            Box::pin(tracedecay_mcp::handlers::graph_tool::compute_graph_tool(
                &ctx,
                &open,
                operation,
                args,
                scope_prefix,
                options.code_index_ignored_dependency_admission.as_deref(),
            ))
            .await
        };
        let mut completion = match tokio::time::timeout(budget, computed).await {
            Ok(result) => result?,
            Err(_elapsed) => return Err(tool_dispatch_deadline_error(tool_name, budget)),
        };
        if !completion.result.carries_freshness_verdict() {
            completion.code_graph = options.served_code_graph.served();
        }
        Ok(completion)
    })
}

/// The project-info and runtime reads the owner answers from the daemon
/// authorities it holds: the census and readiness waiter (status), the
/// Remote Brain reader (remote status), and the doctor report and global
/// registry (runtime).
async fn compute_project_info(
    cg: &TraceDecay,
    operation: ApplicationSurfaceOperation,
    args: &Value,
    scope_prefix: Option<&str>,
    options: &ToolCallRegistryOptions<'_>,
) -> Result<GraphToolResultV1> {
    let tool_name = operation.mcp_tool_name();
    match operation {
        ApplicationSurfaceOperation::Status => {
            let request: StatusSurfaceRequestV1 = decode_primitive_request(args, tool_name)?;
            // Wait before admitting snapshots, so the payload describes the
            // worktree the wait ended on.
            let (wait, reached_freshness) = match request.wait_for {
                Some(wait_for) => {
                    let (outcome, reached) =
                        status_readiness_wait(options, cg.project_root(), wait_for).await?;
                    (Some(outcome), reached)
                }
                None => (None, None),
            };
            let project = admitted_project_authorities(cg, options)?;
            let snapshots = admitted_status_snapshots(options).await;
            let ctx = admitted_tool_context_for(options, &project, &snapshots)?;
            let session_projection = options
                .dashboard_session_retrieval_service
                .as_ref()
                .and_then(|retrieval| retrieval.projection_serving_status())
                .unwrap_or_else(|| RefreshWorkerMissing.serving_status());
            portable_info::compute_status(
                &ctx,
                &request,
                options.server_stats.clone(),
                session_projection,
                scope_prefix,
                wait,
                reached_freshness,
            )
            .await
            .map(GraphToolResultV1::Status)
        }
        ApplicationSurfaceOperation::ActiveProject => {
            let ActiveProjectSurfaceRequestV1 {} = decode_primitive_request(args, tool_name)?;
            let project = admitted_project_authorities(cg, options)?;
            let snapshots = AdmittedRequestSnapshotsV1::default();
            let ctx = admitted_tool_context_for(options, &project, &snapshots)?;
            Ok(GraphToolResultV1::ActiveProject(
                portable_info::compute_active_project(&ctx, scope_prefix).await,
            ))
        }
        ApplicationSurfaceOperation::RemoteStatus => {
            let RemoteStatusSurfaceRequestV1 {} = decode_primitive_request(args, tool_name)?;
            Ok(GraphToolResultV1::RemoteStatus(
                portable_info::read_remote_status(options.remote_operational_status.as_ref()),
            ))
        }
        ApplicationSurfaceOperation::Runtime => {
            let request: RuntimeSurfaceRequestV1 = decode_primitive_request(args, tool_name)?;
            let project = admitted_project_authorities(cg, options)?;
            let snapshots = admitted_runtime_snapshots(options, request.doctor_report).await;
            let ctx = admitted_tool_context(options, &project, &snapshots, None)?;
            portable_health::compute_runtime(
                &ctx,
                &request,
                options.global_db.map(RegisteredGlobalDbLeaseV1::as_ref),
                tracedecay_project::version::build_version()?,
            )
            .await
            .map(|runtime| GraphToolResultV1::Runtime(Box::new(runtime)))
        }
        operation => Err(unknown_tool_error(operation.mcp_tool_name())),
    }
}

/// Builds the request-scoped admitted project snapshot from the live
/// `TraceDecay` this call already holds. It must not be cached: a later
/// branch reopen swaps the served instance.
///
/// Every served route publishes a checkout at project-open. Absence is a
/// typed root failure, not a second binding shape. The session store is the
/// canonical `registered_project_session_db` lease only, never a silent
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
        cg.get_config().index_paths.clone(),
        options.registered_project_session_db.clone(),
    )
    .map_err(Into::into)
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

/// Status needs the census. Freshness is a lazy reader on the binding so
/// the handler reads it when it renders. It does not run the doctor reader:
/// that report is runtime-only.
async fn admitted_status_snapshots(
    options: &ToolCallRegistryOptions<'_>,
) -> AdmittedRequestSnapshotsV1 {
    AdmittedRequestSnapshotsV1 {
        generation_census: hotpath::future!(
            admitted_generation_census(options),
            label = "mcp.info.status.generation_census"
        )
        .await,
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
    }
}

/// Hold a status read until the project reaches the requested readiness,
/// for at most the caller's `timeout_ms`, returning the reading that reached
/// it alongside the outcome.
///
/// The budget is the caller's; a budget this call cannot live out is refused
/// rather than shortened. Cancellation ends the wait with a typed outcome and
/// dropping it leaves no state behind.
async fn status_readiness_wait(
    options: &ToolCallRegistryOptions<'_>,
    project_root: &std::path::Path,
    request: CodeIndexReadinessWaitV1,
) -> Result<(
    CodeIndexReadinessWaitOutcomeV1,
    Option<CodeIndexWorktreeFreshnessV1>,
)> {
    let budget = std::time::Duration::from_millis(request.timeout_ms);
    let dispatch_budget =
        tool_dispatch_budget("tracedecay_status", options.application_deadline.as_ref())
            .unwrap_or_default();
    if budget > dispatch_budget {
        return Err(TraceDecayError::Config {
            message: format!(
                "tracedecay_status wait_for.timeout_ms {} exceeds this call's {} ms dispatch budget",
                request.timeout_ms,
                dispatch_budget.as_millis()
            ),
        });
    }
    let Some(waiter) = options.code_index_readiness_waiter.as_ref() else {
        return Ok((
            CodeIndexReadinessWaitOutcomeV1::Unavailable {
                reason: "code_index_scheduler_authority_not_attached".to_owned(),
            },
            None,
        ));
    };
    let wait = waiter(project_root.to_path_buf(), request.state, budget);
    let cancelled = async {
        match options.application_cancellation.as_ref() {
            Some(cancellation) => cancellation.cancelled().await,
            None => std::future::pending().await,
        }
    };
    Ok(tokio::select! {
        biased;
        () = cancelled => (CodeIndexReadinessWaitOutcomeV1::Unavailable { reason: "request_cancelled".to_owned() }, None),
        read = wait => match read {
            Ok(read) => {
                let reached = match &read {
                    CodeIndexReadinessWaitReadV1::Reached { reading } => Some(reading.as_ref().clone()),
                    CodeIndexReadinessWaitReadV1::TimedOut { .. }
                    | CodeIndexReadinessWaitReadV1::Unreachable { .. } => None,
                };
                (portable_info::readiness_wait_outcome(read), reached)
            }
            Err(_) => (
                CodeIndexReadinessWaitOutcomeV1::Unavailable {
                    reason: "code_index_freshness_read_failed".to_owned(),
                },
                None,
            ),
        },
    })
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
/// Everything the family may touch, the resolved project scope, the caller's
/// deadline and cancellation, the registered project session store that
/// authenticates PR-context cursors, and the daemon-owned code-index executors
/// with the authorization proved for them, crosses into `tracedecay-mcp` as
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
        options.code_index_similar_executor.as_ref(),
        options.code_index_redundancy_executor.as_ref(),
        options.code_index_branch_diff_executor.as_ref(),
    ) {
        (Some(authority), search, similar, redundancy, branch_diff) => Some(
            AdmittedCodeIndex::new(authority, search, similar, redundancy, branch_diff)?,
        ),
        (None, None, None, None, None) => None,
        (None, _, _, _, _) => {
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
        doctor_report: match &snapshots.doctor_report {
            DoctorReportSnapshotV1::Read(report) => McpDoctorReportV1::Read(report),
            DoctorReportSnapshotV1::ReadFailed => McpDoctorReportV1::ReadFailed,
            DoctorReportSnapshotV1::NotAttached => McpDoctorReportV1::NotAttached,
        },
    };
    Ok(McpToolContext::bind(McpToolBinding { project, request })?)
}

/// One `tracedecay_admin_cli` action for the served project, under the
/// profile, session, and sync authorities this owner carries.
async fn compute_admin_cli(
    cg: &TraceDecay,
    args: &Value,
    options: &ToolCallRegistryOptions<'_>,
) -> Result<AdminCliResultV1> {
    admin_cli::compute_admin_cli(
        cg,
        decode_primitive_request(args, ApplicationSurfaceOperation::AdminCli.mcp_tool_name())?,
        options.global_db,
        options.accounting_db,
        options.profile.map(ProfileRoot::data_dir),
        options.session_authorities.clone(),
        options.session_sync_service,
        options.application_request_id.clone(),
        options.application_deadline.clone(),
        options.application_cancellation.clone(),
    )
    .await
}

/// Runs the affected tests and records the run in the project session store
/// against the session the request names.
async fn compute_run_affected_tests(
    cg: &TraceDecay,
    args: Value,
    options: &ToolCallRegistryOptions<'_>,
) -> Result<tracedecay_contracts::graph_tool::GraphToolCompletionV1> {
    let recording = options
        .registered_project_session_db
        .clone()
        .map(|store| workflow::ManagedTestRunRecording {
            store,
            session_id: mcp_analytics_session_id(&args),
        })
        .ok_or_else(|| {
            TraceDecayError::project_route(
                "runtime_mounting",
                true,
                "managed test runs are recorded in the project session store, which is still mounting",
            )
        });
    workflow::compute_run_affected_tests(
        cg,
        admitted_graph_query(options, "file_dependents"),
        args,
        recording,
        options.application_cancellation.clone(),
    )
    .await
}

/// Runs one side-effecting owner operation under the owner's admitted
/// authorities. The dashboard composes the daemon-owned readers and writers
/// this owner carries; the test run admits the verified graph to select tests;
/// admin project maintains the project's counter, registry accounting, and
/// automation scheduler; admin CLI runs the served project's profile
/// maintenance.
async fn compute_owner_side_effect(
    cg: &TraceDecay,
    operation: ApplicationSurfaceOperation,
    args: Value,
    options: &ToolCallRegistryOptions<'_>,
) -> Result<tracedecay_contracts::graph_tool::GraphToolCompletionV1> {
    let result = match operation {
        ApplicationSurfaceOperation::RunAffectedTests => {
            return compute_run_affected_tests(cg, args, options).await;
        }
        ApplicationSurfaceOperation::AdminSync => {
            let AdminSyncSurfaceRequestV1 {} =
                decode_primitive_request(&args, operation.mcp_tool_name())?;
            GraphToolResultV1::AdminSync(
                info::admin_sync(cg, options.code_index_reconcile_sink.as_ref()).await?,
            )
        }
        ApplicationSurfaceOperation::AdminCli => {
            GraphToolResultV1::AdminCli(Box::new(compute_admin_cli(cg, &args, options).await?))
        }
        ApplicationSurfaceOperation::AdminProject => {
            let request: AdminProjectSurfaceRequestV1 =
                decode_primitive_request(&args, operation.mcp_tool_name())?;
            let (Some(deadline), Some(cancellation)) = (
                options.application_deadline.clone(),
                options.application_cancellation.clone(),
            ) else {
                return Err(TraceDecayError::project_route(
                    "application_surface_controls_unavailable",
                    true,
                    "tracedecay_admin_project requires the caller's deadline and cancellation",
                ));
            };
            GraphToolResultV1::AdminProject(Box::new(
                admin_project::compute_admin_project(
                    cg,
                    request,
                    options.global_db.map(RegisteredGlobalDbLeaseV1::as_ref),
                    options.automation_scheduler_reconciler.clone(),
                    deadline,
                    cancellation,
                )
                .await?,
            ))
        }
        ApplicationSurfaceOperation::HookRuntime => GraphToolResultV1::HookRuntime(
            hook_runtime::compute_hook_runtime(
                cg,
                hook_runtime::decode_hook_runtime_request(&args)?,
                options.profile.map(ProfileRoot::data_dir),
                options.global_db.map(RegisteredGlobalDbLeaseV1::as_ref),
                options.session_authorities.clone(),
            )
            .await?,
        ),
        ApplicationSurfaceOperation::Dashboard => {
            let request = tracedecay_mcp::handlers::decode_primitive_request(
                &args,
                operation.mcp_tool_name(),
            )?;
            GraphToolResultV1::Dashboard(
                dashboard::compute_dashboard(
                    cg,
                    request,
                    options.retained_project_server_resolver.clone(),
                    options.code_graph_read_admission_port.clone(),
                    options.code_graph_projection_read_port.clone(),
                    options.registered_project_session_db.clone(),
                    options.registered_profile_session_db.clone(),
                    options.daemon_user_profile_id.clone(),
                    options.profile.cloned(),
                    options.dashboard_session_retrieval_service.clone(),
                    options.dashboard_session_retrieval_identity.clone(),
                    options.registered_savings_db.clone(),
                    options.automation_scheduler_reconciler.clone(),
                    options.automation_writer.clone(),
                    options.doctor_report_reader.clone(),
                    options.remote_operational_status.clone(),
                    options.code_index_freshness_reader.clone(),
                    options.feedback_status_reader.clone(),
                    options.pr_autotrack_reader.clone(),
                    options.diagnostics_lsp.clone(),
                    options.dashboard_application_invocation_executor.clone(),
                    options.dashboard_delivery_settlement_authority.clone(),
                    options.daemon_invocation_service.cloned(),
                )
                .await?,
            )
        }
        operation => return Err(unknown_tool_error(operation.mcp_tool_name())),
    };
    Ok(tracedecay_contracts::graph_tool::GraphToolCompletionV1 {
        result,
        touched_files: Vec::new(),
        code_graph: None,
        analytics: None,
        cost: None,
    })
}
