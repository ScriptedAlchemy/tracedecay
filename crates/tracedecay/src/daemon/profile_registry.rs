//! The daemon's owner of the profile project-registry reads.
//!
//! `tracedecay_project_list`, `tracedecay_project_search`, and
//! `tracedecay_project_context` read the authenticated profile's registry. They
//! name no project, so the composition root answers them from the profile's
//! store administration for every transport: CLI invocations, project MCP
//! servers, and projectless connections. The caller's project, when it has
//! one, only marks that project active and is the context read's default.

use std::future::Future;
use std::path::Path;

use serde_json::{Map, Value};
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
use tracedecay_mcp::tools::dispatch_ceiling::{tool_dispatch_budget, tool_dispatch_deadline_error};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use super::StoreAdministration;
use super::profile_retained::profile_session_scope;
use crate::mcp::tools::graph_tool_error_problem;

/// Serve one profile registry read from the daemon's pinned profile.
#[allow(clippy::too_many_arguments)]
#[hotpath::measure(label = "daemon.profile_registry.invoke", future = true)]
pub(super) async fn invoke_profile_registry_read(
    store_administration: &StoreAdministration,
    active_project_root: Option<&Path>,
    request_id: String,
    operation: ApplicationSurfaceOperation,
    arguments: Map<String, Value>,
    deadline: Deadline,
    cancellation: CancellationContext,
    request_cancellation: Option<CancellationToken>,
) -> DaemonInvocationResponse {
    let authority = async {
        let scope = profile_session_scope(store_administration.profile_identity()?)?;
        let registry = Box::pin(store_administration.registered_profile_database()).await?;
        Ok((scope, registry))
    };
    answer_profile_registry_read(
        authority,
        active_project_root,
        request_id,
        operation,
        arguments,
        deadline,
        cancellation,
        request_cancellation,
    )
    .await
}

/// Answer one profile registry read from `authority`, the profile session
/// scope and registered profile registry, and settle its typed terminal.
///
/// `request_cancellation` is the daemon's registered cancellation for this
/// request; a cancelled read settles as the typed cancelled problem.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn answer_profile_registry_read(
    authority: impl Future<Output = Result<(ResolvedScope, RegisteredGlobalDbLeaseV1)>> + Send,
    active_project_root: Option<&Path>,
    request_id: String,
    operation: ApplicationSurfaceOperation,
    arguments: Map<String, Value>,
    deadline: Deadline,
    cancellation: CancellationContext,
    request_cancellation: Option<CancellationToken>,
) -> DaemonInvocationResponse {
    if matches!(cancellation.state, CancellationState::Cancelled { .. }) {
        return DaemonInvocationResponse::application_problem(
            request_id,
            ApplicationProblem::cancelled_before_admission(),
        );
    }
    if !operation.is_profile_registry_read() {
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
    let read = async {
        let (scope, registry) = authority.await?;
        let registry = DaemonProjectRegistryReadService::new(registry);
        let completion = tracedecay_mcp::handlers::info::compute_registry_read(
            &registry,
            active_project_root,
            operation,
            Value::Object(arguments),
        )
        .await?;
        Ok::<_, TraceDecayError>((scope, completion))
    };
    let bounded = async {
        match tokio::time::timeout(budget, read).await {
            Ok(result) => result,
            Err(_elapsed) => Err(tool_dispatch_deadline_error(tool_name, budget)),
        }
    };
    let cancelled = async {
        match request_cancellation {
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
