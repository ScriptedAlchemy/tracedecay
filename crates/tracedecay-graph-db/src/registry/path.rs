use std::path::Path;

use crate::GraphDbError;
use crate::location::PersistentGraphStoreState;

/// Validates the canonical graph database file and reports whether Grafeo must
/// create it. The registry never creates or opens the file itself.
pub(super) fn inspect_graph_database_file(
    path: &Path,
) -> Result<PersistentGraphStoreState, GraphDbError> {
    validate_graph_database_file(path)?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(PersistentGraphStoreState::Existing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(PersistentGraphStoreState::Prospective)
        }
        Err(error) => Err(GraphDbError::unavailable(format!(
            "failed to inspect canonical graph database file {}: {error}",
            path.display()
        ))),
    }
}

pub(super) fn validate_graph_database_file(path: &Path) -> Result<(), GraphDbError> {
    let parent = path
        .parent()
        .ok_or_else(|| GraphDbError::invalid("canonical graph database file has no parent"))?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|error| {
        GraphDbError::unavailable(format!(
            "failed to resolve graph database parent {}: {error}",
            parent.display()
        ))
    })?;
    if canonical_parent != parent {
        return Err(GraphDbError::invalid(
            "graph database parent must be the exact canonical store directory",
        ));
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some("grafeo") {
        return Err(GraphDbError::invalid(
            "canonical graph database filename must end in .grafeo",
        ));
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            GraphDbError::invalid("canonical graph database must be a regular file"),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GraphDbError::unavailable(format!(
            "failed to inspect canonical graph database file {}: {error}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{inspect_graph_database_file, validate_graph_database_file};
    use crate::GraphDbError;
    use crate::location::PersistentGraphStoreState;

    #[test]
    fn inspection_classifies_a_prospective_and_existing_database_file() {
        let temp = tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap().join("graph.grafeo");

        assert_eq!(
            inspect_graph_database_file(&path).unwrap(),
            PersistentGraphStoreState::Prospective
        );
        std::fs::write(&path, b"fixture").unwrap();
        assert_eq!(
            inspect_graph_database_file(&path).unwrap(),
            PersistentGraphStoreState::Existing
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_graph_database_file_is_rejected() {
        let temp = tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let path = root.join("graph.grafeo");
        let target = root.join("target.grafeo");
        std::fs::write(&target, b"fixture").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();

        assert!(matches!(
            validate_graph_database_file(&path).unwrap_err(),
            GraphDbError::InvalidRequest { .. }
        ));
    }

    #[test]
    fn non_grafeo_filename_is_rejected() {
        let temp = tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap().join("graph.db");

        assert!(matches!(
            validate_graph_database_file(&path).unwrap_err(),
            GraphDbError::InvalidRequest { .. }
        ));
    }

    #[test]
    fn directory_at_database_path_is_rejected() {
        let temp = tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap().join("graph.grafeo");
        std::fs::create_dir(&path).unwrap();

        assert!(matches!(
            validate_graph_database_file(&path).unwrap_err(),
            GraphDbError::InvalidRequest { .. }
        ));
    }
}
