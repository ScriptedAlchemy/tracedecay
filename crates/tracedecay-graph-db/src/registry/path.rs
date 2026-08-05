use std::path::{Path, PathBuf};

use fs2::FileExt;
use grafeo_storage::file::GrafeoFileManager;
use same_file::Handle;
use tracedecay_domain::framed_log::{DirectorySyncPolicy, sync_directory};
use tracedecay_store::GRAPH_STORE_PRIVATE_DIRECTORY;

use crate::GraphDbError;
use crate::error::rollback_failure;

pub(super) struct GraphPathAnchor {
    parent: Handle,
    file: Handle,
    lock_probe: std::fs::File,
    path: PathBuf,
    remove_on_drop: bool,
}

struct PendingGraphFile {
    path: PathBuf,
    file: Handle,
    lock_probe: std::fs::File,
}

impl GraphPathAnchor {
    pub(super) fn acquire(path: &Path) -> Result<Self, GraphDbError> {
        let parent_path = path
            .parent()
            .ok_or_else(|| GraphDbError::invalid("canonical graph path has no parent"))?;
        validate_managed_graph_path(path)?;
        let parent = tracedecay_private_fs::open_private_directory(parent_path)
            .and_then(Handle::from_file)
            .map_err(|error| {
                GraphDbError::unavailable(format!(
                    "failed to anchor private graph directory: {error}"
                ))
            })?;
        let (mut pending, existing_file) =
            match tracedecay_private_fs::open_private_file(path).and_then(anchor_file) {
                Ok(file) => (None, Some(file)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    (Some(initialize_graph_file(path)?), None)
                }
                Err(error) => {
                    return Err(GraphDbError::unavailable(format!(
                        "failed to anchor canonical graph database {}: {error}",
                        path.display()
                    )));
                }
            };
        if let Err(error) = validate_managed_graph_path(path) {
            return Err(abort_pending(pending, error));
        }
        let current_parent = tracedecay_private_fs::open_private_directory(parent_path)
            .and_then(Handle::from_file)
            .map_err(|error| {
                abort_pending(
                    pending.take(),
                    GraphDbError::unavailable(format!(
                        "failed to re-anchor private graph directory: {error}"
                    )),
                )
            })?;
        if parent != current_parent {
            return Err(abort_pending(pending, GraphDbError::Conflict));
        }
        let (file, lock_probe, created) = match pending {
            Some(pending) => {
                let (file, lock_probe) = pending.finish();
                (file, lock_probe, true)
            }
            None => {
                let (file, lock_probe) = existing_file.ok_or_else(|| {
                    GraphDbError::unavailable("canonical graph file anchor disappeared")
                })?;
                (file, lock_probe, false)
            }
        };
        verify_lock_available(&lock_probe)?;
        Ok(Self {
            parent,
            file,
            lock_probe,
            path: path.to_path_buf(),
            remove_on_drop: created,
        })
    }

    pub(super) fn verify(&self, path: &Path) -> Result<(), GraphDbError> {
        validate_managed_graph_path(path)?;
        let current_parent = tracedecay_private_fs::open_private_directory(
            path.parent()
                .ok_or_else(|| GraphDbError::invalid("canonical graph path has no parent"))?,
        )
        .and_then(Handle::from_file)
        .map_err(|error| {
            GraphDbError::unavailable(format!("failed to verify private graph directory: {error}"))
        })?;
        if self.parent != current_parent {
            return Err(GraphDbError::Conflict);
        }
        let current = tracedecay_private_fs::open_private_file(path)
            .and_then(Handle::from_file)
            .map_err(|error| {
                GraphDbError::unavailable(format!(
                    "failed to verify canonical graph database {}: {error}",
                    path.display()
                ))
            })?;
        if self.file != current {
            return Err(GraphDbError::Conflict);
        }
        Ok(())
    }

    /// Proves Grafeo took its exclusive lock on this exact anchored file.
    ///
    /// Acquisition first proves this handle was unlocked. Under the
    /// owner-private, single-daemon registry invariant, the only permitted
    /// intervening locker is the `GraphDbOwner` opened by this registry.
    pub(super) fn verify_engine_lock(&self) -> Result<(), GraphDbError> {
        match self.lock_probe.try_lock_exclusive() {
            Ok(()) => {
                FileExt::unlock(&self.lock_probe).map_err(|error| {
                    GraphDbError::DurabilityUncertain {
                        message: format!(
                            "failed to release unexpected graph identity probe lock: {error}"
                        ),
                    }
                })?;
                Err(GraphDbError::Conflict)
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
            Err(error) => Err(GraphDbError::unavailable(format!(
                "failed to prove Grafeo owns the anchored graph file: {error}"
            ))),
        }
    }

    pub(super) fn commit(&mut self) {
        self.remove_on_drop = false;
    }

    pub(super) fn abort(mut self, primary: GraphDbError) -> GraphDbError {
        if !self.remove_on_drop {
            return primary;
        }
        self.remove_on_drop = false;
        cleanup_created_file(&self.path, Some(&self.file), primary)
    }
}

impl PendingGraphFile {
    fn new(path: &Path, file: Handle, lock_probe: std::fs::File) -> Self {
        Self {
            path: path.to_path_buf(),
            file,
            lock_probe,
        }
    }

    fn abort(self, primary: GraphDbError) -> GraphDbError {
        cleanup_created_file(&self.path, Some(&self.file), primary)
    }

    fn finish(self) -> (Handle, std::fs::File) {
        (self.file, self.lock_probe)
    }
}

fn initialize_graph_file(path: &Path) -> Result<PendingGraphFile, GraphDbError> {
    let initialized = GrafeoFileManager::create(path).map_err(|error| {
        GraphDbError::unavailable(format!(
            "failed to initialize canonical graph database {}: {error}",
            path.display()
        ))
    })?;
    let (file, lock_probe) =
        match tracedecay_private_fs::make_private_file(path).and_then(anchor_file) {
            Ok(file) => file,
            Err(error) => {
                drop(initialized);
                return Err(cleanup_created_file(
                    path,
                    None,
                    GraphDbError::unavailable(format!(
                        "failed to anchor initialized graph database {}: {error}",
                        path.display()
                    )),
                ));
            }
        };
    let pending = PendingGraphFile::new(path, file, lock_probe);
    drop(initialized);
    let Some(parent) = path.parent() else {
        return Err(pending.abort(GraphDbError::invalid("canonical graph path has no parent")));
    };
    if let Err(error) = sync_directory(parent, DirectorySyncPolicy::Strict) {
        return Err(pending.abort(GraphDbError::unavailable(format!(
            "failed to persist canonical graph database creation: {error}"
        ))));
    }
    Ok(pending)
}

fn anchor_file(file: std::fs::File) -> std::io::Result<(Handle, std::fs::File)> {
    let identity = Handle::from_file(file.try_clone()?)?;
    Ok((identity, file))
}

fn verify_lock_available(file: &std::fs::File) -> Result<(), GraphDbError> {
    file.try_lock_exclusive().map_err(|error| {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            GraphDbError::Conflict
        } else {
            GraphDbError::unavailable(format!(
                "failed to probe canonical graph file lock: {error}"
            ))
        }
    })?;
    FileExt::unlock(file).map_err(|error| GraphDbError::DurabilityUncertain {
        message: format!("failed to release canonical graph file probe lock: {error}"),
    })
}

fn abort_pending(pending: Option<PendingGraphFile>, primary: GraphDbError) -> GraphDbError {
    pending.map_or(primary.clone(), |pending| pending.abort(primary))
}

fn cleanup_created_file(
    path: &Path,
    expected: Option<&Handle>,
    primary: GraphDbError,
) -> GraphDbError {
    let Some(expected) = expected else {
        return rollback_failure(
            "refuse graph initialization cleanup without identity",
            primary,
            "initialized graph file identity was never anchored",
        );
    };
    {
        let current =
            match tracedecay_private_fs::open_private_file(path).and_then(Handle::from_file) {
                Ok(current) => current,
                Err(error) => {
                    return rollback_failure("abort graph initialization", primary, error);
                }
            };
        if &current != expected {
            return rollback_failure(
                "abort graph initialization",
                primary,
                "initialized graph file identity changed before cleanup",
            );
        }
    }
    if let Err(error) = std::fs::remove_file(path) {
        return rollback_failure("abort graph initialization", primary, error);
    }
    let Some(parent) = path.parent() else {
        return rollback_failure(
            "abort graph initialization",
            primary,
            "canonical graph path has no parent",
        );
    };
    if let Err(error) = sync_directory(parent, DirectorySyncPolicy::Strict) {
        return rollback_failure("persist graph initialization cleanup", primary, error);
    }
    primary
}

pub(super) fn validate_managed_graph_path(path: &Path) -> Result<(), GraphDbError> {
    let parent = path
        .parent()
        .ok_or_else(|| GraphDbError::invalid("canonical graph path has no parent"))?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|error| {
        GraphDbError::unavailable(format!(
            "failed to resolve canonical graph directory {}: {error}",
            parent.display()
        ))
    })?;
    if canonical_parent != parent {
        return Err(GraphDbError::invalid(
            "graph database parent must be the exact canonical directory",
        ));
    }
    if parent.file_name().and_then(|name| name.to_str()) != Some(GRAPH_STORE_PRIVATE_DIRECTORY) {
        return Err(GraphDbError::invalid(
            "graph database parent is not the daemon private graph directory",
        ));
    }
    tracedecay_private_fs::validate_private_directory(parent)
        .map_err(|error| map_private_path_error("graph database parent", error))?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            GraphDbError::invalid("canonical graph database must be a real file"),
        ),
        Ok(metadata) => validate_private_graph_file(path, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GraphDbError::unavailable(format!(
            "failed to inspect canonical graph database {}: {error}",
            path.display()
        ))),
    }
}

fn validate_private_graph_file(
    path: &Path,
    _metadata: &std::fs::Metadata,
) -> Result<(), GraphDbError> {
    tracedecay_private_fs::validate_private_file(path)
        .map_err(|error| map_private_path_error("canonical graph database", error))
}

fn map_private_path_error(description: &str, error: std::io::Error) -> GraphDbError {
    match error.kind() {
        std::io::ErrorKind::InvalidInput | std::io::ErrorKind::PermissionDenied => {
            GraphDbError::invalid(format!("{description} is not private to the daemon owner"))
        }
        _ => {
            GraphDbError::unavailable(format!("failed to validate private {description}: {error}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::GraphPathAnchor;
    use crate::GraphDbError;

    #[test]
    fn engine_lock_proves_the_exact_anchored_file() {
        let temp = tempdir().unwrap();
        let private_directory = temp
            .path()
            .join(tracedecay_store::GRAPH_STORE_PRIVATE_DIRECTORY);
        tracedecay_private_fs::create_private_directory(&private_directory).unwrap();
        let path = private_directory.join("graph.grafeo");
        let anchor = GraphPathAnchor::acquire(&path).unwrap();

        let manager = grafeo_storage::file::GrafeoFileManager::open(&path).unwrap();
        assert_eq!(anchor.verify_engine_lock(), Ok(()));
        drop(manager);
    }

    #[cfg(unix)]
    #[test]
    fn engine_lock_rejects_swap_away_and_back() {
        let temp = tempdir().unwrap();
        let private_directory = temp
            .path()
            .join(tracedecay_store::GRAPH_STORE_PRIVATE_DIRECTORY);
        tracedecay_private_fs::create_private_directory(&private_directory).unwrap();
        let path = private_directory.join("graph.grafeo");
        let anchor = GraphPathAnchor::acquire(&path).unwrap();
        let anchored_away = private_directory.join("anchored-away.grafeo");
        std::fs::rename(&path, &anchored_away).unwrap();

        let replacement = private_directory.join("replacement.grafeo");
        drop(grafeo_storage::file::GrafeoFileManager::create(&replacement).unwrap());
        drop(tracedecay_private_fs::make_private_file(&replacement).unwrap());
        std::fs::rename(&replacement, &path).unwrap();
        let replacement_manager = grafeo_storage::file::GrafeoFileManager::open(&path).unwrap();
        let replacement_away = private_directory.join("replacement-away.grafeo");
        std::fs::rename(&path, &replacement_away).unwrap();
        std::fs::rename(&anchored_away, &path).unwrap();

        assert_eq!(anchor.verify(&path), Ok(()));
        assert_eq!(anchor.verify_engine_lock(), Err(GraphDbError::Conflict));
        drop(replacement_manager);
    }
}
