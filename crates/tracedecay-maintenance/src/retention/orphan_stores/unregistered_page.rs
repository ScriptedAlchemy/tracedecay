//! Bounded, resumable census and collection of unregistered project leaves.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, SystemTimeSpec};
use cap_std::fs::OpenOptions;
use serde::{Deserialize, Serialize};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};

use super::fence::capture_store_content_fence_controlled;
use super::fence::open_store_directory_nofollow;
use super::quarantine::{
    QuarantineRecoveryOutcome, quarantined_project_id, recover_named_store_quarantine,
};
use super::{
    CollectionCompletionV1, CollectionControl, CollectionFailure, CollectionFailureKind,
    CollectionOutcome, CollectionRecoveryAction, CollectionRecoveryReceipt, StoreContentFence,
    StoreDirectoryFence, UnregisteredCollectionPlan, UnregisteredStoreFinding,
    capture_store_directory_fence, dir_size_bytes_controlled,
    execute_unregistered_collection_controlled, newest_mtime_secs_controlled,
    plan_unregistered_collection,
};

pub const DEFAULT_UNREGISTERED_STORE_PAGE_LIMIT: usize = 8;
const MAX_UNREGISTERED_STORE_PAGE_LIMIT: usize = 64;
const UNREGISTERED_STORE_DIRECTORY_ENTRY_MULTIPLIER: usize = 8;
const PORTABLE_INVENTORY_MAX_IDLE: Duration = Duration::from_hours(24);
const PORTABLE_INVENTORY_GC_SCAN_LIMIT: usize = 64;
const PORTABLE_INVENTORY_GC_REMOVE_LIMIT: usize = 8;

pub(in crate::retention) enum ProjectDirectoryWorkV1 {
    Project(String),
    Quarantine {
        project_id: String,
        quarantine_name: String,
    },
}

pub(in crate::retention) struct ProjectDirectoryPageV1 {
    pub entries: Vec<ProjectDirectoryWorkV1>,
    pub next_cursor: Option<String>,
    /// Raw directory or portable-inventory entries consumed to produce this
    /// slice. Summing pages exposes nonlinear rescans without timing heuristics.
    pub entries_scanned: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnregisteredSweepCompletionV1 {
    #[default]
    Complete,
    Cancelled,
    DeadlineExceeded,
}

impl UnregisteredSweepCompletionV1 {
    fn interrupted(cancellation: &CancellationToken, deadline: MonotonicDeadline) -> Option<Self> {
        if cancellation.is_cancelled() {
            Some(Self::Cancelled)
        } else if deadline.is_elapsed_at(Instant::now()) {
            Some(Self::DeadlineExceeded)
        } else {
            None
        }
    }
}

/// One daemon/Doctor-owned page. The cursor is an opaque position in the
/// profile's project directory; callers persist it only after this page has
/// reached a terminal completion state.
pub struct UnregisteredStoreSweepRequestV1<'a> {
    pub(crate) cursor: Option<String>,
    pub(crate) limit: usize,
    pub(crate) retention_secs: i64,
    pub(crate) now: i64,
    pub(crate) apply: bool,
    pub(crate) cancellation: &'a CancellationToken,
    pub(crate) deadline: MonotonicDeadline,
}

/// Inspection/confirmation/apply receipt for exactly one bounded page.
#[derive(Debug, Clone, Default)]
pub struct UnregisteredStoreSweepReport {
    pub plan: UnregisteredCollectionPlan,
    pub applied: bool,
    pub outcome: CollectionOutcome,
    pub next_cursor: Option<String>,
    pub completion: UnregisteredSweepCompletionV1,
}

/// Performs the full inspection → confirmation → apply journey for one page,
/// honoring cancellation/deadline before each bounded filesystem/registry
/// action. A cancellation never reports an empty successful page or mutates a
/// partially inspected plan.
#[hotpath::measure(
    label = "maintenance.orphan_stores.sweep_unregistered_page",
    future = true
)]
pub async fn sweep_unregistered_store_page(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    request: UnregisteredStoreSweepRequestV1<'_>,
) -> tracedecay_domain::errors::Result<UnregisteredStoreSweepReport> {
    let limit = request.limit.clamp(1, MAX_UNREGISTERED_STORE_PAGE_LIMIT);
    if let Some(completion) =
        UnregisteredSweepCompletionV1::interrupted(request.cancellation, request.deadline)
    {
        return Ok(observed_page_report(interrupted_report(
            completion,
            CollectionOutcome::default(),
        )));
    }
    let mut recovery_outcome = CollectionOutcome::default();
    let census = census_unregistered_project_dirs_page(
        db,
        profile_root,
        request.cursor.as_deref(),
        limit,
        request.now,
        request.apply,
        request.cancellation,
        request.deadline,
        &mut recovery_outcome,
    )
    .await?;
    let Some((findings, next_cursor)) = census else {
        return Ok(observed_page_report(interrupted_report(
            UnregisteredSweepCompletionV1::interrupted(request.cancellation, request.deadline)
                .unwrap_or(UnregisteredSweepCompletionV1::DeadlineExceeded),
            recovery_outcome,
        )));
    };
    let plan = plan_unregistered_collection(findings, request.retention_secs);
    if !request.apply {
        return Ok(observed_page_report(UnregisteredStoreSweepReport {
            plan,
            applied: false,
            outcome: recovery_outcome,
            next_cursor,
            completion: UnregisteredSweepCompletionV1::Complete,
        }));
    }
    if let Some(completion) =
        UnregisteredSweepCompletionV1::interrupted(request.cancellation, request.deadline)
    {
        return Ok(observed_page_report(UnregisteredStoreSweepReport {
            plan: UnregisteredCollectionPlan::default(),
            applied: false,
            outcome: recovery_outcome,
            next_cursor: request.cursor,
            completion,
        }));
    }
    let mut outcome = execute_unregistered_collection_controlled(
        db,
        &plan,
        profile_root,
        CollectionControl::new(request.cancellation, request.deadline),
    )
    .await?;
    outcome.reclaimed_bytes = outcome
        .reclaimed_bytes
        .saturating_add(recovery_outcome.reclaimed_bytes);
    outcome.collected.extend(recovery_outcome.collected);
    outcome.errors.extend(recovery_outcome.errors);
    outcome
        .recovery_receipts
        .extend(recovery_outcome.recovery_receipts);
    let completion = match outcome.completion {
        CollectionCompletionV1::Complete => UnregisteredSweepCompletionV1::Complete,
        CollectionCompletionV1::Cancelled => UnregisteredSweepCompletionV1::Cancelled,
        CollectionCompletionV1::DeadlineExceeded => UnregisteredSweepCompletionV1::DeadlineExceeded,
    };
    Ok(observed_page_report(UnregisteredStoreSweepReport {
        plan,
        applied: completion == UnregisteredSweepCompletionV1::Complete,
        outcome,
        next_cursor: (completion == UnregisteredSweepCompletionV1::Complete)
            .then_some(next_cursor)
            .flatten(),
        completion,
    }))
}

fn interrupted_report(
    completion: UnregisteredSweepCompletionV1,
    outcome: CollectionOutcome,
) -> UnregisteredStoreSweepReport {
    UnregisteredStoreSweepReport {
        outcome,
        completion,
        ..UnregisteredStoreSweepReport::default()
    }
}

/// Page-terminal census: cancelled and deadline-bounded pages count next to
/// complete ones so a starved sweep is visible, and collected/failed items
/// are attributed even when the page ends early.
fn observed_page_report(report: UnregisteredStoreSweepReport) -> UnregisteredStoreSweepReport {
    match report.completion {
        UnregisteredSweepCompletionV1::Complete => {
            hotpath::gauge!("maintenance.orphan_stores.unregistered.page_complete_total")
                .inc(1_u64);
        }
        UnregisteredSweepCompletionV1::Cancelled => {
            hotpath::gauge!("maintenance.orphan_stores.unregistered.page_cancelled_total")
                .inc(1_u64);
        }
        UnregisteredSweepCompletionV1::DeadlineExceeded => {
            hotpath::gauge!("maintenance.orphan_stores.unregistered.page_deadline_total")
                .inc(1_u64);
        }
    }
    hotpath::gauge!("maintenance.orphan_stores.unregistered.collected_total")
        .inc(report.outcome.collected.len());
    hotpath::gauge!("maintenance.orphan_stores.unregistered.failed_total")
        .inc(report.outcome.errors.len());
    hotpath::gauge!("maintenance.orphan_stores.unregistered.reclaimed_bytes_total")
        .inc(report.outcome.reclaimed_bytes);
    report
}

/// Builds only one page of costly child inventories. Its directory cursor is
/// advanced by a bounded number of raw entries, so a profile with many
/// unregistered leaves cannot turn one writer admission into a full scan.
async fn census_unregistered_project_dirs_page(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    cursor: Option<&str>,
    limit: usize,
    now: i64,
    recover_interrupted_quarantines: bool,
    cancellation: &CancellationToken,
    deadline: MonotonicDeadline,
    recovery_outcome: &mut CollectionOutcome,
) -> tracedecay_domain::errors::Result<Option<(Vec<UnregisteredStoreFinding>, Option<String>)>> {
    let projects_dir = profile_root.join("projects");
    let interrupted =
        || UnregisteredSweepCompletionV1::interrupted(cancellation, deadline).is_some();
    let page = read_project_directory_page(profile_root, cursor, limit, &interrupted)?;
    let Some(page) = page else {
        return Ok(None);
    };
    let next_cursor = page.next_cursor;
    let mut recovered_project_ids = HashSet::new();
    let mut findings = Vec::with_capacity(page.entries.len());
    for work in page.entries {
        if UnregisteredSweepCompletionV1::interrupted(cancellation, deadline).is_some() {
            return Ok(None);
        }
        let ProjectDirectoryWorkV1::Quarantine {
            project_id,
            quarantine_name,
        } = work
        else {
            let ProjectDirectoryWorkV1::Project(name) = work else {
                continue;
            };
            let control = CollectionControl::new(cancellation, deadline);
            let is_registered = match control.race(db.code_project_exists(&name)).await {
                Ok(Ok(exists)) => exists,
                Ok(Err(error)) => return Err(error),
                Err(_) => return Ok(None),
            };
            if is_registered {
                continue;
            }
            if recovered_project_ids.contains(&name) {
                continue;
            }
            let data_root = projects_dir.join(&name);
            let metadata = match std::fs::symlink_metadata(&data_root) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            let expected_data_root_fence = capture_store_directory_fence(profile_root, &data_root)
                .unwrap_or(StoreDirectoryFence::Unverifiable);
            let expected_content_fence = match capture_store_content_fence_controlled(
                profile_root,
                &data_root,
                CollectionControl::new(cancellation, deadline),
            ) {
                Ok(fence) => fence,
                Err(CollectionFailureKind::Cancelled) => return Ok(None),
                Err(_) => StoreContentFence::Unverifiable,
            };
            if UnregisteredSweepCompletionV1::interrupted(cancellation, deadline).is_some() {
                return Ok(None);
            }
            let last_write_secs = match newest_mtime_secs_controlled(&data_root, control) {
                Ok(mtime) => mtime,
                Err(CollectionFailureKind::Cancelled) => return Ok(None),
                Err(_) => return Ok(None),
            };
            let size_bytes = match dir_size_bytes_controlled(&data_root, control) {
                Ok(size) => size,
                Err(CollectionFailureKind::Cancelled) => return Ok(None),
                Err(_) => return Ok(None),
            };
            findings.push(UnregisteredStoreFinding {
                project_dir_name: name,
                data_root,
                age_secs: now.saturating_sub(last_write_secs).max(0),
                size_bytes,
                expected_payload_mtime_secs: last_write_secs,
                expected_data_root_fence,
                expected_content_fence,
            });
            continue;
        };
        if recover_interrupted_quarantines {
            let data_root = projects_dir.join(&project_id);
            let record_recovery = match recover_named_store_quarantine(
                profile_root,
                &data_root,
                std::ffi::OsStr::new(&quarantine_name),
                &projects_dir,
            ) {
                Ok(Some(QuarantineRecoveryOutcome::Restored {
                    restored_path,
                    journal_pending,
                })) => {
                    recovered_project_ids.insert(project_id.clone());
                    Some((
                        projects_dir.join(&quarantine_name),
                        restored_path,
                        if journal_pending {
                            CollectionRecoveryAction::RetainedForRecovery
                        } else {
                            CollectionRecoveryAction::Restored
                        },
                    ))
                }
                Ok(Some(QuarantineRecoveryOutcome::Retained { quarantine_path })) => {
                    recovered_project_ids.insert(project_id.clone());
                    Some((
                        quarantine_path.clone(),
                        quarantine_path,
                        CollectionRecoveryAction::RetainedForRecovery,
                    ))
                }
                Ok(None) => None,
                Err(kind) => {
                    recovery_outcome.errors.push(CollectionFailure {
                        store_id: project_id.clone(),
                        kind,
                    });
                    None
                }
            };
            if let Some((quarantine_path, actual_path, action)) = record_recovery {
                recovery_outcome
                    .recovery_receipts
                    .push(CollectionRecoveryReceipt {
                        store_id: project_id.clone(),
                        original_path: data_root.clone(),
                        actual_path,
                        quarantine_path,
                        action,
                    });
                recovery_outcome.errors.push(CollectionFailure {
                    store_id: project_id,
                    kind: CollectionFailureKind::PayloadChanged,
                });
            }
        }
    }
    Ok(Some((findings, next_cursor)))
}

/// Read one bounded result page through immutable, atomically published
/// directory-inventory chunks. Each build admission persists its source
/// continuation before returning, while result cursors address only immutable
/// chunk records.
pub(in crate::retention) fn read_project_directory_page(
    profile_root: &Path,
    cursor: Option<&str>,
    limit: usize,
    interrupted: &dyn Fn() -> bool,
) -> tracedecay_domain::errors::Result<Option<ProjectDirectoryPageV1>> {
    let projects_dir = profile_root.join("projects");
    let saved = cursor.and_then(parse_portable_directory_cursor);
    let mut entries_scanned = 0usize;
    match saved {
        Some(PortableDirectoryCursor::Build {
            build_id,
            signature,
            previous_start,
        }) => advance_portable_inventory_build(
            profile_root,
            &projects_dir,
            build_id,
            signature,
            previous_start,
            limit,
            &mut entries_scanned,
            interrupted,
        ),
        Some(PortableDirectoryCursor::Page {
            build_id,
            signature,
            chunk_start,
            entry_index,
        }) => read_portable_inventory_page(
            profile_root,
            build_id,
            signature,
            chunk_start,
            entry_index,
            limit,
            entries_scanned,
            interrupted,
        ),
        None => start_portable_inventory_build(
            profile_root,
            &projects_dir,
            limit,
            &mut entries_scanned,
            interrupted,
        ),
    }
}

#[derive(Clone, PartialEq, Eq)]
enum PortableDirectoryCursor {
    Build {
        build_id: String,
        signature: String,
        previous_start: Option<i64>,
    },
    Page {
        build_id: String,
        signature: String,
        chunk_start: i64,
        entry_index: usize,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct PortableInventoryChunk {
    signature: String,
    start_offset: i64,
    entries: Vec<String>,
    next: Option<PortableInventoryResume>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PortableInventoryResume {
    offset: i64,
    anchor: Vec<u8>,
}

enum PortableChunkWrite {
    Written(PortableInventoryChunk),
    Interrupted,
    Invalidated,
}

fn parse_portable_directory_cursor(value: &str) -> Option<PortableDirectoryCursor> {
    let fields = value.split(':').collect::<Vec<_>>();
    match fields.as_slice() {
        ["portable-v3", build_id, signature, "build", "start"] => valid_cursor_component(build_id)
            .then_some(PortableDirectoryCursor::Build {
                build_id: (*build_id).to_owned(),
                signature: (*signature).to_owned(),
                previous_start: None,
            }),
        ["portable-v3", build_id, signature, "build", previous] => valid_cursor_component(build_id)
            .then_some(PortableDirectoryCursor::Build {
                build_id: (*build_id).to_owned(),
                signature: (*signature).to_owned(),
                previous_start: Some(previous.parse().ok()?),
            }),
        ["portable-v3", build_id, signature, "page", chunk, index] => {
            valid_cursor_component(build_id).then_some(PortableDirectoryCursor::Page {
                build_id: (*build_id).to_owned(),
                signature: (*signature).to_owned(),
                chunk_start: chunk.parse().ok()?,
                entry_index: index.parse().ok()?,
            })
        }
        _ => None,
    }
}

fn valid_cursor_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn format_build_cursor(build_id: &str, signature: &str, previous_start: Option<i64>) -> String {
    let previous = previous_start.map_or_else(|| "start".to_owned(), |value| value.to_string());
    format!("portable-v3:{build_id}:{signature}:build:{previous}")
}

fn format_page_cursor(
    build_id: &str,
    signature: &str,
    chunk_start: i64,
    entry_index: usize,
) -> String {
    format!("portable-v3:{build_id}:{signature}:page:{chunk_start}:{entry_index}")
}

fn portable_inventory_root(profile_root: &Path) -> PathBuf {
    profile_root
        .join("maintenance")
        .join("unregistered-project-directory-inventory-v3")
}

fn portable_inventory_build_path(profile_root: &Path, build_id: &str) -> PathBuf {
    portable_inventory_root(profile_root).join(build_id)
}

fn portable_inventory_chunk_path(
    profile_root: &Path,
    build_id: &str,
    start_offset: i64,
) -> PathBuf {
    portable_inventory_build_path(profile_root, build_id).join(format!("chunk-{start_offset}.json"))
}

fn portable_inventory_temporary_path(chunk: &Path) -> tracedecay_domain::errors::Result<PathBuf> {
    let parent =
        chunk
            .parent()
            .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                message: "unregistered inventory chunk has no parent".to_owned(),
            })?;
    let name = chunk
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: "unregistered inventory chunk has no UTF-8 file name".to_owned(),
        })?;
    let sequence = PORTABLE_INVENTORY_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence)))
}

pub(super) fn portable_directory_signature(directory: &Path) -> std::io::Result<String> {
    let metadata = directory.metadata()?;
    let modified = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| std::io::Error::other("project directory timestamp predates epoch"))?;
    Ok(format!(
        "{}-{}-{}",
        modified.as_secs(),
        modified.subsec_nanos(),
        metadata.len()
    ))
}

pub(super) fn portable_inventory_entry_is_valid(name: &str) -> bool {
    quarantined_project_id(name).is_some()
        || tracedecay_runtime_core::storage::validate_project_id(name).is_ok()
}

fn allocate_portable_inventory_build(
    profile_root: &Path,
) -> tracedecay_domain::errors::Result<String> {
    let root = portable_inventory_root(profile_root);
    tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all_durable(&root).map_err(
        |error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("create unregistered inventory root: {error}"),
        },
    )?;
    let timestamp = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("allocate unregistered inventory generation: {error}"),
        })?
        .as_nanos();
    for _ in 0..16 {
        let sequence = PORTABLE_INVENTORY_BUILD_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let build_id = format!("{timestamp:x}-{:x}-{sequence:x}", std::process::id());
        let build_path = portable_inventory_build_path(profile_root, &build_id);
        match tracedecay_runtime_core::storage::PrivateStoreIo::create_private_directory(
            &build_path,
        ) {
            Ok(()) => return Ok(build_id),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("create unregistered inventory generation: {error}"),
                });
            }
        }
    }
    Err(tracedecay_domain::errors::TraceDecayError::Config {
        message: "could not allocate unregistered inventory generation".to_owned(),
    })
}

fn discard_stale_portable_inventory_builds(
    profile_root: &Path,
) -> tracedecay_domain::errors::Result<()> {
    let root_path = portable_inventory_root(profile_root);
    match std::fs::symlink_metadata(&root_path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("inspect unregistered inventory root for cleanup: {error}"),
            });
        }
    }
    let capability = match open_store_directory_nofollow(profile_root, &root_path) {
        Ok(capability) => capability,
        Err(CollectionFailureKind::PayloadChanged) => return Ok(()),
        Err(kind) => {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("open unregistered inventory root for cleanup: {kind:?}"),
            });
        }
    };
    let now = SystemTime::now();
    let mut mutations = 0usize;
    let entries = capability.root.read_dir(".").map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("list unregistered inventory generations for cleanup: {error}"),
        }
    })?;
    for entry in entries.take(PORTABLE_INVENTORY_GC_SCAN_LIMIT) {
        if mutations == PORTABLE_INVENTORY_GC_REMOVE_LIMIT {
            break;
        }
        let entry = entry.map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("inspect unregistered inventory cleanup candidate: {error}"),
        })?;
        let name = entry.file_name();
        let Some(build_id) = name.to_str() else {
            continue;
        };
        if !valid_cursor_component(build_id) {
            continue;
        }
        let metadata = capability.root.symlink_metadata(&name).map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("inspect unregistered inventory generation age: {error}"),
            }
        })?;
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let Ok(age) = now.duration_since(modified.into_std()) else {
            continue;
        };
        if age < PORTABLE_INVENTORY_MAX_IDLE {
            continue;
        }
        let directory = capability.root.open_dir_nofollow(&name).map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("open stale unregistered inventory generation: {error}"),
            }
        })?;
        let mut children = directory.read_dir(".").map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("list stale unregistered inventory generation: {error}"),
            }
        })?;
        let Some(child) = children.next() else {
            drop(children);
            directory.remove_open_dir().map_err(|error| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("remove empty unregistered inventory generation: {error}"),
                }
            })?;
            mutations = mutations.saturating_add(1);
            continue;
        };
        let child = child.map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("inspect stale unregistered inventory artifact: {error}"),
        })?;
        let child_name = child.file_name();
        let child_metadata = directory.symlink_metadata(&child_name).map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("inspect stale unregistered inventory artifact: {error}"),
            }
        })?;
        if child_metadata.is_dir() {
            continue;
        }
        directory.remove_file(&child_name).map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("remove stale unregistered inventory artifact: {error}"),
            }
        })?;
        mutations = mutations.saturating_add(1);
        drop(children);
        let mut remaining = directory.read_dir(".").map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("recheck stale unregistered inventory generation: {error}"),
            }
        })?;
        let is_empty = match remaining.next() {
            None => true,
            Some(Ok(_)) => false,
            Some(Err(error)) => {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("inspect remaining unregistered inventory artifact: {error}"),
                });
            }
        };
        drop(remaining);
        if is_empty && mutations < PORTABLE_INVENTORY_GC_REMOVE_LIMIT {
            tracedecay_private_fs::capability_dir::sync_directory(&directory).map_err(|error| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("sync drained unregistered inventory generation: {error}"),
                }
            })?;
            directory.remove_open_dir().map_err(|error| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("remove drained unregistered inventory generation: {error}"),
                }
            })?;
            mutations = mutations.saturating_add(1);
        } else {
            directory
                .set_mtime(".", SystemTimeSpec::Absolute(modified))
                .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "preserve stale unregistered inventory cleanup continuation: {error}"
                    ),
                })?;
            tracedecay_private_fs::capability_dir::sync_directory(&directory).map_err(|error| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "sync stale unregistered inventory cleanup continuation: {error}"
                    ),
                }
            })?;
        }
    }
    if mutations > 0 {
        tracedecay_private_fs::capability_dir::sync_directory(&capability.root).map_err(
            |error| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("sync unregistered inventory cleanup: {error}"),
            },
        )?;
    }
    Ok(())
}

fn start_portable_inventory_build(
    profile_root: &Path,
    projects_dir: &Path,
    limit: usize,
    entries_scanned: &mut usize,
    interrupted: &dyn Fn() -> bool,
) -> tracedecay_domain::errors::Result<Option<ProjectDirectoryPageV1>> {
    discard_stale_portable_inventory_builds(profile_root)?;
    let signature = match portable_directory_signature(projects_dir) {
        Ok(signature) => signature,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some(ProjectDirectoryPageV1 {
                entries: Vec::new(),
                next_cursor: None,
                entries_scanned: *entries_scanned,
            }));
        }
        Err(error) => {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("inspect unregistered project-directory page: {error}"),
            });
        }
    };
    let build_id = allocate_portable_inventory_build(profile_root)?;
    advance_portable_inventory_build(
        profile_root,
        projects_dir,
        build_id,
        signature,
        None,
        limit,
        entries_scanned,
        interrupted,
    )
}

fn restart_portable_inventory_build(
    profile_root: &Path,
    projects_dir: &Path,
    entries_scanned: usize,
) -> tracedecay_domain::errors::Result<Option<ProjectDirectoryPageV1>> {
    let signature = portable_directory_signature(projects_dir).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("restart unregistered project-directory inventory: {error}"),
        }
    })?;
    let build_id = allocate_portable_inventory_build(profile_root)?;
    Ok(Some(ProjectDirectoryPageV1 {
        entries: Vec::new(),
        next_cursor: Some(format_build_cursor(&build_id, &signature, None)),
        entries_scanned,
    }))
}

#[allow(clippy::too_many_arguments)]
fn advance_portable_inventory_build(
    profile_root: &Path,
    projects_dir: &Path,
    build_id: String,
    signature: String,
    previous_start: Option<i64>,
    limit: usize,
    entries_scanned: &mut usize,
    interrupted: &dyn Fn() -> bool,
) -> tracedecay_domain::errors::Result<Option<ProjectDirectoryPageV1>> {
    let current_signature = match portable_directory_signature(projects_dir) {
        Ok(signature) => signature,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some(ProjectDirectoryPageV1 {
                entries: Vec::new(),
                next_cursor: None,
                entries_scanned: *entries_scanned,
            }));
        }
        Err(error) => {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("inspect unregistered inventory continuation: {error}"),
            });
        }
    };
    if current_signature != signature {
        return restart_portable_inventory_build(profile_root, projects_dir, *entries_scanned);
    }
    let (start_offset, anchor) = if let Some(previous_start) = previous_start {
        let previous = match read_portable_inventory_chunk(
            profile_root,
            &build_id,
            &signature,
            previous_start,
        ) {
            Ok(chunk) => chunk,
            Err(_) => {
                return restart_portable_inventory_build(
                    profile_root,
                    projects_dir,
                    *entries_scanned,
                );
            }
        };
        let Some(next) = previous.next else {
            return read_portable_inventory_page(
                profile_root,
                build_id,
                signature,
                0,
                0,
                limit,
                *entries_scanned,
                interrupted,
            );
        };
        (next.offset, Some(next.anchor))
    } else {
        (0, None)
    };
    let raw_limit = limit
        .saturating_mul(UNREGISTERED_STORE_DIRECTORY_ENTRY_MULTIPLIER)
        .max(2);
    let write = write_portable_inventory_chunk(
        profile_root,
        projects_dir,
        &build_id,
        &signature,
        start_offset,
        anchor.as_deref(),
        raw_limit,
        entries_scanned,
        interrupted,
    );
    let write = write?;
    match write {
        PortableChunkWrite::Interrupted => Ok(None),
        PortableChunkWrite::Invalidated => {
            restart_portable_inventory_build(profile_root, projects_dir, *entries_scanned)
        }
        PortableChunkWrite::Written(chunk) if chunk.next.is_some() => {
            Ok(Some(ProjectDirectoryPageV1 {
                entries: Vec::new(),
                next_cursor: Some(format_build_cursor(
                    &build_id,
                    &signature,
                    Some(chunk.start_offset),
                )),
                entries_scanned: *entries_scanned,
            }))
        }
        PortableChunkWrite::Written(_) => read_portable_inventory_page(
            profile_root,
            build_id,
            signature,
            0,
            0,
            limit,
            *entries_scanned,
            interrupted,
        ),
    }
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn write_portable_inventory_chunk(
    profile_root: &Path,
    projects_dir: &Path,
    build_id: &str,
    signature: &str,
    start_offset: i64,
    anchor: Option<&[u8]>,
    raw_limit: usize,
    entries_scanned: &mut usize,
    interrupted: &dyn Fn() -> bool,
) -> tracedecay_domain::errors::Result<PortableChunkWrite> {
    use std::os::fd::AsFd;

    let directory = open_store_directory_nofollow(profile_root, projects_dir).map_err(|kind| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("open unregistered project-directory inventory: {kind:?}"),
        }
    })?;
    let readable = directory.root.open(".").map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("open readable project-directory inventory stream: {error}"),
        }
    })?;
    let mut stream = rustix::fs::Dir::read_from(readable.as_fd()).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("open unregistered project-directory inventory stream: {error}"),
        }
    })?;
    if start_offset > 0 && stream.seek(start_offset).is_err() {
        return Ok(PortableChunkWrite::Invalidated);
    }
    let mut entries = Vec::new();
    let mut resume_offset = start_offset;
    let mut consumed = 0usize;
    if let Some(expected_anchor) = anchor {
        if interrupted() {
            return Ok(PortableChunkWrite::Interrupted);
        }
        let Some(entry) = read_rustix_directory_entry(&mut stream)? else {
            return Ok(PortableChunkWrite::Invalidated);
        };
        *entries_scanned = entries_scanned.saturating_add(1);
        consumed = consumed.saturating_add(1);
        if entry.0 != expected_anchor {
            return Ok(PortableChunkWrite::Invalidated);
        }
        resume_offset = entry.1;
        collect_portable_inventory_name(&entry.0, &mut entries);
    }
    let entry_limit = raw_limit.saturating_sub(1);
    let mut reached_end = false;
    while consumed < entry_limit {
        if interrupted() {
            return Ok(PortableChunkWrite::Interrupted);
        }
        let Some(entry) = read_rustix_directory_entry(&mut stream)? else {
            reached_end = true;
            break;
        };
        *entries_scanned = entries_scanned.saturating_add(1);
        consumed = consumed.saturating_add(1);
        resume_offset = entry.1;
        collect_portable_inventory_name(&entry.0, &mut entries);
    }
    let next = if reached_end {
        None
    } else {
        if interrupted() {
            return Ok(PortableChunkWrite::Interrupted);
        }
        match read_rustix_directory_entry(&mut stream)? {
            Some((anchor, _)) => {
                *entries_scanned = entries_scanned.saturating_add(1);
                Some(PortableInventoryResume {
                    offset: resume_offset,
                    anchor,
                })
            }
            None => None,
        }
    };
    if portable_directory_signature(projects_dir).ok().as_deref() != Some(signature) {
        return Ok(PortableChunkWrite::Invalidated);
    }
    let chunk = PortableInventoryChunk {
        signature: signature.to_owned(),
        start_offset,
        entries,
        next,
    };
    let chunk = write_portable_inventory_chunk_atomically(profile_root, build_id, chunk)?;
    Ok(PortableChunkWrite::Written(chunk))
}

#[cfg(unix)]
fn read_rustix_directory_entry(
    stream: &mut rustix::fs::Dir,
) -> tracedecay_domain::errors::Result<Option<(Vec<u8>, i64)>> {
    match stream.read() {
        Some(Ok(entry)) if entry.offset() >= 0 => Ok(Some((
            entry.file_name().to_bytes().to_vec(),
            entry.offset(),
        ))),
        Some(Ok(_)) => Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "project-directory inventory returned a negative offset".to_owned(),
        }),
        Some(Err(error)) => Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("read unregistered project-directory inventory stream: {error}"),
        }),
        None => Ok(None),
    }
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn write_portable_inventory_chunk(
    profile_root: &Path,
    projects_dir: &Path,
    build_id: &str,
    signature: &str,
    start_offset: i64,
    anchor: Option<&[u8]>,
    raw_limit: usize,
    entries_scanned: &mut usize,
    interrupted: &dyn Fn() -> bool,
) -> tracedecay_domain::errors::Result<PortableChunkWrite> {
    let directory = open_store_directory_nofollow(profile_root, projects_dir).map_err(|kind| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("open unregistered project-directory inventory: {kind:?}"),
        }
    })?;
    let prefix = match (start_offset, anchor) {
        (0, None) => String::new(),
        (offset, Some(anchor)) if offset > 0 => {
            let prefix = std::str::from_utf8(anchor).map_err(|_| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: "Windows inventory prefix is not UTF-8".to_owned(),
                }
            })?;
            prefix.to_owned()
        }
        _ => return Ok(PortableChunkWrite::Invalidated),
    };
    if interrupted() {
        return Ok(PortableChunkWrite::Interrupted);
    }
    let alphabet = windows_inventory_alphabet(&directory.root)?;
    if !prefix.is_empty() && !windows_inventory_prefix_is_valid(&prefix, &alphabet) {
        return Ok(PortableChunkWrite::Invalidated);
    }
    let pattern = format!("{prefix}*");
    let matched = windows_query_directory_names(&directory.root, &pattern, raw_limit)?;
    *entries_scanned = entries_scanned.saturating_add(matched.entries_read);
    if interrupted() {
        return Ok(PortableChunkWrite::Interrupted);
    }
    let exact = if matched.entries_read == raw_limit && !prefix.is_empty() {
        let exact = windows_query_directory_names(&directory.root, &prefix, 1)?;
        *entries_scanned = entries_scanned.saturating_add(exact.entries_read);
        exact.names
    } else {
        Vec::new()
    };
    let (names, next_prefix) = windows_partition_inventory_node(
        &prefix,
        matched.names,
        matched.entries_read,
        exact,
        raw_limit,
        &alphabet,
    )?;
    let mut entries = Vec::new();
    for name in names {
        collect_portable_inventory_name(name.as_bytes(), &mut entries);
    }
    if portable_directory_signature(projects_dir).ok().as_deref() != Some(signature) {
        return Ok(PortableChunkWrite::Invalidated);
    }
    let next = next_prefix.map(|prefix| PortableInventoryResume {
        offset: start_offset.saturating_add(1),
        anchor: prefix.into_bytes(),
    });
    let chunk = PortableInventoryChunk {
        signature: signature.to_owned(),
        start_offset,
        entries,
        next,
    };
    let chunk = write_portable_inventory_chunk_atomically(profile_root, build_id, chunk)?;
    Ok(PortableChunkWrite::Written(chunk))
}

#[cfg(windows)]
fn windows_inventory_alphabet(
    directory: &cap_std::fs::Dir,
) -> tracedecay_domain::errors::Result<Vec<char>> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_CASE_SENSITIVE_INFO, FileCaseSensitiveInfo, GetFileInformationByHandleEx,
    };
    use windows_sys::Win32::System::SystemServices::FILE_CS_FLAG_CASE_SENSITIVE_DIR;

    let mut info = FILE_CASE_SENSITIVE_INFO::default();
    // SAFETY: `directory` is a live directory handle and `info` is the exact
    // output structure selected by `FileCaseSensitiveInfo`.
    let queried = unsafe {
        GetFileInformationByHandleEx(
            directory.as_raw_handle(),
            FileCaseSensitiveInfo,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of_val(&info) as u32,
        )
    };
    if queried == 0 {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "inspect Windows project-directory case authority: {}",
                std::io::Error::last_os_error()
            ),
        });
    }
    let case_sensitive = info.Flags & FILE_CS_FLAG_CASE_SENSITIVE_DIR != 0;
    let mut alphabet = "-.0123456789_abcdefghijklmnopqrstuvwxyz"
        .chars()
        .collect::<Vec<_>>();
    if case_sensitive {
        alphabet.extend('A'..='Z');
    }
    Ok(alphabet)
}

#[cfg(any(test, windows))]
pub(super) fn windows_inventory_prefix_is_valid(prefix: &str, alphabet: &[char]) -> bool {
    !prefix.is_empty()
        && prefix.len() <= 255
        && prefix
            .chars()
            .all(|character| alphabet.contains(&character))
}

#[cfg(any(test, windows))]
fn windows_next_inventory_prefix(prefix: &str, alphabet: &[char]) -> Option<String> {
    let mut chars = prefix.chars().collect::<Vec<_>>();
    while let Some(last) = chars.pop() {
        let position = alphabet.iter().position(|candidate| *candidate == last)?;
        if let Some(next) = alphabet.get(position.saturating_add(1)) {
            chars.push(*next);
            return Some(chars.into_iter().collect());
        }
    }
    None
}

#[cfg(any(test, windows))]
pub(super) fn windows_partition_inventory_node(
    prefix: &str,
    matched: Vec<String>,
    matched_entries: usize,
    exact: Vec<String>,
    raw_limit: usize,
    alphabet: &[char],
) -> tracedecay_domain::errors::Result<(Vec<String>, Option<String>)> {
    if matched_entries < raw_limit {
        return Ok((matched, windows_next_inventory_prefix(prefix, alphabet)));
    }
    if prefix.len() >= 255 {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "Windows project-directory prefix cannot be partitioned further".to_owned(),
        });
    }
    let Some(first) = alphabet.first() else {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "Windows project-directory prefix alphabet is empty".to_owned(),
        });
    };
    Ok((exact, Some(format!("{prefix}{first}"))))
}

#[cfg(any(test, windows))]
pub(super) fn windows_directory_name_fits(
    fixed_bytes: usize,
    name_bytes: usize,
    information_bytes: usize,
) -> bool {
    name_bytes.is_multiple_of(2)
        && fixed_bytes
            .checked_add(name_bytes)
            .is_some_and(|end| end <= information_bytes)
}

#[cfg(windows)]
struct WindowsDirectoryNames {
    names: Vec<String>,
    entries_read: usize,
}

#[cfg(windows)]
fn windows_query_directory_names(
    directory: &cap_std::fs::Dir,
    pattern: &str,
    limit: usize,
) -> tracedecay_domain::errors::Result<WindowsDirectoryNames> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_NAMES_INFORMATION, FileNamesInformation, NtQueryDirectoryFile,
    };
    use windows_sys::Win32::Foundation::{STATUS_NO_MORE_FILES, UNICODE_STRING};
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    let readable = directory.open_dir(".").map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("open Windows project-directory query handle: {error}"),
        }
    })?;
    let mut pattern_utf16 = pattern.encode_utf16().collect::<Vec<_>>();
    let pattern_bytes = pattern_utf16.len().checked_mul(2).ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "Windows project-directory query pattern is too long".to_owned(),
        }
    })?;
    let pattern_length = u16::try_from(pattern_bytes).map_err(|_| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "Windows project-directory query pattern is too long".to_owned(),
        }
    })?;
    let pattern = UNICODE_STRING {
        Length: pattern_length,
        MaximumLength: pattern_length,
        Buffer: pattern_utf16.as_mut_ptr(),
    };
    let mut names = Vec::with_capacity(limit);
    let mut entries_read = 0usize;
    let mut first = true;
    while entries_read < limit {
        let mut status = IO_STATUS_BLOCK::default();
        let mut buffer = [0_u64; 128];
        // SAFETY: all pointers reference live, correctly aligned buffers for
        // the duration of this synchronous call. The first call supplies the
        // stable UTF-16 search expression; subsequent calls resume the same
        // handle and intentionally pass no replacement expression.
        let result = unsafe {
            NtQueryDirectoryFile(
                readable.as_raw_handle(),
                std::ptr::null_mut(),
                None,
                std::ptr::null(),
                &mut status,
                buffer.as_mut_ptr().cast(),
                std::mem::size_of_val(&buffer) as u32,
                FileNamesInformation,
                true,
                if first { &pattern } else { std::ptr::null() },
                first,
            )
        };
        first = false;
        if result == STATUS_NO_MORE_FILES {
            break;
        }
        if result < 0 {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("query Windows project-directory entries: NTSTATUS {result:#x}"),
            });
        }
        entries_read = entries_read.saturating_add(1);
        let fixed = std::mem::offset_of!(FILE_NAMES_INFORMATION, FileName);
        if status.Information < fixed {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "Windows project-directory query returned a truncated entry".to_owned(),
            });
        }
        // SAFETY: the successful query returned at least the fixed structure;
        // the variable UTF-16 tail is bounded by `status.Information` below.
        let entry = unsafe { &*buffer.as_ptr().cast::<FILE_NAMES_INFORMATION>() };
        let name_bytes = entry.FileNameLength as usize;
        if !windows_directory_name_fits(fixed, name_bytes, status.Information) {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "Windows project-directory query returned an invalid name length"
                    .to_owned(),
            });
        }
        // SAFETY: `FileNameLength` is byte-counted UTF-16 and the bounds above
        // prove the slice stays within the initialized response buffer.
        let name = unsafe { std::slice::from_raw_parts(entry.FileName.as_ptr(), name_bytes / 2) };
        if let Ok(name) = String::from_utf16(name) {
            names.push(name);
        }
    }
    Ok(WindowsDirectoryNames {
        names,
        entries_read,
    })
}

#[cfg(not(any(unix, windows)))]
#[allow(clippy::too_many_arguments)]
/// Platforms without a maintained restartable directory-stream primitive may
/// complete a small snapshot, but must fail after one bounded probe rather
/// than hide an unbounded scan or issue a cursor that can never converge.
fn write_portable_inventory_chunk(
    profile_root: &Path,
    projects_dir: &Path,
    build_id: &str,
    signature: &str,
    start_offset: i64,
    anchor: Option<&[u8]>,
    raw_limit: usize,
    entries_scanned: &mut usize,
    interrupted: &dyn Fn() -> bool,
) -> tracedecay_domain::errors::Result<PortableChunkWrite> {
    if start_offset != 0 || anchor.is_some() {
        return Ok(PortableChunkWrite::Invalidated);
    }
    let directory = open_store_directory_nofollow(profile_root, projects_dir).map_err(|kind| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("open unregistered project-directory inventory: {kind:?}"),
        }
    })?;
    let source = directory.root.read_dir(".").map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("open unregistered project-directory inventory stream: {error}"),
        }
    })?;
    let mut entries = Vec::new();
    for entry in source.take(raw_limit.saturating_add(1)) {
        if interrupted() {
            return Ok(PortableChunkWrite::Interrupted);
        }
        *entries_scanned = entries_scanned.saturating_add(1);
        if *entries_scanned > raw_limit {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message:
                    "bounded resumable project-directory iteration is unavailable on this platform"
                        .to_owned(),
            });
        }
        let Ok(entry) = entry else {
            continue;
        };
        if let Ok(name) = entry.file_name().into_string() {
            collect_portable_inventory_name(name.as_bytes(), &mut entries);
        }
    }
    if portable_directory_signature(projects_dir).ok().as_deref() != Some(signature) {
        return Ok(PortableChunkWrite::Invalidated);
    }
    let chunk = PortableInventoryChunk {
        signature: signature.to_owned(),
        start_offset,
        entries,
        next: None,
    };
    let chunk = write_portable_inventory_chunk_atomically(profile_root, build_id, chunk)?;
    Ok(PortableChunkWrite::Written(chunk))
}

fn collect_portable_inventory_name(bytes: &[u8], entries: &mut Vec<String>) {
    let Ok(name) = std::str::from_utf8(bytes) else {
        return;
    };
    if portable_inventory_entry_is_valid(name) {
        entries.push(name.to_owned());
    }
}

fn write_portable_inventory_chunk_atomically(
    profile_root: &Path,
    build_id: &str,
    chunk: PortableInventoryChunk,
) -> tracedecay_domain::errors::Result<PortableInventoryChunk> {
    use std::io::Write;

    let path = portable_inventory_chunk_path(profile_root, build_id, chunk.start_offset);
    let bytes = serde_json::to_vec(&chunk).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("serialize unregistered inventory chunk: {error}"),
        }
    })?;
    let temporary = portable_inventory_temporary_path(&path)?;
    let build_path = portable_inventory_build_path(profile_root, build_id);
    let capability = open_store_directory_nofollow(profile_root, &build_path).map_err(|kind| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("open unregistered inventory generation for publication: {kind:?}"),
        }
    })?;
    let temporary_name = temporary.file_name().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "unregistered inventory temporary file has no name".to_owned(),
        }
    })?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut temporary_file = capability
        .root
        .open_with(temporary_name, &options)
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("create unregistered inventory chunk staging file: {error}"),
        })?;
    temporary_file
        .write_all(&bytes)
        .and_then(|()| temporary_file.sync_all())
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("durably stage unregistered inventory chunk: {error}"),
        })?;
    drop(temporary_file);
    let target_name =
        path.file_name()
            .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                message: "unregistered inventory chunk has no name".to_owned(),
            })?;
    match tracedecay_private_fs::capability_dir::rename_noreplace(
        &capability.root,
        temporary_name,
        &capability.root,
        target_name,
    ) {
        Ok(()) => {
            tracedecay_private_fs::capability_dir::sync_directory(&capability.root).map_err(
                |error| tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("sync published unregistered inventory chunk: {error}"),
                },
            )?;
            Ok(chunk)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            capability
                .root
                .remove_file(temporary_name)
                .and_then(|()| {
                    tracedecay_private_fs::capability_dir::sync_directory(&capability.root)
                })
                .map_err(
                    |cleanup_error| tracedecay_domain::errors::TraceDecayError::Config {
                        message: format!(
                            "discard duplicate unregistered inventory chunk: {cleanup_error}"
                        ),
                    },
                )?;
            read_portable_inventory_chunk(
                profile_root,
                build_id,
                &chunk.signature,
                chunk.start_offset,
            )
        }
        Err(error) => {
            let cleanup = capability.root.remove_file(temporary_name);
            if let Err(cleanup_error) = cleanup {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "publish unregistered inventory chunk: {error}; cleanup failed: {cleanup_error}"
                    ),
                });
            }
            Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("publish unregistered inventory chunk: {error}"),
            })
        }
    }
}

fn read_portable_inventory_chunk(
    profile_root: &Path,
    build_id: &str,
    signature: &str,
    start_offset: i64,
) -> tracedecay_domain::errors::Result<PortableInventoryChunk> {
    let build_path = portable_inventory_build_path(profile_root, build_id);
    let capability = open_store_directory_nofollow(profile_root, &build_path).map_err(|kind| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("open unregistered inventory generation: {kind:?}"),
        }
    })?;
    let name = format!("chunk-{start_offset}.json");
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = capability
        .root
        .open_with(&name, &options)
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("open unregistered inventory chunk: {error}"),
        })?;
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "unregistered inventory chunk is not a regular file".to_owned(),
        });
    }
    let chunk: PortableInventoryChunk = serde_json::from_reader(file).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("decode unregistered inventory chunk: {error}"),
        }
    })?;
    if chunk.signature != signature
        || chunk.start_offset != start_offset
        || chunk
            .entries
            .iter()
            .any(|name| !portable_inventory_entry_is_valid(name))
        || chunk
            .next
            .as_ref()
            .is_some_and(|next| next.offset <= chunk.start_offset)
    {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "unregistered inventory chunk identity is invalid".to_owned(),
        });
    }
    Ok(chunk)
}

#[allow(clippy::too_many_arguments)]
fn read_portable_inventory_page(
    profile_root: &Path,
    build_id: String,
    signature: String,
    requested_chunk_start: i64,
    requested_entry_index: usize,
    limit: usize,
    mut entries_scanned: usize,
    interrupted: &dyn Fn() -> bool,
) -> tracedecay_domain::errors::Result<Option<ProjectDirectoryPageV1>> {
    let requested =
        read_portable_inventory_chunk(profile_root, &build_id, &signature, requested_chunk_start);
    let (mut chunk_start, mut entry_index, mut chunk) = match requested {
        Ok(chunk)
            if requested_entry_index < chunk.entries.len()
                || (requested_entry_index == 0 && chunk.entries.is_empty()) =>
        {
            (requested_chunk_start, requested_entry_index, chunk)
        }
        Ok(_) if requested_chunk_start != 0 || requested_entry_index != 0 => (
            0,
            0,
            read_portable_inventory_chunk(profile_root, &build_id, &signature, 0)?,
        ),
        Ok(chunk) => (0, 0, chunk),
        Err(error) => return Err(error),
    };
    let mut work = Vec::with_capacity(limit);
    loop {
        while entry_index < chunk.entries.len() && work.len() < limit {
            if interrupted() {
                return Ok(None);
            }
            let name = chunk.entries[entry_index].clone();
            entry_index = entry_index.saturating_add(1);
            entries_scanned = entries_scanned.saturating_add(1);
            if let Some(project_id) = quarantined_project_id(&name) {
                work.push(ProjectDirectoryWorkV1::Quarantine {
                    project_id,
                    quarantine_name: name,
                });
            } else {
                work.push(ProjectDirectoryWorkV1::Project(name));
            }
        }
        if work.len() == limit {
            let next_cursor = if entry_index < chunk.entries.len() {
                Some(format_page_cursor(
                    &build_id,
                    &signature,
                    chunk_start,
                    entry_index,
                ))
            } else {
                chunk
                    .next
                    .as_ref()
                    .map(|next| format_page_cursor(&build_id, &signature, next.offset, 0))
            };
            return Ok(Some(ProjectDirectoryPageV1 {
                entries: work,
                next_cursor,
                entries_scanned,
            }));
        }
        let Some(next) = &chunk.next else {
            return Ok(Some(ProjectDirectoryPageV1 {
                entries: work,
                next_cursor: None,
                entries_scanned,
            }));
        };
        chunk_start = next.offset;
        entry_index = 0;
        chunk = read_portable_inventory_chunk(profile_root, &build_id, &signature, chunk_start)?;
    }
}

static PORTABLE_INVENTORY_BUILD_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static PORTABLE_INVENTORY_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);
