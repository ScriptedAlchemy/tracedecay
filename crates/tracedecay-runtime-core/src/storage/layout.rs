use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::config;
use crate::errors::{Result, TraceDecayError};

use super::{
    EnrollmentMarker, ProjectIdentity, STORE_MANIFEST_FILENAME, StorageMode, StoreKind,
    StoreLayout, read_repository_identity_marker, validate_project_id,
};

pub fn profile_sharded_data_root(profile_root: &Path, project_id: &str) -> PathBuf {
    profile_root.join("projects").join(project_id)
}

fn project_id_for_identity_root(identity_root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(identity_root.to_string_lossy().as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("proj_{}", &digest[..16])
}

/// The id a store keyed to this exact directory would use, ignoring any
/// repository it belongs to.
///
/// Only discovery wants this. Discovery asks a narrower question than identity
/// resolution — not "which repository owns this checkout" but "was a store
/// ever minted for this exact directory" — and answering it with the
/// repository id would report every linked worktree of an initialized
/// repository as independently initialized.
pub fn path_local_profile_project_id(project_root: &Path) -> String {
    project_id_for_identity_root(
        &project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf()),
    )
}

/// The default identity for a project root.
///
/// This is the only place a project id is minted, so a linked worktree cannot
/// acquire a store of its own even when every marker and registry lookup has
/// missed: the fallback itself resolves to the repository. A primary checkout
/// resolves to itself, so every id minted before repository collapse existed
/// is byte-identical and no live store is orphaned.
pub fn default_profile_project_id(project_root: &Path) -> String {
    match crate::worktree::repository_identity_root(project_root) {
        Some(repository_root) => project_id_for_identity_root(&repository_root),
        None => path_local_profile_project_id(project_root),
    }
}

/// Whether a profile shard keyed to this exact path already holds a graph.
///
/// See [`path_local_profile_project_id`] for why discovery must not consult
/// the repository-collapsed identity here.
pub(crate) fn has_path_local_profile_store(project_root: &Path) -> bool {
    let Ok(profile_root) = default_profile_root() else {
        return false;
    };
    let data_root =
        profile_sharded_data_root(&profile_root, &path_local_profile_project_id(project_root));
    data_root.join(config::db_filename(&data_root)).exists()
}

pub fn default_profile_sharded_layout(
    project_root: &Path,
    profile_root: &Path,
) -> Result<StoreLayout> {
    let marker = EnrollmentMarker {
        project_id: default_profile_project_id(project_root),
        storage_mode: StorageMode::ProfileSharded,
    };
    profile_sharded_layout(project_root, profile_root, &marker)
}

pub fn profile_sharded_layout(
    project_root: &Path,
    profile_root: &Path,
    marker: &EnrollmentMarker,
) -> Result<StoreLayout> {
    if marker.storage_mode != StorageMode::ProfileSharded {
        return Err(TraceDecayError::Config {
            message: format!(
                "enrollment marker for '{}' uses storage_mode={:?}, not profile_sharded",
                project_root.display(),
                marker.storage_mode
            ),
        });
    }
    validate_project_id(&marker.project_id).map_err(|message| TraceDecayError::Config {
        message: format!(
            "invalid enrollment marker for '{}': {message}",
            project_root.display()
        ),
    })?;
    let data_root = profile_sharded_data_root(profile_root, &marker.project_id);
    Ok(StoreLayout::new(
        ProjectIdentity {
            project_id: Some(marker.project_id.clone()),
            display_root: project_root.to_path_buf(),
            primary_alias: project_root.to_path_buf(),
        },
        StoreKind::CodeProject,
        StorageMode::ProfileSharded,
        project_root.to_path_buf(),
        data_root,
        Some(STORE_MANIFEST_FILENAME),
    ))
}

pub fn resolve_layout(project_root: &Path, profile_root: &Path) -> Result<StoreLayout> {
    if let Some(layout) = resolve_persisted_layout(project_root, profile_root)? {
        return Ok(layout);
    }
    default_profile_sharded_layout(project_root, profile_root)
}

pub fn resolve_persisted_layout(
    project_root: &Path,
    profile_root: &Path,
) -> Result<Option<StoreLayout>> {
    // A linked worktree has its own checkout root but shares the repository
    // identity stored in Git's common directory. That authority must win over
    // any path-derived guess, or one repository can acquire two project
    // stores and two mutable writer lanes.
    if let Some(marker) = read_repository_identity_marker(project_root)? {
        return profile_sharded_layout(
            project_root,
            profile_root,
            &EnrollmentMarker {
                project_id: marker.project_id,
                storage_mode: StorageMode::ProfileSharded,
            },
        )
        .map(Some);
    }

    // Without a repository-side marker (a non-git project, or a repository
    // whose `.git/` marker was lost), the persisted evidence is the profile
    // shard itself: a store minted for this root's deterministic identity.
    // Nothing in the working tree carries identity.
    let project_id = default_profile_project_id(project_root);
    let data_root = profile_sharded_data_root(profile_root, &project_id);
    let store_exists = data_root.join(config::db_filename(&data_root)).exists()
        || data_root.join(STORE_MANIFEST_FILENAME).is_file();
    if store_exists {
        return profile_sharded_layout(
            project_root,
            profile_root,
            &EnrollmentMarker {
                project_id,
                storage_mode: StorageMode::ProfileSharded,
            },
        )
        .map(Some);
    }
    Ok(None)
}

pub fn default_profile_root() -> Result<PathBuf> {
    config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not resolve user profile data directory".to_string(),
    })
}

/// Synchronous store resolution for callers that cannot await the registry:
/// hooks, MCP response handles, config resolution, the agent command, Doctor,
/// and diagnostics.
///
/// This used to read only the enrollment marker and otherwise derive a project
/// id from the checkout path, so it disagreed with the async registry resolver
/// about the same directory and split one repository across shards. It now
/// consults every authority available without awaiting — the same enrollment
/// marker and repository identity marker via [`resolve_persisted_layout`].
pub fn resolve_layout_for_current_profile(project_root: &Path) -> Result<StoreLayout> {
    let profile_root = default_profile_root()?;
    match resolve_enrolled_layout(project_root, &profile_root)? {
        Some(layout) => Ok(layout),
        None => default_profile_sharded_layout(project_root, &profile_root),
    }
}

/// Resolves this checkout's store only when an authority already names it, and
/// reports `Ok(None)` when the answer would be a path-derived guess.
///
/// Callers that merely want somewhere to put a file — hook analytics is the
/// motivating one — must not enroll a directory as a side effect. Every
/// directory this resolver declines is a store shard that never gets minted for
/// a path that was never a project.
pub fn resolve_enrolled_layout_for_current_profile(
    project_root: &Path,
) -> Result<Option<StoreLayout>> {
    let profile_root = default_profile_root()?;
    resolve_enrolled_layout(project_root, &profile_root)
}

fn resolve_enrolled_layout(
    project_root: &Path,
    profile_root: &Path,
) -> Result<Option<StoreLayout>> {
    resolve_persisted_layout(project_root, profile_root)
}

pub fn resolve_project_session_db_path(project_root: &Path) -> Result<PathBuf> {
    Ok(resolve_layout_for_current_profile(project_root)?.sessions_db_path)
}

pub fn resolve_response_handle_root(project_root: &Path) -> Result<PathBuf> {
    Ok(resolve_layout_for_current_profile(project_root)?.response_handle_root)
}

pub fn resolve_lcm_payload_root(project_root: &Path) -> Result<PathBuf> {
    Ok(resolve_layout_for_current_profile(project_root)?.lcm_payload_root)
}
