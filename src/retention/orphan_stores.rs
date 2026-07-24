//! Store-level orphan detection and collection (plan 38, §2).
//!
//! The parent module prunes append-only *rows* inside a live store. This
//! submodule operates one level up: whole profile-sharded store directories
//! whose project identity no longer resolves to a live repository root.
//!
//! A project-root migration re-registers a repository under a new identity and
//! silently strands the prior store on disk. `migrate registry-gc` removes the
//! stale *registry row* but never the on-disk store *data*, so the payload
//! accumulates invisibly (measured at ~41 GB in one dogfood profile). This
//! module makes those stores a typed finding — carrying age and size — and
//! collects them under an owner-visible retention window.
//!
//! The contract is "re-link or explicitly retire, never orphan silently": a
//! store whose registry roots are gone but whose manifest points at a
//! *different, currently-live* root is classified [`StoreDisposition::Relinkable`]
//! and is never collected here — an applied sweep atomically transfers its
//! registry identity to that exact live project. Only stores with no live root
//! at all are eligible for collection, and only once older than the retention
//! window.

use std::path::{Path, PathBuf};

use crate::global_db::RegisteredGlobalDb;

/// One profile-sharded store observed on disk, paired with the registry
/// identity that points at it. This is the pure input to classification so the
/// decision logic is testable without a filesystem or database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreCensusEntry {
    pub project_id: String,
    pub store_id: String,
    /// Registry canonical root for the store's identity.
    pub canonical_root: PathBuf,
    /// Registry display root, when distinct from the canonical root.
    pub display_root: Option<PathBuf>,
    /// On-disk store data directory (`profile_root` joined with the store relpath).
    pub data_root: PathBuf,
    /// `project_root` recorded in the store manifest, when the manifest was read.
    pub manifest_root: Option<PathBuf>,
    /// Newest payload mtime under `data_root`, unix seconds. Drives the age.
    pub last_write_secs: i64,
    /// Total bytes on disk under `data_root`.
    pub size_bytes: u64,
    /// Exact registry identity observed with this filesystem census.
    pub expected_store_relpath: String,
    pub expected_created_at: i64,
    pub expected_last_write_at: Option<i64>,
    /// Payload mtime and manifest bytes fence collection against revival.
    pub expected_payload_mtime_secs: i64,
    pub expected_manifest_bytes: Option<Vec<u8>>,
}

/// What should happen to a store, decided purely from its census entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreDisposition {
    /// The identity still resolves to a live repository root. Keep.
    Live,
    /// The registry roots are gone but the manifest points at a different,
    /// currently-live root: the repository moved. Re-link, never collect.
    Relinkable { live_root: PathBuf },
    /// No live repository root resolves to this identity. Eligible for
    /// collection once older than the retention window.
    Orphaned,
}

/// A typed finding over one store: its disposition plus the age and size an
/// owner surface (Doctor) reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanStoreFinding {
    pub project_id: String,
    pub store_id: String,
    pub data_root: PathBuf,
    pub disposition: StoreDisposition,
    /// `now - last_write_secs`, clamped at zero.
    pub age_secs: i64,
    pub size_bytes: u64,
    pub expected_store_relpath: String,
    pub expected_created_at: i64,
    pub expected_last_write_at: Option<i64>,
    pub expected_payload_mtime_secs: i64,
    pub expected_manifest_bytes: Option<Vec<u8>>,
}

impl OrphanStoreFinding {
    pub fn is_orphaned(&self) -> bool {
        matches!(self.disposition, StoreDisposition::Orphaned)
    }

    pub fn is_relinkable(&self) -> bool {
        matches!(self.disposition, StoreDisposition::Relinkable { .. })
    }
}

/// A store directory is treated as a live repository root when the path exists.
/// The registry keys off repository working-tree roots, so existence is the
/// same liveness test `migrate::registry::code_project_root_exists` applies.
fn root_is_live(root: &Path) -> bool {
    root.exists()
}

fn classify_one(entry: &StoreCensusEntry) -> StoreDisposition {
    if root_is_live(&entry.canonical_root)
        || entry.display_root.as_deref().is_some_and(root_is_live)
    {
        return StoreDisposition::Live;
    }
    // Registry identity is dead. If the manifest still names a live root the
    // repository moved rather than vanished — re-link instead of collecting.
    if let Some(manifest_root) = entry.manifest_root.as_deref()
        && manifest_root != entry.canonical_root
        && entry.display_root.as_deref() != Some(manifest_root)
        && root_is_live(manifest_root)
    {
        return StoreDisposition::Relinkable {
            live_root: manifest_root.to_path_buf(),
        };
    }
    StoreDisposition::Orphaned
}

/// Classify every census entry. Pure: no filesystem writes, no deletion.
pub fn classify_stores(census: &[StoreCensusEntry], now: i64) -> Vec<OrphanStoreFinding> {
    census
        .iter()
        .map(|entry| OrphanStoreFinding {
            project_id: entry.project_id.clone(),
            store_id: entry.store_id.clone(),
            data_root: entry.data_root.clone(),
            disposition: classify_one(entry),
            age_secs: now.saturating_sub(entry.last_write_secs).max(0),
            size_bytes: entry.size_bytes,
            expected_store_relpath: entry.expected_store_relpath.clone(),
            expected_created_at: entry.expected_created_at,
            expected_last_write_at: entry.expected_last_write_at,
            expected_payload_mtime_secs: entry.expected_payload_mtime_secs,
            expected_manifest_bytes: entry.expected_manifest_bytes.clone(),
        })
        .collect()
}

/// The partitioned collection decision over a set of findings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CollectionPlan {
    /// Orphaned and older than the retention window — collect these.
    pub collect: Vec<OrphanStoreFinding>,
    /// Orphaned but still inside the retention window — kept for now, surfaced.
    pub retained_immature: Vec<OrphanStoreFinding>,
    /// Re-linkable (moved repository) — never collected; an applied sweep
    /// transfers these to the exact registered live project identity.
    pub relink: Vec<OrphanStoreFinding>,
}

impl CollectionPlan {
    /// Total bytes that collecting [`Self::collect`] would reclaim.
    pub fn collectable_bytes(&self) -> u64 {
        self.collect
            .iter()
            .fold(0u64, |acc, f| acc.saturating_add(f.size_bytes))
    }
}

/// Partition findings under a retention window. Live stores are dropped from
/// the plan entirely — they are never a retention concern. Pure.
pub fn plan_collection(findings: Vec<OrphanStoreFinding>, retention_secs: i64) -> CollectionPlan {
    let mut plan = CollectionPlan::default();
    for finding in findings {
        match &finding.disposition {
            StoreDisposition::Live => {}
            StoreDisposition::Relinkable { .. } => plan.relink.push(finding),
            StoreDisposition::Orphaned => {
                if finding.age_secs >= retention_secs {
                    plan.collect.push(finding);
                } else {
                    plan.retained_immature.push(finding);
                }
            }
        }
    }
    plan
}

/// A store directory that was deleted from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedStore {
    pub project_id: String,
    pub store_id: String,
    pub data_root: PathBuf,
    pub size_bytes: u64,
}

/// Outcome of executing a [`CollectionPlan`] against the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionFailureKind {
    OutsideProfile,
    InspectFailed,
    RemoveFailed,
    RegistryChanged,
    ManifestChanged,
    PayloadChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionFailure {
    pub store_id: String,
    pub kind: CollectionFailureKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CollectionOutcome {
    pub collected: Vec<CollectedStore>,
    pub reclaimed_bytes: u64,
    pub errors: Vec<CollectionFailure>,
}

/// Delete the on-disk data directories for every store in `plan.collect`.
/// Re-linkable and immature stores are left untouched. A directory that is
/// already gone counts as collected (idempotent). Best-effort: a failed
/// removal is recorded in `errors` and does not abort the rest.
#[cfg(test)]
pub fn execute_collection(plan: &CollectionPlan, profile_root: &Path) -> CollectionOutcome {
    let mut outcome = CollectionOutcome::default();
    let canonical_profile = match profile_root.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            outcome
                .errors
                .extend(plan.collect.iter().map(|finding| CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind: CollectionFailureKind::InspectFailed,
                }));
            return outcome;
        }
    };
    for finding in &plan.collect {
        let canonical_target = match finding.data_root.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let is_profile_child = finding
                    .data_root
                    .parent()
                    .and_then(|parent| parent.canonicalize().ok())
                    .is_some_and(|parent| {
                        parent.starts_with(&canonical_profile) && parent != canonical_profile
                    });
                if !is_profile_child {
                    outcome.errors.push(CollectionFailure {
                        store_id: finding.store_id.clone(),
                        kind: CollectionFailureKind::OutsideProfile,
                    });
                    continue;
                }
                outcome.reclaimed_bytes =
                    outcome.reclaimed_bytes.saturating_add(finding.size_bytes);
                outcome.collected.push(CollectedStore {
                    project_id: finding.project_id.clone(),
                    store_id: finding.store_id.clone(),
                    data_root: finding.data_root.clone(),
                    size_bytes: finding.size_bytes,
                });
                continue;
            }
            Err(_) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind: CollectionFailureKind::InspectFailed,
                });
                continue;
            }
        };
        if canonical_target == canonical_profile
            || !canonical_target.starts_with(&canonical_profile)
        {
            outcome.errors.push(CollectionFailure {
                store_id: finding.store_id.clone(),
                kind: CollectionFailureKind::OutsideProfile,
            });
            continue;
        }
        match std::fs::remove_dir_all(&finding.data_root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind: CollectionFailureKind::RemoveFailed,
                });
                continue;
            }
        }
        outcome.reclaimed_bytes = outcome.reclaimed_bytes.saturating_add(finding.size_bytes);
        outcome.collected.push(CollectedStore {
            project_id: finding.project_id.clone(),
            store_id: finding.store_id.clone(),
            data_root: finding.data_root.clone(),
            size_bytes: finding.size_bytes,
        });
    }
    outcome
}

pub(crate) fn store_finding_is_profile_contained(
    finding: &OrphanStoreFinding,
    profile_root: &Path,
) -> bool {
    let relpath = Path::new(&finding.expected_store_relpath);
    if relpath.as_os_str().is_empty()
        || relpath
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || profile_root.join(relpath) != finding.data_root
    {
        return false;
    }
    let Ok(canonical_profile) = profile_root.canonicalize() else {
        return false;
    };
    let target_exists = finding.data_root.exists();
    let mut existing = finding.data_root.as_path();
    while !existing.exists() {
        let Some(parent) = existing.parent() else {
            return false;
        };
        existing = parent;
    }
    existing.canonicalize().is_ok_and(|path| {
        path.starts_with(&canonical_profile) && (!target_exists || path != canonical_profile)
    })
}

/// Executes collection while holding the registered writer transaction from
/// the final registry/manifest/payload recheck through retirement. This closes
/// the revival window between the census and filesystem deletion.
pub(crate) async fn execute_registered_collection(
    db: &RegisteredGlobalDb,
    plan: &CollectionPlan,
    profile_root: &Path,
) -> crate::errors::Result<(CollectionOutcome, usize)> {
    let mut outcome = CollectionOutcome::default();
    let mut retired = 0usize;
    for finding in &plan.collect {
        if !store_finding_is_profile_contained(finding, profile_root) {
            outcome.errors.push(CollectionFailure {
                store_id: finding.store_id.clone(),
                kind: CollectionFailureKind::OutsideProfile,
            });
            continue;
        }

        let transaction = db.begin_write_transaction().await?;
        let mut rows = transaction
            .query(
                "SELECT store_relpath, created_at, last_write_at
                 FROM store_instances
                 WHERE project_id = ?1 AND store_id = ?2",
                crate::db::engine::params![finding.project_id.as_str(), finding.store_id.as_str()],
            )
            .await
            .map_err(|error| orphan_db_error("recheck orphan store identity", error))?;
        let current = match rows
            .next()
            .await
            .map_err(|error| orphan_db_error("read orphan store identity", error))?
        {
            Some(row) => Some((
                row.get::<String>(0)
                    .map_err(|error| orphan_db_error("decode orphan store relpath", error))?,
                row.get::<i64>(1)
                    .map_err(|error| orphan_db_error("decode orphan store generation", error))?,
                row.get::<Option<i64>>(2)
                    .map_err(|error| orphan_db_error("decode orphan last write", error))?,
            )),
            None => None,
        };
        drop(rows);
        if current
            != Some((
                finding.expected_store_relpath.clone(),
                finding.expected_created_at,
                finding.expected_last_write_at,
            ))
        {
            transaction
                .rollback()
                .await
                .map_err(|error| orphan_db_error("rollback changed orphan store", error))?;
            outcome.errors.push(CollectionFailure {
                store_id: finding.store_id.clone(),
                kind: CollectionFailureKind::RegistryChanged,
            });
            continue;
        }

        let manifest_path = finding
            .data_root
            .join(crate::storage::STORE_MANIFEST_FILENAME);
        let current_manifest = match std::fs::read(&manifest_path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => {
                transaction.rollback().await.map_err(|error| {
                    orphan_db_error("rollback unreadable orphan manifest", error)
                })?;
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind: CollectionFailureKind::InspectFailed,
                });
                continue;
            }
        };
        if current_manifest != finding.expected_manifest_bytes {
            transaction
                .rollback()
                .await
                .map_err(|error| orphan_db_error("rollback changed orphan manifest", error))?;
            outcome.errors.push(CollectionFailure {
                store_id: finding.store_id.clone(),
                kind: CollectionFailureKind::ManifestChanged,
            });
            continue;
        }
        if finding.data_root.exists()
            && newest_mtime_secs(&finding.data_root) != finding.expected_payload_mtime_secs
        {
            transaction
                .rollback()
                .await
                .map_err(|error| orphan_db_error("rollback revived orphan payload", error))?;
            outcome.errors.push(CollectionFailure {
                store_id: finding.store_id.clone(),
                kind: CollectionFailureKind::PayloadChanged,
            });
            continue;
        }

        match std::fs::remove_dir_all(&finding.data_root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|error| orphan_db_error("rollback failed orphan removal", error))?;
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind: CollectionFailureKind::RemoveFailed,
                });
                continue;
            }
        }
        let deleted = transaction
            .execute(
                "DELETE FROM store_instances
                 WHERE project_id = ?1 AND store_id = ?2
                   AND store_relpath = ?3 AND created_at = ?4
                   AND last_write_at IS ?5",
                crate::db::engine::params![
                    finding.project_id.as_str(),
                    finding.store_id.as_str(),
                    finding.expected_store_relpath.as_str(),
                    finding.expected_created_at,
                    finding.expected_last_write_at
                ],
            )
            .await
            .map_err(|error| orphan_db_error("retire collected orphan store", error))?;
        if deleted != 1 {
            transaction
                .rollback()
                .await
                .map_err(|error| orphan_db_error("rollback raced orphan retirement", error))?;
            outcome.errors.push(CollectionFailure {
                store_id: finding.store_id.clone(),
                kind: CollectionFailureKind::RegistryChanged,
            });
            continue;
        }
        transaction
            .execute(
                "DELETE FROM code_projects
                 WHERE project_id = ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM store_instances WHERE project_id = ?1
                   )",
                crate::db::engine::params![finding.project_id.as_str()],
            )
            .await
            .map_err(|error| orphan_db_error("retire empty collected project", error))?;
        transaction
            .commit()
            .await
            .map_err(|error| orphan_db_error("commit collected orphan retirement", error))?;

        retired = retired.saturating_add(1);
        outcome.reclaimed_bytes = outcome.reclaimed_bytes.saturating_add(finding.size_bytes);
        outcome.collected.push(CollectedStore {
            project_id: finding.project_id.clone(),
            store_id: finding.store_id.clone(),
            data_root: finding.data_root.clone(),
            size_bytes: finding.size_bytes,
        });
    }
    Ok((outcome, retired))
}

fn orphan_db_error(
    operation: &'static str,
    error: impl std::fmt::Display,
) -> crate::errors::TraceDecayError {
    crate::errors::TraceDecayError::Database {
        operation: operation.to_string(),
        message: error.to_string(),
    }
}

/// Newest mtime under `dir`, unix seconds, or `0` when nothing is readable.
fn newest_mtime_secs(dir: &Path) -> i64 {
    fn walk(path: &Path, newest: &mut i64) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if let Ok(modified) = meta.modified()
                && let Ok(elapsed) = modified.duration_since(std::time::UNIX_EPOCH)
            {
                *newest = (*newest).max(elapsed.as_secs() as i64);
            }
            if meta.is_dir() {
                walk(&entry.path(), newest);
            }
        }
    }
    let mut newest = 0i64;
    walk(dir, &mut newest);
    newest
}

/// Total size in bytes of every file under `dir`. Best-effort: unreadable
/// entries are skipped. Kept local to the lib because the binary-only
/// `global::tracedecay_dir_size` is not reachable from this crate module.
///
/// Symlinks are never followed. `DirEntry::metadata` follows them, so a
/// symlink pointing at an ancestor would recurse until the stack ran out, and
/// one pointing outside the store would bill another directory's bytes to
/// this one. `file_type` reports the link itself, so the walk stays inside
/// the directory it was given.
pub(crate) fn dir_size_bytes(dir: &Path) -> u64 {
    fn walk(path: &Path, acc: &mut u64) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                walk(&entry.path(), acc);
            } else if file_type.is_file()
                && let Ok(meta) = entry.metadata()
            {
                *acc = acc.saturating_add(meta.len());
            }
        }
    }
    let mut total = 0u64;
    walk(dir, &mut total);
    total
}

/// Build the on-disk store census from the registry. Reads manifests and sizes
/// directories but never mutates. Only profile-sharded stores are considered;
/// other storage modes are not laid out under the profile root here.
pub async fn build_store_census(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
) -> crate::errors::Result<Vec<StoreCensusEntry>> {
    let mut census = Vec::new();
    for project in db.list_code_projects(usize::MAX).await? {
        for store in db
            .try_list_store_instances_for_project(&project.project_id)
            .await?
        {
            if store.storage_mode != "profile_sharded" {
                continue;
            }
            let data_root = profile_root.join(&store.store_relpath);
            let manifest_path = data_root.join(crate::storage::STORE_MANIFEST_FILENAME);
            let expected_manifest_bytes = match std::fs::read(&manifest_path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(crate::errors::TraceDecayError::Config {
                        message: format!(
                            "failed to snapshot store manifest '{}': {error}",
                            manifest_path.display()
                        ),
                    });
                }
            };
            let manifest_root = expected_manifest_bytes
                .as_deref()
                .and_then(|bytes| {
                    serde_json::from_slice::<crate::storage::StoreManifest>(bytes).ok()
                })
                .map(|manifest| manifest.project_root);
            let expected_payload_mtime_secs = newest_mtime_secs(&data_root);
            let last_write_secs = store
                .last_write_at
                .filter(|value| *value > 0)
                .unwrap_or(expected_payload_mtime_secs);
            let size_bytes = dir_size_bytes(&data_root);
            census.push(StoreCensusEntry {
                project_id: project.project_id.clone(),
                store_id: store.store_id.clone(),
                canonical_root: PathBuf::from(&project.canonical_root),
                display_root: (project.display_root != project.canonical_root)
                    .then(|| PathBuf::from(&project.display_root)),
                data_root,
                manifest_root,
                last_write_secs,
                size_bytes,
                expected_store_relpath: store.store_relpath,
                expected_created_at: store.created_at,
                expected_last_write_at: store.last_write_at,
                expected_payload_mtime_secs,
                expected_manifest_bytes,
            });
        }
    }
    Ok(census)
}

/// The report returned by a sweep: the full classified plan plus, when
/// applied, what was collected on disk and the registry rows retired.
#[derive(Debug, Clone, Default)]
pub struct OrphanSweepReport {
    pub plan: CollectionPlan,
    pub applied: bool,
    pub outcome: CollectionOutcome,
    /// Registry identities transferred to their exact currently-live project.
    pub relinked_registry_rows: usize,
    /// Registry rows removed for collected stores.
    pub retired_registry_rows: usize,
}

/// Typed daemon/doctor entry point: census → classify → plan → optionally
/// collect. When `apply` is set, orphan store directories older than
/// `retention_secs` are deleted and their now-dangling registry rows retired in
/// the same operation, so an identity migration never leaves a silent orphan.
///
/// The caller (daemon backstop tick or Doctor pass) owns cadence and mutation
/// authority.
pub async fn sweep_orphan_stores(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    retention_secs: i64,
    now: i64,
    apply: bool,
) -> crate::errors::Result<OrphanSweepReport> {
    let census = build_store_census(db, profile_root).await?;
    let findings = classify_stores(&census, now);
    let plan = plan_collection(findings, retention_secs);

    if !apply {
        return Ok(OrphanSweepReport {
            plan,
            applied: false,
            outcome: CollectionOutcome::default(),
            relinked_registry_rows: 0,
            retired_registry_rows: 0,
        });
    }

    let mut relinked_registry_rows = 0usize;
    let mut preflight_errors = Vec::new();
    for finding in &plan.relink {
        let StoreDisposition::Relinkable { live_root } = &finding.disposition else {
            continue;
        };
        if !store_finding_is_profile_contained(finding, profile_root) {
            preflight_errors.push(CollectionFailure {
                store_id: finding.store_id.clone(),
                kind: CollectionFailureKind::OutsideProfile,
            });
            continue;
        }
        if db
            .relink_orphan_store_instance(
                &finding.project_id,
                &finding.store_id,
                live_root,
                profile_root,
                &finding.data_root,
                &finding.expected_store_relpath,
                finding.expected_created_at,
                finding.expected_last_write_at,
                finding.expected_manifest_bytes.as_deref(),
            )
            .await?
        {
            relinked_registry_rows = relinked_registry_rows.saturating_add(1);
        }
    }

    let (mut outcome, retired_registry_rows) =
        execute_registered_collection(db, &plan, profile_root).await?;
    outcome.errors.splice(0..0, preflight_errors);

    Ok(OrphanSweepReport {
        plan,
        applied: true,
        outcome,
        relinked_registry_rows,
        retired_registry_rows,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
