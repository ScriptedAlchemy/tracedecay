//! Kernel-owned configuration primitives.
//!
//! These items used to live in the root crate's `config` module, but the
//! storage layout, database, branch-metadata, and store layers all need them
//! and those layers moved into this crate. The root `config` module re-exports
//! every item declared here, so `crate::config::<item>` keeps resolving on
//! both sides of the split.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Name of the hidden directory used to store `TraceDecay` metadata.
pub const TRACEDECAY_DIR: &str = ".tracedecay";

/// Environment variable that pins the user-level `TraceDecay` data directory.
pub const USER_DATA_DIR_ENV: &str = "TRACEDECAY_DATA_DIR";

/// Environment variable that pins the user-level global database path.
pub const GLOBAL_DB_PATH_ENV: &str = "TRACEDECAY_GLOBAL_DB";

/// Project graph database filename inside a `.tracedecay/` data dir.
pub const DB_FILENAME: &str = "tracedecay.db";

/// Filename of the user-level global database inside the profile root.
pub const GLOBAL_DB_FILENAME: &str = "global.db";

/// Output directories a cargo target dir may hold for this workspace: the
/// built-in profiles plus the `perf` profile that `cargo test-ci`/`test-all`
/// and the CI test lanes build with (see `Cargo.toml` `[profile.perf]`).
/// Heuristics that recognise "inside a cargo target dir" by layout must
/// accept every entry, or they silently switch off under one profile.
pub const CARGO_PROFILE_DIRS: &[&str] = &["debug", "release", "perf"];

/// True when `target_dir` holds a build output directory for any workspace
/// cargo profile.
pub fn holds_cargo_profile_dir(target_dir: &Path) -> bool {
    CARGO_PROFILE_DIRS
        .iter()
        .any(|profile| target_dir.join(profile).is_dir())
}

/// New runtime storage lives in the user-level profile shard. The project root
/// only carries lightweight marker/config files under `.tracedecay/`.
pub fn get_tracedecay_dir(project_root: &Path) -> PathBuf {
    project_root.join(TRACEDECAY_DIR)
}

pub fn active_data_dir_name(project_root: &Path) -> &'static str {
    let _ = project_root;
    TRACEDECAY_DIR
}

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
    let home = home_dir()?;
    Some(canonicalize_data_dir(home.join(TRACEDECAY_DIR)))
}

/// Process home directory used when `TRACEDECAY_DATA_DIR` is unset.
///
/// Matches [`PinnedUserDataDir`]: `HOME` on Unix, `USERPROFILE` on Windows.
/// Absence is `None`; callers map that into a typed configuration error.
fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    const HOME_ENV: &str = "USERPROFILE";
    #[cfg(not(windows))]
    const HOME_ENV: &str = "HOME";
    std::env::var_os(HOME_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn global_db_path_override() -> Option<PathBuf> {
    std::env::var_os(GLOBAL_DB_PATH_ENV)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// Path to the user-level global database.
///
/// Default is `global.db` inside [`user_data_dir`]. `TRACEDECAY_GLOBAL_DB`
/// pins an explicit path. This is the same formula `tracedecay-global-db`
/// previously owned; the helper lives here so handshake identity can resolve
/// the path without taking that crate as a dependency.
pub fn global_db_path() -> Option<PathBuf> {
    if let Some(path) = global_db_path_override() {
        return Some(path);
    }
    user_data_dir().map(|dir| dir.join(GLOBAL_DB_FILENAME))
}

/// True when `TRACEDECAY_GLOBAL_DB` pins the global DB to an explicit path.
pub fn global_db_path_is_overridden() -> bool {
    global_db_path_override().is_some()
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
        && profile_dir.parent().is_some_and(holds_cargo_profile_dir);
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
    crate::path_safety::canonicalize_path_or_existing_parent(&path)
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
        let at_worktree_root = worktree_root
            .as_ref()
            .is_some_and(|root| paths_same(&dir, root));
        let initialized = has_project_database(&dir)
            || crate::storage::has_path_local_profile_store(&dir)
            || (at_worktree_root && crate::storage::has_repository_identity_marker(&dir));
        if initialized && !is_ambient_project_root(&dir) {
            return Some(dir);
        }
        if at_worktree_root {
            return None;
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Returns whether a path is too broad to be an implicit code-project root.
///
/// Filesystem roots and the current user profile commonly contain many
/// repositories. Treating either as an implicit project can turn MCP startup
/// freshness work into a full-machine or full-home traversal.
pub fn is_ambient_project_root(path: &Path) -> bool {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical.parent().is_none()
        || ["HOME", "USERPROFILE"]
            .iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .map(|home| std::fs::canonicalize(&home).unwrap_or(home))
            .any(|home| home == canonical)
}
fn paths_same(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

pub use tracedecay_domain::source_path_policy::{GENERATED_DIR_SEGMENTS, is_generated_dir_segment};

// Deliberately unconditional (not gated behind `cfg(test)` /
// `feature = "test-helpers"`): some call sites reach it from a non-test build
// — e.g. the root crate's `session_temporal_benchmark`, which backs
// `cargo bench` and compiles as an optimized bench profile, not under
// `cfg(test)`. The mutex and accessor are trivial and side-effect free, so
// keeping them unconditional costs nothing while guaranteeing every consumer,
// test or not, serializes on the same lock.
static USER_DATA_DIR_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Serializes tests (and benchmark harnesses) that mutate process-wide
/// profile discovery variables.
pub fn lock_user_data_dir_test_env() -> std::sync::MutexGuard<'static, ()> {
    USER_DATA_DIR_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Pins profile discovery to an isolated directory for the guard's lifetime.
#[cfg(any(test, feature = "test-helpers"))]
pub struct PinnedUserDataDir {
    _lock: std::sync::MutexGuard<'static, ()>,
    _root: tempfile::TempDir,
    previous: Option<OsString>,
    previous_home: Option<OsString>,
    previous_userprofile: Option<OsString>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl PinnedUserDataDir {
    pub fn new() -> Self {
        let lock = lock_user_data_dir_test_env();
        let root = tempfile::TempDir::new()
            .unwrap_or_else(|error| panic!("failed to create temp profile dir: {error}"));
        let profile = root.path().join(TRACEDECAY_DIR);
        crate::storage::PrivateStoreIo::create_dir_all(&profile)
            .unwrap_or_else(|error| panic!("failed to create isolated profile root: {error}"));
        let previous = std::env::var_os(USER_DATA_DIR_ENV);
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        unsafe {
            std::env::set_var(USER_DATA_DIR_ENV, &profile);
            std::env::set_var("HOME", root.path());
            std::env::set_var("USERPROFILE", root.path());
        }
        Self {
            _lock: lock,
            _root: root,
            previous,
            previous_home,
            previous_userprofile,
        }
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl Default for PinnedUserDataDir {
    fn default() -> Self {
        Self::new()
    }
}

/// The search path for host and service program resolution (`kiro-cli`,
/// `gemini`, `systemctl`, env-shebang interpreters, ...).
///
/// Production reads the ambient `PATH` at each call. Tests substitute a
/// fixture directory through [`HostProgramSearchPathGuard`] instead of
/// mutating the process environment: a narrowed process-global `PATH` is
/// visible to every concurrently running test, so unrelated `sh`/`git` spawns
/// fail with `NotFound` for the guard's lifetime.
///
/// [`crate::git::try_git_program`] deliberately does not consult this seam:
/// the Git authority is process-wide and must never observe a test fixture.
pub fn host_program_search_path() -> Option<OsString> {
    #[cfg(any(test, feature = "test-helpers"))]
    if let Some(path) = HOST_PROGRAM_SEARCH_PATH_OVERRIDE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return Some(path);
    }
    std::env::var_os("PATH")
}

#[cfg(any(test, feature = "test-helpers"))]
static HOST_PROGRAM_SEARCH_PATH_OVERRIDE: std::sync::RwLock<Option<OsString>> =
    std::sync::RwLock::new(None);
#[cfg(any(test, feature = "test-helpers"))]
static HOST_PROGRAM_SEARCH_PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Substitutes [`host_program_search_path`] for the guard's lifetime without
/// touching the process `PATH`.
///
/// Guards serialize on their own lock, acquired after
/// [`lock_user_data_dir_test_env`] whenever a test holds both (never the
/// reverse), so sibling tests that spawn `sh`, `git`, or the product binary
/// keep seeing the ambient environment.
#[cfg(any(test, feature = "test-helpers"))]
pub struct HostProgramSearchPathGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl HostProgramSearchPathGuard {
    pub fn set(path: impl AsRef<std::ffi::OsStr>) -> Self {
        let lock = HOST_PROGRAM_SEARCH_PATH_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *HOST_PROGRAM_SEARCH_PATH_OVERRIDE
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(path.as_ref().to_os_string());
        Self { _lock: lock }
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl Drop for HostProgramSearchPathGuard {
    fn drop(&mut self) {
        *HOST_PROGRAM_SEARCH_PATH_OVERRIDE
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl Drop for PinnedUserDataDir {
    fn drop(&mut self) {
        unsafe {
            match self.previous.take() {
                Some(previous) => std::env::set_var(USER_DATA_DIR_ENV, previous),
                None => std::env::remove_var(USER_DATA_DIR_ENV),
            }
            match self.previous_home.take() {
                Some(previous) => std::env::set_var("HOME", previous),
                None => std::env::remove_var("HOME"),
            }
            match self.previous_userprofile.take() {
                Some(previous) => std::env::set_var("USERPROFILE", previous),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
    }
}

#[cfg(test)]
mod host_program_search_path_tests {
    use super::*;

    #[test]
    fn fixture_search_path_leaves_process_path_untouched() {
        let ambient = std::env::var_os("PATH");
        let fixture = tempfile::tempdir().expect("fixture search directory");
        {
            let _guard = HostProgramSearchPathGuard::set(fixture.path());
            assert_eq!(
                host_program_search_path().as_deref(),
                Some(fixture.path().as_os_str())
            );
            assert_eq!(std::env::var_os("PATH"), ambient);
        }
        assert_eq!(host_program_search_path(), ambient);
    }
}
