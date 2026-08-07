use std::fs;
use std::path::{Path, PathBuf};

use crate::config::TRACEDECAY_DIR;
use crate::errors::{Result, TraceDecayError};

use super::{
    ENROLLMENT_FILENAME, EnrollmentMarker, PrivateStoreIo, REPOSITORY_IDENTITY_FILENAME,
    REPOSITORY_IDENTITY_SCHEMA_VERSION, RepositoryIdentityMarker, StorageMode,
    validate_enrollment_marker, validate_project_id,
};

pub fn enrollment_marker_path(project_root: &Path) -> PathBuf {
    project_root.join(TRACEDECAY_DIR).join(ENROLLMENT_FILENAME)
}

pub fn has_enrollment_marker(project_root: &Path) -> bool {
    matches!(
        read_enrollment_marker(project_root),
        Ok(Some(marker)) if marker.storage_mode == StorageMode::ProfileSharded
    )
}

pub fn read_enrollment_marker(project_root: &Path) -> Result<Option<EnrollmentMarker>> {
    let path = enrollment_marker_path(project_root);
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|e| TraceDecayError::Config {
        message: format!("failed to read enrollment marker '{}': {e}", path.display()),
    })?;
    let marker = serde_json::from_str(&text).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to parse enrollment marker '{}': {e}",
            path.display()
        ),
    })?;
    validate_enrollment_marker(&marker, &path)?;
    Ok(Some(marker))
}

pub fn write_enrollment_marker(project_root: &Path, marker: &EnrollmentMarker) -> Result<()> {
    validate_enrollment_marker(marker, &enrollment_marker_path(project_root))?;
    let path = enrollment_marker_path(project_root);
    let text = serde_json::to_vec_pretty(marker).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to serialize enrollment marker '{}': {e}",
            path.display()
        ),
    })?;
    // Several independent paths enroll the same project (CLI init, the
    // daemon's first-touch open, enrollment-root repair) while the store
    // resolver may read the marker concurrently. A truncate-then-write here
    // briefly exposes an empty file, which the resolver reports as an
    // invalid/missing enrollment and callers surface as a denial. Replace
    // atomically so a reader only ever sees a complete marker or none.
    let temp_path = path.with_extension(format!(
        "json.tmp-{}-{}",
        std::process::id(),
        enrollment_marker_temp_nonce()
    ));
    PrivateStoreIo::write_file_atomically(&path, &temp_path, &text).map_err(|e| {
        TraceDecayError::Config {
            message: format!(
                "failed to write enrollment marker '{}': {e}",
                path.display()
            ),
        }
    })
}

/// Distinguishes concurrent in-process enrollment writers, which would
/// otherwise race each other on one shared pid-derived temp path.
fn enrollment_marker_temp_nonce() -> u64 {
    static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub fn remove_enrollment_marker(project_root: &Path, project_id: &str) -> Result<bool> {
    let path = enrollment_marker_path(project_root);
    let Some(marker) = read_enrollment_marker(project_root)? else {
        return Ok(false);
    };
    if marker.project_id != project_id || marker.storage_mode != StorageMode::ProfileSharded {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to remove enrollment marker '{}': it does not match project_id '{}'",
                path.display(),
                project_id
            ),
        });
    }
    fs::remove_file(&path).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to remove enrollment marker '{}': {e}",
            path.display()
        ),
    })?;
    Ok(true)
}

/// The repository-wide identity marker shared by every checkout of a
/// repository, including detached linked worktrees.
///
/// Detached worktrees share repository identity with the primary checkout.
/// Worktree/ref/snapshot identity is retained as query and generation
/// provenance; it never selects a second mutable project database.
pub fn repository_identity_path(project_root: &Path) -> Option<PathBuf> {
    crate::worktree::git_common_dir(project_root)
        .map(|common_dir| common_dir.join(REPOSITORY_IDENTITY_FILENAME))
}

pub fn read_repository_identity_marker(
    project_root: &Path,
) -> Result<Option<RepositoryIdentityMarker>> {
    let Some(path) = repository_identity_path(project_root) else {
        return Ok(None);
    };
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to read repository identity marker '{}': {e}",
            path.display()
        ),
    })?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to parse repository identity marker '{}': {e}",
                path.display()
            ),
        })?;
    let schema_version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "repository identity marker '{}' has no valid schema_version",
                path.display()
            ),
        })?;
    if schema_version != REPOSITORY_IDENTITY_SCHEMA_VERSION {
        return Err(TraceDecayError::Config {
            message: format!(
                "unsupported repository identity schema_version={} in '{}'; expected {}",
                schema_version,
                path.display(),
                REPOSITORY_IDENTITY_SCHEMA_VERSION
            ),
        });
    }
    let marker: RepositoryIdentityMarker =
        serde_json::from_value(value).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to parse repository identity marker '{}': {e}",
                path.display()
            ),
        })?;
    validate_project_id(&marker.project_id).map_err(|message| TraceDecayError::Config {
        message: format!(
            "invalid repository identity marker '{}': {message}",
            path.display()
        ),
    })?;
    let stored_common_dir = Path::new(&marker.git_common_dir);
    if !stored_common_dir.is_absolute() {
        return Err(TraceDecayError::Config {
            message: format!(
                "invalid repository identity marker '{}': git_common_dir must be absolute",
                path.display()
            ),
        });
    }
    let current_common_dir = path.parent().ok_or_else(|| TraceDecayError::Config {
        message: format!(
            "repository identity marker '{}' has no parent directory",
            path.display()
        ),
    })?;
    let stored_key = stored_common_dir
        .canonicalize()
        .unwrap_or_else(|_| stored_common_dir.to_path_buf());
    let current_key = current_common_dir
        .canonicalize()
        .unwrap_or_else(|_| current_common_dir.to_path_buf());
    if stored_key != current_key
        && stored_common_dir.exists()
        && stored_dir_marker_names_project(stored_common_dir, &marker.project_id)
    {
        // The stored git common dir still exists, canonicalizes to a different
        // live directory, and hosts a marker naming the SAME project: this is a
        // genuine true copy (e.g. `cp -a`/rsync duplicated the marker) with two
        // live checkouts claiming one project id. Fail closed. A move where the
        // old path was reused by an UNRELATED repo (absent/unreadable/different
        // marker there) is accepted below and self-heals on the next writable
        // open, which rewrites git_common_dir to this checkout.
        return Err(TraceDecayError::Config {
            message: format!(
                "repository identity conflict: marker '{}' names project '{}' but its original \
                 git common directory '{}' is still live; this checkout uses '{}'",
                path.display(),
                marker.project_id,
                stored_common_dir.display(),
                current_common_dir.display()
            ),
        });
    }
    Ok(Some(marker))
}

/// Probe the repository identity marker stored inside `stored_common_dir` and
/// report whether it names `expected_project_id`.
///
/// This is a raw JSON read that deliberately does NOT recurse through
/// [`read_repository_identity_marker`] (which would re-run conflict detection
/// against the probed directory). An absent, unreadable, malformed, or
/// differently-named marker returns `false`.
fn stored_dir_marker_names_project(stored_common_dir: &Path, expected_project_id: &str) -> bool {
    let marker_path = stored_common_dir.join(REPOSITORY_IDENTITY_FILENAME);
    let Ok(text) = fs::read_to_string(&marker_path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    value.get("project_id").and_then(serde_json::Value::as_str) == Some(expected_project_id)
}

pub fn write_repository_identity_marker(project_root: &Path, project_id: &str) -> Result<bool> {
    validate_project_id(project_id).map_err(|message| TraceDecayError::Config {
        message: message.to_string(),
    })?;
    let Some(path) = repository_identity_path(project_root) else {
        return Ok(false);
    };
    let git_common_dir = path.parent().ok_or_else(|| TraceDecayError::Config {
        message: format!(
            "repository identity marker '{}' has no parent directory",
            path.display()
        ),
    })?;
    let marker = RepositoryIdentityMarker {
        schema_version: REPOSITORY_IDENTITY_SCHEMA_VERSION,
        project_id: project_id.to_string(),
        git_common_dir: git_common_dir.to_string_lossy().to_string(),
    };
    let contents = serde_json::to_vec_pretty(&marker).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to serialize repository identity marker '{}': {e}",
            path.display()
        ),
    })?;
    let temp_path = path.with_extension(format!("json.tmp-{}", std::process::id()));
    PrivateStoreIo::write_file_atomically(&path, &temp_path, &contents).map_err(|e| {
        TraceDecayError::Config {
            message: format!(
                "failed to write repository identity marker '{}': {e}",
                path.display()
            ),
        }
    })?;
    Ok(true)
}
