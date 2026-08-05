use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use notify::{RecursiveMode, Watcher};
use walkdir::WalkDir;

use super::GIT_OBSERVATION_BUDGET;
use super::state::{WatchCancellation, WatchState};

pub(super) const MAX_METADATA_WATCH_DIRECTORIES: usize = 512;
const MAX_METADATA_DIRECTORY_ENTRIES: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WatchPlanFailure {
    Cancelled,
    Capacity,
    Unavailable,
}

#[derive(Debug)]
pub(super) enum WatchInstallFailure {
    Plan(WatchPlanFailure),
    Notify(notify::Error),
}

impl fmt::Display for WatchInstallFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plan(failure) => write!(formatter, "metadata watch plan failed: {failure:?}"),
            Self::Notify(error) => write!(formatter, "metadata watch install failed: {error}"),
        }
    }
}

fn insert_directory(
    directories: &mut BTreeSet<PathBuf>,
    path: PathBuf,
) -> Result<(), WatchPlanFailure> {
    directories.insert(path);
    if directories.len() > MAX_METADATA_WATCH_DIRECTORIES {
        Err(WatchPlanFailure::Capacity)
    } else {
        Ok(())
    }
}

fn enumerate_tree_directories(
    root: &Path,
    directories: &mut BTreeSet<PathBuf>,
    cancellation: &WatchCancellation,
) -> Result<(), WatchPlanFailure> {
    if !root.is_dir() {
        return Ok(());
    }
    for (entry_count, entry) in WalkDir::new(root)
        .follow_links(false)
        .same_file_system(true)
        .into_iter()
        .enumerate()
    {
        if cancellation.is_cancelled() {
            return Err(WatchPlanFailure::Cancelled);
        }
        if entry_count >= MAX_METADATA_DIRECTORY_ENTRIES {
            return Err(WatchPlanFailure::Capacity);
        }
        let entry = entry.map_err(|_| WatchPlanFailure::Unavailable)?;
        if entry.file_type().is_dir() {
            insert_directory(directories, entry.into_path())?;
        }
    }
    Ok(())
}

fn build_watch_plan(
    state: &WatchState,
    cancellation: &WatchCancellation,
) -> Result<Vec<PathBuf>, WatchPlanFailure> {
    let mut directories = BTreeSet::new();
    insert_directory(&mut directories, state.common_dir.clone())?;
    enumerate_tree_directories(
        &state.common_dir.join("refs"),
        &mut directories,
        cancellation,
    )?;
    enumerate_tree_directories(
        &state.common_dir.join("worktrees"),
        &mut directories,
        cancellation,
    )?;
    for git_dir in state.git_dirs() {
        if cancellation.is_cancelled() {
            return Err(WatchPlanFailure::Cancelled);
        }
        insert_directory(&mut directories, git_dir)?;
    }
    Ok(directories.into_iter().collect())
}

pub(super) async fn observe_watch_plan(
    state: Arc<WatchState>,
    cancellation: WatchCancellation,
) -> Result<Vec<PathBuf>, WatchPlanFailure> {
    let worker_cancellation = cancellation.clone();
    let mut handle =
        tokio::task::spawn_blocking(move || build_watch_plan(&state, &worker_cancellation));
    tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            handle.abort();
            Err(WatchPlanFailure::Cancelled)
        }
        result = tokio::time::timeout(GIT_OBSERVATION_BUDGET, &mut handle) => match result {
            Ok(Ok(plan)) => plan,
            Ok(Err(_)) if cancellation.is_cancelled() => Err(WatchPlanFailure::Cancelled),
            Ok(Err(_)) | Err(_) => Err(WatchPlanFailure::Unavailable),
        }
    }
}

pub(super) async fn install_watches(
    watcher: &mut notify::RecommendedWatcher,
    state: Arc<WatchState>,
    cancellation: WatchCancellation,
) -> Result<(), WatchInstallFailure> {
    let plan = observe_watch_plan(state, cancellation)
        .await
        .map_err(WatchInstallFailure::Plan)?;
    for directory in plan {
        watcher
            .watch(&directory, RecursiveMode::NonRecursive)
            .map_err(WatchInstallFailure::Notify)?;
    }
    Ok(())
}
