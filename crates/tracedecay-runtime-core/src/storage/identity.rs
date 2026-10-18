use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::git::GitOidV1;

use super::{
    PrivateStoreIo, REPOSITORY_IDENTITY_FILENAME, REPOSITORY_IDENTITY_SCHEMA_VERSION,
    RepositoryIdentityMarker, validate_project_id,
};

/// History that names a repository wherever its checkouts live, so a second
/// clone of an enrolled repository resolves to the same project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityAnchor {
    /// A parentless commit reachable from HEAD.
    RootCommit(GitOidV1),
    /// A project rooted below its repository's worktree root: one of the
    /// repository's root commits plus the project's path inside the checkout.
    Subproject { root: GitOidV1, relative: PathBuf },
    /// The normalized `origin` URL, used when the roots cannot be walked.
    Remote(String),
}

impl IdentityAnchor {
    /// The `project_aliases` key this anchor is recorded under.
    pub fn alias_key(&self) -> String {
        match self {
            Self::RootCommit(root) => format!("git-root-commit:{}", root.as_str()),
            Self::Subproject { root, relative } => format!(
                "git-subproject:{}:{}",
                root.as_str(),
                relative.to_string_lossy().replace('\\', "/")
            ),
            Self::Remote(url) => format!("git-remote-url:{url}"),
        }
    }
}

const ROOT_COMMIT_WALK_LIMIT: usize = 1_000_000;
const ROOT_COMMIT_WALK_DEADLINE: Duration = Duration::from_secs(10);

/// The anchors naming the repository `project_root` belongs to; empty outside
/// a repository, in a bare repository, or before the first commit.
pub fn repository_identity_anchors(project_root: &Path) -> Vec<IdentityAnchor> {
    let Ok(authority) = crate::git_repository::GitRepositoryAuthority::discover(project_root)
    else {
        return Vec::new();
    };
    let Some(worktree_root) = authority.worktree_root() else {
        return Vec::new();
    };
    let relative = fs::canonicalize(project_root)
        .ok()
        .and_then(|root| root.strip_prefix(worktree_root).ok().map(Path::to_path_buf))
        .unwrap_or_default();
    let remote = || {
        crate::git::git_remote_url(project_root)
            .as_deref()
            .and_then(crate::git::normalize_git_remote_url)
            .map(IdentityAnchor::Remote)
    };
    // ponytail: walks on every registry miss; memoize per (git dir, HEAD) if a
    // history without a commit-graph makes unenrolled lookups slow.
    match authority.root_commits(ROOT_COMMIT_WALK_LIMIT, ROOT_COMMIT_WALK_DEADLINE) {
        Some(roots) if roots.is_empty() => Vec::new(),
        // The remote is recorded beside the roots so a later shallow clone,
        // which can only name its remote, finds this project.
        Some(roots) if relative.as_os_str().is_empty() => roots
            .into_iter()
            .map(IdentityAnchor::RootCommit)
            .chain(remote())
            .collect(),
        Some(roots) => roots
            .into_iter()
            .map(|root| IdentityAnchor::Subproject {
                root,
                relative: relative.clone(),
            })
            .collect(),
        // A remote names the whole repository, so it cannot tell two
        // subprojects of one repository apart.
        None if !relative.as_os_str().is_empty() => Vec::new(),
        None => remote().into_iter().collect(),
    }
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
        let git = crate::git::try_git_program().map_err(|error| TraceDecayError::Config {
            message: format!(
                "cannot git init fixture '{}': {error}",
                project_root.display()
            ),
        })?;
        let status = std::process::Command::new(git)
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

/// [`write_repository_identity_marker`] unless a readable marker already names
/// another project. Every profile reads the same `.git/` marker, so one naming
/// another id is another profile's enrollment and is left as it is.
pub fn claim_repository_identity_marker(project_root: &Path, project_id: &str) -> Result<bool> {
    if let Ok(Some(marker)) = read_repository_identity_marker(project_root)
        && marker.project_id != project_id
    {
        return Ok(false);
    }
    write_repository_identity_marker(project_root, project_id)
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
