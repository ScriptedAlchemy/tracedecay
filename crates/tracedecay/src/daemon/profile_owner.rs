//! The daemon's profile owner.
//!
//! It answers the graph-tool requests that name no project, only the
//! authenticated profile, for every transport: CLI invocations, project MCP
//! servers, and projectless connections. `tracedecay_project_list`,
//! `tracedecay_project_search`, and `tracedecay_project_context` read the
//! profile's registry; `tracedecay_admin_project` reconciles every cached
//! automation scheduler of the profile; `tracedecay_hook_runtime` records the
//! session evidence of a hook with no project route. The caller's project,
//! when it has one, only marks that project active in registry reads and is
//! the context read's default.
//!
//! An operation joins by naming the requests the profile owner answers in
//! `ApplicationSurfaceOperation::is_profile_owner_request` and computing its
//! typed result in [`compute_profile_owner_operation`].

use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use serde_json::{Map, Value};
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    AdminProjectResultV1, AdminProjectSurfaceRequestV1, AutomationReconcileScope,
    HookRuntimeResultV1, ProfileAutomationReconcileReport, UncachedProjectReconcileOutcome,
};
use tracedecay_contracts::{
    ApplicationProblem, CancellationContext, CancellationStage, CancellationState, Deadline,
    ResolvedScope,
};
use tracedecay_daemon_identity::profile_identity::LocalProfileIdentityAuthorityV1;
use tracedecay_daemon_protocol::{
    DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationResponse,
};
use tracedecay_daemon_service::DaemonProjectRegistryReadService;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_mcp::handlers::{
    SessionAuthorities, decode_primitive_request, hook_runtime, unknown_tool_error,
};
use tracedecay_mcp::server::join_hook_ingest_refresh;
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
        ApplicationSurfaceOperation::HookRuntime => GraphToolResultV1::HookRuntime(
            Box::pin(profile_hook_runtime(
                store_administration,
                profile_identity,
                arguments,
            ))
            .await?,
        ),
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

/// One hook action with no project route. It lands in the profile's user
/// session store, and a transcript ingest answers only once the profile's
/// refresh owner has published what it wrote.
async fn profile_hook_runtime(
    store_administration: &StoreAdministration,
    profile_identity: &LocalProfileIdentityAuthorityV1,
    arguments: Map<String, Value>,
) -> Result<HookRuntimeResultV1> {
    let request = hook_runtime::decode_hook_runtime_request(&Value::Object(arguments))?;
    let global_db = Box::pin(store_administration.registered_profile_database()).await?;
    let user_session_db =
        Box::pin(store_administration.registered_profile_session_database()).await?;
    let host_admission_broker =
        Box::pin(store_administration.host_admission_broker(&user_session_db)).await?;
    let schedulers = store_administration.session_temporal_refresh_schedulers();
    let refresh_wake = Box::pin(schedulers.ensure_profile(
        user_session_db.db_path().to_path_buf(),
        user_session_db.clone(),
    ))
    .await;
    let result = Box::pin(hook_runtime::compute_projectless_hook_runtime(
        request,
        profile_identity.profile_root(),
        global_db.as_ref(),
        SessionAuthorities::new(None, Some(&user_session_db))
            .with_profile_identity(Some(Arc::new(profile_identity.clone())))
            .with_background_cpu(schedulers.background_cpu()),
        Ok(&host_admission_broker),
    ))
    .await?;
    if let HookRuntimeResultV1::IngestTranscript(ingest) = &result {
        join_hook_ingest_refresh(ingest.user_scope, None, Some(&refresh_wake)).await?;
    } else {
        refresh_wake.wake();
    }
    Ok(result)
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
