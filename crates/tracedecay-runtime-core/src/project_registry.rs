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
pub fn primary_checkout_root(project_root: &Path, git_common_dir: Option<&Path>) -> Option<PathBuf> {
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
