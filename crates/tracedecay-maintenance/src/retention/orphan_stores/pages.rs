//! Census paging, store walks, and sweep reports for orphan-store retention.

use std::path::{Path, PathBuf};

use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};

use super::fence::{
    StoreContentFence, StoreDirectoryFence, capture_store_content_fence,
    capture_store_content_fence_controlled, capture_store_directory_fence,
};
use super::quarantine::{RegularFileSnapshot, read_regular_file};
use super::unregistered_page::{
    DEFAULT_UNREGISTERED_STORE_PAGE_LIMIT, UnregisteredStoreSweepReport,
    UnregisteredStoreSweepRequestV1, UnregisteredSweepCompletionV1, sweep_unregistered_store_page,
};
use super::{
    CollectionControl, CollectionFailureKind, CollectionOutcome, CollectionPlan, StoreCensusEntry,
    StoreDisposition, classify_one,
};
#[cfg(test)]
use super::{
    CollectionFailure, classify_stores, execute_registered_collection, plan_collection,
    quarantine::store_finding_is_profile_contained,
};

pub(super) struct StoreWalkStats {
    pub(super) newest_mtime_secs: i64,
    pub(super) size_bytes: u64,
}

/// One no-follow walk for age and size. Symlinks contribute mtime but are
/// never followed or billed, matching the prior separate walk policies.
pub(super) fn walk_store_stats(dir: &Path) -> StoreWalkStats {
    fn walk(path: &Path, newest: &mut i64, size: &mut u64) {
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
            if meta.is_symlink() {
                continue;
            }
            if meta.is_dir() {
                walk(&entry.path(), newest, size);
            } else if meta.is_file() {
                *size = size.saturating_add(meta.len());
            }
        }
    }
    let mut newest = 0i64;
    let mut size = 0u64;
    walk(dir, &mut newest, &mut size);
    StoreWalkStats {
        newest_mtime_secs: newest,
        size_bytes: size,
    }
}

/// Controlled counterpart of [`walk_store_stats`]. Every recursive descent
/// checks the maintenance admission before asking the next directory for
/// entries, so a cancellation cannot turn age accounting into an unbounded
/// traversal. Ordinary I/O remains best-effort exactly as in the unbounded
/// census; only the caller-owned interruption is surfaced distinctly.
fn walk_store_stats_controlled(
    dir: &Path,
    control: CollectionControl<'_>,
) -> Result<StoreWalkStats, CollectionFailureKind> {
    fn walk(
        path: &Path,
        newest: &mut i64,
        size: &mut u64,
        control: CollectionControl<'_>,
    ) -> Result<(), CollectionFailureKind> {
        if control.completion().is_some() {
            return Err(CollectionFailureKind::Cancelled);
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return Ok(());
        };
        for entry in entries {
            if control.completion().is_some() {
                return Err(CollectionFailureKind::Cancelled);
            }
            let Ok(entry) = entry else {
                continue;
            };
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if let Ok(modified) = meta.modified()
                && let Ok(elapsed) = modified.duration_since(std::time::UNIX_EPOCH)
            {
                *newest = (*newest).max(elapsed.as_secs() as i64);
            }
            if meta.is_symlink() {
                continue;
            }
            if meta.is_dir() {
                walk(&entry.path(), newest, size, control)?;
            } else if meta.is_file() {
                *size = size.saturating_add(meta.len());
            }
        }
        Ok(())
    }

    let mut newest = 0i64;
    let mut size = 0u64;
    walk(dir, &mut newest, &mut size, control)?;
    Ok(StoreWalkStats {
        newest_mtime_secs: newest,
        size_bytes: size,
    })
}

pub(crate) fn newest_mtime_secs_controlled(
    dir: &Path,
    control: CollectionControl<'_>,
) -> Result<i64, CollectionFailureKind> {
    walk_store_stats_controlled(dir, control).map(|stats| stats.newest_mtime_secs)
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
    walk_store_stats(dir).size_bytes
}

/// Controlled counterpart of [`dir_size_bytes`]. It preserves the original
/// best-effort accounting policy for unreadable entries while making the
/// recursive work bounded by the caller's admission control.
pub(crate) fn dir_size_bytes_controlled(
    dir: &Path,
    control: CollectionControl<'_>,
) -> Result<u64, CollectionFailureKind> {
    walk_store_stats_controlled(dir, control).map(|stats| stats.size_bytes)
}

/// Build the on-disk store census from the registry. Reads manifests and sizes
/// directories but never mutates. Only profile-sharded stores are considered;
/// other storage modes are not laid out under the profile root here.
#[hotpath::measure(label = "maintenance.orphan_stores.census", future = true)]
pub async fn build_store_census(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
) -> tracedecay_domain::errors::Result<Vec<StoreCensusEntry>> {
    let projects = db.list_code_projects(usize::MAX).await?;
    build_store_census_for_projects(db, profile_root, &projects, None)
        .await?
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: "unbounded store census was unexpectedly interrupted".to_owned(),
        })
}

#[derive(Debug, Clone)]
pub struct StoreCensusPageV1 {
    pub entries: Vec<StoreCensusEntry>,
    pub next_cursor: Option<String>,
}

#[hotpath::measure(label = "maintenance.orphan_stores.census_page", future = true)]
pub async fn build_store_census_page(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    after_project_id: Option<&str>,
    limit: usize,
) -> tracedecay_domain::errors::Result<StoreCensusPageV1> {
    let limit = limit.clamp(1, 64);
    let mut projects = db
        .list_code_projects_after(after_project_id, limit.saturating_add(1))
        .await?;
    let has_more = projects.len() > limit;
    projects.truncate(limit);
    let next_cursor = has_more
        .then(|| projects.last().map(|project| project.project_id.clone()))
        .flatten();
    let entries = build_store_census_for_projects(db, profile_root, &projects, None)
        .await?
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: "unbounded store census page was unexpectedly interrupted".to_owned(),
        })?;
    Ok(StoreCensusPageV1 {
        entries,
        next_cursor,
    })
}

async fn build_store_census_for_projects(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    projects: &[tracedecay_global_db::CodeProjectRecord],
    control: Option<CollectionControl<'_>>,
) -> tracedecay_domain::errors::Result<Option<Vec<StoreCensusEntry>>> {
    let mut census = Vec::new();
    // Aliases and the git common directory are part of the identity: a linked
    // worktree or a second enrolled checkout keeps the store live even when
    // this row's canonical root is gone.
    let contexts = match control {
        Some(control) => match control
            .race(db.project_registry_contexts_for_projects(projects))
            .await
        {
            Ok(Ok(contexts)) => contexts,
            Ok(Err(error)) => return Err(error),
            Err(_) => return Ok(None),
        },
        None => db.project_registry_contexts_for_projects(projects).await?,
    };
    for context in contexts {
        if control.is_some_and(|control| control.completion().is_some()) {
            return Ok(None);
        }
        let project = &context.project;
        let alias_roots = context
            .aliases
            .iter()
            .map(|alias| PathBuf::from(&alias.alias_path))
            .collect::<Vec<_>>();
        let git_common_dir = project.git_common_dir.as_deref().map(PathBuf::from);
        let stores = match control {
            Some(control) => match control
                .race(db.try_list_store_instances_for_project(&project.project_id))
                .await
            {
                Ok(Ok(stores)) => stores,
                Ok(Err(error)) => return Err(error),
                Err(_) => return Ok(None),
            },
            None => {
                db.try_list_store_instances_for_project(&project.project_id)
                    .await?
            }
        };
        for store in stores {
            if control.is_some_and(|control| control.completion().is_some()) {
                return Ok(None);
            }
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
            let cheap = match inspect_store_leaf_cheap(profile_root, &data_root, control).await {
                Ok(Some(cheap)) => cheap,
                Ok(None) => return Ok(None),
                Err(error) => return Err(error),
            };
            let last_write_secs = store
                .last_write_at
                .filter(|value| *value > 0)
                .unwrap_or(cheap.expected_payload_mtime_secs);
            census.push(StoreCensusEntry {
                project_id: project.project_id.clone(),
                store_id: store.store_id.clone(),
                canonical_root: PathBuf::from(&project.canonical_root),
                display_root: (project.display_root != project.canonical_root)
                    .then(|| PathBuf::from(&project.display_root)),
                git_common_dir: git_common_dir.clone(),
                alias_roots: alias_roots.clone(),
                manifest_readable: cheap.manifest_readable,
                data_root,
                manifest_root: cheap.manifest_root,
                last_write_secs,
                size_bytes: cheap.size_bytes,
                expected_store_relpath: store.store_relpath,
                expected_created_at: store.created_at,
                expected_last_write_at: store.last_write_at,
                expected_payload_mtime_secs: cheap.expected_payload_mtime_secs,
                expected_data_root_fence: cheap.expected_data_root_fence,
                expected_content_fence: StoreContentFence::Missing,
                expected_manifest_bytes: cheap.expected_manifest_bytes,
                graph_scope_relpaths,
            });
        }
    }
    if attach_lazy_content_fences(&mut census, profile_root, control)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    Ok(Some(census))
}

struct CheapStoreInspect {
    expected_payload_mtime_secs: i64,
    size_bytes: u64,
    expected_data_root_fence: StoreDirectoryFence,
    expected_manifest_bytes: Option<Vec<u8>>,
    manifest_readable: bool,
    manifest_root: Option<PathBuf>,
}

fn inspect_store_leaf_cheap_sync(profile_root: &Path, data_root: &Path) -> CheapStoreInspect {
    let manifest_path = data_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME);
    let expected_manifest_bytes = match read_regular_file(&manifest_path) {
        RegularFileSnapshot::Bytes(bytes) => Some(bytes),
        RegularFileSnapshot::Missing | RegularFileSnapshot::Unverifiable => None,
    };
    let parsed_manifest = expected_manifest_bytes.as_deref().map(|bytes| {
        serde_json::from_slice::<tracedecay_runtime_core::storage::StoreManifest>(bytes).ok()
    });
    let manifest_readable = matches!(parsed_manifest, Some(Some(_)));
    let manifest_root = parsed_manifest
        .flatten()
        .map(|manifest| manifest.project_root);
    let stats = walk_store_stats(data_root);
    let expected_data_root_fence = capture_store_directory_fence(profile_root, data_root)
        .unwrap_or(StoreDirectoryFence::Unverifiable);
    CheapStoreInspect {
        expected_payload_mtime_secs: stats.newest_mtime_secs,
        size_bytes: stats.size_bytes,
        expected_data_root_fence,
        expected_manifest_bytes,
        manifest_readable,
        manifest_root,
    }
}

async fn inspect_store_leaf_cheap(
    profile_root: &Path,
    data_root: &Path,
    control: Option<CollectionControl<'_>>,
) -> tracedecay_domain::errors::Result<Option<CheapStoreInspect>> {
    if let Some(control) = control {
        if control.completion().is_some() {
            return Ok(None);
        }
        let manifest_path =
            data_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME);
        let expected_manifest_bytes = match read_regular_file(&manifest_path) {
            RegularFileSnapshot::Bytes(bytes) => Some(bytes),
            RegularFileSnapshot::Missing | RegularFileSnapshot::Unverifiable => None,
        };
        let parsed_manifest = expected_manifest_bytes.as_deref().map(|bytes| {
            serde_json::from_slice::<tracedecay_runtime_core::storage::StoreManifest>(bytes).ok()
        });
        let manifest_readable = matches!(parsed_manifest, Some(Some(_)));
        let manifest_root = parsed_manifest
            .flatten()
            .map(|manifest| manifest.project_root);
        let stats = match walk_store_stats_controlled(data_root, control) {
            Ok(stats) => stats,
            Err(_) => return Ok(None),
        };
        let expected_data_root_fence = capture_store_directory_fence(profile_root, data_root)
            .unwrap_or(StoreDirectoryFence::Unverifiable);
        return Ok(Some(CheapStoreInspect {
            expected_payload_mtime_secs: stats.newest_mtime_secs,
            size_bytes: stats.size_bytes,
            expected_data_root_fence,
            expected_manifest_bytes,
            manifest_readable,
            manifest_root,
        }));
    }
    let profile_root = profile_root.to_path_buf();
    let data_root = data_root.to_path_buf();
    tokio::task::spawn_blocking(move || inspect_store_leaf_cheap_sync(&profile_root, &data_root))
        .await
        .map(Some)
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("store census inspect join failed: {error}"),
        })
}

async fn attach_lazy_content_fences(
    census: &mut [StoreCensusEntry],
    profile_root: &Path,
    control: Option<CollectionControl<'_>>,
) -> tracedecay_domain::errors::Result<Option<()>> {
    for entry in census.iter_mut() {
        if matches!(classify_one(entry), StoreDisposition::Live) {
            continue;
        }
        if control.is_some_and(|control| control.completion().is_some()) {
            return Ok(None);
        }
        let profile_root = profile_root.to_path_buf();
        let data_root = entry.data_root.clone();
        entry.expected_content_fence = if let Some(control) = control {
            match capture_store_content_fence_controlled(&profile_root, &data_root, control) {
                Ok(fence) => fence,
                Err(CollectionFailureKind::Cancelled) => return Ok(None),
                Err(_) => StoreContentFence::Unverifiable,
            }
        } else {
            tokio::task::spawn_blocking(move || {
                capture_store_content_fence(&profile_root, &data_root)
                    .unwrap_or(StoreContentFence::Unverifiable)
            })
            .await
            .unwrap_or(StoreContentFence::Unverifiable)
        };
    }
    Ok(Some(()))
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
#[cfg(test)]
pub(crate) async fn sweep_orphan_stores(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    retention_secs: i64,
    now: i64,
    apply: bool,
) -> tracedecay_domain::errors::Result<OrphanSweepReport> {
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

// Unregistered store directories.
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
// profile-sharded store uses, see [`tracedecay_runtime_core::storage::profile_sharded_data_root`])
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
    pub expected_payload_mtime_secs: i64,
    /// Stable data-root generation captured with the inspection finding.
    pub expected_data_root_fence: StoreDirectoryFence,
    /// Exact no-follow inventory/content identity captured at census time.
    pub expected_content_fence: StoreContentFence,
    /// The store's own manifest names a project root that can never be
    /// registered here again: it lies under the OS temp directory while this
    /// profile is durable, or it no longer exists on disk. Such a store is
    /// collectable without waiting out the retention window.
    pub abandoned_root: bool,
}

/// Test-only one-page census convenience. Production callers use
/// [`sweep_unregistered_store_page`] and persist its cursor between bounded
/// daemon admissions.
#[cfg(test)]
pub async fn census_unregistered_project_dirs(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    now: i64,
) -> tracedecay_domain::errors::Result<Vec<UnregisteredStoreFinding>> {
    let cancellation = CancellationToken::new();
    let report = sweep_unregistered_store_page(
        db,
        profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: None,
            limit: DEFAULT_UNREGISTERED_STORE_PAGE_LIMIT,
            retention_secs: i64::MAX,
            now,
            apply: false,
            cancellation: &cancellation,
            deadline: MonotonicDeadline::at(
                std::time::Instant::now() + std::time::Duration::from_secs(5),
            ),
        },
    )
    .await?;
    Ok(report
        .plan
        .collect
        .into_iter()
        .chain(report.plan.retained_immature)
        .collect())
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
        if finding.abandoned_root || finding.age_secs >= retention_secs {
            plan.collect.push(finding);
        } else {
            plan.retained_immature.push(finding);
        }
    }
    plan
}

/// Whether the manifest under `data_root` names a project root this durable
/// profile can never register again: one under the OS temp directory, or one
/// that is definitively gone. A missing or unreadable manifest, a root that
/// still exists, or an unreadable root all answer `false` and leave the
/// retention window in charge.
pub(crate) fn manifest_names_abandoned_root(data_root: &Path, profile_root: &Path) -> bool {
    let Ok(manifest) = tracedecay_runtime_core::storage::read_store_manifest(
        &data_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
    ) else {
        return false;
    };
    if manifest.project_root.as_os_str().is_empty() {
        return false;
    }
    tracedecay_global_db::ephemeral_root_rejection(&manifest.project_root, profile_root).is_some()
        || matches!(
            std::fs::symlink_metadata(&manifest.project_root),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        )
}

/// Deletes unregistered directories through the same two-phase boundary:
/// content/durable inspection and quarantine first, then a short final
/// still-unregistered confirmation before the irreversible phase.
///
/// Compatibility convenience for one bounded read/apply page. The daemon uses
/// [`sweep_unregistered_store_page`] directly so it can persist the returned
/// cursor across maintenance cadences; Doctor deliberately receives one
/// bounded preview rather than a hidden full-profile traversal.
#[hotpath::measure(label = "maintenance.orphan_stores.sweep_unregistered", future = true)]
pub async fn sweep_unregistered_stores(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    retention_secs: i64,
    now: i64,
    apply: bool,
) -> tracedecay_domain::errors::Result<UnregisteredStoreSweepReport> {
    let cancellation = CancellationToken::new();
    let report = sweep_unregistered_store_page(
        db,
        profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: None,
            limit: DEFAULT_UNREGISTERED_STORE_PAGE_LIMIT,
            retention_secs,
            now,
            apply,
            cancellation: &cancellation,
            deadline: MonotonicDeadline::at(
                std::time::Instant::now() + std::time::Duration::from_secs(5),
            ),
        },
    )
    .await?;
    let completion_is_terminal = report.completion == UnregisteredSweepCompletionV1::Complete;
    let receipt_is_consistent = (!report.applied || apply && completion_is_terminal)
        && (completion_is_terminal || report.next_cursor.is_none())
        && (apply || report.outcome.collected.is_empty());
    if !receipt_is_consistent {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "unregistered-store page returned an inconsistent receipt".to_owned(),
        });
    }
    Ok(report)
}
