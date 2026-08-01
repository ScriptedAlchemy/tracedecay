use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};

use super::project::push_integrity_issue;
use super::sqlite::sqlite_quick_check;
use crate::inventory::{
    InventoryIntegrityMode, InventoryStoreAuthority, SkippedPath, SqliteIntegrityOutcome,
    StoreArtifact, StoreStatus,
};

const MAX_CONCURRENT_BRANCH_CHECKS: usize = 8;

pub(super) fn record_optional_artifact(
    data_dir: &Path,
    kind: &str,
    relpath: &str,
    artifacts: &mut Vec<StoreArtifact>,
) {
    let path = data_dir.join(relpath);
    if path.is_file() {
        let size_bytes = file_size(&path);
        artifacts.push(StoreArtifact {
            kind: kind.to_string(),
            size_bytes,
            path,
        });
    } else if path.is_dir() {
        let size_bytes = dir_size(&path);
        artifacts.push(StoreArtifact {
            kind: kind.to_string(),
            size_bytes,
            path,
        });
    }
}

pub(super) async fn record_branch_db_artifacts(
    data_dir: &Path,
    current_branch_db: Option<&Path>,
    follow_symlinks: bool,
    skipped: &mut Vec<SkippedPath>,
    statuses: &mut Vec<StoreStatus>,
    artifacts: &mut Vec<StoreArtifact>,
    integrity: InventoryIntegrityMode,
) {
    let mut branches_dir = data_dir.join("branches");
    let Ok(meta) = std::fs::symlink_metadata(&branches_dir) else {
        return;
    };
    if meta.file_type().is_symlink() {
        if !follow_symlinks {
            skipped.push(SkippedPath {
                path: branches_dir,
                reason: "symlink".to_string(),
            });
            return;
        }
        if !branches_dir.is_dir() {
            return;
        }
        branches_dir = branches_dir.canonicalize().unwrap_or(branches_dir);
    } else if !meta.is_dir() {
        return;
    }

    let Ok(entries) = std::fs::read_dir(branches_dir) else {
        return;
    };
    let mut db_paths = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            entry
                .file_type()
                .is_ok_and(|file_type| file_type.is_file())
                .then_some(path)
        })
        .filter(|path| path.extension().is_some_and(|extension| extension == "db"))
        .collect::<Vec<_>>();
    db_paths.sort();

    for path in &db_paths {
        artifacts.push(StoreArtifact {
            kind: "branch_graph_db".to_string(),
            size_bytes: file_size(path),
            path: path.clone(),
        });
        record_sqlite_family_sidecars(
            path,
            "branch_graph_db_wal",
            "branch_graph_db_shm",
            artifacts,
        );
    }

    match integrity {
        InventoryIntegrityMode::MetadataOnly if !db_paths.is_empty() => {
            if !statuses.contains(&StoreStatus::IntegrityUnchecked) {
                statuses.push(StoreStatus::IntegrityUnchecked);
            }
        }
        InventoryIntegrityMode::MetadataOnly => {}
        InventoryIntegrityMode::Full => {
            let outcomes =
                check_branch_databases(
                    db_paths,
                    |path| async move { sqlite_quick_check(&path).await },
                )
                .await;
            for (path, outcome) in outcomes {
                let authority =
                    if current_branch_db.is_some_and(|current| same_path(current, &path)) {
                        InventoryStoreAuthority::Authoritative
                    } else {
                        InventoryStoreAuthority::StaleBranch
                    };
                push_integrity_issue(statuses, &path, authority, outcome);
            }
        }
    }
}

async fn check_branch_databases<F, Fut>(
    db_paths: Vec<PathBuf>,
    mut check: F,
) -> Vec<(PathBuf, SqliteIntegrityOutcome)>
where
    F: FnMut(PathBuf) -> Fut,
    Fut: Future<Output = SqliteIntegrityOutcome> + Send + 'static,
{
    let mut pending = db_paths.into_iter();
    let mut checks = tokio::task::JoinSet::new();
    for _ in 0..MAX_CONCURRENT_BRANCH_CHECKS {
        let Some(path) = pending.next() else {
            break;
        };
        let outcome = check(path.clone());
        checks.spawn(async move { (path, outcome.await) });
    }

    let mut outcomes = Vec::new();
    while let Some(result) = checks.join_next().await {
        if let Ok(result) = result {
            outcomes.push(result);
        }
        if let Some(path) = pending.next() {
            let outcome = check(path.clone());
            checks.spawn(async move { (path, outcome.await) });
        }
    }
    outcomes.sort_by(|left, right| left.0.cmp(&right.0));
    outcomes
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    left == right
}

pub(super) fn record_sqlite_family_sidecars(
    db_path: &Path,
    wal_kind: &str,
    shm_kind: &str,
    artifacts: &mut Vec<StoreArtifact>,
) {
    record_sqlite_sidecar_artifact(db_path, "-wal", wal_kind, artifacts);
    record_sqlite_sidecar_artifact(db_path, "-shm", shm_kind, artifacts);
}

fn record_sqlite_sidecar_artifact(
    db_path: &Path,
    suffix: &str,
    kind: &str,
    artifacts: &mut Vec<StoreArtifact>,
) {
    let mut path = db_path.as_os_str().to_os_string();
    path.push(suffix);
    let path = PathBuf::from(path);
    if path.is_file() {
        artifacts.push(StoreArtifact {
            kind: kind.to_string(),
            size_bytes: file_size(&path),
            path,
        });
    }
}

pub(super) fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |meta| meta.len())
}

pub(super) fn dir_size(dir: &Path) -> u64 {
    fn walk(path: &Path, total: &mut u64, visited_dirs: &mut HashSet<PathBuf>) {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return;
        };
        if meta.file_type().is_symlink() {
            *total = total.saturating_add(meta.len());
            return;
        }
        if !meta.is_dir() {
            if meta.is_file() {
                *total = total.saturating_add(meta.len());
            }
            return;
        }
        let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if !visited_dirs.insert(key) {
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            walk(&entry.path(), total, visited_dirs);
        }
    }

    let mut total = 0;
    let mut visited_dirs = HashSet::new();
    walk(dir, &mut total, &mut visited_dirs);
    total
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::Barrier;

    use super::{MAX_CONCURRENT_BRANCH_CHECKS, SqliteIntegrityOutcome, check_branch_databases};

    #[tokio::test]
    async fn branch_database_checks_are_bounded_and_complete() {
        let paths = (0..512)
            .map(|index| PathBuf::from(format!("branch-{index}.db")))
            .collect();
        let active = Arc::new(AtomicUsize::new(0));
        let checked = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(AtomicUsize::new(0));
        let first_batch = Arc::new(Barrier::new(MAX_CONCURRENT_BRANCH_CHECKS));

        let outcomes = check_branch_databases(paths, |_| {
            let active = Arc::clone(&active);
            let checked = Arc::clone(&checked);
            let peak = Arc::clone(&peak);
            let started = Arc::clone(&started);
            let first_batch = Arc::clone(&first_batch);
            async move {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                if started.fetch_add(1, Ordering::SeqCst) < MAX_CONCURRENT_BRANCH_CHECKS {
                    first_batch.wait().await;
                }
                checked.fetch_add(1, Ordering::SeqCst);
                active.fetch_sub(1, Ordering::SeqCst);
                SqliteIntegrityOutcome::Verified
            }
        })
        .await;

        assert_eq!(outcomes.len(), 512);
        assert!(
            outcomes
                .iter()
                .all(|(_, outcome)| outcome == &SqliteIntegrityOutcome::Verified)
        );
        assert_eq!(checked.load(Ordering::SeqCst), 512);
        assert_eq!(peak.load(Ordering::SeqCst), MAX_CONCURRENT_BRANCH_CHECKS);
    }
}
