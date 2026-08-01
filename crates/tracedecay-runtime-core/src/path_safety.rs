//! Filesystem path primitives shared across the runtime.
//!
//! Two families live here because both were re-implemented in several
//! subsystems at once:
//!
//! * canonicalization that tolerates a not-yet-created (or since-moved) tail,
//!   plus the lexical `.`/`..` collapse some identity paths apply on top of
//!   it; and
//! * the relative-path validation every source-edit authority runs before it
//!   opens a file beneath a project worktree.
//!
//! The canonicalization callers deliberately differ in what they do with the
//! result — see [`collapse_relative_components`] — so only the algorithm is
//! shared, never the policy.

use std::io;
use std::path::{Component, Path, PathBuf};

use crate::errors::{Result, TraceDecayError};

/// Canonicalizes `path`, or the deepest ancestor that canonicalizes with the
/// still-missing tail reattached. `None` when no ancestor canonicalizes.
///
/// Resolving through the deepest *existing* ancestor is what preserves OS
/// aliases such as macOS `/var` -> `/private/var` for a path whose final
/// components do not exist yet, or whose directory was moved or had a symlink
/// alias removed after the path was recorded.
#[must_use]
pub fn canonicalize_existing_prefix(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = path.canonicalize() {
        return Some(canonical);
    }

    let mut current = path;
    let mut missing_suffix = PathBuf::new();
    while let Some(name) = current.file_name() {
        missing_suffix = Path::new(name).join(missing_suffix);
        current = current.parent()?;
        if let Ok(canonical_parent) = current.canonicalize() {
            return Some(canonical_parent.join(missing_suffix));
        }
    }

    None
}

/// [`canonicalize_existing_prefix`] falling back to `path` unchanged.
#[must_use]
pub fn canonicalize_path_or_existing_parent(path: &Path) -> PathBuf {
    canonicalize_existing_prefix(path).unwrap_or_else(|| path.to_path_buf())
}

/// Drops `.` and resolves `..` lexically, without touching the filesystem.
///
/// This is a separate step rather than part of canonicalization because the
/// callers genuinely disagree about it: the daemon authority applies it to the
/// identity paths it writes, while the profile-identity migration must not —
/// collapsing there would rewrite identity strings already on disk.
#[must_use]
pub fn collapse_relative_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Reduces `path` to the plain relative path a source edit may address
/// beneath its authorized worktree.
///
/// `.` components are dropped; anything that could escape or re-root the
/// worktree — an absolute path, a prefix or root component, `..`, or a path
/// that normalizes away to nothing — is rejected rather than repaired.
pub fn normalize_source_edit_relative_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(source_edit_unsafe_path());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => normalized.push(component),
            Component::CurDir => {}
            _ => return Err(source_edit_unsafe_path()),
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(source_edit_unsafe_path());
    }
    Ok(normalized)
}

/// The single rejection every source-edit path check reports, so a caller
/// cannot learn which specific check refused it.
#[must_use]
pub fn source_edit_unsafe_path() -> TraceDecayError {
    TraceDecayError::Config {
        message: "source edit path is not a regular file beneath the authorized worktree"
            .to_owned(),
    }
}

/// Names the failed source-edit filesystem operation alongside its `io` cause.
#[must_use]
pub fn source_edit_path_error(operation: &'static str, error: io::Error) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("{operation}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        canonicalize_existing_prefix, collapse_relative_components,
        normalize_source_edit_relative_path,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn canonicalization_reattaches_a_missing_tail_to_an_existing_ancestor() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let existing = temp.path().canonicalize().expect("canonical temp root");
        let missing = temp.path().join("absent").join("deeper.db");

        assert_eq!(
            canonicalize_existing_prefix(&missing),
            Some(existing.join("absent").join("deeper.db"))
        );
    }

    #[test]
    fn canonicalization_reports_no_existing_ancestor() {
        assert_eq!(canonicalize_existing_prefix(Path::new("")), None);
    }

    #[test]
    fn relative_components_collapse_lexically() {
        assert_eq!(
            collapse_relative_components(Path::new("/a/./b/../c")),
            PathBuf::from("/a/c")
        );
    }

    #[test]
    fn source_edit_paths_reject_everything_that_could_leave_the_worktree() {
        assert_eq!(
            normalize_source_edit_relative_path(Path::new("./src/lib.rs"))
                .expect("a plain relative path is addressable"),
            PathBuf::from("src/lib.rs")
        );
        for rejected in ["", "/etc/passwd", "../outside", "."] {
            assert!(
                normalize_source_edit_relative_path(Path::new(rejected)).is_err(),
                "'{rejected}' must not be addressable"
            );
        }
    }
}
