//! Store-maintenance operations performed by the daemon git watcher.
//!
//! Every operation that opens, tracks, or garbage-collects a store lives here
//! so its [`StoreAdministration`] lifetime is kept separate from the watcher
//! state machine.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::branch::BranchAdminAction;
use crate::config::{CompactionThresholdConfig, RetentionConfig};
use crate::tracedecay::TraceDecay;

use super::super::{branch_admin::StoreAdministration, log_daemon_event};
use super::GitWatcherInner;

const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

pub(super) fn retention_window_secs(days: u64) -> i64 {
    i64::try_from(days)
        .unwrap_or(i64::MAX)
        .saturating_mul(SECONDS_PER_DAY)
}

/// Opens the project store and runs a diff-scoped incremental sync (or a full
/// sync when the diff base is missing / oversized). Returns true on success.
/// `SyncLock` is treated as success (a peer synced).
///
/// The `TraceDecay` sync/open futures are `Send` (the sync path scopes its
/// `!Send` `gix` values so they drop before every `.await`; see
/// `indexing::stamp_last_synced_commit`), so this awaits them directly on the
/// caller's task under the daemon-wide sync semaphore — no nested runtime.
pub(super) async fn sync_project(
    cg: &TraceDecay,
    escalation: usize,
    administration: &StoreAdministration,
) -> bool {
    // Hold the administration gate from before opening the store until the
    // `TraceDecay` handle drops. This prevents branch-store GC from selecting
    // or unlinking the SQLite family while a watcher sync owns it.
    administration
        .with_writer(|| async {
            let base = cg.last_synced_commit().await;
            let result = match base {
                Some(base) => match cg.stale_files_since_commit(&base, escalation) {
                    Some(files) if files.is_empty() => Ok(()),
                    Some(files) => cg.sync_if_stale_silent(&files).await,
                    // Base missing/unreachable or over the escalation limit → full.
                    None => cg.sync().await.map(|_| ()),
                },
                None => cg.sync().await.map(|_| ()),
            };
            result.is_ok()
        })
        .await
}

/// Proactively tracks a linked worktree's branch. Returns the
/// [`crate::branch::BranchAddOutcome`] name for logging, or `None` on error.
pub(super) async fn track_worktree_branch(
    administration: &StoreAdministration,
    cg: &TraceDecay,
    wt_root: PathBuf,
    branch: String,
) -> Option<String> {
    administration
        .with_writer(|| async {
            cg.track_worktree_branch(&wt_root, &branch)
                .await
                .ok()
                .map(|outcome| format!("{outcome:?}"))
        })
        .await
}

/// Resolves a `worktrees/<name>` leaf to `(worktree_root, branch)` by reading
/// its `gitdir` file and the linked HEAD.
pub(super) fn resolve_worktree(common: &Path, name: &str) -> Option<(PathBuf, String)> {
    let wt_meta = common.join("worktrees").join(name);
    let gitdir_file = wt_meta.join("gitdir");
    let gitdir_raw = std::fs::read_to_string(&gitdir_file).ok()?;
    // `gitdir` points at `<worktree>/.git`; the worktree root is its parent.
    let gitdir = PathBuf::from(gitdir_raw.trim());
    let wt_root = gitdir.parent()?.to_path_buf();
    let branch = crate::branch::current_branch(&wt_root)?;
    Some((wt_root, branch))
}

/// Returns the current linked-worktree metadata leaves for one conservative
/// reconciliation pass. Native Git remains authoritative when each leaf is
/// resolved; this inventory only recovers callback path detail lost to lock
/// contention.
pub(super) fn linked_worktree_names(common: &Path) -> std::collections::HashSet<String> {
    let Ok(entries) = std::fs::read_dir(common.join("worktrees")) else {
        return std::collections::HashSet::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect()
}

/// Runs branch-store GC for a project through the daemon administration
/// coordinator, logging what it removed. Returns `false` when layout resolution
/// or administration fails so the backstop keeps the GC cadence eligible for a
/// retry.
pub(super) async fn run_gc(inner: &Arc<GitWatcherInner>, cg: &TraceDecay) -> bool {
    let root = cg.project_root();
    let data_root = &cg.store_layout().data_root;

    // Preserve the sync-semaphore → administration-gate acquisition order used
    // by sync and worktree tracking. The coordinator owns the writer gate and
    // its process/store-holder safety checks.
    let _permit = inner.sync_semaphore.acquire().await;
    let report = inner
        .administration
        .execute_branch_admin_in_layout(
            root,
            data_root,
            BranchAdminAction::Gc,
            inner.config.branch_gc_days,
            inner.config.orphan_db_gc_days,
        )
        .await;
    let report = match report {
        Ok(report) => report,
        Err(_) => {
            log_daemon_event(
                "git_watch_degraded",
                &[
                    ("scope", "project".to_string()),
                    ("reason", "branch_gc_deferred".to_string()),
                    ("failure", "branch_administration_failed".to_string()),
                ],
            );
            return false;
        }
    };

    if !report.removed_branches.is_empty() || !report.removed_orphan_dbs.is_empty() {
        log_daemon_event(
            "git_watch_synced",
            &[
                ("scope", "project".to_string()),
                ("action", "gc".to_string()),
                ("removed_tracked", report.removed_branches.len().to_string()),
                (
                    "removed_orphans",
                    report.removed_orphan_dbs.len().to_string(),
                ),
            ],
        );
    }
    true
}

/// Current unix time in whole seconds, as the `i64` the retention engines
/// compare row timestamps against.
fn now_secs_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

pub(super) async fn run_session_retention(
    database: &crate::global_db::RegisteredGlobalDb,
    config: &RetentionConfig,
) -> bool {
    let now = now_secs_i64();
    let mut succeeded = true;

    if config.session_lcm.enabled {
        match database
            .run_session_lcm_retention(
                "all",
                None,
                &config.session_lcm,
                crate::sessions::lcm::RetentionMode::Apply,
                now,
            )
            .await
        {
            Ok(report) => {
                let reclaimed = report.bytes_reclaimed();
                succeeded &= report.errors.is_empty();
                if reclaimed > 0 || !report.errors.is_empty() {
                    log_daemon_event(
                        "retention_session_lcm",
                        &[
                            ("store", "mounted_sessions".to_string()),
                            ("bytes_reclaimed", reclaimed.to_string()),
                            ("errors", report.errors.len().to_string()),
                        ],
                    );
                }
            }
            Err(_) => {
                succeeded = false;
                log_daemon_event(
                    "retention_degraded",
                    &[
                        ("pass", "session_lcm".to_string()),
                        ("failure", "retention_pass_failed".to_string()),
                    ],
                );
            }
        }
    }

    if config.observation.enabled {
        match database
            .run_observation_retention(
                None,
                &config.observation,
                crate::global_db::observation::retention::RetentionMode::Apply,
                now,
            )
            .await
        {
            Ok(report) => {
                let reclaimed = report.bytes_reclaimed();
                succeeded &= report.errors.is_empty();
                if reclaimed > 0 || !report.errors.is_empty() {
                    log_daemon_event(
                        "retention_observation",
                        &[
                            ("store", "mounted_sessions".to_string()),
                            ("bytes_reclaimed", reclaimed.to_string()),
                            ("errors", report.errors.len().to_string()),
                        ],
                    );
                }
            }
            Err(_) => {
                succeeded = false;
                log_daemon_event(
                    "retention_degraded",
                    &[
                        ("pass", "observation".to_string()),
                        ("failure", "retention_pass_failed".to_string()),
                    ],
                );
            }
        }
    }

    if let Some(compaction) = &config.compaction {
        succeeded &= run_compaction(
            RetainedCompactionStore::Registered(database),
            "mounted_sessions",
            compaction,
        )
        .await;
    }
    succeeded
}

pub(super) async fn run_global_compaction(
    database: &crate::global_db::RegisteredGlobalDb,
    config: &CompactionThresholdConfig,
) -> bool {
    run_compaction(
        RetainedCompactionStore::Registered(database),
        "global.db",
        config,
    )
    .await
}

pub(super) async fn run_project_compaction(
    database: &crate::db::Database,
    config: &CompactionThresholdConfig,
) -> bool {
    run_compaction(
        RetainedCompactionStore::Project(database),
        crate::config::DB_FILENAME,
        config,
    )
    .await
}

enum RetainedCompactionStore<'a> {
    Registered(&'a crate::global_db::RegisteredGlobalDb),
    Project(&'a crate::db::Database),
}

impl RetainedCompactionStore<'_> {
    async fn storage_page_counts(&self) -> crate::errors::Result<(u64, u64, u64)> {
        match self {
            Self::Registered(database) => database.storage_page_counts(),
            Self::Project(database) => database.storage_page_counts().await,
        }
    }

    async fn run_bounded_incremental_compaction(
        &self,
        max_pages: u64,
    ) -> crate::errors::Result<()> {
        match self {
            Self::Registered(database) => {
                database.run_bounded_incremental_compaction(max_pages).await
            }
            Self::Project(database) => database.run_incremental_vacuum(max_pages).await,
        }
    }
}

/// Samples the store's free-page ratio and, when the owner-configured threshold
/// is met, schedules a bounded incremental vacuum in the deferred background
/// lane (Plan 38 §6). The placement is structurally forbidden from competing
/// with foreground writes; the page cap keeps the reclaim off the hot path.
async fn run_compaction(
    store: RetainedCompactionStore<'_>,
    store_name: &'static str,
    config: &CompactionThresholdConfig,
) -> bool {
    let Ok((page_size, page_count, freelist)) = store.storage_page_counts().await else {
        log_daemon_event(
            "retention_degraded",
            &[
                ("pass", "compaction".to_string()),
                ("failure", "store_size_sample_failed".to_string()),
            ],
        );
        return false;
    };
    let Ok(scheduled) = compaction_is_scheduled(page_size, page_count, freelist, config) else {
        return false;
    };
    if !scheduled {
        return true;
    }
    let pages = config.max_pages_per_tick.max(1);
    let freelist_before = freelist;
    if store
        .run_bounded_incremental_compaction(u64::from(pages))
        .await
        .is_err()
    {
        log_daemon_event(
            "retention_degraded",
            &[
                ("pass", "compaction".to_string()),
                ("failure", "incremental_vacuum_failed".to_string()),
            ],
        );
        return false;
    }
    let Ok((_, _, freelist_after)) = store.storage_page_counts().await else {
        log_daemon_event(
            "retention_degraded",
            &[
                ("pass", "compaction".to_string()),
                ("failure", "post_compaction_sample_failed".to_string()),
            ],
        );
        return false;
    };
    log_compaction(store_name, freelist_before, freelist_after);
    true
}

fn compaction_is_scheduled(
    page_size: u64,
    page_count: u64,
    freelist: u64,
    config: &CompactionThresholdConfig,
) -> Result<bool, ()> {
    use tracedecay_application::storage::compaction::CompactionTriggerPolicyV1;
    use tracedecay_application::storage::identity::{
        FreePageRatioV1, StorageByteSizeV1, StoreKeyV1,
    };
    use tracedecay_application::storage::telemetry::StoreSizeSampleV1;
    use tracedecay_domain::UtcMicros;

    if page_size == 0 || page_count == 0 {
        return Ok(false);
    }
    let store_key = StoreKeyV1::new("store.db").map_err(|_| ())?;
    let page_size_bytes = u32::try_from(page_size).map_err(|_| ())?;
    let sample = StoreSizeSampleV1 {
        store: store_key,
        page_size_bytes,
        page_count,
        freelist_pages: freelist,
        observed_at: UtcMicros(now_secs_i64().saturating_mul(1_000_000)),
    };
    let threshold = FreePageRatioV1::new(config.free_page_ratio_threshold).map_err(|_| ())?;
    let policy = CompactionTriggerPolicyV1 {
        free_page_ratio_threshold: threshold,
        minimum_reclaimable_bytes: StorageByteSizeV1(config.minimum_reclaimable_bytes),
    };
    policy
        .decide(&sample)
        .map(|decision| decision.is_scheduled())
        .map_err(|_| ())
}

fn log_compaction(store_name: &'static str, freelist_before: u64, freelist_after: u64) {
    log_daemon_event(
        "retention_compaction",
        &[
            ("store", store_name.to_string()),
            (
                "freed_pages",
                freelist_before.saturating_sub(freelist_after).to_string(),
            ),
        ],
    );
}

/// Runs bounded incremental-vacuum compaction over every tracked branch
/// database other than the one `cg` currently has mounted (that store already
/// goes through [`run_project_compaction`]). Best-effort and independent per
/// file: a busy or failing branch database never blocks the rest, but keeps
/// the maintenance cadence retry-eligible — see
/// `src/retention/branch_compaction.rs` for the compaction policy itself.
pub(super) async fn run_branch_compaction(
    cg: &TraceDecay,
    config: &CompactionThresholdConfig,
) -> bool {
    let layout = cg.store_layout();
    let Some(meta) = crate::branch_meta::load_branch_meta(&layout.data_root) else {
        return true;
    };
    let active_db_path = cg.db_path();
    let candidates = crate::retention::branch_compaction::select_branch_db_candidates(
        &layout.data_root,
        &meta,
        &active_db_path,
    );
    if candidates.is_empty() {
        return true;
    }
    let report = crate::retention::branch_compaction::compact_branch_databases(&candidates, config);
    if report.policy_invalid {
        // Never silent: an out-of-range threshold disables the pass entirely
        // and would otherwise be indistinguishable from "nothing to compact".
        log_daemon_event(
            "retention_degraded",
            &[
                ("pass", "branch_compaction".to_string()),
                ("failure", "invalid_compaction_policy".to_string()),
                (
                    "free_page_ratio_threshold",
                    config.free_page_ratio_threshold.to_string(),
                ),
            ],
        );
        return false;
    }
    if report.compacted.is_empty() && report.skipped.is_empty() {
        return true;
    }
    let freed_pages: u64 = report
        .compacted
        .iter()
        .map(|outcome| outcome.freed_pages)
        .sum();
    let unreclaimable = report
        .skipped
        .iter()
        .filter(|skip| {
            skip.reason
                == crate::retention::branch_compaction::BranchCompactionSkipReason::IncrementalVacuumUnavailable
        })
        .count();
    log_daemon_event(
        "retention_branch_compaction",
        &[
            ("project", cg.project_root().display().to_string()),
            ("compacted", report.compacted.len().to_string()),
            ("freed_pages", freed_pages.to_string()),
            ("skipped", report.skipped.len().to_string()),
            // Branch databases predating `auto_vacuum = INCREMENTAL`: their
            // free pages need a full VACUUM this pass deliberately avoids.
            ("unreclaimable", unreclaimable.to_string()),
        ],
    );
    branch_compaction_succeeded(&report)
}

pub(super) fn branch_compaction_succeeded(
    report: &crate::retention::branch_compaction::BranchCompactionReport,
) -> bool {
    !report.policy_invalid && report.skipped.is_empty()
}

pub(super) async fn run_orphan_store_sweep(
    database: &crate::global_db::RegisteredGlobalDb,
    profile_root: &Path,
    orphan_store_gc_days: u64,
) -> bool {
    let retention_secs = retention_window_secs(orphan_store_gc_days);
    let now = now_secs_i64();
    let report = crate::retention::orphan_stores::sweep_orphan_stores(
        database,
        profile_root,
        retention_secs,
        now,
        true,
    )
    .await;
    let report = match report {
        Ok(report) => report,
        Err(_) => {
            log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "orphan_store_sweep".to_string()),
                    ("failure", "registry_read_failed".to_string()),
                ],
            );
            return false;
        }
    };
    let doctor_findings = report
        .plan
        .collect
        .iter()
        .chain(report.plan.retained_immature.iter())
        .chain(report.plan.relink.iter())
        .filter_map(crate::doctor::registry_drift::orphan_store_doctor_finding)
        .count();
    let orphaned = report
        .plan
        .collect
        .len()
        .saturating_add(report.plan.retained_immature.len());
    let orphan_bytes = report
        .plan
        .collect
        .iter()
        .chain(report.plan.retained_immature.iter())
        .fold(0u64, |total, finding| {
            total.saturating_add(finding.size_bytes)
        });
    let oldest_orphan_age_secs = report
        .plan
        .collect
        .iter()
        .chain(report.plan.retained_immature.iter())
        .map(|finding| finding.age_secs)
        .max()
        .unwrap_or(0);
    let failure_count = |kind| {
        report
            .outcome
            .errors
            .iter()
            .filter(|failure| failure.kind == kind)
            .count()
    };
    let registry_retirement_failed = report.retired_registry_rows < report.outcome.collected.len();

    if !report.outcome.collected.is_empty()
        || !report.plan.relink.is_empty()
        || !report.outcome.errors.is_empty()
        || doctor_findings > 0
    {
        log_daemon_event(
            "retention_orphan_stores",
            &[
                ("collected", report.outcome.collected.len().to_string()),
                ("orphaned", orphaned.to_string()),
                ("orphan_bytes", orphan_bytes.to_string()),
                ("oldest_orphan_age_secs", oldest_orphan_age_secs.to_string()),
                (
                    "reclaimed_bytes",
                    report.outcome.reclaimed_bytes.to_string(),
                ),
                ("relinkable", report.plan.relink.len().to_string()),
                (
                    "relinked_registry_rows",
                    report.relinked_registry_rows.to_string(),
                ),
                ("doctor_findings", doctor_findings.to_string()),
                (
                    "retired_registry_rows",
                    report.retired_registry_rows.to_string(),
                ),
                (
                    "registry_retirement_failed",
                    registry_retirement_failed.to_string(),
                ),
                (
                    "outside_profile_failures",
                    failure_count(
                        crate::retention::orphan_stores::CollectionFailureKind::OutsideProfile,
                    )
                    .to_string(),
                ),
                (
                    "inspect_failures",
                    failure_count(
                        crate::retention::orphan_stores::CollectionFailureKind::InspectFailed,
                    )
                    .to_string(),
                ),
                (
                    "remove_failures",
                    failure_count(
                        crate::retention::orphan_stores::CollectionFailureKind::RemoveFailed,
                    )
                    .to_string(),
                ),
                ("errors", report.outcome.errors.len().to_string()),
            ],
        );
    }

    let unregistered_report = crate::retention::orphan_stores::sweep_unregistered_stores(
        database,
        profile_root,
        retention_secs,
        now,
        true,
    )
    .await;
    let unregistered_report = match unregistered_report {
        Ok(report) => report,
        Err(_) => {
            log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "unregistered_store_sweep".to_string()),
                    ("failure", "registry_read_failed".to_string()),
                ],
            );
            return false;
        }
    };
    let unregistered_bytes = unregistered_report
        .plan
        .collect
        .iter()
        .chain(unregistered_report.plan.retained_immature.iter())
        .fold(0u64, |total, finding| {
            total.saturating_add(finding.size_bytes)
        });
    let unregistered_count = unregistered_report
        .plan
        .collect
        .len()
        .saturating_add(unregistered_report.plan.retained_immature.len());
    if !unregistered_report.outcome.collected.is_empty()
        || !unregistered_report.outcome.errors.is_empty()
        || unregistered_count > 0
    {
        log_daemon_event(
            "retention_unregistered_stores",
            &[
                (
                    "collected",
                    unregistered_report.outcome.collected.len().to_string(),
                ),
                ("unregistered", unregistered_count.to_string()),
                ("unregistered_bytes", unregistered_bytes.to_string()),
                (
                    "reclaimed_bytes",
                    unregistered_report.outcome.reclaimed_bytes.to_string(),
                ),
                (
                    "errors",
                    unregistered_report.outcome.errors.len().to_string(),
                ),
            ],
        );
    }

    report.outcome.errors.is_empty()
        && !registry_retirement_failed
        && unregistered_report.outcome.errors.is_empty()
}

/// Quarantines and collects incident debris on its own retention policy. This
/// pass is independent from orphan-store GC: disabling or failing one must not
/// suppress recovery/corruption artifact ownership for otherwise-live stores.
pub(super) async fn run_incident_debris_sweep(
    database: &crate::global_db::RegisteredGlobalDb,
    profile_root: &Path,
    retention_days: u64,
) -> bool {
    let retention_secs = retention_window_secs(retention_days);
    let now = now_secs_i64();
    let debris_report =
        match crate::retention::orphan_stores::build_store_census(database, profile_root).await {
            Ok(census) => crate::retention::incident_debris::sweep_incident_debris(
                &census,
                profile_root,
                retention_secs,
                now,
            ),
            Err(_) => {
                log_daemon_event(
                    "retention_degraded",
                    &[
                        ("pass", "incident_debris".to_string()),
                        ("failure", "registry_read_failed".to_string()),
                    ],
                );
                return false;
            }
        };
    if debris_report.quarantined > 0
        || debris_report.collected > 0
        || !debris_report.errors.is_empty()
    {
        log_daemon_event(
            "retention_incident_debris",
            &[
                ("quarantined", debris_report.quarantined.to_string()),
                ("collected", debris_report.collected.to_string()),
                ("retained", debris_report.retained.to_string()),
                ("reclaimed_bytes", debris_report.reclaimed_bytes.to_string()),
                ("errors", debris_report.errors.len().to_string()),
            ],
        );
    }
    debris_report.errors.is_empty()
}
