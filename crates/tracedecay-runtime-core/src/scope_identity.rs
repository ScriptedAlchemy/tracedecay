//! Structural repository and worktree identity shared by application scopes
//! and code indexing. Authorization remains with the registered-root caller.

use std::path::{Path, PathBuf};

use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_domain::{RepositoryId, WorktreeId};

use crate::path_safety::canonical_existing_identity;

/// Failure to resolve the structural identity of an authorized project root.
#[derive(Debug, thiserror::Error)]
pub enum ScopeIdentityError {
    #[error("scope identity: canonical worktree path unavailable: {}: {source}", path.display())]
    CanonicalWorktreePath {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("scope identity: canonical id construction failed: {0}")]
    Domain(String),
}

/// Preserve the indexed repository identity: linked checkouts share the Git
/// common directory; a root outside Git uses the supplied project path.
pub fn repository_id_for(project_root: &Path) -> Result<RepositoryId, ScopeIdentityError> {
    let common =
        crate::worktree::git_common_dir(project_root).unwrap_or_else(|| project_root.to_path_buf());
    repository_id_for_common_dir(&common)
}

/// Resolve identity from an already-discovered repository common directory.
pub fn repository_id_for_common_dir(common_dir: &Path) -> Result<RepositoryId, ScopeIdentityError> {
    let digest = sha256_hex(common_dir.to_string_lossy().as_bytes());
    RepositoryId::new(format!("repository.daemon.{digest}"))
        .map_err(|error| ScopeIdentityError::Domain(error.to_string()))
}

/// Resolve one exact checkout or independently enrolled package path.
pub fn worktree_id_for(project_root: &Path) -> Result<WorktreeId, ScopeIdentityError> {
    let project_root = canonical_existing_identity(project_root).map_err(|source| {
        ScopeIdentityError::CanonicalWorktreePath {
            path: project_root.to_path_buf(),
            source,
        }
    })?;
    WorktreeId::new(format!(
        "worktree.daemon.{}",
        sha256_hex(project_root.to_string_lossy().as_bytes())
    ))
    .map_err(|error| ScopeIdentityError::Domain(error.to_string()))
}
