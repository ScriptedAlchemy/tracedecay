//! First-touch recovery for a MOVED non-git project.
//!
//! After the working-tree `.tracedecay/` enrollment file was eliminated, a
//! non-git project has no checkout-side identity marker. Its durable home is
//! the profile registry plus the profile store's recorded root. Moving the
//! directory changes the path-derived project id, so first-touch would mint
//! a new empty shard unless init remaps the existing registry row whose store
//! evidence still names the old root.
//!
//! Rebinding a registered identity onto a new root is an operator decision,
//! never a heuristic: a stale registry row carries no evidence tying it to
//! whatever directory happens to be initialized next, so a silent remap would
//! alias one project's graph and facts onto an unrelated root. Ambient
//! first-touch (agent tools, hooks) therefore always mints fresh, and even
//! explicit `tracedecay init` adopts only under [`MovedStoreAdoption::AdoptNamed`]
//! or [`MovedStoreAdoption::AdoptUnique`]. The one exception that needs no
//! flag is resuming an interrupted remap, where the store's own manifest
//! already records the new root — positive linkage this module wrote under a
//! previous explicit adoption.
//!
//! Known miss (documented, not a remap hazard): on a case-insensitive
//! filesystem a case-only rename leaves the recorded previous root still
//! resolvable, so the moved store is not discovered as a candidate and an
//! explicit init at the renamed path mints a fresh identity instead of
//! offering the old one.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::storage::{self, StoreLayout};

use super::TraceDecay;

/// How first-touch resolution treats a moved non-git store whose registry row
/// no longer resolves at its recorded root.
///
/// Old daemons ignore the handshake field carrying this value and new daemons
/// default a missing field to [`Self::Never`], so mixed-version pairs always
/// degrade to the safe no-adoption behavior.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MovedStoreAdoption {
    /// Mint a fresh identity without consulting moved-store candidates. The
    /// ambient first-touch default (agent tools, hooks) and
    /// `tracedecay init --fresh`.
    #[default]
    Never,
    /// Explicit `tracedecay init` without adoption flags: mint fresh when no
    /// moved store could claim this root; otherwise refuse with the
    /// candidates so the operator chooses `--adopt-project`, `--yes`, or
    /// `--fresh` explicitly — never a silent remap, never a silent identity
    /// split.
    OfferCandidates,
    /// `tracedecay init --yes`: adopt when candidates identify exactly one
    /// moved store; anything ambiguous remains a typed refusal.
    AdoptUnique,
    /// `tracedecay init --adopt-project <id>`: adopt exactly this project.
    AdoptNamed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MovedNongitCandidate {
    project_id: String,
    /// The shard's own evidence already records the root being initialized —
    /// an interrupted remap journal record, resumable without a flag.
    records_new_root: bool,
}

/// What a candidate's store-side evidence records, relative to the roots that
/// matter for adoption. Unreadable evidence is a typed error, never a silent
/// non-match: dropping the true candidate would make a wrong one unique.
enum MovedStoreEvidence {
    RecordsPreviousRoot,
    RecordsNewRoot,
    NoMatch,
}

impl TraceDecay {
    /// Remaps a moved non-git project onto `project_root` under an explicit
    /// operator adoption decision.
    ///
    /// Returns `Ok(None)` when adoption was not requested or there is no
    /// moved-store candidate, so first-touch may mint a new identity.
    /// Ambiguous or conflicting adoption is a typed refusal, never an alias.
    pub(crate) async fn adopt_moved_nongit_project(
        project_root: &Path,
        profile_root: &Path,
        registry: &RegisteredGlobalDb,
        adoption: &MovedStoreAdoption,
    ) -> Result<Option<StoreLayout>> {
        if matches!(adoption, MovedStoreAdoption::Never) {
            return Ok(None);
        }
        if crate::worktree::git_common_dir(project_root).is_some() {
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
            return refuse_if_adoption_conflicts(adoption, &existing.project.project_id, &new_root);
        }

        let candidates =
            discover_moved_nongit_candidates(&new_root, profile_root, registry).await?;
        let resuming = candidates
            .iter()
            .filter(|candidate| candidate.records_new_root)
            .collect::<Vec<_>>();
        let selected = match adoption {
            MovedStoreAdoption::Never => return Ok(None),
            MovedStoreAdoption::AdoptNamed(requested) => {
                match candidates
                    .iter()
                    .find(|candidate| &candidate.project_id == requested)
                {
                    Some(candidate) => candidate,
                    None => {
                        return Err(TraceDecayError::Config {
                            message: format!(
                                "project '{requested}' is not a moved non-git store \
                                 that can be adopted at '{}'",
                                new_root.display()
                            ),
                        });
                    }
                }
            }
            MovedStoreAdoption::AdoptUnique => match (resuming.as_slice(), candidates.as_slice()) {
                (_, []) => return Ok(None),
                // A store whose manifest already records this exact root is
                // positive linkage; it outranks unlinked stale rows.
                ([resumable], _) => *resumable,
                (_, [candidate]) => candidate,
                _ => {
                    return Err(TraceDecayError::Config {
                        message: format!(
                            "moved non-git project adoption at '{}' is ambiguous \
                             (candidates: {}); re-run `tracedecay init` with \
                             --adopt-project <proj_id>, or with --fresh to mint a \
                             new project identity here",
                            new_root.display(),
                            candidate_ids(&candidates)
                        ),
                    });
                }
            },
            MovedStoreAdoption::OfferCandidates => {
                match (resuming.as_slice(), candidates.as_slice()) {
                    (_, []) => return Ok(None),
                    // Resuming an interrupted remap needs no flag: the store's
                    // manifest recording this root was written under a previous
                    // explicit adoption and is the journal record to replay.
                    ([resumable], _) => *resumable,
                    _ => {
                        return Err(TraceDecayError::Config {
                            message: format!(
                                "a moved non-git store may belong at '{}' (candidates: {}); \
                             adoption rebinds a registered project identity and needs an \
                             explicit choice: re-run `tracedecay init` with \
                             --adopt-project <proj_id> (or --yes when exactly one \
                             candidate exists), or with --fresh to mint a new project \
                             identity here",
                                new_root.display(),
                                candidate_ids(&candidates)
                            ),
                        });
                    }
                }
            }
        };

        remap_moved_nongit_project(&new_root, profile_root, registry, selected).await
    }
}

fn candidate_ids(candidates: &[MovedNongitCandidate]) -> String {
    candidates
        .iter()
        .map(|candidate| candidate.project_id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn refuse_if_adoption_conflicts(
    adoption: &MovedStoreAdoption,
    existing_project_id: &str,
    new_root: &Path,
) -> Result<Option<StoreLayout>> {
    if let MovedStoreAdoption::AdoptNamed(requested) = adoption
        && requested != existing_project_id
    {
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

async fn discover_moved_nongit_candidates(
    new_root: &Path,
    profile_root: &Path,
    registry: &RegisteredGlobalDb,
) -> Result<Vec<MovedNongitCandidate>> {
    let projects = registry.list_code_projects(usize::MAX).await?;
    let mut candidates = Vec::new();
    for project in projects {
        if project.git_common_dir.is_some() {
            continue;
        }
        let previous = PathBuf::from(&project.canonical_root);
        if previous == new_root || previous.is_dir() {
            continue;
        }
        let Some(layout) = existing_profile_store_layout(profile_root, &project.project_id)? else {
            continue;
        };
        let records_new_root = match moved_store_evidence(&layout, &previous, new_root)? {
            MovedStoreEvidence::RecordsPreviousRoot => false,
            MovedStoreEvidence::RecordsNewRoot => true,
            MovedStoreEvidence::NoMatch => continue,
        };
        candidates.push(MovedNongitCandidate {
            project_id: project.project_id,
            records_new_root,
        });
    }
    candidates.sort_by(|left, right| left.project_id.cmp(&right.project_id));
    Ok(candidates)
}

fn existing_profile_store_layout(
    profile_root: &Path,
    project_id: &str,
) -> Result<Option<StoreLayout>> {
    let layout = storage::profile_sharded_layout(
        profile_root,
        profile_root,
        &storage::EnrollmentMarker {
            project_id: project_id.to_owned(),
            storage_mode: storage::StorageMode::ProfileSharded,
        },
    )?;
    let store_exists = layout.graph_db_path.is_file()
        || layout.manifest_path.as_deref().is_some_and(Path::is_file);
    Ok(store_exists.then_some(layout))
}

fn moved_store_evidence(
    layout: &StoreLayout,
    previous_root: &Path,
    new_root: &Path,
) -> Result<MovedStoreEvidence> {
    if let Some(manifest_path) = layout.manifest_path.as_deref()
        && manifest_path.is_file()
    {
        let manifest = storage::read_store_manifest(manifest_path).map_err(|error| {
            TraceDecayError::Config {
                message: format!(
                    "cannot evaluate moved-store adoption evidence: {error}; repair or \
                     remove the manifest, or re-run `tracedecay init` with --fresh to \
                     mint a new identity without adoption"
                ),
            }
        })?;
        if paths_record_same_root(&manifest.project_root, new_root) {
            return Ok(MovedStoreEvidence::RecordsNewRoot);
        }
        if paths_record_same_root(&manifest.project_root, previous_root) {
            return Ok(MovedStoreEvidence::RecordsPreviousRoot);
        }
    }
    if layout.config_path.is_file() {
        let config = crate::config::load_config_from_path(previous_root, &layout.config_path)
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "cannot evaluate moved-store adoption evidence from '{}': {error}; \
                     repair or remove the store config, or re-run `tracedecay init` \
                     with --fresh to mint a new identity without adoption",
                    layout.config_path.display()
                ),
            })?;
        let recorded = PathBuf::from(&config.root_dir);
        if paths_record_same_root(&recorded, new_root) {
            return Ok(MovedStoreEvidence::RecordsNewRoot);
        }
        if paths_record_same_root(&recorded, previous_root) {
            return Ok(MovedStoreEvidence::RecordsPreviousRoot);
        }
    }
    Ok(MovedStoreEvidence::NoMatch)
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

/// Rebinds `candidate` onto `new_root` as a journaled sequence.
///
/// Store-side evidence (shard manifest, then config) is written first: a
/// manifest recording the new root is the journal record an interrupted remap
/// resumes from, because it is positive linkage between this store and the
/// root. The registry upsert commits last — it is what makes the root resolve
/// — so every intermediate state either still resolves the old registration
/// or resumes here on the next explicit init.
async fn remap_moved_nongit_project(
    new_root: &Path,
    profile_root: &Path,
    registry: &RegisteredGlobalDb,
    candidate: &MovedNongitCandidate,
) -> Result<Option<StoreLayout>> {
    let layout = storage::profile_sharded_layout(
        new_root,
        profile_root,
        &storage::EnrollmentMarker {
            project_id: candidate.project_id.clone(),
            storage_mode: storage::StorageMode::ProfileSharded,
        },
    )?;
    storage::write_store_manifest(&layout)?;
    if layout.config_path.is_file() {
        let root_dir = new_root
            .to_str()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "moved-project root '{}' is not valid UTF-8 and cannot be recorded \
                     in the store config",
                    new_root.display()
                ),
            })?
            .to_owned();
        let mut config = crate::config::load_config_from_path(new_root, &layout.config_path)?;
        config.root_dir = root_dir;
        crate::config::save_config_to_path(&layout.config_path, &config)?;
    }
    registry
        .upsert_code_project(&candidate.project_id, new_root, None, None, None)
        .await?;
    Ok(Some(layout))
}
