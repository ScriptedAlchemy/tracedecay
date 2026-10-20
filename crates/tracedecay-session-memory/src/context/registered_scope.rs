//! Registered-root resolution into the canonical application scope.

use std::path::{Path, PathBuf};

use tracedecay_contracts::ResolvedScope;
use tracedecay_domain::{ProjectId, RefId};
use tracedecay_runtime_core::git_repository::{
    GitRepositoryError, GitTopologyProbeError, try_repository_topology,
};
use tracedecay_runtime_core::path_safety::canonical_existing_identity;
use tracedecay_runtime_core::scope_identity::{repository_id_for, worktree_id_for};

use super::ApplicationScopeError;

/// Resolves only an explicitly registered root or another checkout whose
/// repository marker names the project.
pub struct RegisteredScopeResolver;

impl RegisteredScopeResolver {
    /// Authorize and canonicalize the exact root an identity authority may
    /// resolve.
    ///
    /// This method deliberately returns a path rather than minting repository
    /// or worktree identifiers. Composition roots use it before invoking their
    /// canonical identity authority, so path aliases cannot create a parallel
    /// identity namespace or affect a route cache before normalization.
    #[tracing::instrument(
        name = "usecases.context.registered_scope.authorize",
        level = "trace",
        skip_all
    )]
    pub fn canonical_scope_root(
        registered_root: &Path,
        requested_root: &Path,
        project_id: &ProjectId,
    ) -> Result<PathBuf, ApplicationScopeError> {
        resolve_authorized_root(registered_root, requested_root, project_id)
    }

    #[tracing::instrument(name = "usecases.context.registered_scope", level = "trace", skip_all)]
    pub fn resolve(
        registered_root: &Path,
        requested_root: &Path,
        project_id: &ProjectId,
    ) -> Result<ResolvedScope, ApplicationScopeError> {
        let scope_root = resolve_authorized_root(registered_root, requested_root, project_id)?;
        let repository_id = repository_id_for(&scope_root)
            .map_err(|error| ApplicationScopeError::Resolution(error.to_string()))?;
        let worktree_id = worktree_id_for(&scope_root)
            .map_err(|error| ApplicationScopeError::Resolution(error.to_string()))?;
        let reference = tracedecay_runtime_core::branch::current_branch(&scope_root)
            .and_then(|branch| RefId::new(format!("refs/heads/{branch}")).ok());
        let scope = ResolvedScope::new(project_id.clone(), repository_id, worktree_id, reference)
            .map_err(|error| ApplicationScopeError::Contract(error.to_string()))?;
        scope
            .validate()
            .map_err(|error| ApplicationScopeError::InconsistentScope(error.to_string()))?;
        Ok(scope)
    }
}

fn resolve_authorized_root(
    registered_root: &Path,
    requested_root: &Path,
    project_id: &ProjectId,
) -> Result<PathBuf, ApplicationScopeError> {
    let registered_root = canonical_root(registered_root, "registered root")?;
    let requested_root = canonical_root(requested_root, "requested root")?;
    // A checkout may be nested under the registered checkout. Resolve its
    // actual Git root before ordinary subdirectories converge on their owner;
    // lexical containment alone cannot authorize another repository.
    let scope_root = match try_repository_topology(&requested_root) {
        Ok(topology) => {
            let checkout = topology.worktree_root.clone().ok_or_else(|| {
                ApplicationScopeError::Resolution(format!(
                    "requested root '{}' has no Git checkout",
                    requested_root.display()
                ))
            })?;
            // An independently enrolled package owns its path-local identity;
            // discovery of the enclosing repository cannot widen that scope.
            if requested_root.starts_with(&registered_root)
                && !checkout.starts_with(&registered_root)
            {
                validate_non_git_project_identity(&registered_root, project_id)?;
                return Ok(registered_root);
            } else {
                checkout
            }
        }
        Err(GitTopologyProbeError::Repository(GitRepositoryError::NotARepository { .. })) => {
            if requested_root.starts_with(&registered_root) {
                registered_root.clone()
            } else {
                requested_root
            }
        }
        Err(error) => return Err(ApplicationScopeError::Resolution(error.to_string())),
    };
    let marker = tracedecay_runtime_core::storage::read_repository_identity_marker(&scope_root)
        .map_err(|error| ApplicationScopeError::Resolution(error.to_string()))?;
    match marker.as_ref() {
        Some(marker) if marker.project_id == project_id.as_str() => {}
        // A linked worktree or a clone outside the registered root is
        // authorized only by its own repository marker naming the project.
        _ if scope_root != registered_root => {
            return Err(ApplicationScopeError::UnauthorizedSiblingRoot {
                registered_root: registered_root.display().to_string(),
                requested_root: scope_root.display().to_string(),
            });
        }
        Some(_) => {
            return Err(ApplicationScopeError::Resolution(format!(
                "registered identity authority rejected '{}'",
                scope_root.display()
            )));
        }
        None if tracedecay_runtime_core::worktree::git_common_dir(&registered_root).is_some() => {
            return Err(ApplicationScopeError::Resolution(format!(
                "registered identity marker is unavailable for '{}'",
                registered_root.display()
            )));
        }
        None => validate_non_git_project_identity(&registered_root, project_id)?,
    }
    Ok(scope_root)
}

fn validate_non_git_project_identity(
    registered_root: &Path,
    project_id: &ProjectId,
) -> Result<(), ApplicationScopeError> {
    let derived_id = tracedecay_runtime_core::storage::default_profile_project_id(registered_root);
    if derived_id != project_id.as_str() {
        return Err(ApplicationScopeError::Resolution(format!(
            "deterministic identity for '{}' does not match registered project '{}'",
            registered_root.display(),
            project_id
        )));
    }
    Ok(())
}

fn canonical_root(root: &Path, label: &str) -> Result<PathBuf, ApplicationScopeError> {
    if !root.is_absolute() {
        return Err(ApplicationScopeError::RelativeRoot {
            requested_root: root.display().to_string(),
        });
    }
    canonical_existing_identity(root).map_err(|error| {
        ApplicationScopeError::Resolution(format!(
            "{label} '{}' could not be canonicalized: {error}",
            root.display()
        ))
    })
}
