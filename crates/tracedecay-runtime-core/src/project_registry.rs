//! Kernel-owned slice of the root `project_registry` module.
//!
//! `worktree::repository_identity_root` needs the primary-checkout derivation
//! and moved into this crate. The rule is pure path logic with no registry
//! state, so it moved down rather than becoming an injected parameter. The
//! root `project_registry` module re-exports it.

use std::path::{Path, PathBuf};

/// Derives the primary checkout root for a linked worktree from its git
/// common directory, or `None` when this checkout already is the primary one
/// or the repository has a shape whose primary checkout cannot be derived
/// safely (bare repos, submodule gitlinks).
pub fn primary_checkout_root(
    project_root: &Path,
    git_common_dir: Option<&Path>,
) -> Option<PathBuf> {
    let common_dir = git_common_dir?;
    // Only a plain, non-bare `<repo>/.git` common dir has a parent that is
    // reliably the checkout root. Bare repos and submodule gitlinks (whose
    // common dir lives under `.git/modules/...`) are left alone rather than
    // risk deriving a bogus "primary" and redirecting registration there.
    if common_dir.file_name().and_then(|name| name.to_str()) != Some(".git") {
        return None;
    }
    let primary_root = common_dir.parent()?;
    let canonical_project_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    if primary_root == canonical_project_root {
        // `project_root` already is the primary checkout.
        return None;
    }
    primary_root.is_dir().then(|| primary_root.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::primary_checkout_root;

    #[test]
    fn primary_checkout_root_redirects_linked_worktree_to_existing_primary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let primary = tmp.path().join("main");
        let worktree = tmp.path().join("main-wt");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        // `crate::worktree::git_common_dir` always returns a canonicalized
        // path — mirror that guarantee here rather than a raw join.
        let primary = primary.canonicalize().unwrap();
        let common_dir = primary.join(".git");
        std::fs::create_dir_all(&common_dir).unwrap();

        let redirected = primary_checkout_root(&worktree, Some(&common_dir));

        assert_eq!(
            redirected,
            Some(primary),
            "a linked worktree with a live primary checkout must redirect to it"
        );
    }

    #[test]
    fn primary_checkout_root_is_none_when_project_root_is_already_primary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let primary = tmp.path().join("main");
        std::fs::create_dir_all(&primary).unwrap();
        let primary = primary.canonicalize().unwrap();
        let common_dir = primary.join(".git");
        std::fs::create_dir_all(&common_dir).unwrap();

        assert_eq!(
            primary_checkout_root(&primary, Some(&common_dir)),
            None,
            "the primary checkout must never be redirected to itself"
        );
    }

    #[test]
    fn primary_checkout_root_is_none_without_git_common_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_root = tmp.path().join("not-a-worktree");
        std::fs::create_dir_all(&project_root).unwrap();

        assert_eq!(
            primary_checkout_root(&project_root, None),
            None,
            "non-git projects must register themselves unchanged"
        );
    }

    #[test]
    fn primary_checkout_root_keeps_worktree_when_primary_checkout_is_missing() {
        // The primary checkout no longer exists on disk (deleted, moved off
        // this machine, ...). A worktree-only project is legitimate and
        // must keep registering its own root rather than redirecting to a
        // path that doesn't exist.
        let tmp = tempfile::TempDir::new().unwrap();
        let missing_primary = tmp.path().join("deleted-main");
        let worktree = tmp.path().join("main-wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let common_dir = missing_primary.join(".git");

        assert_eq!(
            primary_checkout_root(&worktree, Some(&common_dir)),
            None,
            "a missing primary checkout must not be adopted as canonical_root"
        );
    }

    #[test]
    fn primary_checkout_root_ignores_non_dot_git_common_dirs() {
        // Bare repos and submodule gitlinks resolve `git_common_dir` to a
        // path that isn't a plain `<repo>/.git`, so the parent directory
        // isn't reliably a checkout root — leave registration alone rather
        // than risk deriving a bogus "primary".
        let tmp = tempfile::TempDir::new().unwrap();
        let worktree = tmp.path().join("checkout");
        std::fs::create_dir_all(&worktree).unwrap();
        let submodule_common_dir = tmp.path().join("main/.git/modules/sub");
        std::fs::create_dir_all(&submodule_common_dir).unwrap();

        assert_eq!(
            primary_checkout_root(&worktree, Some(&submodule_common_dir)),
            None,
            "non-`.git` common dirs must not redirect registration"
        );
    }
}
