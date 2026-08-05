//! Durable multi-root scope-set coordination.

use super::*;

impl DaemonInvocationService {
    pub(crate) async fn compare_and_swap_scope_set(
        &self,
        active_project_root: &Path,
        idempotency_key: &str,
        request: MultiRootScopeSetCasRequestV1,
        mut roots: Vec<(
            PathBuf,
            ResolvedScope,
            tracedecay_application::RegisteredRootLocatorV1,
        )>,
        observed_at: UtcMicros,
        deadline: &Deadline,
        cancellation: &CancellationContext,
    ) -> Option<(ResolvedScope, MultiRootScopeSetCasResultV1)> {
        if cancellation.is_cancelled() || deadline.is_elapsed_at(observed_at) {
            return None;
        }
        request.validate().ok()?;
        roots.sort_by(|left, right| left.1.scope_digest.cmp(&right.1.scope_digest));
        if roots.is_empty()
            || roots
                .windows(2)
                .any(|pair| pair[0].1.scope_digest == pair[1].1.scope_digest)
        {
            return None;
        }
        let (capability, use_case) = tracedecay_application::multi_root_operation_authority(
            tracedecay_application::MultiRootApplicationOperation::ScopeSetCompareAndSwap,
        )
        .ok()?;
        let active_runtime = self
            .project_runtimes
            .get::<RegisteredCallableCodeRuntime>(active_project_root)
            .await?;
        let active_scope = active_runtime.scope.clone();
        self.multi_root_query_context(
            active_project_root,
            &active_scope,
            roots.len(),
            observed_at,
            deadline,
            cancellation,
            &capability,
            &use_case,
        )
        .await?;
        let active_owner = self.lsp_owner(Some(active_project_root)).await?;
        let active_storage = active_owner.scope_set_storage?;
        let next_revision = match request.expected_revision {
            Some(expected) => expected.checked_next().ok()?,
            None => ScopeSetRevision::new(1).ok()?,
        };
        let mut admissions = Vec::with_capacity(roots.len());
        let mut storages = vec![(active_scope.scope_digest.clone(), active_storage.clone())];
        for (ordinal, (project_root, scope, locator)) in roots.iter().enumerate() {
            let owner = self.lsp_owner(Some(project_root)).await?;
            let storage = owner.scope_set_storage?;
            storages.push((scope.scope_digest.clone(), storage));
            let context = self
                .multi_root_query_context(
                    project_root,
                    scope,
                    ordinal,
                    observed_at,
                    deadline,
                    cancellation,
                    &capability,
                    &use_case,
                )
                .await?;
            admissions.push(
                tracedecay_application::AuthorizedRootAdmission::new(context, locator.clone())
                    .ok()?,
            );
        }
        let next = AuthorizedScopeSetAuthority::authorize_registered(
            request.scope_set_id,
            next_revision,
            admissions,
            &capability,
            &use_case,
            observed_at,
        )
        .ok()?;
        let command_digest = canonical_sha256(&(
            "tracedecay.daemon.multi-root-scope-set-cas-command.v1",
            &request,
            next.digest(),
        ))
        .ok()?;
        let coordinator = match active_storage
            .begin_durable_compare_and_swap(
                idempotency_key,
                &command_digest,
                request.expected_revision,
                &next,
            )
            .ok()?
        {
            tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetDurableCasV1::Applied(
                applied,
            ) => {
                return Some((
                    active_scope,
                    MultiRootScopeSetCasResultV1 {
                        status: MultiRootScopeSetCasStatusV1::Applied,
                        scope_set: Some(applied),
                    },
                ));
            }
            tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetDurableCasV1::Conflict(
                current,
            ) => {
                return Some((
                    active_scope,
                    MultiRootScopeSetCasResultV1 {
                        status: MultiRootScopeSetCasStatusV1::Conflict,
                        scope_set: current,
                    },
                ));
            }
            tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetDurableCasV1::Pending(
                pending,
            ) => pending,
        };
        if coordinator != next {
            return None;
        }
        storages.sort_by(|left, right| left.0.cmp(&right.0));
        storages.dedup_by(|left, right| left.0 == right.0);
        let replica_digests = storages
            .iter()
            .map(|(digest, _)| digest.clone())
            .collect::<Vec<_>>();
        for (replica_digest, storage) in &storages {
            if cancellation.is_cancelled() || deadline.is_elapsed_at(current_micros()) {
                return None;
            }
            match storage.compare_and_swap(request.expected_revision, &next) {
                Err(_) => {
                    let current = storage.read(next.scope_set_id()).ok()?;
                    let terminal = active_storage
                        .conflict_durable_compare_and_swap(
                            idempotency_key,
                            &command_digest,
                            current.as_ref(),
                        )
                        .ok()?;
                    let tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetDurableCasV1::Conflict(
                        current,
                    ) = terminal
                    else {
                        return None;
                    };
                    return Some((
                        active_scope,
                        MultiRootScopeSetCasResultV1 {
                            status: MultiRootScopeSetCasStatusV1::Conflict,
                            scope_set: current,
                        },
                    ));
                }
                Ok(tracedecay_store::runtime::ScopeSetCasOutcomeV1::Applied(_)) => {}
                Ok(tracedecay_store::runtime::ScopeSetCasOutcomeV1::Conflict { .. }) => {
                    let stored = storage.read(next.scope_set_id()).ok()?;
                    if stored.as_ref() != Some(&next) {
                        let terminal = active_storage
                            .conflict_durable_compare_and_swap(
                                idempotency_key,
                                &command_digest,
                                stored.as_ref(),
                            )
                            .ok()?;
                        let tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetDurableCasV1::Conflict(
                            current,
                        ) = terminal
                        else {
                            return None;
                        };
                        return Some((
                            active_scope,
                            MultiRootScopeSetCasResultV1 {
                                status: MultiRootScopeSetCasStatusV1::Conflict,
                                scope_set: current,
                            },
                        ));
                    }
                }
            }
            if cancellation.is_cancelled() || deadline.is_elapsed_at(current_micros()) {
                return None;
            }
            active_storage
                .record_durable_replica(idempotency_key, &command_digest, replica_digest)
                .ok()?;
        }
        if cancellation.is_cancelled() || deadline.is_elapsed_at(current_micros()) {
            return None;
        }
        match active_storage
            .complete_durable_compare_and_swap(idempotency_key, &command_digest, &replica_digests)
            .ok()?
        {
            tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetDurableCasV1::Applied(
                applied,
            ) => Some((
                active_scope,
                MultiRootScopeSetCasResultV1 {
                    status: MultiRootScopeSetCasStatusV1::Applied,
                    scope_set: Some(applied),
                },
            )),
            tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetDurableCasV1::Conflict(
                current,
            ) => Some((
                active_scope,
                MultiRootScopeSetCasResultV1 {
                    status: MultiRootScopeSetCasStatusV1::Conflict,
                    scope_set: current,
                },
            )),
            tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetDurableCasV1::Pending(_) => {
                None
            }
        }
    }
}
