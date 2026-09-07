use std::cmp::Ordering;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    BranchStackId, BranchStackRevisionId, BranchStackRevisionV1, ManifestDigest, ProjectId, RefId,
    RepositoryId, ScopeSetId, ScopeSetRevision, WorktreeId, WorktreeInventoryEpoch,
    WorktreeInventorySnapshotId,
};
use tracedecay_graph_db::GraphCancellation;

use super::{GitTopologyProjectionError, GitTopologyProjectionStore};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitBranchStackBindingV1 {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub scope_set_id: ScopeSetId,
    pub scope_set_revision: ScopeSetRevision,
    pub scope_set_digest: ManifestDigest,
    pub revision: BranchStackRevisionV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitWorktreeOccupancyV1 {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub scope_set_id: ScopeSetId,
    pub scope_set_revision: ScopeSetRevision,
    pub scope_set_digest: ManifestDigest,
    pub worktree_id: WorktreeId,
    pub reference: Option<RefId>,
}

impl GitTopologyProjectionStore {
    pub fn branch_stacks(&self) -> &[GitBranchStackBindingV1] {
        &self.branch_stacks
    }

    pub fn worktree_occupancies(&self) -> &[GitWorktreeOccupancyV1] {
        &self.worktree_occupancies
    }

    #[allow(clippy::too_many_arguments)]
    pub fn branch_stack_revision_exact(
        &self,
        project: &ProjectId,
        repository: &RepositoryId,
        scope_set_id: &ScopeSetId,
        scope_set_revision: ScopeSetRevision,
        scope_set_digest: &ManifestDigest,
        stack_id: &BranchStackId,
        revision_id: &BranchStackRevisionId,
        revision_digest: &ManifestDigest,
        inventory_snapshot_id: &WorktreeInventorySnapshotId,
        inventory_epoch: WorktreeInventoryEpoch,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<BranchStackRevisionV1>, GitTopologyProjectionError> {
        check_cancellation(cancellation.as_ref())?;
        validate_exact_binding_request(
            project,
            repository,
            scope_set_id,
            scope_set_revision,
            scope_set_digest,
        )?;
        stack_id
            .validate()
            .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
        revision_id
            .validate()
            .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
        revision_digest
            .validate()
            .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
        inventory_snapshot_id
            .validate()
            .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
        inventory_epoch
            .validate()
            .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
        self.validate_repository(repository)?;
        self.require_exact_scope_binding(
            project,
            repository,
            scope_set_id,
            scope_set_revision,
            scope_set_digest,
        )?;

        let stack_revisions = self
            .branch_stacks
            .iter()
            .filter(|binding| {
                exact_scope_matches(
                    binding.project_id.eq(project),
                    binding.repository_id.eq(repository),
                    binding.scope_set_id.eq(scope_set_id),
                    binding.scope_set_revision,
                    scope_set_revision,
                    binding.scope_set_digest.eq(scope_set_digest),
                ) && binding.revision.stack_id == *stack_id
            })
            .collect::<Vec<_>>();
        let Some(binding) = stack_revisions
            .iter()
            .copied()
            .find(|binding| binding.revision.revision_id == *revision_id)
        else {
            return if stack_revisions.is_empty() {
                Ok(None)
            } else {
                Err(GitTopologyProjectionError::StaleBinding {
                    detail: "branch-stack revision changed",
                })
            };
        };
        if binding.revision.digest != *revision_digest {
            return Err(GitTopologyProjectionError::StaleBinding {
                detail: "branch-stack revision digest changed",
            });
        }
        if binding.revision.inventory_snapshot_id != *inventory_snapshot_id
            || binding.revision.inventory_epoch != inventory_epoch
        {
            return Err(GitTopologyProjectionError::StaleBinding {
                detail: "worktree inventory fence changed",
            });
        }
        check_cancellation(cancellation.as_ref())?;
        Ok(Some(binding.revision.clone()))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn worktree_occupancy_exact(
        &self,
        project: &ProjectId,
        repository: &RepositoryId,
        scope_set_id: &ScopeSetId,
        scope_set_revision: ScopeSetRevision,
        scope_set_digest: &ManifestDigest,
        reference: &RefId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<WorktreeId>, GitTopologyProjectionError> {
        check_cancellation(cancellation.as_ref())?;
        validate_exact_binding_request(
            project,
            repository,
            scope_set_id,
            scope_set_revision,
            scope_set_digest,
        )?;
        reference
            .validate()
            .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
        self.validate_repository(repository)?;
        self.require_exact_scope_binding(
            project,
            repository,
            scope_set_id,
            scope_set_revision,
            scope_set_digest,
        )?;
        let mut worktrees = self
            .worktree_occupancies
            .iter()
            .filter(|occupancy| {
                exact_scope_matches(
                    occupancy.project_id.eq(project),
                    occupancy.repository_id.eq(repository),
                    occupancy.scope_set_id.eq(scope_set_id),
                    occupancy.scope_set_revision,
                    scope_set_revision,
                    occupancy.scope_set_digest.eq(scope_set_digest),
                ) && occupancy.reference.as_ref() == Some(reference)
            })
            .map(|occupancy| occupancy.worktree_id.clone())
            .collect::<Vec<_>>();
        worktrees.sort();
        check_cancellation(cancellation.as_ref())?;
        Ok(worktrees)
    }

    fn validate_repository(
        &self,
        repository: &RepositoryId,
    ) -> Result<(), GitTopologyProjectionError> {
        if &self.repository == repository {
            Ok(())
        } else {
            Err(GitTopologyProjectionError::RepositoryMismatch)
        }
    }

    fn require_exact_scope_binding(
        &self,
        project: &ProjectId,
        repository: &RepositoryId,
        scope_set_id: &ScopeSetId,
        scope_set_revision: ScopeSetRevision,
        scope_set_digest: &ManifestDigest,
    ) -> Result<(), GitTopologyProjectionError> {
        let logical_scope_matches =
            |candidate_project: &ProjectId,
             candidate_repository: &RepositoryId,
             candidate_scope_set: &ScopeSetId| {
                candidate_project == project
                    && candidate_repository == repository
                    && candidate_scope_set == scope_set_id
            };
        let branch_scopes = self.branch_stacks.iter().map(|binding| {
            (
                &binding.project_id,
                &binding.repository_id,
                &binding.scope_set_id,
                binding.scope_set_revision,
                &binding.scope_set_digest,
            )
        });
        let occupancy_scopes = self.worktree_occupancies.iter().map(|occupancy| {
            (
                &occupancy.project_id,
                &occupancy.repository_id,
                &occupancy.scope_set_id,
                occupancy.scope_set_revision,
                &occupancy.scope_set_digest,
            )
        });
        let scopes = branch_scopes.chain(occupancy_scopes).collect::<Vec<_>>();
        if scopes.iter().any(
            |(candidate_project, candidate_repository, candidate_scope_set, revision, digest)| {
                logical_scope_matches(candidate_project, candidate_repository, candidate_scope_set)
                    && *revision == scope_set_revision
                    && *digest == scope_set_digest
            },
        ) {
            return Ok(());
        }
        if scopes.iter().any(
            |(candidate_project, candidate_repository, candidate_scope_set, _, _)| {
                logical_scope_matches(candidate_project, candidate_repository, candidate_scope_set)
            },
        ) {
            return Err(GitTopologyProjectionError::StaleBinding {
                detail: "scope-set revision or digest changed",
            });
        }
        Err(GitTopologyProjectionError::Unavailable(
            "exact scope-set topology projection is unavailable".to_owned(),
        ))
    }
}

pub(super) fn validate_declared_topology(
    repository: &RepositoryId,
    branch_stacks: &[GitBranchStackBindingV1],
    worktree_occupancies: &[GitWorktreeOccupancyV1],
) -> Result<(), GitTopologyProjectionError> {
    for binding in branch_stacks {
        validate_branch_stack_binding(binding)?;
        if &binding.repository_id != repository {
            return Err(GitTopologyProjectionError::RepositoryMismatch);
        }
    }
    if branch_stacks.windows(2).any(|pair| {
        compare_branch_stack_bindings(&pair[0], &pair[1]).is_ge()
            || same_branch_stack_identity(&pair[0], &pair[1])
    }) {
        return Err(GitTopologyProjectionError::NonCanonicalBranchStacks);
    }
    for occupancy in worktree_occupancies {
        validate_worktree_occupancy(occupancy)?;
        if &occupancy.repository_id != repository {
            return Err(GitTopologyProjectionError::RepositoryMismatch);
        }
    }
    if worktree_occupancies.windows(2).any(|pair| {
        compare_worktree_occupancies(&pair[0], &pair[1]).is_ge()
            || same_worktree_occupancy_identity(&pair[0], &pair[1])
    }) {
        return Err(GitTopologyProjectionError::NonCanonicalWorktreeOccupancies);
    }
    Ok(())
}

fn validate_branch_stack_binding(
    binding: &GitBranchStackBindingV1,
) -> Result<(), GitTopologyProjectionError> {
    binding
        .project_id
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    binding
        .repository_id
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    binding
        .scope_set_id
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    binding
        .scope_set_revision
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    binding
        .scope_set_digest
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    binding
        .revision
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    if binding.revision.nodes.iter().any(|node| {
        node.project_id != binding.project_id || node.repository_id != binding.repository_id
    }) {
        return Err(GitTopologyProjectionError::Contract(
            "branch-stack nodes do not match their projection binding".to_owned(),
        ));
    }
    Ok(())
}

fn validate_worktree_occupancy(
    occupancy: &GitWorktreeOccupancyV1,
) -> Result<(), GitTopologyProjectionError> {
    occupancy
        .project_id
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    occupancy
        .repository_id
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    occupancy
        .scope_set_id
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    occupancy
        .scope_set_revision
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    occupancy
        .scope_set_digest
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    occupancy
        .worktree_id
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    if let Some(reference) = &occupancy.reference {
        reference
            .validate()
            .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    }
    Ok(())
}

pub(super) fn compare_branch_stack_bindings(
    left: &GitBranchStackBindingV1,
    right: &GitBranchStackBindingV1,
) -> Ordering {
    left.project_id
        .cmp(&right.project_id)
        .then_with(|| left.repository_id.cmp(&right.repository_id))
        .then_with(|| left.scope_set_id.cmp(&right.scope_set_id))
        .then_with(|| left.scope_set_revision.cmp(&right.scope_set_revision))
        .then_with(|| left.scope_set_digest.cmp(&right.scope_set_digest))
        .then_with(|| left.revision.stack_id.cmp(&right.revision.stack_id))
        .then_with(|| left.revision.revision_id.cmp(&right.revision.revision_id))
        .then_with(|| {
            left.revision
                .inventory_snapshot_id
                .cmp(&right.revision.inventory_snapshot_id)
        })
        .then_with(|| {
            left.revision
                .inventory_epoch
                .cmp(&right.revision.inventory_epoch)
        })
        .then_with(|| left.revision.digest.cmp(&right.revision.digest))
}

fn same_branch_stack_identity(
    left: &GitBranchStackBindingV1,
    right: &GitBranchStackBindingV1,
) -> bool {
    left.project_id == right.project_id
        && left.repository_id == right.repository_id
        && left.scope_set_id == right.scope_set_id
        && left.scope_set_revision == right.scope_set_revision
        && left.scope_set_digest == right.scope_set_digest
        && left.revision.stack_id == right.revision.stack_id
        && left.revision.revision_id == right.revision.revision_id
}

pub(super) fn compare_worktree_occupancies(
    left: &GitWorktreeOccupancyV1,
    right: &GitWorktreeOccupancyV1,
) -> Ordering {
    left.project_id
        .cmp(&right.project_id)
        .then_with(|| left.repository_id.cmp(&right.repository_id))
        .then_with(|| left.scope_set_id.cmp(&right.scope_set_id))
        .then_with(|| left.scope_set_revision.cmp(&right.scope_set_revision))
        .then_with(|| left.scope_set_digest.cmp(&right.scope_set_digest))
        .then_with(|| left.worktree_id.cmp(&right.worktree_id))
        .then_with(|| left.reference.cmp(&right.reference))
}

fn same_worktree_occupancy_identity(
    left: &GitWorktreeOccupancyV1,
    right: &GitWorktreeOccupancyV1,
) -> bool {
    left.project_id == right.project_id
        && left.repository_id == right.repository_id
        && left.scope_set_id == right.scope_set_id
        && left.scope_set_revision == right.scope_set_revision
        && left.scope_set_digest == right.scope_set_digest
        && left.worktree_id == right.worktree_id
}

fn validate_exact_binding_request(
    project: &ProjectId,
    repository: &RepositoryId,
    scope_set_id: &ScopeSetId,
    scope_set_revision: ScopeSetRevision,
    scope_set_digest: &ManifestDigest,
) -> Result<(), GitTopologyProjectionError> {
    project
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    repository
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    scope_set_id
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    scope_set_revision
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    scope_set_digest
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    Ok(())
}

fn exact_scope_matches(
    project_matches: bool,
    repository_matches: bool,
    scope_set_matches: bool,
    candidate_revision: ScopeSetRevision,
    requested_revision: ScopeSetRevision,
    digest_matches: bool,
) -> bool {
    project_matches
        && repository_matches
        && scope_set_matches
        && candidate_revision == requested_revision
        && digest_matches
}

fn check_cancellation(
    cancellation: &dyn GraphCancellation,
) -> Result<(), GitTopologyProjectionError> {
    if cancellation.is_cancelled() {
        Err(GitTopologyProjectionError::Cancelled)
    } else {
        Ok(())
    }
}
