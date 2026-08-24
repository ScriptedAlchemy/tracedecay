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
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
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

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{canonical_graph_database_file, inspect_graph_database_file};
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

    /// Callers build a database pathname from a store root they were handed,
    /// which is not always the root's canonical spelling: Windows
    /// canonicalization produces the `\\?\` verbatim form, macOS resolves
    /// `/var` to `/private/var`. The boundary resolves that spelling to the
    /// one pathname the registry keys on rather than refusing it.
    #[test]
    fn a_caller_spelling_resolves_to_the_canonical_database_pathname() {
        let temp = tempdir().unwrap();
        let canonical_root = temp.path().canonicalize().unwrap();

        assert_eq!(
            canonical_graph_database_file(&temp.path().join("graph.grafeo")).unwrap(),
            canonical_root.join("graph.grafeo")
        );
        assert_eq!(
            inspect_graph_database_file(&temp.path().join("graph.grafeo")).unwrap(),
            PersistentGraphStoreState::Prospective
        );
    }

    /// The macOS shape, reproduced on any host with symlinks: the store
    /// directory is real, but an *ancestor* of it is an alias -- exactly how
    /// `/var/folders/...` reaches `/private/var/folders/...`. The ancestor
    /// belongs to the host, so it resolves; the store directory itself is
    /// still required to be a directory rather than a link (see
    /// `a_symlinked_store_directory_is_rejected`).
    #[cfg(unix)]
    #[test]
    fn a_store_directory_below_an_aliased_ancestor_resolves_to_its_target() {
        let temp = tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let real = root.join("private");
        let store = real.join("store");
        std::fs::create_dir_all(&store).unwrap();
        std::os::unix::fs::symlink(&real, root.join("alias")).unwrap();

        assert_eq!(
            canonical_graph_database_file(&root.join("alias").join("store").join("graph.grafeo"))
                .unwrap(),
            store.join("graph.grafeo")
        );
    }

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
