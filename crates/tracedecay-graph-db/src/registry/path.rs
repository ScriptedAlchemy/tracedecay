use std::path::{Path, PathBuf};

use grafeo_storage::file::GrafeoFileManager;
use same_file::Handle;
use tracedecay_store::GRAPH_STORE_PRIVATE_DIRECTORY;

use crate::GraphDbError;

pub(super) struct GraphPathAnchor {
    parent: Handle,
    file: Handle,
    path: PathBuf,
    remove_on_drop: bool,
}

impl GraphPathAnchor {
    pub(super) fn acquire(path: &Path) -> Result<Self, GraphDbError> {
        validate_managed_graph_path(path)?;
        let parent = Handle::from_path(
            path.parent()
                .ok_or_else(|| GraphDbError::invalid("canonical graph path has no parent"))?,
        )
        .map_err(|error| {
            GraphDbError::unavailable(format!("failed to anchor private graph directory: {error}"))
        })?;
        let created = match Handle::from_path(path) {
            Ok(_) => false,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                drop(GrafeoFileManager::create(path).map_err(|error| {
                    GraphDbError::unavailable(format!(
                        "failed to initialize canonical graph database {}: {error}",
                        path.display()
                    ))
                })?);
                set_private_file_permissions(path)?;
                true
            }
            Err(error) => {
                return Err(GraphDbError::unavailable(format!(
                    "failed to anchor canonical graph database {}: {error}",
                    path.display()
                )));
            }
        };
        validate_managed_graph_path(path)?;
        let current_parent = Handle::from_path(
            path.parent()
                .ok_or_else(|| GraphDbError::invalid("canonical graph path has no parent"))?,
        )
        .map_err(|error| {
            GraphDbError::unavailable(format!(
                "failed to re-anchor private graph directory: {error}"
            ))
        })?;
        if parent != current_parent {
            return Err(GraphDbError::Conflict);
        }
        let file = Handle::from_path(path).map_err(|error| {
            GraphDbError::unavailable(format!(
                "failed to anchor canonical graph database {}: {error}",
                path.display()
            ))
        })?;
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
}

impl Drop for GraphPathAnchor {
    fn drop(&mut self) {
        if !self.remove_on_drop {
            return;
        }
        let Ok(current) = Handle::from_path(&self.path) else {
            return;
        };
        if current == self.file {
            let _ = std::fs::remove_file(&self.path);
        }
    }
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
    #[cfg(windows)]
    tracedecay_runtime_core::windows_security::validate_private_directory(parent).map_err(
        |_| GraphDbError::invalid("graph database parent is not private to the daemon owner"),
    )?;
    #[cfg(not(any(unix, windows)))]
    return Err(GraphDbError::invalid(
        "private graph storage is unsupported on this platform",
    ));
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            GraphDbError::invalid("canonical graph database must be a real file"),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GraphDbError::unavailable(format!(
            "failed to inspect canonical graph database {}: {error}",
            path.display()
        ))),
    }
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
