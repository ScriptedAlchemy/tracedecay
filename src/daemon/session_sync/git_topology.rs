use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_application::{AuthorizedRoot, AuthorizedScopeSet};
use tracedecay_code_index::git_projection::{
    GIT_TOPOLOGY_PROJECTOR_REVISION_V1, GitTopologyProjectionError, GitTopologyProjectionStore,
    build_git_topology_manifest_checked, git_topology_idempotency_key, git_topology_namespace,
    git_topology_projection_identity,
};
use tracedecay_domain::{GitHeadStateV1, RefId, RepositoryId, WorktreeId};
use tracedecay_global_db::VerifiedGraphRuntimePortV1;
use tracedecay_graph_db::{GraphCancellation, GraphDbError, GraphProjectorRevision};
use tracedecay_runtime_core::git_repository::GitRepositoryAuthority;
use tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetSqliteStorage;
use tracedecay_store::FactReadControl;

use crate::git_intelligence::{GIT_HISTORY_MAX_COUNT_LIMIT, NativeGitIntelligence};
use crate::global_db::RegisteredGlobalDbLeaseV1;

use super::{SESSION_SYNC_POLL_INTERVAL, SessionSyncProjectContext, work::SessionSyncInterruption};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GitTopologySyncFailure {
    Stale,
    Denied,
    Unavailable,
}

impl GitTopologySyncFailure {
    pub(super) const fn failure_code(self) -> &'static str {
        match self {
            Self::Stale => "git_topology_declared_state_stale",
            Self::Denied => "git_topology_declared_authority_denied",
            Self::Unavailable => "git_topology_declared_authority_unavailable",
        }
    }
}

pub(super) enum GitTopologySyncOutcome {
    Finished(Result<(), GitTopologySyncFailure>),
    Interrupted(SessionSyncInterruption),
}

impl SessionSyncProjectContext {
    pub(super) async fn publish_git_topology(
        &self,
        request: &tracedecay_application::session_sync::SessionSyncRequestV1,
        shutdown: &tracedecay_usecases::observation::ObservationCancellation,
        project_sessions: RegisteredGlobalDbLeaseV1,
    ) -> GitTopologySyncOutcome {
        let scope = match crate::daemon::project_open_owners::resolved_scope_for_project(
            &self.project_root,
            &self.project_id,
        ) {
            Ok(scope) => scope,
            Err(_) => {
                return GitTopologySyncOutcome::Finished(Err(GitTopologySyncFailure::Unavailable));
            }
        };
        let Some(runtime) = project_sessions.project_graph_runtime().cloned() else {
            return GitTopologySyncOutcome::Finished(Err(GitTopologySyncFailure::Unavailable));
        };
        let scope_sets = match project_sessions.authorized_scope_set_storage() {
            Ok(scope_sets) => scope_sets,
            Err(_) => {
                return GitTopologySyncOutcome::Finished(Err(GitTopologySyncFailure::Unavailable));
            }
        };
        let project_root = self.project_root.clone();
        let repository = scope.repository_id;
        let worktree = scope.worktree_id;
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let worker = tokio::task::spawn_blocking(move || {
            publish_native_topology(
                runtime,
                project_root,
                repository,
                worktree,
                scope_sets,
                worker_cancelled,
            )
        });
        tokio::pin!(worker);
        let mut interruption = None;
        loop {
            tokio::select! {
                result = &mut worker => {
                    return match interruption {
                        Some(interruption) => GitTopologySyncOutcome::Interrupted(interruption),
                        None => GitTopologySyncOutcome::Finished(match result {
                            Ok(result) => result,
                            Err(_) => Err(GitTopologySyncFailure::Unavailable),
                        }),
                    };
                }
                () = tokio::time::sleep(SESSION_SYNC_POLL_INTERVAL) => {
                    if interruption.is_none() {
                        if shutdown.is_cancelled() {
                            interruption = Some(SessionSyncInterruption::Shutdown);
                            cancelled.store(true, Ordering::Release);
                        } else if request.cancellation().is_cancelled() {
                            interruption = Some(SessionSyncInterruption::Cancelled);
                            cancelled.store(true, Ordering::Release);
                        } else if request.deadline().is_elapsed_at(
                            tracedecay_application::now_micros(),
                        ) {
                            interruption = Some(SessionSyncInterruption::TimedOut);
                            cancelled.store(true, Ordering::Release);
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone)]
struct GitTopologySyncCancellation(Arc<AtomicBool>);

impl GraphCancellation for GitTopologySyncCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub(super) fn publish_native_topology(
    runtime: Arc<dyn VerifiedGraphRuntimePortV1>,
    project_root: PathBuf,
    repository: RepositoryId,
    worktree: WorktreeId,
    scope_sets: AuthorizedScopeSetSqliteStorage,
    cancelled: Arc<AtomicBool>,
) -> Result<(), GitTopologySyncFailure> {
    let identity = git_topology_projection_identity(
        git_topology_namespace(&repository).map_err(|_| GitTopologySyncFailure::Unavailable)?,
    )
    .map_err(|_| GitTopologySyncFailure::Unavailable)?;
    let read_cancelled = Arc::clone(&cancelled);
    let current = runtime
        .verified_snapshot(
            &identity,
            FactReadControl::new(Arc::new(move || read_cancelled.load(Ordering::Acquire))),
        )
        .map_err(|_| GitTopologySyncFailure::Unavailable)?;
    let (branch_stacks, worktree_occupancies) = match current {
        Some(snapshot) => {
            let store = GitTopologyProjectionStore::from_verified_snapshot_verified(
                snapshot,
                Arc::new(GitTopologySyncCancellation(Arc::clone(&cancelled))),
            )
            .map_err(|_| GitTopologySyncFailure::Unavailable)?;
            validate_retained_declared_topology(
                &repository,
                &project_root,
                &store,
                &scope_sets,
                &cancelled,
            )?;
            (
                store.branch_stacks().to_vec(),
                store.worktree_occupancies().to_vec(),
            )
        }
        None => (Vec::new(), Vec::new()),
    };
    let adapter = NativeGitIntelligence::new(project_root, repository.clone(), worktree);
    let projection = adapter
        .topology_projection(GIT_HISTORY_MAX_COUNT_LIMIT)
        .map_err(|_| GitTopologySyncFailure::Unavailable)?
        .with_declared_topology(branch_stacks, worktree_occupancies)
        .map_err(|_| GitTopologySyncFailure::Unavailable)?;
    let revision = GraphProjectorRevision::try_from(GIT_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned())
        .map_err(|_| GitTopologySyncFailure::Unavailable)?;
    let check = || {
        if cancelled.load(Ordering::Relaxed) {
            Err(GraphDbError::Cancelled)
        } else {
            Ok(())
        }
    };
    let manifest = build_git_topology_manifest_checked(identity, &projection, &revision, &check)
        .map_err(|_| GitTopologySyncFailure::Unavailable)?;
    let idempotency = git_topology_idempotency_key(&projection, &revision)
        .map_err(|_| GitTopologySyncFailure::Unavailable)?;
    runtime
        .publish_verified_manifest(&manifest, idempotency, cancelled)
        .map_err(|_| GitTopologySyncFailure::Unavailable)?;
    Ok(())
}

pub(super) fn validate_retained_declared_topology(
    repository: &RepositoryId,
    repository_root: &Path,
    topology: &GitTopologyProjectionStore,
    storage: &AuthorizedScopeSetSqliteStorage,
    cancelled: &Arc<AtomicBool>,
) -> Result<(), GitTopologySyncFailure> {
    let enrolled = GitRepositoryAuthority::discover(repository_root)
        .map_err(|_| GitTopologySyncFailure::Unavailable)?;
    for binding in topology.branch_stacks() {
        let scope_set = exact_scope_set(
            storage,
            &binding.scope_set_id,
            binding.scope_set_revision,
            &binding.scope_set_digest,
        )?;
        let exact_revision = topology
            .branch_stack_revision_exact(
                &binding.project_id,
                &binding.repository_id,
                &binding.scope_set_id,
                binding.scope_set_revision,
                &binding.scope_set_digest,
                &binding.revision.stack_id,
                &binding.revision.revision_id,
                &binding.revision.digest,
                &binding.revision.inventory_snapshot_id,
                binding.revision.inventory_epoch,
                Arc::new(GitTopologySyncCancellation(Arc::clone(cancelled))),
            )
            .map_err(topology_read_failure)?;
        if exact_revision.as_ref() != Some(&binding.revision) {
            return Err(GitTopologySyncFailure::Stale);
        }
        let expected_worktrees = scope_set
            .roots()
            .iter()
            .filter(|root| {
                root.scope().project_id == binding.project_id
                    && root.scope().repository_id == binding.repository_id
            })
            .map(|root| root.scope().worktree_id.clone())
            .collect::<BTreeSet<_>>();
        let projected_worktrees = topology
            .worktree_occupancies()
            .iter()
            .filter(|occupancy| {
                occupancy.project_id == binding.project_id
                    && occupancy.repository_id == binding.repository_id
                    && occupancy.scope_set_id == binding.scope_set_id
                    && occupancy.scope_set_revision == binding.scope_set_revision
                    && occupancy.scope_set_digest == binding.scope_set_digest
            })
            .map(|occupancy| occupancy.worktree_id.clone())
            .collect::<BTreeSet<_>>();
        if expected_worktrees != projected_worktrees {
            return Err(GitTopologySyncFailure::Stale);
        }
        for node in &binding.revision.nodes {
            let roots = scope_set
                .roots()
                .iter()
                .filter(|root| {
                    root.scope().project_id == node.project_id
                        && root.scope().repository_id == node.repository_id
                        && root.scope().reference.as_ref() == Some(&node.reference)
                        && node
                            .worktree_id
                            .as_ref()
                            .is_none_or(|worktree| &root.scope().worktree_id == worktree)
                })
                .collect::<Vec<_>>();
            if roots.is_empty() {
                return Err(GitTopologySyncFailure::Denied);
            }
            for root in roots {
                let authority = root_authority(root, repository, &enrolled)?;
                let tip = authority
                    .exact_reference_tip(node.reference.as_str())
                    .map_err(|_| GitTopologySyncFailure::Unavailable)?;
                if tip.as_str() != node.tip.as_str() {
                    return Err(GitTopologySyncFailure::Stale);
                }
            }
            let occupied = occupied_worktrees(
                &scope_set,
                &binding.project_id,
                &binding.repository_id,
                &node.reference,
                &enrolled,
            )?;
            let projected_occupied = topology
                .worktree_occupancy_exact(
                    &binding.project_id,
                    &binding.repository_id,
                    &binding.scope_set_id,
                    binding.scope_set_revision,
                    &binding.scope_set_digest,
                    &node.reference,
                    Arc::new(GitTopologySyncCancellation(Arc::clone(cancelled))),
                )
                .map_err(topology_read_failure)?;
            if projected_occupied != occupied {
                return Err(GitTopologySyncFailure::Stale);
            }
            if occupied.len() > 1 || occupied.first() != node.worktree_id.as_ref() {
                return Err(GitTopologySyncFailure::Stale);
            }
        }
    }
    for occupancy in topology.worktree_occupancies() {
        let scope_set = exact_scope_set(
            storage,
            &occupancy.scope_set_id,
            occupancy.scope_set_revision,
            &occupancy.scope_set_digest,
        )?;
        let roots = scope_set
            .roots()
            .iter()
            .filter(|root| {
                root.scope().project_id == occupancy.project_id
                    && root.scope().repository_id == occupancy.repository_id
                    && root.scope().worktree_id == occupancy.worktree_id
            })
            .collect::<Vec<_>>();
        if roots.len() != 1 {
            return Err(GitTopologySyncFailure::Denied);
        }
        let authority = root_authority(roots[0], repository, &enrolled)?;
        if !head_occupancy_matches(&authority, occupancy.reference.as_ref())? {
            return Err(GitTopologySyncFailure::Stale);
        }
    }
    Ok(())
}

fn topology_read_failure(error: GitTopologyProjectionError) -> GitTopologySyncFailure {
    match error {
        GitTopologyProjectionError::Stale { .. }
        | GitTopologyProjectionError::StaleBinding { .. } => GitTopologySyncFailure::Stale,
        GitTopologyProjectionError::RepositoryMismatch => GitTopologySyncFailure::Denied,
        _ => GitTopologySyncFailure::Unavailable,
    }
}

fn exact_scope_set(
    storage: &AuthorizedScopeSetSqliteStorage,
    id: &tracedecay_domain::ScopeSetId,
    revision: tracedecay_domain::ScopeSetRevision,
    digest: &tracedecay_domain::ManifestDigest,
) -> Result<AuthorizedScopeSet, GitTopologySyncFailure> {
    let scope_set = storage
        .read(id)
        .map_err(|_| GitTopologySyncFailure::Unavailable)?
        .ok_or(GitTopologySyncFailure::Unavailable)?;
    if scope_set.revision() != revision || scope_set.digest() != digest {
        return Err(GitTopologySyncFailure::Stale);
    }
    Ok(scope_set)
}

fn occupied_worktrees(
    scope_set: &AuthorizedScopeSet,
    project: &tracedecay_domain::ProjectId,
    repository: &RepositoryId,
    reference: &RefId,
    enrolled: &GitRepositoryAuthority,
) -> Result<Vec<WorktreeId>, GitTopologySyncFailure> {
    let mut occupied = Vec::new();
    for root in scope_set.roots().iter().filter(|root| {
        &root.scope().project_id == project && &root.scope().repository_id == repository
    }) {
        let authority = root_authority(root, repository, enrolled)?;
        if head_occupancy_matches(&authority, Some(reference))? {
            occupied.push(root.scope().worktree_id.clone());
        }
    }
    occupied.sort();
    occupied.dedup();
    Ok(occupied)
}

fn root_authority(
    root: &AuthorizedRoot,
    repository: &RepositoryId,
    enrolled: &GitRepositoryAuthority,
) -> Result<GitRepositoryAuthority, GitTopologySyncFailure> {
    if &root.scope().repository_id != repository {
        return Err(GitTopologySyncFailure::Denied);
    }
    let locator = root.locator().ok_or(GitTopologySyncFailure::Unavailable)?;
    let authority = GitRepositoryAuthority::discover(&locator.canonical_root)
        .map_err(|_| GitTopologySyncFailure::Unavailable)?;
    if authority.common_dir() != enrolled.common_dir() {
        return Err(GitTopologySyncFailure::Denied);
    }
    Ok(authority)
}

fn head_occupancy_matches(
    authority: &GitRepositoryAuthority,
    reference: Option<&RefId>,
) -> Result<bool, GitTopologySyncFailure> {
    let status = authority
        .status()
        .map_err(|_| GitTopologySyncFailure::Unavailable)?;
    Ok(match (&status.head, reference) {
        (GitHeadStateV1::Attached { branch, .. }, Some(reference)) => {
            branch == reference.as_str()
                || reference
                    .as_str()
                    .strip_prefix("refs/heads/")
                    .is_some_and(|short| branch == short)
        }
        (GitHeadStateV1::Detached { .. } | GitHeadStateV1::Unborn { .. }, None) => true,
        _ => false,
    })
}
