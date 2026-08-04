//! Pure project-registry path resolution shared by registration adapters.

use std::path::{Path, PathBuf};

/// Resolves the canonical registration root for a project store rooted at
/// `project_root`.
pub fn primary_checkout_root(
    project_root: &Path,
    git_common_dir: Option<&Path>,
) -> Option<PathBuf> {
    let common_dir = git_common_dir?;
    if common_dir.file_name().and_then(|name| name.to_str()) != Some(".git") {
        return None;
    }
    let primary_root = common_dir.parent()?;
    let canonical_project_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    if primary_root == canonical_project_root {
        return None;
    }
    primary_root.is_dir().then(|| primary_root.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::primary_checkout_root;

    #[test]
    fn redirects_linked_worktree_to_existing_primary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let primary = tmp.path().join("main");
        let worktree = tmp.path().join("main-wt");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        let primary = primary.canonicalize().unwrap();
        let common_dir = primary.join(".git");
        std::fs::create_dir_all(&common_dir).unwrap();

        assert_eq!(
            primary_checkout_root(&worktree, Some(&common_dir)),
            Some(primary)
        );
    }

    #[test]
    fn keeps_primary_and_non_git_projects_unredirected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let primary = tmp.path().join("main");
        std::fs::create_dir_all(&primary).unwrap();
        let primary = primary.canonicalize().unwrap();
        let common_dir = primary.join(".git");
        std::fs::create_dir_all(&common_dir).unwrap();

        assert_eq!(primary_checkout_root(&primary, Some(&common_dir)), None);
        assert_eq!(primary_checkout_root(&primary, None), None);
    }
}
