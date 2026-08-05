use std::path::Path;

use same_file::Handle;

use crate::GraphDbError;

pub(super) enum GraphPathAnchor {
    Existing(Handle),
    Prospective,
}

impl GraphPathAnchor {
    pub(super) fn acquire(path: &Path) -> Result<Self, GraphDbError> {
        validate_managed_graph_path(path)?;
        match Handle::from_path(path) {
            Ok(handle) => {
                validate_managed_graph_path(path)?;
                Ok(Self::Existing(handle))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::Prospective),
            Err(error) => Err(GraphDbError::unavailable(format!(
                "failed to anchor canonical graph database {}: {error}",
                path.display()
            ))),
        }
    }

    pub(super) fn verify(&self, path: &Path) -> Result<(), GraphDbError> {
        validate_managed_graph_path(path)?;
        let current = Handle::from_path(path).map_err(|error| {
            GraphDbError::unavailable(format!(
                "failed to verify canonical graph database {}: {error}",
                path.display()
            ))
        })?;
        if let Self::Existing(anchor) = self
            && anchor != &current
        {
            return Err(GraphDbError::Conflict);
        }
        Ok(())
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
