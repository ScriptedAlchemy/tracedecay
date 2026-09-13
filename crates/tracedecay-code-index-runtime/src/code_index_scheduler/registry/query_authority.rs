//! Query-authority installation and lookup for mounted scopes.

use std::{path::Path, sync::Arc};

use tracedecay_domain::{ManifestDigest, configuration::ConfigurationRevisionId};

use super::super::{CodeIndexSchedulerErrorV1, LatestCompleteCodeIndexV1};
use super::{CodeIndexSchedulerRegistryV1, QueryActivationAttemptV1, unique_mounted_for_scope};

impl CodeIndexSchedulerRegistryV1 {
    pub(super) async fn install_test_attribution_authority(
        &self,
        project_root: &Path,
        latest: &LatestCompleteCodeIndexV1,
    ) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let Ok(authority) = latest.test_attribution_authority() else {
            return false;
        };
        let serving_generation = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                return false;
            };
            Arc::clone(&worktree.serving_generation)
        };
        let serving = serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let generation_id = latest.generation.manifest().generation_id.clone();
        if serving
            .as_ref()
            .map(LatestCompleteCodeIndexV1::generation)
            .map(|generation| &generation.manifest().generation_id)
            != Some(&generation_id)
        {
            return false;
        }
        self.test_attribution_authorities
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(project_root, (generation_id, authority));
        true
    }

    #[cfg(test)]
    pub(in crate::code_index_scheduler) fn remove_test_attribution_authority(
        &self,
        project_root: &Path,
    ) {
        let Ok(project_root) = project_root.canonicalize() else {
            return;
        };
        self.test_attribution_authorities
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&project_root);
    }

    /// Mount the accepted query profile and query/cursor key owner for one exact
    /// admitted scope. The authority cannot be inherited by another project,
    /// repository, worktree, or ref.
    pub async fn mount_query_authority(
        &self,
        project_root: &Path,
        scope: &tracedecay_contracts::ResolvedScope,
        authority: Arc<tracedecay_query::retrieval::QueryAuthorityV1>,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        scope
            .validate()
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        let project_root = project_root.canonicalize()?;
        let mut mounted = self.mounted.lock().await;
        let worktree = mounted.get_mut(&project_root).ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "cannot mount query authority before its worktree".to_owned(),
            )
        })?;
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "query authority scope does not match the mounted worktree".to_owned(),
            ));
        }
        if worktree.query_activation_revision.is_some() {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "standalone query authority cannot replace a committed authority pair".to_owned(),
            ));
        }
        worktree.query_authority = Some((scope.scope_digest.clone(), authority));
        Ok(())
    }

    /// Mount the query authority already serving this project and repository
    /// onto an explicitly published branch worktree.
    ///
    /// Manual branch publication mounts its retained worktree independently of
    /// project-open query activation. An exact branch read may therefore reach
    /// a sealed generation whose worktree has no query authority even though a
    /// peer checkout of the same project already owns one. Reuse is allowed
    /// only when the target and a unique peer both prove the same project and
    /// repository through their sealed generations; disagreement fails closed.
    pub async fn mount_query_authority_from_project_peer(
        &self,
        project_root: &Path,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        scope
            .validate()
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        let project_root = project_root.canonicalize()?;
        let mut mounted = self.mounted.lock().await;
        let target = mounted.get(&project_root).ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "cannot mount a branch query authority before its worktree".to_owned(),
            )
        })?;
        if target.repository_id != scope.repository_id || target.worktree_id != scope.worktree_id {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "branch query authority scope does not match the mounted worktree".to_owned(),
            ));
        }
        if target.query_authority.is_some() {
            return Ok(true);
        }
        let target_project_matches = target
            .serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|latest| latest.generation().manifest().project_id == scope.project_id);
        if !target_project_matches {
            return Ok(false);
        }

        let mut authority = None;
        for (root, peer) in mounted.iter() {
            if root == &project_root || peer.repository_id != scope.repository_id {
                continue;
            }
            let peer_project_matches = peer
                .serving_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .is_some_and(|latest| {
                    latest.generation().manifest().project_id == scope.project_id
                });
            if !peer_project_matches {
                continue;
            }
            let Some((_, candidate)) = peer.query_authority.as_ref() else {
                continue;
            };
            if authority
                .as_ref()
                .is_some_and(|existing| !Arc::ptr_eq(existing, candidate))
            {
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "multiple query authorities are mounted for this project repository".to_owned(),
                ));
            }
            authority = Some(Arc::clone(candidate));
        }
        let Some(authority) = authority else {
            return Ok(false);
        };
        let target = mounted.get_mut(&project_root).ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "branch query authority target disappeared during installation".to_owned(),
            )
        })?;
        if target.query_activation_revision.is_some() {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "standalone query authority cannot replace a committed authority pair".to_owned(),
            ));
        }
        target.query_authority = Some((scope.scope_digest.clone(), authority));
        Ok(true)
    }

    /// Seat the core query fallback while an exact committed semantic
    /// activation is still warming. Unlike a standalone mount, this preserves
    /// the committed revision fence and never replaces an already-usable query
    /// authority (including one installed by a completed activation).
    pub async fn mount_query_authority_for_committed_fallback(
        &self,
        project_root: &Path,
        scope: &tracedecay_contracts::ResolvedScope,
        expected_revision: &ConfigurationRevisionId,
        authority: Arc<tracedecay_query::retrieval::QueryAuthorityV1>,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        scope
            .validate()
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        let project_root = project_root.canonicalize()?;
        let mut mounted = self.mounted.lock().await;
        let worktree = mounted.get_mut(&project_root).ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "cannot mount committed query fallback before its worktree".to_owned(),
            )
        })?;
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "committed query fallback scope does not match the mounted worktree".to_owned(),
            ));
        }
        let activation = tracedecay_application::semantic_runtime::project_semantic_activation_gate(
            &project_root,
        );
        let _activation = activation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if worktree.query_activation_revision.as_ref() != Some(expected_revision) {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "committed query fallback revision is no longer desired".to_owned(),
            ));
        }
        if worktree.query_authority.is_none() {
            worktree.query_authority = Some((scope.scope_digest.clone(), authority));
        }
        Ok(())
    }

    /// Install the observability lane for one mounted worktree. Installation
    /// is once per mount: a repeated install against the same mounted worktree
    /// keeps the incumbent lane, and a worktree that is not mounted is a typed
    /// error so the caller can log the absence.
    pub async fn install_index_observability(
        &self,
        project_root: &Path,
        observability: super::super::observability::CodeIndexObservabilityV1,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        let project_root = project_root.canonicalize()?;
        let mounted = self.mounted.lock().await;
        let worktree = mounted.get(&project_root).ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "cannot install index observability before its worktree".to_owned(),
            )
        })?;
        // A remount creates a fresh empty slot, so an ignored second set here
        // can only be a same-mount duplicate carrying the same project lane.
        let _ = worktree.index_observability.set(observability);
        Ok(())
    }

    /// The installed observability lane for one exact admitted scope, if the
    /// worktree is mounted and the lane was installed.
    pub async fn index_observability_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<super::super::observability::CodeIndexObservabilityV1> {
        let mounted = self.mounted.lock().await;
        unique_mounted_for_scope(&mounted, scope)
            .unique()
            .and_then(|(_, worktree)| worktree.index_observability.get().cloned())
    }

    /// Install the core and optional semantic query routes as one committed
    /// configuration observation. The provider CAS is repeated while the
    /// mounted-worktree lock is held, so a delayed observer cannot publish a
    /// stale authority pair after a newer committed revision.
    pub async fn begin_committed_query_activation(
        &self,
        project_root: &Path,
        scope: &tracedecay_contracts::ResolvedScope,
        epoch: i64,
        result_revision: &ConfigurationRevisionId,
        transition_digest: &ManifestDigest,
        prepared_redundancy: &tracedecay_application::semantic_runtime::PreparedSemanticRedundancyAuthorityV1,
    ) -> Result<QueryActivationAttemptV1, CodeIndexSchedulerErrorV1> {
        if epoch <= 0 || prepared_redundancy.configuration_revision() != result_revision {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "prepared redundancy revision does not match query activation".to_owned(),
            ));
        }
        let project_root = project_root.canonicalize()?;
        let mut mounted = self.mounted.lock().await;
        let worktree = mounted.get_mut(&project_root).ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "cannot begin query activation before its worktree".to_owned(),
            )
        })?;
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "query activation scope does not match the mounted worktree".to_owned(),
            ));
        }
        let mut exact_retry = false;
        if let Some(desired_epoch) = worktree.query_activation_epoch {
            let advances = epoch > desired_epoch;
            exact_retry = epoch == desired_epoch
                && worktree.query_activation_revision.as_ref() == Some(result_revision)
                && worktree.query_activation_transition_digest.as_ref() == Some(transition_digest)
                && worktree.query_activation_redundancy.as_ref() == Some(prepared_redundancy);
            if !advances && !exact_retry {
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "query activation is older than the desired configuration fence".to_owned(),
                ));
            }
        }
        let activation = tracedecay_application::semantic_runtime::project_semantic_activation_gate(
            &project_root,
        );
        let _activation = activation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        worktree.query_activation_attempt = worktree
            .query_activation_attempt
            .checked_add(1)
            .ok_or_else(|| {
                CodeIndexSchedulerErrorV1::Identity(
                    "query activation attempt sequence is exhausted".to_owned(),
                )
            })?;
        worktree.query_activation_revision = Some(result_revision.clone());
        worktree.query_activation_epoch = Some(epoch);
        worktree.query_activation_transition_digest = Some(transition_digest.clone());
        worktree.query_activation_redundancy = Some(prepared_redundancy.clone());
        if !exact_retry {
            worktree.semantic_query_authority = None;
            tracedecay_application::semantic_runtime::commit_project_semantic_redundancy_authority_under_gate(
                project_root,
                prepared_redundancy,
                false,
            );
        }
        Ok(QueryActivationAttemptV1 {
            revision: result_revision.clone(),
            token: worktree.query_activation_attempt,
            preserves_existing_authority: exact_retry,
        })
    }

    #[hotpath::measure(
        label = "daemon.code_index.registry.install_query_authorities",
        future = true
    )]
    pub async fn install_committed_query_authorities(
        &self,
        project_root: &Path,
        scope: &tracedecay_contracts::ResolvedScope,
        commit_prepared: impl FnOnce() -> Result<(), String>,
        prepared: crate::ports::PreparedQueryActivationViewV1,
        semantic_authority: Option<
            Arc<super::super::semantic_query_runtime::SemanticQueryAuthorityV1>,
        >,
        prepared_runtime: Option<
            tracedecay_application::semantic_runtime::PreparedProductionSemanticRuntimeCommitV1,
        >,
        disabled_cache_generation: Option<&tracedecay_domain::VectorGenerationIdV1>,
        prepared_redundancy: tracedecay_application::semantic_runtime::PreparedSemanticRedundancyAuthorityV1,
        attempt: &QueryActivationAttemptV1,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        scope
            .validate()
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        if prepared.scope() != scope {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "prepared query activation scope does not match the committed scope".to_owned(),
            ));
        }
        let project_root = project_root.canonicalize()?;
        let mut mounted = self.mounted.lock().await;
        let worktree = mounted.get_mut(&project_root).ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "cannot install query authorities before their worktree".to_owned(),
            )
        })?;
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "query authority scope does not match the mounted worktree".to_owned(),
            ));
        }
        let activation = tracedecay_application::semantic_runtime::project_semantic_activation_gate(
            &project_root,
        );
        let _activation = activation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if worktree.query_activation_revision.as_ref() != Some(&attempt.revision)
            || worktree.query_activation_attempt != attempt.token
            || prepared.configuration_revision() != &attempt.revision
            || prepared_redundancy.configuration_revision() != &attempt.revision
            || worktree.query_activation_redundancy.as_ref() != Some(&prepared_redundancy)
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "prepared query activation attempt is no longer desired".to_owned(),
            ));
        }
        if let Some(prepared_runtime) = prepared_runtime {
            if !prepared_runtime.commit() {
                if !attempt.preserves_existing_authority {
                    worktree.semantic_query_authority = None;
                    worktree.query_activation_revision =
                        Some(prepared.configuration_revision().clone());
                    tracedecay_application::semantic_runtime::commit_project_semantic_redundancy_authority_under_gate(
                        project_root.clone(),
                        &prepared_redundancy,
                        false,
                    );
                }
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "prepared semantic runtime became stale before coherent installation"
                        .to_owned(),
                ));
            }
        } else if semantic_authority.is_none()
            && let Some(generation) = disabled_cache_generation
        {
            tracedecay_application::semantic_runtime::unbind_project_semantic_cache_if_current(
                &project_root,
                generation,
            );
        }
        if let Err(error) = commit_prepared() {
            if !attempt.preserves_existing_authority {
                worktree.semantic_query_authority = None;
                worktree.query_activation_revision =
                    Some(prepared.configuration_revision().clone());
                tracedecay_application::semantic_runtime::commit_project_semantic_redundancy_authority_under_gate(
                    project_root.clone(),
                    &prepared_redundancy,
                    false,
                );
            }
            return Err(CodeIndexSchedulerErrorV1::Identity(error));
        }
        tracedecay_application::semantic_runtime::commit_project_semantic_redundancy_authority_under_gate(
            project_root.clone(),
            &prepared_redundancy,
            semantic_authority.is_some(),
        );
        worktree.query_authority = Some((
            scope.scope_digest.clone(),
            Arc::clone(prepared.query_authority()),
        ));
        worktree.semantic_query_authority =
            semantic_authority.map(|authority| (scope.scope_digest.clone(), authority));
        worktree.query_activation_revision = Some(prepared.configuration_revision().clone());
        Ok(())
    }

    /// Revoke a failed committed transition without letting a delayed observer
    /// erase a different revision that already installed coherently.
    pub async fn clear_failed_query_activation(
        &self,
        project_root: &Path,
        scope: &tracedecay_contracts::ResolvedScope,
        cache_generation: Option<&tracedecay_domain::VectorGenerationIdV1>,
        failed_redundancy: tracedecay_application::semantic_runtime::PreparedSemanticRedundancyAuthorityV1,
        attempt: &QueryActivationAttemptV1,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        scope
            .validate()
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        let project_root = project_root.canonicalize()?;
        let mut mounted = self.mounted.lock().await;
        let worktree = mounted.get_mut(&project_root).ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "cannot clear query authorities before their worktree".to_owned(),
            )
        })?;
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "failed query activation scope does not match the mounted worktree".to_owned(),
            ));
        }
        let activation = tracedecay_application::semantic_runtime::project_semantic_activation_gate(
            &project_root,
        );
        let _activation = activation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if worktree.query_activation_revision.as_ref() == Some(&attempt.revision)
            && worktree.query_activation_attempt == attempt.token
            && failed_redundancy.configuration_revision() == &attempt.revision
            && worktree.query_activation_redundancy.as_ref() == Some(&failed_redundancy)
        {
            if attempt.preserves_existing_authority {
                return Ok(false);
            }
            worktree.semantic_query_authority = None;
            tracedecay_application::semantic_runtime::commit_project_semantic_redundancy_authority_under_gate(
                project_root.clone(),
                &failed_redundancy,
                false,
            );
            if let Some(generation) = cache_generation {
                tracedecay_application::semantic_runtime::unbind_project_semantic_cache_if_current(
                    &project_root,
                    generation,
                );
            }
            return Ok(true);
        }
        Ok(false)
    }

    #[hotpath::measure(label = "daemon.code_index.registry.query_authority", future = true)]
    pub async fn query_authority_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<Arc<tracedecay_query::retrieval::QueryAuthorityV1>> {
        self.activate_for_scope(scope);
        let mounted = self.mounted.lock().await;
        // Same worktree isolation as `latest_matches_scope_identity`: a
        // mid-session ref switch keeps the mounted ranking authority until
        // the route remounts. Exact digest is a remount key, not a reason
        // to deny search after HEAD moved.
        unique_mounted_for_scope(&mounted, scope)
            .unique()
            .and_then(|(_, worktree)| {
                worktree
                    .query_authority
                    .as_ref()
                    .map(|(_scope_digest, authority)| Arc::clone(authority))
            })
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn has_query_authority_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> bool {
        self.query_authority_for_scope(scope).await.is_some()
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn query_authority_installation_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<(bool, bool, Option<ConfigurationRevisionId>)> {
        let mounted = self.mounted.lock().await;
        let (_, worktree) = unique_mounted_for_scope(&mounted, scope).unique()?;
        Some((
            worktree
                .query_authority
                .as_ref()
                .is_some_and(|(digest, _)| digest == &scope.scope_digest),
            worktree
                .semantic_query_authority
                .as_ref()
                .is_some_and(|(digest, _)| digest == &scope.scope_digest),
            worktree.query_activation_revision.clone(),
        ))
    }
}
