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

pub(super) async fn resolve_root_query_generation(
    schedulers: &code_index_scheduler::CodeIndexSchedulerRegistryV1,
    scope: &tracedecay_application::ResolvedScope,
    sealed: Option<&tracedecay_domain::RootScopeOutcomeV1<tracedecay_domain::RootGenerationV1>>,
) -> std::result::Result<
    (
        tracedecay_domain::RootScopeOutcomeV1<tracedecay_domain::RootGenerationV1>,
        Option<code_index_scheduler::LatestCompleteCodeIndexV1>,
    ),
    service::invocation::DaemonInvocationProblem,
> {
    let Some(sealed) = sealed else {
        let Some(latest) = schedulers.latest_complete_ready_for_scope(scope).await else {
            return Ok((
                unavailable_root_generation(
                    scope,
                    tracedecay_domain::ScopeUnavailableReasonV1::AuthorityUnavailable,
                )?,
                None,
            ));
        };
        let generation = published_root_generation(scope, &latest)?;
        return Ok((
            tracedecay_domain::RootScopeOutcomeV1::new(
                scope.scope_digest.clone(),
                tracedecay_domain::ScopeOutcome::Exact(generation),
            )
            .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)?,
            Some(latest),
        ));
    };
    if sealed.scope_digest != scope.scope_digest {
        return Err(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized);
    }
    let retained = match &sealed.outcome {
        tracedecay_domain::ScopeOutcome::Exact(generation)
        | tracedecay_domain::ScopeOutcome::Partial {
            value: generation, ..
        } => schedulers
            .generation_for(scope, &generation.index_generation)
            .await
            .filter(|latest| {
                published_root_generation(scope, latest)
                    .is_ok_and(|published| published == *generation)
            }),
        tracedecay_domain::ScopeOutcome::Denied
        | tracedecay_domain::ScopeOutcome::Unavailable { .. } => None,
    };
    Ok((sealed.clone(), retained))
}

pub(super) fn completed_root_generation(
    continuation: Option<&tracedecay_application::MultiRootContinuationStateV1>,
    ordinal: usize,
    scope: &tracedecay_application::ResolvedScope,
) -> std::result::Result<
    Option<tracedecay_domain::RootScopeOutcomeV1<tracedecay_domain::RootGenerationV1>>,
    service::invocation::DaemonInvocationProblem,
> {
    let Some(state) = continuation else {
        return Ok(None);
    };
    let cursor = state
        .root_cursors
        .get(ordinal)
        .ok_or(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized)?;
    if !cursor_is_complete(cursor) {
        return Ok(None);
    }
    state
        .root_generations
        .get(ordinal)
        .filter(|sealed| sealed.scope_digest == scope.scope_digest)
        .cloned()
        .map(Some)
        .ok_or(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized)
}

pub(super) fn cursor_is_complete(cursor: &tracedecay_application::MultiRootRootCursorV1) -> bool {
    matches!(
        cursor.cursor.as_ref(),
        Some(tracedecay_application::MultiRootRootContinuationV1::Complete)
    )
}

pub(super) fn terminalize_cursor(cursor: &mut tracedecay_application::MultiRootRootCursorV1) {
    cursor.cursor = Some(tracedecay_application::MultiRootRootContinuationV1::Complete);
}

pub(super) fn insert_completed_root_outcome(
    outcomes: &mut BTreeMap<
        tracedecay_domain::ManifestDigest,
        tracedecay_domain::ScopeOutcome<Vec<Value>>,
    >,
    scope: &tracedecay_application::ResolvedScope,
    generation: &tracedecay_domain::RootScopeOutcomeV1<tracedecay_domain::RootGenerationV1>,
) {
    if matches!(
        &generation.outcome,
        tracedecay_domain::ScopeOutcome::Exact(_) | tracedecay_domain::ScopeOutcome::Partial { .. }
    ) {
        outcomes.insert(
            scope.scope_digest.clone(),
            tracedecay_domain::ScopeOutcome::Exact(Vec::new()),
        );
    }
}

pub(super) fn retain_unexecuted_cursor(
    generation: &tracedecay_domain::RootScopeOutcomeV1<tracedecay_domain::RootGenerationV1>,
    current: &tracedecay_application::MultiRootRootCursorV1,
    next: &mut tracedecay_application::MultiRootRootCursorV1,
) {
    if matches!(&generation.outcome, tracedecay_domain::ScopeOutcome::Denied) {
        terminalize_cursor(next);
    } else {
        *next = current.clone();
    }
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
                None,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> tracedecay_domain::ManifestDigest {
        tracedecay_domain::ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("digest")
    }

    #[test]
    fn denial_terminalizes_cursor_and_restored_authority_cannot_restart_root() {
        let scope = tracedecay_application::ResolvedScope::new(
            tracedecay_domain::ProjectId::new("project.denied-root").expect("project"),
            tracedecay_domain::RepositoryId::new("repository.denied-root").expect("repository"),
            tracedecay_domain::WorktreeId::new("worktree.denied-root").expect("worktree"),
            None,
        )
        .expect("scope");
        let current = tracedecay_application::MultiRootRootCursorV1::new(
            scope.scope_digest.clone(),
            Some(tracedecay_application::MultiRootRootContinuationV1::Page(
                tracedecay_application::OpaqueCursor::new("cursor.denied-root")
                    .expect("page cursor"),
            )),
        )
        .expect("root cursor");
        let denied = tracedecay_domain::RootScopeOutcomeV1::new(
            scope.scope_digest.clone(),
            tracedecay_domain::ScopeOutcome::Denied,
        )
        .expect("denied root");
        let mut next =
            tracedecay_application::MultiRootRootCursorV1::new(scope.scope_digest.clone(), None)
                .expect("next root cursor");

        retain_unexecuted_cursor(&denied, &current, &mut next);
        assert!(cursor_is_complete(&next));

        let restored = tracedecay_domain::RootGenerationV1::new(
            scope.scope_digest.clone(),
            tracedecay_domain::CodeGenerationId::new("generation.denied-root.restored")
                .expect("generation"),
            digest('a'),
            digest('b'),
        )
        .expect("restored generation");
        let restored = tracedecay_domain::RootScopeOutcomeV1::new(
            scope.scope_digest.clone(),
            tracedecay_domain::ScopeOutcome::Exact(restored),
        )
        .expect("restored root");
        let mut outcomes: BTreeMap<
            tracedecay_domain::ManifestDigest,
            tracedecay_domain::ScopeOutcome<Vec<Value>>,
        > = BTreeMap::new();
        insert_completed_root_outcome(&mut outcomes, &scope, &restored);
        assert!(matches!(
            outcomes.get(&scope.scope_digest),
            Some(tracedecay_domain::ScopeOutcome::Exact(values)) if values.is_empty()
        ));
    }
}
