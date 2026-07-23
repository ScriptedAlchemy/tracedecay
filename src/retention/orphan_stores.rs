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
//! and is never collected here — it is surfaced for the reconciliation path
//! (`doctor::registry_drift`) to re-link. Only stores with no live root at all
//! are eligible for collection, and only once older than the retention window.

use std::path::{Path, PathBuf};

use crate::global_db::GlobalDb;

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
    /// On-disk store data directory (profile_root joined with the store relpath).
    pub data_root: PathBuf,
    /// `project_root` recorded in the store manifest, when the manifest was read.
    pub manifest_root: Option<PathBuf>,
    /// Newest payload mtime under `data_root`, unix seconds. Drives the age.
    pub last_write_secs: i64,
    /// Total bytes on disk under `data_root`.
    pub size_bytes: u64,
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
    if let Some(manifest_root) = entry.manifest_root.as_deref() {
        if manifest_root != entry.canonical_root
            && entry.display_root.as_deref() != Some(manifest_root)
            && root_is_live(manifest_root)
        {
            return StoreDisposition::Relinkable {
                live_root: manifest_root.to_path_buf(),
            };
        }
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
    /// Re-linkable (moved repository) — never collected here, surfaced for
    /// reconciliation so the identity is re-linked rather than orphaned.
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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CollectionOutcome {
    pub collected: Vec<CollectedStore>,
    pub reclaimed_bytes: u64,
    pub errors: Vec<String>,
}

/// Delete the on-disk data directories for every store in `plan.collect`.
/// Re-linkable and immature stores are left untouched. A directory that is
/// already gone counts as collected (idempotent). Best-effort: a failed
/// removal is recorded in `errors` and does not abort the rest.
pub fn execute_collection(plan: &CollectionPlan) -> CollectionOutcome {
    let mut outcome = CollectionOutcome::default();
    for finding in &plan.collect {
        match std::fs::remove_dir_all(&finding.data_root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                outcome.errors.push(format!(
                    "failed to collect orphan store '{}' at '{}': {error}",
                    finding.store_id,
                    finding.data_root.display()
                ));
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
fn dir_size_bytes(dir: &Path) -> u64 {
    fn walk(path: &Path, acc: &mut u64) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                walk(&entry.path(), acc);
            } else if meta.is_file() {
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
pub async fn build_store_census(db: &GlobalDb, profile_root: &Path) -> Vec<StoreCensusEntry> {
    let mut census = Vec::new();
    for project in db.list_code_projects(usize::MAX).await {
        let Some(context) = db.project_registry_context_by_id(&project.project_id).await else {
            continue;
        };
        for store_context in context.stores {
            let store = store_context.store;
            if store.storage_mode != "profile_sharded" {
                continue;
            }
            let data_root = profile_root.join(&store.store_relpath);
            if !data_root.exists() {
                continue;
            }
            let manifest_root = crate::storage::read_store_manifest(
                &data_root.join(crate::storage::STORE_MANIFEST_FILENAME),
            )
            .ok()
            .map(|manifest| manifest.project_root);
            let last_write_secs = store
                .last_write_at
                .filter(|value| *value > 0)
                .unwrap_or_else(|| newest_mtime_secs(&data_root));
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
            });
        }
    }
    census
}

/// The report returned by a sweep: the full classified plan plus, when
/// applied, what was collected on disk and the registry rows retired.
#[derive(Debug, Clone, Default)]
pub struct OrphanSweepReport {
    pub plan: CollectionPlan,
    pub applied: bool,
    pub outcome: CollectionOutcome,
    /// Registry rows removed for collected stores.
    pub retired_registry_rows: usize,
}

/// Typed daemon/doctor entry point: census → classify → plan → optionally
/// collect. When `apply` is set, orphan store directories older than
/// `retention_secs` are deleted and their now-dangling registry rows retired in
/// the same operation, so an identity migration never leaves a silent orphan.
///
/// This is deliberately not wired to a scheduler here; the caller (daemon
/// backstop tick or Doctor pass) owns cadence and mutation authority.
pub async fn sweep_orphan_stores(
    db: &GlobalDb,
    profile_root: &Path,
    retention_secs: i64,
    now: i64,
    apply: bool,
) -> OrphanSweepReport {
    let census = build_store_census(db, profile_root).await;
    let findings = classify_stores(&census, now);
    let plan = plan_collection(findings, retention_secs);

    if !apply {
        return OrphanSweepReport {
            plan,
            applied: false,
            outcome: CollectionOutcome::default(),
            retired_registry_rows: 0,
        };
    }

    let outcome = execute_collection(&plan);
    let collected_roots = plan
        .collect
        .iter()
        .filter(|finding| {
            outcome
                .collected
                .iter()
                .any(|c| c.store_id == finding.store_id)
        })
        .map(|finding| {
            // Retire the identity by its canonical registry key. Only stores we
            // actually removed from disk are retired, so a failed deletion
            // keeps its row and stays a finding.
            census
                .iter()
                .find(|entry| entry.store_id == finding.store_id)
                .map(|entry| entry.canonical_root.clone())
        })
        .collect::<Vec<_>>();
    let collected_roots = collected_roots.into_iter().flatten().collect::<Vec<_>>();
    let retired_registry_rows = if collected_roots.is_empty() {
        0
    } else {
        db.delete_project_paths(&collected_roots).await
    };

    OrphanSweepReport {
        plan,
        applied: true,
        outcome,
        retired_registry_rows,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
