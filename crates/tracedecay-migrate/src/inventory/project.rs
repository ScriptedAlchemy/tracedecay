use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::artifacts::{
    dir_size, file_size, record_branch_db_artifacts, record_optional_artifact,
    record_sqlite_family_sidecars,
};
use super::sqlite::sqlite_quick_check;
use crate::inventory::{
    InventoryIntegrityMode, InventoryStoreAuthority, RegistryStatus, SkippedPath,
    SqliteIntegrityOutcome, StoreArtifact, StoreBrand, StoreInventory, StoreRole, StoreStatus,
};
use crate::root_seam::config::{self, TRACEDECAY_DIR, db_filename};
use crate::root_seam::errors::Result;
use crate::root_seam::storage::{
    BRANCH_META_FILENAME, SESSIONS_DB_FILENAME, STORE_MANIFEST_FILENAME,
};

#[derive(Clone, Copy)]
pub(super) struct InventoryScanOptions {
    pub follow_symlinks: bool,
    pub integrity: InventoryIntegrityMode,
}

pub(super) async fn scan_root(
    root: &Path,
    options: InventoryScanOptions,
    seen_data_dirs: &mut HashSet<PathBuf>,
    stores: &mut Vec<StoreInventory>,
    skipped: &mut Vec<SkippedPath>,
) -> Result<()> {
    let mut visited = HashSet::new();
    let mut work = vec![root.to_path_buf()];

    while let Some(dir) = work.pop() {
        let visit_key = if options.follow_symlinks {
            dir.canonicalize().unwrap_or_else(|_| dir.clone())
        } else {
            dir.clone()
        };
        if !visited.insert(visit_key) {
            continue;
        }

        inspect_data_dir_candidate(
            &dir,
            TRACEDECAY_DIR,
            options,
            seen_data_dirs,
            stores,
            skipped,
            StoreRole::CodeProjectStore,
        )
        .await?;

        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_symlink() && !options.follow_symlinks {
                skipped.push(SkippedPath {
                    path,
                    reason: "symlink".to_string(),
                });
                continue;
            }
            if file_type.is_symlink() {
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if !meta.is_dir() {
                    continue;
                }
            } else if !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == TRACEDECAY_DIR {
                continue;
            }
            if should_prune_dir(&name) {
                continue;
            }
            work.push(path);
        }
    }

    Ok(())
}

pub(super) async fn inspect_data_dir_candidate(
    project_root: &Path,
    dir_name: &str,
    options: InventoryScanOptions,
    seen_data_dirs: &mut HashSet<PathBuf>,
    stores: &mut Vec<StoreInventory>,
    skipped: &mut Vec<SkippedPath>,
    role: StoreRole,
) -> Result<()> {
    let mut data_dir = project_root.join(dir_name);
    let Ok(meta) = std::fs::symlink_metadata(&data_dir) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() {
        if !options.follow_symlinks {
            skipped.push(SkippedPath {
                path: data_dir,
                reason: "symlink".to_string(),
            });
            return Ok(());
        }
        if !data_dir.is_dir() {
            return Ok(());
        }
        data_dir = data_dir.canonicalize().unwrap_or(data_dir);
    } else if !meta.is_dir() {
        return Ok(());
    }

    let db_path = data_dir.join(db_filename(&data_dir));
    if role == StoreRole::CodeProjectStore
        && dir_name == TRACEDECAY_DIR
        && !db_path.is_file()
        && crate::root_seam::storage::read_enrollment_marker(project_root).is_ok_and(|marker| {
            marker.is_some_and(|marker| {
                marker.storage_mode == crate::root_seam::storage::StorageMode::ProfileSharded
            })
        })
    {
        return Ok(());
    }

    inspect_data_dir(
        project_root,
        &data_dir,
        options,
        seen_data_dirs,
        stores,
        skipped,
        role,
    )
    .await
}

pub(super) async fn inspect_data_dir(
    project_root: &Path,
    data_dir: &Path,
    options: InventoryScanOptions,
    seen_data_dirs: &mut HashSet<PathBuf>,
    stores: &mut Vec<StoreInventory>,
    skipped: &mut Vec<SkippedPath>,
    role: StoreRole,
) -> Result<()> {
    let key = data_dir
        .canonicalize()
        .unwrap_or_else(|_| data_dir.to_path_buf());
    if !seen_data_dirs.insert(key) {
        return Ok(());
    }
    let brand = StoreBrand::TraceDecay;
    let db_path = data_dir.join(db_filename(data_dir));
    let store = inspect_project_store(
        project_root,
        data_dir,
        db_path,
        brand,
        role,
        options,
        skipped,
    )
    .await?;
    stores.push(store);
    Ok(())
}

async fn inspect_project_store(
    project_root: &Path,
    data_dir: &Path,
    db_path: PathBuf,
    brand: StoreBrand,
    role: StoreRole,
    options: InventoryScanOptions,
    skipped: &mut Vec<SkippedPath>,
) -> Result<StoreInventory> {
    let mut statuses = Vec::new();
    let mut artifacts = Vec::new();

    if db_path.is_file() {
        artifacts.push(StoreArtifact {
            kind: "graph_db".to_string(),
            size_bytes: file_size(&db_path),
            path: db_path.clone(),
        });
        record_sqlite_family_sidecars(&db_path, "graph_db_wal", "graph_db_shm", &mut artifacts);
        match options.integrity {
            InventoryIntegrityMode::MetadataOnly => {
                statuses.push(StoreStatus::IntegrityUnchecked);
            }
            InventoryIntegrityMode::Full => {
                push_integrity_issue(
                    &mut statuses,
                    &db_path,
                    InventoryStoreAuthority::Authoritative,
                    sqlite_quick_check(&db_path).await,
                );
            }
        }
    } else {
        statuses.push(StoreStatus::MissingDb);
    }

    let sessions_db_path = data_dir.join(SESSIONS_DB_FILENAME);
    record_optional_artifact(
        data_dir,
        "sessions_db",
        SESSIONS_DB_FILENAME,
        &mut artifacts,
    );
    record_sqlite_family_sidecars(
        &sessions_db_path,
        "sessions_db_wal",
        "sessions_db_shm",
        &mut artifacts,
    );
    if sessions_db_path.is_file() {
        match options.integrity {
            InventoryIntegrityMode::MetadataOnly => {
                if !statuses.contains(&StoreStatus::IntegrityUnchecked) {
                    statuses.push(StoreStatus::IntegrityUnchecked);
                }
            }
            InventoryIntegrityMode::Full => {
                push_integrity_issue(
                    &mut statuses,
                    &sessions_db_path,
                    InventoryStoreAuthority::Authoritative,
                    sqlite_quick_check(&sessions_db_path).await,
                );
            }
        }
    }
    record_optional_artifact(
        data_dir,
        "branch_meta",
        BRANCH_META_FILENAME,
        &mut artifacts,
    );
    let current_branch_db = current_branch_database(project_root, data_dir);
    record_branch_db_artifacts(
        data_dir,
        current_branch_db.as_deref(),
        options.follow_symlinks,
        skipped,
        &mut statuses,
        &mut artifacts,
        options.integrity,
    )
    .await;
    record_optional_artifact(data_dir, "config", "config.json", &mut artifacts);
    record_optional_artifact(
        data_dir,
        "store_manifest",
        STORE_MANIFEST_FILENAME,
        &mut artifacts,
    );
    record_optional_artifact(
        data_dir,
        "response_handles",
        "response-handles",
        &mut artifacts,
    );
    record_optional_artifact(data_dir, "lcm_payloads", "lcm-payloads", &mut artifacts);
    record_optional_artifact(data_dir, "dashboard", "dashboard", &mut artifacts);

    let dirty = data_dir.join("dirty");
    if dirty.exists() {
        statuses.push(StoreStatus::Dirty);
        artifacts.push(StoreArtifact {
            kind: "dirty_sentinel".to_string(),
            size_bytes: file_size(&dirty),
            path: dirty,
        });
    }

    let sync_lock = data_dir.join("sync.lock");
    if sync_lock.is_file() {
        match crate::root_seam::storage::try_acquire_sidecar_lock(&sync_lock) {
            Ok(Some(lock)) => drop(lock),
            Ok(None) => statuses.push(StoreStatus::Locked),
            Err(error) if crate::root_seam::db::is_lock_contended(&error) => {
                statuses.push(StoreStatus::Locked);
            }
            Err(_) => statuses.push(StoreStatus::NeedsManualReview),
        }
        artifacts.push(StoreArtifact {
            kind: "sync_lock".to_string(),
            size_bytes: file_size(&sync_lock),
            path: sync_lock,
        });
    }

    let config_tmp = data_dir.join("config.json.tmp");
    if config_tmp.exists() {
        statuses.push(StoreStatus::NeedsManualReview);
        artifacts.push(StoreArtifact {
            kind: "config_tmp".to_string(),
            size_bytes: file_size(&config_tmp),
            path: config_tmp,
        });
    }

    // A TraceDecay store historically nested under a Hermes profile cannot
    // be treated as a code-project store. Its target must first be proven by
    // the dedicated legacy migration; the generic manifest copier must not
    // route it by the profile directory.
    if role == StoreRole::HermesProfileStore {
        statuses.push(StoreStatus::NeedsManualReview);
    } else if statuses.is_empty() {
        statuses.push(StoreStatus::Ok);
    }

    Ok(StoreInventory {
        project_root: project_root.to_path_buf(),
        data_dir: data_dir.to_path_buf(),
        db_path,
        brand,
        role,
        registry_status: RegistryStatus::Unregistered,
        size_bytes: dir_size(data_dir),
        statuses,
        artifacts,
    })
}

pub(super) fn push_integrity_issue(
    statuses: &mut Vec<StoreStatus>,
    path: &Path,
    authority: InventoryStoreAuthority,
    outcome: SqliteIntegrityOutcome,
) {
    if matches!(
        outcome,
        SqliteIntegrityOutcome::NotChecked | SqliteIntegrityOutcome::Verified
    ) {
        return;
    }
    statuses.push(StoreStatus::IntegrityIssue {
        path: path.to_path_buf(),
        authority,
        outcome,
    });
}

fn current_branch_database(project_root: &Path, data_dir: &Path) -> Option<PathBuf> {
    let branch = crate::root_seam::branch::current_branch(project_root)?;
    let metadata = crate::root_seam::branch_meta::load_branch_meta(data_dir)?;
    crate::root_seam::branch::resolve_branch_db_path(data_dir, &branch, &metadata)
}

pub(super) fn missing_registered_store(project_root: &Path) -> StoreInventory {
    let data_dir = project_root.join(TRACEDECAY_DIR);
    missing_registered_store_at(project_root, data_dir)
}

pub(super) fn missing_registered_store_at(
    project_root: &Path,
    data_dir: PathBuf,
) -> StoreInventory {
    StoreInventory {
        project_root: project_root.to_path_buf(),
        db_path: data_dir.join(config::DB_FILENAME),
        data_dir,
        brand: StoreBrand::TraceDecay,
        role: StoreRole::CodeProjectStore,
        registry_status: RegistryStatus::Registered,
        size_bytes: 0,
        statuses: vec![StoreStatus::MissingDb],
        artifacts: Vec::new(),
    }
}

pub(super) fn mark_registry_status(
    stores: &mut [StoreInventory],
    registered_project_keys: &HashSet<PathBuf>,
) {
    for store in stores {
        let key = canonicalize_lossy(&store.project_root);
        store.registry_status = if registered_project_keys.contains(&key) {
            RegistryStatus::Registered
        } else {
            RegistryStatus::Unregistered
        };
    }
}

pub(super) fn canonical_path_set(paths: &[PathBuf]) -> HashSet<PathBuf> {
    paths.iter().map(|path| canonicalize_lossy(path)).collect()
}

pub(super) fn canonicalize_lossy(path: &Path) -> PathBuf {
    crate::root_seam::lifecycle_lease::canonical_or_original(path)
}

/// Authoritative prune during migration inventory scans (unlike the
/// scan.rs hint, nothing here is config-overridable — these directories are
/// always skipped while hunting for legacy data stores). Delegates the
/// generated/vendored segment check to the shared
/// [`crate::root_seam::config::is_generated_dir_segment`] list, plus one site-local
/// addition: `.git`, which migration scans always want to skip but which
/// isn't part of the shared "generated/vendored" concept (the other three
/// call sites — scan hint, config default excludes, redundancy scanner —
/// don't all treat `.git` the same way).
pub(super) fn should_prune_dir(name: &str) -> bool {
    name == ".git" || config::is_generated_dir_segment(name)
}
