use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::config;
use crate::errors::{Result, TraceDecayError};

use super::{
    EnrollmentMarker, IDENTITY_CUTOVER_BACKUP_MANIFEST_FILENAME, ProjectIdentity,
    STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreKind, StoreLayout,
    read_enrollment_marker, read_repository_identity_marker, read_store_manifest,
    validate_project_id,
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
    if let Some(marker) = read_enrollment_marker(project_root)? {
        if marker.storage_mode != StorageMode::ProfileSharded {
            return Err(TraceDecayError::Config {
                message: format!(
                    "unsupported storage_mode={:?} in enrollment marker for '{}'; \
                     run TraceDecay migration to move this project into the user profile store",
                    marker.storage_mode,
                    project_root.display()
                ),
            });
        }
        return profile_sharded_layout(project_root, profile_root, &marker).map(Some);
    }
    let Some(marker) = read_repository_identity_marker(project_root)? else {
        return Ok(None);
    };
    profile_sharded_layout(
        project_root,
        profile_root,
        &EnrollmentMarker {
            project_id: marker.project_id,
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .map(Some)
}

/// Finds pre-repository-identity profile stores that were keyed by an older
/// path-derived project id but still name this exact local checkout, or one of
/// its linked worktrees, in their manifest. Remote URLs are deliberately not
/// considered: two clones of one remote are different local identities.
pub fn matching_legacy_profile_layouts(
    project_root: &Path,
    profile_root: &Path,
    excluded_project_id: Option<&str>,
) -> Result<(Vec<StoreLayout>, bool)> {
    matching_legacy_profile_layouts_with_git_resolver(
        project_root,
        profile_root,
        excluded_project_id,
        crate::worktree::git_common_dir,
    )
}

pub(super) fn matching_legacy_profile_layouts_with_git_resolver<G>(
    project_root: &Path,
    profile_root: &Path,
    excluded_project_id: Option<&str>,
    mut git_common_dir: G,
) -> Result<(Vec<StoreLayout>, bool)>
where
    G: FnMut(&Path) -> Option<PathBuf>,
{
    let projects_root = profile_root.join("projects");
    let Ok(entries) = fs::read_dir(&projects_root) else {
        return Ok((Vec::new(), false));
    };
    let mut manifest_paths = entries
        .flatten()
        .map(|entry| entry.path().join(STORE_MANIFEST_FILENAME))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    manifest_paths.sort();

    let mut exact_manifests = Vec::new();
    let mut non_exact_manifests = Vec::new();
    let mut selected_manifest_matches_exact_root = false;
    for manifest_path in manifest_paths {
        let Ok(manifest) = read_store_manifest(&manifest_path) else {
            continue;
        };
        let exact_root = same_local_path(&manifest.project_root, project_root);
        if manifest.project_id.is_some() && manifest.project_id.as_deref() == excluded_project_id {
            selected_manifest_matches_exact_root |= exact_root;
            continue;
        }
        if exact_root {
            exact_manifests.push((manifest_path, manifest));
            continue;
        }
        non_exact_manifests.push((manifest_path, manifest));
    }

    // A linked worktree may have its own profile shard while sharing a Git
    // common directory with every sibling checkout. A non-excluded exact
    // manifest overrides the selected identity. Otherwise the shared-Git
    // recovery path still runs, and the caller decides whether a selected
    // identity naming this exact checkout outranks what it finds.
    let selected_is_sole_exact_root =
        selected_manifest_matches_exact_root && exact_manifests.is_empty();
    let matching_manifests = if exact_manifests.is_empty() {
        let project_git_common_dir = git_common_dir(project_root);
        let mut legacy_git_common_dirs = HashMap::<PathBuf, Option<PathBuf>>::new();
        non_exact_manifests
            .into_iter()
            .filter(|(_, manifest)| {
                project_git_common_dir.as_deref().is_some_and(|current| {
                    legacy_git_common_dirs
                        .entry(manifest.project_root.clone())
                        .or_insert_with(|| {
                            manifest
                                .project_root
                                .is_dir()
                                .then(|| git_common_dir(&manifest.project_root))
                                .flatten()
                        })
                        .as_deref()
                        .is_some_and(|legacy| same_local_path(legacy, current))
                })
            })
            .collect()
    } else {
        exact_manifests
    };
    let mut layouts = Vec::new();
    for (manifest_path, manifest) in matching_manifests {
        let project_id = manifest
            .project_id
            .as_deref()
            .ok_or_else(|| invalid_legacy_manifest(&manifest_path, "project_id is missing"))?;
        validate_project_id(project_id)
            .map_err(|message| invalid_legacy_manifest(&manifest_path, message))?;
        if manifest.schema_version != STORE_MANIFEST_SCHEMA_VERSION
            || manifest.store_kind != StoreKind::CodeProject
            || manifest.storage_mode != StorageMode::ProfileSharded
        {
            return Err(invalid_legacy_manifest(
                &manifest_path,
                "unsupported schema, store kind, or storage mode",
            ));
        }

        let layout = profile_sharded_layout(
            project_root,
            profile_root,
            &EnrollmentMarker {
                project_id: project_id.to_string(),
                storage_mode: StorageMode::ProfileSharded,
            },
        )?;
        let manifest_data_root = manifest
            .data_root
            .canonicalize()
            .unwrap_or_else(|_| manifest.data_root.clone());
        let layout_data_root = layout
            .data_root
            .canonicalize()
            .unwrap_or_else(|_| layout.data_root.clone());
        if manifest_path.parent() != Some(manifest.data_root.as_path())
            || manifest_data_root != layout_data_root
            || manifest.data_root.join(&manifest.graph_db_relpath) != layout.graph_db_path
            || manifest.data_root.join(&manifest.sessions_db_relpath) != layout.sessions_db_path
            || manifest.data_root.join(&manifest.branch_meta_relpath) != layout.branch_meta_path
        {
            return Err(invalid_legacy_manifest(
                &manifest_path,
                "manifest paths do not match the profile shard layout",
            ));
        }
        layouts.push(layout);
    }
    Ok((layouts, selected_is_sole_exact_root))
}

pub fn retire_identity_cutover_manifest(layout: &StoreLayout) -> Result<PathBuf> {
    let source = layout
        .manifest_path
        .as_ref()
        .ok_or_else(|| TraceDecayError::Config {
            message: "profile store has no manifest path".to_string(),
        })?;
    let backup = layout
        .data_root
        .join(IDENTITY_CUTOVER_BACKUP_MANIFEST_FILENAME);
    if !source.exists() && backup.is_file() {
        return Ok(backup);
    }
    if backup.exists() {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to replace existing identity-cutover backup '{}'",
                backup.display()
            ),
        });
    }
    fs::rename(source, &backup).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to retire empty identity-cutover manifest '{}' to '{}': {error}",
            source.display(),
            backup.display()
        ),
    })?;
    Ok(backup)
}

fn same_local_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn invalid_legacy_manifest(path: &Path, detail: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "legacy profile store manifest '{}' cannot be adopted safely: {detail}",
            path.display()
        ),
    }
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
/// marker and repository identity marker via [`resolve_persisted_layout`], then
/// legacy manifest recovery — and treats an ambiguous recovery as a failure
/// rather than minting a fresh path-derived identity.
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
    if let Some(layout) = resolve_persisted_layout(project_root, profile_root)? {
        return Ok(Some(layout));
    }
    let (mut candidates, _) = matching_legacy_profile_layouts(project_root, profile_root, None)?;
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(Some(candidates.remove(0))),
        _ => {
            // Choosing between several pre-identity stores needs the repair
            // path the async resolver owns. Deriving an id from the path here
            // would answer with a shard that holds none of this project's
            // history, so report the ambiguity instead.
            let ids = candidates
                .iter()
                .filter_map(|layout| layout.identity.project_id.as_deref())
                .collect::<Vec<_>>()
                .join(", ");
            Err(TraceDecayError::Config {
                message: format!(
                    "project '{}' matches several profile stores ({ids}) and has no enrollment or \
                     repository identity marker; open it through the daemon so the registry can \
                     resolve and repair its identity",
                    project_root.display()
                ),
            })
        }
    }
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
