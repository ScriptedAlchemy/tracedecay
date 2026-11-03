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
        // An alias can exist before the canonical-root remap commits. It
        // names the project but does not prove that adoption completed.
        // Only the committed root permits this idempotent shortcut; other
        // aliases must pass the ordinary move and manifest checks below.
        if paths_record_same_root(Path::new(&existing.project.canonical_root), &new_root) {
            return storage::profile_sharded_layout(&new_root, profile_root, requested).map(Some);
        }
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tracedecay_daemon_protocol::MovedStoreAdoption;
    use tracedecay_domain::ProjectId;
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
    use tracedecay_runtime_core::storage;

    use super::adopt_moved_nongit_project;

    #[tokio::test]
    async fn named_adoption_requires_canonical_root_agreement_for_existing_alias() {
        let directory = tempfile::tempdir().expect("isolated adoption fixture");
        let root = directory.path().canonicalize().expect("canonical fixture");
        let profile = root.join("profile");
        let original = root.join("original");
        let destination = root.join("destination");
        std::fs::create_dir_all(&original).expect("original project directory");
        std::fs::create_dir_all(&destination).expect("prospective destination");
        std::fs::write(original.join("source.txt"), b"retained source")
            .expect("original source");
        let project_id = "proj_named_adoption_alias";
        let runtime = RegisteredGlobalDbTestRuntime::project(
            &profile,
            &original,
            ProjectId::new(project_id).expect("project identity"),
        )
        .await
        .expect("registered project runtime");
        let registry = runtime.profile_database();
        let layout = storage::profile_sharded_layout(&original, &profile, project_id)
            .expect("original shard layout");
        storage::write_store_manifest(&layout).expect("original manifest");
        let manifest_path = layout.data_root.join(storage::STORE_MANIFEST_FILENAME);
        let original_manifest = std::fs::read(&manifest_path).expect("manifest bytes");
        registry
            .upsert_code_project(project_id, &original, None, None, None)
            .await
            .expect("register canonical project root");

        // The real registry permits another same-project path alias without
        // replacing a live canonical root. That alias is not a completed move.
        let aliased = registry
            .upsert_code_project(project_id, &destination, None, None, None)
            .await
            .expect("register same-project alias");
        assert_eq!(Path::new(&aliased.canonical_root), original.as_path());
        let alias = registry
            .project_registry_context_by_alias(&destination)
            .await
            .expect("read alias")
            .expect("alias was committed");
        assert_eq!(alias.project.project_id, project_id);
        assert_eq!(Path::new(&alias.project.canonical_root), original.as_path());
        let adoption = MovedStoreAdoption::AdoptNamed(project_id.to_owned());
        let error = adopt_moved_nongit_project(&destination, &profile, registry, &adoption)
            .await
            .expect_err("a same-project alias cannot bypass a surviving original root");
        assert!(error.to_string().contains("has not moved"), "{error}");
        assert_eq!(std::fs::read(&manifest_path).unwrap(), original_manifest);
        assert_eq!(
            std::fs::read(original.join("source.txt")).unwrap(),
            b"retained source"
        );
        let unchanged = registry
            .get_code_project(project_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(Path::new(&unchanged.canonical_root), original.as_path());

        // Once the directory really moves, the existing alias must not prevent
        // the normal manifest-first remap, nor falsely skip its registry write.
        std::fs::remove_dir(&destination).expect("remove empty prospective destination");
        std::fs::rename(&original, &destination).expect("move original directory");
        let adopted = adopt_moved_nongit_project(&destination, &profile, registry, &adoption)
            .await
            .expect("adopt genuinely moved project")
            .expect("named adoption returns its store");
        assert_eq!(adopted.data_root, layout.data_root);
        let remapped = registry
            .get_code_project(project_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(Path::new(&remapped.canonical_root), destination.as_path());
        let manifest = storage::read_store_manifest(&manifest_path).unwrap();
        assert_eq!(manifest.project_root, destination);
        assert_eq!(
            std::fs::read(destination.join("source.txt")).unwrap(),
            b"retained source"
        );
        let replayed = adopt_moved_nongit_project(&destination, &profile, registry, &adoption)
            .await
            .expect("replay completed adoption")
            .expect("completed adoption returns the same store");
        assert_eq!(replayed.data_root, layout.data_root);
    }
}
