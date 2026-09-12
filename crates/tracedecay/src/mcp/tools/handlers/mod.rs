//! MCP tool call handlers.
//!
//! Each `handle_*` function implements one MCP tool: it deserializes
//! the JSON arguments, calls the appropriate `TraceDecay` method, and
//! formats the result.

mod admin_cli;
pub(crate) use admin_cli::handle_projectless_admin_cli;
pub(crate) use hook_runtime::{
    HookV2AdmissionOutcomeV1, admit_hook_v2_envelope,
    admit_hook_v2_replayed_envelope_with_lifecycle, handle_projectless_hook_runtime,
    hook_v2_pending_work_envelopes, replay_projectless_hermes_host_admission,
};
mod admin_project;
mod analytics;
mod application_surface;
mod automation_runs;
pub mod dashboard;
mod dashboard_delivery;
mod dashboard_git_correlation;
// Only reached by the test-transport dashboard git-correlation fixture
// (`dashboard::dashboard_git_correlation_read_authority_for_test`); gate it
// so the default production build does not carry it as an unused re-export.
#[cfg(feature = "test-transport")]
pub(crate) use dashboard_git_correlation::DashboardGitCorrelationReadAdapter;
mod dashboard_lcm;
// Only reached by the test-transport dashboard LCM fixture
// (`dashboard::dashboard_lcm_read_authority_for_test`); gate it so the
// default production build does not carry it as an unused re-export.
#[cfg(feature = "test-transport")]
pub(crate) use dashboard_lcm::DashboardLcmReadAdapter;
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod dispatch_tests;
pub mod edit;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod graph_search_dispatch_tests;
pub mod hook_runtime;
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
mod session_authorities;
pub mod skills;
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
pub mod workflow;
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod workflow_dispatch_tests;

pub use session_authorities::SessionAuthorities;
use std::path::Path;
use std::sync::Arc;
pub(crate) use tool_call_support::resolve_registered_project_route_for_tool;
pub(super) use tool_call_support::{json_result, text_tool_result};

use serde_json::{Value, json};
use tracedecay_contracts::RetainedSurfaceOperation;
#[cfg(test)]
use tracedecay_contracts::{
    APPLICATION_DEFAULT_PROFILE_ID, retained_surface_application_operation,
};
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingSurface};
#[cfg(test)]
use tracedecay_tool_catalog::{ProfileId, SurfaceOperationName};

use super::LegacyToolCompatibilityOwner;
use super::binding::{
    McpToolDispatchGroup, dispatch_group_for_tool, tool_accepts_registered_project_selector,
    tool_dispatches_registered_project_reader,
};
use crate::project::TraceDecay;
pub(crate) use dispatch_groups::tool_dispatch_ceiling;
use dispatch_groups::{
    dispatch_admin_tools, dispatch_analysis_tools, dispatch_application_surface_tools,
    dispatch_edit_tools, dispatch_git_tools, dispatch_graph_tools, dispatch_health_tools,
    dispatch_info_tools, dispatch_memory_tools, dispatch_retained_application_tools,
    dispatch_session_workflow_tools,
};
use retained_catalog::dispatch_profile_retained_application_tool;
#[cfg(test)]
use retained_catalog::retained_mcp_composition;
pub(crate) use tool_call_support::INTERNAL_DAEMON_TOOL_NAMES;
use tool_call_support::{boxed_send, rejected_tool_project_selector_present};
use tracedecay_api::{WorkHttpRequest, WorkflowHttpRequest};
use tracedecay_contracts::ProjectRegistryReadPort;
use tracedecay_daemon_protocol::DaemonInvocationExecutor;
use tracedecay_daemon_service::application_surface::resolve_catalog_tool_binding;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_mcp::ToolResult;
use tracedecay_mcp::handlers::unknown_tool_error;
use tracedecay_mcp::{handle_multi_root, handle_work, handle_workflow};
use tracedecay_runtime_core::storage::registered_project_id;

/// Dispatches a tool call to the appropriate handler.
///
/// Returns the tool result and touched file paths, or an error if the tool
/// name is unknown or the handler fails. The optional `server_stats` value
/// is included in `tracedecay_status` responses when provided.
fn ensure_mcp_dispatch_available(tool_name: &str) -> Result<()> {
    if INTERNAL_DAEMON_TOOL_NAMES.contains(&tool_name) {
        return Ok(());
    }
    let contract =
        super::mcp_dispatch_contract(tool_name).map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
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

pub async fn handle_tool_call(
    cg: &TraceDecay,
    tool_name: &str,
    args: Value,
    server_stats: Option<Value>,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    Box::pin(handle_tool_call_with_registry_options(
        cg,
        tool_name,
        args,
        server_stats,
        scope_prefix,
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

/// Evidence for the `code_graph_freshness` response trailer when a
/// graph-backed tool served the last complete seated generation instead of a
/// proven-current one.
#[derive(Clone, Debug)]
pub(crate) struct ServedStaleCodeGraphReadV1 {
    /// Identity of the generation that answered.
    pub(crate) generation: String,
    /// When that generation was durably sealed.
    pub(crate) sealed_at: tracedecay_domain::UtcMicros,
    /// Whether a reconcile pass or pending scheduler wake existed at open
    /// time. False means nothing is progressing: the route is stalled, not
    /// mid-rebuild, and the trailer must not claim a rebuild.
    pub(crate) rebuild_in_flight: bool,
}

#[derive(Clone)]
pub struct ToolCallRegistryOptions<'a> {
    pub(crate) global_db: Option<&'a RegisteredGlobalDbLeaseV1>,
    /// Daemon-owned project-registry reads. `None` is the typed
    /// missing-registry state, not an empty registry.
    pub(crate) project_registry_reads: Option<&'a dyn ProjectRegistryReadPort>,
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
    /// identity, so it must not be derived from the project-session store —
    /// that authority mounts behind the core project-open publication and is
    /// absent on the core server that answers the first tool calls.
    pub(crate) daemon_user_profile_id: Option<tracedecay_domain::configuration::UserProfileId>,
    pub profile_root: Option<&'a Path>,
    pub(crate) resolved_project_route: Option<&'a crate::mcp::project_route::ResolvedProjectRoute>,
    pub automation_scheduler_reconciler:
        Option<tracedecay_dashboard_api::AutomationSchedulerReconciler>,
    pub automation_writer: tracedecay_dashboard_api::DashboardAutomationWriter,
    pub(crate) doctor_report_reader: Option<tracedecay_dashboard_api::DoctorReportReader>,
    pub(crate) remote_operational_status:
        Option<tracedecay_contracts::RemoteOperationalStatusReaderV1>,
    pub(crate) code_index_freshness_reader:
        Option<tracedecay_contracts::code_index_freshness::CodeIndexFreshnessReader>,
    pub(crate) explorer_semantic_reader: Option<tracedecay_dashboard_api::ExplorerSemanticReader>,
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
    pub(crate) code_index_search_executor: Option<crate::mcp::server::CodeIndexSearchExecutor>,
    pub(crate) code_index_branch_diff_executor:
        Option<crate::mcp::server::CodeIndexBranchDiffExecutor>,
    pub(crate) code_index_search_authority: Option<crate::mcp::server::CodeIndexSearchAuthorityV1>,
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
    /// One-shot report from the single verified-graph open funnel
    /// (`dispatch_groups::admitted_graph_query`) back to the top-level
    /// dispatch boundary: set when a graph-backed tool answered from the last
    /// complete seated generation, so the response gains a typed
    /// `code_graph_freshness` trailer carrying the seat's age and whether a
    /// rebuild pass is actually in flight. Constructed fresh per tool call.
    pub(crate) served_stale_graph_generation:
        std::sync::Arc<std::sync::OnceLock<ServedStaleCodeGraphReadV1>>,
    pub session_authorities: SessionAuthorities<'a>,
}

impl Default for ToolCallRegistryOptions<'_> {
    fn default() -> Self {
        Self {
            global_db: None,
            project_registry_reads: None,
            accounting_db: None,
            registered_project_session_db: None,
            registered_profile_session_db: None,
            registered_savings_db: None,
            dashboard_session_retrieval_service: None,
            dashboard_session_retrieval_identity: None,
            daemon_user_profile_id: None,
            profile_root: None,
            resolved_project_route: None,
            automation_scheduler_reconciler: None,
            automation_writer: tracedecay_dashboard_api::standalone_dashboard_automation_writer(),
            doctor_report_reader: None,
            remote_operational_status: None,
            code_index_freshness_reader: None,
            explorer_semantic_reader: None,
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
            served_stale_graph_generation: std::sync::Arc::new(std::sync::OnceLock::new()),
            session_authorities: SessionAuthorities::default(),
        }
    }
}

impl<'a> ToolCallRegistryOptions<'a> {
    pub fn with_session_authorities(session_authorities: SessionAuthorities<'a>) -> Self {
        // Canonical session-store field is `registered_project_session_db`.
        // The helper is the one place that copies the lease out of the
        // authorities bag so dispatch never `.or()`s the two fields.
        Self {
            registered_project_session_db: session_authorities.project.cloned(),
            session_authorities,
            ..Self::default()
        }
    }

    /// Marks this call as admitted for the opened project's checkout.
    /// Fixture `handle_tool_call` uses this; production carries the scope
    /// from project-open publication.
    pub fn admit_opened_project(mut self, cg: &TraceDecay) -> Result<Self> {
        self.admitted_project_scope = Some(opened_project_scope(cg)?);
        Ok(self)
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Tool-call handling is one registry dispatch match onto the owning handler."
)]
pub fn handle_tool_call_with_registry_options<'a>(
    cg: &'a TraceDecay,
    tool_name: &'a str,
    mut args: Value,
    server_stats: Option<Value>,
    scope_prefix: Option<&'a str>,
    options: ToolCallRegistryOptions<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + 'a>> {
    #[cfg(feature = "hotpath")]
    let hotpath_tool_name = mcp_tool_hotpath_identity(tool_name);
    let dispatch = async move {
        #[cfg(feature = "hotpath")]
        hotpath::val!("mcp.tool.name").set(&hotpath_tool_name);
        for removed in ["hermes_home"] {
            if args.get(removed).is_some() {
                return Err(TraceDecayError::Config {
                    message: format!("unknown parameter `{removed}` for `{tool_name}`"),
                });
            }
        }
        if args.get("memory_scope").and_then(Value::as_str) == Some("user")
            && matches!(
                RetainedSurfaceOperation::from_tool_name(tool_name),
                Some(
                    RetainedSurfaceOperation::FactStoreAdd
                        | RetainedSurfaceOperation::FactStoreSearch
                        | RetainedSurfaceOperation::FactStoreProbe
                        | RetainedSurfaceOperation::FactStoreRelated
                        | RetainedSurfaceOperation::FactStoreReason
                        | RetainedSurfaceOperation::FactStoreContradict
                        | RetainedSurfaceOperation::FactStoreGet
                        | RetainedSurfaceOperation::FactStoreUpdate
                        | RetainedSurfaceOperation::FactStoreRemove
                        | RetainedSurfaceOperation::FactStoreSupersede
                        | RetainedSurfaceOperation::FactStoreList
                        | RetainedSurfaceOperation::FactFeedback
                        | RetainedSurfaceOperation::MemoryStatus
                )
            )
        {
            if args.get("storage_scope").is_some() {
                return Err(TraceDecayError::Config {
                    message: format!("unknown parameter `storage_scope` for `{tool_name}`"),
                });
            }
            ensure_mcp_dispatch_available(tool_name)?;
            let operation =
                RetainedSurfaceOperation::from_tool_name(tool_name).ok_or_else(|| {
                    TraceDecayError::Config {
                        message: format!("{tool_name} requires a supported retained action"),
                    }
                })?;
            return dispatch_profile_retained_application_tool(
                operation, tool_name, cg, args, options,
            )
            .await;
        }
        // A profile-scoped session refresh names its owner in the canonical
        // request; like `memory_scope=user`, that selects the profile session
        // authority and never the active project's session store.
        if crate::mcp::tools::session_refresh_profile_scope_requested(tool_name, &args) {
            if args.get("storage_scope").is_some() {
                return Err(TraceDecayError::Config {
                    message: format!("unknown parameter `storage_scope` for `{tool_name}`"),
                });
            }
            ensure_mcp_dispatch_available(tool_name)?;
            let operation = RetainedSurfaceOperation::from_tool_name(tool_name)
                .ok_or_else(|| unknown_tool_error(tool_name))?;
            return dispatch_profile_retained_application_tool(
                operation, tool_name, cg, args, options,
            )
            .await;
        }
        if let Some(storage_scope) = args.get("storage_scope").and_then(Value::as_str) {
            if !tool_name.starts_with("tracedecay_lcm_") && tool_name != "tracedecay_message_search"
            {
                return Err(TraceDecayError::Config {
                    message: format!("unknown parameter `storage_scope` for `{tool_name}`"),
                });
            }
            match storage_scope {
                "user" => {
                    // User-scoped retained/LCM calls return before the root
                    // dispatch guard below. Keep the canonical availability
                    // decision ahead of every profile handler and store effect.
                    if RetainedSurfaceOperation::from_tool_name(tool_name).is_some()
                        || tool_name == "tracedecay_message_search"
                    {
                        ensure_mcp_dispatch_available(tool_name)?;
                    }
                    if let Some(operation) = RetainedSurfaceOperation::from_tool_name(tool_name) {
                        let dispatch: std::pin::Pin<
                            Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + '_>,
                        > = Box::pin(dispatch_profile_retained_application_tool(
                            operation, tool_name, cg, args, options,
                        ));
                        return dispatch.await;
                    }
                    return Err(TraceDecayError::Config {
                        message: format!(
                            "storage_scope=user is unavailable for non-retained tool `{tool_name}`"
                        ),
                    });
                }
                "project" => {
                    if let Some(object) = args.as_object_mut() {
                        object.remove("storage_scope");
                    }
                }
                _ => {
                    return Err(TraceDecayError::Config {
                        message: "storage_scope must be one of project, user".to_string(),
                    });
                }
            }
        }
        if tool_accepts_registered_project_selector(tool_name) {
            support::validate_registered_project_selector_aliases(
                &args,
                crate::mcp::project_route::semantic_route_argument_fields(tool_name),
            )?;
        } else if rejected_tool_project_selector_present(tool_name, &args) {
            return Err(TraceDecayError::Config {
                message: format!(
                    "{tool_name} is scoped to the active project and does not accept project selectors"
                ),
            });
        }
        if tool_dispatches_registered_project_reader(tool_name)
            && crate::mcp::project_route::arguments_have_project_selector(tool_name, &args)
            && options.resolved_project_route.is_none()
        {
            return Err(TraceDecayError::project_route(
                "project_route_unavailable",
                true,
                "registered project selection was not resolved before handler dispatch",
            ));
        }
        let selected_scope_prefix = scope_prefix;
        // Classify before moving `args` so large payloads are not cloned into every
        // group probe. Application-surface tools still run before catalog checks.
        let dispatch_group = classify_mcp_tool_dispatch_group(tool_name);
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
            // Work routes through the same canonical owner as HTTP rather than
            // entering compatibility dispatch below.
            ensure_mcp_dispatch_available(tool_name)?;
            return boxed_send(handle_work(
                tool_name,
                args,
                options.application_invocation_executor.map(|executor| {
                    move |request| invoke_admitted_work_operation(executor, request)
                }),
                options.application_request_id,
                options.application_deadline,
                options.application_cancellation,
            ))
            .await;
        }
        if dispatch_group == Some(McpToolDispatchGroup::Workflow) {
            // Workflow is Work's sibling closed family and reaches the same
            // canonical owner HTTP and the CLI reach, for the same reason.
            ensure_mcp_dispatch_available(tool_name)?;
            return boxed_send(handle_workflow(
                tool_name,
                args,
                |request| {
                    invoke_admitted_workflow_operation(
                        options.application_invocation_executor,
                        request,
                    )
                },
                options.application_request_id,
                options.application_deadline,
                options.application_cancellation,
            ))
            .await;
        }
        // Catalog-declared compatibility operations must resolve the MCP binding
        // before reaching their retained typed handler. Operations without an
        // application-catalog contract remain under the explicit root MCP
        // migration owner until their family receives one.
        if let Err(error) = resolve_catalog_tool_binding(BindingSurface::Mcp, tool_name) {
            return Err(TraceDecayError::Config {
                message: error.to_string(),
            });
        }
        let compatibility_owned =
            LegacyToolCompatibilityOwner::admits(tool_name).map_err(|error| {
                TraceDecayError::project_route(
                    "mcp.catalog_discovery_unavailable",
                    false,
                    format!("MCP tool discovery is unavailable: {error}"),
                )
            })?;
        if !compatibility_owned && !INTERNAL_DAEMON_TOOL_NAMES.contains(&tool_name) {
            return Err(unknown_tool_error(tool_name));
        }
        ensure_mcp_dispatch_available(tool_name)?;
        // The universal ceiling. Every dispatch group below runs inside this one
        // bound, so a group added later inherits it without opting in and no
        // handler can be reached unbounded. Per-group wraps (git, memory) stay:
        // they report a nicer domain-shaped result and a shorter bound, and this
        // is only the backstop beneath them.
        let dispatch_budget =
            dispatch_groups::tool_dispatch_budget(tool_name, options.application_deadline.as_ref());
        let Some(dispatch_budget) = dispatch_budget else {
            // `deadline_remaining` yields `None` only for an already-elapsed
            // carried deadline, which must be rejected rather than dispatched.
            return Err(dispatch_groups::tool_dispatch_deadline_error(
                tool_name,
                std::time::Duration::ZERO,
            ));
        };
        // The lease is cloned out of `options` (one field, not the whole
        // struct) so the dispatch arms below can take `options` by value.
        let project_session_db_lease = options.registered_project_session_db.clone();
        let served_stale_graph_generation = Arc::clone(&options.served_stale_graph_generation);
        let project_session_db = project_session_db_lease.as_ref();
        let dispatched = async {
            match dispatch_group {
                Some(McpToolDispatchGroup::Graph) => {
                    boxed_send(dispatch_graph_tools(
                        tool_name,
                        cg,
                        args,
                        selected_scope_prefix,
                        options,
                    ))
                    .await
                }
                Some(McpToolDispatchGroup::Info) => {
                    boxed_send(dispatch_info_tools(
                        tool_name,
                        cg,
                        args,
                        server_stats,
                        scope_prefix,
                        selected_scope_prefix,
                        project_session_db,
                        options,
                    ))
                    .await
                }
                Some(McpToolDispatchGroup::Admin) => {
                    boxed_send(dispatch_admin_tools(tool_name, cg, args, options)).await
                }
                Some(McpToolDispatchGroup::Analysis) => {
                    boxed_send(dispatch_analysis_tools(
                        tool_name,
                        cg,
                        args,
                        scope_prefix,
                        options,
                    ))
                    .await
                }
                Some(McpToolDispatchGroup::Git) => {
                    boxed_send(dispatch_git_tools(tool_name, cg, args, options)).await
                }
                Some(McpToolDispatchGroup::Edit) => {
                    boxed_send(dispatch_edit_tools(tool_name, cg, args, options)).await
                }
                Some(McpToolDispatchGroup::Health) => {
                    boxed_send(dispatch_health_tools(
                        tool_name,
                        cg,
                        args,
                        scope_prefix,
                        project_session_db,
                        options,
                    ))
                    .await
                }
                Some(McpToolDispatchGroup::RetainedApplication) => {
                    boxed_send(dispatch_retained_application_tools(
                        tool_name,
                        cg,
                        args,
                        scope_prefix,
                        project_session_db,
                        options,
                    ))
                    .await
                }
                Some(McpToolDispatchGroup::Memory) => {
                    boxed_send(dispatch_memory_tools(tool_name, cg, args, options)).await
                }
                Some(McpToolDispatchGroup::SessionWorkflow) => {
                    boxed_send(dispatch_session_workflow_tools(
                        tool_name, cg, args, options,
                    ))
                    .await
                }
                // Typed daemon surface tools already returned above; reaching here means
                // the name resolves to no reachable dispatch entry.
                Some(
                    McpToolDispatchGroup::ApplicationSurface
                    | McpToolDispatchGroup::MultiRoot
                    | McpToolDispatchGroup::Work
                    | McpToolDispatchGroup::Workflow,
                )
                | None => Err(unknown_tool_error(tool_name)),
            }
        };
        let result = if matches!(
            dispatch_group,
            Some(McpToolDispatchGroup::RetainedApplication)
        ) || super::binding::tool_requires_canonical_effect_settlement(tool_name)
        {
            // Canonically settled effects complete their own deadline and
            // cancellation protocol before this adapter receives a terminal.
            // Dropping that terminal in the generic transport timeout would
            // erase an admitted Effect or PartialEffect receipt.
            dispatched.await
        } else {
            match tokio::time::timeout(dispatch_budget, dispatched).await {
                Ok(result) => result,
                Err(_elapsed) => Err(dispatch_groups::tool_dispatch_deadline_error(
                    tool_name,
                    dispatch_budget,
                )),
            }
        };
        match result {
            Ok(mut result) => {
                // The verified-graph open funnel reports a stale serving seat
                // through the one-shot options slot. The answer is sound for
                // that generation but may trail the live worktree, so name
                // whether source movement proved a rebuild or source currency
                // remains unverified.
                if let Some(served) = served_stale_graph_generation.get() {
                    append_code_graph_freshness(&mut result, served);
                }
                Ok(result)
            }
            Err(error) => Err(error),
        }
    };
    Box::pin(hotpath::future!(dispatch, label = "mcp.tool_call"))
}

pub(super) fn append_code_graph_freshness(
    result: &mut ToolResult,
    served: &ServedStaleCodeGraphReadV1,
) {
    let Some(content) = result
        .value
        .get_mut("content")
        .and_then(|content| content.as_array_mut())
    else {
        return;
    };
    let generation = &served.generation;
    let age = seated_generation_age_label(served.sealed_at);
    let remedy = if served.rebuild_in_flight {
        "while the code index rebuilds"
    } else {
        "while source freshness remains unverified"
    };
    content.push(json!({"type": "text", "text": format!(
        "\ncode_graph_freshness: stale — serving the last complete generation \
         {generation} (sealed {age} ago) {remedy}; results may trail the live worktree"
    )}));
}

/// Coarse human duration between a generation's seal time and now, for the
/// freshness trailer. A routine rebuild window reads in seconds or minutes; a
/// wedged route reads in hours or days.
fn seated_generation_age_label(sealed_at: tracedecay_domain::UtcMicros) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(sealed_at.0, |elapsed| elapsed.as_micros() as i64);
    let seconds = now.saturating_sub(sealed_at.0).max(0) / 1_000_000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
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

#[cfg(any(feature = "hotpath", test))]
fn mcp_tool_hotpath_identity(tool_name: &str) -> &str {
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
    if let Some(group) = dispatch_group_for_tool(tool_name) {
        return Some(group);
    }
    RetainedSurfaceOperation::from_tool_name(tool_name)
        .map(|_| McpToolDispatchGroup::RetainedApplication)
}

/// Whether a tool's dispatch resolves to the git handler family.
///
/// The MCP server uses this to give every git-walking read the same bounded
/// deadline the catalog-owned git reads already carry. Asking the canonical
/// binding table keeps that horizon from drifting into a separate name list
/// that a newly added git tool would silently miss.
pub(crate) fn tool_dispatches_git_reads(tool_name: &str) -> bool {
    dispatch_group_for_tool(tool_name) == Some(McpToolDispatchGroup::Git)
}
