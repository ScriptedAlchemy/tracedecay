use std::fs;
use std::path::{Path, PathBuf};

use crate::config::TRACEDECAY_DIR;
use crate::errors::{Result, TraceDecayError};

use super::{
    ENROLLMENT_FILENAME, EnrollmentMarker, PrivateStoreIo, REPOSITORY_IDENTITY_FILENAME,
    REPOSITORY_IDENTITY_SCHEMA_VERSION, RepositoryIdentityMarker, validate_enrollment_marker,
    validate_project_id,
};

/// Location of the retired repo-local enrollment marker.
///
/// `TraceDecay` never creates files inside a project's working tree. This path
/// exists only so legacy identity can be adopted (read once, ingested into
/// the home-profile registry) and so cleanup flows can recognize the debris.
/// Users may delete the file at any time.
pub fn legacy_enrollment_marker_path(project_root: &Path) -> PathBuf {
    project_root.join(TRACEDECAY_DIR).join(ENROLLMENT_FILENAME)
}

/// Reads the retired repo-local enrollment marker, if the user still has one.
///
/// Read-only legacy adoption source: registry-aware resolution ingests the
/// identity it names exactly once (when the project is not otherwise
/// resolvable) and never consults the file again. Nothing writes it.
pub fn read_legacy_enrollment_marker(project_root: &Path) -> Result<Option<EnrollmentMarker>> {
    let path = legacy_enrollment_marker_path(project_root);
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

/// Whether this checkout's repository carries a `.git/`-side identity marker.
///
/// Presence-only probe for discovery walks; identity resolution goes through
/// [`read_repository_identity_marker`], which also validates the contents.
pub fn has_repository_identity_marker(project_root: &Path) -> bool {
    repository_identity_path(project_root).is_some_and(|path| path.is_file())
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

/// Pins a fixture checkout's identity in the sanctioned `.git/` repository
/// identity marker, initializing a real git repository first when the fixture
/// root does not already have one.
///
/// Test-support only: production identity minting flows through init/open and
/// the registry. Fixtures use this instead of fabricating any working-tree
/// state.
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
pub fn pin_fixture_repository_identity(project_root: &Path, project_id: &str) -> Result<()> {
    if crate::worktree::git_common_dir(project_root).is_none() {
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(project_root)
            .status()
            .map_err(|e| TraceDecayError::Config {
                message: format!(
                    "failed to run git init in fixture '{}': {e}",
                    project_root.display()
                ),
            })?;
        if !status.success() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "git init failed in fixture '{}': {status}",
                    project_root.display()
                ),
            });
        }
    }
    if !write_repository_identity_marker(project_root, project_id)? {
        return Err(TraceDecayError::Config {
            message: format!(
                "fixture '{}' did not accept a repository identity marker",
                project_root.display()
            ),
        });
    }
    Ok(())
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
