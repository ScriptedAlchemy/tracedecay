//! Query-authority installation and lookup for mounted scopes.

use std::{path::Path, sync::Arc};

use super::super::{CodeIndexSchedulerErrorV1, LatestCompleteCodeIndexV1};
use super::{CodeIndexSchedulerRegistryV1, unique_mounted_for_scope};

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

    /// Mount the query profile and query/cursor key owner for one exact
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
        target.query_authority = Some((scope.scope_digest.clone(), authority));
        Ok(true)
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
}
