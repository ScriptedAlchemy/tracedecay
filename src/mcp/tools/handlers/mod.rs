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
pub mod edit;
pub mod git;
pub mod graph;
pub mod grep;
pub mod health;
pub mod hook_runtime;
pub mod info;
pub mod memory;
mod project_registry;
pub mod redundancy;
pub mod session;
pub mod skills;
mod support;
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
pub(crate) use workflow_index::{
    WorkflowAgentView, WorkflowIndexReadPort, WorkflowIndexUnavailableReason,
    WorkflowRunDetailCommand, WorkflowRunDetailFuture, WorkflowRunDetailOutcome,
    WorkflowRunDetailView, WorkflowRunListCommand, WorkflowRunListFuture, WorkflowRunListOutcome,
    WorkflowRunScope,
};

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use serde_json::{Value, json};
use tracedecay_application::handlers::CanonicalApplicationDispatcher;
use tracedecay_application::{
    APPLICATION_DEFAULT_PROFILE_ID, ApplicationOperation, RetainedSurfaceOperation,
    retained_surface_application_operation,
};
use tracedecay_tool_catalog::{BindingSurface, ProfileId, SurfaceOperationName};

#[cfg(test)]
use crate::application_surface::APPLICATION_SURFACE_OPERATIONS;
use crate::application_surface::{ApplicationSurfaceOperation, resolve_catalog_tool_binding};
use crate::catalog_composition::{ApplicationCatalogComposition, compose_application_catalog};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::mcp::response_handles::{ResponseHandleLookup, retrieve_response_handle};
use crate::tracedecay::TraceDecay;
use crate::tracedecay::current_timestamp;

pub async fn handle_user_lcm_tool(
    tool_name: &str,
    args: Value,
    profile_root: &Path,
) -> Result<crate::mcp::tools::ToolResult> {
    handle_user_lcm_tool_with_db(tool_name, args, profile_root, None, None, None).await
}

/// Projectless daemon path: retain the profile session DB and optional
/// temporal retrieval service already attached by store administration.
pub(crate) async fn handle_user_lcm_tool_with_retained_authority(
    tool_name: &str,
    args: Value,
    profile_root: &Path,
    retained_session_db: &Arc<RegisteredGlobalDb>,
    retrieval_service: Option<&dyn session::message_search::SessionRetrievalServicePort>,
) -> Result<crate::mcp::tools::ToolResult> {
    handle_user_lcm_tool_with_db(
        tool_name,
        args,
        profile_root,
        Some(retained_session_db),
        None,
        retrieval_service,
    )
    .await
}

pub(crate) async fn handle_user_lcm_tool_with_db(
    tool_name: &str,
    args: Value,
    profile_root: &Path,
    retained_session_db: Option<&Arc<RegisteredGlobalDb>>,
    _registry_db: Option<&RegisteredGlobalDb>,
    retrieval_service: Option<&dyn session::message_search::SessionRetrievalServicePort>,
) -> Result<crate::mcp::tools::ToolResult> {
    if args.get("storage_scope").and_then(Value::as_str) != Some("user") {
        return Err(TraceDecayError::Config {
            message: "projectless LCM dispatch requires storage_scope=user".to_string(),
        });
    }
    if [
        "project_id",
        "project_path",
        "project_root",
        "project_scope",
        "project_selector",
    ]
    .iter()
    .any(|key| args.get(*key).is_some())
    {
        return Err(TraceDecayError::Config {
            message:
                "storage_scope=user cannot be combined with a project selector or project_scope"
                    .to_string(),
        });
    }
    if tool_name == "tracedecay_message_search" {
        return session::message_search::handle_message_search_with_service(
            None,
            session::message_search::SessionRetrievalStoreScope::Profile,
            args,
            retrieval_service,
        )
        .await;
    }
    let sessions_db_path = crate::sessions::user_sessions_db_path(profile_root);
    let context =
        session::LcmHandlerContext::user(&sessions_db_path, retained_session_db, retrieval_service);
    dispatch_lcm_tool(tool_name, args, context).await
}

async fn dispatch_lcm_tool(
    tool_name: &str,
    args: Value,
    context: session::LcmHandlerContext<'_>,
) -> Result<crate::mcp::tools::ToolResult> {
    match tool_name {
        "tracedecay_lcm_status" => session::handle_lcm_status(context, args).await,
        "tracedecay_lcm_doctor" => session::handle_lcm_doctor(context, args).await,
        "tracedecay_lcm_load_session" => session::handle_lcm_load_session(context, args).await,
        "tracedecay_lcm_grep" => session::handle_lcm_grep(context, args).await,
        "tracedecay_lcm_describe" => session::handle_lcm_describe(context, args).await,
        "tracedecay_lcm_expand" => session::handle_lcm_expand(context, args).await,
        "tracedecay_lcm_expand_query" => session::handle_lcm_expand_query(context, args).await,
        "tracedecay_lcm_preflight" => session::handle_lcm_preflight(context, args).await,
        "tracedecay_lcm_compress" => session::handle_lcm_compress(context, args).await,
        "tracedecay_lcm_session_boundary" => {
            session::handle_lcm_session_boundary(context, args).await
        }
        _ => Err(TraceDecayError::Config {
            message: format!("unknown user-scoped LCM tool: {tool_name}"),
        }),
    }
}

/// Database authorities retained by the owning MCP server for its lifetime.
/// Hook and LCM handlers borrow these capabilities; they never rediscover or
/// reopen a session database while dispatching an action.
#[derive(Clone, Copy, Default)]
pub struct SessionAuthorities<'a> {
    pub(crate) project: Option<&'a Arc<RegisteredGlobalDb>>,
    pub(crate) user: Option<&'a Arc<RegisteredGlobalDb>>,
    pub(crate) profile_identity:
        Option<&'a crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1>,
    pub(crate) project_registered: Option<&'a crate::global_db::RegisteredGlobalDb>,
    pub(crate) profile_registered: Option<&'a crate::global_db::RegisteredGlobalDb>,
    project_refresh: Option<&'a dyn session::SessionRefreshServicePort>,
    profile_refresh: Option<&'a dyn session::SessionRefreshServicePort>,
    project_retrieval: Option<&'a dyn session::message_search::SessionRetrievalServicePort>,
    profile_retrieval: Option<&'a dyn session::message_search::SessionRetrievalServicePort>,
}

impl<'a> SessionAuthorities<'a> {
    pub(crate) const fn new(
        project: Option<&'a Arc<RegisteredGlobalDb>>,
        user: Option<&'a Arc<RegisteredGlobalDb>>,
    ) -> Self {
        Self {
            project,
            user,
            profile_identity: None,
            project_registered: None,
            profile_registered: None,
            project_refresh: None,
            profile_refresh: None,
            project_retrieval: None,
            profile_retrieval: None,
        }
    }

    pub(crate) const fn with_registered_databases(
        mut self,
        project: Option<&'a crate::global_db::RegisteredGlobalDb>,
        profile: Option<&'a crate::global_db::RegisteredGlobalDb>,
    ) -> Self {
        self.project_registered = project;
        self.profile_registered = profile;
        self
    }

    pub(crate) const fn with_profile_identity(
        mut self,
        profile_identity: Option<
            &'a crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
        >,
    ) -> Self {
        self.profile_identity = profile_identity;
        self
    }

    pub(crate) const fn with_refresh_services(
        mut self,
        project: Option<&'a dyn session::SessionRefreshServicePort>,
        profile: Option<&'a dyn session::SessionRefreshServicePort>,
    ) -> Self {
        self.project_refresh = project;
        self.profile_refresh = profile;
        self
    }

    pub(crate) const fn with_retrieval_services(
        mut self,
        project: Option<&'a dyn session::message_search::SessionRetrievalServicePort>,
        profile: Option<&'a dyn session::message_search::SessionRetrievalServicePort>,
    ) -> Self {
        self.project_retrieval = project;
        self.profile_retrieval = profile;
        self
    }

    const fn refresh_services(self) -> session::SessionRefreshServices<'a> {
        session::SessionRefreshServices::new(self.project_refresh, self.profile_refresh)
    }
}

use super::dispatch_policy::{
    tool_accepts_registered_project_selector, tool_dispatches_registered_project_reader,
};
use super::render;
use super::{LegacyToolCompatibilityOwner, ToolResult};
use support::{project_registry_context, project_selector_present};

pub(super) fn text_tool_result(text: &str) -> ToolResult {
    ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        Vec::new(),
    )
}

pub(super) fn rendered_tool_json(
    project_root: Option<&Path>,
    args: &Value,
    value: &Value,
) -> ToolResult {
    let text = render::finalize(project_root, args, value, || render::generic_md(value));
    text_tool_result(&text)
}

pub(super) fn json_result(value: &Value) -> ToolResult {
    text_tool_result(&serde_json::to_string(value).unwrap_or_default())
}

fn boxed_send<'a, T, F>(
    future: F,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>
where
    F: std::future::Future<Output = T> + Send + 'a,
{
    Box::pin(future)
}

const INTERNAL_DAEMON_TOOL_NAMES: &[&str] = &[
    "tracedecay_admin_branch_add",
    "tracedecay_admin_cli",
    "tracedecay_admin_project",
    "tracedecay_admin_sync",
    "tracedecay_hook_runtime",
];

fn rejected_tool_project_selector_present(tool_name: &str, args: &Value) -> bool {
    let top_level_path_keys = if tool_name.starts_with("tracedecay_lcm_") {
        &["project_path"][..]
    } else {
        &["project_path", "project_root"][..]
    };
    project_selector_present(args, top_level_path_keys)
}

pub(crate) async fn selected_registered_project_reader(
    tool_name: String,
    args: Value,
    global_db: Option<&RegisteredGlobalDb>,
    resolver: Option<crate::mcp::server::RetainedProjectGraphResolver>,
) -> Result<Option<crate::mcp::project_route::ResolvedProjectRoute>> {
    if !tool_dispatches_registered_project_reader(&tool_name) {
        return Ok(None);
    }
    let context = boxed_send(project_registry_context(
        &args,
        &["project_path", "project_root"],
        global_db,
    ));
    let Some(context) = context.await.map_err(|error| {
        crate::mcp::project_route::ProjectRouteFailure::from_selection_error(&error).into_error()
    })?
    else {
        return Ok(None);
    };

    let Some(resolver) = resolver else {
        return Err(TraceDecayError::project_route(
            "project_route_unavailable",
            true,
            "registered project graph resolver is unavailable",
        ));
    };
    let requested_path = args
        .get("project_selector")
        .and_then(Value::as_object)
        .and_then(|selector| {
            selector
                .get("path")
                .or_else(|| selector.get("project_path"))
        })
        .or_else(|| args.get("project_path"))
        .or_else(|| args.get("project_root"))
        .and_then(Value::as_str)
        .map(Path::new)
        .and_then(|path| {
            crate::worktree::git_worktree_root(path).or_else(|| path.canonicalize().ok())
        })
        .unwrap_or_else(|| Path::new(&context.project.canonical_root).to_path_buf());
    let request = crate::mcp::server::RetainedProjectGraphRequest::for_registered_project(
        context.clone(),
        requested_path.clone(),
    );
    let graph = resolver(request.clone()).await?.ok_or_else(|| {
        TraceDecayError::project_route(
            "project_route_unavailable",
            true,
            format!(
                "registered project '{}' is not mounted for workspace {}",
                context.project.project_id,
                requested_path.display()
            ),
        )
    })?;
    let scope = crate::mcp::scope::resolve_query_scope(&context, &requested_path)
        .map_err(|error| error.into_route_failure().into_error())?;
    Ok(Some(crate::mcp::project_route::ResolvedProjectRoute {
        graph,
        owner: context,
        requested_root: requested_path,
        requested_git_common_dir: request.requested_git_common_dir,
        requested_branch: request.requested_branch,
        scope,
    }))
}

fn handle_retrieve(cg: &TraceDecay, args: &Value) -> Result<ToolResult> {
    let handle =
        args.get("handle")
            .and_then(Value::as_str)
            .ok_or_else(|| TraceDecayError::Config {
                message:
                    "missing required parameter: handle (copy the exact `handle` value from a truncated MCP response envelope)"
                        .to_string(),
            })?;
    let payload = match retrieve_response_handle(cg.project_root(), handle, current_timestamp())? {
        ResponseHandleLookup::Found(record) => {
            // Retrieval never truncates: the stored content is by definition
            // larger than the response cap, so neither output path may route
            // through the truncating envelope again. Markdown (default)
            // returns the stored text verbatim under a small header; JSON
            // serializes the payload directly.
            let text = if render::wants_json(args) {
                serde_json::to_string(&json!({
                    "handle": record.handle,
                    "expired": false,
                    "original_chars": record.original_chars(),
                    "created_at": record.created_at,
                    "expires_at": record.expires_at,
                    "content": record.content,
                }))
                .unwrap_or_default()
            } else {
                format!(
                    "## Retrieved Response\n**handle:** `{}` ({} chars, expires at {})\n\n{}",
                    record.handle,
                    record.original_chars(),
                    record.expires_at,
                    record.content,
                )
            };
            return Ok(ToolResult::new(
                json!({ "content": [{ "type": "text", "text": text }] }),
                Vec::new(),
            ));
        }
        ResponseHandleLookup::Missing => json!({
            "handle": handle,
            "expired": true,
            "content": null,
            "reason_code": "handle_not_found",
            "message": "Response handle was not found in this project's local cache.",
            "retryable": true,
            "retry_instruction": "Re-run the original MCP tool in this project to regenerate the full response and a fresh handle.",
        }),
        ResponseHandleLookup::Expired {
            created_at,
            expires_at,
        } => json!({
            "handle": handle,
            "expired": true,
            "content": null,
            "reason_code": "handle_expired",
            "message": format!(
                "Response handle expired at {expires_at} and was removed from this project's local cache."
            ),
            "retryable": true,
            "retry_instruction": "Re-run the original MCP tool in this project to regenerate the full response and a fresh handle.",
            "created_at": created_at,
            "expires_at": expires_at,
        }),
    };
    let text = render::finalize(Some(cg.project_root()), args, &payload, || {
        render::generic_md(&payload)
    });
    Ok(ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        Vec::new(),
    ))
}

/// Dispatches a tool call to the appropriate handler.
///
/// Returns the tool result and touched file paths, or an error if the tool
/// name is unknown or the handler fails. The optional `server_stats` value
/// is included in `tracedecay_status` responses when provided.
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

pub(crate) async fn handle_tool_call_with_registry(
    cg: &TraceDecay,
    tool_name: &str,
    args: Value,
    server_stats: Option<Value>,
    scope_prefix: Option<&str>,
    global_db: Option<&RegisteredGlobalDb>,
) -> Result<ToolResult> {
    Box::pin(handle_tool_call_with_registry_and_implicit_project(
        cg,
        tool_name,
        args,
        server_stats,
        scope_prefix,
        ToolCallRegistryOptions {
            global_db,
            ..Default::default()
        },
    ))
    .await
}

#[derive(Clone)]
pub struct ToolCallRegistryOptions<'a> {
    pub global_db: Option<&'a RegisteredGlobalDb>,
    /// Daemon-owned project-registry reads. `None` is the typed
    /// missing-registry state, not an empty registry.
    pub project_registry_reads: Option<&'a dyn ProjectRegistryReadPort>,
    /// Daemon-owned workflow-index reads. `None` is an unavailable retained
    /// project-session authority, not a successful empty index.
    pub workflow_index_reads: Option<&'a dyn WorkflowIndexReadPort>,
    pub accounting_db: Option<&'a crate::global_db::RegisteredGlobalDb>,
    pub registered_project_session_db: Option<Arc<crate::global_db::RegisteredGlobalDb>>,
    pub registered_savings_db: Option<Arc<crate::global_db::RegisteredGlobalDb>>,
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
        Option<Arc<tokio::sync::Mutex<crate::diagnostics::lsp::broker::DiagnosticBroker>>>,
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
                    let profile_root = match options.profile_root {
                        Some(profile_root) => profile_root.to_path_buf(),
                        None => support::profile_root_for_global_db(options.global_db)?,
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
                        options.global_db,
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
                options.global_db,
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
        // group probe. Application-surface tools still run before catalog checks.
        let dispatch_group = classify_mcp_tool_dispatch_group(tool_name);
        if dispatch_group == Some(McpToolDispatchGroup::ApplicationSurface) {
            return expect_classified_dispatch(
                tool_name,
                boxed_send(dispatch_application_surface_tools(
                    tool_name,
                    cg,
                    args,
                    options.clone(),
                ))
                .await,
            );
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
            return Err(TraceDecayError::Config {
                message: format!("unknown tool: {tool_name}"),
            });
        }
        match dispatch_group {
            Some(McpToolDispatchGroup::ApplicationSurface) => Err(TraceDecayError::Config {
                message: format!(
                    "internal: application-surface tool `{tool_name}` escaped early dispatch"
                ),
            }),
            Some(McpToolDispatchGroup::Graph) => expect_classified_dispatch(
                tool_name,
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
                .await,
            ),
            Some(McpToolDispatchGroup::Info) => expect_classified_dispatch(
                tool_name,
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
                .await,
            ),
            Some(McpToolDispatchGroup::Admin) => expect_classified_dispatch(
                tool_name,
                boxed_send(dispatch_admin_tools(tool_name, cg, args, options.clone())).await,
            ),
            Some(McpToolDispatchGroup::Analysis) => expect_classified_dispatch(
                tool_name,
                boxed_send(dispatch_analysis_tools(
                    tool_name,
                    cg,
                    args,
                    scope_prefix,
                    active_project_session_db,
                    options.clone(),
                ))
                .await,
            ),
            Some(McpToolDispatchGroup::Git) => expect_classified_dispatch(
                tool_name,
                boxed_send(dispatch_git_tools(tool_name, cg, args, options.clone())).await,
            ),
            Some(McpToolDispatchGroup::Edit) => expect_classified_dispatch(
                tool_name,
                boxed_send(dispatch_edit_tools(tool_name, cg, args, options.clone())).await,
            ),
            Some(McpToolDispatchGroup::Health) => expect_classified_dispatch(
                tool_name,
                boxed_send(dispatch_health_tools(
                    tool_name,
                    cg,
                    args,
                    scope_prefix,
                    active_project_session_db,
                    options.clone(),
                ))
                .await,
            ),
            Some(McpToolDispatchGroup::RetainedApplication) => expect_classified_dispatch(
                tool_name,
                boxed_send(dispatch_retained_application_tools(
                    tool_name,
                    cg,
                    args,
                    scope_prefix,
                    active_project_session_db,
                    active_lcm_context,
                    options.clone(),
                ))
                .await,
            ),
            Some(McpToolDispatchGroup::Memory) => expect_classified_dispatch(
                tool_name,
                boxed_send(dispatch_memory_tools(tool_name, cg, args, options.clone())).await,
            ),
            Some(McpToolDispatchGroup::SessionWorkflow) => expect_classified_dispatch(
                tool_name,
                boxed_send(dispatch_session_workflow_tools(
                    tool_name,
                    cg,
                    args,
                    options.clone(),
                ))
                .await,
            ),
            None if tool_name.starts_with("tracedecay_lcm_") => {
                boxed_send(dispatch_lcm_tool(tool_name, args, active_lcm_context)).await
            }
            None => Err(TraceDecayError::Config {
                message: format!("unknown tool: {tool_name}"),
            }),
        }
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum McpToolDispatchGroup {
    ApplicationSurface,
    Graph,
    Info,
    Admin,
    Analysis,
    Git,
    Edit,
    Health,
    RetainedApplication,
    Memory,
    SessionWorkflow,
}

fn expect_classified_dispatch(
    tool_name: &str,
    result: Option<Result<ToolResult>>,
) -> Result<ToolResult> {
    result.unwrap_or_else(|| {
        Err(TraceDecayError::Config {
            message: format!("internal: classified tool `{tool_name}` failed to dispatch"),
        })
    })
}

fn classify_mcp_tool_dispatch_group(tool_name: &str) -> Option<McpToolDispatchGroup> {
    if ApplicationSurfaceOperation::from_tool_name(tool_name).is_some() {
        return Some(McpToolDispatchGroup::ApplicationSurface);
    }
    match tool_name {
        "tracedecay_search"
        | "tracedecay_grep"
        | "tracedecay_ast_grep_search"
        | "tracedecay_retrieve"
        | "tracedecay_context"
        | "tracedecay_callers"
        | "tracedecay_callees"
        | "tracedecay_impact"
        | "tracedecay_node"
        | "tracedecay_similar"
        | "tracedecay_rename_preview"
        | "tracedecay_implementations"
        | "tracedecay_callers_for"
        | "tracedecay_find_exact_symbol"
        | "tracedecay_by_qualified_name"
        | "tracedecay_signature"
        | "tracedecay_impls"
        | "tracedecay_derives" => Some(McpToolDispatchGroup::Graph),
        "tracedecay_status"
        | "tracedecay_active_project"
        | "tracedecay_project_list"
        | "tracedecay_project_search"
        | "tracedecay_project_context"
        | "tracedecay_files"
        | "tracedecay_admin_sync"
        | "tracedecay_port_status"
        | "tracedecay_port_order"
        | "tracedecay_simplify_scan"
        | "tracedecay_type_hierarchy"
        | "tracedecay_body"
        | "tracedecay_todos"
        | "tracedecay_read"
        | "tracedecay_outline"
        | "tracedecay_config"
        | "tracedecay_signature_search" => Some(McpToolDispatchGroup::Info),
        "tracedecay_hook_runtime" | "tracedecay_admin_cli" | "tracedecay_admin_project" => {
            Some(McpToolDispatchGroup::Admin)
        }
        "tracedecay_dead_code"
        | "tracedecay_circular"
        | "tracedecay_hotspots"
        | "tracedecay_unused_imports"
        | "tracedecay_rank"
        | "tracedecay_largest"
        | "tracedecay_coupling"
        | "tracedecay_inheritance_depth"
        | "tracedecay_distribution"
        | "tracedecay_recursion"
        | "tracedecay_complexity"
        | "tracedecay_doc_coverage"
        | "tracedecay_god_class"
        | "tracedecay_unsafe_patterns"
        | "tracedecay_constructors"
        | "tracedecay_field_sites" => Some(McpToolDispatchGroup::Analysis),
        "tracedecay_admin_branch_add"
        | "tracedecay_affected"
        | "tracedecay_diff_context"
        | "tracedecay_changelog"
        | "tracedecay_commit_context"
        | "tracedecay_pr_context"
        | "tracedecay_branch_search"
        | "tracedecay_branch_diff"
        | "tracedecay_branch_list" => Some(McpToolDispatchGroup::Git),
        "tracedecay_str_replace"
        | "tracedecay_multi_str_replace"
        | "tracedecay_insert_at"
        | "tracedecay_ast_grep_rewrite"
        | "tracedecay_replace_symbol"
        | "tracedecay_insert_at_symbol"
        | "tracedecay_move_symbol"
        | "tracedecay_api_migration_plan"
        | "tracedecay_api_migration_apply"
        | "tracedecay_source_edit_reconcile" => Some(McpToolDispatchGroup::Edit),
        "tracedecay_test_map"
        | "tracedecay_gini"
        | "tracedecay_dependency_depth"
        | "tracedecay_health"
        | "tracedecay_redundancy"
        | "tracedecay_runtime"
        | "tracedecay_dsm"
        | "tracedecay_test_risk" => Some(McpToolDispatchGroup::Health),
        _ if RetainedSurfaceOperation::from_name(tool_name).is_some() => {
            Some(McpToolDispatchGroup::RetainedApplication)
        }
        "tracedecay_automation_run_artifact_view"
        | "tracedecay_analytics"
        | "tracedecay_skill_list"
        | "tracedecay_skill_view"
        | "tracedecay_hermes_skill_bridge" => Some(McpToolDispatchGroup::Memory),
        "tracedecay_diagnose" | "tracedecay_run_affected_tests" | "tracedecay_dashboard" => {
            Some(McpToolDispatchGroup::SessionWorkflow)
        }
        _ => None,
    }
}

#[cfg(test)]
#[test]
fn diagnostics_without_an_executor_stays_on_the_read_only_application_surface() {
    assert_eq!(
        classify_mcp_tool_dispatch_group("tracedecay_diagnostics"),
        Some(McpToolDispatchGroup::ApplicationSurface)
    );
}

#[derive(Debug)]
struct CatalogBoundRetainedMcpRequest {
    operation: RetainedSurfaceOperation,
    arguments: Value,
}

#[derive(Clone, Copy)]
enum RetainedMcpExecutionContext<'call, 'authority> {
    Profile {
        tool_name: &'call str,
        profile_root: &'call Path,
        options: &'call ToolCallRegistryOptions<'authority>,
    },
    Project {
        tool_name: &'call str,
        cg: &'call TraceDecay,
        scope_prefix: Option<&'call str>,
        active_project_session_db: Option<&'call Arc<RegisteredGlobalDb>>,
        active_lcm_context: session::LcmHandlerContext<'call>,
        options: &'call ToolCallRegistryOptions<'authority>,
    },
}

static RETAINED_MCP_COMPOSITION: OnceLock<
    std::result::Result<ApplicationCatalogComposition<()>, String>,
> = OnceLock::new();

struct RetainedMcpCatalogDispatcher<'call, 'authority> {
    context: RetainedMcpExecutionContext<'call, 'authority>,
}

type RetainedMcpInvocationFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + 'a>>;

impl<'call> CanonicalApplicationDispatcher<CatalogBoundRetainedMcpRequest>
    for RetainedMcpCatalogDispatcher<'call, '_>
{
    type Output = RetainedMcpInvocationFuture<'call>;

    fn invoke(
        &self,
        operation: &ApplicationOperation,
        request: CatalogBoundRetainedMcpRequest,
    ) -> Self::Output {
        let expected = match retained_surface_application_operation(request.operation)
            .map_err(retained_catalog_error)
        {
            Ok(expected) => expected,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        if operation != &expected {
            let error = retained_catalog_error(
                "resolved retained MCP handler does not own the requested operation",
            );
            return Box::pin(async move { Err(error) });
        }
        let context = self.context;
        Box::pin(async move {
            match context {
                RetainedMcpExecutionContext::Profile {
                    tool_name,
                    profile_root,
                    options,
                } => {
                    execute_profile_retained_application_tool(
                        request,
                        tool_name,
                        profile_root,
                        options,
                    )
                    .await
                }
                RetainedMcpExecutionContext::Project {
                    tool_name,
                    cg,
                    scope_prefix,
                    active_project_session_db,
                    active_lcm_context,
                    options,
                } => {
                    execute_project_retained_application_tool(
                        request,
                        tool_name,
                        cg,
                        scope_prefix,
                        active_project_session_db,
                        active_lcm_context,
                        options,
                    )
                    .await
                }
            }
        })
    }
}

fn retained_catalog_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("retained application catalog is unavailable: {error}"),
    }
}

fn retained_mcp_composition() -> Result<&'static ApplicationCatalogComposition<()>> {
    RETAINED_MCP_COMPOSITION
        .get_or_init(|| compose_application_catalog(()).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(retained_catalog_error)
}

async fn invoke_retained_mcp_request(
    context: RetainedMcpExecutionContext<'_, '_>,
    operation: RetainedSurfaceOperation,
    arguments: Value,
) -> Result<ToolResult> {
    let composition = retained_mcp_composition()?;
    let profile_id =
        ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).map_err(retained_catalog_error)?;
    let operation_name =
        SurfaceOperationName::new(operation.as_str()).map_err(retained_catalog_error)?;
    let capability = composition
        .snapshot()
        .resolve_binding(
            &profile_id,
            BindingSurface::Mcp,
            &operation_name,
            1,
            &BTreeSet::new(),
        )
        .ok_or_else(|| retained_catalog_error("retained MCP binding is not callable"))?;
    let expected =
        retained_surface_application_operation(operation).map_err(retained_catalog_error)?;
    if capability.capability_id() != expected.capability_id()
        || capability.use_case_id() != expected.use_case_id()
    {
        return Err(retained_catalog_error(
            "retained MCP binding resolves a different application operation",
        ));
    }
    let dispatcher = RetainedMcpCatalogDispatcher { context };
    let handler = composition
        .bind_handler(capability.use_case_id(), &dispatcher)
        .ok_or_else(|| retained_catalog_error("retained MCP handler is not registered"))?;
    handler
        .invoke(CatalogBoundRetainedMcpRequest {
            operation,
            arguments,
        })
        .await
}

async fn dispatch_profile_retained_application_tool(
    operation: RetainedSurfaceOperation,
    tool_name: &str,
    args: Value,
    profile_root: &Path,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    invoke_retained_mcp_request(
        RetainedMcpExecutionContext::Profile {
            tool_name,
            profile_root,
            options: &options,
        },
        operation,
        args,
    )
    .await
}

async fn execute_profile_retained_application_tool(
    request: CatalogBoundRetainedMcpRequest,
    tool_name: &str,
    profile_root: &Path,
    options: &ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match request.operation {
        RetainedSurfaceOperation::MessageSearch
        | RetainedSurfaceOperation::LcmStatus
        | RetainedSurfaceOperation::LcmDoctor
        | RetainedSurfaceOperation::LcmLoadSession
        | RetainedSurfaceOperation::LcmGrep
        | RetainedSurfaceOperation::LcmDescribe
        | RetainedSurfaceOperation::LcmExpand
        | RetainedSurfaceOperation::LcmExpandQuery
        | RetainedSurfaceOperation::LcmPreflight
        | RetainedSurfaceOperation::LcmCompress
        | RetainedSurfaceOperation::LcmSessionBoundary => {
            handle_user_lcm_tool_with_db(
                tool_name,
                request.arguments,
                profile_root,
                options.session_authorities.user,
                options.global_db,
                options.session_authorities.profile_retrieval,
            )
            .await
        }
        _ => Err(TraceDecayError::Config {
            message: format!("storage_scope=user is not supported for `{tool_name}`"),
        }),
    }
}

/// Dispatch code-graph navigation and lookup tools (`tracedecay_search`,
/// `tracedecay_callers`, ...). Returns `None` when `tool_name` belongs to a
/// different domain so the caller can try the next dispatch group.
async fn dispatch_graph_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    selected_scope_prefix: Option<&str>,
    search_executor: Option<&crate::mcp::server::CodeIndexSearchExecutor>,
    search_authority: Option<&crate::mcp::server::CodeIndexSearchAuthorityV1>,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> Option<Result<ToolResult>> {
    let result = match tool_name {
        "tracedecay_search" => {
            graph::handle_search(
                cg,
                args,
                selected_scope_prefix,
                search_executor,
                search_authority,
                deadline,
                cancellation,
            )
            .await
        }
        "tracedecay_grep" => grep::handle_grep(cg, args, selected_scope_prefix).await,
        "tracedecay_ast_grep_search" => {
            ast_grep_search::handle_ast_grep_search(cg, args, selected_scope_prefix).await
        }
        "tracedecay_retrieve" => handle_retrieve(cg, &args),
        "tracedecay_context" => graph::handle_context(cg, args, selected_scope_prefix).await,
        "tracedecay_callers" => graph::handle_callers(cg, args).await,
        "tracedecay_callees" => graph::handle_callees(cg, args).await,
        "tracedecay_impact" => graph::handle_impact(cg, args).await,
        "tracedecay_node" => graph::handle_node(cg, args).await,
        "tracedecay_similar" => graph::handle_similar(cg, args).await,
        "tracedecay_rename_preview" => graph::handle_rename_preview(cg, args).await,
        "tracedecay_implementations" => {
            graph::handle_implementations(cg, args, selected_scope_prefix).await
        }
        "tracedecay_callers_for" => graph::handle_callers_for(cg, args).await,
        "tracedecay_find_exact_symbol" => {
            graph::handle_find_exact_symbol(cg, args, selected_scope_prefix).await
        }
        "tracedecay_by_qualified_name" => graph::handle_by_qualified_name(cg, args).await,
        "tracedecay_signature" => graph::handle_signature(cg, args).await,
        "tracedecay_impls" => graph::handle_impls(cg, args).await,
        "tracedecay_derives" => graph::handle_derives(cg, args).await,
        _ => return None,
    };
    Some(result)
}

/// Dispatch project-info, registry, and file-inspection tools
/// (`tracedecay_status`, `tracedecay_project_list`, `tracedecay_read`, ...).
#[allow(clippy::too_many_arguments)]
async fn dispatch_info_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    server_stats: Option<Value>,
    scope_prefix: Option<&str>,
    selected_scope_prefix: Option<&str>,
    active_project_session_db: Option<&Arc<RegisteredGlobalDb>>,
    options: ToolCallRegistryOptions<'_>,
) -> Option<Result<ToolResult>> {
    let result = match tool_name {
        "tracedecay_status" => {
            info::handle_status(
                cg,
                args,
                server_stats,
                scope_prefix,
                active_project_session_db.map(Arc::as_ref),
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
        "tracedecay_files" => info::handle_files(cg, args, selected_scope_prefix).await,
        "tracedecay_admin_sync" => info::handle_admin_sync(cg, args).await,
        "tracedecay_port_status" => info::handle_port_status(cg, args).await,
        "tracedecay_port_order" => info::handle_port_order(cg, args).await,
        "tracedecay_simplify_scan" => info::handle_simplify_scan(cg, args, scope_prefix).await,
        "tracedecay_type_hierarchy" => info::handle_type_hierarchy(cg, args).await,
        "tracedecay_body" => info::handle_body(cg, args, selected_scope_prefix).await,
        "tracedecay_todos" => info::handle_todos(cg, args, scope_prefix).await,
        "tracedecay_read" => info::handle_read(cg, args).await,
        "tracedecay_outline" => info::handle_outline(cg, args).await,
        "tracedecay_config" => info::handle_config(cg, &args),
        "tracedecay_signature_search" => {
            info::handle_signature_search(cg, args, selected_scope_prefix).await
        }
        _ => return None,
    };
    Some(result)
}

/// Dispatch administrative tools (`tracedecay_hook_runtime`,
/// `tracedecay_admin_cli`, `tracedecay_admin_project`).
async fn dispatch_admin_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Option<Result<ToolResult>> {
    let result = match tool_name {
        "tracedecay_hook_runtime" => {
            hook_runtime::handle_hook_runtime(
                cg,
                args,
                options.global_db,
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
            )
            .await
        }
        "tracedecay_admin_project" => {
            admin_project::handle_admin_project(
                cg,
                args,
                options.global_db,
                options.automation_scheduler_reconciler.clone(),
            )
            .await
        }
        _ => return None,
    };
    Some(result)
}

/// Dispatch catalog-owned application surfaces.
async fn dispatch_application_surface_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Option<Result<ToolResult>> {
    let operation = ApplicationSurfaceOperation::from_tool_name(tool_name)?;
    let normalized_args =
        match crate::application_surface::normalize_application_tool_args(tool_name, args) {
            Ok(args) => args,
            Err(error) => {
                return Some(Err(TraceDecayError::Config {
                    message: error.to_string(),
                }));
            }
        };
    Some(
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
        .await,
    )
}

/// Dispatch static-analysis report tools (`tracedecay_dead_code`,
/// `tracedecay_complexity`, `tracedecay_diagnostics`, ...).
async fn dispatch_analysis_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
    active_project_session_db: Option<&Arc<RegisteredGlobalDb>>,
    options: ToolCallRegistryOptions<'_>,
) -> Option<Result<ToolResult>> {
    let result = match tool_name {
        "tracedecay_dead_code" => analysis::handle_dead_code(cg, args, scope_prefix).await,
        "tracedecay_circular" => analysis::handle_circular(cg, args).await,
        "tracedecay_hotspots" => analysis::handle_hotspots(cg, args, scope_prefix).await,
        "tracedecay_unused_imports" => {
            analysis::handle_unused_imports(cg, args, scope_prefix).await
        }
        "tracedecay_rank" => analysis::handle_rank(cg, args, scope_prefix).await,
        "tracedecay_largest" => analysis::handle_largest(cg, args, scope_prefix).await,
        "tracedecay_coupling" => analysis::handle_coupling(cg, args, scope_prefix).await,
        "tracedecay_inheritance_depth" => {
            analysis::handle_inheritance_depth(cg, args, scope_prefix).await
        }
        "tracedecay_distribution" => analysis::handle_distribution(cg, args, scope_prefix).await,
        "tracedecay_recursion" => analysis::handle_recursion(cg, args, scope_prefix).await,
        "tracedecay_complexity" => analysis::handle_complexity(cg, args, scope_prefix).await,
        "tracedecay_doc_coverage" => analysis::handle_doc_coverage(cg, args, scope_prefix).await,
        "tracedecay_god_class" => analysis::handle_god_class(cg, args, scope_prefix).await,
        "tracedecay_unsafe_patterns" => {
            analysis::handle_unsafe_patterns(cg, args, scope_prefix).await
        }
        "tracedecay_constructors" => analysis::handle_constructors(cg, args, scope_prefix).await,
        "tracedecay_field_sites" => analysis::handle_field_sites(cg, args, scope_prefix).await,
        "tracedecay_diagnostics" => {
            analysis::handle_diagnostics(
                cg,
                args,
                options.diagnostics_cache,
                options.diagnostics_lsp.as_deref(),
                active_project_session_db.map(Arc::as_ref),
            )
            .await
        }
        _ => return None,
    };
    Some(result)
}

/// Dispatch git-aware tools (`tracedecay_affected`, `tracedecay_changelog`,
/// branch and PR context helpers).
async fn dispatch_git_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    _options: ToolCallRegistryOptions<'_>,
) -> Option<Result<ToolResult>> {
    let result = match tool_name {
        "tracedecay_admin_branch_add" => git::handle_admin_branch_add(cg, args).await,
        "tracedecay_affected" => git::handle_affected(cg, args).await,
        "tracedecay_diff_context" => git::handle_diff_context(cg, args).await,
        "tracedecay_changelog" => git::handle_changelog(cg, args).await,
        "tracedecay_commit_context" => git::handle_commit_context(cg, args).await,
        "tracedecay_pr_context" => git::handle_pr_context(cg, args).await,
        "tracedecay_branch_search" => git::handle_branch_search(cg, args).await,
        "tracedecay_branch_diff" => git::handle_branch_diff(cg, args).await,
        "tracedecay_branch_list" => Ok(git::handle_branch_list(cg, &args)),
        _ => return None,
    };
    Some(result)
}

/// Dispatch source-editing tools (`tracedecay_str_replace`,
/// `tracedecay_move_symbol`, ...).
async fn dispatch_edit_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Option<Result<ToolResult>> {
    let invocation = edit::SourceEditInvocationContext {
        executor: options.source_edit_executor.clone(),
        reconciliation_executor: options.source_edit_reconciliation_executor.clone(),
        request_id: options.application_request_id.clone(),
        deadline: options.application_deadline.clone(),
        cancellation: options.application_cancellation.clone(),
    };
    let result = match tool_name {
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
        "tracedecay_api_migration_plan" => edit::handle_api_migration_plan(cg, args).await,
        "tracedecay_api_migration_apply" => {
            edit::handle_api_migration_apply(cg, args, invocation.clone()).await
        }
        "tracedecay_source_edit_reconcile" => {
            edit::handle_source_edit_reconcile(cg, args, invocation).await
        }
        _ => return None,
    };
    Some(result)
}

/// Dispatch code-health and session-baseline tools (`tracedecay_health`,
/// `tracedecay_test_risk`, `tracedecay_runtime`, ...).
async fn dispatch_health_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
    active_project_session_db: Option<&Arc<RegisteredGlobalDb>>,
    options: ToolCallRegistryOptions<'_>,
) -> Option<Result<ToolResult>> {
    let result = match tool_name {
        "tracedecay_test_map" => health::handle_test_map(cg, args, scope_prefix).await,
        "tracedecay_gini" => health::handle_gini(cg, args, scope_prefix).await,
        "tracedecay_dependency_depth" => {
            health::handle_dependency_depth(cg, args, scope_prefix).await
        }
        "tracedecay_health" => health::handle_health(cg, args, scope_prefix).await,
        "tracedecay_redundancy" => redundancy::handle_redundancy(cg, args, scope_prefix).await,
        "tracedecay_runtime" => {
            health::handle_runtime(
                cg,
                args,
                options.global_db,
                active_project_session_db.map(Arc::as_ref),
                options.doctor_report_reader.as_ref(),
            )
            .await
        }
        "tracedecay_dsm" => health::handle_dsm(cg, args, scope_prefix).await,
        "tracedecay_test_risk" => health::handle_test_risk(cg, args, scope_prefix).await,
        _ => return None,
    };
    Some(result)
}

/// Dispatch retained memory, session, and workflow operations only after the
/// application-owned catalog has resolved their stable operation identity.
async fn dispatch_retained_application_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
    active_project_session_db: Option<&Arc<RegisteredGlobalDb>>,
    active_lcm_context: session::LcmHandlerContext<'_>,
    options: ToolCallRegistryOptions<'_>,
) -> Option<Result<ToolResult>> {
    let operation = RetainedSurfaceOperation::from_name(tool_name)?;
    Some(
        invoke_retained_mcp_request(
            RetainedMcpExecutionContext::Project {
                tool_name,
                cg,
                scope_prefix,
                active_project_session_db,
                active_lcm_context,
                options: &options,
            },
            operation,
            args,
        )
        .await,
    )
}

#[allow(clippy::too_many_arguments)]
async fn execute_project_retained_application_tool(
    request: CatalogBoundRetainedMcpRequest,
    tool_name: &str,
    cg: &TraceDecay,
    scope_prefix: Option<&str>,
    active_project_session_db: Option<&Arc<RegisteredGlobalDb>>,
    active_lcm_context: session::LcmHandlerContext<'_>,
    options: &ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match request.operation {
        RetainedSurfaceOperation::FactStore => {
            memory::handle_fact_store(cg, request.arguments, options.global_db).await
        }
        RetainedSurfaceOperation::FactFeedback => {
            memory::handle_fact_feedback(cg, request.arguments, options.global_db).await
        }
        RetainedSurfaceOperation::MemoryStatus => {
            memory::handle_memory_status(cg, request.arguments, options.global_db).await
        }
        RetainedSurfaceOperation::SessionRefresh => {
            session::handle_session_refresh(
                request.arguments,
                options.session_authorities.refresh_services(),
            )
            .await
        }
        RetainedSurfaceOperation::MessageSearch => {
            Box::pin(session::message_search::handle_message_search_with_service(
                Some(cg.project_root()),
                session::message_search::SessionRetrievalStoreScope::Project,
                request.arguments,
                options.session_authorities.project_retrieval,
            ))
            .await
        }
        RetainedSurfaceOperation::SessionsFor => {
            session::handle_sessions_for(
                cg,
                active_project_session_db.map(Arc::as_ref),
                request.arguments,
            )
            .await
        }
        RetainedSurfaceOperation::Workflows => {
            workflow_query::handle_workflows(cg, request.arguments, options.workflow_index_reads)
                .await
        }
        RetainedSurfaceOperation::LcmStatus
        | RetainedSurfaceOperation::LcmDoctor
        | RetainedSurfaceOperation::LcmLoadSession
        | RetainedSurfaceOperation::LcmGrep
        | RetainedSurfaceOperation::LcmDescribe
        | RetainedSurfaceOperation::LcmExpand
        | RetainedSurfaceOperation::LcmExpandQuery
        | RetainedSurfaceOperation::LcmPreflight
        | RetainedSurfaceOperation::LcmCompress
        | RetainedSurfaceOperation::LcmSessionBoundary => {
            dispatch_lcm_tool(tool_name, request.arguments, active_lcm_context).await
        }
        RetainedSurfaceOperation::SessionStart => {
            let db = active_project_session_db.ok_or_else(|| TraceDecayError::Config {
                message: "health-delta observation authority is unavailable".to_owned(),
            })?;
            health::handle_session_start(cg, db.as_ref(), request.arguments, scope_prefix).await
        }
        RetainedSurfaceOperation::SessionEnd => {
            let db = active_project_session_db.ok_or_else(|| TraceDecayError::Config {
                message: "health-delta observation authority is unavailable".to_owned(),
            })?;
            health::handle_session_end(cg, db.as_ref(), request.arguments, scope_prefix).await
        }
    }
}

/// Dispatch memory, skill, and analytics tools (`tracedecay_fact_store`,
/// `tracedecay_skill_list`, `tracedecay_analytics`, ...).
async fn dispatch_memory_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Option<Result<ToolResult>> {
    let result = match tool_name {
        "tracedecay_automation_run_artifact_view" => {
            skills::handle_automation_run_artifact_view(cg, args).await
        }
        "tracedecay_analytics" => {
            analytics::handle_analytics(cg, args, options.global_db, options.accounting_db).await
        }
        "tracedecay_skill_list" => skills::handle_skill_list(cg, args, options.accounting_db).await,
        "tracedecay_skill_view" => skills::handle_skill_view(cg, args, options.accounting_db).await,
        "tracedecay_hermes_skill_bridge" => skills::handle_hermes_skill_bridge(cg, &args),
        _ => return None,
    };
    Some(result)
}

/// Dispatch session, dashboard, and workflow tools (`tracedecay_dashboard`,
/// `tracedecay_message_search`, `tracedecay_workflows`, ...).
async fn dispatch_session_workflow_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Option<Result<ToolResult>> {
    let result = match tool_name {
        "tracedecay_diagnose" => {
            workflow::handle_diagnose(cg, args, options.code_index_publication_identity.as_deref())
                .await
        }
        "tracedecay_run_affected_tests" => {
            workflow::handle_run_affected_tests(
                cg,
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
                options.retained_project_graph_resolver.clone(),
                options.registered_project_session_db.clone(),
                options.registered_savings_db.clone(),
                options.automation_scheduler_reconciler.clone(),
                options.automation_writer.clone(),
                options.doctor_report_reader.clone(),
                options.doctor_remediation_dispatcher.clone(),
                options.code_index_freshness_reader.clone(),
                options.feedback_status_reader.clone(),
                options.diagnostics_lsp.clone(),
                options.dashboard_application_invocation_executor.clone(),
            )
            .await
        }
        _ => return None,
    };
    Some(result)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::await_holding_lock,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::{OsStr, OsString};
    use std::fmt::Write as _;
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;

    use serde_json::json;
    use tempfile::TempDir;

    use super::super::get_tool_definitions;
    use super::*;
    use crate::config::{USER_DATA_DIR_ENV, lock_user_data_dir_test_env};
    use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;

    struct SelectorRegistry {
        database: Arc<RegisteredGlobalDb>,
        _registry: DaemonSessionRuntimeRegistryV1,
        _scope: crate::db::DaemonDatabaseScope,
    }

    impl SelectorRegistry {
        async fn open() -> Self {
            let profile_root = crate::config::user_data_dir().expect("selector profile root");
            let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
                .expect("selector profile identity");
            let scope = crate::db::enter_daemon_database_scope(
                identity.profile_root(),
                1,
                "host-admission-test-runtime",
            )
            .expect("selector daemon database scope");
            let registry = DaemonSessionRuntimeRegistryV1::open(identity)
                .await
                .expect("selector session runtime registry");
            let database = registry
                .profile_database()
                .await
                .expect("selector registered profile database");
            Self {
                database,
                _registry: registry,
                _scope: scope,
            }
        }
    }

    fn selector_options(
        registry: &SelectorRegistry,
        graphs: Vec<Arc<TraceDecay>>,
    ) -> ToolCallRegistryOptions<'_> {
        let graphs = Arc::new(
            graphs
                .into_iter()
                .map(|graph| (graph.project_root().to_path_buf(), graph))
                .collect::<BTreeMap<_, _>>(),
        );
        let resolver: crate::mcp::server::RetainedProjectGraphResolver = Arc::new(move |request| {
            let graph = graphs.get(&request.registered_root).cloned();
            Box::pin(async move { Ok(graph) })
        });
        ToolCallRegistryOptions {
            global_db: Some(registry.database.as_ref()),
            retained_project_graph_resolver: Some(resolver),
            ..Default::default()
        }
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(previous) = self.previous.take() {
                    std::env::set_var(self.key, previous);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    struct SelectorEnv {
        _home: EnvVarGuard,
        _userprofile: EnvVarGuard,
        _data_dir: EnvVarGuard,
        _global_db: EnvVarGuard,
    }

    impl SelectorEnv {
        fn new(root: &Path) -> Self {
            let home = root.join("home");
            let profile_root = home.join(".tracedecay");
            crate::storage::PrivateStoreIo::create_dir_all(&profile_root).unwrap();
            let home = home.canonicalize().unwrap();
            let profile_root = home.join(".tracedecay");
            let global_db_path = profile_root.join("global.db");
            Self {
                _home: EnvVarGuard::set("HOME", &home),
                _userprofile: EnvVarGuard::set("USERPROFILE", &home),
                _data_dir: EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root),
                _global_db: EnvVarGuard::set("TRACEDECAY_GLOBAL_DB", &global_db_path),
            }
        }
    }

    fn dispatch_tool_names_from_source(function_name: &str) -> BTreeSet<String> {
        let source = include_str!("mod.rs");
        let fn_marker = format!("async fn {function_name}");
        let function_source = source
            .split_once(&fn_marker)
            .unwrap_or_else(|| panic!("missing function source for {function_name}"))
            .1;
        let match_source = function_source
            .split_once("match tool_name {")
            .unwrap_or_else(|| panic!("{function_name} does not match on tool_name"))
            .1;
        let handler_arms = match_source
            .split_once("_ =>")
            .unwrap_or_else(|| panic!("{function_name} does not have a wildcard fallback arm"))
            .0;

        handler_arms
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim_start();
                if !trimmed.starts_with("\"tracedecay_") || !trimmed.contains("=>") {
                    return None;
                }
                let after_opening_quote = trimmed.strip_prefix('"')?;
                let (name, after_name) = after_opening_quote.split_once('"')?;
                if after_name.trim_start().starts_with("=>") {
                    Some(name.to_string())
                } else {
                    None
                }
            })
            .collect()
    }

    fn assert_set_empty(names: BTreeSet<String>, message: &str) {
        assert!(
            names.is_empty(),
            "{message}: {}",
            names.into_iter().collect::<Vec<_>>().join(", ")
        );
    }

    // MCP registry maintenance guardrail:
    // when adding a tool, update all three surfaces together: its `def_*`
    // entry in definitions.rs, the `get_tool_definitions()` registry, and
    // the application operation catalog. These lockstep tests
    // intentionally fail with the missing tool name when any surface drifts.
    #[test]
    fn tool_definitions_and_dispatch_handlers_stay_in_lockstep() {
        let definition_names = get_tool_definitions()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<BTreeSet<_>>();
        let mut handler_names = dispatch_tool_names_from_source("handle_tool_call");
        handler_names.extend(dispatch_tool_names_from_source("dispatch_lcm_tool"));
        for dispatch_fn in [
            "dispatch_graph_tools",
            "dispatch_info_tools",
            "dispatch_admin_tools",
            "dispatch_analysis_tools",
            "dispatch_git_tools",
            "dispatch_edit_tools",
            "dispatch_health_tools",
            "dispatch_memory_tools",
            "dispatch_session_workflow_tools",
        ] {
            handler_names.extend(dispatch_tool_names_from_source(dispatch_fn));
        }
        handler_names.extend(
            APPLICATION_SURFACE_OPERATIONS
                .into_iter()
                .map(|operation| format!("tracedecay_{}", operation.as_str())),
        );
        for hidden in super::super::definitions::UNADVERTISED_HANDLE_GATED_TOOL_NAMES {
            handler_names.remove(*hidden);
        }
        for operation in RetainedSurfaceOperation::ALL {
            let tool_name = format!("tracedecay_{}", operation.as_str());
            let composition = retained_mcp_composition()
                .unwrap_or_else(|error| panic!("{tool_name} catalog composition failed: {error}"));
            let profile = ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).unwrap();
            let operation_name = SurfaceOperationName::new(operation.as_str()).unwrap();
            let capability = composition
                .snapshot()
                .resolve_binding(
                    &profile,
                    BindingSurface::Mcp,
                    &operation_name,
                    1,
                    &BTreeSet::new(),
                )
                .unwrap_or_else(|| panic!("{tool_name} catalog binding is not callable"));
            let expected = retained_surface_application_operation(operation).unwrap();
            assert_eq!(capability.capability_id(), expected.capability_id());
            assert_eq!(capability.use_case_id(), expected.use_case_id());
            assert!(
                composition
                    .bind_handler(capability.use_case_id(), &())
                    .is_some(),
                "{tool_name} application handler is not registered"
            );
            handler_names.insert(tool_name);
        }
        for internal in INTERNAL_DAEMON_TOOL_NAMES {
            handler_names.remove(*internal);
        }

        // These tools are intentionally hidden from the advertised surface when
        // the host ast-grep CLI capability they need is unavailable; mirror the
        // runtime filters so the integrity check covers the actual tools/list
        // surface.
        if !super::super::definitions::ast_grep_available() {
            handler_names.remove("tracedecay_ast_grep_rewrite");
        }

        assert_set_empty(
            definition_names
                .difference(&handler_names)
                .cloned()
                .collect(),
            "MCP tool definitions missing handle_tool_call handlers",
        );
        assert_set_empty(
            handler_names
                .difference(&definition_names)
                .cloned()
                .collect(),
            "handle_tool_call handlers missing MCP tool definitions",
        );
    }

    #[test]
    fn graph_reader_selector_dispatch_policy_is_allowlisted() {
        for tool in get_tool_definitions() {
            let properties = &tool.input_schema["properties"];
            let schema_has_registered_project_selector =
                ["project_selector", "project_id", "project_path"]
                    .iter()
                    .any(|selector_key| properties.get(*selector_key).is_some());
            assert_eq!(
                tool_accepts_registered_project_selector(&tool.name),
                schema_has_registered_project_selector,
                "{} registered-project selector schema and dispatch policy should stay in lockstep",
                tool.name
            );
        }

        for tool_name in [
            "tracedecay_search",
            "tracedecay_str_replace",
            "tracedecay_run_affected_tests",
            "tracedecay_status",
            "tracedecay_health",
            "tracedecay_dead_code",
        ] {
            assert!(
                !tool_accepts_registered_project_selector(tool_name),
                "{tool_name} should not be routed by the pure graph-reader selector policy"
            );
        }
    }

    #[tokio::test]
    async fn graph_reader_selector_dispatch_targets_registered_project() {
        let _env_lock = lock_user_data_dir_test_env();
        let dir = TempDir::new().unwrap();
        let _env = SelectorEnv::new(dir.path());
        let active_project = dir.path().join("active");
        let target_project = dir.path().join("target");
        fs::create_dir_all(active_project.join("src")).unwrap();
        fs::create_dir_all(target_project.join("src")).unwrap();
        fs::write(active_project.join("src/active.rs"), "pub fn active() {}\n").unwrap();
        fs::write(target_project.join("src/target.rs"), "pub fn target() {}\n").unwrap();

        let (active, _active_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            &active_project,
            "project.mcp-active-selector",
        )
        .await
        .unwrap();
        let (target, _target_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            &target_project,
            "project.mcp-target-selector",
        )
        .await
        .unwrap();
        let target = Arc::new(target);
        let target_still_stale = target
            .sync_if_stale(&["src/target.rs".to_string()])
            .await
            .unwrap();
        assert!(
            !target_still_stale,
            "target fixture source should be indexed for selected-project file listing"
        );
        let registry = SelectorRegistry::open().await;
        let target_project_id = target
            .store_layout()
            .identity
            .project_id
            .as_deref()
            .expect("target project should be registered")
            .to_string();

        let result = handle_tool_call_with_registry_and_implicit_project(
            &active,
            "tracedecay_files",
            json!({
                "project_id": target_project_id,
                "path": "src"
            }),
            None,
            Some("tests"),
            selector_options(&registry, vec![Arc::clone(&target)]),
        )
        .await
        .unwrap();
        let text = result.value["content"][0]["text"].as_str().unwrap();

        assert!(
            text.contains("target.rs"),
            "selected registered project file listing should return target graph results: {text}"
        );
        assert!(
            !text.contains("active.rs"),
            "selected registered project file listing should not query the active graph: {text}"
        );

        active.checkpoint().await.unwrap();
        target.checkpoint().await.unwrap();
        active.close();
        Arc::into_inner(target)
            .expect("selector target graph should no longer be retained")
            .close();
    }

    #[tokio::test]
    async fn graph_reader_selector_dispatch_accepts_unique_project_basename() {
        let _env_lock = lock_user_data_dir_test_env();
        let dir = TempDir::new().unwrap();
        let _env = SelectorEnv::new(dir.path());
        let active_project = dir.path().join("active");
        let target_project = dir.path().join("target");
        fs::create_dir_all(active_project.join("src")).unwrap();
        fs::create_dir_all(target_project.join("src")).unwrap();
        fs::write(active_project.join("src/active.rs"), "pub fn active() {}\n").unwrap();
        fs::write(target_project.join("src/target.rs"), "pub fn target() {}\n").unwrap();

        let (active, _active_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            &active_project,
            "project.mcp-active-basename",
        )
        .await
        .unwrap();
        let (target, _target_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            &target_project,
            "project.mcp-target-basename",
        )
        .await
        .unwrap();
        let target = Arc::new(target);
        target.index_all().await.unwrap();
        let registry = SelectorRegistry::open().await;

        let result = handle_tool_call_with_registry_and_implicit_project(
            &active,
            "tracedecay_grep",
            json!({
                "project_selector": {"path": "target"},
                "pattern": "target",
                "limit": 5,
            }),
            None,
            None,
            selector_options(&registry, vec![Arc::clone(&target)]),
        )
        .await
        .unwrap();
        let text = result.value["content"][0]["text"].as_str().unwrap();

        assert!(
            text.contains("target"),
            "unique basename selector should return target graph results: {text}"
        );
        assert!(
            !text.contains("active"),
            "unique basename selector should not query the active graph: {text}"
        );

        active.checkpoint().await.unwrap();
        target.checkpoint().await.unwrap();
        active.close();
        Arc::into_inner(target)
            .expect("basename target graph should no longer be retained")
            .close();
    }

    #[tokio::test]
    async fn graph_reader_selector_rejects_ambiguous_project_basename() {
        let _env_lock = lock_user_data_dir_test_env();
        let dir = TempDir::new().unwrap();
        let _env = SelectorEnv::new(dir.path());
        let active_project = dir.path().join("active");
        let first_target = dir.path().join("first").join("target");
        let second_target = dir.path().join("second").join("target");
        fs::create_dir_all(&active_project).unwrap();
        fs::create_dir_all(&first_target).unwrap();
        fs::create_dir_all(&second_target).unwrap();

        let (active, _active_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            &active_project,
            "project.mcp-active-ambiguous",
        )
        .await
        .unwrap();
        let (first, _first_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            &first_target,
            "project.mcp-first-ambiguous",
        )
        .await
        .unwrap();
        let (second, _second_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            &second_target,
            "project.mcp-second-ambiguous",
        )
        .await
        .unwrap();
        let registry = SelectorRegistry::open().await;

        let err = handle_tool_call_with_registry(
            &active,
            "tracedecay_grep",
            json!({
                "project_selector": {"path": "target"},
                "pattern": "target",
            }),
            None,
            None,
            Some(registry.database.as_ref()),
        )
        .await
        .unwrap_err();
        assert!(
            format!("{err}").contains("registered project not found for selector"),
            "ambiguous basename selector should be rejected: {err}"
        );

        active.checkpoint().await.unwrap();
        first.checkpoint().await.unwrap();
        second.checkpoint().await.unwrap();
        active.close();
        first.close();
        second.close();
    }

    #[tokio::test]
    async fn status_and_runtime_share_cursor_session_ingest_authority() {
        let _env_lock = lock_user_data_dir_test_env();
        let dir = TempDir::new().unwrap();
        let _env = SelectorEnv::new(dir.path());
        let project = dir.path().join("active");
        fs::create_dir_all(&project).unwrap();
        let (cg, runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            &project,
            "project.mcp-session-ingest-authority",
        )
        .await
        .unwrap();
        let database = runtime
            .registered_database(crate::application::host_admission::HostAdmissionScope::Project)
            .unwrap();
        let cursor_path = dir.path().join("cursor.jsonl");
        let claude_path = dir.path().join("claude.jsonl");
        fs::write(&cursor_path, b"0123456789").unwrap();
        fs::write(&claude_path, b"01234567890123456789").unwrap();
        for (provider, session_id, path) in [
            ("cursor", "session.cursor", cursor_path.as_path()),
            ("claude", "session.claude", claude_path.as_path()),
        ] {
            assert!(
                database
                    .upsert_session(&crate::sessions::SessionRecord {
                        provider: provider.to_owned(),
                        session_id: session_id.to_owned(),
                        project_key: project.display().to_string(),
                        project_path: project.display().to_string(),
                        title: None,
                        started_at: None,
                        ended_at: None,
                        transcript_path: Some(path.display().to_string()),
                        metadata_json: None,
                        parent_session_id: None,
                        is_subagent: false,
                        agent_id: None,
                        parent_tool_use_id: None,
                    })
                    .await
            );
        }
        database
            .set_parse_offset(
                cursor_path.to_str().unwrap(),
                crate::global_db::ParseOffset {
                    byte_offset: 4,
                    mtime: 100,
                    file_id: 0,
                },
            )
            .await
            .unwrap();
        database
            .set_parse_offset(
                claude_path.to_str().unwrap(),
                crate::global_db::ParseOffset {
                    byte_offset: 20,
                    mtime: 200,
                    file_id: 0,
                },
            )
            .await
            .unwrap();
        let options = || ToolCallRegistryOptions {
            registered_project_session_db: runtime.registered_database_arc(
                crate::application::host_admission::HostAdmissionScope::Project,
            ),
            ..Default::default()
        };
        let status = handle_tool_call_with_registry_and_implicit_project(
            &cg,
            "tracedecay_status",
            json!({
                "format": "json",
                "include_branch_diagnostics": false,
                "include_storage_health": false,
                "include_staleness": false,
            }),
            None,
            None,
            options(),
        )
        .await
        .unwrap();
        let runtime_result = handle_tool_call_with_registry_and_implicit_project(
            &cg,
            "tracedecay_runtime",
            json!({
                "format": "json",
                "session_ingest_health": true,
            }),
            None,
            None,
            options(),
        )
        .await
        .unwrap();
        let parse = |result: ToolResult| {
            serde_json::from_str::<Value>(
                result.value["content"][0]["text"]
                    .as_str()
                    .expect("tool JSON text"),
            )
            .expect("parse tool JSON")
        };
        let status = parse(status);
        let runtime_result = parse(runtime_result);

        assert_eq!(
            status["session_ingest"],
            runtime_result["cursor_session_ingest"]
        );
        assert_eq!(status["session_ingest"]["tracked_transcripts"], 1);
        assert_eq!(status["session_ingest"]["pending_bytes"], 6);
        assert_eq!(status["session_ingest"]["last_ingest_unix"], 100);

        cg.checkpoint().await.unwrap();
        cg.close();
    }

    #[tokio::test]
    async fn unsupported_selector_tool_rejects_explicit_project_selector() {
        let _env_lock = lock_user_data_dir_test_env();
        let dir = TempDir::new().unwrap();
        let _env = SelectorEnv::new(dir.path());
        let project = dir.path().join("active");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), "pub fn active_symbol() {}\n").unwrap();
        let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            &project,
            "project.mcp-unsupported-selector",
        )
        .await
        .unwrap();
        cg.index_all().await.unwrap();

        let err = handle_tool_call(
            &cg,
            "tracedecay_status",
            json!({
                "project_id": "explicit-selector-should-not-fall-open",
            }),
            None,
            None,
        )
        .await
        .expect_err("unsupported selector tools must reject explicit selectors");

        cg.checkpoint().await.unwrap();
        cg.close();

        assert!(
            format!("{err}").contains("does not accept project selectors"),
            "unexpected selector rejection error: {err}"
        );
    }

    #[tokio::test]
    async fn pr9_search_rejects_cross_project_selector() {
        let _env_lock = lock_user_data_dir_test_env();
        let dir = TempDir::new().unwrap();
        let _env = SelectorEnv::new(dir.path());
        let project = dir.path().join("active");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), "pub fn active_symbol() {}\n").unwrap();
        let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            &project,
            "project.mcp-cross-selector",
        )
        .await
        .unwrap();

        let err = handle_tool_call(
            &cg,
            "tracedecay_search",
            json!({
                "project_id": "cross-project-must-not-be-relabelled",
                "query": "target",
            }),
            None,
            None,
        )
        .await
        .expect_err("single-root search must reject project selectors");

        cg.close();
        assert!(
            format!("{err}").contains("does not accept project selectors"),
            "unexpected selector rejection error: {err}"
        );
    }

    #[tokio::test]
    async fn selected_project_retrieve_finds_selected_project_response_handle() {
        const LARGE_RESPONSE_MARKER_COUNT: usize = 200;
        const LAST_RETURNED_RESPONSE_MARKER: usize = 19;

        let _env_lock = lock_user_data_dir_test_env();
        let dir = TempDir::new().unwrap();
        let _env = SelectorEnv::new(dir.path());
        let active_project = dir.path().join("active");
        let target_project = dir.path().join("target");
        fs::create_dir_all(active_project.join("src")).unwrap();
        fs::create_dir_all(target_project.join("src")).unwrap();
        fs::write(
            active_project.join("src/lib.rs"),
            "pub fn active_only_symbol() {}\n",
        )
        .unwrap();

        let mut target_source = String::new();
        let response_padding = "x".repeat(256);
        for i in 0..LARGE_RESPONSE_MARKER_COUNT {
            let _ = writeln!(
                target_source,
                "pub fn selected_project_handle_marker_{i:03}() -> &'static str {{ \"marker-{i:03}-{response_padding}\" }}"
            );
        }
        fs::write(target_project.join("src/lib.rs"), target_source).unwrap();

        let (active, _active_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            &active_project,
            "project.mcp-active-retrieval",
        )
        .await
        .unwrap();
        let (target, _target_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            &target_project,
            "project.mcp-target-retrieval",
        )
        .await
        .unwrap();
        let target = Arc::new(target);
        active.index_all().await.unwrap();
        target.index_all().await.unwrap();
        let target_project_id = target
            .store_layout()
            .identity
            .project_id
            .as_deref()
            .expect("target project should be registered")
            .to_string();

        let registry = SelectorRegistry::open().await;
        let result = handle_tool_call_with_registry_and_implicit_project(
            &active,
            "tracedecay_grep",
            json!({
                "pattern": "selected_project_handle_marker",
                "project_id": target_project_id,
                "max_results": LARGE_RESPONSE_MARKER_COUNT,
                "context_lines": 3,
                "format": "json"
            }),
            None,
            None,
            selector_options(&registry, vec![Arc::clone(&target)]),
        )
        .await
        .unwrap();
        let envelope: Value = serde_json::from_str(
            result.value["content"][0]["text"]
                .as_str()
                .expect("search result text"),
        )
        .expect("truncated search envelope");
        assert_eq!(envelope["truncated"], true);
        let handle = envelope["handle"]
            .as_str()
            .expect("large selected-project search should return a handle");
        let retrieve_instruction = envelope["retrieve_instruction"]
            .as_str()
            .expect("truncated envelope should include retrieve guidance");
        assert!(
            retrieve_instruction.contains("pass the same selector"),
            "selected-project envelopes should tell clients to retrieve from the same project: {retrieve_instruction}"
        );

        let retrieved = handle_tool_call_with_registry_and_implicit_project(
            &active,
            "tracedecay_retrieve",
            json!({
                "handle": handle,
                "project_id": target.store_layout().identity.project_id.as_deref().unwrap(),
                "format": "json"
            }),
            None,
            None,
            selector_options(&registry, vec![Arc::clone(&target)]),
        )
        .await
        .unwrap();
        let payload: Value = serde_json::from_str(
            retrieved.value["content"][0]["text"]
                .as_str()
                .expect("retrieve result text"),
        )
        .expect("retrieve payload");

        assert_eq!(payload["expired"], false);
        assert!(
            payload["content"]
                .as_str()
                .is_some_and(|content| content.contains(&format!(
                    "selected_project_handle_marker_{LAST_RETURNED_RESPONSE_MARKER:03}"
                ))),
            "selected project retrieve should return the full selected-project response: {payload}"
        );
    }

    #[test]
    fn test_tool_definitions_complete() {
        let tools = get_tool_definitions();
        // ast-grep-backed tools are conditionally registered based on the
        // host CLI capabilities they need; agents should never see a tool that
        // will instantly fail. The count and per-tool checks below adapt to
        // the host's capability set.
        let expected_total = super::super::definitions::ALWAYS_REGISTERED_TOOL_COUNT
            + usize::from(super::super::definitions::ast_grep_available());
        assert_eq!(tools.len(), expected_total);
        let compatibility_tools = tools
            .iter()
            .filter(|tool| ApplicationSurfaceOperation::from_tool_name(&tool.name).is_none())
            .collect::<Vec<_>>();
        assert_eq!(
            compatibility_tools.len(),
            102 + usize::from(super::super::definitions::ast_grep_available())
        );
        for tool in compatibility_tools {
            assert!(
                LegacyToolCompatibilityOwner::admits(&tool.name),
                "{} must have an explicit compatibility owner",
                tool.name
            );
        }

        let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(tool_names.contains(&"tracedecay_search"));
        assert!(tool_names.contains(&"tracedecay_move_symbol"));
        assert!(tool_names.contains(&"tracedecay_api_migration_plan"));
        assert!(tool_names.contains(&"tracedecay_api_migration_apply"));
        assert!(tool_names.contains(&"tracedecay_analytics"));
        assert!(tool_names.contains(&"tracedecay_retrieve"));
        assert!(tool_names.contains(&"tracedecay_context"));
        assert!(tool_names.contains(&"tracedecay_callers"));
        assert!(tool_names.contains(&"tracedecay_callees"));
        assert!(tool_names.contains(&"tracedecay_callers_for"));
        assert!(tool_names.contains(&"tracedecay_by_qualified_name"));
        assert!(tool_names.contains(&"tracedecay_signature"));
        assert!(tool_names.contains(&"tracedecay_impls"));
        assert!(tool_names.contains(&"tracedecay_diagnose"));
        assert!(tool_names.contains(&"tracedecay_run_affected_tests"));
        assert!(tool_names.contains(&"tracedecay_derives"));
        assert!(tool_names.contains(&"tracedecay_fact_store"));
        assert!(tool_names.contains(&"tracedecay_fact_feedback"));
        assert!(tool_names.contains(&"tracedecay_memory_status"));
        assert!(tool_names.contains(&"tracedecay_session_refresh"));
        assert!(tool_names.contains(&"tracedecay_message_search"));
        assert!(tool_names.contains(&"tracedecay_impact"));
        assert!(tool_names.contains(&"tracedecay_node"));
        assert!(tool_names.contains(&"tracedecay_status"));
        assert!(tool_names.contains(&"tracedecay_active_project"));
        assert!(tool_names.contains(&"tracedecay_storage_status"));
        assert!(tool_names.contains(&"tracedecay_project_list"));
        assert!(tool_names.contains(&"tracedecay_project_search"));
        assert!(tool_names.contains(&"tracedecay_project_context"));
        assert!(tool_names.contains(&"tracedecay_files"));
        assert!(tool_names.contains(&"tracedecay_affected"));
        assert!(tool_names.contains(&"tracedecay_dead_code"));
        assert!(tool_names.contains(&"tracedecay_diff_context"));
        assert!(tool_names.contains(&"tracedecay_module_api"));
        assert!(tool_names.contains(&"tracedecay_circular"));
        assert!(tool_names.contains(&"tracedecay_hotspots"));
        assert!(tool_names.contains(&"tracedecay_similar"));
        assert!(tool_names.contains(&"tracedecay_rename_preview"));
        assert!(tool_names.contains(&"tracedecay_unused_imports"));
        assert!(tool_names.contains(&"tracedecay_changelog"));
        assert!(tool_names.contains(&"tracedecay_rank"));
        assert!(tool_names.contains(&"tracedecay_largest"));
        assert!(tool_names.contains(&"tracedecay_coupling"));
        assert!(tool_names.contains(&"tracedecay_inheritance_depth"));
        assert!(tool_names.contains(&"tracedecay_distribution"));
        assert!(tool_names.contains(&"tracedecay_recursion"));
        assert!(tool_names.contains(&"tracedecay_complexity"));
        assert!(tool_names.contains(&"tracedecay_doc_coverage"));
        assert!(tool_names.contains(&"tracedecay_god_class"));
        assert!(tool_names.contains(&"tracedecay_port_status"));
        assert!(tool_names.contains(&"tracedecay_port_order"));
        assert!(tool_names.contains(&"tracedecay_commit_context"));
        assert!(tool_names.contains(&"tracedecay_pr_context"));
        assert!(tool_names.contains(&"tracedecay_simplify_scan"));
        assert!(tool_names.contains(&"tracedecay_test_map"));
        assert!(tool_names.contains(&"tracedecay_type_hierarchy"));
        assert!(tool_names.contains(&"tracedecay_branch_search"));
        assert!(tool_names.contains(&"tracedecay_branch_diff"));
        assert!(tool_names.contains(&"tracedecay_branch_list"));
        assert!(tool_names.contains(&"tracedecay_str_replace"));
        assert!(tool_names.contains(&"tracedecay_multi_str_replace"));
        assert!(tool_names.contains(&"tracedecay_insert_at"));
        // Structural search runs in-process (bundled grammars), so it is always
        // advertised — unlike the CLI-backed rewrite tool gated just below.
        assert!(tool_names.contains(&"tracedecay_ast_grep_search"));
        if super::super::definitions::ast_grep_available() {
            assert!(tool_names.contains(&"tracedecay_ast_grep_rewrite"));
        } else {
            assert!(!tool_names.contains(&"tracedecay_ast_grep_rewrite"));
        }
        assert!(tool_names.contains(&"tracedecay_gini"));
        assert!(tool_names.contains(&"tracedecay_dependency_depth"));
        assert!(tool_names.contains(&"tracedecay_health"));
        assert!(tool_names.contains(&"tracedecay_redundancy"));
        assert!(tool_names.contains(&"tracedecay_runtime"));
        assert!(tool_names.contains(&"tracedecay_dsm"));
        assert!(tool_names.contains(&"tracedecay_test_risk"));
        assert!(tool_names.contains(&"tracedecay_session_start"));
        assert!(tool_names.contains(&"tracedecay_session_end"));
        assert!(tool_names.contains(&"tracedecay_body"));
        assert!(tool_names.contains(&"tracedecay_todos"));
        assert!(tool_names.contains(&"tracedecay_fact_store"));
        assert!(tool_names.contains(&"tracedecay_fact_feedback"));
        assert!(tool_names.contains(&"tracedecay_memory_status"));
        assert!(tool_names.contains(&"tracedecay_dashboard"));
        assert!(tool_names.contains(&"tracedecay_message_search"));
        assert!(tool_names.contains(&"tracedecay_sessions_for"));
        assert!(tool_names.contains(&"tracedecay_workflows"));
        assert!(tool_names.contains(&"tracedecay_lcm_status"));
        assert!(tool_names.contains(&"tracedecay_lcm_doctor"));
        assert!(tool_names.contains(&"tracedecay_lcm_load_session"));
        assert!(tool_names.contains(&"tracedecay_lcm_grep"));
        assert!(tool_names.contains(&"tracedecay_lcm_describe"));
        assert!(tool_names.contains(&"tracedecay_lcm_expand"));
        assert!(tool_names.contains(&"tracedecay_lcm_expand_query"));
        assert!(tool_names.contains(&"tracedecay_lcm_preflight"));
        assert!(tool_names.contains(&"tracedecay_lcm_compress"));
        assert!(tool_names.contains(&"tracedecay_lcm_session_boundary"));
        assert!(tool_names.contains(&"tracedecay_read"));
        assert!(tool_names.contains(&"tracedecay_outline"));
        assert!(tool_names.contains(&"tracedecay_implementations"));
        assert!(tool_names.contains(&"tracedecay_unsafe_patterns"));
        assert!(tool_names.contains(&"tracedecay_diagnostics"));
        assert!(tool_names.contains(&"tracedecay_config"));
        assert!(tool_names.contains(&"tracedecay_signature_search"));
        assert!(tool_names.contains(&"tracedecay_constructors"));
        assert!(tool_names.contains(&"tracedecay_field_sites"));
        assert!(tool_names.contains(&"tracedecay_call_chain"));
        assert!(tool_names.contains(&"tracedecay_file_dependents"));
        assert!(tool_names.contains(&"tracedecay_replace_symbol"));
        assert!(tool_names.contains(&"tracedecay_insert_at_symbol"));
        assert!(tool_names.contains(&"tracedecay_move_symbol"));
        assert!(tool_names.contains(&"tracedecay_api_migration_plan"));
        assert!(tool_names.contains(&"tracedecay_api_migration_apply"));
        assert!(tool_names.contains(&"tracedecay_source_edit_reconcile"));
        assert!(tool_names.contains(&"tracedecay_find_exact_symbol"));
    }

    #[test]
    fn test_tool_definitions_have_schemas() {
        let tools = get_tool_definitions();
        for tool in &tools {
            assert!(!tool.name.is_empty());
            assert!(!tool.description.is_empty());
            assert!(tool.input_schema.is_object());
            assert_eq!(tool.input_schema["type"], "object");
        }
    }

    #[test]
    fn format_capable_tools_advertise_markdown_json_without_tables() {
        let tools = get_tool_definitions();
        for tool_name in super::super::definitions::format_capable_tool_names() {
            if *tool_name == "tracedecay_ast_grep_rewrite"
                && !super::super::definitions::ast_grep_available()
            {
                continue;
            }
            let tool = tools
                .iter()
                .find(|tool| tool.name == *tool_name)
                .unwrap_or_else(|| panic!("{tool_name} missing tool definition"));
            let format = &tool.input_schema["properties"]["format"];
            assert_eq!(
                format["enum"],
                json!(["markdown", "json"]),
                "{tool_name} should expose markdown/json format choices"
            );
            let description = format["description"]
                .as_str()
                .unwrap_or_else(|| panic!("{tool_name} format must have a description"));
            assert!(
                description.contains("Default 'markdown'"),
                "{tool_name} should document Markdown as default: {description}"
            );
            assert!(
                description.contains("no tables"),
                "{tool_name} should advertise no-table Markdown: {description}"
            );
            assert!(
                !description.contains("prose/tables"),
                "{tool_name} should not advertise table-heavy Markdown: {description}"
            );
        }
    }

    #[test]
    fn every_advertised_application_surface_uses_canonical_output_formats() {
        let tools = get_tool_definitions();
        for operation in APPLICATION_SURFACE_OPERATIONS {
            let tool_name = format!("tracedecay_{}", operation.as_str());
            if super::super::definitions::UNADVERTISED_HANDLE_GATED_TOOL_NAMES
                .contains(&tool_name.as_str())
            {
                continue;
            }
            let tool = tools
                .iter()
                .find(|tool| tool.name == tool_name)
                .unwrap_or_else(|| panic!("{tool_name} missing tool definition"));
            assert_eq!(
                tool.input_schema["properties"]["format"]["enum"],
                json!(["markdown", "json"]),
                "{tool_name} must expose the canonical output formats"
            );
        }
    }

    #[test]
    fn redundancy_tool_definition_describes_ranking_contract() {
        let tools = get_tool_definitions();
        let tool = tools
            .iter()
            .find(|tool| tool.name == "tracedecay_redundancy")
            .expect("tracedecay_redundancy tool definition");
        // Assert only literal output keys — free prose in the description may
        // be reworded without breaking the ranking contract.
        for required in [
            "ranking_score",
            "body_vector_cosine",
            "generic_helper_downranked",
        ] {
            assert!(
                tool.description.contains(required),
                "redundancy definition should mention {required}: {}",
                tool.description
            );
        }
    }

    #[test]
    fn test_tool_definitions_have_annotations() {
        let tools = get_tool_definitions();
        let write_tools = [
            "tracedecay_str_replace",
            "tracedecay_multi_str_replace",
            "tracedecay_insert_at",
            "tracedecay_replace_symbol",
            "tracedecay_insert_at_symbol",
            "tracedecay_move_symbol",
            "tracedecay_ast_grep_rewrite",
            "tracedecay_api_migration_apply",
            "tracedecay_source_edit_reconcile",
            "tracedecay_git_apply",
            "tracedecay_run_affected_tests",
            "tracedecay_session_start",
            "tracedecay_session_end",
            "tracedecay_fact_store",
            "tracedecay_fact_feedback",
            "tracedecay_memory_status",
            "tracedecay_session_refresh",
            "tracedecay_configuration_set",
            "tracedecay_configuration_unset",
            "tracedecay_configuration_batch",
            "tracedecay_configuration_write_credential",
            "tracedecay_configuration_protected_apply",
            "tracedecay_configuration_rollback_apply",
            "tracedecay_context_scout_pause",
            "tracedecay_context_scout_resume",
            "tracedecay_context_scout_cancel",
            "tracedecay_context_scout_claim",
            "tracedecay_context_scout_delivery",
            "tracedecay_context_scout_feedback",
            "tracedecay_lcm_doctor",
            "tracedecay_lcm_preflight",
            "tracedecay_lcm_compress",
            "tracedecay_lcm_session_boundary",
        ];
        for tool in &tools {
            let ann = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("{} missing annotations", tool.name));
            if write_tools.contains(&tool.name.as_str()) {
                assert_eq!(
                    ann["readOnlyHint"], false,
                    "{} should have readOnlyHint=false",
                    tool.name
                );
            } else {
                assert_eq!(
                    ann["readOnlyHint"], true,
                    "{} missing readOnlyHint",
                    tool.name
                );
            }
            assert!(
                ann["title"].is_string(),
                "{} missing title annotation",
                tool.name
            );
        }
    }

    #[test]
    fn test_always_load_tools() {
        let tools = get_tool_definitions();
        let always_load: Vec<&str> = tools
            .iter()
            .filter(|t| {
                t.meta
                    .as_ref()
                    .and_then(|m| m.get("anthropic/alwaysLoad"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            })
            .map(|t| t.name.as_str())
            .collect();
        assert!(
            always_load.contains(&"tracedecay_context"),
            "tracedecay_context must be alwaysLoad"
        );
        assert!(
            always_load.contains(&"tracedecay_search"),
            "tracedecay_search must be alwaysLoad"
        );
        assert!(
            always_load.contains(&"tracedecay_status"),
            "tracedecay_status must be alwaysLoad"
        );
        assert!(
            always_load.contains(&"tracedecay_active_project"),
            "tracedecay_active_project must be alwaysLoad"
        );
        assert!(
            always_load.contains(&"tracedecay_storage_status"),
            "tracedecay_storage_status must be alwaysLoad"
        );
        // grep and callers cover the two most common native-tool reflexes
        // (content search and "who calls this"), so they join the always-loaded
        // set to keep the model from ToolSearch-ing before reaching for Bash.
        assert!(
            always_load.contains(&"tracedecay_grep"),
            "tracedecay_grep must be alwaysLoad"
        );
        assert!(
            always_load.contains(&"tracedecay_callers"),
            "tracedecay_callers must be alwaysLoad"
        );
        assert_eq!(
            always_load.len(),
            7,
            "exactly 7 tools should be alwaysLoad (cap), got {:?}",
            always_load
        );
    }

    #[test]
    fn test_tool_definitions_serializable() {
        let tools = get_tool_definitions();
        let json = serde_json::to_string(&tools).unwrap();
        assert!(json.contains("tracedecay_search"));
        assert!(json.contains("tracedecay_status"));
    }
}
