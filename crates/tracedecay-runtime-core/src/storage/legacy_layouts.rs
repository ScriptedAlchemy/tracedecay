use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::git_discovery::{
    GitDiscoveryUnknown, GitRepositoryIdentityOutcome, discover_repository_identity_cli_first,
};
use crate::worktree;
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::{
    EnrollmentMarker, STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode,
    StoreKind, StoreLayout, profile_sharded_layout, read_store_manifest, validate_project_id,
};

/// Finds pre-repository-identity profile stores that were keyed by an older
/// path-derived project id but still name this exact local checkout, or one of
/// its linked worktrees, in their manifest. Remote URLs are deliberately not
/// considered: two clones of one remote are different local identities.
pub fn matching_legacy_profile_layouts(
    project_root: &Path,
    profile_root: &Path,
    excluded_project_id: Option<&str>,
) -> Result<(Vec<StoreLayout>, bool, bool)> {
    matching_legacy_profile_layouts_with_git_identity_resolver(
        project_root,
        profile_root,
        excluded_project_id,
        worktree::is_detached_linked_worktree,
        discover_repository_identity_cli_first,
    )
}

fn matching_legacy_profile_layouts_with_git_identity_resolver<D, G>(
    project_root: &Path,
    profile_root: &Path,
    excluded_project_id: Option<&str>,
    mut is_detached_linked_worktree: D,
    mut git_identity: G,
) -> Result<(Vec<StoreLayout>, bool, bool)>
where
    D: FnMut(&Path) -> bool,
    G: FnMut(&Path) -> GitRepositoryIdentityOutcome,
{
    let projects_root = profile_root.join("projects");
    let Ok(entries) = fs::read_dir(&projects_root) else {
        return Ok((Vec::new(), false, false));
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

    let candidates_match_exact_root = !exact_manifests.is_empty();
    let matching_manifests = if exact_manifests.is_empty() {
        let project_git_common_dir = if is_detached_linked_worktree(project_root) {
            None
        } else {
            match git_identity(project_root) {
                GitRepositoryIdentityOutcome::Resolved(identity) => Some(identity.common_dir),
                GitRepositoryIdentityOutcome::NotRepository => None,
                GitRepositoryIdentityOutcome::Unknown(reason) => {
                    return Err(unknown_git_identity(project_root, reason));
                }
            }
        };
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
                                .then(|| match git_identity(&manifest.project_root) {
                                    GitRepositoryIdentityOutcome::Resolved(identity) => {
                                        Some(identity.common_dir)
                                    }
                                    GitRepositoryIdentityOutcome::NotRepository => None,
                                    // Skip an unreadable sibling rather than
                                    // adopting it. The current checkout's
                                    // Unknown already failed closed above.
                                    GitRepositoryIdentityOutcome::Unknown(_) => None,
                                })
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
    Ok((
        layouts,
        selected_manifest_matches_exact_root,
        candidates_match_exact_root,
    ))
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

fn unknown_git_identity(path: &Path, reason: GitDiscoveryUnknown) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "cannot adopt a legacy profile store for '{}': git repository identity is unknown ({reason})",
            path.display()
        ),
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
