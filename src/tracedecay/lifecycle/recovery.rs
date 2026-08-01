//! Corruption detection and recovery: preflight health checks around a
//! registered open, preserving a corrupt derived branch store, and the
//! actionable errors surfaced when recovery cannot proceed automatically.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::db::DatabaseAuthority;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::storage::StoreLayout;
use crate::tracedecay::ActiveGraphLayout;
use crate::tracedecay::locking::try_acquire_graph_sync_locks;

use super::{TraceDecay, TraceDecayOpenOptions};

impl TraceDecay {
    pub(super) async fn recover_corrupt_branch_or_fail(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
        store_layout: &StoreLayout,
        db_path: &Path,
        detail: impl std::fmt::Display,
        repair_corrupt_branch: bool,
        configuration_database: Arc<RegisteredGlobalDb>,
        profile_database: Arc<RegisteredGlobalDb>,
        runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    ) -> Result<Self> {
        let detail = detail.to_string();
        if repair_corrupt_branch {
            if let Err(close_error) = runtime_registry
                .close_code_graph_paths([db_path.to_path_buf()])
                .await
            {
                print_corruption_warning(db_path);
                return Err(recovery_required_error(
                    db_path,
                    format!(
                        "{detail}; automatic derived-branch repair could not retire the registered runtime before replacing the database: {close_error}"
                    ),
                ));
            }
            let active_graph_layout = active_graph_layout(db_path);
            let repair_result = (|| {
                let _sync_locks = try_acquire_graph_sync_locks(
                    &active_graph_layout.sync_lock_path,
                    &store_layout.sync_lock_path,
                )?;
                let _authority =
                    DatabaseAuthority::for_runtime(db_path, "preserve corrupt branch store")?;
                preserve_corrupt_branch_store(store_layout, db_path)
            })();

            match repair_result {
                Ok(recovery_dir) => {
                    eprintln!(
                        "[tracedecay] corrupt derived branch index preserved at '{}' — rebuilding from a healthy tracked ancestor",
                        recovery_dir.display()
                    );
                    return Box::pin(Self::open_with_registered_configuration_inner(
                        project_root,
                        open_options,
                        store_layout.clone(),
                        configuration_database,
                        profile_database,
                        runtime_registry,
                        false,
                        false,
                    ))
                    .await;
                }
                Err(repair_error) => {
                    print_corruption_warning(db_path);
                    return Err(recovery_required_error(
                        db_path,
                        format!("{detail}; automatic derived-branch repair failed: {repair_error}"),
                    ));
                }
            }
        }

        print_corruption_warning(db_path);
        Err(recovery_required_error(db_path, detail))
    }
}

fn graph_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut file_name = db_path.file_name().unwrap_or_default().to_os_string();
    file_name.push(suffix);
    db_path.with_file_name(file_name)
}

fn preserve_corrupt_branch_store(store_layout: &StoreLayout, db_path: &Path) -> Result<PathBuf> {
    let db_name = db_path.file_name().ok_or_else(|| TraceDecayError::Config {
        message: format!(
            "cannot preserve corrupt branch store with no filename: '{}'",
            db_path.display()
        ),
    })?;
    let recovery_root = store_layout.data_root.join("recovery");
    std::fs::create_dir_all(&recovery_root).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to create branch recovery directory '{}': {error}",
            recovery_root.display()
        ),
    })?;
    let recovery_dir = recovery_root.join(format!(
        "{}-{}-{}",
        db_name.to_string_lossy(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id()
    ));
    std::fs::create_dir(&recovery_dir).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to create branch recovery set '{}': {error}",
            recovery_dir.display()
        ),
    })?;

    let db_wal = graph_sidecar_path(db_path, "-wal");
    let db_shm = graph_sidecar_path(db_path, "-shm");
    let db_dirty = graph_sidecar_path(db_path, ".dirty");
    let sources = [&db_wal, &db_shm, &db_dirty, db_path];
    let mut preserved_db = false;
    let mut preserved = Vec::new();
    for source in sources {
        let metadata = match std::fs::symlink_metadata(source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "failed to inspect recovery-set member '{}': {error}",
                        source.display()
                    ),
                });
            }
        };
        if !metadata.file_type().is_file() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "refusing to preserve non-regular recovery-set member '{}'",
                    source.display()
                ),
            });
        }
        let target = recovery_dir.join(source.file_name().unwrap_or_default());
        let copied = std::fs::copy(source, &target).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to preserve recovery-set member '{}' at '{}': {error}",
                source.display(),
                target.display()
            ),
        })?;
        if copied != metadata.len() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "incomplete recovery-set copy for '{}': copied {copied} of {} bytes",
                    source.display(),
                    metadata.len()
                ),
            });
        }
        // Windows FlushFileBuffers requires a write handle.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&target)
            .and_then(|file| file.sync_all())
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to sync preserved recovery-set member '{}': {error}",
                    target.display()
                ),
            })?;
        preserved_db |= source == db_path;
        preserved.push(source.to_path_buf());
    }
    if !preserved_db {
        return Err(TraceDecayError::Config {
            message: format!(
                "corrupt branch database '{}' disappeared",
                db_path.display()
            ),
        });
    }
    #[cfg(unix)]
    for directory in [&recovery_dir, &recovery_root, &store_layout.data_root] {
        std::fs::File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to sync preserved recovery-set directory '{}': {error}",
                    directory.display()
                ),
            })?;
    }

    // Retire the source database last. A complete copied recovery set remains
    // available if any source-side cleanup fails.
    for source in preserved {
        std::fs::remove_file(&source).map_err(|error| TraceDecayError::Config {
            message: format!(
                "preserved recovery set at '{}', but failed to retire '{}': {error}",
                recovery_dir.display(),
                source.display()
            ),
        })?;
    }
    Ok(recovery_dir)
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
pub(super) fn is_readonly_recovery_block(problem: &str) -> bool {
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
