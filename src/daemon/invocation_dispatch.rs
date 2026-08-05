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
    let selectors = match &request.payload {
        service::invocation::DaemonInvocationPayload::MultiRootScopeSetCompareAndSwap {
            request,
            ..
        } => Some(request.roots.clone()),
        service::invocation::DaemonInvocationPayload::MultiRootExecute {
            request,
            observed_at,
            deadline,
            cancellation,
        } => {
            let Some(active_project_root) = project_path.as_deref() else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            };
            let scope_set = match store_administration
                .registered_profile_database()
                .await
                .ok()
                .and_then(|database| database.authorized_scope_set_storage().ok())
            {
                Some(storage) => invocation
                    .service
                    .persisted_scope_set(
                        active_project_root,
                        Some(&storage),
                        &request.scope_set_id,
                        tracedecay_application::MultiRootApplicationOperation::Execute,
                        *observed_at,
                        deadline,
                        cancellation,
                    )
                    .await
                    .filter(|scope_set| {
                        scope_set.revision() == request.scope_set_revision
                            && scope_set.digest() == &request.scope_set_digest
                    }),
                None => None,
            };
            let Some(scope_set) = scope_set else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            };
            let selectors = match scope_set
                .roots()
                .iter()
                .map(|root| {
                    let locator = root
                        .locator()
                        .ok_or(DaemonInvocationProblem::NotFoundOrNotAuthorized)?;
                    tracedecay_application::RegisteredRootSelectorV1::new(
                        locator.project_id.clone(),
                        &locator.canonical_root,
                    )
                    .map_err(|_| DaemonInvocationProblem::InvalidRequest)
                })
                .collect::<std::result::Result<Vec<_>, _>>()
            {
                Ok(selectors) => selectors,
                Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
            };
            Some(selectors)
        }
        _ => None,
    };
    if let Some(selectors) = selectors {
        let roots = match resolve_multi_root_projects(
            &store_administration,
            &invocation.service,
            &selectors,
        )
        .await
        {
            Ok(roots) => roots,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        for (root, scope, _) in roots {
            let mut root_handshake = handshake.clone();
            root_handshake.project_path = Some(root.clone());
            root_handshake.allow_init = false;
            root_handshake.allow_initialize_root_routing = false;
            if Box::pin(portable_project_server_for_request(
                lifecycle.clone(),
                store_administration.clone(),
                Arc::clone(&project_open_gates),
                invocation.clone(),
                http_application_registry.clone(),
                &root_handshake,
                ProjectServerRequirement::Core,
                #[cfg(test)]
                project_open_attempts.clone(),
            ))
            .await
            .is_err()
                || !invocation
                    .service
                    .lsp_owner_matches_scope(&root, &scope)
                    .await
            {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            }
        }
    }
    invocation
        .invoke_for_project(
            &store_administration,
            project_path.as_deref(),
            None,
            request,
        )
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
    _service: &service::invocation::DaemonInvocationService,
    selectors: &[tracedecay_application::RegisteredRootSelectorV1],
) -> std::result::Result<
    Vec<(
        PathBuf,
        tracedecay_application::ResolvedScope,
        tracedecay_application::RegisteredRootLocatorV1,
    )>,
    service::invocation::DaemonInvocationProblem,
> {
    let database = store_administration
        .registered_profile_database()
        .await
        .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
    let profile_id = store_administration
        .profile_identity()
        .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?
        .profile_id()
        .clone();
    let mut roots = Vec::with_capacity(selectors.len());
    for selector in selectors {
        let context = database
            .project_registry_context_by_id(selector.project_id.as_str())
            .await
            .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?
            .ok_or(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized)?;
        if context.project.project_id != selector.project_id.as_str() {
            return Err(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized);
        }
        let mut stores = context
            .stores
            .iter()
            .filter(|store| store.store.project_id == selector.project_id.as_str());
        let Some(store) = stores.next() else {
            return Err(service::invocation::DaemonInvocationProblem::Unavailable);
        };
        if stores.next().is_some() {
            return Err(service::invocation::DaemonInvocationProblem::Unavailable);
        }
        let registered_root = PathBuf::from(context.project.canonical_root);
        if !registered_root.is_absolute()
            || registered_root.canonicalize().ok().as_ref() != Some(&registered_root)
        {
            return Err(service::invocation::DaemonInvocationProblem::Unavailable);
        }
        let root = selector
            .root
            .canonicalize()
            .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
        tracedecay_usecases::context::RegisteredScopeResolver::resolve(
            &registered_root,
            &root,
            &selector.project_id,
        )
        .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
        let scope = crate::daemon::project_open_owners::resolved_scope_for_project(
            &root,
            &selector.project_id,
        )
        .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
        let locator = tracedecay_application::RegisteredRootLocatorV1::new(
            selector.project_id.clone(),
            profile_id.clone(),
            store.store.store_id.clone(),
            root.clone(),
        )
        .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
        roots.push((root, scope, locator));
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
async fn mount_multi_root_owners(
    engine: &DaemonEngine,
    handshake: &DaemonHandshake,
    active_project_root: &Path,
    request: &DaemonInvocationRequest,
) -> std::result::Result<(), service::invocation::DaemonInvocationProblem> {
    let roots = match &request.payload {
        service::invocation::DaemonInvocationPayload::MultiRootScopeSetCompareAndSwap {
            request,
            ..
        } => {
            resolve_multi_root_projects(
                &engine.store_administration,
                &engine.invocation.service,
                &request.roots,
            )
            .await?
        }
        service::invocation::DaemonInvocationPayload::MultiRootExecute {
            request,
            observed_at,
            deadline,
            cancellation,
        } => {
            let storage = engine
                .store_administration
                .registered_profile_database()
                .await
                .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?
                .authorized_scope_set_storage()
                .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
            let scope_set = engine
                .invocation
                .service
                .persisted_scope_set(
                    active_project_root,
                    Some(&storage),
                    &request.scope_set_id,
                    tracedecay_application::MultiRootApplicationOperation::Execute,
                    *observed_at,
                    deadline,
                    cancellation,
                )
                .await
                .filter(|scope_set| {
                    scope_set.revision() == request.scope_set_revision
                        && scope_set.digest() == &request.scope_set_digest
                })
                .ok_or(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized)?;
            let selectors = scope_set
                .roots()
                .iter()
                .map(|root| {
                    let locator = root.locator().ok_or(
                        service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
                    )?;
                    tracedecay_application::RegisteredRootSelectorV1::new(
                        locator.project_id.clone(),
                        &locator.canonical_root,
                    )
                    .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)
                })
                .collect::<std::result::Result<Vec<_>, _>>()?;
            resolve_multi_root_projects(
                &engine.store_administration,
                &engine.invocation.service,
                &selectors,
            )
            .await?
        }
        _ => return Ok(()),
    };
    for (root, scope, _) in roots {
        if engine
            .invocation
            .service
            .lsp_owner_matches_scope(&root, &scope)
            .await
        {
            continue;
        }
        let mut root_handshake = handshake.clone();
        root_handshake.project_path = Some(root.clone());
        root_handshake.allow_init = false;
        root_handshake.allow_initialize_root_routing = false;
        engine
            .project_server_for_request(&root_handshake, ProjectServerRequirement::Core)
            .await
            .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
        if !engine
            .invocation
            .service
            .lsp_owner_matches_scope(&root, &scope)
            .await
        {
            return Err(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized);
        }
    }
    Ok(())
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
    if let Some(active_project_root) = project_path.as_deref()
        && let Err(problem) =
            mount_multi_root_owners(engine, handshake, active_project_root, &request).await
    {
        return DaemonInvocationResponse::problem(request_id, problem);
    }
    engine
        .invocation
        .invoke_for_project(
            &engine.store_administration,
            project_path.as_deref(),
            None,
            request,
        )
        .await
}
