//! Compatibility façade for git worktree topology.

pub use tracedecay_runtime_core::worktree::{
    WorktreeIndexMismatch, detect_worktree_index_mismatch, git_common_dir, git_may_resolve_repo,
    git_worktree_root, is_detached_linked_worktree, worktree_mismatch_notice,
    worktree_mismatch_warning,
};
pub(crate) use tracedecay_runtime_core::worktree::{
    GitRepoIdentity, GitRepoIdentityOutcome, git_repo_identity, git_repo_identity_outcome,
};
