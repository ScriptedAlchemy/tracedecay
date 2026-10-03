//! Daemon-wide wake for hook spool appends and delivery receipts.
//!
//! Hook callbacks never contact the daemon to be drained: they append to
//! `<data_root>/hook-v2-spool/<host>`, publish delivery receipts under
//! `<data_root>/hook-delivery-spool/<host>`, and exit. One filesystem watch
//! over every registered project's spool directories turns each append or
//! receipt publication into either a wake of that project's running replay
//! consumer or, when the project is not open, a project open whose replay
//! consumer then drains the spools. The same opener runs once at startup for
//! every registered project that already holds spooled records or receipts,
//! so a restart does not strand them until some other client opens the
//! project.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::{Notify, mpsc};
use tracedecay_hooks::{
    HOOK_SPOOL_RECORDS_FILE, hook_delivery_publication_name, hook_delivery_spool_directory,
    hook_v2_spool_directory,
};

/// A project whose spool the daemon should drain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SpooledProject {
    pub(super) project_root: PathBuf,
    pub(super) data_root: PathBuf,
}

#[derive(Default)]
struct WatchTargets {
    /// Keyed by the project's data root, which holds both watched directories.
    spools: BTreeMap<PathBuf, WatchedSpool>,
    opener: Option<mpsc::UnboundedSender<SpooledProject>>,
}

struct WatchedSpool {
    project: SpooledProject,
    consumer: Option<Arc<Notify>>,
}

struct SpoolWatch {
    watcher: notify::RecommendedWatcher,
    targets: Arc<StdMutex<WatchTargets>>,
}

fn spool_watch() -> &'static StdMutex<Option<SpoolWatch>> {
    static WATCH: OnceLock<StdMutex<Option<SpoolWatch>>> = OnceLock::new();
    WATCH.get_or_init(|| StdMutex::new(None))
}

/// Runs `update` on the daemon's spool watch, creating it on first use. A
/// watch the platform refuses (for example an exhausted inotify budget)
/// leaves each consumer's interval sweep as the only drain; that degradation
/// is reported, not hidden.
fn with_watch<T>(update: impl FnOnce(&mut SpoolWatch) -> T) -> Option<T> {
    let mut watch = spool_watch().lock().ok()?;
    if watch.is_none() {
        let targets = Arc::new(StdMutex::new(WatchTargets::default()));
        let callback_targets = Arc::clone(&targets);
        match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event {
                wake_for_append(&callback_targets, &event);
            }
        }) {
            Ok(watcher) => *watch = Some(SpoolWatch { watcher, targets }),
            Err(error) => {
                tracing::warn!(%error, "hook spool watch is unavailable; spooled hook events wait for the replay interval");
                return None;
            }
        }
    }
    watch.as_mut().map(update)
}

/// Registers `project`'s spools with the watch. `consumer`, when given,
/// replaces the project's wake target.
pub(super) fn watch_project(project: &SpooledProject, consumer: Option<Arc<Notify>>) {
    with_watch(|watch| {
        {
            let Ok(mut targets) = watch.targets.lock() else {
                return;
            };
            if let Some(watched) = targets.spools.get_mut(&project.data_root) {
                watched.project = project.clone();
                if consumer.is_some() {
                    watched.consumer = consumer;
                }
                return;
            }
        }
        // Host spools are created by whichever callback writes first, so each
        // shared parent must exist for the recursive watch to see them. The
        // target lock is released first: the watcher's event thread takes it
        // in the callback while `watch` waits on that thread.
        for spool_directory in [
            hook_v2_spool_directory(&project.data_root),
            hook_delivery_spool_directory(&project.data_root),
        ] {
            if let Err(error) = std::fs::create_dir_all(&spool_directory)
                .map_err(notify::Error::io)
                .and_then(|()| {
                    watch
                        .watcher
                        .watch(&spool_directory, RecursiveMode::Recursive)
                })
            {
                tracing::warn!(
                    %error,
                    spool = %spool_directory.display(),
                    "hook spool watch could not observe a project; its spooled events wait for the replay interval"
                );
                return;
            }
        }
        if let Ok(mut targets) = watch.targets.lock() {
            targets.spools.insert(
                project.data_root.clone(),
                WatchedSpool {
                    project: project.clone(),
                    consumer,
                },
            );
        }
    });
}

/// Wakes for an in-place data write to a host's records file (an append)
/// and for a delivery receipt arriving under its published or readied name.
/// The drain's own acknowledgements, cursors, and compaction touch other
/// files, remove receipts, or publish records by rename, so they never wake
/// it again; only its adoption of a readied receipt does, once.
///
/// Windows' `ReadDirectoryChangesW` reports every in-place modification as
/// `ModifyKind::Any`; accepting it alongside `Data` keeps the filename check
/// the only gate, so an append wakes the drain on either platform.
fn wake_for_append(targets: &StdMutex<WatchTargets>, event: &notify::Event) {
    let wakes: fn(&Path) -> bool = match event.kind {
        EventKind::Modify(notify::event::ModifyKind::Data(_) | notify::event::ModifyKind::Any) => {
            |path| path.file_name() == Some(OsStr::new(HOOK_SPOOL_RECORDS_FILE))
        }
        EventKind::Create(_)
        | EventKind::Modify(notify::event::ModifyKind::Name(
            notify::event::RenameMode::To
            | notify::event::RenameMode::Both
            | notify::event::RenameMode::Any,
        )) => |path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(hook_delivery_publication_name)
        },
        _ => return,
    };
    let Ok(targets) = targets.lock() else {
        return;
    };
    for path in &event.paths {
        if !wakes(path) {
            continue;
        }
        let Some(watched) = path
            .ancestors()
            .find_map(|ancestor| targets.spools.get(ancestor))
        else {
            continue;
        };
        match (&watched.consumer, &targets.opener) {
            (Some(consumer), _) => consumer.notify_one(),
            (None, Some(opener)) => {
                let _ = opener.send(watched.project.clone());
            }
            (None, None) => {}
        }
    }
}

/// A running replay consumer takes over its project's wakes.
pub(super) fn attach_consumer(data_root: &Path, project_root: &Path, consumer: Arc<Notify>) {
    watch_project(
        &SpooledProject {
            project_root: project_root.to_path_buf(),
            data_root: data_root.to_path_buf(),
        },
        Some(consumer),
    );
}

/// Once its consumer stops, a project's next append opens it again.
pub(super) fn detach_consumer(data_root: &Path) {
    let Ok(watch) = spool_watch().lock() else {
        return;
    };
    let Some(watch) = watch.as_ref() else {
        return;
    };
    if let Ok(mut targets) = watch.targets.lock()
        && let Some(watched) = targets.spools.get_mut(data_root)
    {
        watched.consumer = None;
    }
}

pub(super) fn install_opener(opener: mpsc::UnboundedSender<SpooledProject>) {
    with_watch(|watch| {
        if let Ok(mut targets) = watch.targets.lock() {
            targets.opener = Some(opener);
        }
    });
}

pub(super) fn consumer_attached(data_root: &Path) -> bool {
    spool_watch().lock().is_ok_and(|watch| {
        watch.as_ref().is_some_and(|watch| {
            watch.targets.lock().is_ok_and(|targets| {
                targets
                    .spools
                    .get(data_root)
                    .is_some_and(|watched| watched.consumer.is_some())
            })
        })
    })
}
