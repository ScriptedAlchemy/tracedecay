use std::path::{Path, PathBuf};

use crate::GraphDbError;

const GRAPH_DIRECTORY: &str = "graph";
const GRAPH_FILENAME: &str = "graph.grafeo";

pub(super) fn validate_managed_graph_path(
    graph_root: &Path,
    path: &Path,
) -> Result<(), GraphDbError> {
    let root_metadata = std::fs::symlink_metadata(graph_root).map_err(|error| {
        GraphDbError::unavailable(format!(
            "failed to inspect canonical graph directory {}: {error}",
            graph_root.display()
        ))
    })?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(GraphDbError::invalid(
            "canonical graph directory must be a real directory",
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

pub(super) fn canonical_graph_path(store_root: &Path) -> Result<PathBuf, GraphDbError> {
    let canonical_root = std::fs::canonicalize(store_root).map_err(|error| {
        GraphDbError::unavailable(format!(
            "failed to resolve canonical graph store root {}: {error}",
            store_root.display()
        ))
    })?;
    if !canonical_root.is_dir() {
        return Err(GraphDbError::invalid(
            "canonical graph store root is not a directory",
        ));
    }
    Ok(canonical_root.join(GRAPH_DIRECTORY).join(GRAPH_FILENAME))
}
