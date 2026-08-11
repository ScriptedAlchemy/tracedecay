//! Scope checks for sealed code-index generations.

use tracedecay_application::ResolvedScope;

use super::super::LatestCompleteCodeIndexV1;

/// The exact scope gate: project, repository, worktree, and reference must
/// equal the admitted scope. It admits only canonical scope digests.
pub(in crate::daemon::code_index_scheduler) fn latest_matches_scope(
    latest: &LatestCompleteCodeIndexV1,
    scope: &ResolvedScope,
) -> bool {
    latest_matches_scope_identity(latest, scope)
        && latest.generation().snapshot().reference == scope.reference
}

/// The relaxed scope gate for stale serving arms. A moved reference may retain
/// a sealed generation, but only for its canonical project/repository/worktree
/// authority; callers must mark that response stale.
pub(in crate::daemon::code_index_scheduler) fn latest_matches_scope_identity(
    latest: &LatestCompleteCodeIndexV1,
    scope: &ResolvedScope,
) -> bool {
    let generation = latest.generation();
    let snapshot = generation.snapshot();
    scope.validate().is_ok()
        && generation.manifest().project_id == scope.project_id
        && snapshot.repository == scope.repository_id
        && snapshot.worktree.as_ref() == Some(&scope.worktree_id)
}
