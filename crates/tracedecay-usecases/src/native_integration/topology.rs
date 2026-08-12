//! Exact native-integration topology resolution.
//!
//! Independent branches are proven from the enrolled repository. Declared
//! stacks additionally require the exact registered linked-worktree roots and
//! publish their frozen revision and observed occupancies through the verified
//! Git topology projection.
//!
//! Branch names, paths, provider order, and graph proximity never select or
//! infer the pair: the enrolled repository identity is supplied at trusted
//! composition and the refs come from the already-authorized scope.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tracedecay_application::{
    AuthorizedRoot, CancellationSignal, NativeIntegrationPortError,
    NativeIntegrationSelectionBindingV1, NativeIntegrationStackResolutionOutcomeV1,
    NativeIntegrationStackResolutionPort, NativeIntegrationStackResolutionRequestV1,
};
use tracedecay_code_index::git_projection::{
    GIT_TOPOLOGY_PROJECTOR_REVISION_V1, GitBranchStackBindingV1, GitTopologyProjectionStore,
    GitWorktreeOccupancyV1, build_git_topology_manifest_checked, git_topology_idempotency_key,
    git_topology_namespace, git_topology_projection_identity,
};
use tracedecay_domain::{
    BranchStackRevisionV1, FrozenBranchStackSnapshotV1, FrozenIndependentBranchSelectionV1,
    GitHeadStateV1, GitOidV1, NativeIntegrationSelectionV1, ProjectId, RefId, RepositoryId,
    WorktreeId,
};
use tracedecay_global_db::VerifiedGraphRuntimePortV1;
use tracedecay_graph_db::{GraphCancellation, GraphProjectorRevision};
use tracedecay_runtime_core::git_repository::GitRepositoryAuthority;
use tracedecay_store::{FactReadControl, StoreShardIdV1, StoreShardScopeV1};

use super::{domain_error, native_error};
use crate::git_intelligence::{GIT_HISTORY_MAX_COUNT_LIMIT, NativeGitIntelligence};

struct NeverCancelled;

impl GraphCancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// One enrolled repository's exact-pair resolver.
///
/// The root is supplied only at trusted composition; no request field can
/// replace it, so a caller cannot redirect resolution at another repository.
pub struct ExactPairNativeIntegrationTopology {
    project_id: ProjectId,
    repository_id: RepositoryId,
    repository_root: PathBuf,
    repository: GitRepositoryAuthority,
    graph_runtime: Option<Arc<dyn VerifiedGraphRuntimePortV1>>,
}

impl ExactPairNativeIntegrationTopology {
    pub fn open(
        project_id: ProjectId,
        repository_id: RepositoryId,
        enrolled_repository_root: &Path,
    ) -> Result<Self, NativeIntegrationPortError> {
        Self::open_with_optional_graph_runtime(
            project_id,
            repository_id,
            enrolled_repository_root,
            None,
        )
    }

    pub fn open_with_graph_runtime(
        project_id: ProjectId,
        repository_id: RepositoryId,
        enrolled_repository_root: &Path,
        expected_graph_shard: StoreShardIdV1,
        graph_runtime: Arc<dyn VerifiedGraphRuntimePortV1>,
    ) -> Result<Self, NativeIntegrationPortError> {
        Self::open_with_optional_graph_runtime(
            project_id,
            repository_id,
            enrolled_repository_root,
            Some((expected_graph_shard, graph_runtime)),
        )
    }

    fn open_with_optional_graph_runtime(
        project_id: ProjectId,
        repository_id: RepositoryId,
        enrolled_repository_root: &Path,
        graph_runtime: Option<(StoreShardIdV1, Arc<dyn VerifiedGraphRuntimePortV1>)>,
    ) -> Result<Self, NativeIntegrationPortError> {
        project_id.validate().map_err(domain_error)?;
        repository_id.validate().map_err(domain_error)?;
        if let Some((expected_shard, runtime)) = &graph_runtime {
            let binding = runtime.relational_binding();
            let locator = runtime.relational_verified_locator();
            let exact_project = matches!(
                &expected_shard.scope,
                StoreShardScopeV1::Project { project_id: bound } if bound == &project_id
            );
            if !exact_project
                || &binding.shard_id != expected_shard
                || locator.shard_id != binding.shard_id
                || locator.incarnation != binding.incarnation
            {
                return Err(NativeIntegrationPortError::Unavailable);
            }
        }
        let repository =
            GitRepositoryAuthority::discover(enrolled_repository_root).map_err(native_error)?;
        Ok(Self {
            project_id,
            repository_id,
            repository_root: enrolled_repository_root.to_path_buf(),
            repository,
            graph_runtime: graph_runtime.map(|(_, runtime)| runtime),
        })
    }

    /// Whether this repository's HEAD is attached to `reference`.
    ///
    /// A destination that is checked out cannot take a plain compare-and-swap
    /// ref update, so the preview must know. `GitHeadStateV1::Attached` carries
    /// the branch as Git spells it, which may be the full ref or its
    /// `refs/heads/` short form; both are accepted, and nothing else is
    /// treated as a match.
    fn head_occupies(&self, reference: &RefId) -> Result<bool, NativeIntegrationPortError> {
        let status = self.repository.status().map_err(native_error)?;
        let GitHeadStateV1::Attached { branch, .. } = &status.head else {
            // Detached and unborn heads occupy no branch, so neither can hold
            // the destination ref.
            return Ok(false);
        };
        let reference = reference.as_str();
        Ok(branch == reference
            || reference
                .strip_prefix("refs/heads/")
                .is_some_and(|short| branch == short))
    }

    fn tip(&self, reference: &RefId) -> Result<Option<GitOidV1>, NativeIntegrationPortError> {
        // A ref this repository cannot resolve is missing evidence, not a
        // failure of the resolver: the caller still gets useful read-only
        // partial state and apply stays blocked.
        Ok(self.repository.exact_reference_tip(reference.as_str()).ok())
    }

    fn resolve_declared_stack(
        &self,
        request: &NativeIntegrationStackResolutionRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationStackResolutionOutcomeV1, NativeIntegrationPortError> {
        let Some(runtime) = &self.graph_runtime else {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
        };
        let NativeIntegrationSelectionBindingV1::DeclaredStackEdge {
            declared_revision,
            source_node_id,
            destination_node_id,
            direction,
            ..
        } = &request.selection
        else {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
        };
        if cancellation.is_cancelled() {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
        }
        if request.destination.project_id != self.project_id
            || request.destination.repository_id != self.repository_id
            || request.source.project_id != self.project_id
            || request.source.repository_id != self.repository_id
        {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Denied);
        }
        let runtime_profile = &runtime.relational_binding().shard_id.profile_id;
        let exact_profile = request
            .authorized_scope_set
            .roots()
            .iter()
            .filter(|root| {
                root.scope().project_id == self.project_id
                    && root.scope().repository_id == self.repository_id
            })
            .all(|root| {
                root.locator()
                    .is_some_and(|locator| &locator.profile.profile_id == runtime_profile)
            });
        if !exact_profile {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
        }
        let (binding, occupancies) =
            match self.verify_declared_authority(request, declared_revision, cancellation) {
                Ok(value) => value,
                Err(outcome) => return Ok(outcome),
            };
        let selection = FrozenBranchStackSnapshotV1::new(
            declared_revision.clone(),
            source_node_id.clone(),
            destination_node_id.clone(),
            *direction,
            request.observed_at,
        )
        .map_err(domain_error)?;
        let identity = match git_topology_namespace(&self.repository_id)
            .and_then(git_topology_projection_identity)
        {
            Ok(identity) => identity,
            Err(_) => return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable),
        };
        let read_cancellation = cancellation.clone();
        let previous = match runtime.verified_snapshot(
            &identity,
            FactReadControl::new(Arc::new(move || read_cancellation.is_cancelled())),
        ) {
            Ok(Some(snapshot)) => {
                match GitTopologyProjectionStore::from_verified_snapshot_verified(
                    snapshot,
                    Arc::new(NeverCancelled),
                ) {
                    Ok(store) => (
                        store.branch_stacks().to_vec(),
                        store.worktree_occupancies().to_vec(),
                    ),
                    Err(_) => return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable),
                }
            }
            Ok(None) => (Vec::new(), Vec::new()),
            Err(_) => return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable),
        };
        let (mut branch_stacks, mut worktree_occupancies) = previous;
        branch_stacks.retain(|candidate| {
            candidate.project_id != binding.project_id
                || candidate.repository_id != binding.repository_id
                || candidate.scope_set_id != binding.scope_set_id
        });
        worktree_occupancies.retain(|candidate| {
            candidate.project_id != binding.project_id
                || candidate.repository_id != binding.repository_id
                || candidate.scope_set_id != binding.scope_set_id
        });
        branch_stacks.push(binding);
        worktree_occupancies.extend(occupancies);

        let projection = match NativeGitIntelligence::new(
            self.repository_root.clone(),
            self.repository_id.clone(),
            request.destination.worktree_id.clone(),
        )
        .topology_projection(GIT_HISTORY_MAX_COUNT_LIMIT)
        .and_then(|projection| {
            projection
                .with_declared_topology(branch_stacks, worktree_occupancies)
                .map_err(
                    |error| crate::git_intelligence::GitIntelligenceError::MalformedOutput {
                        operation: "declared topology projection",
                        detail: error.to_string(),
                    },
                )
        }) {
            Ok(projection) => projection,
            Err(_) => return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable),
        };
        if cancellation.is_cancelled() {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
        }
        let revision =
            match GraphProjectorRevision::try_from(GIT_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned()) {
                Ok(revision) => revision,
                Err(_) => return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable),
            };
        let check = || {
            if cancellation.is_cancelled() {
                Err(tracedecay_graph_db::GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        };
        let manifest =
            match build_git_topology_manifest_checked(identity, &projection, &revision, &check) {
                Ok(manifest) => manifest,
                Err(_) => return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable),
            };
        let idempotency = match git_topology_idempotency_key(&projection, &revision) {
            Ok(idempotency) => idempotency,
            Err(_) => return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable),
        };
        let cancelled = Arc::new(AtomicBool::new(cancellation.is_cancelled()));
        if runtime
            .publish_verified_manifest(&manifest, idempotency, cancelled)
            .is_err()
        {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
        }
        Ok(NativeIntegrationStackResolutionOutcomeV1::Complete(
            Box::new(NativeIntegrationSelectionV1::DeclaredStackEdge(selection)),
        ))
    }

    fn verify_declared_authority(
        &self,
        request: &NativeIntegrationStackResolutionRequestV1,
        revision: &BranchStackRevisionV1,
        cancellation: &CancellationSignal,
    ) -> Result<
        (GitBranchStackBindingV1, Vec<GitWorktreeOccupancyV1>),
        NativeIntegrationStackResolutionOutcomeV1,
    > {
        let mut occupancies = Vec::new();
        let mut occupied = BTreeMap::<RefId, BTreeSet<WorktreeId>>::new();
        for root in request.authorized_scope_set.roots().iter().filter(|root| {
            root.scope().project_id == self.project_id
                && root.scope().repository_id == self.repository_id
        }) {
            if cancellation.is_cancelled() {
                return Err(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
            }
            let authority = self.root_authority(root)?;
            let reference = attached_reference(&authority)?;
            if let Some(reference) = &reference {
                occupied
                    .entry(reference.clone())
                    .or_default()
                    .insert(root.scope().worktree_id.clone());
            }
            occupancies.push(GitWorktreeOccupancyV1 {
                project_id: root.scope().project_id.clone(),
                repository_id: root.scope().repository_id.clone(),
                scope_set_id: request.authorized_scope_set.scope_set_id().clone(),
                scope_set_revision: request.authorized_scope_set.revision(),
                scope_set_digest: request.authorized_scope_set.digest().clone(),
                worktree_id: root.scope().worktree_id.clone(),
                reference,
            });
        }
        for node in &revision.nodes {
            let root = request.authorized_scope_set.roots().iter().find(|root| {
                root.scope().project_id == node.project_id
                    && root.scope().repository_id == node.repository_id
                    && root.scope().reference.as_ref() == Some(&node.reference)
                    && node.worktree_id.as_ref() == Some(&root.scope().worktree_id)
            });
            let Some(root) = root else {
                return Err(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
            };
            let tip = self
                .root_authority(root)?
                .exact_reference_tip(node.reference.as_str())
                .map_err(|_| NativeIntegrationStackResolutionOutcomeV1::Stale)?;
            if tip.as_str() != node.tip.as_str() {
                return Err(NativeIntegrationStackResolutionOutcomeV1::Stale);
            }
            let actual = occupied.get(&node.reference).cloned().unwrap_or_default();
            let expected = node.worktree_id.iter().cloned().collect::<BTreeSet<_>>();
            if actual != expected {
                return Err(NativeIntegrationStackResolutionOutcomeV1::Stale);
            }
        }
        Ok((
            GitBranchStackBindingV1 {
                project_id: request.source.project_id.clone(),
                repository_id: request.source.repository_id.clone(),
                scope_set_id: request.authorized_scope_set.scope_set_id().clone(),
                scope_set_revision: request.authorized_scope_set.revision(),
                scope_set_digest: request.authorized_scope_set.digest().clone(),
                revision: revision.clone(),
            },
            occupancies,
        ))
    }

    fn root_authority(
        &self,
        root: &AuthorizedRoot,
    ) -> Result<GitRepositoryAuthority, NativeIntegrationStackResolutionOutcomeV1> {
        let locator = root
            .locator()
            .ok_or(NativeIntegrationStackResolutionOutcomeV1::Unavailable)?;
        let authority = GitRepositoryAuthority::discover(&locator.canonical_root)
            .map_err(|_| NativeIntegrationStackResolutionOutcomeV1::Unavailable)?;
        if authority.common_dir() != self.repository.common_dir() {
            return Err(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
        }
        Ok(authority)
    }
}

fn attached_reference(
    authority: &GitRepositoryAuthority,
) -> Result<Option<RefId>, NativeIntegrationStackResolutionOutcomeV1> {
    let status = authority
        .status()
        .map_err(|_| NativeIntegrationStackResolutionOutcomeV1::Unavailable)?;
    let GitHeadStateV1::Attached { branch, .. } = status.head else {
        return Ok(None);
    };
    let reference = if branch.starts_with("refs/") {
        branch
    } else {
        format!("refs/heads/{branch}")
    };
    RefId::new(reference)
        .map(Some)
        .map_err(|_| NativeIntegrationStackResolutionOutcomeV1::Unavailable)
}

impl NativeIntegrationStackResolutionPort for ExactPairNativeIntegrationTopology {
    fn resolve(
        &self,
        request: &NativeIntegrationStackResolutionRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationStackResolutionOutcomeV1, NativeIntegrationPortError> {
        if cancellation.is_cancelled() {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
        }
        if request.validate().is_err() {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
        }

        if matches!(
            request.selection,
            NativeIntegrationSelectionBindingV1::DeclaredStackEdge { .. }
        ) {
            return self.resolve_declared_stack(request, cancellation);
        }

        let NativeIntegrationSelectionBindingV1::IndependentBranch { proposal_digest } =
            &request.selection
        else {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
        };

        // The authorized scope must name the repository this resolver was
        // enrolled for. A mismatch is denied without revealing whether the
        // named target exists.
        if request.destination.project_id != self.project_id
            || request.destination.repository_id != self.repository_id
            || request.source.project_id != self.project_id
            || request.source.repository_id != self.repository_id
        {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Denied);
        }

        // `validate` already proved both references are present and distinct;
        // treat their absence as unresolvable rather than unwrapping.
        let (Some(source_ref), Some(destination_ref)) = (
            request.source.reference.as_ref(),
            request.destination.reference.as_ref(),
        ) else {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Unavailable);
        };

        let (Some(source_tip), Some(destination_tip)) =
            (self.tip(source_ref)?, self.tip(destination_ref)?)
        else {
            return Ok(NativeIntegrationStackResolutionOutcomeV1::Partial);
        };

        // Occupancy is observed, never assumed from the caller's scope: the
        // scope's worktree id names where the *request* came from, which is not
        // evidence about where a ref is checked out.
        let source_worktree_id = self
            .head_occupies(source_ref)?
            .then(|| request.source.worktree_id.clone());
        let destination_worktree_id = self
            .head_occupies(destination_ref)?
            .then(|| request.destination.worktree_id.clone());

        let selection = FrozenIndependentBranchSelectionV1::new(
            self.project_id.clone(),
            self.repository_id.clone(),
            request.inventory_snapshot_id.clone(),
            request.inventory_epoch,
            source_worktree_id,
            destination_worktree_id,
            source_ref.clone(),
            destination_ref.clone(),
            source_tip,
            destination_tip,
            proposal_digest.clone(),
            request.observed_at,
        )
        .map_err(domain_error)?;

        Ok(NativeIntegrationStackResolutionOutcomeV1::Complete(
            Box::new(NativeIntegrationSelectionV1::IndependentBranch(selection)),
        ))
    }
}
