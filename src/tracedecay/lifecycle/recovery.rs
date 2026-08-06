//! Corruption detection and recovery around the canonical project store.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::db::{Database, DatabaseAccessMode};
use crate::errors::{Result, TraceDecayError};
use crate::storage::StoreLayout;
use crate::tracedecay::ActiveGraphLayout;
use crate::tracedecay::locking::{
    adopt_dirty_marker_at, dirty_marker_owner_is_live, has_dirty_sentinel_at,
    try_acquire_graph_sync_locks,
};

use super::TraceDecay;

/// Outcome of the post-crash / post-open health preflight a registered open
/// runs before it can trust the mounted database.
///
/// `Ready` carries the mounted database onward so the caller can finish
/// constructing `Self`. `Recovered` means the preflight already resolved the
/// open by failing closed with an actionable recovery error, and the caller
/// must return it as-is.
pub(super) enum OpenHealthOutcome {
    Ready { db: Database },
    Recovered(Box<Result<TraceDecay>>),
}

impl TraceDecay {
    /// Runs the crash/health preflight for the canonical registered store.
    pub(super) async fn run_open_health_recovery(
        project_root: &Path,
        store_layout: &StoreLayout,
        db_path: &Path,
        active_graph_layout: &ActiveGraphLayout,
        defer_post_open_health: bool,
        runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    ) -> Result<OpenHealthOutcome> {
        let marker_is_abandoned =
            |path: &Path| has_dirty_sentinel_at(path) && !dirty_marker_owner_is_live(path);
        let crashed = marker_is_abandoned(&active_graph_layout.dirty_path)
            || marker_is_abandoned(&store_layout.dirty_path);
        let mut crash_preflight_healthy = false;
        if crashed {
            eprintln!(
                "[tracedecay] previous operation was interrupted — checking database integrity…"
            );
        }

        // A dirty marker can also describe a sync that is still active in a
        // peer process. Recovery must own both graph-local and legacy locks so
        // it cannot race that writer or clear its sentinel. Preflight through
        // the read-only connection before Database::open applies writable
        // pragmas or migrations to a potentially damaged recovery set.
        let recovery_lock = if crashed {
            Some(try_acquire_graph_sync_locks(
                &active_graph_layout.sync_lock_path,
                &store_layout.sync_lock_path,
            )?)
        } else {
            None
        };
        // Recovery owns exactly the markers it observed once the lease was
        // held. A marker republished after this point describes a newer
        // writer's work and must survive this recovery, so the clear below is
        // scoped to these adopted epochs rather than to whatever the paths
        // happen to hold at commit time.
        let mut adopted_dirty_markers = Vec::new();
        if crashed {
            adopted_dirty_markers.extend(adopt_dirty_marker_at(&active_graph_layout.dirty_path));
            if active_graph_layout.dirty_path != store_layout.dirty_path {
                adopted_dirty_markers.extend(adopt_dirty_marker_at(&store_layout.dirty_path));
            }
        }
        if crashed {
            // FTS-only damage is repairable from the content table on the
            // writable open below; do not force offline recovery for it. The
            // read-only open runs its own integrity validation, so the damage
            // can surface either as its open error or as a problem row here.
            match Self::mount_project_graph(
                runtime_registry.as_ref(),
                project_root,
                store_layout,
                "crash verification",
                DatabaseAccessMode::ReadOnly,
            )
            .await
            {
                Ok(verification) => {
                    let integrity = verification.quick_check_report().await;
                    verification.close();
                    match integrity {
                        Ok(None) => crash_preflight_healthy = true,
                        Ok(Some(problem)) if is_fts_only_corruption(&problem) => {}
                        Ok(Some(problem)) => {
                            drop(recovery_lock);
                            return Ok(OpenHealthOutcome::Recovered(Box::new(
                                Self::recover_corrupt_store_or_fail(
                                    db_path,
                                    format!("read-only SQLite quick_check reported: {problem}"),
                                )
                                .await,
                            )));
                        }
                        Err(error) => {
                            drop(recovery_lock);
                            return Ok(OpenHealthOutcome::Recovered(Box::new(
                                Self::recover_corrupt_store_or_fail(db_path, error).await,
                            )));
                        }
                    }
                }
                Err(error) if is_fts_only_corruption(&error.to_string()) => {}
                // A hot rollback journal from an interrupted writer needs
                // write access to recover, so the read-only preflight cannot
                // open it at all. That is normal crash recovery, not damage:
                // defer to the writable open below, which rolls the journal
                // back and still runs the post-open quick_check.
                Err(error) if is_readonly_recovery_block(&error.to_string()) => {}
                Err(error) => {
                    drop(recovery_lock);
                    return Ok(OpenHealthOutcome::Recovered(Box::new(
                        Self::recover_corrupt_store_or_fail(db_path, error).await,
                    )));
                }
            }
        }

        // Ordinary opens never replace database files. A daemon or another MCP
        // process may still hold the current DB/WAL/SHM inodes, and deleting
        // them here would split readers and writers across different stores.
        let mut open_result = Self::mount_project_graph(
            runtime_registry.as_ref(),
            project_root,
            store_layout,
            "open project store",
            DatabaseAccessMode::ReadWrite,
        )
        .await;
        // Open-time validation fails closed on any corruption, including
        // FTS-only damage that is fully derivable from the content table.
        // Rebuild that index under the open's writer authority and retry
        // once; stores corrupted by a live writer carry no dirty sentinel,
        // so this repair cannot be gated on the crash path.
        if let Err(error) = &open_result
            && is_fts_only_corruption(&error.to_string())
        {
            eprintln!("[tracedecay] repairing FTS index after interrupted operation ({error})…");
            match Self::mount_project_graph(
                runtime_registry.as_ref(),
                project_root,
                store_layout,
                "remount project store for FTS repair",
                DatabaseAccessMode::ReadWrite,
            )
            .await
            {
                Ok(database) => match database.repair_fts_after_open().await {
                    Ok(_) => open_result = Ok(database),
                    Err(repair_error) => {
                        database.close();
                        drop(recovery_lock);
                        return Ok(OpenHealthOutcome::Recovered(Box::new(
                            Self::recover_corrupt_store_or_fail(db_path, repair_error).await,
                        )));
                    }
                },
                Err(repair_error) => {
                    drop(recovery_lock);
                    return Ok(OpenHealthOutcome::Recovered(Box::new(
                        Self::recover_corrupt_store_or_fail(db_path, repair_error).await,
                    )));
                }
            }
        }
        let db = match open_result {
            Ok(database) => database,
            Err(e) if Database::is_corruption_error(&e) || crashed => {
                drop(recovery_lock);
                return Ok(OpenHealthOutcome::Recovered(Box::new(
                    Self::recover_corrupt_store_or_fail(db_path, e).await,
                )));
            }
            Err(e) => return Err(e),
        };
        crate::db::migrations::ensure_schema_current(&db).await?;

        // Validation before Database::open cannot observe FTS damage on a
        // retained shared handle because the open reuses that connection.
        // Classify its complete quick-check report after open and schedule the
        // existing rebuild through the canonical writer lane. Non-FTS damage
        // fails closed without entering either repair path.
        // The crash preflight already ran the same complete quick-check while
        // holding both recovery locks. Repeating it after the writable mount
        // doubles peak SQLite scratch memory without adding evidence. Ordinary
        // opens still run this retained-handle check so live-writer FTS damage
        // without a dirty marker remains detectable.
        if !crash_preflight_healthy && !defer_post_open_health {
            match db.repair_fts_after_open().await {
                Ok(Some(problem)) => {
                    eprintln!(
                        "[tracedecay] repaired FTS index after post-open health check ({problem})"
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    db.close();
                    drop(recovery_lock);
                    return Ok(OpenHealthOutcome::Recovered(Box::new(
                        Self::recover_corrupt_store_or_fail(db_path, error).await,
                    )));
                }
            }
        }

        if crashed && crash_preflight_healthy {
            for marker in &adopted_dirty_markers {
                marker.clear();
            }
        }

        // If the sentinel was set but the read-only preflight could not prove
        // the database healthy, validate the writable recovery before clearing
        // either marker.
        if crashed && !crash_preflight_healthy {
            let mut integrity = db.quick_check_report().await;
            // An interrupted bulk load can desync the FTS5 inverted index from
            // its content table. That damage is fully derivable: rebuild it in
            // place under the held recovery locks instead of failing closed.
            if let Ok(Some(problem)) = &integrity
                && is_fts_only_corruption(problem)
            {
                eprintln!(
                    "[tracedecay] repairing FTS index after interrupted operation ({problem})…"
                );
                match db.rebuild_fts().await {
                    Ok(()) => integrity = db.quick_check_report().await,
                    Err(error) => {
                        db.close();
                        drop(recovery_lock);
                        return Ok(OpenHealthOutcome::Recovered(Box::new(
                            Self::recover_corrupt_store_or_fail(db_path, error).await,
                        )));
                    }
                }
            }
            match integrity {
                Ok(None) => {
                    for marker in &adopted_dirty_markers {
                        marker.clear();
                    }
                }
                Ok(Some(problem)) => {
                    db.close();
                    drop(recovery_lock);
                    return Ok(OpenHealthOutcome::Recovered(Box::new(
                        Self::recover_corrupt_store_or_fail(
                            db_path,
                            format!("SQLite quick_check reported: {problem}"),
                        )
                        .await,
                    )));
                }
                Err(e) => {
                    db.close();
                    drop(recovery_lock);
                    return Ok(OpenHealthOutcome::Recovered(Box::new(
                        Self::recover_corrupt_store_or_fail(db_path, e).await,
                    )));
                }
            }
        }

        Ok(OpenHealthOutcome::Ready { db })
    }

    async fn recover_corrupt_store_or_fail(
        db_path: &Path,
        detail: impl std::fmt::Display,
    ) -> Result<Self> {
        let detail = detail.to_string();
        print_corruption_warning(db_path);
        Err(recovery_required_error(db_path, detail))
    }
}

fn graph_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut file_name = db_path.file_name().unwrap_or_default().to_os_string();
    file_name.push(suffix);
    db_path.with_file_name(file_name)
}

pub(super) fn active_graph_layout(db_path: &Path) -> ActiveGraphLayout {
    ActiveGraphLayout {
        dirty_path: graph_sidecar_path(db_path, ".dirty"),
        sync_lock_path: graph_sidecar_path(db_path, ".sync.lock"),
    }
}

/// Whether a `PRAGMA quick_check` problem row describes damage confined to the
/// graph's FTS5 index (e.g. "malformed inverted index for FTS5 table
/// `main.nodes_fts`"). Such damage is fully derivable from the content table via
/// [`crate::db::Database::rebuild_fts`] and never requires offline recovery.
pub(crate) fn is_fts_only_corruption(problem: &str) -> bool {
    problem.contains("malformed inverted index for FTS5 table main.nodes_fts")
        || problem.contains("malformed inverted index for FTS5 table nodes_fts")
        || (problem.contains("fts5: corruption found") && problem.contains("nodes_fts"))
}

/// Whether a read-only preflight failure means the store needs ordinary
/// writable crash recovery (e.g. a hot rollback journal), which a read-only
/// connection can never perform, rather than actual damage.
fn is_readonly_recovery_block(problem: &str) -> bool {
    problem.contains("attempt to write a readonly database")
}

/// Build an actionable error without replacing any member of the `SQLite`
/// recovery set.
fn recovery_required_error(
    db_path: &std::path::Path,
    detail: impl std::fmt::Display,
) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!(
            "database recovery required at '{}'; DB/WAL/SHM and dirty sentinel were preserved: {detail}",
            db_path.display()
        ),
        operation: "open_recovery_required".to_string(),
    }
}

fn print_corruption_warning(db_path: &std::path::Path) {
    let version = env!("CARGO_PKG_VERSION");
    eprintln!("[tracedecay] \x1b[33m⚠ database recovery required — store preserved\x1b[0m");
    eprintln!("[tracedecay]");
    eprintln!("[tracedecay] Store: {}", db_path.display());
    eprintln!("[tracedecay] Stop TraceDecay daemon/MCP processes before explicit repair.");
    eprintln!("[tracedecay] Preserve the DB, WAL, SHM, and dirty sentinel as one recovery set.");
    eprintln!("[tracedecay] Run `tracedecay doctor` from the project root for exact paths.");
    eprintln!("[tracedecay] Please report this at:");
    eprintln!("[tracedecay]   https://github.com/ScriptedAlchemy/tracedecay/issues");
    eprintln!(
        "[tracedecay]   Include: tracedecay version (v{version}), OS, and what happened before the crash."
    );
    eprintln!("[tracedecay]");
}
