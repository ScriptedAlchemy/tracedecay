//! Bounded, resumable census and collection of unregistered project leaves.

use std::collections::{BinaryHeap, HashSet};
use std::path::Path;
use std::time::Instant;

use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};

use super::fence::{capture_store_content_fence_controlled, open_store_directory_nofollow};
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
    /// Raw directory entries read to select this slice. One page reads the
    /// directory once, so summing pages exposes an unbounded rescan loop
    /// without timing heuristics.
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

/// One daemon/Doctor-owned page. The cursor is a restart-safe position in the
/// profile's project directory that callers return unchanged; they persist it
/// only after this page has reached a terminal completion state.
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

/// Builds only one page of costly child inventories. Its directory cursor
/// advances by exactly one page of names, so a profile with many unregistered
/// leaves cannot turn one writer admission into a full recursive census.
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

/// One page's resume token: the last project-directory name the previous page
/// returned. Prefixing it keeps an unrelated or truncated token from being
/// mistaken for a name, so an unknown token restarts the pass instead of
/// silently skipping the directories that sort before it.
const PROJECT_DIRECTORY_CURSOR_PREFIX: &str = "name-v3:";

/// Read exactly one bounded page of project-directory names through a
/// capability opened beneath the profile root.
///
/// # Contract
///
/// The cursor is a project-directory *name*, so it is restart-safe by
/// construction: it names a position in the total order of the directory's
/// contents rather than a position in one process's open directory stream.
/// No `telldir` cookie, no live iterator, and no durable sidecar log can be
/// invalidated by a concurrent create, delete, or replacement, and the same
/// cursor read twice describes the same slice. A full pass therefore:
///
/// * returns every candidate that existed for the whole pass, exactly once —
///   each page selects only names strictly greater than its cursor, so a
///   replay is impossible rather than merely unlikely;
/// * converges in `ceil(candidates / limit)` pages under any amount of
///   directory churn, because a page that returns a cursor has advanced the
///   name order by a full page; and
/// * holds at most `limit` names in memory, whatever the directory's size,
///   at the cost of one cheap directory read per page.
pub(in crate::retention) fn read_project_directory_page(
    profile_root: &Path,
    cursor: Option<&str>,
    limit: usize,
    interrupted: &dyn Fn() -> bool,
) -> tracedecay_domain::errors::Result<Option<ProjectDirectoryPageV1>> {
    let projects_dir = profile_root.join("projects");
    let root = match open_store_directory_nofollow(profile_root, &projects_dir) {
        Ok(capability) => capability.root,
        Err(super::CollectionFailureKind::PayloadChanged) => {
            return Ok(Some(ProjectDirectoryPageV1 {
                entries: Vec::new(),
                next_cursor: None,
                entries_scanned: 0,
            }));
        }
        Err(kind) => {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("open unregistered project-directory page: {kind:?}"),
            });
        }
    };
    // `open_dir_nofollow` intentionally returns an `O_PATH` capability on
    // Linux, which cannot be listed. Open its exact dot entry instead: the
    // listing stays rooted at the already verified no-follow capability, and
    // `.` is never a symlink, so nothing is followed to obtain it.
    let listing = root.open_dir(Path::new(".")).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("open unregistered project-directory listing: {error}"),
        }
    })?;
    let entries =
        listing
            .entries()
            .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("read unregistered project-directory listing: {error}"),
            })?;

    let after = cursor.and_then(parse_project_directory_cursor);
    let limit = limit.max(1);
    // A max-heap trimmed back to `limit` keeps the `limit` smallest names
    // after the cursor without ever retaining the directory's whole name set.
    let mut selected = BinaryHeap::new();
    let mut candidates = 0usize;
    let mut entries_scanned = 0usize;
    for entry in entries {
        if interrupted() {
            return Ok(None);
        }
        entries_scanned += 1;
        // A directory entry that cannot be read or named is not a candidate:
        // no project id and no quarantine name is unreadable or non-UTF-8, so
        // skipping one keeps the pass converging instead of failing the page.
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !is_project_directory_candidate(&name) {
            continue;
        }
        if after.as_deref().is_some_and(|after| name.as_str() <= after) {
            continue;
        }
        candidates += 1;
        selected.push(name);
        if selected.len() > limit {
            selected.pop();
        }
    }

    let names = selected.into_sorted_vec();
    // A page that exhausted the name order ends the pass; one that stopped at
    // its limit hands back the last name it returned.
    let next_cursor = match names.last() {
        Some(last) if candidates > names.len() => Some(format_project_directory_cursor(last)),
        _ => None,
    };
    let entries = names
        .into_iter()
        .map(|name| match quarantined_project_id(&name) {
            Some(project_id) => ProjectDirectoryWorkV1::Quarantine {
                project_id,
                quarantine_name: name,
            },
            None => ProjectDirectoryWorkV1::Project(name),
        })
        .collect::<Vec<_>>();
    Ok(Some(ProjectDirectoryPageV1 {
        entries,
        next_cursor,
        entries_scanned,
    }))
}

/// The two directory-entry classes this sweep owns. Every other name is
/// filtered before selection, so garbage entries never consume a page slot and
/// never stall the cursor.
fn is_project_directory_candidate(name: &str) -> bool {
    quarantined_project_id(name).is_some()
        || tracedecay_runtime_core::storage::validate_project_id(name).is_ok()
}

fn parse_project_directory_cursor(value: &str) -> Option<String> {
    value
        .strip_prefix(PROJECT_DIRECTORY_CURSOR_PREFIX)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

fn format_project_directory_cursor(name: &str) -> String {
    format!("{PROJECT_DIRECTORY_CURSOR_PREFIX}{name}")
}
