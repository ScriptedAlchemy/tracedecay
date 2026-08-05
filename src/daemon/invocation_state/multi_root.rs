//! Exact registered-root resolution for daemon multi-root execution.

use std::path::PathBuf;

use super::*;

pub(super) enum AuthorizedRootResolution {
    Denied,
    Unavailable(tracedecay_domain::ScopeUnavailableReasonV1),
}

pub(super) async fn canonical_scope_set_storage(
    store_administration: &StoreAdministration,
) -> Option<tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetSqliteStorage> {
    store_administration
        .registered_profile_database()
        .await
        .ok()?
        .authorized_scope_set_storage()
        .ok()
}

pub(super) async fn resolve_authorized_root(
    store_administration: &StoreAdministration,
    database: &crate::global_db::RegisteredGlobalDb,
    root: &tracedecay_application::AuthorizedRoot,
) -> std::result::Result<PathBuf, AuthorizedRootResolution> {
    let scope = root.scope();
    let locator = root.locator().ok_or(AuthorizedRootResolution::Unavailable(
        tracedecay_domain::ScopeUnavailableReasonV1::AuthorityUnavailable,
    ))?;
    let profile_identity = store_administration.profile_identity().map_err(|_| {
        AuthorizedRootResolution::Unavailable(
            tracedecay_domain::ScopeUnavailableReasonV1::AuthorityUnavailable,
        )
    })?;
    if profile_identity.profile_id() != &locator.profile.profile_id {
        return Err(AuthorizedRootResolution::Unavailable(
            tracedecay_domain::ScopeUnavailableReasonV1::AuthorityUnavailable,
        ));
    }
    let registry_context = database
        .project_registry_context_by_id(locator.project_id.as_str())
        .await
        .map_err(|_| {
            AuthorizedRootResolution::Unavailable(
                tracedecay_domain::ScopeUnavailableReasonV1::StoreUnavailable,
            )
        })?
        .ok_or(AuthorizedRootResolution::Denied)?;
    if registry_context.project.project_id != locator.project_id.as_str()
        || !registry_context.stores.iter().any(|store| {
            store.store.project_id == locator.project_id.as_str()
                && store.store.store_id == locator.profile.store_id
        })
    {
        return Err(AuthorizedRootResolution::Denied);
    }
    let registered_root = PathBuf::from(registry_context.project.canonical_root);
    if !registered_root.is_absolute()
        || registered_root.canonicalize().ok().as_ref() != Some(&registered_root)
    {
        return Err(AuthorizedRootResolution::Unavailable(
            tracedecay_domain::ScopeUnavailableReasonV1::RootMissing,
        ));
    }
    let exact_root = locator.canonical_root.clone();
    if exact_root.canonicalize().ok().as_ref() != Some(&exact_root) {
        return Err(AuthorizedRootResolution::Unavailable(
            tracedecay_domain::ScopeUnavailableReasonV1::RootMissing,
        ));
    }
    tracedecay_usecases::context::RegisteredScopeResolver::resolve(
        &registered_root,
        &exact_root,
        &locator.project_id,
    )
    .map_err(|_| AuthorizedRootResolution::Denied)?;
    let exact_scope =
        project_open_owners::resolved_scope_for_project(&exact_root, &locator.project_id).map_err(
            |_| {
                AuthorizedRootResolution::Unavailable(
                    tracedecay_domain::ScopeUnavailableReasonV1::RootMissing,
                )
            },
        )?;
    if &exact_scope != scope {
        return Err(AuthorizedRootResolution::Denied);
    }
    Ok(exact_root)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_one_multi_root_operation(
    state: &DaemonInvocationState,
    store_administration: &StoreAdministration,
    root: &std::path::Path,
    scope: &tracedecay_application::ResolvedScope,
    ordinal: usize,
    operation: &tracedecay_application::MultiRootOperationV1,
    root_cursor: Option<&tracedecay_application::MultiRootRootContinuationV1>,
    observed_at: tracedecay_domain::UtcMicros,
    deadline: tracedecay_application::Deadline,
    cancellation: tracedecay_application::CancellationContext,
    pinned_generation: &code_index_scheduler::LatestCompleteCodeIndexV1,
) -> std::result::Result<
    (
        Value,
        Option<tracedecay_application::MultiRootRootContinuationV1>,
    ),
    service::invocation::DaemonInvocationProblem,
> {
    match operation {
        tracedecay_application::MultiRootOperationV1::Work { request } => {
            ensure_pinned_generation(state, scope, pinned_generation).await?;
            let mut request = serde_json::from_value::<
                service::invocation::WorkApplicationInvocationV1,
            >(request.clone())
            .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)?;
            if !matches!(
                request,
                service::invocation::WorkApplicationInvocationV1::Snapshot(_)
                    | service::invocation::WorkApplicationInvocationV1::Delta(_)
            ) {
                return Err(service::invocation::DaemonInvocationProblem::InvalidRequest);
            }
            if let Some(root_cursor) = root_cursor {
                let tracedecay_application::MultiRootRootContinuationV1::Work(cursor) = root_cursor
                else {
                    return Err(service::invocation::DaemonInvocationProblem::InvalidRequest);
                };
                let page_size = match &request {
                    service::invocation::WorkApplicationInvocationV1::Snapshot(request) => {
                        request.page_size
                    }
                    service::invocation::WorkApplicationInvocationV1::Delta(request) => {
                        request.page_size
                    }
                    _ => return Err(service::invocation::DaemonInvocationProblem::InvalidRequest),
                };
                request = service::invocation::WorkApplicationInvocationV1::Delta(
                    tracedecay_application::WorkProjectionDeltaRequestV1 {
                        cursor: cursor.clone(),
                        page_size,
                    },
                );
            }
            let response = Box::pin(state.invoke_for_project(
                store_administration,
                Some(root),
                Some((scope.clone(), pinned_generation.clone())),
                DaemonInvocationRequest::work_application(
                    format!("request.multi-root.work.{ordinal}"),
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                ),
            ))
            .await;
            let service::invocation::DaemonInvocationOutcome::WorkApplication {
                scope: actual_scope,
                outcome,
            } = response.outcome
            else {
                return Err(service::invocation::DaemonInvocationProblem::Unavailable);
            };
            if &actual_scope != scope {
                return Err(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized);
            }
            let result = extract_work_application_payload(&outcome)?;
            ensure_pinned_generation(state, scope, pinned_generation).await?;
            Ok(result)
        }
        tracedecay_application::MultiRootOperationV1::Git { request }
        | tracedecay_application::MultiRootOperationV1::Feedback { request }
        | tracedecay_application::MultiRootOperationV1::Impact { request }
        | tracedecay_application::MultiRootOperationV1::Query { request } => {
            let wire = serde_json::from_value::<FederatedSurfaceRequestV1>(request.clone())
                .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)?;
            if !multi_root_family_allows(operation, wire.operation) {
                return Err(service::invocation::DaemonInvocationProblem::InvalidRequest);
            }
            let generation_owned_query = multi_root_query_uses_generation_owner(wire.operation);
            if !generation_owned_query {
                ensure_pinned_generation(state, scope, pinned_generation).await?;
            }
            let page_cursor = match root_cursor {
                Some(tracedecay_application::MultiRootRootContinuationV1::Page(cursor)) => {
                    Some(cursor.clone())
                }
                Some(
                    tracedecay_application::MultiRootRootContinuationV1::Work(_)
                    | tracedecay_application::MultiRootRootContinuationV1::Complete,
                ) => return Err(service::invocation::DaemonInvocationProblem::InvalidRequest),
                None => None,
            };
            let (payload, cursor) = crate::application_surface::invoke_multi_root_surface_request(
                Arc::new(InProcessDaemonInvocationExecutor::new_pinned(
                    state.clone(),
                    store_administration.clone(),
                    root.to_path_buf(),
                    scope.clone(),
                    pinned_generation.clone(),
                    generation_owned_query,
                )),
                wire.operation,
                tracedecay_application::RequestId::new(format!(
                    "request.multi-root.surface.{ordinal}"
                ))
                .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)?,
                tracedecay_application::PageRequest::new(100, page_cursor)
                    .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)?,
                deadline,
                tracedecay_application::CancellationSignal::active(cancellation.token_id.as_str())
                    .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)?,
                wire.request,
            )
            .await
            .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
            if !generation_owned_query {
                ensure_pinned_generation(state, scope, pinned_generation).await?;
            }
            Ok((
                payload,
                cursor.map(tracedecay_application::MultiRootRootContinuationV1::Page),
            ))
        }
    }
}

pub(super) async fn ensure_pinned_generation(
    state: &DaemonInvocationState,
    scope: &tracedecay_application::ResolvedScope,
    pinned: &code_index_scheduler::LatestCompleteCodeIndexV1,
) -> std::result::Result<(), service::invocation::DaemonInvocationProblem> {
    let current = state
        .code_index_schedulers
        .latest_complete_ready_for_scope(scope)
        .await
        .ok_or(service::invocation::DaemonInvocationProblem::Unavailable)?;
    if published_root_generation(scope, &current)? != published_root_generation(scope, pinned)? {
        return Err(service::invocation::DaemonInvocationProblem::Unavailable);
    }
    Ok(())
}
