//! Registered-root resolution into the canonical application scope.

use std::path::{Path, PathBuf};

use tracedecay_application::ResolvedScope;
use tracedecay_domain::{ProjectId, RefId};

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
        let marker =
            tracedecay_runtime_core::storage::read_repository_identity_marker(&registered_root)
                .map_err(|error| ApplicationScopeError::Resolution(error.to_string()))?
                .ok_or_else(|| {
                    ApplicationScopeError::Resolution(format!(
                        "registered identity marker is unavailable for '{}'",
                        registered_root.display()
                    ))
                })?;
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
        let scope = ResolvedScope::new(identity.0, identity.1, identity.2, reference)
            .map_err(|error| ApplicationScopeError::Contract(error.to_string()))?;
        scope
            .validate()
            .map_err(|error| ApplicationScopeError::InconsistentScope(error.to_string()))?;
        Ok(scope)
    }
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
