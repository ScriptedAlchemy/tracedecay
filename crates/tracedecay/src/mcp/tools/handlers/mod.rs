//! MCP tool call handlers.
//!
//! Each `handle_*` function implements one MCP tool: it deserializes
//! the JSON arguments, calls the appropriate `TraceDecay` method, and
//! formats the result.

mod application_surface;
pub(crate) use application_surface::graph_tool_error_problem;
pub use application_surface::{
    GraphToolOutcome, RetainedSurfaceExecution, command_refusal_document,
    execute_graph_tool_surface, execute_retained_surface_tool, handle_application_surface,
    render_application_surface_result, render_retained_execution, render_settled_route_refusal,
    retained_tool_target, run_retained_surface_tool, tool_refusal_response,
};
pub(crate) use dispatch_groups::compute_graph_tool_for_owner;
pub use support::{registered_project_not_found, registered_project_selector_id};
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod configuration_batch_behavior_tests;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod configuration_dispatch_tests;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod context_scout_control_dispatch_tests;
pub mod dashboard;
mod dispatch_controls;
mod dispatch_groups;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod dispatch_test_support;
#[cfg(test)]
pub(crate) use dispatch_test_support::dispatch_on_graph_authority;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod dispatch_tests;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod graph_search_dispatch_tests;
pub mod info;
pub(crate) mod retained_catalog;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod retained_timeout_dispatch_tests;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod runtime_behavior_dispatch_tests;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod runtime_generation_census_dispatch_tests;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod search_graph_independence_tests;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod stack_snapshot_behavior_tests;
mod support;
mod tool_call_support;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod tool_definition_tests;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod verified_graph_query_authority_tests;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod work_dispatch_tests;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod workflow_dispatch_tests;

use std::sync::Arc;
pub(crate) use tool_call_support::resolve_registered_project_route_for_tool;

use serde_json::Value;
use tracedecay_contracts::retrieval::ServedCodeGraphGenerationV1;
use tracedecay_contracts::{InvocationTarget, RetainedSurfaceOperation};
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use dispatch_groups::dispatch_application_surface_tools;
use tool_call_support::{boxed_send, rejected_tool_project_selector_present};
use tracedecay_api::{WorkHttpRequest, WorkflowHttpRequest};
use tracedecay_daemon_protocol::{ApplicationSurfaceAdapterError, DaemonInvocationExecutor};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_mcp::ToolResult;
use tracedecay_mcp::handlers::{SessionAuthorities, unknown_tool_error};
use tracedecay_mcp::tools::binding::{
    INTERNAL_DAEMON_TOOL_NAMES, McpToolDispatchGroup, dispatch_group_for_tool,
    mcp_dispatch_contract, tool_accepts_registered_project_selector,
    tool_dispatches_registered_project_reader,
};
use tracedecay_mcp::tools::dispatch::McpDispatchMetadataError;
use tracedecay_mcp::{handle_multi_root, handle_work, handle_workflow};
use tracedecay_project::project::TraceDecay;
use tracedecay_runtime_core::storage::registered_project_id;

fn ensure_mcp_dispatch_available(tool_name: &str) -> Result<()> {
    if INTERNAL_DAEMON_TOOL_NAMES.contains(&tool_name) {
        return Ok(());
    }
    let contract = mcp_dispatch_contract(tool_name).map_err(|error| match error {
        McpDispatchMetadataError::MissingContract(_) => unknown_tool_error(tool_name),
        error => TraceDecayError::Config {
            message: error.to_string(),
        },
    })?;
    if let tracedecay_tool_catalog::McpDispatchAvailability::Unavailable { reason, retryable } =
        contract.availability()
    {
        return Err(TraceDecayError::project_route(
            match reason {
                tracedecay_tool_catalog::McpDispatchUnavailableReason::EffectJourneyUnverified => {
                    "mcp_dispatch_effect_journey_unverified"
                }
            },
            *retryable,
            format!(
                "MCP tool '{tool_name}' is advertised but unavailable until its effect journey is verified"
            ),
        ));
    }
    Ok(())
}

/// Dispatches a tool call to the appropriate handler.
///
/// Returns the tool result and touched file paths, or an error if the tool
/// name is unknown or the handler fails.
pub async fn handle_tool_call(cg: &TraceDecay, tool_name: &str, args: Value) -> Result<ToolResult> {
    Box::pin(handle_tool_call_with_registry_options(
        cg,
        tool_name,
        args,
        ToolCallRegistryOptions::default().admit_opened_project(cg)?,
    ))
    .await
}

/// Fixture `handle_tool_call` derives the checkout the opened project already
/// holds so integration tests get an admitted snapshot. Production dispatch
/// carries `admitted_project_scope` from project-open; without it the root
/// fails closed.
pub(crate) fn opened_project_scope(cg: &TraceDecay) -> Result<tracedecay_contracts::ResolvedScope> {
    let project_id = registered_project_id(cg.store_layout())?;
    tracedecay_code_index_runtime::resolved_scope_for_project(cg.project_root(), &project_id)
        .map_err(|error| {
            TraceDecayError::project_route(
                "admitted_project_scope_unresolved",
                false,
                error.to_string(),
            )
        })
}

/// The code-graph generation one tool call served, reported by the single
/// verified-graph open funnel. A stale seat replaces an earlier current one,
/// so a later stale open is never hidden behind the first.
#[derive(Clone, Default)]
pub(crate) struct ServedCodeGraphSlot(Arc<std::sync::Mutex<Option<ServedCodeGraphGenerationV1>>>);

impl ServedCodeGraphSlot {
    pub(crate) fn record(&self, served: ServedCodeGraphGenerationV1) {
        let mut slot = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot
            .as_ref()
            .is_none_or(|recorded| !recorded.freshness.is_stale())
        {
            *slot = Some(served);
        }
    }

    pub(crate) fn served(&self) -> Option<ServedCodeGraphGenerationV1> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[derive(Clone)]
pub struct ToolCallRegistryOptions<'a> {
    pub(crate) global_db: Option<&'a RegisteredGlobalDbLeaseV1>,
    /// The serving MCP server's request counters, which the graph-tool owner
    /// reports in `tracedecay_status`.
    pub(crate) server_stats: Option<Value>,
    pub(crate) accounting_db: Option<&'a tracedecay_global_db::RegisteredGlobalDb>,
    pub(crate) registered_project_session_db:
        Option<tracedecay_global_db::RegisteredGlobalDbLeaseV1>,
    pub(crate) registered_profile_session_db:
        Option<tracedecay_global_db::RegisteredGlobalDbLeaseV1>,
    pub(crate) registered_savings_db: Option<tracedecay_global_db::RegisteredGlobalDbLeaseV1>,
    pub(crate) dashboard_session_retrieval_service: Option<
        Arc<dyn tracedecay_session_runtime::session_retrieval::SessionApplicationRetrievalPortV1>,
    >,
    pub(crate) dashboard_session_retrieval_identity:
        Option<tracedecay_session_memory::context::ResolvedSessionIdentity>,
    /// The canonical profile identity bound by the daemon handshake. A
    /// dashboard profile write resolves its configuration layer through this
    /// identity, so it must not be derived from the project-session store,
    /// that authority mounts behind the core project-open publication and is
    /// absent on the core server that answers the first tool calls.
    pub(crate) daemon_user_profile_id: Option<tracedecay_domain::configuration::UserProfileId>,
    /// The daemon owner profile the tool call runs for, when daemon-owned.
    pub profile: Option<&'a tracedecay_runtime_core::config::ProfileRoot>,
    pub(crate) resolved_project_route: Option<&'a crate::mcp::project_route::ResolvedProjectRoute>,
    pub automation_scheduler_reconciler:
        Option<tracedecay_dashboard_api::AutomationSchedulerReconciler>,
    pub automation_writer: tracedecay_dashboard_api::DashboardAutomationWriter,
    pub(crate) doctor_report_reader: Option<tracedecay_dashboard_api::DoctorReportReader>,
    pub(crate) remote_operational_status:
        Option<tracedecay_contracts::RemoteOperationalStatusReaderV1>,
    pub(crate) code_index_freshness_reader:
        Option<tracedecay_contracts::code_index_freshness::CodeIndexFreshnessReader>,
    pub(crate) code_index_readiness_waiter:
        Option<tracedecay_contracts::code_index_freshness::CodeIndexReadinessWaiter>,
    pub feedback_status_reader:
        Option<tracedecay_dashboard_api::feedback_api::FeedbackStatusReader>,
    pub(crate) pr_autotrack_reader:
        Option<tracedecay_dashboard_api::PrAutoTrackManagedSummaryReader>,
    pub diagnostics_lsp:
        Option<Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>>,
    pub application_invocation_executor:
        Option<&'a dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
    pub dashboard_application_invocation_executor:
        Option<Arc<dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>>,
    pub(crate) daemon_invocation_service:
        Option<&'a tracedecay_daemon_service::DaemonInvocationService>,
    pub(crate) dashboard_delivery_settlement_authority:
        Option<Arc<tracedecay_application::observability::DeliverySettlementAuthorityV1>>,
    pub application_request_id: Option<tracedecay_contracts::RequestId>,
    pub application_deadline: Option<tracedecay_contracts::Deadline>,
    pub application_cancellation: Option<tracedecay_contracts::CancellationSignal>,
    pub application_invocation_target: tracedecay_contracts::InvocationTarget,
    /// The code-index generation authority producers resolve identity through.
    pub code_index_publication_identity:
        Option<crate::mcp::server::CodeIndexPublicationIdentityResolver>,
    pub(crate) code_index_reconcile_sink: Option<crate::mcp::server::CodeIndexReconcileSink>,
    pub(crate) code_index_search_executor:
        Option<tracedecay_query::code_search::CodeIndexSearchExecutor>,
    pub(crate) code_index_similar_executor:
        Option<tracedecay_query::code_search::CodeIndexSimilarExecutor>,
    pub(crate) code_index_redundancy_executor:
        Option<tracedecay_query::code_search::CodeIndexRedundancyExecutor>,
    pub(crate) code_index_branch_diff_executor:
        Option<tracedecay_query::code_search::CodeIndexBranchDiffExecutor>,
    pub(crate) code_index_search_authority:
        Option<tracedecay_query::code_search::CodeIndexSearchAuthorityV1>,
    /// The checkout the serving route was admitted for. Every scoped authority
    /// a moved handler family reads binds against this one scope; absent, no
    /// scoped authority may be admitted at all.
    pub(crate) admitted_project_scope: Option<tracedecay_contracts::ResolvedScope>,
    pub(crate) code_graph_projection_read_port:
        Option<crate::mcp::server::CodeGraphProjectionReadPort>,
    pub(crate) code_graph_read_admission_port:
        Option<crate::mcp::server::CodeGraphReadAdmissionPort>,
    pub(crate) verified_graph_query_port:
        Option<std::sync::Arc<dyn tracedecay_graph_query::VerifiedGraphQueryPort + 'static>>,
    pub(crate) code_index_ignored_dependency_admission:
        Option<crate::mcp::server::CodeIndexIgnoredDependencyAdmissionPort>,
    /// Exact-scope sealed-generation census authority for runtime telemetry.
    pub(crate) generation_census_reader:
        Option<tracedecay_runtime_core::runtime_telemetry::GenerationCensusReader>,
    /// Retained server authority consumed by the dashboard boundary. Project
    /// selection itself is completed before handler dispatch.
    pub(crate) retained_project_server_resolver:
        Option<crate::mcp::server::RetainedProjectServerResolver>,
    /// Daemon-owned bounded native transcript and session/Git convergence.
    /// Absence is a typed unavailable authority, never a local store fallback.
    pub(crate) session_sync_service:
        Option<&'a dyn tracedecay_contracts::session_sync::SessionSyncServicePort>,
    /// Report from the single verified-graph open funnel
    /// (`dispatch_groups::admitted_graph_query`) back to the dispatch
    /// boundary: the generation a graph-backed tool answered from, so a stale
    /// seat gains the typed `code_graph_freshness` trailer. Constructed fresh
    /// per tool call.
    pub(crate) served_code_graph: ServedCodeGraphSlot,
    pub session_authorities: SessionAuthorities<'a>,
}

impl Default for ToolCallRegistryOptions<'_> {
    fn default() -> Self {
        Self {
            global_db: None,
            server_stats: None,
            accounting_db: None,
            registered_project_session_db: None,
            registered_profile_session_db: None,
            registered_savings_db: None,
            dashboard_session_retrieval_service: None,
            dashboard_session_retrieval_identity: None,
            daemon_user_profile_id: None,
            profile: None,
            resolved_project_route: None,
            automation_scheduler_reconciler: None,
            automation_writer: tracedecay_dashboard_api::standalone_dashboard_automation_writer(),
            doctor_report_reader: None,
            remote_operational_status: None,
            code_index_freshness_reader: None,
            code_index_readiness_waiter: None,
            feedback_status_reader: None,
            pr_autotrack_reader: None,
            diagnostics_lsp: None,
            application_invocation_executor: None,
            dashboard_application_invocation_executor: None,
            daemon_invocation_service: None,
            dashboard_delivery_settlement_authority: None,
            application_request_id: None,
            application_deadline: None,
            application_cancellation: None,
            application_invocation_target: tracedecay_contracts::InvocationTarget::CurrentProject,
            code_index_publication_identity: None,
            code_index_reconcile_sink: None,
            code_index_search_executor: None,
            code_index_similar_executor: None,
            code_index_redundancy_executor: None,
            code_index_branch_diff_executor: None,
            code_index_search_authority: None,
            admitted_project_scope: None,
            code_graph_projection_read_port: None,
            code_graph_read_admission_port: None,
            verified_graph_query_port: None,
            code_index_ignored_dependency_admission: None,
            generation_census_reader: None,
            retained_project_server_resolver: None,
            session_sync_service: None,
            served_code_graph: ServedCodeGraphSlot::default(),
            session_authorities: SessionAuthorities::default(),
        }
    }
}

impl ToolCallRegistryOptions<'_> {
    /// Marks this call as admitted for the opened project's checkout.
    /// Fixture `handle_tool_call` uses this; production carries the scope
    /// from project-open publication.
    pub fn admit_opened_project(mut self, cg: &TraceDecay) -> Result<Self> {
        self.admitted_project_scope = Some(opened_project_scope(cg)?);
        Ok(self)
    }
}

pub fn handle_tool_call_with_registry_options<'a>(
    cg: &'a TraceDecay,
    tool_name: &'a str,
    args: Value,
    options: ToolCallRegistryOptions<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + 'a>> {
    let dispatch = async move {
        tracing::trace!(name: "mcp.tool.name", value = ?mcp_tool_bounded_identity(tool_name));
        if args.get("hermes_home").is_some() {
            return Err(ApplicationSurfaceAdapterError::invalid_request(format!(
                "unknown parameter `hermes_home` for `{tool_name}`"
            ))
            .into_trace_decay_error());
        }
        if let Some(retained) = RetainedSurfaceOperation::from_tool_name(tool_name) {
            // A profile-targeted call names no project, so it skips project
            // selector routing and goes straight to the profile owner.
            if retained_tool_target(retained, &args)? == InvocationTarget::Profile {
                ensure_mcp_dispatch_available(tool_name)?;
                return boxed_send(dispatch_application_surface_tools(
                    tool_name, cg, args, options,
                ))
                .await;
            }
        } else if let Some(storage_scope) = args.get("storage_scope") {
            return Err(ApplicationSurfaceAdapterError::invalid_request(
                if tool_name.starts_with("tracedecay_lcm_")
                    && storage_scope.as_str() == Some("user")
                {
                    format!("storage_scope=user is unavailable for non-retained tool `{tool_name}`")
                } else {
                    format!("unknown parameter `storage_scope` for `{tool_name}`")
                },
            )
            .into_trace_decay_error());
        }
        if tool_accepts_registered_project_selector(tool_name) {
            support::validate_registered_project_selector_aliases(&args)?;
        } else if rejected_tool_project_selector_present(tool_name, &args) {
            return Err(TraceDecayError::Config {
                message: format!(
                    "{tool_name} is scoped to the active project and does not accept project selectors"
                ),
            });
        }
        if tool_dispatches_registered_project_reader(tool_name)
            && crate::mcp::project_route::arguments_have_project_selector(&args)
            && options.resolved_project_route.is_none()
        {
            return Err(TraceDecayError::project_route(
                "project_route_unavailable",
                true,
                "registered project selection was not resolved before handler dispatch",
            ));
        }
        // Classify before moving `args` so large payloads are not cloned into every
        // group probe. Application-surface tools still run before catalog checks.
        let dispatch_group = classify_mcp_tool_dispatch_group(tool_name);
        if dispatch_group == Some(McpToolDispatchGroup::CatalogDiscovery) {
            ensure_mcp_dispatch_available(tool_name)?;
            return tracedecay_mcp::tools::catalog_discovery::execute_tool_search(&args).map_err(
                |error| TraceDecayError::Config {
                    message: error.to_string(),
                },
            );
        }
        if dispatch_group == Some(McpToolDispatchGroup::ApplicationSurface) {
            // Application-surface tools return before the root guard below.
            // Reject unavailable effects before parsing, routing, or invoking
            // the canonical application handler.
            ensure_mcp_dispatch_available(tool_name)?;
            return boxed_send(dispatch_application_surface_tools(
                tool_name, cg, args, options,
            ))
            .await;
        }
        if dispatch_group == Some(McpToolDispatchGroup::MultiRoot) {
            // Multi-root tools are daemon-owned: they carry no application
            // surface binding, so they return here rather than falling through
            // to the catalog resolution below.
            ensure_mcp_dispatch_available(tool_name)?;
            return boxed_send(handle_multi_root(
                tool_name,
                args,
                options.application_invocation_executor,
                options.application_request_id,
                options.application_deadline,
                options.application_cancellation,
            ))
            .await;
        }
        if dispatch_group == Some(McpToolDispatchGroup::Work) {
            ensure_mcp_dispatch_available(tool_name)?;
            return boxed_send(execute_work_tool_surface(
                tool_name,
                args,
                options.application_invocation_executor,
                options.application_request_id,
                options.application_deadline,
                options.application_cancellation,
            ))
            .await;
        }
        if dispatch_group == Some(McpToolDispatchGroup::Workflow) {
            ensure_mcp_dispatch_available(tool_name)?;
            return boxed_send(execute_workflow_tool_surface(
                tool_name,
                args,
                options.application_invocation_executor,
                options.application_request_id,
                options.application_deadline,
                options.application_cancellation,
            ))
            .await;
        }
        // The daemon serves its internal branch-add tool before MCP dispatch;
        // every other name has returned through its typed owner above.
        Err(unknown_tool_error(tool_name))
    };
    Box::pin(tracing::Instrument::instrument(
        dispatch,
        tracing::trace_span!("mcp.tool_call"),
    ))
}

/// Runs one Work tool through the canonical Work owner on `executor`: the
/// daemon's project server for MCP, the daemon socket client for
/// `tracedecay tool`. Both surfaces therefore return the same typed envelope
/// HTTP serves, bound to the Work executable registry's result contract.
pub async fn execute_work_tool_surface(
    tool_name: &str,
    args: Value,
    executor: Option<&dyn DaemonInvocationExecutor>,
    request_id: Option<tracedecay_contracts::RequestId>,
    deadline: Option<tracedecay_contracts::Deadline>,
    cancellation: Option<tracedecay_contracts::CancellationSignal>,
) -> Result<ToolResult> {
    handle_work(
        tool_name,
        args,
        executor.map(|executor| move |request| invoke_admitted_work_operation(executor, request)),
        request_id,
        deadline,
        cancellation,
    )
    .await
}

/// Workflow's counterpart of [`execute_work_tool_surface`].
pub async fn execute_workflow_tool_surface(
    tool_name: &str,
    args: Value,
    executor: Option<&dyn DaemonInvocationExecutor>,
    request_id: Option<tracedecay_contracts::RequestId>,
    deadline: Option<tracedecay_contracts::Deadline>,
    cancellation: Option<tracedecay_contracts::CancellationSignal>,
) -> Result<ToolResult> {
    handle_workflow(
        tool_name,
        args,
        |request| invoke_admitted_workflow_operation(executor, request),
        request_id,
        deadline,
        cancellation,
    )
    .await
}

/// Reads the canonical Work HTTP envelope the daemon owner already produced.
async fn invoke_admitted_work_operation(
    executor: &dyn DaemonInvocationExecutor,
    request: WorkHttpRequest,
) -> Result<Value> {
    let response =
        tracedecay_daemon_service::application_surface::invoke_work_operation(executor, request)
            .await;
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .map_err(|error| {
            TraceDecayError::project_route(
                "work.response_unavailable",
                true,
                format!("The Work application response could not be read: {error}"),
            )
        })?;
    serde_json::from_slice(&body).map_err(|error| {
        TraceDecayError::project_route(
            "work.response_invalid",
            true,
            format!("The Work application response was not valid JSON: {error}"),
        )
    })
}

/// Reads the canonical Workflow HTTP envelope the daemon owner already produced.
async fn invoke_admitted_workflow_operation(
    executor: Option<&dyn DaemonInvocationExecutor>,
    request: WorkflowHttpRequest,
) -> Result<Value> {
    let response = tracedecay_daemon_service::application_surface::invoke_workflow_operation(
        executor, request,
    )
    .await;
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .map_err(|error| {
            TraceDecayError::project_route(
                "workflow.response_unavailable",
                true,
                format!("The Workflow application response could not be read: {error}"),
            )
        })?;
    serde_json::from_slice(&body).map_err(|error| {
        TraceDecayError::project_route(
            "workflow.response_invalid",
            true,
            format!("The Workflow application response was not valid JSON: {error}"),
        )
    })
}

fn mcp_tool_bounded_identity(tool_name: &str) -> &str {
    if RetainedSurfaceOperation::from_tool_name(tool_name).is_some()
        || classify_mcp_tool_dispatch_group(tool_name).is_some()
    {
        tool_name
    } else {
        "unknown"
    }
}

fn classify_mcp_tool_dispatch_group(tool_name: &str) -> Option<McpToolDispatchGroup> {
    if ApplicationSurfaceOperation::from_tool_name(tool_name).is_some() {
        return Some(McpToolDispatchGroup::ApplicationSurface);
    }
    dispatch_group_for_tool(tool_name)
}

/// Whether a tool is bound to the git dispatch family: the internal
/// branch-add tool, which walks git while building its branch index.
///
/// The MCP server uses this to give it the same bounded deadline the
/// catalog-owned git reads carry. Asking the canonical binding table keeps
/// that horizon from drifting into a separate name list.
pub(crate) fn tool_dispatches_git_reads(tool_name: &str) -> bool {
    dispatch_group_for_tool(tool_name) == Some(McpToolDispatchGroup::Git)
}
