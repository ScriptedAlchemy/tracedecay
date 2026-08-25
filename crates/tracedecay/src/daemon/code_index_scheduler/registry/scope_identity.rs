//! Scope checks for sealed code-index generations.

use tracedecay_application::ResolvedScope;

use super::super::LatestCompleteCodeIndexV1;

/// The serving scope gate: project, repository, and worktree must equal the
/// admitted scope's checkout identity. It admits only canonical scope digests.
///
/// The sealed `reference` is deliberately not compared. Repository and
/// worktree are checkout identity; the reference is the branch label HEAD
/// happened to carry when the generation was sealed (or when the caller's
/// scope was resolved), and it moves under a fixed worktree on every ordinary
/// commit, branch switch, or rebase. Demanding label equality here refused
/// the exact checkout's own generation whenever the two resolutions straddled
/// a label move — a retained route scope pinned at project open outlived
/// every later `git switch`, so the graph the daemon was actively indexing
/// became permanently unservable for that route. Currency is not this gate's
/// job either: the ready ladder proves the generation against the live
/// worktree (git metadata fingerprint plus stat signature) before it is
/// admitted, and the label the generation was sealed under stays on its own
/// snapshot, so attribution remains generation-bound.
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
