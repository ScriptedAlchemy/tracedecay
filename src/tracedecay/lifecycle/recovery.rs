//! Project-store corruption detection and crash recovery preflight.

use std::path::Path;
use std::sync::Arc;

use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::db::{Database, DatabaseAccessMode};
use crate::errors::{Result, TraceDecayError};
use crate::storage::StoreLayout;
use crate::tracedecay::locking::{
    adopt_dirty_marker_at, dirty_marker_owner_is_live, has_dirty_sentinel_at,
    try_acquire_sync_lock_at,
};

use super::TraceDecay;

impl TraceDecay {
    /// Runs crash and integrity preflight against the one project-wide graph
    /// store. Corruption fails closed with the authoritative SQLite recovery
    /// set untouched; only the rebuildable FTS projection is repaired in
    /// place.
    pub(super) async fn run_open_health_recovery(
        project_root: &Path,
        store_layout: &StoreLayout,
        db_path: &Path,
        defer_post_open_health: bool,
        runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    ) -> Result<Database> {
        let crashed = has_dirty_sentinel_at(&store_layout.dirty_path)
            && !dirty_marker_owner_is_live(&store_layout.dirty_path);
        let mut crash_preflight_healthy = false;
        if crashed {
            eprintln!(
                "[tracedecay] previous operation was interrupted — checking database integrity…"
            );
        }

        // A structured marker can describe live work in a peer process.
        // Recovery adopts the exact abandoned epoch only while holding the
        // project store's writer lease, so a newer marker is never cleared.
        let recovery_lock = if crashed {
            Some(try_acquire_sync_lock_at(&store_layout.sync_lock_path)?)
        } else {
            None
        };
        let adopted_dirty_marker = crashed
            .then(|| adopt_dirty_marker_at(&store_layout.dirty_path))
            .flatten();

        if matches!(
            crate::storage::has_sqlite_database_header(db_path),
            Ok(false)
        ) {
            drop(recovery_lock);
            return Err(recovery_required_error(
                db_path,
                "invalid SQLite database header",
            ));
        }

        if crashed {
            // Preflight before writable SQLite recovery can alter sidecars.
            // A hot rollback journal can require a writable open and therefore
            // is handled by the ordinary mount below.
            match Self::mount_project_graph(
                runtime_registry.as_ref(),
                project_root,
                store_layout,
                db_path,
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
                            return Err(recovery_required_error(
                                db_path,
                                format!("read-only SQLite quick_check reported: {problem}"),
                            ));
                        }
                        Err(error) => {
                            drop(recovery_lock);
                            return Err(recovery_required_error(db_path, error));
                        }
                    }
                }
                Err(error) if is_fts_only_corruption(&error.to_string()) => {}
                Err(error) if is_readonly_recovery_block(&error.to_string()) => {}
                Err(error) => {
                    drop(recovery_lock);
                    return Err(recovery_required_error(db_path, error));
                }
            }
        }

        // The project store is never replaced during open. Replacing its
        // DB/WAL/SHM while another retained handle exists would split one
        // logical project across inode sets.
        let mut open_result = Self::mount_project_graph(
            runtime_registry.as_ref(),
            project_root,
            store_layout,
            db_path,
            "open project store",
            DatabaseAccessMode::ReadWrite,
        )
        .await;

        // FTS is derived from the canonical content table and can be rebuilt
        // through the registered writer without replacing the store.
        if let Err(error) = &open_result
            && is_fts_only_corruption(&error.to_string())
        {
            eprintln!("[tracedecay] repairing FTS index after interrupted operation ({error})…");
            match Self::mount_project_graph(
                runtime_registry.as_ref(),
                project_root,
                store_layout,
                db_path,
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
                        return Err(recovery_required_error(db_path, repair_error));
                    }
                },
                Err(repair_error) => {
                    drop(recovery_lock);
                    return Err(recovery_required_error(db_path, repair_error));
                }
            }
        }

        let db = match open_result {
            Ok(database) => database,
            Err(error) if Database::is_corruption_error(&error) || crashed => {
                drop(recovery_lock);
                return Err(recovery_required_error(db_path, error));
            }
            Err(error) => return Err(error),
        };
        crate::db::migrations::ensure_schema_current(&db).await?;

        // A retained runtime may bypass the read-only preflight's connection,
        // so ordinary opens still validate the mounted handle.
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
                    return Err(recovery_required_error(db_path, error));
                }
            }
        }

        if crashed && crash_preflight_healthy {
            if let Some(marker) = &adopted_dirty_marker {
                marker.clear();
            }
        }

        // Writable rollback-journal recovery still requires a complete
        // quick-check before the adopted marker can be cleared.
        if crashed && !crash_preflight_healthy {
            let mut integrity = db.quick_check_report().await;
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
                        return Err(recovery_required_error(db_path, error));
                    }
                }
            }
            match integrity {
                Ok(None) => {
                    if let Some(marker) = &adopted_dirty_marker {
                        marker.clear();
                    }
                }
                Ok(Some(problem)) => {
                    db.close();
                    drop(recovery_lock);
                    return Err(recovery_required_error(
                        db_path,
                        format!("SQLite quick_check reported: {problem}"),
                    ));
                }
                Err(error) => {
                    db.close();
                    drop(recovery_lock);
                    return Err(recovery_required_error(db_path, error));
                }
            }
        }

        Ok(db)
    }
}

/// Whether a `PRAGMA quick_check` problem row describes damage confined to the
/// rebuildable graph FTS5 projection.
pub(crate) fn is_fts_only_corruption(problem: &str) -> bool {
    problem.contains("malformed inverted index for FTS5 table main.nodes_fts")
        || problem.contains("malformed inverted index for FTS5 table nodes_fts")
        || (problem.contains("fts5: corruption found") && problem.contains("nodes_fts"))
}

/// A hot rollback journal may require writable SQLite recovery, which a
/// read-only connection cannot perform.
fn is_readonly_recovery_block(problem: &str) -> bool {
    problem.contains("attempt to write a readonly database")
}

fn recovery_required_error(db_path: &Path, detail: impl std::fmt::Display) -> TraceDecayError {
    print_corruption_warning(db_path);
    TraceDecayError::Database {
        message: format!(
            "database recovery required at '{}'; DB/WAL/SHM and dirty sentinel were preserved: {detail}",
            db_path.display()
        ),
        operation: "open_recovery_required".to_string(),
    }
}

fn print_corruption_warning(db_path: &Path) {
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
