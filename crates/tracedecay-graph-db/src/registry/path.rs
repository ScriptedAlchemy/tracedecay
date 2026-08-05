use std::path::{Path, PathBuf};

use fs2::FileExt;
use same_file::Handle;
use tracedecay_domain::framed_log::{DirectorySyncPolicy, sync_directory};
use tracedecay_store::GRAPH_STORE_PRIVATE_DIRECTORY;

use crate::GraphDbError;
use crate::error::rollback_failure;

pub(super) struct GraphPathAnchor {
    parent: Handle,
    parent_probe: std::fs::File,
    file: Handle,
    lock_probe: std::fs::File,
    path: PathBuf,
    remove_on_drop: bool,
}

pub(super) struct GraphPathVerification {
    _parent: Handle,
    _parent_probe: std::fs::File,
    _file: Handle,
    _file_probe: std::fs::File,
    engine_lock_probe: std::fs::File,
}

pub(super) enum GraphPathPreparation {
    Existing(GraphPathAnchor),
    Missing {
        parent: Handle,
        parent_probe: std::fs::File,
        path: PathBuf,
    },
}

pub(super) struct GraphPathCompletionFailure {
    pub(super) anchor: Option<GraphPathAnchor>,
    pub(super) error: GraphDbError,
}

impl GraphPathPreparation {
    pub(super) fn prepare(path: &Path) -> Result<Self, GraphDbError> {
        let parent_path = path
            .parent()
            .ok_or_else(|| GraphDbError::invalid("canonical graph path has no parent"))?;
        validate_managed_graph_path(path)?;
        let (parent, parent_probe) = anchor_private_directory(parent_path, "anchor")?;
        let file = match tracedecay_private_fs::open_private_file(path).and_then(anchor_file) {
            Ok((file, lock_probe)) => Some((file, lock_probe)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(GraphDbError::unavailable(format!(
                    "failed to anchor canonical graph database {}: {error}",
                    path.display()
                )));
            }
        };
        let (current_parent, _current_parent_probe) =
            anchor_private_directory(parent_path, "re-anchor")?;
        if parent != current_parent {
            return Err(GraphDbError::Conflict);
        }
        match file {
            Some((file, lock_probe)) => {
                let anchor = GraphPathAnchor {
                    parent,
                    parent_probe,
                    file,
                    lock_probe,
                    path: path.to_path_buf(),
                    remove_on_drop: false,
                };
                drop(anchor.verify(path)?);
                verify_lock_available(&anchor.lock_probe)?;
                Ok(Self::Existing(anchor))
            }
            None => {
                require_path_absent(path)?;
                Ok(Self::Missing {
                    parent,
                    parent_probe,
                    path: path.to_path_buf(),
                })
            }
        }
    }

    pub(super) fn complete_after_open(self) -> Result<GraphPathAnchor, GraphPathCompletionFailure> {
        match self {
            Self::Existing(anchor) => {
                let verification = match anchor.verify(&anchor.path) {
                    Ok(verification) => verification,
                    Err(error) => {
                        return Err(GraphPathCompletionFailure {
                            anchor: Some(anchor),
                            error,
                        });
                    }
                };
                if let Err(error) = verification.verify_engine_lock() {
                    return Err(GraphPathCompletionFailure {
                        anchor: Some(anchor),
                        error,
                    });
                }
                Ok(anchor)
            }
            Self::Missing {
                parent,
                parent_probe,
                path,
            } => {
                let (file, lock_probe) =
                    match tracedecay_private_fs::make_private_file(&path).and_then(anchor_file) {
                        Ok(file) => file,
                        Err(error) => {
                            return Err(GraphPathCompletionFailure {
                                anchor: None,
                                error: GraphDbError::unavailable(format!(
                                    "failed to anchor created graph database {}: {error}",
                                    path.display()
                                )),
                            });
                        }
                    };
                let anchor = GraphPathAnchor {
                    parent,
                    parent_probe,
                    file,
                    lock_probe,
                    path,
                    remove_on_drop: true,
                };
                let verification = match anchor.verify(&anchor.path) {
                    Ok(verification) => verification,
                    Err(error) => {
                        return Err(GraphPathCompletionFailure {
                            anchor: Some(anchor),
                            error,
                        });
                    }
                };
                if let Err(error) = verification.verify_engine_lock() {
                    return Err(GraphPathCompletionFailure {
                        anchor: Some(anchor),
                        error,
                    });
                }
                if let Err(error) = sync_graph_parent(&anchor.path) {
                    return Err(GraphPathCompletionFailure {
                        anchor: Some(anchor),
                        error,
                    });
                }
                Ok(anchor)
            }
        }
    }

    pub(super) fn abort_failed_open(self, primary: GraphDbError) -> GraphDbError {
        match self {
            Self::Existing(_) => primary,
            Self::Missing { path, .. } => match std::fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => primary,
                Ok(_) => rollback_failure(
                    "refuse failed graph cleanup without identity",
                    primary,
                    "failed open left a graph path that was never anchored",
                ),
                Err(error) => {
                    rollback_failure("inspect failed graph open for cleanup", primary, error)
                }
            },
        }
    }
}

impl GraphPathAnchor {
    pub(super) fn verify(&self, path: &Path) -> Result<GraphPathVerification, GraphDbError> {
        validate_managed_graph_path(path)?;
        let (current_parent, current_parent_probe) = anchor_private_directory(
            path.parent()
                .ok_or_else(|| GraphDbError::invalid("canonical graph path has no parent"))?,
            "verify",
        )?;
        if self.parent != current_parent {
            return Err(GraphDbError::Conflict);
        }
        let (current, current_probe) = tracedecay_private_fs::open_private_file(path)
            .and_then(anchor_file)
            .map_err(|error| {
                GraphDbError::unavailable(format!(
                    "failed to verify canonical graph database {}: {error}",
                    path.display()
                ))
            })?;
        if self.file != current {
            return Err(GraphDbError::Conflict);
        }
        let engine_lock_probe =
            self.lock_probe
                .try_clone()
                .map_err(|error| GraphDbError::Unavailable {
                    message: format!(
                        "failed to retain exact graph lock probe through publication: {error}"
                    ),
                })?;
        Ok(GraphPathVerification {
            _parent: current_parent,
            _parent_probe: current_parent_probe,
            _file: current,
            _file_probe: current_probe,
            engine_lock_probe,
        })
    }

    /// Proves Grafeo took its exclusive lock on this exact anchored file.
    ///
    /// Preparation first proves an existing handle was unlocked. Under the
    /// owner-private, single-daemon registry invariant, the only permitted
    /// intervening locker is the `GraphDbOwner` opened by this registry.
    pub(super) fn verify_engine_lock(&self) -> Result<(), GraphDbError> {
        verify_engine_lock(&self.lock_probe)
    }

    pub(super) fn commit(&mut self) {
        self.remove_on_drop = false;
    }

    pub(super) fn abort(mut self, primary: GraphDbError) -> GraphDbError {
        if !self.remove_on_drop {
            return primary;
        }
        self.remove_on_drop = false;
        cleanup_created_file(
            &self.path,
            &self.parent,
            &self.parent_probe,
            &self.file,
            &self.lock_probe,
            primary,
        )
    }
}

impl GraphPathVerification {
    pub(super) fn verify_engine_lock(&self) -> Result<(), GraphDbError> {
        verify_engine_lock(&self.engine_lock_probe)
    }
}

fn verify_engine_lock(lock_probe: &std::fs::File) -> Result<(), GraphDbError> {
    match lock_probe.try_lock_exclusive() {
        Ok(()) => {
            FileExt::unlock(lock_probe).map_err(|error| GraphDbError::DurabilityUncertain {
                message: format!("failed to release unexpected graph identity probe lock: {error}"),
            })?;
            Err(GraphDbError::Conflict)
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
        Err(error) => Err(GraphDbError::unavailable(format!(
            "failed to prove Grafeo owns the anchored graph file: {error}"
        ))),
    }
}

fn anchor_private_directory(
    path: &Path,
    operation: &str,
) -> Result<(Handle, std::fs::File), GraphDbError> {
    tracedecay_private_fs::open_private_directory(path)
        .and_then(anchor_file)
        .map_err(|error| {
            GraphDbError::unavailable(format!(
                "failed to {operation} private graph directory: {error}"
            ))
        })
}

fn require_path_absent(path: &Path) -> Result<(), GraphDbError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(GraphDbError::Conflict),
        Err(error) => Err(GraphDbError::unavailable(format!(
            "failed to verify absent graph database {}: {error}",
            path.display()
        ))),
    }
}

fn sync_graph_parent(path: &Path) -> Result<(), GraphDbError> {
    let parent = path
        .parent()
        .ok_or_else(|| GraphDbError::invalid("canonical graph path has no parent"))?;
    sync_directory(parent, DirectorySyncPolicy::Strict).map_err(|error| {
        GraphDbError::unavailable(format!(
            "failed to persist canonical graph database creation: {error}"
        ))
    })
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

fn cleanup_created_file(
    path: &Path,
    expected_parent: &Handle,
    expected_parent_probe: &std::fs::File,
    expected: &Handle,
    expected_probe: &std::fs::File,
    primary: GraphDbError,
) -> GraphDbError {
    let Some(parent) = path.parent() else {
        return rollback_failure(
            "abort graph initialization",
            primary,
            "canonical graph path has no parent",
        );
    };
    let (current_parent, current_parent_probe) =
        match tracedecay_private_fs::open_private_directory(parent).and_then(anchor_file) {
            Ok(current) => current,
            Err(error) => {
                return rollback_failure("abort graph initialization", primary, error);
            }
        };
    if &current_parent != expected_parent {
        return rollback_failure(
            "abort graph initialization",
            primary,
            "private graph directory identity changed before cleanup",
        );
    }
    let (current, current_probe) =
        match tracedecay_private_fs::open_private_file(path).and_then(anchor_file) {
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
    if let Err(error) = std::fs::remove_file(path) {
        return rollback_failure("abort graph initialization", primary, error);
    }
    if let Err(error) = sync_directory(parent, DirectorySyncPolicy::Strict) {
        return rollback_failure("persist graph initialization cleanup", primary, error);
    }
    let _retained_through_cleanup = (
        current_parent_probe,
        expected_parent_probe,
        current_probe,
        expected_probe,
    );
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

    use super::GraphPathPreparation;
    use crate::GraphDbError;

    #[test]
    fn missing_preparation_never_precreates_a_markerless_graph_file() {
        let temp = tempdir().unwrap();
        let private_directory = temp
            .path()
            .join(tracedecay_store::GRAPH_STORE_PRIVATE_DIRECTORY);
        tracedecay_private_fs::create_private_directory(&private_directory).unwrap();
        let path = private_directory.join("graph.grafeo");

        let preparation = GraphPathPreparation::prepare(&path).unwrap();

        assert!(!path.exists());
        assert!(matches!(preparation, GraphPathPreparation::Missing { .. }));
    }

    #[test]
    fn failed_fresh_open_never_deletes_an_unanchored_path() {
        let temp = tempdir().unwrap();
        let private_directory = temp
            .path()
            .join(tracedecay_store::GRAPH_STORE_PRIVATE_DIRECTORY);
        tracedecay_private_fs::create_private_directory(&private_directory).unwrap();
        let path = private_directory.join("graph.grafeo");
        let preparation = GraphPathPreparation::prepare(&path).unwrap();
        drop(tracedecay_private_fs::create_private_file(&path).unwrap());

        let error = preparation.abort_failed_open(GraphDbError::Cancelled);

        let GraphDbError::DurabilityUncertain { message } = error else {
            panic!("unanchored cleanup refusal must report uncertain durability");
        };
        assert!(message.contains("failed open left a graph path that was never anchored"));
        assert!(path.is_file(), "unanchored graph path must not be deleted");
    }

    #[test]
    fn engine_lock_proves_the_exact_anchored_file() {
        let temp = tempdir().unwrap();
        let private_directory = temp
            .path()
            .join(tracedecay_store::GRAPH_STORE_PRIVATE_DIRECTORY);
        tracedecay_private_fs::create_private_directory(&private_directory).unwrap();
        let path = private_directory.join("graph.grafeo");
        let preparation = GraphPathPreparation::prepare(&path).unwrap();
        let manager = grafeo_storage::file::GrafeoFileManager::create(&path).unwrap();
        let anchor = match preparation.complete_after_open() {
            Ok(anchor) => anchor,
            Err(failure) => panic!("created graph anchor failed: {}", failure.error),
        };
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
        drop(grafeo_storage::file::GrafeoFileManager::create(&path).unwrap());
        drop(tracedecay_private_fs::make_private_file(&path).unwrap());
        let preparation = GraphPathPreparation::prepare(&path).unwrap();
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

        let failure = match preparation.complete_after_open() {
            Ok(_) => panic!("replacement manager must not satisfy the retained file identity"),
            Err(failure) => failure,
        };
        assert_eq!(failure.error, GraphDbError::Conflict);
        drop(replacement_manager);
    }
}
