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

use tracedecay_domain::errors::{Result, TraceDecayError};

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

    // The missing tail is collected outermost-last and re-pushed in order.
    // Building it with `Path::join` against an empty `PathBuf` instead would
    // leave a trailing separator on the result, which compares equal as a
    // `Path` but not as the string form registries and identity records store.
    let mut current = path;
    let mut missing_suffix: Vec<&std::ffi::OsStr> = Vec::new();
    while let Some(name) = current.file_name() {
        missing_suffix.push(name);
        current = current.parent()?;
        if let Ok(canonical_parent) = current.canonicalize() {
            let mut resolved = canonical_parent;
            resolved.extend(missing_suffix.iter().rev());
            return Some(resolved);
        }
    }

    None
}

/// [`canonicalize_existing_prefix`] falling back to `path` unchanged.
#[must_use]
pub fn canonicalize_path_or_existing_parent(path: &Path) -> PathBuf {
    canonicalize_existing_prefix(path).unwrap_or_else(|| path.to_path_buf())
}

/// Whether two pathnames name the same file, whatever spelling each carries.
///
/// A host offers more than one name for one file, and the two sides of a
/// registry check routinely arrive spelled differently: one side has been
/// through [`std::fs::canonicalize`] and the other is the name a caller built.
/// `canonicalize` returns the `\\?\` verbatim form for every Windows path, so
/// a registered locator reads `\\?\D:\store\graph.db` where its caller built
/// `D:\store\graph.db`; macOS reaches `/tmp` and `/var` through symlinks, so
/// the same pair reads `/private/var/...` against `/var/...`. Comparing the
/// spellings reports two names of one file as two files.
///
/// Both sides are resolved through their deepest existing ancestor, so a
/// locator whose final component has not been created yet still compares.
#[must_use]
pub fn same_canonical_path(left: &Path, right: &Path) -> bool {
    left == right
        || canonicalize_path_or_existing_parent(left) == canonicalize_path_or_existing_parent(right)
}

/// Rewrites a Windows extended-length (`\\?\`) *disk* path to its ordinary
/// form, so it can be handed to a tool that does not understand the verbatim
/// prefix.
///
/// [`std::fs::canonicalize`] returns the verbatim form for every Windows path,
/// so every path this runtime resolves reads `\\?\D:\repo\.git` rather than
/// `D:\repo\.git`. Git for Windows normalizes the paths it is given and
/// rejects that spelling — most visibly through
/// `GIT_ALTERNATE_OBJECT_DIRECTORIES`, where a verbatim entry makes every
/// object-writing command fail — so a resolved path must be spelled plainly
/// before it crosses into a child process.
///
/// Only a verbatim disk path is shortened. `\\?\UNC\server\share` and device
/// namespace paths genuinely need the prefix and are returned unchanged, as is
/// every path that does not carry one (which is every path on Unix).
#[must_use]
pub fn plain_host_path(path: &Path) -> PathBuf {
    // Deliberately a string transform rather than `Path::components`: the
    // prefix is only *parsed* as a prefix on Windows, and this must behave
    // identically wherever the path was produced, so it can be exercised on
    // any host.
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    let Some(rest) = text.strip_prefix(r"\\?\") else {
        return path.to_path_buf();
    };
    let bytes = rest.as_bytes();
    let is_verbatim_disk =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\';
    if is_verbatim_disk {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
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
        normalize_source_edit_relative_path, plain_host_path, same_canonical_path,
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

    /// The registry shape: one side has been canonicalized, the other is the
    /// name its caller built, and the file itself does not exist yet.
    #[test]
    fn a_canonicalized_locator_matches_the_spelling_its_caller_built() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let built = temp.path().join("projects").join("graph.db");
        let canonical = temp
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("projects")
            .join("graph.db");

        assert!(same_canonical_path(&canonical, &built));
        assert!(same_canonical_path(&built, &canonical));
    }

    /// The macOS shape reproduced on every host that has symlinks: `/var` and
    /// `/private/var` are one directory reached by two names.
    #[cfg(unix)]
    #[test]
    fn a_locator_reached_through_a_symlinked_ancestor_matches_its_target() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().canonicalize().expect("canonical temp root");
        let real = root.join("private");
        std::fs::create_dir(&real).expect("target directory");
        std::os::unix::fs::symlink(&real, root.join("alias")).expect("directory alias");

        assert!(same_canonical_path(
            &real.join("graph.db"),
            &root.join("alias").join("graph.db"),
        ));
    }

    #[test]
    fn two_distinct_locators_stay_distinct() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().canonicalize().expect("canonical temp root");

        assert!(!same_canonical_path(
            &root.join("left.db"),
            &root.join("right.db"),
        ));
    }

    #[test]
    fn relative_components_collapse_lexically() {
        assert_eq!(
            collapse_relative_components(Path::new("/a/./b/../c")),
            PathBuf::from("/a/c")
        );
    }

    /// Runs on every host: the transform is defined on the spelling, not on
    /// how the running platform happens to parse it.
    #[test]
    fn only_a_verbatim_disk_path_is_spelled_plainly_for_child_processes() {
        assert_eq!(
            plain_host_path(Path::new(r"\\?\D:\a\_temp\repo\.git\objects")),
            PathBuf::from(r"D:\a\_temp\repo\.git\objects")
        );
        assert_eq!(
            plain_host_path(Path::new(r"\\?\c:\repo")),
            PathBuf::from(r"c:\repo")
        );
        for preserved in [
            r"\\?\UNC\server\share\repo",
            r"\\?\Volume{9f4c2b1e-0000-0000-0000-100000000000}\repo",
            r"\\server\share\repo",
            r"D:\repo",
            "/home/user/repo",
        ] {
            assert_eq!(
                plain_host_path(Path::new(preserved)),
                PathBuf::from(preserved),
                "{preserved} must be handed on unchanged"
            );
        }
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
