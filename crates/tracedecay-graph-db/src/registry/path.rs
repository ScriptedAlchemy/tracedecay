use std::path::{Path, PathBuf};

use grafeo_storage::file::GrafeoFileManager;
use same_file::Handle;
use tracedecay_domain::framed_log::{DirectorySyncPolicy, sync_directory};
use tracedecay_store::GRAPH_STORE_PRIVATE_DIRECTORY;

use crate::GraphDbError;
use crate::error::rollback_failure;

pub(super) struct GraphPathAnchor {
    parent: Handle,
    file: Handle,
    path: PathBuf,
    remove_on_drop: bool,
}

struct PendingGraphFile {
    path: PathBuf,
    file: Handle,
}

impl GraphPathAnchor {
    pub(super) fn acquire(path: &Path) -> Result<Self, GraphDbError> {
        let parent_path = path
            .parent()
            .ok_or_else(|| GraphDbError::invalid("canonical graph path has no parent"))?;
        validate_managed_graph_path(path)?;
        let parent = Handle::from_path(parent_path).map_err(|error| {
            GraphDbError::unavailable(format!("failed to anchor private graph directory: {error}"))
        })?;
        let (mut pending, existing_file) = match Handle::from_path(path) {
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
        let current_parent = Handle::from_path(parent_path).map_err(|error| {
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
        let (file, created) = match pending {
            Some(pending) => (pending.finish(), true),
            None => (
                existing_file.ok_or_else(|| {
                    GraphDbError::unavailable("canonical graph file anchor disappeared")
                })?,
                false,
            ),
        };
        Ok(Self {
            parent,
            file,
            path: path.to_path_buf(),
            remove_on_drop: created,
        })
    }

    pub(super) fn verify(&self, path: &Path) -> Result<(), GraphDbError> {
        validate_managed_graph_path(path)?;
        let current_parent = Handle::from_path(
            path.parent()
                .ok_or_else(|| GraphDbError::invalid("canonical graph path has no parent"))?,
        )
        .map_err(|error| {
            GraphDbError::unavailable(format!("failed to verify private graph directory: {error}"))
        })?;
        if self.parent != current_parent {
            return Err(GraphDbError::Conflict);
        }
        let current = Handle::from_path(path).map_err(|error| {
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
    fn new(path: &Path, file: Handle) -> Self {
        Self {
            path: path.to_path_buf(),
            file,
        }
    }

    fn abort(self, primary: GraphDbError) -> GraphDbError {
        cleanup_created_file(&self.path, Some(&self.file), primary)
    }

    fn finish(self) -> Handle {
        self.file
    }
}

fn initialize_graph_file(path: &Path) -> Result<PendingGraphFile, GraphDbError> {
    let initialized = GrafeoFileManager::create(path).map_err(|error| {
        GraphDbError::unavailable(format!(
            "failed to initialize canonical graph database {}: {error}",
            path.display()
        ))
    })?;
    let file = match Handle::from_path(path) {
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
    let pending = PendingGraphFile::new(path, file);
    if let Err(error) = set_private_file_permissions(path) {
        drop(initialized);
        return Err(pending.abort(error));
    }
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

fn abort_pending(pending: Option<PendingGraphFile>, primary: GraphDbError) -> GraphDbError {
    pending.map_or(primary.clone(), |pending| pending.abort(primary))
}

fn cleanup_created_file(
    path: &Path,
    expected: Option<&Handle>,
    primary: GraphDbError,
) -> GraphDbError {
    if let Some(expected) = expected {
        let current = match Handle::from_path(path) {
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
    #[cfg(unix)]
    let parent_metadata = std::fs::symlink_metadata(parent).map_err(|error| {
        GraphDbError::unavailable(format!(
            "failed to inspect private graph directory {}: {error}",
            parent.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if parent_metadata.permissions().mode() & 0o077 != 0
            || parent_metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(GraphDbError::invalid(
                "graph database parent is not private to the daemon owner",
            ));
        }
    }
    #[cfg(not(any(unix, windows)))]
    return Err(GraphDbError::invalid(
        "private graph storage is unsupported on this platform",
    ));
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

#[cfg(unix)]
fn validate_private_graph_file(
    _path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), GraphDbError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if metadata.permissions().mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(GraphDbError::invalid(
            "canonical graph database is not private to the daemon owner",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_graph_file(
    _path: &Path,
    _metadata: &std::fs::Metadata,
) -> Result<(), GraphDbError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), GraphDbError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|error| {
        GraphDbError::unavailable(format!(
            "failed to make canonical graph database private {}: {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), GraphDbError> {
    Ok(())
}
