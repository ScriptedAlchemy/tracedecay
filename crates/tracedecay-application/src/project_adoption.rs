//! Explicit identity-based recovery for a moved non-git project.
//!
//! Non-git projects retain their identity in the profile registry and store
//! manifest. A missing old root does not establish which new directory owns
//! that history: adoption therefore requires the exact registered project id.
//! Ordinary init never enumerates or selects other projects. Retrying the
//! same named adoption resumes an interrupted manifest-first registry remap.

use std::path::{Path, PathBuf};

use tracedecay_daemon_protocol::MovedStoreAdoption;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::storage::{self, StoreLayout, ValidatedProfileShard};

/// Remaps only the named non-git project after verifying its registry and
/// store evidence. No adoption request means no adoption reads or writes.
#[tracing::instrument(name = "lifecycle.adopt_moved_nongit", level = "trace", skip_all)]
pub async fn adopt_moved_nongit_project(
    project_root: &Path,
    profile_root: &Path,
    registry: &RegisteredGlobalDb,
    adoption: &MovedStoreAdoption,
) -> Result<Option<StoreLayout>> {
    let MovedStoreAdoption::AdoptNamed(requested) = adoption else {
        return Ok(None);
    };
    if tracedecay_runtime_core::worktree::git_common_dir(project_root).is_some() {
        return Err(TraceDecayError::Config {
            message: "--adopt-project applies only to moved non-git projects; \
                      Git repositories retain their repository identity"
                .to_owned(),
        });
    }
    let new_root = project_root
        .canonicalize()
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "could not canonicalize moved-project adoption root '{}': {error}",
                project_root.display()
            ),
        })?;

    if let Some(existing) = registry
        .project_registry_context_by_alias(&new_root)
        .await?
    {
        if &existing.project.project_id != requested {
            return Err(TraceDecayError::Config {
                message: format!(
                    "cannot adopt project '{requested}' onto root '{}' that already \
                     resolves to registered project '{}'",
                    new_root.display(),
                    existing.project.project_id,
                ),
            });
        }
        return storage::profile_sharded_layout(&new_root, profile_root, requested).map(Some);
    }

    let project =
        registry
            .get_code_project(requested)
            .await?
            .ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "project '{requested}' is not registered; use `tracedecay projects list` \
                 to select the moved non-git project"
                ),
            })?;
    if project.git_common_dir.is_some() {
        return Err(TraceDecayError::Config {
            message: format!(
                "project '{requested}' is a Git repository, not a moved non-git project"
            ),
        });
    }
    let previous_root = PathBuf::from(&project.canonical_root);
    // The registered root owns a directory, not any replacement file at
    // the old pathname. Preserve inspection errors rather than assuming a
    // directory was moved when its metadata cannot be checked.
    let previous_directory_exists = match std::fs::metadata(&previous_root) {
        Ok(metadata) => metadata.is_dir(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "cannot inspect registered root '{}' for project '{requested}': {error}",
                    previous_root.display(),
                ),
            });
        }
    };
    if previous_root == new_root || previous_directory_exists {
        return Err(TraceDecayError::Config {
            message: format!(
                "project '{requested}' has not moved from its registered root '{}'",
                previous_root.display(),
            ),
        });
    }

    // The canonical shard owner verifies private-path containment, project
    // identity, manifest shape, and the retained sessions database. A name or
    // a missing old directory cannot authorize a foreign store.
    let layout = storage::profile_sharded_layout(&new_root, profile_root, requested)?;
    let shard = ValidatedProfileShard::resolve_existing(profile_root, requested).map_err(
        |error| TraceDecayError::Config {
            message: format!(
                "cannot evaluate moved-store adoption evidence for project '{requested}': {error:?}"
            ),
        },
    )?;
    let manifest =
        storage::read_store_manifest(&shard.store_root().join(storage::STORE_MANIFEST_FILENAME))?;
    if !paths_record_same_root(&manifest.project_root, &previous_root)
        && !paths_record_same_root(&manifest.project_root, &new_root)
    {
        return Err(TraceDecayError::Config {
            message: format!(
                "project '{requested}' has no moved-store evidence linking registered root '{}' \
                 or adoption destination '{}' to its store",
                previous_root.display(),
                new_root.display(),
            ),
        });
    }

    // The manifest is the existing recovery record. Publish the destination
    // there first, then commit the registry. A retry of this exact named
    // adoption accepts the destination recorded by an interrupted attempt.
    storage::write_store_manifest(&layout)?;
    let registered = registry
        .upsert_code_project(requested, &new_root, None, None, None)
        .await?;
    if registered.project_id != *requested
        || !paths_record_same_root(Path::new(&registered.canonical_root), &new_root)
    {
        return Err(TraceDecayError::Config {
            message: format!(
                "cannot complete adoption of project '{requested}' onto root '{}': \
                 the registry retained project '{}' at '{}'; resolve the conflicting \
                 registration before retrying this named adoption",
                new_root.display(),
                registered.project_id,
                registered.canonical_root,
            ),
        });
    }
    Ok(Some(layout))
}

fn paths_record_same_root(recorded: &Path, root: &Path) -> bool {
    if recorded == root {
        return true;
    }
    match (recorded.canonicalize(), root.canonicalize()) {
        (Ok(recorded), Ok(root)) => recorded == root,
        _ => false,
    }
}
