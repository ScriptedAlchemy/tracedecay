//! Store-level orphan detection and collection (plan 38, §2).
//!
//! The parent module prunes append-only *rows* inside a live store. This
//! submodule operates one level up: whole profile-sharded store directories
//! whose project identity no longer resolves to a live repository root.
//!
//! A project-root migration re-registers a repository under a new identity and
//! silently strands the prior store on disk. Registry GC removes the
//! stale *registry row* but never the on-disk store *data*, so the payload
//! accumulates invisibly (measured at ~41 GB in one observed profile). This
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
use crate::migrate::registry::{RootLivenessV1, probe_root};

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
    /// Git common directory recorded for the project. A linked worktree shares
    /// it with the primary checkout, so it keeps the identity live.
    pub git_common_dir: Option<PathBuf>,
    /// Every registered alias path for the project. Any live alias keeps the
    /// store live even when the canonical root is gone.
    pub alias_roots: Vec<PathBuf>,
    /// Whether the store manifest was read and parsed. A malformed or
    /// unreadable manifest makes the store's project root unverifiable, never
    /// "absent".
    pub manifest_readable: bool,
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
    /// Registered graph-scope database paths, relative to `data_root`. Scopes
    /// may sit at custom relative paths, so the durable-data check cannot infer
    /// them from the main graph alone.
    pub graph_scope_relpaths: Vec<PathBuf>,
}

/// Why a store's identity could not be resolved either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnverifiableReason {
    /// A root could not be inspected (permission or I/O failure), so absence
    /// was never proven.
    RootInspectionFailed,
    /// The store manifest was missing, unreadable, or malformed, so the store's
    /// own record of its project root could not be trusted.
    ManifestUnreadable,
}

/// What should happen to a store, decided purely from its census entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreDisposition {
    /// The identity still resolves to a live repository root. Keep.
    Live,
    /// The registry roots are gone but the manifest points at a different,
    /// currently-live root: the repository moved. Re-link, never collect.
    Relinkable { live_root: PathBuf },
    /// Liveness could not be determined. Never collected: retirement requires
    /// proof of absence, and a failed inspection is not proof.
    Unverifiable { reason: UnverifiableReason },
    /// Every root of this identity was *proven* absent. Eligible for collection
    /// once older than the retention window.
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
    /// Registered graph-scope database paths, relative to `data_root`; carried
    /// through so the durable-data check covers every scope, not just the main
    /// graph.
    pub graph_scope_relpaths: Vec<PathBuf>,
}

impl OrphanStoreFinding {
    pub fn is_orphaned(&self) -> bool {
        matches!(self.disposition, StoreDisposition::Orphaned)
    }

    pub fn is_relinkable(&self) -> bool {
        matches!(self.disposition, StoreDisposition::Relinkable { .. })
    }
}

/// Every root that can keep this store's identity alive: the registry roots,
/// the git common directory shared with linked worktrees, and every registered
/// alias path. Collecting a store because one checkout vanished, while another
/// checkout of the same repository is still enrolled, destroys live data.
fn identity_roots(entry: &StoreCensusEntry) -> impl Iterator<Item = &Path> {
    std::iter::once(entry.canonical_root.as_path())
        .chain(entry.display_root.as_deref())
        .chain(entry.git_common_dir.as_deref())
        .chain(entry.alias_roots.iter().map(PathBuf::as_path))
}

fn classify_one(entry: &StoreCensusEntry) -> StoreDisposition {
    let identity = identity_roots(entry).fold(RootLivenessV1::Absent, |liveness, root| {
        liveness.merge(probe_root(root))
    });
    match identity {
        RootLivenessV1::Live => return StoreDisposition::Live,
        // An inspection that failed proves nothing. Retiring on it would delete
        // a store whose repository may be perfectly alive behind an unreadable
        // parent directory or a stale mount.
        RootLivenessV1::Unverifiable => {
            return StoreDisposition::Unverifiable {
                reason: UnverifiableReason::RootInspectionFailed,
            };
        }
        RootLivenessV1::Absent => {}
    }
    // The manifest names this store's project root. If it could not be read or
    // parsed, the identity is unproven and the store is not collectable.
    if !entry.manifest_readable {
        return StoreDisposition::Unverifiable {
            reason: UnverifiableReason::ManifestUnreadable,
        };
    }
    // Registry identity is dead. If the manifest still names a live root the
    // repository moved rather than vanished — re-link instead of collecting.
    if let Some(manifest_root) = entry.manifest_root.as_deref()
        && manifest_root != entry.canonical_root
        && entry.display_root.as_deref() != Some(manifest_root)
    {
        match probe_root(manifest_root) {
            RootLivenessV1::Live => {
                return StoreDisposition::Relinkable {
                    live_root: manifest_root.to_path_buf(),
                };
            }
            RootLivenessV1::Unverifiable => {
                return StoreDisposition::Unverifiable {
                    reason: UnverifiableReason::RootInspectionFailed,
                };
            }
            RootLivenessV1::Absent => {}
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
            expected_store_relpath: entry.expected_store_relpath.clone(),
            expected_created_at: entry.expected_created_at,
            expected_last_write_at: entry.expected_last_write_at,
            expected_payload_mtime_secs: entry.expected_payload_mtime_secs,
            expected_manifest_bytes: entry.expected_manifest_bytes.clone(),
            graph_scope_relpaths: entry.graph_scope_relpaths.clone(),
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
    /// Liveness could not be proven either way — never collected, surfaced so
    /// an owner can resolve the inspection failure instead of losing the store.
    pub unverifiable: Vec<OrphanStoreFinding>,
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
            StoreDisposition::Unverifiable { .. } => plan.unverifiable.push(finding),
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
    /// The store's graph database carries rows in a durable per-project memory
    /// table (or the check could not prove otherwise). Never collected, even
    /// when every other eligibility check passed — see
    /// [`DurableMemoryCheck`]/[`check_durable_memory_rows`].
    DurableDataProtected,
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

        let scratch_root = durable_check_scratch_root(profile_root);
        match check_store_durable_memory(
            &finding.data_root,
            finding.expected_manifest_bytes.as_deref(),
            &finding.graph_scope_relpaths,
            &scratch_root,
        )
        .await
        {
            DurableMemoryCheck::Empty => {}
            DurableMemoryCheck::Present | DurableMemoryCheck::Unverifiable => {
                transaction.rollback().await.map_err(|error| {
                    orphan_db_error("rollback durable-memory-protected orphan store", error)
                })?;
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind: CollectionFailureKind::DurableDataProtected,
                });
                continue;
            }
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

/// Result of checking a store's graph database for durable memory rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurableMemoryCheck {
    /// No durable memory table has any row (including: none of the tables
    /// exist, or the database file itself does not exist). Safe to collect.
    Empty,
    /// At least one durable memory table has at least one row.
    Present,
    /// The check could not prove the store is free of durable memory rows
    /// (I/O error, corrupt/locked database, the source changed mid-check).
    /// Fails closed: treated exactly like `Present` by every caller.
    Unverifiable,
}

/// Every database under a store that can carry durable rows, or a typed
/// statement that the inventory itself could not be trusted.
///
/// A store is not one database. Besides the manifest-selected main graph there
/// are registered graph scopes (which may live at custom relative paths) and
/// per-branch databases under `branches/`, and legacy branch-exclusive memory
/// rows are known to exist only in the latter. Checking the main graph alone
/// declares a store empty while its branch databases still hold durable facts.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DurableDatabaseInventoryV1 {
    /// The complete set of database paths, relative to the store's data root.
    Resolved(Vec<PathBuf>),
    /// The set could not be enumerated — a missing or malformed manifest, or a
    /// directory that could not be listed. Never a green light for deletion.
    Unverifiable,
}

/// Enumerates every durable database under `data_root`.
///
/// Fails closed. The manifest is the store's own record of where its graph
/// lives; if it is absent or will not parse, guessing the default filename
/// would check the wrong file (or no file) and report "empty" for a store whose
/// real graph sits elsewhere.
fn durable_database_inventory(
    data_root: &Path,
    manifest_bytes: Option<&[u8]>,
    graph_scope_relpaths: &[PathBuf],
) -> DurableDatabaseInventoryV1 {
    let Some(bytes) = manifest_bytes else {
        return DurableDatabaseInventoryV1::Unverifiable;
    };
    let Ok(manifest) = serde_json::from_slice::<crate::storage::StoreManifest>(bytes) else {
        return DurableDatabaseInventoryV1::Unverifiable;
    };

    let mut inventory = vec![manifest.graph_db_relpath];
    for relpath in graph_scope_relpaths {
        if !inventory.contains(relpath) {
            inventory.push(relpath.clone());
        }
    }

    // Branch databases are discovered on disk: a legacy store can hold branch
    // databases the registry never recorded a scope for.
    let branches = data_root.join("branches");
    match std::fs::read_dir(&branches) {
        Ok(entries) => {
            for entry in entries {
                let Ok(entry) = entry else {
                    return DurableDatabaseInventoryV1::Unverifiable;
                };
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("db") {
                    continue;
                }
                let Some(name) = path.file_name() else {
                    continue;
                };
                let relpath = Path::new("branches").join(name);
                if !inventory.contains(&relpath) {
                    inventory.push(relpath);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        // The directory exists but could not be listed: its contents are
        // unknown, so the store's durable data is unproven.
        Err(_) => return DurableDatabaseInventoryV1::Unverifiable,
    }

    DurableDatabaseInventoryV1::Resolved(inventory)
}

/// Runs [`check_durable_memory_rows`] over every database in the store's
/// inventory. Any single `Present` or `Unverifiable` protects the whole store.
async fn check_store_durable_memory(
    data_root: &Path,
    manifest_bytes: Option<&[u8]>,
    graph_scope_relpaths: &[PathBuf],
    scratch_root: &Path,
) -> DurableMemoryCheck {
    let inventory =
        match durable_database_inventory(data_root, manifest_bytes, graph_scope_relpaths) {
            DurableDatabaseInventoryV1::Resolved(inventory) => inventory,
            DurableDatabaseInventoryV1::Unverifiable => return DurableMemoryCheck::Unverifiable,
        };
    for relpath in inventory {
        match check_durable_memory_rows(data_root, &relpath, scratch_root).await {
            DurableMemoryCheck::Empty => {}
            protected => return protected,
        }
    }
    DurableMemoryCheck::Empty
}

/// The read-snapshot scratch directory for durable-memory checks.
///
/// It lives under the *profile* root, never inside the store being examined.
/// Two reasons, both load-bearing: the store is a deletion candidate, and
/// writing into it bumps the newest mtime that
/// [`newest_mtime_secs`] uses as the revival fence — a store that failed one
/// check would have its age reset by the check itself and could never mature
/// past the retention window again.
fn durable_check_scratch_root(profile_root: &Path) -> PathBuf {
    profile_root.join("scratch").join("sqlite-read")
}

/// Checks whether `data_root`'s graph database carries rows in any canonical
/// `memory_*` table. This intentionally discovers tables from the schema
/// instead of maintaining a fixed list: both legacy memory and Memory V2 add
/// durable tables, and a newly added table must be protected automatically.
/// Side-effect-free with respect to the store: opens the database through
/// [`crate::sqlite_read_snapshot`], so the live store is never mutated or
/// locked against a concurrent writer.
async fn check_durable_memory_rows(
    data_root: &Path,
    graph_db_relpath: &Path,
    scratch_root: &Path,
) -> DurableMemoryCheck {
    let graph_db_path = data_root.join(graph_db_relpath);
    if !graph_db_path.exists() {
        // No database file at all: there is no schema that could carry rows.
        return DurableMemoryCheck::Empty;
    }
    // The snapshot layer creates only the final scratch component, so its
    // parent must exist first. Without this the snapshot fails NotFound, the
    // check fails closed as `Unverifiable`, and — because `Unverifiable` is
    // treated exactly like `Present` — *every* collection is refused. That is
    // safe, but it silently disables orphan reclamation entirely.
    if std::fs::create_dir_all(scratch_root).is_err() {
        return DurableMemoryCheck::Unverifiable;
    }
    let snapshot = match crate::sqlite_read_snapshot::open_in(&graph_db_path, scratch_root).await {
        Ok(snapshot) => snapshot,
        Err(_) => return DurableMemoryCheck::Unverifiable,
    };
    let connection = snapshot.connection();
    let mut rows = match connection
        .query(
            "SELECT name
             FROM pragma_table_list
             WHERE schema = 'main'
               AND type = 'table'
               AND name LIKE ?1 ESCAPE '\\'
             ORDER BY name",
            crate::db::engine::params!["memory\\_%"],
        )
        .await
    {
        Ok(rows) => rows,
        Err(_) => return DurableMemoryCheck::Unverifiable,
    };
    let mut present_tables = Vec::new();
    loop {
        match rows.next().await {
            Ok(Some(row)) => match row.get::<String>(0) {
                Ok(name) => present_tables.push(name),
                Err(_) => return DurableMemoryCheck::Unverifiable,
            },
            Ok(None) => break,
            Err(_) => return DurableMemoryCheck::Unverifiable,
        }
    }
    for table in present_tables {
        // `pragma_table_list.type = 'table'` intentionally excludes FTS
        // virtual/shadow tables, whose internal config rows are derived and
        // exist even when there is no durable memory. Identifiers cannot be
        // SQL parameters, so only interpolate TraceDecay's canonical shape;
        // an unexpected name fails closed rather than becoming SQL text.
        if !is_memory_table_identifier(&table) {
            return DurableMemoryCheck::Unverifiable;
        }
        let probe_sql = format!("SELECT 1 FROM \"{table}\" LIMIT 1");
        let mut probe_rows = match connection.query(&probe_sql, ()).await {
            Ok(rows) => rows,
            Err(_) => return DurableMemoryCheck::Unverifiable,
        };
        match probe_rows.next().await {
            Ok(Some(_)) => return DurableMemoryCheck::Present,
            Ok(None) => {}
            Err(_) => return DurableMemoryCheck::Unverifiable,
        }
    }
    if snapshot.validate_source().is_err() {
        // The file changed under us mid-check: cannot trust an empty result.
        return DurableMemoryCheck::Unverifiable;
    }
    DurableMemoryCheck::Empty
}

fn is_memory_table_identifier(table: &str) -> bool {
    table.strip_prefix("memory_").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

/// Newest mtime under `dir`, unix seconds, or `0` when nothing is readable.
///
/// Symlinks are not followed, and are stat'd as links rather than targets.
/// This walk is the revival fence for a delete: following a symlink would let
/// a link planted under a store recurse without bound, or make the fence read
/// an unrelated directory's mtime instead of the payload's.
fn newest_mtime_secs(dir: &Path) -> i64 {
    fn walk(path: &Path, newest: &mut i64) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else {
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
pub(crate) async fn build_store_census(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
) -> crate::errors::Result<Vec<StoreCensusEntry>> {
    let projects = db.list_code_projects(usize::MAX).await?;
    build_store_census_for_projects(db, profile_root, &projects).await
}

#[derive(Debug, Clone)]
pub(crate) struct StoreCensusPageV1 {
    pub(crate) entries: Vec<StoreCensusEntry>,
    pub(crate) next_cursor: Option<String>,
}

pub(crate) async fn build_store_census_page(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    after_project_id: Option<&str>,
    limit: usize,
) -> crate::errors::Result<StoreCensusPageV1> {
    let limit = limit.clamp(1, 64);
    let mut projects = db
        .list_code_projects_after(after_project_id, limit.saturating_add(1))
        .await?;
    let has_more = projects.len() > limit;
    projects.truncate(limit);
    let next_cursor = has_more
        .then(|| projects.last().map(|project| project.project_id.clone()))
        .flatten();
    let entries = build_store_census_for_projects(db, profile_root, &projects).await?;
    Ok(StoreCensusPageV1 {
        entries,
        next_cursor,
    })
}

async fn build_store_census_for_projects(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    projects: &[crate::global_db::CodeProjectRecord],
) -> crate::errors::Result<Vec<StoreCensusEntry>> {
    let mut census = Vec::new();
    // Aliases and the git common directory are part of the identity: a linked
    // worktree or a second enrolled checkout keeps the store live even when
    // this row's canonical root is gone.
    let contexts = db.project_registry_contexts_for_projects(projects).await?;
    for context in contexts {
        let project = &context.project;
        let alias_roots = context
            .aliases
            .iter()
            .map(|alias| PathBuf::from(&alias.alias_path))
            .collect::<Vec<_>>();
        let git_common_dir = project.git_common_dir.as_deref().map(PathBuf::from);
        for store in db
            .try_list_store_instances_for_project(&project.project_id)
            .await?
        {
            let graph_scope_relpaths = context
                .stores
                .iter()
                .filter(|candidate| candidate.store.store_id == store.store_id)
                .flat_map(|candidate| candidate.graph_scopes.iter())
                .map(|scope| PathBuf::from(&scope.db_relpath))
                .collect::<Vec<_>>();
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
            // A manifest that is absent or will not parse leaves this store's
            // own record of its project root unknown. Fail closed: record that
            // it is unverifiable rather than defaulting to "no manifest root",
            // which reads downstream as a collectable orphan.
            let parsed_manifest = expected_manifest_bytes
                .as_deref()
                .map(|bytes| serde_json::from_slice::<crate::storage::StoreManifest>(bytes).ok());
            let manifest_readable = matches!(parsed_manifest, Some(Some(_)));
            let manifest_root = parsed_manifest
                .flatten()
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
                git_common_dir: git_common_dir.clone(),
                alias_roots: alias_roots.clone(),
                manifest_readable,
                data_root,
                manifest_root,
                last_write_secs,
                size_bytes,
                expected_store_relpath: store.store_relpath,
                expected_created_at: store.created_at,
                expected_last_write_at: store.last_write_at,
                expected_payload_mtime_secs,
                expected_manifest_bytes,
                graph_scope_relpaths,
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
pub(crate) async fn sweep_orphan_stores(
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

// === Unregistered store directories (plan 38 §2, disjoint audit class) =====
//
// `build_store_census` walks *from* the registry: for every registered
// project, for every one of its registered store instances. A store dir with
// no registry trace at all — no `code_projects` row for its identity, ever —
// is invisible to that walk no matter how large it grows. This is a distinct
// failure mode from [`StoreDisposition::Orphaned`] (whose registry row still
// exists; only its root vanished): here the row itself is gone, e.g. because
// registry GC removed the stale identity row without also removing
// the on-disk payload it pointed at. The owner's audit measured this class at
// 322 directories / 655 MB in one profile. This section is a bottom-up
// counterpart: scan `profile_root/projects/*` (the layout every
// profile-sharded store uses, see [`crate::storage::profile_sharded_data_root`])
// and flag any leaf directory whose name is not a currently-registered
// `project_id`.

/// One store directory found on disk with no registry identity at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnregisteredStoreFinding {
    /// The `projects/` leaf directory name — the project id this store would
    /// have if it were registered.
    pub project_dir_name: String,
    pub data_root: PathBuf,
    /// `now - newest mtime under data_root`, clamped at zero.
    pub age_secs: i64,
    pub size_bytes: u64,
    /// Payload mtime fence captured at census time; re-verified before delete.
    pub(crate) expected_payload_mtime_secs: i64,
}

/// Bottom-up scan of `profile_root/projects/*`. Pure I/O plus one registry
/// read; never deletes anything. A directory is a candidate only when its
/// name both looks like a real project id ([`crate::storage::validate_project_id`])
/// and has no matching `code_projects` row — a stray file or a directory with
/// an unsafe name is skipped outright rather than risking misclassification.
pub(crate) async fn census_unregistered_project_dirs(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    now: i64,
) -> crate::errors::Result<Vec<UnregisteredStoreFinding>> {
    let registered: std::collections::HashSet<String> = db
        .list_code_projects(usize::MAX)
        .await?
        .into_iter()
        .map(|project| project.project_id)
        .collect();
    let projects_dir = profile_root.join("projects");
    let Ok(entries) = std::fs::read_dir(&projects_dir) else {
        return Ok(Vec::new());
    };
    let mut findings = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            // Never a store: a stray file under `projects/` is not part of the
            // profile-sharded contract and is left alone.
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if crate::storage::validate_project_id(&name).is_err() || registered.contains(&name) {
            continue;
        }
        let data_root = entry.path();
        let last_write_secs = newest_mtime_secs(&data_root);
        let size_bytes = dir_size_bytes(&data_root);
        findings.push(UnregisteredStoreFinding {
            project_dir_name: name,
            data_root,
            age_secs: now.saturating_sub(last_write_secs).max(0),
            size_bytes,
            expected_payload_mtime_secs: last_write_secs,
        });
    }
    findings.sort_by(|left, right| left.project_dir_name.cmp(&right.project_dir_name));
    Ok(findings)
}

/// The partitioned collection decision over a set of unregistered-store
/// findings. There is no `Live`/`Relinkable` disposition here — an
/// unregistered directory has no registry identity to resolve at all — so
/// every finding is either past the retention window or not.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UnregisteredCollectionPlan {
    pub collect: Vec<UnregisteredStoreFinding>,
    pub retained_immature: Vec<UnregisteredStoreFinding>,
}

impl UnregisteredCollectionPlan {
    /// Total bytes that collecting [`Self::collect`] would reclaim.
    pub fn collectable_bytes(&self) -> u64 {
        self.collect
            .iter()
            .fold(0u64, |acc, f| acc.saturating_add(f.size_bytes))
    }
}

/// Partition findings under a retention window. Pure.
pub fn plan_unregistered_collection(
    findings: Vec<UnregisteredStoreFinding>,
    retention_secs: i64,
) -> UnregisteredCollectionPlan {
    let mut plan = UnregisteredCollectionPlan::default();
    for finding in findings {
        if finding.age_secs >= retention_secs {
            plan.collect.push(finding);
        } else {
            plan.retained_immature.push(finding);
        }
    }
    plan
}

/// Deletes every directory in `plan.collect`, holding one write transaction
/// per store across a final revival recheck (still-unregistered, payload
/// unchanged, no durable memory rows) through the removal, so the window
/// between census and delete can never silently destroy a directory that was
/// registered or written to in between.
pub(crate) async fn execute_unregistered_collection(
    db: &RegisteredGlobalDb,
    plan: &UnregisteredCollectionPlan,
    profile_root: &Path,
) -> crate::errors::Result<CollectionOutcome> {
    let mut outcome = CollectionOutcome::default();
    let Ok(canonical_profile) = profile_root.canonicalize() else {
        outcome
            .errors
            .extend(plan.collect.iter().map(|finding| CollectionFailure {
                store_id: finding.project_dir_name.clone(),
                kind: CollectionFailureKind::InspectFailed,
            }));
        return Ok(outcome);
    };
    for finding in &plan.collect {
        // Containment + shape: only ever delete an exact, safely-named
        // `<profile>/projects/<id>` leaf.
        let expected = profile_root
            .join("projects")
            .join(&finding.project_dir_name);
        if expected != finding.data_root
            || crate::storage::validate_project_id(&finding.project_dir_name).is_err()
        {
            outcome.errors.push(CollectionFailure {
                store_id: finding.project_dir_name.clone(),
                kind: CollectionFailureKind::OutsideProfile,
            });
            continue;
        }
        let Ok(canonical_target) = finding.data_root.canonicalize() else {
            outcome.errors.push(CollectionFailure {
                store_id: finding.project_dir_name.clone(),
                kind: CollectionFailureKind::InspectFailed,
            });
            continue;
        };
        if canonical_target == canonical_profile
            || !canonical_target.starts_with(&canonical_profile)
        {
            outcome.errors.push(CollectionFailure {
                store_id: finding.project_dir_name.clone(),
                kind: CollectionFailureKind::OutsideProfile,
            });
            continue;
        }

        let transaction = db.begin_write_transaction().await?;
        let mut rows = transaction
            .query(
                "SELECT 1 FROM code_projects WHERE project_id = ?1",
                crate::db::engine::params![finding.project_dir_name.as_str()],
            )
            .await
            .map_err(|error| orphan_db_error("recheck unregistered store identity", error))?;
        let now_registered = rows
            .next()
            .await
            .map_err(|error| orphan_db_error("read unregistered store identity", error))?
            .is_some();
        drop(rows);
        if now_registered {
            transaction
                .rollback()
                .await
                .map_err(|error| orphan_db_error("rollback newly-registered store", error))?;
            outcome.errors.push(CollectionFailure {
                store_id: finding.project_dir_name.clone(),
                kind: CollectionFailureKind::RegistryChanged,
            });
            continue;
        }
        if finding.data_root.exists()
            && newest_mtime_secs(&finding.data_root) != finding.expected_payload_mtime_secs
        {
            transaction
                .rollback()
                .await
                .map_err(|error| orphan_db_error("rollback revived unregistered payload", error))?;
            outcome.errors.push(CollectionFailure {
                store_id: finding.project_dir_name.clone(),
                kind: CollectionFailureKind::PayloadChanged,
            });
            continue;
        }

        // An unreadable manifest must not be swallowed into "no manifest": the
        // inventory then fails closed instead of checking a guessed database.
        let manifest_bytes = std::fs::read(
            finding
                .data_root
                .join(crate::storage::STORE_MANIFEST_FILENAME),
        )
        .ok();
        let scratch_root = durable_check_scratch_root(profile_root);
        // An unregistered store has no registry graph scopes by definition;
        // its branch databases are still discovered from disk.
        match check_store_durable_memory(
            &finding.data_root,
            manifest_bytes.as_deref(),
            &[],
            &scratch_root,
        )
        .await
        {
            DurableMemoryCheck::Empty => {}
            DurableMemoryCheck::Present | DurableMemoryCheck::Unverifiable => {
                transaction.rollback().await.map_err(|error| {
                    orphan_db_error(
                        "rollback durable-memory-protected unregistered store",
                        error,
                    )
                })?;
                outcome.errors.push(CollectionFailure {
                    store_id: finding.project_dir_name.clone(),
                    kind: CollectionFailureKind::DurableDataProtected,
                });
                continue;
            }
        }

        match std::fs::remove_dir_all(&finding.data_root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                transaction.rollback().await.map_err(|error| {
                    orphan_db_error("rollback failed unregistered removal", error)
                })?;
                outcome.errors.push(CollectionFailure {
                    store_id: finding.project_dir_name.clone(),
                    kind: CollectionFailureKind::RemoveFailed,
                });
                continue;
            }
        }
        transaction
            .commit()
            .await
            .map_err(|error| orphan_db_error("commit unregistered store fence", error))?;

        outcome.reclaimed_bytes = outcome.reclaimed_bytes.saturating_add(finding.size_bytes);
        outcome.collected.push(CollectedStore {
            project_id: finding.project_dir_name.clone(),
            store_id: finding.project_dir_name.clone(),
            data_root: finding.data_root.clone(),
            size_bytes: finding.size_bytes,
        });
    }
    Ok(outcome)
}

/// The report returned by an unregistered-store sweep.
#[derive(Debug, Clone, Default)]
pub struct UnregisteredStoreSweepReport {
    pub plan: UnregisteredCollectionPlan,
    pub applied: bool,
    pub outcome: CollectionOutcome,
}

/// Typed daemon/doctor entry point for the unregistered-directory class:
/// census → plan → optionally collect. Mirrors [`sweep_orphan_stores`]'s
/// dry-run/apply contract exactly, over the disjoint on-disk-only finding
/// class above.
pub(crate) async fn sweep_unregistered_stores(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    retention_secs: i64,
    now: i64,
    apply: bool,
) -> crate::errors::Result<UnregisteredStoreSweepReport> {
    let findings = census_unregistered_project_dirs(db, profile_root, now).await?;
    let plan = plan_unregistered_collection(findings, retention_secs);
    if !apply {
        return Ok(UnregisteredStoreSweepReport {
            plan,
            applied: false,
            outcome: CollectionOutcome::default(),
        });
    }
    let outcome = execute_unregistered_collection(db, &plan, profile_root).await?;
    Ok(UnregisteredStoreSweepReport {
        plan,
        applied: true,
        outcome,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
