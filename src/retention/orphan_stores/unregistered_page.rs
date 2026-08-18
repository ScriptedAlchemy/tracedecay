//! Bounded, resumable census and collection of unregistered project leaves.

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
use std::collections::HashMap;
use std::collections::HashSet;
#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
use std::ffi::{CStr, OsString};
use std::path::Path;
#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
use std::os::fd::AsRawFd;
#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
use std::os::unix::ffi::OsStringExt;

use crate::global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};

use super::fence::capture_store_content_fence_controlled;
#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
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

pub(crate) const DEFAULT_UNREGISTERED_STORE_PAGE_LIMIT: usize = 8;
const MAX_UNREGISTERED_STORE_PAGE_LIMIT: usize = 64;
const UNREGISTERED_STORE_DIRECTORY_ENTRY_MULTIPLIER: usize = 8;

enum ProjectDirectoryWorkV1 {
    Project(String),
    Quarantine {
        project_id: String,
        quarantine_name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum UnregisteredSweepCompletionV1 {
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
pub(crate) struct UnregisteredStoreSweepRequestV1<'a> {
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
pub(crate) struct UnregisteredStoreSweepReport {
    pub(crate) plan: UnregisteredCollectionPlan,
    pub(crate) applied: bool,
    pub(crate) outcome: CollectionOutcome,
    pub(crate) next_cursor: Option<String>,
    pub(crate) completion: UnregisteredSweepCompletionV1,
}

/// Performs the full inspection → confirmation → apply journey for one page,
/// honoring cancellation/deadline before each bounded filesystem/registry
/// action. A cancellation never reports an empty successful page or mutates a
/// partially inspected plan.
pub(crate) async fn sweep_unregistered_store_page(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    request: UnregisteredStoreSweepRequestV1<'_>,
) -> crate::errors::Result<UnregisteredStoreSweepReport> {
    let limit = request.limit.clamp(1, MAX_UNREGISTERED_STORE_PAGE_LIMIT);
    if let Some(completion) =
        UnregisteredSweepCompletionV1::interrupted(request.cancellation, request.deadline)
    {
        return Ok(interrupted_report(completion, CollectionOutcome::default()));
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
        return Ok(interrupted_report(
            UnregisteredSweepCompletionV1::interrupted(request.cancellation, request.deadline)
                .unwrap_or(UnregisteredSweepCompletionV1::DeadlineExceeded),
            recovery_outcome,
        ));
    };
    let plan = plan_unregistered_collection(findings, request.retention_secs);
    if !request.apply {
        return Ok(UnregisteredStoreSweepReport {
            plan,
            applied: false,
            outcome: recovery_outcome,
            next_cursor,
            completion: UnregisteredSweepCompletionV1::Complete,
        });
    }
    if let Some(completion) =
        UnregisteredSweepCompletionV1::interrupted(request.cancellation, request.deadline)
    {
        return Ok(UnregisteredStoreSweepReport {
            plan: UnregisteredCollectionPlan::default(),
            applied: false,
            outcome: recovery_outcome,
            next_cursor: request.cursor,
            completion,
        });
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
    Ok(UnregisteredStoreSweepReport {
        plan,
        applied: completion == UnregisteredSweepCompletionV1::Complete,
        outcome,
        next_cursor: (completion == UnregisteredSweepCompletionV1::Complete)
            .then_some(next_cursor)
            .flatten(),
        completion,
    })
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
) -> crate::errors::Result<Option<(Vec<UnregisteredStoreFinding>, Option<String>)>> {
    let projects_dir = profile_root.join("projects");
    let page = read_project_directory_page(profile_root, cursor, limit, cancellation, deadline)?;
    let Some((work, next_cursor)) = page else {
        return Ok(None);
    };
    let mut recovered_project_ids = HashSet::new();
    let mut findings = Vec::with_capacity(work.len());
    for work in work {
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

/// Read exactly one bounded page of project-directory names through a
/// capability opened beneath the profile root. The POSIX directory offset is
/// opaque; it is accepted only when the directory identity still matches, so
/// a replacement restarts safely instead of seeking a stale location.
#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
fn read_project_directory_page(
    profile_root: &Path,
    cursor: Option<&str>,
    limit: usize,
    cancellation: &CancellationToken,
    deadline: MonotonicDeadline,
) -> crate::errors::Result<Option<(Vec<ProjectDirectoryWorkV1>, Option<String>)>> {
    let projects_dir = profile_root.join("projects");
    let root = match open_store_directory_nofollow(profile_root, &projects_dir) {
        Ok(capability) => capability.root,
        Err(super::CollectionFailureKind::PayloadChanged) => {
            return Ok(Some((Vec::new(), None)));
        }
        Err(kind) => {
            return Err(crate::errors::TraceDecayError::Config {
                message: format!("open unregistered project-directory page: {kind:?}"),
            });
        }
    };
    let identity = directory_cursor_identity(&root).map_err(|error| {
        crate::errors::TraceDecayError::Config {
            message: format!("inspect unregistered project-directory cursor: {error}"),
        }
    })?;
    let offset = cursor
        .and_then(parse_project_directory_cursor)
        .filter(|saved| saved.identity == identity && saved.offset >= 0)
        .map_or(0, |saved| saved.offset);
    let stream = DirectoryStream::open(root.as_raw_fd()).map_err(|error| {
        crate::errors::TraceDecayError::Config {
            message: format!("open unregistered project-directory stream: {error}"),
        }
    })?;
    if offset > 0 {
        // SAFETY: the opaque offset originated from `telldir` for this exact
        // directory identity. A filesystem that rejects/invalidates the
        // offset produces an empty or repeated page, which simply restarts on
        // the next full pass; it cannot authorize deletion by itself.
        unsafe { libc::seekdir(stream.raw, offset as libc::c_long) };
    }

    let scan_limit = limit.saturating_mul(UNREGISTERED_STORE_DIRECTORY_ENTRY_MULTIPLIER);
    let mut scanned = 0usize;
    let mut work = Vec::with_capacity(limit);
    let mut resume_offset = offset;
    loop {
        if UnregisteredSweepCompletionV1::interrupted(cancellation, deadline).is_some() {
            return Ok(None);
        }
        let Some(name) =
            stream
                .next_name()
                .map_err(|error| crate::errors::TraceDecayError::Config {
                    message: format!("read unregistered project-directory stream: {error}"),
                })?
        else {
            return Ok(Some((work, None)));
        };
        if name == "." || name == ".." {
            continue;
        }
        let position =
            stream
                .position()
                .map_err(|error| crate::errors::TraceDecayError::Config {
                    message: format!("checkpoint unregistered project-directory stream: {error}"),
                })?;
        let entry = if let Some(project_id) = quarantined_project_id(&name) {
            Some(ProjectDirectoryWorkV1::Quarantine {
                project_id,
                quarantine_name: name,
            })
        } else if crate::storage::validate_project_id(&name).is_ok() {
            Some(ProjectDirectoryWorkV1::Project(name))
        } else {
            None
        };
        if let Some(entry) = entry {
            if work.len() == limit {
                return Ok(Some((
                    work,
                    Some(format_project_directory_cursor(identity, resume_offset)),
                )));
            }
            work.push(entry);
        }
        scanned = scanned.saturating_add(1);
        resume_offset = position;
        if scanned >= scan_limit {
            return Ok(Some((
                work,
                Some(format_project_directory_cursor(identity, resume_offset)),
            )));
        }
    }
}

#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectDirectoryCursorIdentity {
    device: u64,
    inode: u64,
}

#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
struct ProjectDirectoryCursor {
    identity: ProjectDirectoryCursorIdentity,
    offset: i64,
}

#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
fn directory_cursor_identity(
    root: &cap_std::fs::Dir,
) -> std::io::Result<ProjectDirectoryCursorIdentity> {
    use cap_std::fs::MetadataExt;

    let metadata = root.metadata(".")?;
    Ok(ProjectDirectoryCursorIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
fn parse_project_directory_cursor(value: &str) -> Option<ProjectDirectoryCursor> {
    let mut fields = value.split(':');
    (fields.next()? == "v1").then_some(())?;
    let device = fields.next()?.parse().ok()?;
    let inode = fields.next()?.parse().ok()?;
    let offset = fields.next()?.parse().ok()?;
    fields.next().is_none().then_some(ProjectDirectoryCursor {
        identity: ProjectDirectoryCursorIdentity { device, inode },
        offset,
    })
}

#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
fn format_project_directory_cursor(
    identity: ProjectDirectoryCursorIdentity,
    offset: i64,
) -> String {
    format!("v1:{}:{}:{offset}", identity.device, identity.inode)
}

#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
struct DirectoryStream {
    raw: *mut libc::DIR,
}

#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
impl DirectoryStream {
    fn open(directory_fd: std::os::fd::RawFd) -> std::io::Result<Self> {
        // `open_dir_nofollow` intentionally returns an O_PATH capability on
        // Linux. `dup` would preserve O_PATH, which `fdopendir` rejects with
        // EBADF. Open its exact dot entry instead: this remains rooted at the
        // already verified no-follow capability while yielding a readable
        // directory fd owned by the stream.
        let readable = unsafe {
            libc::openat(
                directory_fd,
                c".".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if readable < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `readable` is an owned directory fd. On failure it remains
        // owned here and is closed before returning.
        let raw = unsafe { libc::fdopendir(readable) };
        if raw.is_null() {
            // SAFETY: `fdopendir` did not consume this fd on failure.
            unsafe { libc::close(readable) };
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { raw })
    }

    fn next_name(&self) -> std::io::Result<Option<String>> {
        // SAFETY: `raw` remains valid for the lifetime of this stream. Reset
        // errno first so a null return is distinguishable from end-of-stream.
        reset_errno();
        let entry = unsafe { libc::readdir(self.raw) };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            return if error.raw_os_error() == Some(0) {
                Ok(None)
            } else {
                Err(error)
            };
        }
        // SAFETY: POSIX `dirent::d_name` is NUL-terminated for a successful
        // `readdir`; it stays live until the following `readdir` call.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        let name = OsString::from_vec(bytes.to_vec())
            .into_string()
            .map_err(|_| std::io::Error::other("non-UTF-8 project-directory entry"))?;
        Ok(Some(name))
    }

    fn position(&self) -> std::io::Result<i64> {
        // SAFETY: `raw` is a live `DIR*` owned by this stream.
        let position = unsafe { libc::telldir(self.raw) };
        if position < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(position)
        }
    }
}

#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this struct owns the stream and calls `closedir` exactly
        // once when it leaves scope.
        let _ = unsafe { libc::closedir(self.raw) };
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn reset_errno() {
    // SAFETY: libc exposes this thread-local errno cell for the current
    // thread; clearing it distinguishes end-of-directory from read failure.
    unsafe { *libc::__errno_location() = 0 };
}

#[cfg(target_os = "macos")]
fn reset_errno() {
    // SAFETY: macOS exposes the current thread's errno storage through
    // `__error`, with the same end-of-directory contract as Linux.
    unsafe { *libc::__error() = 0 };
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
fn read_project_directory_page(
    profile_root: &Path,
    cursor: Option<&str>,
    limit: usize,
    cancellation: &CancellationToken,
    deadline: MonotonicDeadline,
) -> crate::errors::Result<Option<(Vec<ProjectDirectoryWorkV1>, Option<String>)>> {
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
                    return Ok(Some((Vec::new(), None)));
                }
                Err(error) => {
                    return Err(crate::errors::TraceDecayError::Config {
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
                return Ok(Some((Vec::new(), None)));
            }
            Err(error) => {
                return Err(crate::errors::TraceDecayError::Config {
                    message: format!("inspect unregistered project-directory page: {error}"),
                });
            }
        };
        let inventory = portable_inventory_path(profile_root, &signature);
        (signature, inventory, 0)
    };
    let build_complete = match advance_portable_inventory(
        &projects_dir,
        &inventory,
        &directory_signature,
        limit.saturating_mul(UNREGISTERED_STORE_DIRECTORY_ENTRY_MULTIPLIER),
        cancellation,
        deadline,
    )? {
        Some(complete) => complete,
        None => return Ok(None),
    };
    // A second process may observe the sidecar writer lock before its holder
    // has atomically published the first inventory header. Preserve the same
    // opaque cursor instead of opening a missing log and falsely reporting a
    // complete empty page; maintenance and Doctor will retry this exact slice.
    if !build_complete && !inventory.exists() {
        return Ok(Some((
            Vec::new(),
            Some(format_portable_directory_cursor(directory_signature, start)),
        )));
    }
    let file = match std::fs::File::open(&inventory) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some((Vec::new(), None)));
        }
        Err(error) => {
            return Err(crate::errors::TraceDecayError::Config {
                message: format!("open unregistered project-directory page: {error}"),
            });
        }
    };
    let mut reader = std::io::BufReader::new(file);
    use std::io::{BufRead, Seek, SeekFrom};
    if start == 0 {
        let mut header = String::new();
        reader
            .read_line(&mut header)
            .map_err(|error| crate::errors::TraceDecayError::Config {
                message: format!("read unregistered inventory header: {error}"),
            })?;
    } else {
        reader.seek(SeekFrom::Start(start)).map_err(|error| {
            crate::errors::TraceDecayError::Config {
                message: format!("seek unregistered inventory cursor: {error}"),
            }
        })?;
    }
    let mut work = Vec::with_capacity(limit);
    let scan_limit = limit.saturating_mul(UNREGISTERED_STORE_DIRECTORY_ENTRY_MULTIPLIER);
    let mut scanned = 0usize;
    loop {
        if UnregisteredSweepCompletionV1::interrupted(cancellation, deadline).is_some() {
            return Ok(None);
        }
        let mut name = String::new();
        let bytes = reader.read_line(&mut name).map_err(|error| {
            crate::errors::TraceDecayError::Config {
                message: format!("read unregistered inventory page: {error}"),
            }
        })?;
        if bytes == 0 {
            let next_cursor = (!build_complete).then(|| {
                reader
                    .stream_position()
                    .ok()
                    .map(|offset| format_portable_directory_cursor(directory_signature, offset))
            });
            return Ok(Some((work, next_cursor.flatten())));
        }
        let name = name.trim_end_matches(['\r', '\n']).to_owned();
        let next_offset =
            reader
                .stream_position()
                .map_err(|error| crate::errors::TraceDecayError::Config {
                    message: format!("checkpoint unregistered inventory cursor: {error}"),
                })?;
        scanned = scanned.saturating_add(1);
        if let Some(project_id) = quarantined_project_id(&name) {
            work.push(ProjectDirectoryWorkV1::Quarantine {
                project_id,
                quarantine_name: name,
            });
        } else if crate::storage::validate_project_id(&name).is_ok() {
            work.push(ProjectDirectoryWorkV1::Project(name));
        }
        if work.len() == limit || scanned >= scan_limit {
            return Ok(Some((
                work,
                Some(format_portable_directory_cursor(
                    directory_signature,
                    next_offset,
                )),
            )));
        }
    }
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
#[derive(Clone, PartialEq, Eq)]
struct PortableDirectoryCursor {
    signature: String,
    offset: u64,
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
fn portable_inventory_path(profile_root: &Path, signature: &str) -> std::path::PathBuf {
    profile_root
        .join("maintenance")
        .join("unregistered-project-directory-inventory-v2")
        .join(format!("{signature}.log"))
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
fn portable_directory_signature(directory: &Path) -> std::io::Result<String> {
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

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
fn parse_portable_directory_cursor(value: &str) -> Option<PortableDirectoryCursor> {
    let mut fields = value.split(':');
    let version = fields.next()?;
    let signature = fields.next()?;
    let offset = fields.next()?;
    (version == "portable-v2").then_some(())?;
    fields.next().is_none().then_some(())?;
    Some(PortableDirectoryCursor {
        signature: signature.to_owned(),
        offset: offset.parse().ok()?,
    })
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
fn format_portable_directory_cursor(signature: String, offset: u64) -> String {
    format!("portable-v2:{signature}:{offset}")
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
const PORTABLE_INVENTORY_TAIL_RECOVERY_BYTES: u64 = 64 * 1024;

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
fn portable_inventory_header(signature: &str) -> String {
    format!("v2:{signature}\n")
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
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

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
fn portable_inventory_entry_is_valid(name: &str) -> bool {
    quarantined_project_id(name).is_some() || crate::storage::validate_project_id(name).is_ok()
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
fn clear_portable_inventory_complete(inventory: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(portable_inventory_complete_path(inventory)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
fn portable_inventory_temporary_path(
    inventory: &Path,
) -> crate::errors::Result<std::path::PathBuf> {
    let parent = inventory
        .parent()
        .ok_or_else(|| crate::errors::TraceDecayError::Config {
            message: "unregistered inventory has no parent".to_owned(),
        })?;
    let name = inventory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| crate::errors::TraceDecayError::Config {
            message: "unregistered inventory has no UTF-8 file name".to_owned(),
        })?;
    let sequence = PORTABLE_INVENTORY_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence)))
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
fn ensure_portable_inventory_header(
    inventory: &Path,
    signature: &str,
) -> crate::errors::Result<()> {
    if portable_inventory_matches(inventory, signature) {
        return Ok(());
    }
    // A stale completion marker would otherwise make an atomically repaired
    // empty inventory look complete after a crash between the two operations.
    clear_portable_inventory_complete(inventory).map_err(|error| {
        crate::errors::TraceDecayError::Config {
            message: format!("clear incomplete unregistered inventory marker: {error}"),
        }
    })?;
    let temporary = portable_inventory_temporary_path(inventory)?;
    crate::storage::PrivateStoreIo::write_file_atomically_durable(
        inventory,
        &temporary,
        portable_inventory_header(signature).as_bytes(),
    )
    .map_err(|error| crate::errors::TraceDecayError::Config {
        message: format!("atomically publish unregistered inventory header: {error}"),
    })
}

/// A newline commits one appended record. Only the bounded final suffix is
/// inspected: an interrupted append is truncated back to the last committed
/// record before either hydration or another append can observe it.
#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
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
        .map(|position| tail_start + position as u64 + 1)
        .unwrap_or(header_len)
        .max(header_len);
    file.set_len(truncate_at)?;
    file.sync_all()
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
fn portable_inventory_complete_path(inventory: &Path) -> std::path::PathBuf {
    inventory.with_extension("complete")
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
fn portable_inventory_is_complete(inventory: &Path) -> bool {
    std::fs::symlink_metadata(portable_inventory_complete_path(inventory))
        .is_ok_and(|metadata| metadata.file_type().is_file() && !metadata.file_type().is_symlink())
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
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

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
struct PortableInventoryBuilder {
    source: std::fs::ReadDir,
    /// A restart hydrates this set from the durable log in bounded slices
    /// before replaying source entries, so retained partial work is not
    /// replaced or appended twice.
    known: HashSet<String>,
    hydration: Option<std::io::BufReader<std::fs::File>>,
    complete: bool,
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
static PORTABLE_INVENTORY_BUILDERS: OnceLock<
    Mutex<HashMap<std::path::PathBuf, PortableInventoryBuilder>>,
> = OnceLock::new();

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
static PORTABLE_INVENTORY_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[cfg(all(
    test,
    not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))
))]
pub(super) fn forget_portable_inventory_builder_for_test(inventory: &Path) {
    if let Some(builders) = PORTABLE_INVENTORY_BUILDERS.get()
        && let Ok(mut builders) = builders.lock()
    {
        builders.remove(inventory);
    }
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
fn advance_portable_inventory(
    projects_dir: &Path,
    inventory: &Path,
    signature: &str,
    raw_entry_limit: usize,
    cancellation: &CancellationToken,
    deadline: MonotonicDeadline,
) -> crate::errors::Result<Option<bool>> {
    use std::io::{BufRead, Write};

    let parent = inventory
        .parent()
        .ok_or_else(|| crate::errors::TraceDecayError::Config {
            message: "unregistered inventory has no parent".to_owned(),
        })?;
    std::fs::create_dir_all(parent).map_err(|error| crate::errors::TraceDecayError::Config {
        message: format!("create unregistered inventory parent: {error}"),
    })?;
    // The durable inventory is shared by daemon maintenance and read-only
    // Doctor processes. Hold the canonical cross-process sidecar lock across
    // recovery, hydration, append, and completion so a second process cannot
    // splice bytes into a record or publish completion for a concurrently
    // changing inventory. A contender yields without blocking the admission;
    // the page retains its opaque cursor and retries this bounded slice.
    let lock_path = crate::storage::append_lock_path(inventory);
    let _writer_lock =
        match crate::storage::try_acquire_sidecar_lock(&lock_path).map_err(|error| {
            crate::errors::TraceDecayError::Config {
                message: format!("acquire unregistered inventory writer lock: {error}"),
            }
        })? {
            Some(lock) => lock,
            None => return Ok(Some(false)),
        };
    ensure_portable_inventory_header(inventory, signature)?;
    recover_portable_inventory_tail(inventory, signature).map_err(|error| {
        crate::errors::TraceDecayError::Config {
            message: format!("recover unregistered inventory trailing record: {error}"),
        }
    })?;
    if portable_inventory_is_complete(inventory) {
        return Ok(Some(true));
    }
    let builders = PORTABLE_INVENTORY_BUILDERS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut builders = builders
        .lock()
        .map_err(|_| crate::errors::TraceDecayError::Config {
            message: "unregistered inventory builder lock is poisoned".to_owned(),
        })?;
    if !builders.contains_key(inventory) {
        let source = std::fs::read_dir(projects_dir).map_err(|error| {
            crate::errors::TraceDecayError::Config {
                message: format!("open unregistered project-directory inventory stream: {error}"),
            }
        })?;
        let file = std::fs::File::open(inventory).map_err(|error| {
            crate::errors::TraceDecayError::Config {
                message: format!("open unregistered inventory for recovery: {error}"),
            }
        })?;
        let mut hydration = std::io::BufReader::new(file);
        let mut header = String::new();
        hydration.read_line(&mut header).map_err(|error| {
            crate::errors::TraceDecayError::Config {
                message: format!("read unregistered inventory recovery header: {error}"),
            }
        })?;
        builders.insert(
            inventory.to_path_buf(),
            PortableInventoryBuilder {
                source,
                known: HashSet::new(),
                hydration: Some(hydration),
                complete: false,
            },
        );
    }
    let Some(builder) = builders.get_mut(inventory) else {
        return Err(crate::errors::TraceDecayError::Config {
            message: "unregistered inventory builder was not retained".to_owned(),
        });
    };
    let raw_entry_limit = raw_entry_limit.max(1);
    let mut budget = raw_entry_limit;
    let mut completed = false;
    let mut append_error = None;
    while budget > 0 {
        if UnregisteredSweepCompletionV1::interrupted(cancellation, deadline).is_some() {
            return Ok(None);
        }
        if let Some(hydration) = builder.hydration.as_mut() {
            let mut name = String::new();
            let bytes = hydration.read_line(&mut name).map_err(|error| {
                crate::errors::TraceDecayError::Config {
                    message: format!("read unregistered inventory recovery entry: {error}"),
                }
            })?;
            if bytes == 0 {
                builder.hydration = None;
                continue;
            }
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
        budget = budget.saturating_sub(1);
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if portable_inventory_entry_is_valid(&name) && !builder.known.contains(&name) {
            let append = (|| {
                // Build one complete record before its single locked write.
                // A crash can leave only this final record torn; the bounded
                // tail repair above removes it before the next hydration.
                let record = format!("{name}\n");
                let mut output = std::fs::OpenOptions::new().append(true).open(inventory)?;
                output
                    .write_all(record.as_bytes())
                    .and_then(|()| output.sync_data())
            })();
            if let Err(error) = append {
                append_error = Some(error);
                break;
            }
            builder.known.insert(name);
        }
    }
    if let Some(error) = append_error {
        builders.remove(inventory);
        return Err(crate::errors::TraceDecayError::Config {
            message: format!("durably append unregistered inventory entry: {error}"),
        });
    }
    if completed {
        mark_portable_inventory_complete(inventory).map_err(|error| {
            crate::errors::TraceDecayError::Config {
                message: format!("mark unregistered inventory complete: {error}"),
            }
        })?;
        builders.remove(inventory);
    }
    Ok(Some(completed))
}
