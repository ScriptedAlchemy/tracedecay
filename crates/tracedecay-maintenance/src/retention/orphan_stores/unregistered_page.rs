//! Bounded, resumable census and collection of unregistered project leaves.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};

use super::fence::{
    capture_store_content_fence_controlled, capture_store_directory_fence,
    open_store_directory_nofollow,
};
use super::quarantine::{
    QuarantineRecoveryOutcome, quarantine_recovery_entry, recover_named_store_quarantine,
};
use super::{
    CollectionCompletionV1, CollectionControl, CollectionFailure, CollectionFailureKind,
    CollectionOutcome, CollectionRecoveryAction, CollectionRecoveryReceipt, StoreContentFence,
    StoreDirectoryFence, UnregisteredCollectionPlan, UnregisteredStoreFinding,
    dir_size_bytes_controlled, execute_unregistered_collection_controlled,
    newest_mtime_secs_controlled, plan_unregistered_collection,
};

pub const DEFAULT_UNREGISTERED_STORE_PAGE_LIMIT: usize = 8;
const MAX_UNREGISTERED_STORE_PAGE_LIMIT: usize = 64;
const UNREGISTERED_STORE_DIRECTORY_ENTRY_MULTIPLIER: usize = 8;

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
            if recovered_project_ids.contains(&project_id) {
                continue;
            }
            let data_root = projects_dir.join(&project_id);
            let record_recovery = match recover_named_store_quarantine(
                profile_root,
                &data_root,
                std::ffi::OsStr::new(&quarantine_name),
                &projects_dir,
                CollectionControl::new(cancellation, deadline),
            ) {
                Ok(Some(QuarantineRecoveryOutcome::Removed {
                    journal_failure, ..
                })) => {
                    recovered_project_ids.insert(project_id.clone());
                    if let Some(failure) = journal_failure {
                        recovery_outcome.errors.push(CollectionFailure {
                            store_id: project_id.clone(),
                            kind: CollectionFailureKind::RemoveFailed(failure),
                        });
                    }
                    None
                }
                Ok(Some(QuarantineRecoveryOutcome::Restored {
                    restored_path,
                    failure,
                })) => {
                    recovered_project_ids.insert(project_id.clone());
                    Some((
                        projects_dir.join(&quarantine_name),
                        restored_path,
                        if failure.is_some() {
                            CollectionRecoveryAction::RetainedForRecovery
                        } else {
                            CollectionRecoveryAction::Restored
                        },
                        failure,
                    ))
                }
                Ok(Some(QuarantineRecoveryOutcome::Retained {
                    quarantine_path,
                    actual_path,
                    failure,
                })) => {
                    recovered_project_ids.insert(project_id.clone());
                    Some((
                        quarantine_path.clone(),
                        actual_path,
                        CollectionRecoveryAction::RetainedForRecovery,
                        failure,
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
            if let Some((quarantine_path, actual_path, action, failure)) = record_recovery {
                recovery_outcome
                    .recovery_receipts
                    .push(CollectionRecoveryReceipt {
                        store_id: project_id.clone(),
                        original_path: data_root.clone(),
                        actual_path,
                        quarantine_path,
                        action,
                    });
                if let Some(failure) = failure {
                    recovery_outcome.errors.push(CollectionFailure {
                        store_id: project_id.clone(),
                        kind: CollectionFailureKind::RemoveFailed(failure),
                    });
                }
                recovery_outcome.errors.push(CollectionFailure {
                    store_id: project_id,
                    kind: CollectionFailureKind::PayloadChanged,
                });
            }
        }
    }
    // Directory inventories preserve discovery order. A live leaf can precede
    // its quarantine; recovery invalidates that earlier census just as it
    // prevents a later one, so neither may reach collection in this admission.
    findings.retain(|finding| !recovered_project_ids.contains(&finding.project_dir_name));
    Ok(Some((findings, next_cursor)))
}

/// Resume a durable inventory byte position, never an operating-system directory
/// cookie. The maintained directory iterator stays open while building; after
/// restart its bounded replay deduplicates against committed inventory records.
pub(in crate::retention) fn read_project_directory_page(
    profile_root: &Path,
    cursor: Option<&str>,
    limit: usize,
    interrupted: &dyn Fn() -> bool,
) -> tracedecay_domain::errors::Result<Option<ProjectDirectoryPageV1>> {
    let projects_dir = profile_root.join("projects");
    let saved = cursor.and_then(parse_portable_directory_cursor);
    // Page application mutates `projects/` by removing its own prior entries.
    // A resume cursor therefore binds to a durable append-only inventory, not
    // current directory metadata; otherwise every successful page would
    // invalidate the next page and re-scan the whole directory.
    let (directory_signature, inventory, start) = if let Some(saved) = saved {
        let inventory = portable_inventory_path(profile_root, &saved.signature);
        if portable_inventory_matches(&inventory, &saved.signature) {
            (saved.signature, inventory, saved.offset)
        } else {
            let signature = match portable_directory_signature(&projects_dir) {
                Ok(signature) => signature,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Some(ProjectDirectoryPageV1 {
                        entries: Vec::new(),
                        next_cursor: None,
                        entries_scanned: 0,
                    }));
                }
                Err(error) => {
                    return Err(tracedecay_domain::errors::TraceDecayError::Config {
                        message: format!("inspect unregistered project-directory page: {error}"),
                    });
                }
            };
            let inventory = portable_inventory_path(profile_root, &signature);
            (signature, inventory, 0)
        }
    } else {
        let signature = match portable_directory_signature(&projects_dir) {
            Ok(signature) => signature,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Some(ProjectDirectoryPageV1 {
                    entries: Vec::new(),
                    next_cursor: None,
                    entries_scanned: 0,
                }));
            }
            Err(error) => {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("inspect unregistered project-directory page: {error}"),
                });
            }
        };
        let inventory = portable_inventory_path(profile_root, &signature);
        (signature, inventory, 0)
    };
    let mut entries_scanned = 0usize;
    let build_complete = match advance_portable_inventory(
        &projects_dir,
        &inventory,
        &directory_signature,
        limit.saturating_mul(UNREGISTERED_STORE_DIRECTORY_ENTRY_MULTIPLIER),
        &mut entries_scanned,
        interrupted,
    )? {
        Some(complete) => complete,
        None => return Ok(None),
    };
    // A second process may observe the sidecar writer lock before its holder
    // has atomically published the first inventory header. Preserve the same
    // opaque cursor instead of opening a missing log and falsely reporting a
    // complete empty page; maintenance and Doctor will retry this exact slice.
    if !build_complete && !inventory.exists() {
        return Ok(Some(ProjectDirectoryPageV1 {
            entries: Vec::new(),
            next_cursor: Some(format_portable_directory_cursor(directory_signature, start)),
            entries_scanned,
        }));
    }
    let file = match std::fs::File::open(&inventory) {
        Ok(file) => file,
        Err(error) => {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("open unregistered project-directory page: {error}"),
            });
        }
    };
    let mut reader = std::io::BufReader::new(file);
    use std::io::{BufRead, Read, Seek, SeekFrom};
    if start == 0 {
        let mut header = String::new();
        reader.read_line(&mut header).map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("read unregistered inventory header: {error}"),
            }
        })?;
    } else {
        let length = reader
            .get_ref()
            .metadata()
            .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("inspect unregistered inventory cursor boundary: {error}"),
            })?
            .len();
        let header_length = portable_inventory_header(&directory_signature).len() as u64;
        if start < header_length || start > length {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "unregistered inventory cursor is outside committed records".to_owned(),
            });
        }
        reader.seek(SeekFrom::Start(start - 1)).map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("seek unregistered inventory record boundary: {error}"),
            }
        })?;
        let mut boundary = [0];
        reader.read_exact(&mut boundary).map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("read unregistered inventory record boundary: {error}"),
            }
        })?;
        if boundary != *b"\n" {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "unregistered inventory cursor splits a committed record".to_owned(),
            });
        }
        reader.seek(SeekFrom::Start(start)).map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("seek unregistered inventory cursor: {error}"),
            }
        })?;
    }
    let mut work = Vec::with_capacity(limit);
    let scan_limit = limit.saturating_mul(UNREGISTERED_STORE_DIRECTORY_ENTRY_MULTIPLIER);
    let mut scanned = 0usize;
    loop {
        if interrupted() {
            return Ok(None);
        }
        let mut name = String::new();
        let bytes = reader.read_line(&mut name).map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("read unregistered inventory page: {error}"),
            }
        })?;
        if bytes == 0 {
            let next_cursor = if build_complete {
                None
            } else {
                let offset = reader.stream_position().map_err(|error| {
                    tracedecay_domain::errors::TraceDecayError::Config {
                        message: format!("checkpoint incomplete unregistered inventory: {error}"),
                    }
                })?;
                Some(format_portable_directory_cursor(
                    directory_signature,
                    offset,
                ))
            };
            return Ok(Some(ProjectDirectoryPageV1 {
                entries: work,
                next_cursor,
                entries_scanned,
            }));
        }
        let name = name.trim_end_matches(['\r', '\n']).to_owned();
        let next_offset = reader.stream_position().map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("checkpoint unregistered inventory cursor: {error}"),
            }
        })?;
        scanned = scanned.saturating_add(1);
        entries_scanned = entries_scanned.saturating_add(1);
        if let Some((project_id, quarantine_name)) = quarantine_recovery_entry(&name) {
            work.push(ProjectDirectoryWorkV1::Quarantine {
                project_id,
                quarantine_name,
            });
        } else if tracedecay_runtime_core::storage::validate_project_id(&name).is_ok() {
            work.push(ProjectDirectoryWorkV1::Project(name));
        }
        if work.len() == limit || scanned >= scan_limit {
            return Ok(Some(ProjectDirectoryPageV1 {
                entries: work,
                next_cursor: Some(format_portable_directory_cursor(
                    directory_signature,
                    next_offset,
                )),
                entries_scanned,
            }));
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct PortableDirectoryCursor {
    signature: String,
    offset: u64,
}

pub(super) fn portable_inventory_path(profile_root: &Path, signature: &str) -> std::path::PathBuf {
    profile_root
        .join("maintenance")
        .join("unregistered-project-directory-inventory-v2")
        .join(format!("{signature}.log"))
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

fn parse_portable_directory_cursor(value: &str) -> Option<PortableDirectoryCursor> {
    let mut fields = value.split(':');
    let version = fields.next()?;
    let signature = fields.next()?;
    let offset = fields.next()?;
    (version == "portable-v2").then_some(())?;
    fields.next().is_none().then_some(())?;
    // Signatures become file names; accept only the numeric metadata encoding.
    let parts = signature.split('-').collect::<Vec<_>>();
    (parts.len() == 3 && parts.iter().all(|part| part.parse::<u64>().is_ok())).then_some(())?;
    Some(PortableDirectoryCursor {
        signature: signature.to_owned(),
        offset: offset.parse().ok()?,
    })
}

fn format_portable_directory_cursor(signature: String, offset: u64) -> String {
    format!("portable-v2:{signature}:{offset}")
}

const PORTABLE_INVENTORY_TAIL_RECOVERY_BYTES: u64 = 64 * 1024;

fn portable_inventory_header(signature: &str) -> String {
    format!("v2:{signature}\n")
}

fn portable_inventory_matches(path: &Path, signature: &str) -> bool {
    use std::io::Read;

    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut reader = std::io::BufReader::new(file);
    let expected = portable_inventory_header(signature).into_bytes();
    let mut header = vec![0; expected.len()];
    reader.read_exact(&mut header).is_ok() && header == expected
}

pub(super) fn portable_inventory_entry_is_valid(name: &str) -> bool {
    quarantine_recovery_entry(name).is_some()
        || tracedecay_runtime_core::storage::validate_project_id(name).is_ok()
}

fn clear_portable_inventory_complete(inventory: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(portable_inventory_complete_path(inventory)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn portable_inventory_temporary_path(
    inventory: &Path,
) -> tracedecay_domain::errors::Result<std::path::PathBuf> {
    let parent =
        inventory
            .parent()
            .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                message: "unregistered inventory has no parent".to_owned(),
            })?;
    let name = inventory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: "unregistered inventory has no UTF-8 file name".to_owned(),
        })?;
    let sequence = PORTABLE_INVENTORY_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence)))
}

fn ensure_portable_inventory_header(
    inventory: &Path,
    signature: &str,
) -> tracedecay_domain::errors::Result<()> {
    if portable_inventory_matches(inventory, signature) {
        return Ok(());
    }
    // A stale completion marker would otherwise make an atomically repaired
    // empty inventory look complete after a crash between the two operations.
    clear_portable_inventory_complete(inventory).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("clear incomplete unregistered inventory marker: {error}"),
        }
    })?;
    let temporary = portable_inventory_temporary_path(inventory)?;
    tracedecay_runtime_core::storage::PrivateStoreIo::write_file_atomically_durable(
        inventory,
        &temporary,
        portable_inventory_header(signature).as_bytes(),
    )
    .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("atomically publish unregistered inventory header: {error}"),
    })
}

/// A newline commits one appended record. Only the bounded final suffix is
/// inspected: an interrupted append is truncated back to the last committed
/// record before either hydration or another append can observe it.
fn recover_portable_inventory_tail(inventory: &Path, signature: &str) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};

    let header_len = portable_inventory_header(signature).len() as u64;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(inventory)?;
    let length = file.metadata()?.len();
    if length < header_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unregistered inventory is shorter than its header",
        ));
    }
    let tail_start = length.saturating_sub(PORTABLE_INVENTORY_TAIL_RECOVERY_BYTES);
    file.seek(SeekFrom::Start(tail_start))?;
    let mut tail = Vec::with_capacity((length - tail_start) as usize);
    (&mut file)
        .take(length - tail_start)
        .read_to_end(&mut tail)?;
    if tail.last() == Some(&b'\n') {
        return Ok(());
    }
    let truncate_at = tail
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(header_len, |position| tail_start + position as u64 + 1)
        .max(header_len);
    file.set_len(truncate_at)?;
    file.sync_all()
}

fn portable_inventory_complete_path(inventory: &Path) -> std::path::PathBuf {
    inventory.with_extension("complete")
}

fn portable_inventory_is_complete(inventory: &Path) -> bool {
    std::fs::symlink_metadata(portable_inventory_complete_path(inventory))
        .is_ok_and(|metadata| metadata.file_type().is_file() && !metadata.file_type().is_symlink())
}

fn mark_portable_inventory_complete(inventory: &Path) -> std::io::Result<()> {
    let marker = portable_inventory_complete_path(inventory);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    match options.open(marker) {
        Ok(file) => file.sync_all(),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

struct PortableInventoryBuilder {
    source: cap_std::fs::ReadDir,
    /// A restart hydrates this set from the durable log in bounded slices
    /// before replaying source entries, so retained partial work is not
    /// replaced or appended twice.
    known: HashSet<String>,
    known_offset: u64,
    hydration: Option<std::io::BufReader<std::fs::File>>,
    complete: bool,
}

static PORTABLE_INVENTORY_BUILDERS: OnceLock<
    Mutex<HashMap<std::path::PathBuf, PortableInventoryBuilder>>,
> = OnceLock::new();

static PORTABLE_INVENTORY_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
pub(super) fn forget_portable_inventory_builder_for_test(inventory: &Path) {
    if let Some(builders) = PORTABLE_INVENTORY_BUILDERS.get()
        && let Ok(mut builders) = builders.lock()
    {
        builders.remove(inventory);
    }
}

pub(super) fn advance_portable_inventory(
    projects_dir: &Path,
    inventory: &Path,
    signature: &str,
    raw_entry_limit: usize,
    entries_scanned: &mut usize,
    interrupted: &dyn Fn() -> bool,
) -> tracedecay_domain::errors::Result<Option<bool>> {
    use std::io::{BufRead, Seek, SeekFrom, Write};

    let parent =
        inventory
            .parent()
            .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                message: "unregistered inventory has no parent".to_owned(),
            })?;
    std::fs::create_dir_all(parent).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("create unregistered inventory parent: {error}"),
        }
    })?;
    // The durable inventory is shared by daemon maintenance and read-only
    // Doctor processes. Hold the canonical cross-process sidecar lock across
    // recovery, hydration, append, and completion so a second process cannot
    // splice bytes into a record or publish completion for a concurrently
    // changing inventory. A contender yields without blocking the admission;
    // the page retains its opaque cursor and retries this bounded slice.
    let lock_path = tracedecay_runtime_core::storage::append_lock_path(inventory);
    let _writer_lock = match tracedecay_runtime_core::storage::try_acquire_sidecar_lock(&lock_path)
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("acquire unregistered inventory writer lock: {error}"),
        })? {
        Some(lock) => lock,
        None => return Ok(Some(false)),
    };
    ensure_portable_inventory_header(inventory, signature)?;
    recover_portable_inventory_tail(inventory, signature).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("recover unregistered inventory trailing record: {error}"),
        }
    })?;
    if portable_inventory_is_complete(inventory) {
        // Completion may have been published by another process while this
        // process retained a partial iterator and its deduplication set.
        if let Some(builders) = PORTABLE_INVENTORY_BUILDERS.get() {
            builders
                .lock()
                .map_err(|_| tracedecay_domain::errors::TraceDecayError::Config {
                    message: "unregistered inventory builder lock is poisoned".to_owned(),
                })?
                .remove(inventory);
        }
        return Ok(Some(true));
    }
    let builders = PORTABLE_INVENTORY_BUILDERS.get_or_init(|| Mutex::new(HashMap::new()));
    // The canonical sidecar lock already owns this inventory exclusively.
    // Move its iterator out of the shared map so unrelated profile admissions
    // never wait for this inventory's filesystem reads or durable appends.
    let retained = hotpath::measure_block!("maintenance.orphan_inventory.builder_admission", {
        builders
            .lock()
            .map_err(|_| tracedecay_domain::errors::TraceDecayError::Config {
                message: "unregistered inventory builder lock is poisoned".to_owned(),
            })?
            .remove(inventory)
    });
    let mut builder = if let Some(builder) = retained {
        builder
    } else {
        let profile_root = projects_dir.parent().ok_or_else(|| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: "unregistered projects directory has no profile parent".to_owned(),
            }
        })?;
        let capability =
            open_store_directory_nofollow(profile_root, projects_dir).map_err(|kind| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("open unregistered project-directory capability: {kind:?}"),
                }
            })?;
        let source = capability.root.entries().map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("open unregistered project-directory inventory stream: {error}"),
            }
        })?;
        let file = std::fs::File::open(inventory).map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("open unregistered inventory for recovery: {error}"),
            }
        })?;
        let mut hydration = std::io::BufReader::new(file);
        let mut header = String::new();
        hydration.read_line(&mut header).map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("read unregistered inventory recovery header: {error}"),
            }
        })?;
        PortableInventoryBuilder {
            source,
            known: HashSet::new(),
            known_offset: header.len() as u64,
            hydration: Some(hydration),
            complete: false,
        }
    };
    let result = (|| {
        // The sidecar lock serializes writes, but other processes can append
        // between admissions. Hydrate only their new suffix before resuming this
        // iterator, so each committed name still appears exactly once.
        if builder.hydration.is_none() {
            let mut file = std::fs::File::open(inventory).map_err(|error| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("open unregistered inventory suffix: {error}"),
                }
            })?;
            let length = file
                .metadata()
                .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("inspect unregistered inventory suffix: {error}"),
                })?
                .len();
            if length < builder.known_offset {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: "committed unregistered inventory shrank between admissions"
                        .to_owned(),
                });
            }
            if length > builder.known_offset {
                file.seek(SeekFrom::Start(builder.known_offset))
                    .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
                        message: format!("seek unregistered inventory suffix: {error}"),
                    })?;
                builder.hydration = Some(std::io::BufReader::new(file));
            }
        }
        let raw_entry_limit = raw_entry_limit.max(1);
        let mut budget = raw_entry_limit;
        let mut completed = false;
        let mut append_error = None;
        while budget > 0 {
            if interrupted() {
                return Ok(None);
            }
            if let Some(hydration) = builder.hydration.as_mut() {
                let mut name = String::new();
                let bytes = hydration.read_line(&mut name).map_err(|error| {
                    tracedecay_domain::errors::TraceDecayError::Config {
                        message: format!("read unregistered inventory recovery entry: {error}"),
                    }
                })?;
                if bytes == 0 {
                    builder.hydration = None;
                    continue;
                }
                builder.known_offset += bytes as u64;
                *entries_scanned = entries_scanned.saturating_add(1);
                budget = budget.saturating_sub(1);
                let name = name.trim_end_matches(['\r', '\n']);
                if portable_inventory_entry_is_valid(name) {
                    builder.known.insert(name.to_owned());
                }
                continue;
            }
            if builder.complete {
                completed = true;
                break;
            }
            let Some(entry) = builder.source.next() else {
                builder.complete = true;
                completed = true;
                break;
            };
            *entries_scanned = entries_scanned.saturating_add(1);
            budget = budget.saturating_sub(1);
            let entry =
                entry.map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "read unregistered project-directory inventory entry: {error}"
                    ),
                })?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if portable_inventory_entry_is_valid(&name) && !builder.known.contains(&name) {
                let append = hotpath::measure_block!(
                    "maintenance.orphan_inventory.durable_append",
                    (|| {
                        // Build one complete record before its single locked write.
                        // A crash can leave only this final record torn; the bounded
                        // tail repair above removes it before the next hydration.
                        let record = format!("{name}\n");
                        let mut output =
                            std::fs::OpenOptions::new().append(true).open(inventory)?;
                        output
                            .write_all(record.as_bytes())
                            .and_then(|()| output.sync_data())
                    })()
                );
                if let Err(error) = append {
                    append_error = Some(error);
                    break;
                }
                builder.known_offset += name.len() as u64 + 1;
                builder.known.insert(name);
            }
        }
        if let Some(error) = append_error {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("durably append unregistered inventory entry: {error}"),
            });
        }
        if completed {
            mark_portable_inventory_complete(inventory).map_err(|error| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("mark unregistered inventory complete: {error}"),
                }
            })?;
        }
        Ok(Some(completed))
    })();
    if matches!(&result, Ok(None | Some(false))) {
        builders
            .lock()
            .map_err(|_| tracedecay_domain::errors::TraceDecayError::Config {
                message: "unregistered inventory builder lock is poisoned".to_owned(),
            })?
            .insert(inventory.to_path_buf(), builder);
    }
    result
}
