//! Registered and unregistered orphan-store collection under the census fences.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Instant;

use cap_std::fs::Dir;
use tracedecay_contracts::storage::is_retired_branch_store_path;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_private_fs::capability_dir::{remove_open_dir_all_nofollow, sync_directory};
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};

use super::fence::{
    StoreContentFence, StoreDirectoryFence, capture_store_content_fence_in_dir_controlled,
    capture_store_directory_fence, data_root_fence_matches, open_store_directory_nofollow,
    profile_relative_store_path,
};
use super::pages::newest_mtime_secs_controlled;
use super::{
    CollectedStore, CollectionCompletionV1, CollectionFailure, CollectionFailureKind,
    CollectionMutationFailure, CollectionMutationOperation, CollectionOutcome, CollectionPlan,
    OrphanStoreFinding, UnregisteredCollectionPlan, UnregisteredStoreFinding,
};

/// Cooperative budget carried through every expensive retention read and
/// apply boundary. The database writer is acquired only after content hashing
/// and durable-memory inspection have completed under this control.
#[derive(Clone, Copy)]
pub(crate) struct CollectionControl<'a> {
    cancellation: &'a CancellationToken,
    deadline: MonotonicDeadline,
}

impl<'a> CollectionControl<'a> {
    pub(crate) const fn new(
        cancellation: &'a CancellationToken,
        deadline: MonotonicDeadline,
    ) -> Self {
        Self {
            cancellation,
            deadline,
        }
    }

    pub(crate) fn completion(self) -> Option<CollectionCompletionV1> {
        if self.cancellation.is_cancelled() {
            Some(CollectionCompletionV1::Cancelled)
        } else if self.deadline.is_elapsed_at(Instant::now()) {
            Some(CollectionCompletionV1::DeadlineExceeded)
        } else {
            None
        }
    }

    /// Adapt the retention admission to the canonical `SQLite` read-snapshot
    /// control. The snapshot layer may copy/materialize a foreign database in
    /// `spawn_blocking`, so it must observe the same live cancellation and
    /// deadline rather than an unbounded root-shim control.
    pub(crate) fn snapshot_read_control(
        self,
    ) -> tracedecay_runtime_core::sqlite_read_snapshot::SnapshotReadControl {
        let cancellation = (*self.cancellation).clone();
        tracedecay_runtime_core::sqlite_read_snapshot::SnapshotReadControl::new(
            self.deadline.instant(),
            move || cancellation.is_cancelled(),
        )
    }

    /// Race an awaitable inspection or `SQLite` command against the admission's
    /// cancellation/deadline. Losing the race never authorizes the following
    /// destructive phase; a later pass inspects the store afresh.
    pub(crate) async fn race<T>(
        self,
        future: impl Future<Output = T>,
    ) -> Result<T, CollectionCompletionV1> {
        if let Some(completion) = self.completion() {
            return Err(completion);
        }
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => Err(CollectionCompletionV1::Cancelled),
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(self.deadline.instant())) => {
                Err(CollectionCompletionV1::DeadlineExceeded)
            }
            result = future => {
                self.completion().map_or(Ok(result), Err)
            }
        }
    }
}

pub(crate) fn unbounded_collection_control() -> CollectionControl<'static> {
    static CANCELLATION: std::sync::OnceLock<CancellationToken> = std::sync::OnceLock::new();
    CollectionControl::new(
        CANCELLATION.get_or_init(CancellationToken::new),
        MonotonicDeadline::at(Instant::now() + std::time::Duration::from_hours(24)),
    )
}

pub(crate) fn store_finding_is_profile_contained(
    finding: &OrphanStoreFinding,
    profile_root: &Path,
) -> bool {
    profile_relative_store_path(profile_root, &finding.data_root)
        .is_ok_and(|relative| relative == Path::new(&finding.expected_store_relpath))
        && matches!(
            capture_store_directory_fence(profile_root, &finding.data_root),
            Ok(StoreDirectoryFence::Missing | StoreDirectoryFence::Present { .. })
        )
}

fn registered_payload_fence_matches(
    finding: &OrphanStoreFinding,
    profile_root: &Path,
    control: CollectionControl<'_>,
) -> Result<bool, CollectionFailureKind> {
    if !data_root_fence_matches(
        &finding.expected_data_root_fence,
        profile_root,
        &finding.data_root,
    )? {
        return Ok(false);
    }
    match &finding.expected_data_root_fence {
        StoreDirectoryFence::Missing => Ok(true),
        StoreDirectoryFence::Present { .. } => {
            Ok(newest_mtime_secs_controlled(&finding.data_root, control)?
                == finding.expected_payload_mtime_secs)
        }
        StoreDirectoryFence::Unverifiable => Err(CollectionFailureKind::InspectFailed),
    }
}

fn unregistered_payload_fence_matches(
    finding: &UnregisteredStoreFinding,
    profile_root: &Path,
    control: CollectionControl<'_>,
) -> Result<bool, CollectionFailureKind> {
    if !data_root_fence_matches(
        &finding.expected_data_root_fence,
        profile_root,
        &finding.data_root,
    )? {
        return Ok(false);
    }
    match &finding.expected_data_root_fence {
        StoreDirectoryFence::Missing => Ok(true),
        StoreDirectoryFence::Present { .. } => {
            Ok(newest_mtime_secs_controlled(&finding.data_root, control)?
                == finding.expected_payload_mtime_secs)
        }
        StoreDirectoryFence::Unverifiable => Err(CollectionFailureKind::InspectFailed),
    }
}

/// The censused store leaf, opened no-follow and proven byte-for-byte equal to
/// its census content fence. Removal goes through this handle, so a path
/// swapped in after the proof is never the directory that gets deleted.
pub(super) struct VerifiedStore {
    parent: Dir,
    root: Dir,
    data_root: PathBuf,
}

impl VerifiedStore {
    /// Runs to completion once started: callers invoke it only after the
    /// registry authority is retired, and a cancelled removal would leave a
    /// half-deleted store with no owner.
    pub(super) fn remove(self) -> Result<(), CollectionMutationFailure> {
        let Self {
            parent,
            root,
            data_root,
        } = self;
        remove_open_dir_all_nofollow(root, &mut || Ok(())).map_err(|error| {
            CollectionMutationFailure::from_io_error(
                CollectionMutationOperation::RecursiveRemove,
                data_root.clone(),
                &error,
            )
        })?;
        sync_directory(&parent).map_err(|error| {
            CollectionMutationFailure::from_io_error(
                CollectionMutationOperation::ParentSync,
                data_root
                    .parent()
                    .map_or_else(PathBuf::new, Path::to_path_buf),
                &error,
            )
        })
    }
}

/// Opens `data_root` and proves its exact content still equals `expected`.
/// `Ok(None)` means the census already observed the store absent.
pub(super) fn open_verified_store(
    profile_root: &Path,
    data_root: &Path,
    expected: &StoreContentFence,
    control: CollectionControl<'_>,
) -> Result<Option<VerifiedStore>, CollectionFailureKind> {
    match expected {
        StoreContentFence::Missing => return Ok(None),
        StoreContentFence::Unverifiable => return Err(CollectionFailureKind::InspectFailed),
        StoreContentFence::Present(_) => {}
    }
    let capability = open_store_directory_nofollow(profile_root, data_root)?;
    match capture_store_content_fence_in_dir_controlled(&capability.root, Some(control)) {
        Ok(actual) if matches!(expected, StoreContentFence::Present(inventory) if *inventory == actual) => {
            Ok(Some(VerifiedStore {
                parent: capability.parent,
                root: capability.root,
                data_root: data_root.to_path_buf(),
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
            Err(CollectionFailureKind::Cancelled)
        }
        Ok(_) | Err(_) => Err(CollectionFailureKind::PayloadChanged),
    }
}

/// Terminal state of one finding within a bounded collection pass.
enum FindingStep {
    Collected,
    /// The registry authority was retired but the bytes were not fully
    /// removed; the failure names the exact filesystem operation.
    RemoveFailed(CollectionMutationFailure),
    Refused(CollectionFailureKind),
    Interrupted(CollectionCompletionV1),
}

fn interrupted(control: CollectionControl<'_>) -> FindingStep {
    FindingStep::Interrupted(
        control
            .completion()
            .unwrap_or(CollectionCompletionV1::Cancelled),
    )
}

fn payload_fence_step(
    matches: Result<bool, CollectionFailureKind>,
    control: CollectionControl<'_>,
) -> Option<FindingStep> {
    match matches {
        Ok(true) => None,
        Ok(false) => Some(FindingStep::Refused(CollectionFailureKind::PayloadChanged)),
        Err(CollectionFailureKind::Cancelled) => Some(interrupted(control)),
        Err(kind) => Some(FindingStep::Refused(kind)),
    }
}

fn durable_memory_step(
    check: DurableMemoryCheck,
    control: CollectionControl<'_>,
) -> Option<FindingStep> {
    match check {
        DurableMemoryCheck::Empty => None,
        DurableMemoryCheck::Present | DurableMemoryCheck::Unverifiable => Some(
            FindingStep::Refused(CollectionFailureKind::DurableDataProtected),
        ),
        DurableMemoryCheck::Interrupted => Some(interrupted(control)),
    }
}

fn verified_store_step(
    opened: Result<Option<VerifiedStore>, CollectionFailureKind>,
    control: CollectionControl<'_>,
) -> Result<Option<VerifiedStore>, FindingStep> {
    match opened {
        Ok(store) => Ok(store),
        Err(CollectionFailureKind::Cancelled) => Err(interrupted(control)),
        Err(kind) => Err(FindingStep::Refused(kind)),
    }
}

fn remove_step(store: Option<VerifiedStore>) -> FindingStep {
    match store.map(VerifiedStore::remove) {
        None | Some(Ok(())) => FindingStep::Collected,
        Some(Err(failure)) => FindingStep::RemoveFailed(failure),
    }
}

/// Records one step; returns `false` when the pass must stop.
fn record_step(outcome: &mut CollectionOutcome, step: FindingStep, store: CollectedStore) -> bool {
    let kind = match step {
        FindingStep::Collected => {
            outcome.reclaimed_bytes = outcome.reclaimed_bytes.saturating_add(store.size_bytes);
            outcome.collected.push(store);
            return true;
        }
        FindingStep::Interrupted(completion) => {
            outcome.completion = completion;
            return false;
        }
        FindingStep::RemoveFailed(failure) => CollectionFailureKind::RemoveFailed(failure),
        FindingStep::Refused(kind) => kind,
    };
    outcome.errors.push(CollectionFailure {
        store_id: store.store_id,
        kind,
    });
    true
}

/// Expensive inspection (payload, manifest, durable memory, exact content)
/// runs without a writer; a short final transaction then retires the exact
/// registry row before the verified store directory is deleted.
pub async fn execute_registered_collection(
    db: &RegisteredGlobalDb,
    plan: &CollectionPlan,
    profile_root: &Path,
) -> tracedecay_domain::errors::Result<(CollectionOutcome, usize)> {
    execute_registered_collection_controlled(db, plan, profile_root, unbounded_collection_control())
        .await
}

#[hotpath::measure(label = "maintenance.orphan_stores.collect_registered", future = true)]
pub(crate) async fn execute_registered_collection_controlled(
    db: &RegisteredGlobalDb,
    plan: &CollectionPlan,
    profile_root: &Path,
    control: CollectionControl<'_>,
) -> tracedecay_domain::errors::Result<(CollectionOutcome, usize)> {
    let mut outcome = CollectionOutcome::default();
    let mut retired = 0usize;
    for finding in &plan.collect {
        if let Some(completion) = control.completion() {
            outcome.completion = completion;
            break;
        }
        let step = collect_registered_finding(db, finding, profile_root, control).await?;
        if matches!(step, FindingStep::Collected | FindingStep::RemoveFailed(_)) {
            retired = retired.saturating_add(1);
        }
        let store = CollectedStore {
            project_id: finding.project_id.clone(),
            store_id: finding.store_id.clone(),
            data_root: finding.data_root.clone(),
            size_bytes: finding.size_bytes,
        };
        if !record_step(&mut outcome, step, store) {
            break;
        }
    }
    Ok((outcome, retired))
}

async fn collect_registered_finding(
    db: &RegisteredGlobalDb,
    finding: &OrphanStoreFinding,
    profile_root: &Path,
    control: CollectionControl<'_>,
) -> tracedecay_domain::errors::Result<FindingStep> {
    if !store_finding_is_profile_contained(finding, profile_root) {
        return Ok(FindingStep::Refused(CollectionFailureKind::OutsideProfile));
    }
    if let Some(step) = payload_fence_step(
        registered_payload_fence_matches(finding, profile_root, control),
        control,
    ) {
        return Ok(step);
    }
    let expected_row = Some((
        finding.expected_store_relpath.clone(),
        finding.expected_created_at,
        finding.expected_last_write_at,
    ));
    let current_stores = match control
        .race(db.try_list_store_instances_for_project(&finding.project_id))
        .await
    {
        Ok(stores) => stores?,
        Err(completion) => return Ok(FindingStep::Interrupted(completion)),
    };
    let current = current_stores
        .into_iter()
        .find(|store| store.store_id == finding.store_id)
        .map(|store| (store.store_relpath, store.created_at, store.last_write_at));
    if current != expected_row {
        return Ok(FindingStep::Refused(CollectionFailureKind::RegistryChanged));
    }

    let manifest_path = finding
        .data_root
        .join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME);
    let current_manifest = match read_regular_file(&manifest_path) {
        RegularFileSnapshot::Bytes(bytes) => Some(bytes),
        RegularFileSnapshot::Missing => None,
        RegularFileSnapshot::Unverifiable => {
            return Ok(FindingStep::Refused(CollectionFailureKind::InspectFailed));
        }
    };
    if current_manifest != finding.expected_manifest_bytes {
        return Ok(FindingStep::Refused(CollectionFailureKind::ManifestChanged));
    }

    let check = check_store_durable_memory(
        &finding.data_root,
        finding.expected_manifest_bytes.as_deref(),
        &finding.graph_scope_relpaths,
        &durable_check_scratch_root(profile_root),
        control,
    )
    .await;
    if let Some(step) = durable_memory_step(check, control) {
        return Ok(step);
    }
    // The durable inventory can take a private snapshot and therefore leaves
    // a window for a concurrent replacement.
    if let Some(step) = payload_fence_step(
        registered_payload_fence_matches(finding, profile_root, control),
        control,
    ) {
        return Ok(step);
    }
    let store = match verified_store_step(
        open_verified_store(
            profile_root,
            &finding.data_root,
            &finding.expected_content_fence,
            control,
        ),
        control,
    ) {
        Ok(store) => store,
        Err(step) => return Ok(step),
    };

    let transaction = match control.race(db.begin_write_transaction()).await {
        Ok(transaction) => transaction?,
        Err(completion) => return Ok(FindingStep::Interrupted(completion)),
    };
    let mut rows = match control
        .race(transaction.query(
            "SELECT store_relpath, created_at, last_write_at
             FROM store_instances
             WHERE project_id = ?1 AND store_id = ?2",
            tracedecay_runtime_core::db::engine::params![
                finding.project_id.as_str(),
                finding.store_id.as_str()
            ],
        ))
        .await
    {
        Ok(rows) => rows.map_err(|error| orphan_db_error("confirm orphan registry", error))?,
        Err(completion) => return Ok(FindingStep::Interrupted(completion)),
    };
    let next = match control.race(rows.next()).await {
        Ok(next) => next.map_err(|error| orphan_db_error("read orphan registry", error))?,
        Err(completion) => return Ok(FindingStep::Interrupted(completion)),
    };
    let current = match next {
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
    if current != expected_row {
        transaction
            .rollback()
            .await
            .map_err(|error| orphan_db_error("rollback changed orphan", error))?;
        return Ok(FindingStep::Refused(CollectionFailureKind::RegistryChanged));
    }
    let deleted = match control
        .race(transaction.execute(
            "DELETE FROM store_instances
             WHERE project_id = ?1 AND store_id = ?2
               AND store_relpath = ?3 AND created_at = ?4
               AND last_write_at IS ?5",
            tracedecay_runtime_core::db::engine::params![
                finding.project_id.as_str(),
                finding.store_id.as_str(),
                finding.expected_store_relpath.as_str(),
                finding.expected_created_at,
                finding.expected_last_write_at
            ],
        ))
        .await
    {
        Ok(deleted) => {
            deleted.map_err(|error| orphan_db_error("retire collected orphan store", error))?
        }
        Err(completion) => return Ok(FindingStep::Interrupted(completion)),
    };
    if deleted != 1 {
        transaction
            .rollback()
            .await
            .map_err(|error| orphan_db_error("rollback raced orphan retirement", error))?;
        return Ok(FindingStep::Refused(CollectionFailureKind::RegistryChanged));
    }
    match control
        .race(transaction.execute(
            "DELETE FROM code_projects
             WHERE project_id = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM store_instances WHERE project_id = ?1
            )",
            tracedecay_runtime_core::db::engine::params![finding.project_id.as_str()],
        ))
        .await
    {
        Ok(result) => {
            result.map_err(|error| orphan_db_error("retire empty collected project", error))?;
        }
        Err(completion) => return Ok(FindingStep::Interrupted(completion)),
    }
    // Not raced: abandoning an in-flight commit would leave the retirement
    // ambiguous while the verified bytes stay on disk.
    transaction
        .commit()
        .await
        .map_err(|error| orphan_db_error("commit collected orphan retirement", error))?;
    Ok(remove_step(store))
}

fn orphan_db_error(
    operation: &'static str,
    error: impl std::fmt::Display,
) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Database {
        operation: operation.to_string(),
        message: error.to_string(),
    }
}

/// Result of checking a store's graph database for durable memory rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DurableMemoryCheck {
    /// Cooperative cancellation/deadline interrupted recursive discovery or a
    /// bounded database probe before any mutation.
    Interrupted,
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
/// The databases registered as project authorities for durable memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DurableDatabaseInventoryV1 {
    /// The bounded scan stopped before it could establish a complete durable
    /// database inventory. This is not an unverifiable green light: callers
    /// preserve the exact cancellation/deadline state for the coordinator.
    Interrupted,
    /// The complete set of database paths, relative to the store's data root.
    Resolved(Vec<PathBuf>),
    /// The set could not be enumerated, a missing or malformed manifest, or a
    /// directory that could not be listed. Never a green light for deletion.
    Unverifiable,
}

/// A regular-file read that preserves the difference between an absent
/// optional artifact and an unsafe/unreadable one. In particular, `read()`
/// follows symlinks; retention must never turn a symlinked manifest into a
/// trusted manifest snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RegularFileSnapshot {
    Missing,
    Bytes(Vec<u8>),
    Unverifiable,
}

pub(super) fn read_regular_file(path: &Path) -> RegularFileSnapshot {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RegularFileSnapshot::Missing;
        }
        Err(_) => return RegularFileSnapshot::Unverifiable,
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return RegularFileSnapshot::Unverifiable;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return RegularFileSnapshot::Unverifiable;
    };
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.file_type().is_file() => {
            RegularFileSnapshot::Bytes(bytes)
        }
        _ => RegularFileSnapshot::Unverifiable,
    }
}

/// Store manifests and registry-provided graph scopes are path authorities,
/// not arbitrary filesystem paths. Only normalized, non-empty relative paths
/// made entirely from normal components are accepted; `..`, `.`, roots,
/// prefixes, and empty paths all fail closed before joining.
fn safe_store_relative_path(path: &Path) -> bool {
    let mut saw_normal = false;
    let mut normalized = PathBuf::new();
    for component in path.components() {
        if let std::path::Component::Normal(component) = component {
            saw_normal = true;
            normalized.push(component);
        } else {
            return false;
        }
    }
    saw_normal && normalized == path
}

/// Reject symlinked directory components as well as a symlinked final file.
/// A lexical relative-path check alone is insufficient when an intermediate
/// directory redirects outside the store.
fn safe_store_path(data_root: &Path, relative: &Path) -> bool {
    if !safe_store_relative_path(relative) {
        return false;
    }
    let mut current = data_root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return false;
        };
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return false,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
            Err(_) => return false,
        }
    }
    true
}

/// Enumerates every durable database a store's manifest and registered graph
/// scopes name, relative to its data root.
///
/// Fails closed. The manifest is the store's own record of where its graph
/// lives; if it is absent or will not parse, guessing the default filename
/// would check the wrong file (or no file) and report "empty" for a store whose
/// real graph sits elsewhere.
pub(super) fn durable_database_inventory(
    manifest_bytes: Option<&[u8]>,
    graph_scope_relpaths: &[PathBuf],
    control: CollectionControl<'_>,
) -> DurableDatabaseInventoryV1 {
    if control.completion().is_some() {
        return DurableDatabaseInventoryV1::Interrupted;
    }
    let Some(bytes) = manifest_bytes else {
        return DurableDatabaseInventoryV1::Unverifiable;
    };
    let manifest =
        match serde_json::from_slice::<tracedecay_runtime_core::storage::StoreManifest>(bytes) {
            Ok(manifest) => manifest,
            Err(_) if control.completion().is_some() => {
                return DurableDatabaseInventoryV1::Interrupted;
            }
            Err(_) => return DurableDatabaseInventoryV1::Unverifiable,
        };
    if control.completion().is_some() {
        return DurableDatabaseInventoryV1::Interrupted;
    }

    if !safe_store_relative_path(&manifest.graph_db_relpath) {
        return DurableDatabaseInventoryV1::Unverifiable;
    }

    let mut inventory = vec![manifest.graph_db_relpath];
    for relpath in graph_scope_relpaths {
        if control.completion().is_some() {
            return DurableDatabaseInventoryV1::Interrupted;
        }
        if !safe_store_relative_path(relpath) {
            return DurableDatabaseInventoryV1::Unverifiable;
        }
        if !inventory.contains(relpath) {
            inventory.push(relpath.clone());
        }
    }

    DurableDatabaseInventoryV1::Resolved(inventory)
}

/// Runs [`check_durable_memory_rows`] over every database in the store's
/// inventory. Any single `Present` or `Unverifiable` protects the whole store.
pub(super) async fn check_store_durable_memory(
    data_root: &Path,
    manifest_bytes: Option<&[u8]>,
    graph_scope_relpaths: &[PathBuf],
    scratch_root: &Path,
    control: CollectionControl<'_>,
) -> DurableMemoryCheck {
    if control.completion().is_some() {
        return DurableMemoryCheck::Interrupted;
    }
    let inventory = match durable_database_inventory(manifest_bytes, graph_scope_relpaths, control)
    {
        DurableDatabaseInventoryV1::Interrupted => return DurableMemoryCheck::Interrupted,
        DurableDatabaseInventoryV1::Resolved(inventory) => inventory,
        DurableDatabaseInventoryV1::Unverifiable => return DurableMemoryCheck::Unverifiable,
    };
    for relpath in inventory {
        if control.completion().is_some() {
            return DurableMemoryCheck::Interrupted;
        }
        match check_durable_memory_rows(data_root, &relpath, scratch_root, control).await {
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
/// [`walk_store_stats`] uses as the revival fence, a store that failed one
/// check would have its age reset by the check itself and could never mature
/// past the retention window again.
pub(super) fn durable_check_scratch_root(profile_root: &Path) -> PathBuf {
    profile_root.join("scratch").join("sqlite-read")
}

/// Checks whether `data_root`'s graph database carries rows in any canonical
/// `memory_*` table. This intentionally discovers tables from the schema
/// instead of maintaining a fixed list: both legacy memory and Memory V2 add
/// durable tables, and a newly added table must be protected automatically.
/// Side-effect-free with respect to the store: opens the database through
/// [`tracedecay_runtime_core::sqlite_read_snapshot`], so the live store is never mutated or
/// locked against a concurrent writer.
async fn check_durable_memory_rows(
    data_root: &Path,
    graph_db_relpath: &Path,
    scratch_root: &Path,
    control: CollectionControl<'_>,
) -> DurableMemoryCheck {
    if control.completion().is_some() {
        return DurableMemoryCheck::Interrupted;
    }
    if !safe_store_path(data_root, graph_db_relpath) {
        return DurableMemoryCheck::Unverifiable;
    }
    let graph_db_path = data_root.join(graph_db_relpath);
    match std::fs::symlink_metadata(&graph_db_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.file_type().is_file() => {
            return DurableMemoryCheck::Unverifiable;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // No database file at all: there is no schema that could carry
            // durable rows.
            return DurableMemoryCheck::Empty;
        }
        Err(_) => return DurableMemoryCheck::Unverifiable,
    }
    // The snapshot layer creates only the final scratch component, so its
    // parent must exist first. Without this the snapshot fails NotFound, the
    // check fails closed as `Unverifiable`, and because `Unverifiable` is
    // treated exactly like `Present`, *every* collection is refused. That is
    // safe, but it silently disables orphan reclamation entirely.
    if control.completion().is_some() || std::fs::create_dir_all(scratch_root).is_err() {
        return if control.completion().is_some() {
            DurableMemoryCheck::Interrupted
        } else {
            DurableMemoryCheck::Unverifiable
        };
    }
    let snapshot = match control
        .race(
            tracedecay_runtime_core::sqlite_read_snapshot::open_foreign_in(
                &graph_db_path,
                scratch_root,
                control.snapshot_read_control(),
            ),
        )
        .await
    {
        Err(_) => return DurableMemoryCheck::Interrupted,
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(_)) => return DurableMemoryCheck::Unverifiable,
    };
    if control.completion().is_some() {
        return DurableMemoryCheck::Interrupted;
    }
    let connection = snapshot.connection();
    let mut rows = match control
        .race(connection.query(
            "SELECT name
             FROM pragma_table_list
             WHERE schema = 'main'
               AND type = 'table'
               AND name LIKE ?1 ESCAPE '\\'
             ORDER BY name",
            tracedecay_runtime_core::db::engine::params!["memory\\_%"],
        ))
        .await
    {
        Err(_) => return DurableMemoryCheck::Interrupted,
        Ok(Ok(rows)) => rows,
        Ok(Err(_)) => return DurableMemoryCheck::Unverifiable,
    };
    let mut present_tables = Vec::new();
    loop {
        let next = match control.race(rows.next()).await {
            Err(_) => return DurableMemoryCheck::Interrupted,
            Ok(Ok(next)) => next,
            Ok(Err(_)) => return DurableMemoryCheck::Unverifiable,
        };
        match next {
            Some(row) => match row.get::<String>(0) {
                Ok(name) => present_tables.push(name),
                Err(_) => return DurableMemoryCheck::Unverifiable,
            },
            None => break,
        }
    }
    drop(rows);
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
        let mut probe_rows = match control.race(connection.query(&probe_sql, ())).await {
            Err(_) => return DurableMemoryCheck::Interrupted,
            Ok(Ok(rows)) => rows,
            Ok(Err(_)) => return DurableMemoryCheck::Unverifiable,
        };
        match control.race(probe_rows.next()).await {
            Err(_) => return DurableMemoryCheck::Interrupted,
            Ok(Ok(Some(_))) => return DurableMemoryCheck::Present,
            Ok(Ok(None)) => {}
            Ok(Err(_)) => return DurableMemoryCheck::Unverifiable,
        }
    }
    if control.completion().is_some() {
        return DurableMemoryCheck::Interrupted;
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

#[cfg(test)]
pub(crate) async fn execute_unregistered_collection(
    db: &RegisteredGlobalDb,
    plan: &UnregisteredCollectionPlan,
    profile_root: &Path,
) -> tracedecay_domain::errors::Result<CollectionOutcome> {
    execute_unregistered_collection_controlled(
        db,
        plan,
        profile_root,
        unbounded_collection_control(),
    )
    .await
}

#[hotpath::measure(
    label = "maintenance.orphan_stores.collect_unregistered",
    future = true
)]
pub(crate) async fn execute_unregistered_collection_controlled(
    db: &RegisteredGlobalDb,
    plan: &UnregisteredCollectionPlan,
    profile_root: &Path,
    control: CollectionControl<'_>,
) -> tracedecay_domain::errors::Result<CollectionOutcome> {
    let mut outcome = CollectionOutcome::default();
    for finding in &plan.collect {
        if let Some(completion) = control.completion() {
            outcome.completion = completion;
            break;
        }
        let step = collect_unregistered_finding(db, finding, profile_root, control).await?;
        let store = CollectedStore {
            project_id: finding.project_dir_name.clone(),
            store_id: finding.project_dir_name.clone(),
            data_root: finding.data_root.clone(),
            size_bytes: finding.size_bytes,
        };
        if !record_step(&mut outcome, step, store) {
            break;
        }
    }
    Ok(outcome)
}

async fn collect_unregistered_finding(
    db: &RegisteredGlobalDb,
    finding: &UnregisteredStoreFinding,
    profile_root: &Path,
    control: CollectionControl<'_>,
) -> tracedecay_domain::errors::Result<FindingStep> {
    // Containment + shape: only ever delete an exact, safely-named
    // `<profile>/projects/<id>` leaf.
    let expected = profile_root
        .join("projects")
        .join(&finding.project_dir_name);
    if expected != finding.data_root
        || tracedecay_runtime_core::storage::validate_project_id(&finding.project_dir_name).is_err()
    {
        return Ok(FindingStep::Refused(CollectionFailureKind::OutsideProfile));
    }
    if let Some(step) = payload_fence_step(
        unregistered_payload_fence_matches(finding, profile_root, control),
        control,
    ) {
        return Ok(step);
    }
    match control
        .race(db.code_project_exists(&finding.project_dir_name))
        .await
    {
        Ok(exists) => {
            if exists? {
                return Ok(FindingStep::Refused(CollectionFailureKind::RegistryChanged));
            }
        }
        Err(completion) => return Ok(FindingStep::Interrupted(completion)),
    }

    let scratch_root = durable_check_scratch_root(profile_root);
    // An unreadable manifest must not be swallowed into "no manifest": the
    // inventory then fails closed instead of checking a guessed database. A
    // manifestless directory is different: only an exact empty-tree inventory
    // proves that it carries no durable authority. Arbitrary payload files
    // remain unverifiable, while any discovered `.db` family is inspected
    // directly and remains fail-closed on error.
    let manifest_path = finding
        .data_root
        .join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME);
    let check = match read_regular_file(&manifest_path) {
        RegularFileSnapshot::Bytes(manifest_bytes) => {
            // An unregistered store has no registry graph scopes by
            // definition; the manifest remains the canonical graph path.
            check_store_durable_memory(
                &finding.data_root,
                Some(&manifest_bytes),
                &[],
                &scratch_root,
                control,
            )
            .await
        }
        RegularFileSnapshot::Missing => {
            check_manifestless_store_durable_memory(&finding.data_root, &scratch_root, control)
                .await
        }
        RegularFileSnapshot::Unverifiable => DurableMemoryCheck::Unverifiable,
    };
    if let Some(step) = durable_memory_step(check, control) {
        return Ok(step);
    }
    // The durable-data inspection is not a deletion lock: an in-profile
    // replacement or symlink swap must not inherit its decision.
    if let Some(step) = payload_fence_step(
        unregistered_payload_fence_matches(finding, profile_root, control),
        control,
    ) {
        return Ok(step);
    }
    let store = match verified_store_step(
        open_verified_store(
            profile_root,
            &finding.data_root,
            &finding.expected_content_fence,
            control,
        ),
        control,
    ) {
        Ok(store) => store,
        Err(step) => return Ok(step),
    };

    // The writer transaction orders this final absence proof after any
    // in-flight registration commit.
    let transaction = match control.race(db.begin_write_transaction()).await {
        Ok(transaction) => transaction?,
        Err(completion) => return Ok(FindingStep::Interrupted(completion)),
    };
    let mut rows = match control
        .race(transaction.query(
            "SELECT 1 FROM code_projects WHERE project_id = ?1",
            tracedecay_runtime_core::db::engine::params![finding.project_dir_name.as_str()],
        ))
        .await
    {
        Ok(rows) => rows.map_err(|error| orphan_db_error("confirm unregistered store", error))?,
        Err(completion) => return Ok(FindingStep::Interrupted(completion)),
    };
    let now_registered = match control.race(rows.next()).await {
        Ok(next) => next
            .map_err(|error| orphan_db_error("read unregistered store", error))?
            .is_some(),
        Err(completion) => return Ok(FindingStep::Interrupted(completion)),
    };
    drop(rows);
    transaction
        .rollback()
        .await
        .map_err(|error| orphan_db_error("release unregistered store fence", error))?;
    if now_registered {
        return Ok(FindingStep::Refused(CollectionFailureKind::RegistryChanged));
    }
    Ok(remove_step(store))
}

/// Inspects a manifestless unregistered directory without inventing a graph
/// path. An exactly empty directory is provably free of durable rows. Any
/// arbitrary payload, symlink, or unreadable entry remains unverifiable;
/// when a SQLite-looking file is present, every such file is treated as a
/// possible durable authority and inspected fail-closed.
async fn check_manifestless_store_durable_memory(
    data_root: &Path,
    scratch_root: &Path,
    control: CollectionControl<'_>,
) -> DurableMemoryCheck {
    let mut databases = Vec::new();
    if control.completion().is_some() {
        return DurableMemoryCheck::Interrupted;
    }
    if collect_sqlite_candidates(data_root, data_root, &mut databases, control).is_err() {
        return if control.completion().is_some() {
            DurableMemoryCheck::Interrupted
        } else {
            DurableMemoryCheck::Unverifiable
        };
    }
    if databases.is_empty() {
        return DurableMemoryCheck::Empty;
    }
    for relpath in databases {
        if control.completion().is_some() {
            return DurableMemoryCheck::Interrupted;
        }
        match check_durable_memory_rows(data_root, &relpath, scratch_root, control).await {
            DurableMemoryCheck::Empty => {}
            protected => return protected,
        }
    }
    DurableMemoryCheck::Empty
}

/// Finds only regular `.db` files below a store and never follows symlinks.
/// The manifestless path deliberately does not guess a single filename, so a
/// custom legacy graph cannot be mistaken for payload-only debris. Any other
/// file shape is an unverifiable durable-data candidate, not disposable dust.
/// Files of the retired `branches/` store layout are old data with no current
/// owner: they are neither candidates nor unrecognized payload, and they are
/// deleted with the directory.
fn collect_sqlite_candidates(
    root: &Path,
    current: &Path,
    output: &mut Vec<PathBuf>,
    control: CollectionControl<'_>,
) -> std::io::Result<()> {
    let entries = std::fs::read_dir(current)?;
    for entry in entries {
        if control.completion().is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "retention durable-data inventory interrupted",
            ));
        }
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(std::io::Error::other(
                "manifestless store contains a symlink",
            ));
        }
        let path = entry.path();
        let relative = path.strip_prefix(root).map_err(std::io::Error::other)?;
        if file_type.is_dir() {
            collect_sqlite_candidates(root, &path, output, control)?;
        } else if file_type.is_file() && is_retired_branch_store_path(relative) {
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("db")
        {
            output.push(relative.to_path_buf());
        } else {
            return Err(std::io::Error::other(
                "manifestless store contains an unrecognized payload",
            ));
        }
    }
    output.sort();
    Ok(())
}
