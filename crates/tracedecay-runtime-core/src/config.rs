//! Kernel-owned configuration primitives.
//!
//! These items used to live in the root crate's `config` module, but the
//! storage layout, database, branch-metadata, and store layers all need them
//! and those layers moved into this crate. The root `config` module re-exports
//! every item declared here, so `crate::config::<item>` keeps resolving on
//! both sides of the split.

use std::path::{Path, PathBuf};

/// Name of the hidden directory used to store `TraceDecay` metadata.
pub const TRACEDECAY_DIR: &str = ".tracedecay";

/// Environment variable that pins the user-level `TraceDecay` data directory.
pub const USER_DATA_DIR_ENV: &str = "TRACEDECAY_DATA_DIR";

/// Project graph database filename inside a `.tracedecay/` data dir.
pub const DB_FILENAME: &str = "tracedecay.db";

/// Returns the project marker directory for the given project root.
///
/// New runtime storage lives in the user-level profile shard. The project root
/// only carries lightweight marker/config files under `.tracedecay/`.
pub fn get_tracedecay_dir(project_root: &Path) -> PathBuf {
    project_root.join(TRACEDECAY_DIR)
}

/// Name of the project marker directory for this project root.
pub fn active_data_dir_name(project_root: &Path) -> &'static str {
    let _ = project_root;
    TRACEDECAY_DIR
}

/// Database filename appropriate for the given data directory.
pub fn db_filename(data_dir: &Path) -> &'static str {
    let _ = data_dir;
    DB_FILENAME
}

/// Full path to the repo-local graph database marker path.
///
/// Normal runtime graph storage resolves through [`crate::storage::StoreLayout`]
/// into the user profile shard; this helper is only for explicit marker checks
/// and migration cleanup.
pub fn get_project_db_path(project_root: &Path) -> PathBuf {
    get_tracedecay_dir(project_root).join(DB_FILENAME)
}

/// Returns true when the old repo-local `TraceDecay` graph DB exists at this root.
pub fn has_project_database(project_root: &Path) -> bool {
    project_root.join(TRACEDECAY_DIR).join(DB_FILENAME).exists()
}

/// User-level data directory. Runtime storage is always rooted at
/// `~/.tracedecay` unless `TRACEDECAY_DATA_DIR` explicitly overrides it.
pub fn user_data_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(USER_DATA_DIR_ENV).filter(|path| !path.is_empty()) {
        return Some(nextest_isolated_user_data_dir(canonicalize_data_dir(
            PathBuf::from(path),
        )));
    }
    let home = dirs::home_dir()?;
    Some(canonicalize_data_dir(home.join(TRACEDECAY_DIR)))
}

fn nextest_isolated_user_data_dir(path: PathBuf) -> PathBuf {
    use std::hash::{Hash, Hasher};

    let Some(test_name) = std::env::var_os("NEXTEST_TEST_NAME").filter(|name| !name.is_empty())
    else {
        return path;
    };
    let Some(profile_dir) = path.parent() else {
        return path;
    };
    if path.file_name() != Some(std::ffi::OsStr::new(TRACEDECAY_DIR)) {
        return path;
    }

    let profile_name = profile_dir.file_name().and_then(std::ffi::OsStr::to_str);
    let target_profile = profile_name == Some("test-profile")
        && profile_dir
            .parent()
            .is_some_and(|target| target.join("debug").is_dir());
    let ci_profile =
        profile_name == Some("tracedecay-test-profile") && std::env::var_os("CI").is_some();
    if !target_profile && !ci_profile {
        return path;
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::env::var_os("NEXTEST_RUN_ID")
        .unwrap_or_default()
        .to_string_lossy()
        .hash(&mut hasher);
    std::env::var_os("NEXTEST_ATTEMPT_ID")
        .unwrap_or_default()
        .to_string_lossy()
        .hash(&mut hasher);
    std::env::var_os("NEXTEST_BINARY_ID")
        .unwrap_or_default()
        .to_string_lossy()
        .hash(&mut hasher);
    test_name.to_string_lossy().hash(&mut hasher);
    path.join("nextest")
        .join(format!("{:016x}", hasher.finish()))
}

fn canonicalize_data_dir(path: PathBuf) -> PathBuf {
    if !path.is_absolute() {
        return path;
    }
    canonicalize_path_or_existing_parent(&path)
}

fn canonicalize_path_or_existing_parent(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }

    let mut current = path;
    let mut missing_suffix = PathBuf::new();
    while let Some(name) = current.file_name() {
        missing_suffix = Path::new(name).join(missing_suffix);
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent;
        if let Ok(canonical_parent) = current.canonicalize() {
            return canonical_parent.join(missing_suffix);
        }
    }

    path.to_path_buf()
}

/// Walks up from `start` looking for the nearest ancestor that hosts an
/// initialised `TraceDecay` project, or `None` if the filesystem root is
/// reached without finding one.
///
/// # Canonical local project-root resolution order
///
/// This walk-up is the heart of project-root resolution. Every entry point
/// that needs a project root should resolve it in this order — new code must
/// converge on this chain instead of inventing its own:
///
/// 0. **Template pre-filter** (`serve` only, `sanitize_serve_path_arg`): an
///    explicit path that is a literal unexpanded `${...}` host template
///    variable (e.g. `${workspaceFolder}` from a host that failed to expand
///    it) is discarded with a warning and resolution continues as if no path
///    was given.
/// 1. **Explicit path** (`--path`/`-p`, tool `path` argument): used verbatim,
///    no discovery, and failure to open is fatal — never silently fall back.
/// 2. **CWD walk-up** (this function via `resolve_path_with_discovery`):
///    nearest ancestor of the working directory containing an initialised
///    project database (see [`get_project_db_path`]).
///
/// `serve` forwards this routing metadata to the managed daemon. MCP
/// `initialize` roots and registry aliases are resolved there; the proxy never
/// opens a project or global database and has no in-process fallback.
pub fn discover_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    let worktree_root = crate::worktree::git_worktree_root(start);
    loop {
        if has_project_database(&dir)
            || crate::storage::has_enrollment_marker(&dir)
            || crate::storage::has_path_local_profile_store(&dir)
        {
            return Some(dir);
        }
        if worktree_root
            .as_ref()
            .is_some_and(|root| paths_same(&dir, root))
        {
            return None;
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn paths_same(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}
