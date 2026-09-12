use std::path::{Component, Path, PathBuf};

use crate::GraphDbError;
use crate::location::PersistentGraphStoreState;

/// Validates the canonical graph database file and reports whether Grafeo must
/// create it. The registry never creates or opens the file itself.
pub(super) fn inspect_graph_database_file(
    path: &Path,
) -> Result<PersistentGraphStoreState, GraphDbError> {
    let canonical = canonical_graph_database_file(path)?;
    match std::fs::symlink_metadata(&canonical) {
        Ok(_) => Ok(PersistentGraphStoreState::Existing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(PersistentGraphStoreState::Prospective)
        }
        Err(error) => Err(GraphDbError::unavailable(format!(
            "failed to inspect canonical graph database file {}: {error}",
            canonical.display()
        ))),
    }
}

/// Resolves the store directory a graph database file lives in and returns the
/// database's canonical pathname.
///
/// The registry keys entries by this pathname and refuses a second shard that
/// claims one already registered, so the pathname has to be the file's single
/// name rather than whichever of its names a caller happened to spell. This
/// resolves the ancestors instead of refusing them: a host offers more than
/// one name for one directory -- `fs::canonicalize` returns the `\\?\`
/// verbatim form for every Windows path, and macOS reaches `/tmp` and `/var`
/// through symlinks to `/private/...` -- so demanding that a caller arrive
/// already spelled canonically refused locators that name exactly the
/// directory the registry resolved. Two spellings of one file now collapse to
/// one key, which is what the alias check was protecting in the first place.
///
/// What the ancestors are is the host's business; what the store directory
/// itself is remains the caller's, so a store directory that is a symlink is
/// still refused rather than followed, and so is a `.`/`..` spelling. This is
/// the same division the store locator resolver settled on.
pub(super) fn canonical_graph_database_file(path: &Path) -> Result<PathBuf, GraphDbError> {
    let parent = path
        .parent()
        .ok_or_else(|| GraphDbError::invalid("canonical graph database file has no parent"))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| GraphDbError::invalid("canonical graph database file has no file name"))?;
    if path.extension().and_then(|extension| extension.to_str()) != Some("grafeo") {
        return Err(GraphDbError::invalid(
            "canonical graph database filename must end in .grafeo",
        ));
    }
    if has_traversal_spelling(path) {
        return Err(GraphDbError::invalid(
            "canonical graph database path must not be spelled through '.' or '..'",
        ));
    }
    if std::fs::symlink_metadata(parent).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(GraphDbError::invalid(
            "graph database store directory must not be a symlink",
        ));
    }
    let canonical = std::fs::canonicalize(parent)
        .map_err(|error| {
            GraphDbError::unavailable(format!(
                "failed to resolve graph database parent {}: {error}",
                parent.display()
            ))
        })?
        .join(file_name);
    match std::fs::symlink_metadata(&canonical) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            GraphDbError::invalid("canonical graph database must be a regular file"),
        ),
        Ok(_) => Ok(canonical),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(canonical),
        Err(error) => Err(GraphDbError::unavailable(format!(
            "failed to inspect canonical graph database file {}: {error}",
            canonical.display()
        ))),
    }
}

/// Refuses `.` / `..` in the caller's spelling before canonicalize can erase
/// it. `Path::components` is the usual check; the raw-separator scan catches
/// a Windows `root\..\file` that some hosts collapse before ParentDir appears.
fn has_traversal_spelling(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        || path.to_str().is_some_and(|text| {
            text.split(['/', '\\'])
                .any(|component| component == "." || component == "..")
        })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::tempdir;

    use super::canonical_graph_database_file;
    use crate::GraphDbError;

    #[cfg(unix)]
    #[test]
    fn a_symlinked_store_directory_is_rejected() {
        let temp = tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let target = root.join("target");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, root.join("alias")).unwrap();

        assert!(matches!(
            canonical_graph_database_file(&root.join("alias").join("graph.grafeo")).unwrap_err(),
            GraphDbError::InvalidRequest { .. }
        ));
    }

    #[test]
    fn a_traversal_spelling_is_rejected() {
        let temp = tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();

        assert!(matches!(
            canonical_graph_database_file(&root.join("..").join("graph.grafeo")).unwrap_err(),
            GraphDbError::InvalidRequest { .. }
        ));
        assert!(matches!(
            canonical_graph_database_file(Path::new(r"root\..\graph.grafeo")).unwrap_err(),
            GraphDbError::InvalidRequest { .. }
        ));
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
            canonical_graph_database_file(&path).unwrap_err(),
            GraphDbError::InvalidRequest { .. }
        ));
    }

    #[test]
    fn non_grafeo_filename_is_rejected() {
        let temp = tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap().join("graph.db");

        assert!(matches!(
            canonical_graph_database_file(&path).unwrap_err(),
            GraphDbError::InvalidRequest { .. }
        ));
    }

    #[test]
    fn directory_at_database_path_is_rejected() {
        let temp = tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap().join("graph.grafeo");
        std::fs::create_dir(&path).unwrap();

        assert!(matches!(
            canonical_graph_database_file(&path).unwrap_err(),
            GraphDbError::InvalidRequest { .. }
        ));
    }
}
