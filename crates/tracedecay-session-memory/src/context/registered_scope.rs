//! Registered-root resolution into the canonical application scope.

use std::path::{Path, PathBuf};

use tracedecay_application::ResolvedScope;
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_domain::{ProjectId, RefId, RepositoryId, WorktreeId};
use tracedecay_runtime_core::storage::RepositoryIdentityMarker;

use super::ApplicationScopeError;

/// Resolves only an explicitly registered root or a proved linked worktree.
pub struct RegisteredScopeResolver;

impl RegisteredScopeResolver {
    /// Authorize and canonicalize the exact root an identity authority may
    /// resolve.
    ///
    /// This method deliberately returns a path rather than minting repository
    /// or worktree identifiers. Composition roots use it before invoking their
    /// canonical identity authority, so path aliases cannot create a parallel
    /// identity namespace or affect a route cache before normalization.
    #[hotpath::measure(label = "usecases.context.registered_scope.authorize")]
    pub fn canonical_scope_root(
        registered_root: &Path,
        requested_root: &Path,
        project_id: &ProjectId,
    ) -> Result<PathBuf, ApplicationScopeError> {
        resolve_authorized_root(registered_root, requested_root, project_id)
            .map(|resolved| resolved.scope_root)
    }

    #[hotpath::measure(label = "usecases.context.registered_scope")]
    pub fn resolve(
        registered_root: &Path,
        requested_root: &Path,
        project_id: &ProjectId,
    ) -> Result<ResolvedScope, ApplicationScopeError> {
        let resolved = resolve_authorized_root(registered_root, requested_root, project_id)?;
        let scope = if let Some(marker) = resolved.marker {
            let identity = tracedecay_sessions::repository_provenance::
                RepositoryProvenanceAdmissionContext::from_authoritative_project_marker(
                    &resolved.scope_root,
                    project_id,
                    &marker,
                )
                .and_then(|authority| authority.admitted_identity())
                .ok_or_else(|| {
                    ApplicationScopeError::Resolution(format!(
                        "registered identity authority rejected '{}'",
                        resolved.scope_root.display()
                    ))
                })?;
            let reference = tracedecay_runtime_core::branch::current_branch(&resolved.scope_root)
                .and_then(|branch| RefId::new(format!("refs/heads/{branch}")).ok());
            ResolvedScope::new(identity.0, identity.1, identity.2, reference)
                .map_err(|error| ApplicationScopeError::Contract(error.to_string()))?
        } else {
            resolve_non_git_scope(&resolved.registered_root, &resolved.scope_root, project_id)?
        };
        scope
            .validate()
            .map_err(|error| ApplicationScopeError::InconsistentScope(error.to_string()))?;
        Ok(scope)
    }
}

struct AuthorizedRoot {
    registered_root: PathBuf,
    scope_root: PathBuf,
    marker: Option<RepositoryIdentityMarker>,
}

fn resolve_authorized_root(
    registered_root: &Path,
    requested_root: &Path,
    project_id: &ProjectId,
) -> Result<AuthorizedRoot, ApplicationScopeError> {
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
            requested_root
        };
    let marker =
        tracedecay_runtime_core::storage::read_repository_identity_marker(&registered_root)
            .map_err(|error| ApplicationScopeError::Resolution(error.to_string()))?;
    if let Some(marker) = marker.as_ref() {
        if marker.project_id != project_id.as_str() {
            return Err(ApplicationScopeError::Resolution(format!(
                "registered identity authority rejected '{}'",
                scope_root.display()
            )));
        }
    } else if tracedecay_runtime_core::worktree::git_common_dir(&registered_root).is_some() {
        return Err(ApplicationScopeError::Resolution(format!(
            "registered identity marker is unavailable for '{}'",
            registered_root.display()
        )));
    } else {
        validate_non_git_project_identity(&registered_root, project_id)?;
    }
    Ok(AuthorizedRoot {
        registered_root,
        scope_root,
        marker,
    })
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
    let repository_id = RepositoryId::new(path_identity("repository.daemon", registered_root))
        .map_err(|error| ApplicationScopeError::Contract(error.to_string()))?;
    let worktree_id = WorktreeId::new(path_identity("worktree.daemon", scope_root))
        .map_err(|error| ApplicationScopeError::Contract(error.to_string()))?;
    let reference = tracedecay_runtime_core::branch::current_branch(scope_root)
        .and_then(|branch| RefId::new(format!("refs/heads/{branch}")).ok());
    ResolvedScope::new(project_id.clone(), repository_id, worktree_id, reference)
        .map_err(|error| ApplicationScopeError::Contract(error.to_string()))
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

fn path_identity(prefix: &str, root: &Path) -> String {
    let digest = sha256_hex(root.to_string_lossy().as_bytes());
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
