//! Registered-root resolution into the canonical application scope.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{ProjectId, RefId, RepositoryId, WorktreeId};

use super::ApplicationScopeError;

/// Resolves only an explicitly registered root or a proved linked worktree.
pub struct RegisteredScopeResolver;

impl RegisteredScopeResolver {
    pub fn resolve(
        registered_root: &Path,
        requested_root: &Path,
        project_id: &ProjectId,
    ) -> Result<ResolvedScope, ApplicationScopeError> {
        let registered_root = canonical_root(registered_root, "registered root")?;
        let requested_root = canonical_root(requested_root, "requested root")?;
        let scope_root =
            if requested_root == registered_root || requested_root.starts_with(&registered_root) {
                registered_root.clone()
            } else {
                let registered_repository = git_common_dir(&registered_root)?;
                let requested_repository = git_common_dir(&requested_root)?;
                if registered_repository != requested_repository {
                    return Err(ApplicationScopeError::UnauthorizedSiblingRoot {
                        registered_root: registered_root.display().to_string(),
                        requested_root: requested_root.display().to_string(),
                    });
                }
                requested_root.clone()
            };
        let scope = if let Some(marker) =
            tracedecay_runtime_core::storage::read_repository_identity_marker(&registered_root)
                .map_err(|error| ApplicationScopeError::Resolution(error.to_string()))?
        {
            let identity = tracedecay_sessions::repository_provenance::
                RepositoryProvenanceAdmissionContext::from_authoritative_project_marker(
                    &scope_root,
                    project_id,
                    &marker,
                )
                .and_then(|authority| authority.admitted_identity())
                .ok_or_else(|| {
                    ApplicationScopeError::Resolution(format!(
                        "registered identity authority rejected '{}'",
                        scope_root.display()
                    ))
                })?;
            let reference = tracedecay_runtime_core::branch::current_branch(&scope_root)
                .and_then(|branch| RefId::new(format!("refs/heads/{branch}")).ok());
            ResolvedScope::new(identity.0, identity.1, identity.2, reference)
                .map_err(|error| ApplicationScopeError::Contract(error.to_string()))?
        } else if tracedecay_runtime_core::worktree::git_common_dir(&registered_root).is_none() {
            resolve_non_git_scope(&registered_root, &scope_root, project_id)?
        } else {
            return Err(ApplicationScopeError::Resolution(format!(
                "registered identity marker is unavailable for '{}'",
                registered_root.display()
            )));
        };
        scope
            .validate()
            .map_err(|error| ApplicationScopeError::InconsistentScope(error.to_string()))?;
        Ok(scope)
    }
}

/// Resolve an enrolled project that has no Git repository identity. A non-Git
/// project persists nothing in its working tree: its identity is
/// deterministic from the canonical project path, with the home-profile
/// registry as the durable authority. Requiring the supplied project id to
/// equal that deterministic identity keeps an arbitrary directory from
/// becoming an application scope merely because a caller supplies a
/// project id.
fn resolve_non_git_scope(
    registered_root: &Path,
    scope_root: &Path,
    project_id: &ProjectId,
) -> Result<ResolvedScope, ApplicationScopeError> {
    let derived_id = tracedecay_runtime_core::storage::default_profile_project_id(registered_root);
    if derived_id != project_id.as_str() {
        return Err(ApplicationScopeError::Resolution(format!(
            "deterministic identity for '{}' does not match registered project '{}'",
            registered_root.display(),
            project_id
        )));
    }

    let repository_id = RepositoryId::new(path_identity("repository.daemon", registered_root))
        .map_err(|error| ApplicationScopeError::Contract(error.to_string()))?;
    let worktree_id = WorktreeId::new(path_identity("worktree.daemon", scope_root))
        .map_err(|error| ApplicationScopeError::Contract(error.to_string()))?;
    let reference = tracedecay_runtime_core::branch::current_branch(scope_root)
        .and_then(|branch| RefId::new(format!("refs/heads/{branch}")).ok());
    ResolvedScope::new(project_id.clone(), repository_id, worktree_id, reference)
        .map_err(|error| ApplicationScopeError::Contract(error.to_string()))
}

fn path_identity(prefix: &str, root: &Path) -> String {
    let digest = hex::encode(Sha256::digest(root.to_string_lossy().as_bytes()));
    format!("{prefix}.{digest}")
}

fn canonical_root(root: &Path, label: &str) -> Result<PathBuf, ApplicationScopeError> {
    if !root.is_absolute() {
        return Err(ApplicationScopeError::RelativeRoot {
            requested_root: root.display().to_string(),
        });
    }
    root.canonicalize().map_err(|error| {
        ApplicationScopeError::Resolution(format!(
            "{label} '{}' could not be canonicalized: {error}",
            root.display()
        ))
    })
}

fn git_common_dir(root: &Path) -> Result<PathBuf, ApplicationScopeError> {
    tracedecay_runtime_core::worktree::git_common_dir(root)
        .ok_or_else(|| {
            ApplicationScopeError::Resolution(format!(
                "registered repository identity is unavailable for '{}'",
                root.display()
            ))
        })?
        .canonicalize()
        .map_err(|error| {
            ApplicationScopeError::Resolution(format!(
                "registered repository identity for '{}' could not be canonicalized: {error}",
                root.display()
            ))
        })
}
