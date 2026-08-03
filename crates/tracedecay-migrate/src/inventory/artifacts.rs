use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::model::{SkippedPath, StoreArtifact, StoreStatus};
use super::sqlite::sqlite_quick_check;

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
    follow_symlinks: bool,
    skipped: &mut Vec<SkippedPath>,
    statuses: &mut Vec<StoreStatus>,
    artifacts: &mut Vec<StoreArtifact>,
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

    for path in db_paths {
        artifacts.push(StoreArtifact {
            kind: "branch_graph_db".to_string(),
            size_bytes: file_size(&path),
            path: path.clone(),
        });
        record_sqlite_sidecar_artifact(&path, "-wal", "branch_graph_db_wal", artifacts);
        record_sqlite_sidecar_artifact(&path, "-shm", "branch_graph_db_shm", artifacts);
        if !sqlite_quick_check(&path).await && !statuses.contains(&StoreStatus::Corrupt) {
            statuses.push(StoreStatus::Corrupt);
        }
    }
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
