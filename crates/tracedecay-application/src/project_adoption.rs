//! Explicit recovery for a MOVED non-git project.
//!
//! After the working-tree `.tracedecay/` enrollment file was eliminated, a
//! non-git project has no checkout-side identity marker. Its durable home is
//! the profile registry plus the profile store's recorded root. Moving the
//! directory changes the path-derived project id, so first-touch would mint
//! a new empty shard unless the operator names the registered project to
//! rebind onto the new root.
//!
//! Rebinding a registered identity onto a new root is an operator decision,
//! never a heuristic: a stale registry row carries no evidence tying it to
//! whatever directory happens to be initialized next, so a silent remap would
//! alias one project's graph and facts onto an unrelated root. Adoption
//! therefore happens only under [`MovedStoreAdoption::AdoptNamed`], which
//! reads exactly the named registry row and its store manifest and never
//! scans the profile. Resuming an interrupted remap is the same request
//! re-run: a manifest already recording the new root is positive linkage this
//! module wrote under the earlier adoption.
//!
//! Known miss (documented, not a remap hazard): on a case-insensitive
//! filesystem a case-only rename leaves the recorded previous root still
//! resolvable, so the named project is refused as not moved and an init at the
//! renamed path needs `--fresh`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use tracedecay_daemon_protocol::MovedStoreAdoption;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::storage::{self, StoreLayout};

/// Remaps the registered project `adoption` names onto `project_root`.
///
/// Returns `Ok(None)` when adoption was not requested or the root is a git
/// checkout, so first-touch may mint a new identity. A named project that is
/// not registered, is not a moved non-git store, or whose store evidence does
/// not tie it to this move is a typed refusal that mutates nothing.
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
        return Ok(None);
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
        return refuse_if_adoption_conflicts(requested, &existing.project.project_id, &new_root);
    }

    let not_adoptable = |reason: &str| TraceDecayError::Config {
        message: format!(
            "project '{requested}' cannot be adopted at '{}': {reason}",
            new_root.display()
        ),
    };
    let project = registry
        .get_code_project(requested)
        .await?
        .ok_or_else(|| not_adoptable("it is not a registered project"))?;
    if project.git_common_dir.is_some() {
        return Err(not_adoptable("it is a git project"));
    }
    let previous = PathBuf::from(&project.canonical_root);
    if previous.is_dir() {
        return Err(not_adoptable(&format!(
            "its recorded root '{}' still exists",
            previous.display()
        )));
    }
    let layout = existing_profile_store_layout(profile_root, requested)?
        .ok_or_else(|| not_adoptable("its profile store does not exist"))?;
    if !store_records_either_root(&layout, &previous, &new_root)? {
        return Err(not_adoptable(
            "its store manifest records neither its previous root nor this root",
        ));
    }

    let remapped: Pin<Box<dyn Future<Output = Result<Option<StoreLayout>>> + Send + '_>> = Box::pin(
        remap_moved_nongit_project(&new_root, profile_root, registry, requested),
    );
    remapped.await
}

fn refuse_if_adoption_conflicts(
    requested: &str,
    existing_project_id: &str,
    new_root: &Path,
) -> Result<Option<StoreLayout>> {
    if requested != existing_project_id {
        return Err(TraceDecayError::Config {
            message: format!(
                "cannot adopt project '{requested}' onto root '{}' that already \
                 resolves to registered project '{existing_project_id}'",
                new_root.display()
            ),
        });
    }
    Ok(None)
}

fn existing_profile_store_layout(
    profile_root: &Path,
    project_id: &str,
) -> Result<Option<StoreLayout>> {
    let layout = storage::profile_sharded_layout(profile_root, profile_root, project_id)?;
    let store_exists = layout.graph_db_path.is_file()
        || layout.manifest_path.as_deref().is_some_and(Path::is_file);
    Ok(store_exists.then_some(layout))
}

/// Whether the store manifest records the previous root (an unstarted move) or
/// the new root (an interrupted remap this module journaled). Unreadable
/// evidence is a typed error, never a silent non-match.
fn store_records_either_root(
    layout: &StoreLayout,
    previous_root: &Path,
    new_root: &Path,
) -> Result<bool> {
    let Some(manifest_path) = layout
        .manifest_path
        .as_deref()
        .filter(|path| path.is_file())
    else {
        return Ok(false);
    };
    let manifest =
        storage::read_store_manifest(manifest_path).map_err(|error| TraceDecayError::Config {
            message: format!(
                "cannot evaluate moved-store adoption evidence: {error}; repair or \
                 remove the manifest, or re-run `tracedecay init` with --fresh to \
                 mint a new identity without adoption"
            ),
        })?;
    Ok(paths_record_same_root(&manifest.project_root, new_root)
        || paths_record_same_root(&manifest.project_root, previous_root))
}

fn paths_record_same_root(recorded: &Path, previous_root: &Path) -> bool {
    if recorded == previous_root {
        return true;
    }
    match (recorded.canonicalize(), previous_root.canonicalize()) {
        (Ok(recorded), Ok(previous)) => recorded == previous,
        _ => false,
    }
}

/// Rebinds `project_id` onto `new_root` as a journaled sequence.
///
/// Store-side evidence (the shard manifest) is written first: a
/// manifest recording the new root is the journal record an interrupted remap
/// resumes from, because it is positive linkage between this store and the
/// root. The registry upsert commits last, it is what makes the root resolve,
/// so every intermediate state either still resolves the old registration
/// or resumes here on the next explicit init.
#[tracing::instrument(name = "lifecycle.remap_moved_nongit", level = "trace", skip_all)]
async fn remap_moved_nongit_project(
    new_root: &Path,
    profile_root: &Path,
    registry: &RegisteredGlobalDb,
    project_id: &str,
) -> Result<Option<StoreLayout>> {
    let layout = storage::profile_sharded_layout(new_root, profile_root, project_id)?;
    storage::write_store_manifest(&layout)?;
    registry
        .upsert_code_project(project_id, new_root, None, None, None)
        .await?;
    Ok(Some(layout))
}
