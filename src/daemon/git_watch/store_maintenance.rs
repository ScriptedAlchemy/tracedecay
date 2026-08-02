//! Store-maintenance operations performed by the daemon git watcher.
//!
//! Every operation that opens, tracks, or garbage-collects a store lives here
//! so its [`StoreAdministration`] lifetime is kept separate from the watcher
//! state machine.

use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use crate::branch::BranchAdminAction;
use crate::config::{CompactionThresholdConfig, RetentionConfig};
use crate::tracedecay::TraceDecay;

#[cfg(unix)]
use super::branch_admin::{StoreAdministration, StoreWriterClass};
#[cfg(unix)]
use super::git_watch::GitWatcherInner;
use super::log_daemon_event;

/// Opens the project store and runs a diff-scoped incremental sync (or a full
/// sync when the diff base is missing / oversized). Returns true on success.
/// `SyncLock` is treated as success (a peer synced).
///
/// The `TraceDecay` sync/open futures are `Send` (the sync path scopes its
/// `!Send` `gix` values so they drop before every `.await`; see
/// `indexing::stamp_last_synced_commit`), so this awaits them directly on the
/// caller's task under the daemon-wide sync semaphore — no nested runtime.
#[cfg(unix)]
pub(super) async fn sync_project(
    cg: &TraceDecay,
    escalation: usize,
    administration: &StoreAdministration,
) -> bool {
    // The sync must still exclude branch-store GC — GC selects and unlinks the
    // SQLite family this sync is writing into — but nothing else. It therefore
    // takes the store's *content* lane rather than daemon-wide exclusion.
    //
    // What that changes: `cg.sync()` is O(store) and runs for minutes on a large
    // index. Under the old single daemon-wide gate it blocked every writer in
    // the process, including the first project-open of an unrelated project. Now
    // it blocks only this store's destructive lane and other content writers on
    // this same store, which is exactly the protection the original comment
    // asked for. Owner bookkeeping for this store — project open, owner rekey,
    // scheduler transitions — proceeds beside it.
    administration
        .with_writer_in(
            crate::daemon::branch_admin::graph_writer_scope(cg, StoreWriterClass::Content),
            || async {
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
            },
        )
        .await
}

/// Proactively tracks a linked worktree's branch. Returns the
/// [`crate::branch::BranchAddOutcome`] name for logging, or `None` on error.
#[cfg(unix)]
pub(super) async fn track_worktree_branch(
    administration: &StoreAdministration,
    cg: &TraceDecay,
    wt_root: PathBuf,
    branch: String,
) -> Option<String> {
    administration
        .with_writer_in(
            crate::daemon::branch_admin::graph_writer_scope(cg, StoreWriterClass::Owner),
            || async {
                cg.track_worktree_branch(&wt_root, &branch)
                    .await
                    .ok()
                    .map(|outcome| format!("{outcome:?}"))
            },
        )
        .await
}

/// Resolves a `worktrees/<name>` leaf to `(worktree_root, branch)` by reading
/// its `gitdir` file and the linked HEAD.
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
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

/// Collect superseded code-index generations for one mounted project.
///
/// Sealed generations are ordinary files, so no database retention or
/// compaction pass reclaims them. This runs on the ordinary maintenance cadence
/// and is independent of the semantic projection lane: the only previous caller
/// sat inside legacy vector migration, so a profile with semantic search
/// disabled never collected anything and grew without bound.
///
/// Vector-readable source generations are pinned, so the inventory read is
/// required before any sweep. When the inventory cannot be read this pass
/// reports failure and collects nothing rather than sweeping with an empty
/// protection set, which would delete generations vectors still read from.
pub(super) async fn run_code_generation_retention(graph: &TraceDecay) -> bool {
    use crate::retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        run_code_generation_retention as run_retention,
    };
    use crate::semantic_code::legacy_migration::LegacyVectorInventoryPortV1;
    use crate::store::vector_generations::DatabaseVectorGenerationStoreV1;

    let layout = graph.hook_store_layout();
    let store_root = code_index_store_root(&layout.data_root, &layout.project_root);
    // No published generation means nothing has been sealed for this project.
    if !store_root.join("active-code-generation-v1.json").is_file() {
        return true;
    }

    // Hold the canonical graph writer lane from the pin read through durable
    // filesystem publication. A vector-generation writer cannot publish a new
    // readable source between this inventory snapshot and deletion.
    let writer = match graph
        .db()
        .begin_write_transaction("code generation retention pin fence")
        .await
    {
        Ok(writer) => writer,
        Err(_) => {
            log_code_generation_retention_degraded("vector_writer_lane_unavailable");
            return false;
        }
    };
    let vector_readable_sources =
        match DatabaseVectorGenerationStoreV1::open_legacy_migration(graph.db()).await {
            Ok(store) => match store.read_legacy_inventory().await {
                Ok(inventory) => match inventory.read_only_inventory() {
                    Ok(inventory) => inventory.retained_readable_sources(),
                    Err(_) => {
                        log_code_generation_retention_degraded("vector_inventory_unreadable");
                        return false;
                    }
                },
                Err(_) => {
                    log_code_generation_retention_degraded("vector_inventory_read_failed");
                    return false;
                }
            },
            Err(_) => {
                log_code_generation_retention_degraded("vector_generation_store_unavailable");
                return false;
            }
        };

    let completed_at = tracedecay_domain::UtcMicros(crate::tracedecay::current_timestamp());
    let report = tokio::task::spawn_blocking(move || {
        run_retention(
            &store_root,
            &vector_readable_sources,
            DEFAULT_SUPERSEDED_GENERATION_FLOOR,
            CodeGenerationRetentionModeV1::Apply,
            completed_at,
        )
    })
    .await;
    if writer.rollback().await.is_err() {
        log_code_generation_retention_degraded("vector_writer_lane_release_failed");
        return false;
    }

    match report {
        Ok(Ok(report)) => {
            let reclaimed = report.receipt.as_ref().map_or_else(
                || {
                    report
                        .deleted_generations
                        .iter()
                        .map(|generation| generation.size_bytes)
                        .sum()
                },
                |receipt| receipt.reclaimed_bytes,
            );
            if reclaimed > 0 {
                log_daemon_event(
                    "retention_code_generations",
                    &[
                        ("store", "code-index-v1".to_string()),
                        ("bytes_reclaimed", reclaimed.to_string()),
                        (
                            "generations_collected",
                            report.deleted_generations.len().to_string(),
                        ),
                    ],
                );
            }
            true
        }
        Ok(Err(_)) => {
            log_code_generation_retention_degraded("retention_pass_failed");
            false
        }
        Err(_) => {
            log_code_generation_retention_degraded("retention_task_panicked");
            false
        }
    }
}

/// Reconcile whole code-index *scope roots* for one mounted repository.
///
/// Generation retention above is scoped to a single
/// `code-index-v1/<sha256(canonical_project_root)>/` directory, and every caller
/// derives exactly one such scope from the root it was handed. Nothing has ever
/// enumerated the siblings, so a scope whose project root is gone — a deleted
/// agent worktree is the ordinary cause — is unreachable by any retention pass
/// and uncounted by any report. One dogfood repository carried three scope
/// directories, two of them orphaned, holding 7.2 GiB nothing could see.
///
/// The pass is fail-closed by construction. It collects only when it can prove
/// the *complete* live-root set for the repository: the mounted project root,
/// the primary checkout, and every linked worktree git itself records. If any
/// part of that enumeration cannot be read, the pass collects nothing and names
/// the failure, because an empty or truncated live set would otherwise read as
/// "every scope on disk is stranded".
pub(super) async fn run_code_index_scope_reconciliation(graph: &TraceDecay) -> bool {
    use crate::retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        run_scope_root_retention,
    };

    let layout = graph.hook_store_layout();
    let store_root = code_index_scope_store_root(&layout.data_root);
    if !store_root.is_dir() {
        return true;
    }

    let project_root = layout.project_root.clone();
    let now_secs = now_secs_i64();
    let completed_at = tracedecay_domain::UtcMicros(crate::tracedecay::current_timestamp());
    // Repository discovery and the scope walk are both blocking filesystem work;
    // neither belongs on the async authority lane.
    let report = tokio::task::spawn_blocking(move || {
        let live_roots = resolve_live_code_index_roots(&project_root)?;
        run_scope_root_retention(
            &store_root,
            &live_roots,
            DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
            CodeGenerationRetentionModeV1::Apply,
            now_secs,
            completed_at,
        )
        .map_err(|_| "scope_reconciliation_pass_failed")
    })
    .await;

    match report {
        Ok(Ok(report)) => {
            let reclaimed = report
                .receipt
                .as_ref()
                .map_or(0, |receipt| receipt.reclaimed_bytes);
            if reclaimed > 0 || report.plan.stranded_scope_count() > 0 {
                log_daemon_event(
                    "retention_code_index_scopes",
                    &[
                        ("store", "code-index-v1".to_string()),
                        ("live_scopes", report.plan.live_scope_count.to_string()),
                        (
                            "stranded_scopes",
                            report.plan.stranded_scope_count().to_string(),
                        ),
                        (
                            "stranded_bytes",
                            report.plan.stranded_scope_bytes().to_string(),
                        ),
                        (
                            "retained_immature_scopes",
                            report.plan.retained_immature_scopes.len().to_string(),
                        ),
                        (
                            "refused_scopes",
                            report.plan.refused_scopes.len().to_string(),
                        ),
                        (
                            "collected_scopes",
                            report.collected_scopes.len().to_string(),
                        ),
                        ("bytes_reclaimed", reclaimed.to_string()),
                    ],
                );
            }
            true
        }
        Ok(Err(failure)) => {
            log_code_index_scope_reconciliation_degraded(failure);
            false
        }
        Err(_) => {
            log_code_index_scope_reconciliation_degraded("scope_reconciliation_task_panicked");
            false
        }
    }
}

/// The complete set of canonical project roots that may legitimately own a
/// scope directory under this repository's `code-index-v1/`.
///
/// Scope directories under one `data_root` all belong to one repository: linked
/// worktrees share a git common directory and therefore one project store, and
/// differ only by the per-worktree canonical root the scope hash is derived
/// from. So the authoritative live set is exactly git's own worktree registry
/// for that repository, read from the same `<common>/worktrees/<name>/gitdir`
/// leaves `git worktree list` reports, plus the mounted root and the primary
/// checkout.
///
/// Every failure is an `Err`, never a smaller set: a truncated live set is
/// indistinguishable from stranding and would authorize deletion.
pub(super) fn resolve_live_code_index_roots(
    project_root: &Path,
) -> Result<std::collections::BTreeSet<PathBuf>, &'static str> {
    let mut roots = std::collections::BTreeSet::new();
    insert_live_root_variants(&mut roots, project_root);

    let Some(common) = crate::worktree::git_common_dir(project_root) else {
        return Err("git_common_dir_unresolved");
    };
    // `<repo>/.git` is the common directory of the primary checkout; its parent
    // is that checkout's root.
    if common.file_name().is_some_and(|name| name == ".git")
        && let Some(primary) = common.parent()
    {
        insert_live_root_variants(&mut roots, primary);
    }

    match std::fs::read_dir(common.join("worktrees")) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|_| "worktree_registry_unreadable")?;
                if !entry
                    .file_type()
                    .map_err(|_| "worktree_registry_unreadable")?
                    .is_dir()
                {
                    continue;
                }
                let raw = std::fs::read_to_string(entry.path().join("gitdir"))
                    .map_err(|_| "worktree_gitdir_unreadable")?;
                // `gitdir` points at `<worktree>/.git`; the worktree root is its
                // parent. A worktree whose directory has been removed but whose
                // metadata git still holds stays live here on purpose: only git
                // pruning it makes its scope collectable.
                let gitdir = PathBuf::from(raw.trim());
                let root = gitdir.parent().ok_or("worktree_gitdir_malformed")?;
                insert_live_root_variants(&mut roots, root);
            }
        }
        // A repository with no linked worktrees has no `worktrees/` directory.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("worktree_registry_unreadable"),
    }

    if roots.is_empty() {
        return Err("live_root_set_empty");
    }
    Ok(roots)
}

/// Record both the literal path and its symlink-resolved form. The scope hash
/// is taken over the canonical root string recorded at publication time, and a
/// live root spelled differently must never be mistaken for a dead one.
fn insert_live_root_variants(roots: &mut std::collections::BTreeSet<PathBuf>, root: &Path) {
    roots.insert(root.to_path_buf());
    if let Ok(resolved) = std::fs::canonicalize(root) {
        roots.insert(resolved);
    }
}

/// The shared `code-index-v1/` parent that holds every scope root for one
/// repository. Scope reconciliation operates here; generation retention
/// operates one level down.
pub(super) fn code_index_scope_store_root(data_root: &Path) -> PathBuf {
    data_root.join("code-index-v1")
}

/// Durable failure visibility for scope reconciliation. Every refusal names why
/// so a fail-closed pass is never mistaken for "nothing was stranded".
fn log_code_index_scope_reconciliation_degraded(failure: &str) {
    log_daemon_event(
        "retention_degraded",
        &[
            ("pass", "code_index_scopes".to_string()),
            ("failure", failure.to_string()),
        ],
    );
}

/// The exact per-project code-index store root this cadence sweeps.
///
/// This must stay the scoped root the scheduler publishes into and Doctor
/// reports on. A cadence pointed at any other directory would find no sealed
/// generations and silently reclaim nothing, which is the failure this pass
/// exists to end.
pub(super) fn code_index_store_root(data_root: &Path, project_root: &Path) -> PathBuf {
    crate::retention::code_index_generations::scoped_code_index_store_root(
        &code_index_scope_store_root(data_root),
        project_root,
    )
}

/// Durable failure visibility for the code-generation retention pass. A silent
/// skip is what let generation growth go unnoticed, so every refusal names why.
fn log_code_generation_retention_degraded(failure: &str) {
    log_daemon_event(
        "retention_degraded",
        &[
            ("pass", "code_generations".to_string()),
            ("failure", failure.to_string()),
        ],
    );
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
