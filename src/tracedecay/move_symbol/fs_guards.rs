//! Filesystem write guards for `move_symbol`: containment (no symlink
//! escapes), same-file detection, symlink-preserving write targets, and the
//! optimistic concurrency check that refuses to apply a move against files
//! that changed since the preview snapshot was taken.

use std::path::{Path, PathBuf};

use crate::errors::{Result, TraceDecayError};

/// Reject a destination whose existing file or nearest existing parent
/// resolves outside the canonical project root. This covers both a symlinked
/// destination file and a symlinked directory component while still allowing
/// symlinks that stay inside the checkout.
pub(super) fn validate_write_containment(
    project_root: &Path,
    path: &Path,
    label: &str,
) -> Result<()> {
    let canonical_root = project_root
        .canonicalize()
        .map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to canonicalize project root '{}': {e}",
                project_root.display()
            ),
        })?;
    let mut existing = path;
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                existing = existing.parent().ok_or_else(|| TraceDecayError::Config {
                    message: format!("{label} '{}' has no existing parent", path.display()),
                })?;
            }
            Err(e) => {
                return Err(TraceDecayError::Config {
                    message: format!("failed to inspect {label} '{}': {e}", path.display()),
                });
            }
        }
    }
    let canonical_existing = existing
        .canonicalize()
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to resolve {label} '{}': {e}", path.display()),
        })?;
    if !canonical_existing.starts_with(&canonical_root) {
        return Err(TraceDecayError::Config {
            message: format!(
                "{label} '{}' escapes project root through '{}'",
                path.display(),
                existing.display()
            ),
        });
    }
    Ok(())
}

pub(super) fn same_existing_file(source: &Path, destination: &Path) -> bool {
    same_file::is_same_file(source, destination).unwrap_or(false)
}

pub(super) fn write_path_preserving_final_symlink(path: &Path, label: &str) -> Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            path.canonicalize().map_err(|e| TraceDecayError::Config {
                message: format!("failed to resolve {label} '{}': {e}", path.display()),
            })
        }
        Ok(_) => Ok(path.to_path_buf()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(e) => Err(TraceDecayError::Config {
            message: format!("failed to inspect {label} '{}': {e}", path.display()),
        }),
    }
}

pub(super) fn ensure_text_unchanged(
    path: &Path,
    expected: Option<&str>,
    label: &str,
) -> Result<()> {
    match expected {
        Some(expected) => match std::fs::read_to_string(path) {
            Ok(current) if current == expected => Ok(()),
            Ok(_) => Err(TraceDecayError::Config {
                message: format!("{label} changed while the move was being prepared; retry"),
            }),
            Err(e) => Err(TraceDecayError::Config {
                message: format!("failed to re-read {label} before applying move: {e}"),
            }),
        },
        None => match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(TraceDecayError::Config {
                message: format!("{label} was created while the move was being prepared; retry"),
            }),
            Err(e) => Err(TraceDecayError::Config {
                message: format!("failed to re-check {label} before applying move: {e}"),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::write_path_preserving_final_symlink;
    use super::{ensure_text_unchanged, same_existing_file};

    #[test]
    fn same_existing_file_detects_hard_link() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.rs");
        let alias = dir.path().join("alias.rs");
        std::fs::write(&source, "fn source() {}\n").unwrap();
        std::fs::hard_link(&source, &alias).unwrap();
        assert!(same_existing_file(&source, &alias));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_target_preserves_final_symlink() {
        use std::os::unix::fs as unix_fs;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.rs");
        let alias = dir.path().join("alias.rs");
        std::fs::write(&target, "fn old() {}\n").unwrap();
        unix_fs::symlink(&target, &alias).unwrap();

        let write_path = write_path_preserving_final_symlink(&alias, "test").unwrap();
        crate::agents::safe_write_text_file(&write_path, "fn new() {}\n", None).unwrap();

        assert!(
            std::fs::symlink_metadata(&alias)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "fn new() {}\n");
    }

    #[test]
    fn optimistic_write_guard_rejects_changed_or_created_files() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("existing.rs");
        let created = dir.path().join("created.rs");
        std::fs::write(&existing, "fn before() {}\n").unwrap();
        std::fs::write(&existing, "fn concurrent() {}\n").unwrap();
        std::fs::write(&created, "fn appeared() {}\n").unwrap();

        let changed = ensure_text_unchanged(&existing, Some("fn before() {}\n"), "source")
            .unwrap_err()
            .to_string();
        assert!(changed.contains("changed while the move was being prepared"));
        let appeared = ensure_text_unchanged(&created, None, "destination")
            .unwrap_err()
            .to_string();
        assert!(appeared.contains("was created while the move was being prepared"));
    }
}
