use std::path::Path;

use tracedecay_store::DURABLE_GRAPH_STORE_DIRECTORY;

use crate::GraphDbError;
use crate::location::PersistentGraphStoreState;

/// Validates the canonical graph-store directory and creates it when absent.
///
/// Grafeo owns every file below the returned directory; the registry only
/// preserves the exact directory selected by the canonical store authority.
/// Returns the exact directory creation outcome for format initialization.
pub(super) fn prepare_graph_store_directory(
    path: &Path,
) -> Result<PersistentGraphStoreState, GraphDbError> {
    validate_durable_graph_store_directory(path)?;
    match std::fs::symlink_metadata(path) {
        // Only the exact successful `create_dir` below is fresh. Any
        // preexisting directory, including an empty interrupted-open residue,
        // must retain its reset-required state instead of receiving a marker.
        Ok(_) => Ok(PersistentGraphStoreState::Existing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(path).map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    GraphDbError::Conflict
                } else {
                    GraphDbError::unavailable(format!(
                        "failed to create durable graph-store directory {}: {error}",
                        path.display()
                    ))
                }
            })?;
            Ok(PersistentGraphStoreState::Created)
        }
        Err(error) => Err(GraphDbError::unavailable(format!(
            "failed to inspect canonical graph-store directory {}: {error}",
            path.display()
        ))),
    }
}

pub(super) fn validate_durable_graph_store_directory(path: &Path) -> Result<(), GraphDbError> {
    let parent = path
        .parent()
        .ok_or_else(|| GraphDbError::invalid("canonical graph-store directory has no parent"))?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|error| {
        GraphDbError::unavailable(format!(
            "failed to resolve durable graph-store parent {}: {error}",
            parent.display()
        ))
    })?;
    if canonical_parent != parent {
        return Err(GraphDbError::invalid(
            "graph-store parent must be the exact canonical durable directory",
        ));
    }
    if parent.file_name().and_then(|name| name.to_str()) != Some(DURABLE_GRAPH_STORE_DIRECTORY) {
        return Err(GraphDbError::invalid(
            "graph-store parent is not the daemon durable graph-store directory",
        ));
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(
            GraphDbError::invalid("canonical graph store must be a real directory"),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GraphDbError::unavailable(format!(
            "failed to inspect canonical graph store {}: {error}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{prepare_graph_store_directory, validate_durable_graph_store_directory};
    use crate::GraphDbError;
    use crate::location::PersistentGraphStoreState;

    fn graph_path(temp: &tempfile::TempDir) -> std::path::PathBuf {
        let durable_root = temp
            .path()
            .join(tracedecay_store::DURABLE_GRAPH_STORE_DIRECTORY);
        std::fs::create_dir(&durable_root).unwrap();
        durable_root.join("graph")
    }

    #[test]
    fn preparation_creates_a_durable_graph_store_directory_once() {
        let temp = tempdir().unwrap();
        let path = graph_path(&temp);

        assert_eq!(
            prepare_graph_store_directory(&path).unwrap(),
            PersistentGraphStoreState::Created
        );
        assert!(path.is_dir());
        // A previous creator owns the empty directory's history. It cannot be
        // silently reclassified as a fresh format initialization.
        assert_eq!(
            prepare_graph_store_directory(&path).unwrap(),
            PersistentGraphStoreState::Existing
        );
    }

    #[test]
    fn symlinked_graph_directory_is_rejected() {
        let temp = tempdir().unwrap();
        let path = graph_path(&temp);
        let target = temp.path().join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();

        assert!(matches!(
            validate_durable_graph_store_directory(&path).unwrap_err(),
            GraphDbError::InvalidRequest { .. }
        ));
    }

    #[test]
    fn foreign_parent_directory_is_rejected() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("graph");

        assert!(matches!(
            validate_durable_graph_store_directory(&path).unwrap_err(),
            GraphDbError::InvalidRequest { .. }
        ));
    }
}
