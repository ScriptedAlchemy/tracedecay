//! Git branch provenance helpers.
//!
//! Branches and worktrees select exact graph snapshots inside one project
//! store. They do not own databases, metadata files, or administration locks.

use std::path::Path;

/// Resolves the current branch name in-process using `gix`.
///
/// Returns `None` for detached HEAD or if the repository cannot be opened.
pub fn current_branch(project_root: &Path) -> Option<String> {
    match current_branch_gix(project_root) {
        GixHead::Branch(branch) => Some(branch),
        GixHead::Detached | GixHead::Unavailable => None,
    }
}

/// One live-branch resolution, scoped to a single request or write gate.
///
/// [`current_branch`] opens a `gix` repository. A single request can cross
/// several drift checks and write gates, each of which used to pay that cost
/// again. A `BranchMemo` is created at the request or gate entry, threaded
/// down, and dropped with the request.
///
/// This is deliberately **not** a TTL cache and must never be stored in a
/// long-lived value: drift detection has to notice a checkout on the very next
/// request, so every request starts from an unresolved memo.
#[derive(Debug, Clone)]
pub struct BranchMemo {
    root: std::path::PathBuf,
    resolved: std::sync::OnceLock<Option<String>>,
}

impl BranchMemo {
    /// A memo that will resolve the live branch of `root` at most once.
    #[must_use]
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            root: root.into(),
            resolved: std::sync::OnceLock::new(),
        }
    }

    /// A memo pre-seeded with an already-known resolution.
    #[must_use]
    pub fn resolved(root: impl Into<std::path::PathBuf>, branch: Option<String>) -> Self {
        let memo = Self::new(root);
        let _ = memo.resolved.set(branch);
        memo
    }

    /// The root this memo resolves against.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The live branch of this memo's root, resolving it on first use.
    #[must_use]
    pub fn get(&self) -> Option<&str> {
        self.resolved
            .get_or_init(|| current_branch(&self.root))
            .as_deref()
    }

    /// The live branch of `root`.
    ///
    /// Reuses this memo's single resolution when `root` is the root it was
    /// created for; a different root is a different HEAD, so it is resolved
    /// directly rather than answered from the memo.
    #[must_use]
    pub fn resolve_for(&self, root: &Path) -> Option<String> {
        if root == self.root {
            self.get().map(str::to_owned)
        } else {
            current_branch(root)
        }
    }
}

/// What gix could learn about HEAD without spawning `git`.
enum GixHead {
    /// HEAD points at a local branch.
    Branch(String),
    /// A readable repo whose HEAD is detached (or on a non-branch ref).
    Detached,
    /// No repo could be opened at this path or its HEAD was unreadable.
    Unavailable,
}

fn current_branch_gix(project_root: &Path) -> GixHead {
    let Ok(repo) = gix::open(project_root) else {
        return GixHead::Unavailable;
    };
    let Ok(head) = repo.head() else {
        return GixHead::Unavailable;
    };
    // `Head::name()` is always the literal "HEAD"; the branch HEAD points
    // to (if any) is the referent.
    let Some(name) = head.referent_name() else {
        return GixHead::Detached;
    };
    let Ok(name_str) = std::str::from_utf8(name.as_bstr()) else {
        return GixHead::Unavailable;
    };
    match name_str.strip_prefix("refs/heads/") {
        Some(branch) => GixHead::Branch(branch.to_string()),
        None => GixHead::Detached,
    }
}

#[cfg(test)]
mod branch_memo_tests {
    use super::BranchMemo;

    /// A memo answers repeated reads of its own root from one resolution, and
    /// refuses to answer for a different root — a different repository is a
    /// different HEAD, so it must be resolved directly.
    #[test]
    fn memo_serves_its_own_root_and_bypasses_for_another() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("worktree");
        let other = temp.path().join("elsewhere");
        std::fs::create_dir_all(&root).expect("worktree directory");
        std::fs::create_dir_all(&other).expect("other directory");

        // Seed a resolution that could not have come from these non-repository
        // paths, so any value observed below can only be the memoized one.
        let memo = BranchMemo::resolved(&root, Some("feature/pinned".to_owned()));

        assert_eq!(memo.get(), Some("feature/pinned"));
        assert_eq!(memo.get(), Some("feature/pinned"));
        assert_eq!(memo.root(), root.as_path());
        assert_eq!(
            memo.resolve_for(&root).as_deref(),
            Some("feature/pinned"),
            "the memo's own root must be served from the single resolution"
        );
        assert_eq!(
            memo.resolve_for(&other),
            None,
            "a different root must be resolved directly, not from the memo"
        );
    }

    /// An unseeded memo resolves lazily and caches even a `None` answer, so a
    /// detached HEAD or non-repository root is not re-probed within a request.
    #[test]
    fn unresolved_memo_caches_a_negative_answer() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let memo = BranchMemo::new(temp.path());
        assert_eq!(memo.get(), None);
        assert_eq!(memo.get(), None);
    }
}
