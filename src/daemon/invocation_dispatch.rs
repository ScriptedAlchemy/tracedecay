//! Dispatch of one authenticated daemon invocation.
//!
//! Validates multi-root payloads before they cost a project admission,
//! resolves the roots they name, and runs the invocation on the Unix and
//! portable executors.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;
#[cfg(any(not(unix), test))]
use crate::daemon_contract::DaemonInvocationProblem;

/// Multi-root payloads are routed by `invoke_for_project`, which reaches the
/// executor without passing through `DaemonInvocationService::invoke`'s own
/// `validate` gate. Validating them here keeps a malformed multi-root request
/// from costing a project admission before it is rejected; authorization stays
/// with the `AuthorizedScopeSet` compare-and-swap on the executor side.
pub(super) fn invalid_multi_root_invocation_response(
    request: &DaemonInvocationRequest,
) -> Option<DaemonInvocationResponse> {
    let multi_root_payload = matches!(
        &request.payload,
        service::invocation::DaemonInvocationPayload::MultiRootScopeSetRead { .. }
            | service::invocation::DaemonInvocationPayload::MultiRootScopeSetCompareAndSwap { .. }
            | service::invocation::DaemonInvocationPayload::MultiRootExecute { .. }
    );
    if !multi_root_payload {
        return None;
    }
    request
        .validate()
        .err()
        .map(|problem| DaemonInvocationResponse::problem(request.request_id.clone(), problem))
}

#[cfg(any(not(unix), test))]
pub(super) async fn execute_portable_daemon_invocation(
    lifecycle: DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    handshake: &DaemonHandshake,
    invocation: &DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    request: DaemonInvocationRequest,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> DaemonInvocationResponse {
    if let Some(response) = invalid_multi_root_invocation_response(&request) {
        return response;
    }
    let request_id = request.request_id.clone();
    let git_operation = invocation_is_git_operation(request.operation());
    let mut project_path = None;
    if request.requires_project() {
        if Box::pin(portable_project_server_for_request(
            lifecycle,
            store_administration.clone(),
            project_open_gates,
            invocation.clone(),
            http_application_registry,
            handshake,
            ProjectServerRequirement::Core,
            #[cfg(test)]
            project_open_attempts,
        ))
        .await
        .is_err()
        {
            return DaemonInvocationResponse::problem(
                request_id,
                if git_operation {
                    DaemonInvocationProblem::NotFoundOrNotAuthorized
                } else {
                    DaemonInvocationProblem::Unavailable
                },
            );
        }
        let Ok((resolved_project_path, _)) = project_route_for_handshake(handshake) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        if admitted_lsp_root_for_project_path(&resolved_project_path).is_none() {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
        project_path = Some(resolved_project_path);
    }
    invocation
        .invoke_for_project(&store_administration, project_path.as_deref(), request)
        .await
}

pub(super) async fn git_service_for_project_path(
    store_administration: &StoreAdministration,
    project_path: Option<&Path>,
) -> Option<git_transactions::DaemonGitInvocationOwner> {
    let project_path = project_path?;
    let repository_root = crate::worktree::git_worktree_root(project_path)
        .unwrap_or_else(|| project_path.to_path_buf());
    store_administration
        .git_index_transaction_services()
        .for_repository_root(&repository_root)
        .await
        .ok()
        .flatten()
}

#[cfg(unix)]
pub(super) async fn write_tool_list_changed_notification(
    transport: &mut impl McpTransport,
) -> Result<()> {
    let notification = json!({
        "jsonrpc": "2.0",
        "method": TOOL_LIST_CHANGED_METHOD,
    });
    transport
        .write_line(&format!("{}\n", serde_json::to_string(&notification)?))
        .await?;
    transport.flush().await?;
    Ok(())
}

pub(super) async fn resolve_multi_root_projects(
    store_administration: &StoreAdministration,
    service: &service::invocation::DaemonInvocationService,
    project_ids: &[tracedecay_domain::ProjectId],
) -> std::result::Result<
    Vec<(PathBuf, tracedecay_application::ResolvedScope)>,
    service::invocation::DaemonInvocationProblem,
> {
    let database = store_administration
        .registered_profile_database()
        .await
        .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
    let mut roots = Vec::with_capacity(project_ids.len());
    for project_id in project_ids {
        let context = database
            .project_registry_context_by_id(project_id.as_str())
            .await
            .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?
            .ok_or(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized)?;
        if context.project.project_id != project_id.as_str() {
            return Err(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized);
        }
        let root = PathBuf::from(context.project.canonical_root);
        if !root.is_absolute() || root.canonicalize().ok().as_ref() != Some(&root) {
            return Err(service::invocation::DaemonInvocationProblem::Unavailable);
        }
        let scope = project_open_owners::resolved_scope_for_project(&root, project_id)
            .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
        if !service.lsp_owner_matches_scope(&root, &scope).await {
            return Err(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized);
        }
        roots.push((root, scope));
    }
    roots.sort_by(|left, right| left.1.scope_digest.cmp(&right.1.scope_digest));
    if roots
        .windows(2)
        .any(|pair| pair[0].1.scope_digest == pair[1].1.scope_digest)
    {
        return Err(service::invocation::DaemonInvocationProblem::InvalidRequest);
    }
    Ok(roots)
}

#[cfg(unix)]
pub(super) async fn execute_daemon_invocation(
    engine: &DaemonEngine,
    handshake: &DaemonHandshake,
    request: DaemonInvocationRequest,
) -> DaemonInvocationResponse {
    if let Some(response) = invalid_multi_root_invocation_response(&request) {
        return response;
    }
    let request_id = request.request_id.clone();
    let git_operation = invocation_is_git_operation(request.operation());
    let mut project_path = None;
    if request.requires_project() {
        if engine
            .project_server_for_request(handshake, ProjectServerRequirement::Core)
            .await
            .is_err()
        {
            return DaemonInvocationResponse::problem(
                request_id,
                if git_operation {
                    service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized
                } else {
                    service::invocation::DaemonInvocationProblem::Unavailable
                },
            );
        }
        let Ok((resolved_project_path, _)) = DaemonEngine::project_route(handshake) else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        if admitted_lsp_root_for_project_path(&resolved_project_path).is_none() {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::Unavailable,
            );
        }
        project_path = Some(resolved_project_path);
    }
    engine
        .invocation
        .invoke_for_project(
            &engine.store_administration,
            project_path.as_deref(),
            request,
        )
        .await
}
