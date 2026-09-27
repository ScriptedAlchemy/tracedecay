//! The daemon's profile owner.
//!
//! It answers the graph-tool requests that name no project, only the
//! authenticated profile, for every transport: CLI invocations, project MCP
//! servers, and projectless connections. `tracedecay_project_list`,
//! `tracedecay_project_search`, and `tracedecay_project_context` read the
//! profile's registry; `tracedecay_admin_project` reconciles every cached
//! automation scheduler of the profile; `tracedecay_admin_cli` answers the
//! registry, storage, savings, and profile-wide cost and analytics actions.
//! The caller's project, when it has one, only marks that project active in
//! registry reads and is the context read's default.
//!
//! An operation joins by naming the requests the profile owner answers in
//! `ApplicationSurfaceOperation::is_profile_owner_request` and computing its
//! typed result in [`compute_profile_owner_operation`].

use std::future::Future;
use std::path::Path;

use serde_json::{Map, Value};
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    AdminCliSurfaceRequestV1, AdminProjectResultV1, AdminProjectSurfaceRequestV1,
    AutomationReconcileScope, ProfileAutomationReconcileReport, UncachedProjectReconcileOutcome,
};
use tracedecay_contracts::{
    ApplicationProblem, CancellationContext, CancellationStage, CancellationState, Deadline,
    ResolvedScope,
};
use tracedecay_daemon_protocol::{
    DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationResponse,
};
use tracedecay_daemon_service::DaemonProjectRegistryReadService;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_mcp::handlers::{decode_primitive_request, unknown_tool_error};
use tracedecay_mcp::tools::dispatch_ceiling::{tool_dispatch_budget, tool_dispatch_deadline_error};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use super::StoreAdministration;
use super::profile_retained::profile_session_scope;
use crate::mcp::tools::graph_tool_error_problem;

/// Serve one profile-owner request from the daemon's pinned profile.
#[allow(clippy::too_many_arguments)]
#[hotpath::measure(label = "daemon.profile_owner.invoke", future = true)]
pub(super) async fn invoke_profile_owner_operation(
    store_administration: &StoreAdministration,
    active_project_root: Option<&Path>,
    request_id: String,
    operation: ApplicationSurfaceOperation,
    arguments: Map<String, Value>,
    deadline: Deadline,
    cancellation: CancellationContext,
    request_cancellation: Option<CancellationToken>,
) -> DaemonInvocationResponse {
    answer_profile_owner_operation(
        request_id,
        operation,
        arguments,
        deadline,
        cancellation,
        request_cancellation,
        |arguments| {
            compute_profile_owner_operation(
                store_administration,
                active_project_root,
                operation,
                arguments,
            )
        },
    )
    .await
}

/// The profile owner's typed result for one admitted request, under the
/// profile session scope it reports.
async fn compute_profile_owner_operation(
    store_administration: &StoreAdministration,
    active_project_root: Option<&Path>,
    operation: ApplicationSurfaceOperation,
    arguments: Map<String, Value>,
) -> Result<(ResolvedScope, GraphToolCompletionV1)> {
    let profile_identity = store_administration.profile_identity()?;
    let scope = profile_session_scope(profile_identity)?;
    let result = match operation {
        ApplicationSurfaceOperation::ProjectList
        | ApplicationSurfaceOperation::ProjectSearch
        | ApplicationSurfaceOperation::ProjectContext => {
            let registry = Box::pin(store_administration.registered_profile_database()).await?;
            let completion =
                registry_read(registry, active_project_root, operation, arguments).await?;
            return Ok((scope, completion));
        }
        ApplicationSurfaceOperation::AdminProject => {
            let request: AdminProjectSurfaceRequestV1 =
                decode_primitive_request(&Value::Object(arguments), operation.mcp_tool_name())?;
            let AdminProjectSurfaceRequestV1::AutomationReconcile {
                scope: AutomationReconcileScope::Profile,
            } = request
            else {
                return Err(TraceDecayError::project_route(
                    crate::daemon::PROJECT_REQUIRED_REASON_CODE,
                    false,
                    "this tracedecay_admin_project action requires an initialized code project; \
                     run it inside an initialized project or pass --project <path>",
                ));
            };
            let outcomes = Box::pin(
                store_administration
                    .reconcile_cached_automation_for_profile(profile_identity.profile_root()),
            )
            .await?;
            GraphToolResultV1::AdminProject(Box::new(
                AdminProjectResultV1::ProfileAutomationReconcile(
                    ProfileAutomationReconcileReport {
                        scope: AutomationReconcileScope::Profile,
                        cached_owners: outcomes.len(),
                        outcomes,
                        uncached_projects:
                            UncachedProjectReconcileOutcome::DeferredUntilProjectStartup,
                    },
                ),
            ))
        }
        ApplicationSurfaceOperation::AdminCli => {
            let request: AdminCliSurfaceRequestV1 =
                decode_primitive_request(&Value::Object(arguments), operation.mcp_tool_name())?;
            let registry = Box::pin(store_administration.registered_profile_database()).await?;
            GraphToolResultV1::AdminCli(Box::new(
                Box::pin(
                    tracedecay_mcp::handlers::admin_cli::compute_projectless_admin_cli(
                        request,
                        &registry,
                        tracedecay_global_db::global_accounting_enabled()
                            .then_some(registry.as_ref()),
                        profile_identity.profile_root(),
                        active_project_root,
                    ),
                )
                .await?,
            ))
        }
        operation => return Err(unknown_tool_error(operation.mcp_tool_name())),
    };
    Ok((
        scope,
        GraphToolCompletionV1 {
            result,
            touched_files: Vec::new(),
            code_graph: None,
            analytics: None,
            cost: None,
        },
    ))
}

/// One registry read against `registry`.
pub(crate) async fn registry_read(
    registry: RegisteredGlobalDbLeaseV1,
    active_project_root: Option<&Path>,
    operation: ApplicationSurfaceOperation,
    arguments: Map<String, Value>,
) -> Result<GraphToolCompletionV1> {
    tracedecay_mcp::handlers::info::compute_registry_read(
        &DaemonProjectRegistryReadService::new(registry),
        active_project_root,
        operation,
        Value::Object(arguments),
    )
    .await
}

/// Admit one profile-owner request, run `compute` within its dispatch
/// ceiling, and settle its typed terminal.
///
/// `request_cancellation` is the daemon's registered cancellation for this
/// request; a cancelled read settles as the typed cancelled problem.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn answer_profile_owner_operation<Compute, Computed>(
    request_id: String,
    operation: ApplicationSurfaceOperation,
    arguments: Map<String, Value>,
    deadline: Deadline,
    cancellation: CancellationContext,
    request_cancellation: Option<CancellationToken>,
    compute: Compute,
) -> DaemonInvocationResponse
where
    Compute: FnOnce(Map<String, Value>) -> Computed,
    Computed: Future<Output = Result<(ResolvedScope, GraphToolCompletionV1)>> + Send,
{
    if matches!(cancellation.state, CancellationState::Cancelled { .. }) {
        return DaemonInvocationResponse::application_problem(
            request_id,
            ApplicationProblem::cancelled_before_admission(),
        );
    }
    if !operation.is_profile_owner_request(&arguments) {
        return DaemonInvocationResponse::problem(
            request_id,
            DaemonInvocationProblem::InvalidRequest,
        );
    }
    let tool_name = operation.mcp_tool_name();
    let Some(budget) = tool_dispatch_budget(tool_name, Some(&deadline)) else {
        return DaemonInvocationResponse::application_problem(
            request_id,
            ApplicationProblem::timed_out_before_admission(),
        );
    };
    let computed = compute(arguments);
    let bounded = async {
        match tokio::time::timeout(budget, computed).await {
            Ok(result) => result,
            Err(_elapsed) => Err(tool_dispatch_deadline_error(tool_name, budget)),
        }
    };
    // A read observes cancellation until it answers; an owner side effect,
    // like a scheduler reconcile, is not interruptible once admitted.
    let observed_cancellation =
        request_cancellation.filter(|_| operation.owner_side_effect().is_none());
    let cancelled = async {
        match observed_cancellation {
            Some(token) => token.cancelled().await,
            None => std::future::pending().await,
        }
    };
    let outcome = tokio::select! {
        biased;
        () = cancelled => {
            return match ApplicationProblem::cancelled(CancellationStage::DuringRead) {
                Ok(problem) => DaemonInvocationResponse::application_problem(request_id, problem),
                Err(_) => DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                ),
            };
        }
        outcome = bounded => outcome,
    };
    match outcome {
        Ok((scope, completion)) => DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::GraphTool { scope, completion },
        ),
        Err(error) => DaemonInvocationResponse::application_problem(
            request_id,
            graph_tool_error_problem(&error),
        ),
    }
}
