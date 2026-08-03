//! MCP tool call handlers.
//!
//! Each `handle_*` function implements one MCP tool: it deserializes
//! the JSON arguments, calls the appropriate `TraceDecay` method, and
//! formats the result.

mod admin_cli;
pub(crate) use admin_cli::handle_projectless_admin_cli;
pub(crate) use hook_runtime::{
    HookV2AdmissionOutcomeV1, admit_hook_v2_envelope, handle_projectless_hook_runtime,
    hook_v2_pending_work_envelopes, replay_projectless_hermes_host_admission,
};
mod admin_project;
pub mod analysis;
mod analytics;
mod application_surface;
pub mod ast_grep_search;
pub mod dashboard;
mod dependency_hints;
mod dispatch_groups;
pub mod edit;
pub mod git;
pub mod graph;
pub mod grep;
pub mod health;
pub mod hook_runtime;
pub mod info;
mod lcm_tool_entry;
pub mod memory;
mod project_registry;
pub mod redundancy;
mod retained_catalog;
pub mod session;
mod session_authorities;
pub mod skills;
mod support;
mod tool_call_support;
pub mod workflow;
mod workflow_index;
pub mod workflow_query;
pub(crate) use project_registry::{
    ProjectRegistryContextCommand, ProjectRegistryContextFuture, ProjectRegistryContextOutcome,
    ProjectRegistryContextView, ProjectRegistryListingCommand, ProjectRegistryListingFuture,
    ProjectRegistryListingOutcome, ProjectRegistryListingScope, ProjectRegistryListingView,
    ProjectRegistryReadPort, ProjectRegistrySelector,
};
pub(crate) use session::message_search::{
    LcmDescribeServiceCommand, LcmDescribeServiceFuture, LcmDescribeServiceOutcome,
    LcmExpandServiceCommand, LcmExpandServiceFuture, LcmExpandServiceOutcome,
    SessionRetrievalCommand, SessionRetrievalExplanationView, SessionRetrievalPageView,
    SessionRetrievalServiceFuture, SessionRetrievalServiceOutcome, SessionRetrievalServicePort,
    SessionRetrievalStoreScope, SessionRetrievalUnavailable, SessionRetrievalUnavailableReason,
    SessionRetrievalWorkerBlocker, SessionRetrievalWorkerRetryClass,
    SessionRetrievalWorkerStatusView, SessionTemporalMetadataView, SessionTemporalWatermarksView,
};
pub(crate) use session::{
    SessionRefreshAction, SessionRefreshCommand, SessionRefreshCoverageView,
    SessionRefreshFrontierView, SessionRefreshProgressView, SessionRefreshReceiptView,
    SessionRefreshServiceOutcome, SessionRefreshServicePort, utc_micros_value,
};

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
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod tool_definition_tests;

pub use lcm_tool_entry::handle_user_lcm_tool;
pub(crate) use lcm_tool_entry::{
    handle_user_lcm_tool_with_db, handle_user_lcm_tool_with_retained_authority,
};
pub use session_authorities::SessionAuthorities;
use std::path::Path;
use std::sync::Arc;
pub(crate) use tool_call_support::selected_registered_project_reader;
pub(super) use tool_call_support::{json_result, text_tool_result};

use serde_json::{Value, json};
use tracedecay_application::RetainedSurfaceOperation;
#[cfg(test)]
use tracedecay_application::{
    APPLICATION_DEFAULT_PROFILE_ID, retained_surface_application_operation,
};
use tracedecay_tool_catalog::BindingSurface;
#[cfg(test)]
use tracedecay_tool_catalog::{ProfileId, SurfaceOperationName};

#[cfg(test)]
use crate::application_surface::APPLICATION_SURFACE_OPERATIONS;
use crate::application_surface::{ApplicationSurfaceOperation, resolve_catalog_tool_binding};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::tracedecay::TraceDecay;
#[cfg(test)]
use retained_catalog::retained_mcp_composition;
use tracedecay_sessions::WorkflowIndexReadPort;

use super::binding::{
    McpToolDispatchGroup, dispatch_group_for_tool, tool_accepts_registered_project_selector,
    tool_dispatches_registered_project_reader,
};
use super::{LegacyToolCompatibilityOwner, ToolResult};
#[cfg(test)]
use dispatch_groups::dispatch_memory_operation;
pub(crate) use dispatch_groups::tool_dispatch_ceiling;
use dispatch_groups::{
    dispatch_admin_tools, dispatch_analysis_tools, dispatch_application_surface_tools,
    dispatch_edit_tools, dispatch_git_tools, dispatch_graph_tools, dispatch_health_tools,
    dispatch_info_tools, dispatch_memory_tools, dispatch_retained_application_tools,
    dispatch_session_workflow_tools,
};
use retained_catalog::dispatch_profile_retained_application_tool;
pub(crate) use tool_call_support::INTERNAL_DAEMON_TOOL_NAMES;
use tool_call_support::{boxed_send, rejected_tool_project_selector_present};

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
    Box::pin(handle_tool_call_with_registry_and_implicit_project(
        cg,
        tool_name,
        args,
        server_stats,
        scope_prefix,
        ToolCallRegistryOptions::default(),
    ))
    .await
}

#[derive(Clone)]
pub struct ToolCallRegistryOptions<'a> {
    pub(crate) global_db: Option<&'a Arc<RegisteredGlobalDb>>,
    /// Daemon-owned project-registry reads. `None` is the typed
    /// missing-registry state, not an empty registry.
    pub(crate) project_registry_reads: Option<&'a dyn ProjectRegistryReadPort>,
    /// Daemon-owned workflow-index reads. `None` is an unavailable retained
    /// project-session authority, not a successful empty index.
    pub workflow_index_reads: Option<&'a dyn WorkflowIndexReadPort>,
    pub(crate) accounting_db: Option<&'a crate::global_db::RegisteredGlobalDb>,
    pub(crate) registered_project_session_db: Option<Arc<crate::global_db::RegisteredGlobalDb>>,
    pub(crate) registered_savings_db: Option<Arc<crate::global_db::RegisteredGlobalDb>>,
    pub profile_root: Option<&'a Path>,
    pub implicit_project_path: Option<&'a Path>,
    pub automation_scheduler_reconciler: Option<crate::dashboard::AutomationSchedulerReconciler>,
    pub automation_writer: crate::dashboard::DashboardAutomationWriter,
    pub(crate) doctor_report_reader: Option<crate::dashboard::DoctorReportReader>,
    pub doctor_remediation_dispatcher: Option<crate::dashboard::DoctorRemediationDispatcherV1>,
    pub(crate) code_index_freshness_reader:
        Option<crate::dashboard::code_index_freshness_api::CodeIndexFreshnessReader>,
    pub feedback_status_reader: Option<crate::dashboard::feedback_api::FeedbackStatusReader>,
    pub diagnostics_cache: Option<&'a crate::diagnostics::DiagnosticsCache>,
    pub diagnostics_lsp:
        Option<Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>>,
    pub application_invocation_executor:
        Option<&'a dyn crate::daemon_client::DaemonInvocationExecutor>,
    pub dashboard_application_invocation_executor:
        Option<Arc<dyn crate::daemon_client::DaemonInvocationExecutor>>,
    pub application_request_id: Option<tracedecay_application::RequestId>,
    pub application_deadline: Option<tracedecay_application::Deadline>,
    pub application_cancellation: Option<tracedecay_application::CancellationSignal>,
    pub application_invocation_target: tracedecay_application::InvocationTarget,
    /// The code-index generation authority producers resolve identity through.
    pub code_index_publication_identity:
        Option<crate::mcp::server::CodeIndexPublicationIdentityResolver>,
    pub(crate) code_index_search_executor: Option<crate::mcp::server::CodeIndexSearchExecutor>,
    pub(crate) source_edit_executor: Option<crate::mcp::server::SourceEditExecutor>,
    pub(crate) source_edit_reconciliation_executor:
        Option<crate::mcp::server::SourceEditReconciliationExecutor>,
    pub(crate) code_index_search_authority: Option<crate::mcp::server::CodeIndexSearchAuthorityV1>,
    pub(crate) retained_project_graph_resolver:
        Option<crate::mcp::server::RetainedProjectGraphResolver>,
    pub preselected_project_reader: bool,
    pub session_authorities: SessionAuthorities<'a>,
}

impl Default for ToolCallRegistryOptions<'_> {
    fn default() -> Self {
        Self {
            global_db: None,
            project_registry_reads: None,
            workflow_index_reads: None,
            accounting_db: None,
            registered_project_session_db: None,
            registered_savings_db: None,
            profile_root: None,
            implicit_project_path: None,
            automation_scheduler_reconciler: None,
            automation_writer: crate::dashboard::standalone_dashboard_automation_writer(),
            doctor_report_reader: None,
            doctor_remediation_dispatcher: None,
            code_index_freshness_reader: None,
            feedback_status_reader: None,
            diagnostics_cache: None,
            diagnostics_lsp: None,
            application_invocation_executor: None,
            dashboard_application_invocation_executor: None,
            application_request_id: None,
            application_deadline: None,
            application_cancellation: None,
            application_invocation_target: tracedecay_application::InvocationTarget::CurrentProject,
            code_index_publication_identity: None,
            code_index_search_executor: None,
            source_edit_executor: None,
            source_edit_reconciliation_executor: None,
            code_index_search_authority: None,
            retained_project_graph_resolver: None,
            preselected_project_reader: false,
            session_authorities: SessionAuthorities::default(),
        }
    }
}

impl<'a> ToolCallRegistryOptions<'a> {
    pub fn with_session_authorities(session_authorities: SessionAuthorities<'a>) -> Self {
        Self {
            session_authorities,
            ..Self::default()
        }
    }
}

pub fn handle_tool_call_with_registry_and_implicit_project<'a>(
    cg: &'a TraceDecay,
    tool_name: &'a str,
    mut args: Value,
    server_stats: Option<Value>,
    scope_prefix: Option<&'a str>,
    options: ToolCallRegistryOptions<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + 'a>> {
    Box::pin(async move {
        for removed in ["hermes_home"] {
            if args.get(removed).is_some() {
                return Err(TraceDecayError::Config {
                    message: format!("unknown parameter `{removed}` for `{tool_name}`"),
                });
            }
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
                    if RetainedSurfaceOperation::from_name(tool_name).is_some()
                        || tool_name == "tracedecay_message_search"
                    {
                        ensure_mcp_dispatch_available(tool_name)?;
                    }
                    let profile_root = match options.profile_root {
                        Some(profile_root) => profile_root.to_path_buf(),
                        None => support::profile_root_for_global_db(
                            options.global_db.map(std::sync::Arc::as_ref),
                        )?,
                    };
                    if let Some(operation) = RetainedSurfaceOperation::from_name(tool_name) {
                        let dispatch: std::pin::Pin<
                            Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + '_>,
                        > = Box::pin(dispatch_profile_retained_application_tool(
                            operation,
                            tool_name,
                            args,
                            &profile_root,
                            options.clone(),
                        ));
                        return dispatch.await;
                    }
                    let dispatch: std::pin::Pin<
                        Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + '_>,
                    > = Box::pin(handle_user_lcm_tool_with_db(
                        tool_name,
                        args,
                        &profile_root,
                        options.session_authorities.user,
                        options.global_db.map(std::sync::Arc::as_ref),
                        options.session_authorities.profile_retrieval,
                    ));
                    return dispatch.await;
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
        if !tool_accepts_registered_project_selector(tool_name)
            && rejected_tool_project_selector_present(tool_name, &args)
        {
            return Err(TraceDecayError::Config {
                message: format!(
                    "{tool_name} is scoped to the active project and does not accept project selectors"
                ),
            });
        }
        if let Some(project_path) = options.implicit_project_path
            && tool_dispatches_registered_project_reader(tool_name)
            && !rejected_tool_project_selector_present(tool_name, &args)
            && let Some(map) = args.as_object_mut()
        {
            map.insert(
                "project_path".to_string(),
                json!(project_path.to_string_lossy().to_string()),
            );
        }
        let selected_project = if options.preselected_project_reader {
            None
        } else {
            boxed_send(selected_registered_project_reader(
                tool_name.to_owned(),
                args.clone(),
                options.global_db.map(std::sync::Arc::as_ref),
                options.retained_project_graph_resolver.clone(),
            ))
            .await?
        };
        let selected_scope_prefix =
            if options.preselected_project_reader || selected_project.is_some() {
                None
            } else {
                scope_prefix
            };
        let cg = selected_project
            .as_ref()
            .map_or(cg, |selected| selected.graph.as_ref());
        let active_project_session_db = (!options.preselected_project_reader
            && selected_project.is_none())
        .then(|| {
            options
                .registered_project_session_db
                .as_ref()
                .or(options.session_authorities.project)
        })
        .flatten();
        let active_lcm_context = session::LcmHandlerContext::active(
            cg,
            active_project_session_db,
            options.session_authorities.project_retrieval,
        );
        // Classify before moving `args` so large payloads are not cloned into every
        // group probe. Application-surface tools still run before catalog checks;
        // `tracedecay_diagnostics` without an executor falls through to the
        // analysis group, whose binding row routes it to the local handler.
        let dispatch_group = classify_mcp_tool_dispatch_group(
            tool_name,
            options.application_invocation_executor.is_some(),
        );
        if dispatch_group == Some(McpToolDispatchGroup::ApplicationSurface) {
            // Application-surface tools return before the root guard below.
            // Reject unavailable effects before parsing, routing, or invoking
            // the canonical application handler.
            ensure_mcp_dispatch_available(tool_name)?;
            return boxed_send(dispatch_application_surface_tools(
                tool_name,
                cg,
                args,
                options.clone(),
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
        if !LegacyToolCompatibilityOwner::admits(tool_name)
            && !INTERNAL_DAEMON_TOOL_NAMES.contains(&tool_name)
        {
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
        let dispatched = async {
            match dispatch_group {
                Some(McpToolDispatchGroup::Graph) => {
                    boxed_send(dispatch_graph_tools(
                        tool_name,
                        cg,
                        args,
                        selected_scope_prefix,
                        options.code_index_search_executor.as_ref(),
                        options.code_index_search_authority.as_ref(),
                        options.application_deadline.clone(),
                        options.application_cancellation.clone(),
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
                        active_project_session_db,
                        options.clone(),
                    ))
                    .await
                }
                Some(McpToolDispatchGroup::Admin) => {
                    boxed_send(dispatch_admin_tools(tool_name, cg, args, options.clone())).await
                }
                Some(McpToolDispatchGroup::Analysis) => {
                    boxed_send(dispatch_analysis_tools(
                        tool_name,
                        cg,
                        args,
                        scope_prefix,
                        active_project_session_db,
                        options.clone(),
                    ))
                    .await
                }
                Some(McpToolDispatchGroup::Git) => {
                    boxed_send(dispatch_git_tools(tool_name, cg, args, options.clone())).await
                }
                Some(McpToolDispatchGroup::Edit) => {
                    boxed_send(dispatch_edit_tools(tool_name, cg, args, options.clone())).await
                }
                Some(McpToolDispatchGroup::Health) => {
                    boxed_send(dispatch_health_tools(
                        tool_name,
                        cg,
                        args,
                        scope_prefix,
                        active_project_session_db,
                        options.clone(),
                    ))
                    .await
                }
                Some(McpToolDispatchGroup::RetainedApplication) => {
                    boxed_send(dispatch_retained_application_tools(
                        tool_name,
                        cg,
                        args,
                        scope_prefix,
                        active_project_session_db,
                        active_lcm_context,
                        options.clone(),
                    ))
                    .await
                }
                Some(McpToolDispatchGroup::Memory) => {
                    boxed_send(dispatch_memory_tools(tool_name, cg, args, options.clone())).await
                }
                Some(McpToolDispatchGroup::SessionWorkflow) => {
                    boxed_send(dispatch_session_workflow_tools(
                        tool_name,
                        cg,
                        args,
                        options.clone(),
                    ))
                    .await
                }
                // Application-surface tools already returned above; reaching here means
                // the name resolves to no reachable dispatch entry.
                Some(McpToolDispatchGroup::ApplicationSurface) | None => {
                    Err(unknown_tool_error(tool_name))
                }
            }
        };
        match tokio::time::timeout(dispatch_budget, dispatched).await {
            Ok(result) => result,
            Err(_elapsed) => Err(dispatch_groups::tool_dispatch_deadline_error(
                tool_name,
                dispatch_budget,
            )),
        }
    })
}

/// The single rejection every dispatch group returns for a name it does not own.
fn unknown_tool_error(tool_name: &str) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("unknown tool: {tool_name}"),
    }
}

/// The `diagnostics_read` name that still carries the pre-application argument
/// shape, and so the only one [`dispatch_analysis_tools`] can serve in-process.
const DIAGNOSTICS_COMPATIBILITY_TOOL: &str = "tracedecay_diagnostics";

fn classify_mcp_tool_dispatch_group(
    tool_name: &str,
    application_invocation_executor_available: bool,
) -> Option<McpToolDispatchGroup> {
    if let Some(operation) = ApplicationSurfaceOperation::from_tool_name(tool_name) {
        // `DiagnosticsRead` answers to two tool names. Only the compatibility
        // name has an in-process analysis handler that accepts its arguments,
        // so only that name is deferred when no executor is attached.
        // `tracedecay_diagnostics_read` stays on the surface, which reports the
        // transport as unavailable rather than failing as an unknown tool.
        let defer_diagnostics_without_executor = operation
            == ApplicationSurfaceOperation::DiagnosticsRead
            && tool_name == DIAGNOSTICS_COMPATIBILITY_TOOL
            && !application_invocation_executor_available;
        if !defer_diagnostics_without_executor {
            return Some(McpToolDispatchGroup::ApplicationSurface);
        }
    }
    if let Some(group) = dispatch_group_for_tool(tool_name) {
        return Some(group);
    }
    RetainedSurfaceOperation::from_name(tool_name)
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
