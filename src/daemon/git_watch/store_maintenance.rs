//! Store-maintenance operations performed by the daemon git watcher.
//!
//! Every operation that opens, tracks, or garbage-collects a store lives here
//! so its [`StoreAdministration`] lifetime is kept separate from the watcher
//! state machine.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use libsql::Connection;

use crate::branch::BranchAdminAction;
use crate::config::{CompactionThresholdConfig, RetentionConfig};
use crate::global_db::GlobalDb;
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

use super::super::{branch_admin::StoreAdministration, log_daemon_event};
use super::GitWatcherInner;

const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// Opens the project store and runs a diff-scoped incremental sync (or a full
/// sync when the diff base is missing / oversized). Returns true on success.
/// `SyncLock` is treated as success (a peer synced).
///
/// The `TraceDecay` sync/open futures are `Send` (the sync path scopes its
/// `!Send` `gix` values so they drop before every `.await`; see
/// `indexing::stamp_last_synced_commit`), so this awaits them directly on the
/// caller's task under the daemon-wide sync semaphore — no nested runtime.
pub(super) async fn sync_project(
    root: &Path,
    opts: &TraceDecayOpenOptions,
    escalation: usize,
    administration: &StoreAdministration,
) -> bool {
    // Hold the administration gate from before opening the store until the
    // `TraceDecay` handle drops. This prevents branch-store GC from selecting
    // or unlinking the SQLite family while a watcher sync owns it.
    administration
        .with_writer(|| async {
            let Ok(cg) = TraceDecay::open_with_options(root, opts.clone()).await else {
                return false;
            };
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
            let synced = matches!(
                result,
                Ok(()) | Err(crate::errors::TraceDecayError::SyncLock { .. })
            );
            drop(cg);
            synced
        })
        .await
}

/// Proactively tracks a linked worktree's branch. Returns the
/// [`crate::branch::BranchAddOutcome`] name for logging, or `None` on error.
pub(super) async fn track_worktree_branch(
    administration: &StoreAdministration,
    wt_root: PathBuf,
    branch: String,
    opts: TraceDecayOpenOptions,
) -> Option<String> {
    administration
        .with_writer(move || async move {
            match TraceDecay::add_branch_tracking_with_options(&wt_root, &branch, opts).await {
                Ok(outcome) => Some(format!("{outcome:?}")),
                Err(_) => None,
            }
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

/// Runs branch-store GC for a project through the daemon administration
/// coordinator, logging what it removed. Returns `false` when layout resolution
/// or administration fails so the backstop keeps the GC cadence eligible for a
/// retry.
pub(super) async fn run_gc(
    inner: &Arc<GitWatcherInner>,
    root: &Path,
    opts: &TraceDecayOpenOptions,
) -> bool {
    // Layout discovery is read-only and deliberately stays outside both writer
    // gates. Only the coordinator performs the destructive administration.
    let data_root = match TraceDecay::try_initialized_store_layout_with_options(root, opts).await {
        Ok(Some(layout)) => layout.data_root,
        Ok(None) => return true,
        Err(error) => {
            log_daemon_event(
                "git_watch_degraded",
                &[
                    ("project", root.display().to_string()),
                    ("reason", "branch_gc_layout_failed".to_string()),
                    ("error", error.to_string()),
                ],
            );
            return false;
        }
    };

    // Preserve the sync-semaphore → administration-gate acquisition order used
    // by sync and worktree tracking. The coordinator owns the writer gate and
    // its process/store-holder safety checks.
    let _permit = inner.sync_semaphore.acquire().await;
    let report = inner
        .administration
        .execute_branch_admin_in_layout(
            root,
            &data_root,
            BranchAdminAction::Gc,
            inner.config.branch_gc_days,
            inner.config.orphan_db_gc_days,
        )
        .await;
    let report = match report {
        Ok(report) => report,
        Err(error) => {
            log_daemon_event(
                "git_watch_degraded",
                &[
                    ("project", root.display().to_string()),
                    ("reason", "branch_gc_deferred".to_string()),
                    ("error", error.to_string()),
                ],
            );
            return false;
        }
    };

    if !report.removed_branches.is_empty() || !report.removed_orphan_dbs.is_empty() {
        log_daemon_event(
            "git_watch_synced",
            &[
                ("project", root.display().to_string()),
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

/// Reads a single-value integer `PRAGMA` off `conn`, defaulting to zero when the
/// pragma is unavailable. Best-effort: compaction sampling never fails a tick.
async fn pragma_u64(conn: &Connection, pragma: &str) -> u64 {
    let sql = format!("PRAGMA {pragma}");
    let Ok(mut rows) = conn.query(&sql, ()).await else {
        return 0;
    };
    match rows.next().await {
        Ok(Some(row)) => row.get::<i64>(0).unwrap_or(0).max(0) as u64,
        _ => 0,
    }
}

/// Runs the profile session-store retention passes (Plan 38 §3/§4/§6): LCM
/// session retention, observation-evidence retention, then a compaction
/// (incremental-vacuum) reclaim of the pages the passes freed. Every pass is
/// individually config-gated and inert unless the owner opened a window, so a
/// default configuration performs no work here. Fail-open: a store that cannot
/// be opened (never ingested, migrating) is skipped without degrading the tick.
///
/// Applied off the hot path from the backstop cadence; each engine is bounded
/// per run by its own `max_batch_size`/`max_pages_per_tick`, so a single tick
/// never competes with foreground writes (Plan 38 non-goal).
pub(super) async fn run_session_retention(profile_root: &Path, config: &RetentionConfig) {
    // No opened window anywhere ⇒ nothing to open or scan.
    if !config.session_lcm.enabled && !config.observation.enabled && config.compaction.is_none() {
        return;
    }
    let sessions_db_path = profile_root.join(crate::storage::SESSIONS_DB_FILENAME);
    if !sessions_db_path.exists() {
        return;
    }
    let Some(db) = GlobalDb::open_at(&sessions_db_path).await else {
        return;
    };
    let now = now_secs_i64();

    if config.session_lcm.enabled {
        match db
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
                if reclaimed > 0 || !report.errors.is_empty() {
                    log_daemon_event(
                        "retention_session_lcm",
                        &[
                            ("store", sessions_db_path.display().to_string()),
                            ("bytes_reclaimed", reclaimed.to_string()),
                            ("errors", report.errors.len().to_string()),
                        ],
                    );
                }
            }
            Err(error) => log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "session_lcm".to_string()),
                    ("error", error.to_string()),
                ],
            ),
        }
    }

    if config.observation.enabled {
        match db
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
                if reclaimed > 0 || !report.errors.is_empty() {
                    log_daemon_event(
                        "retention_observation",
                        &[
                            ("store", sessions_db_path.display().to_string()),
                            ("bytes_reclaimed", reclaimed.to_string()),
                            ("errors", report.errors.len().to_string()),
                        ],
                    );
                }
            }
            Err(error) => log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "observation".to_string()),
                    ("error", error.to_string()),
                ],
            ),
        }
    }

    if let Some(compaction) = &config.compaction {
        run_compaction(&db, &sessions_db_path, compaction).await;
    }
}

/// Samples the store's free-page ratio and, when the owner-configured threshold
/// is met, schedules a bounded incremental vacuum in the deferred background
/// lane (Plan 38 §6). The placement is structurally forbidden from competing
/// with foreground writes; the page cap keeps the reclaim off the hot path.
async fn run_compaction(db: &GlobalDb, store: &Path, config: &CompactionThresholdConfig) {
    use tracedecay_application::storage::compaction::CompactionTriggerPolicyV1;
    use tracedecay_application::storage::identity::{
        FreePageRatioV1, StorageByteSizeV1, StoreKeyV1,
    };
    use tracedecay_application::storage::telemetry::StoreSizeSampleV1;
    use tracedecay_domain::UtcMicros;

    let conn = db.read_connection();
    let page_size = pragma_u64(conn, "page_size").await;
    let page_count = pragma_u64(conn, "page_count").await;
    let freelist = pragma_u64(conn, "freelist_count").await;
    if page_size == 0 || page_count == 0 {
        return;
    }
    let store_key = store
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| StoreKeyV1::new(name).ok());
    let Some(store_key) = store_key else {
        return;
    };
    let Ok(page_size_bytes) = u32::try_from(page_size) else {
        return;
    };
    let sample = StoreSizeSampleV1 {
        store: store_key,
        page_size_bytes,
        page_count,
        freelist_pages: freelist,
        observed_at: UtcMicros(now_secs_i64().saturating_mul(1_000_000)),
    };
    let Ok(threshold) = FreePageRatioV1::new(config.free_page_ratio_threshold) else {
        return;
    };
    let policy = CompactionTriggerPolicyV1 {
        free_page_ratio_threshold: threshold,
        minimum_reclaimable_bytes: StorageByteSizeV1(config.minimum_reclaimable_bytes),
    };
    let Ok(decision) = policy.decide(&sample) else {
        return;
    };
    if !decision.is_scheduled() {
        return;
    }
    let pages = config.max_pages_per_tick.max(1);
    let freelist_before = freelist;
    if let Err(error) = conn
        .execute_batch(&format!("PRAGMA incremental_vacuum({pages})"))
        .await
    {
        log_daemon_event(
            "retention_degraded",
            &[
                ("pass", "compaction".to_string()),
                ("error", error.to_string()),
            ],
        );
        return;
    }
    let freelist_after = pragma_u64(conn, "freelist_count").await;
    log_daemon_event(
        "retention_compaction",
        &[
            ("store", store.display().to_string()),
            (
                "freed_pages",
                freelist_before.saturating_sub(freelist_after).to_string(),
            ),
        ],
    );
}

/// Sweeps profile-sharded stores whose project identity no longer resolves to a
/// live repository root and collects those older than the owner-configured
/// window (Plan 38 §2). Re-linkable (moved-repository) stores are never
/// collected — they are surfaced for reconciliation. Fail-open: a registry that
/// cannot be opened is skipped. Applied from the backstop cadence off the hot
/// path; the Doctor surface reports the same findings read-only.
pub(super) async fn run_orphan_store_sweep(
    global_db_path: Option<&Path>,
    profile_root: &Path,
    orphan_store_gc_days: u64,
) {
    let db = match global_db_path {
        Some(path) => GlobalDb::open_at(path).await,
        None => GlobalDb::open().await,
    };
    let Some(db) = db else {
        return;
    };
    let retention_secs = (orphan_store_gc_days as i64).saturating_mul(SECONDS_PER_DAY);
    let report = crate::retention::orphan_stores::sweep_orphan_stores(
        &db,
        profile_root,
        retention_secs,
        now_secs_i64(),
        true,
    )
    .await;
    // Route the classified findings through the typed Doctor Storage-finding
    // constructor (the daemon is the legitimate registry owner; the external
    // `doctor` client never opens the global DB). Re-linkable and still-immature
    // orphans are surfaced even though this apply pass never collects them.
    let doctor_findings = report
        .plan
        .collect
        .iter()
        .chain(report.plan.retained_immature.iter())
        .chain(report.plan.relink.iter())
        .filter_map(crate::doctor::registry_drift::orphan_store_doctor_finding)
        .count();

    if !report.outcome.collected.is_empty()
        || !report.plan.relink.is_empty()
        || !report.outcome.errors.is_empty()
        || doctor_findings > 0
    {
        log_daemon_event(
            "retention_orphan_stores",
            &[
                ("collected", report.outcome.collected.len().to_string()),
                (
                    "reclaimed_bytes",
                    report.outcome.reclaimed_bytes.to_string(),
                ),
                ("relinkable", report.plan.relink.len().to_string()),
                ("doctor_findings", doctor_findings.to_string()),
                (
                    "retired_registry_rows",
                    report.retired_registry_rows.to_string(),
                ),
                ("errors", report.outcome.errors.len().to_string()),
            ],
        );
    }
}
