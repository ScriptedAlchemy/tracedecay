//! Kernel-owned configuration primitives.

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

/// A root's `.tracedecay/` directory. Runtime storage lives in the profile
/// shard; a checkout's copy only holds the retired layout that
/// [`crate::storage::refuse_retired_checkout_layout`] refuses.
pub fn get_tracedecay_dir(project_root: &Path) -> PathBuf {
    project_root.join(TRACEDECAY_DIR)
}

/// Process home variable: `HOME` on Unix, `USERPROFILE` on Windows.
#[cfg(windows)]
pub const HOME_ENV: &str = "USERPROFILE";
/// Process home variable: `HOME` on Unix, `USERPROFILE` on Windows.
#[cfg(not(windows))]
pub const HOME_ENV: &str = "HOME";

/// Environment variable that relocates per-user configuration directories.
pub const XDG_CONFIG_HOME_ENV: &str = "XDG_CONFIG_HOME";

/// The user profile one owner serves: the `TraceDecay` data directory its
/// stores live under, the user home its agent hosts keep their state in, and
/// an optionally pinned global database.
///
/// A process resolves it once from its environment at its boundary (CLI
/// entry, daemon entry, hook entry) with [`ProfileRoot::from_env`] and passes
/// it down; nothing below that boundary reads `TRACEDECAY_DATA_DIR`, `HOME`,
/// or `XDG_CONFIG_HOME`, so two owners with distinct profiles can share one
/// process without seeing each other's stores.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProfileRoot {
    data_dir: PathBuf,
    home: Option<PathBuf>,
    xdg_config_home: Option<PathBuf>,
    global_db_override: Option<PathBuf>,
}

impl ProfileRoot {
    /// Resolves the profile from the process environment. Only process
    /// boundaries call this.
    ///
    /// `TRACEDECAY_DATA_DIR` names the data directory; otherwise it is
    /// `.tracedecay` under the home. `TRACEDECAY_GLOBAL_DB` pins the global
    /// database. A process with neither a data directory nor a home has no
    /// profile, which is a typed configuration error.
    pub fn from_env() -> tracedecay_domain::errors::Result<Self> {
        Self::from_vars(|name| std::env::var_os(name))
    }

    /// [`ProfileRoot::from_env`] with the data directory named explicitly, as
    /// a daemon launched with `--profile-root` has it. The home and overrides
    /// still come from the environment.
    pub fn from_env_with_data_dir(data_dir: impl Into<PathBuf>) -> Self {
        Self::from_vars_with_data_dir(
            |name| std::env::var_os(name),
            canonicalize_data_dir(data_dir.into()),
        )
    }

    /// [`ProfileRoot::from_env`] over an explicit variable lookup.
    pub fn from_vars(
        var: impl Fn(&str) -> Option<OsString>,
    ) -> tracedecay_domain::errors::Result<Self> {
        let data_dir = match var_path(&var, USER_DATA_DIR_ENV) {
            Some(path) => nextest_isolated_user_data_dir(&var, canonicalize_data_dir(path)),
            None => canonicalize_data_dir(
                var_path(&var, HOME_ENV)
                    .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                        message: format!(
                            "could not resolve user profile data directory: neither \
                             {USER_DATA_DIR_ENV} nor {HOME_ENV} is set"
                        ),
                    })?
                    .join(TRACEDECAY_DIR),
            ),
        };
        Ok(Self::from_vars_with_data_dir(var, data_dir))
    }

    fn from_vars_with_data_dir(var: impl Fn(&str) -> Option<OsString>, data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            home: var_path(&var, HOME_ENV),
            xdg_config_home: var_path(&var, XDG_CONFIG_HOME_ENV),
            global_db_override: var_path(&var, GLOBAL_DB_PATH_ENV),
        }
    }

    /// A profile whose data directory is `data_dir`, with no user home.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: canonicalize_data_dir(data_dir.into()),
            home: None,
            xdg_config_home: None,
            global_db_override: None,
        }
    }

    /// The default profile of a user whose home is `home`: data under
    /// `home/.tracedecay`, agent-host state under `home`.
    pub fn under_home(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self::new(home.join(TRACEDECAY_DIR)).with_home(home)
    }

    #[must_use]
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    #[must_use]
    pub fn with_xdg_config_home(mut self, xdg_config_home: impl Into<PathBuf>) -> Self {
        self.xdg_config_home = Some(xdg_config_home.into());
        self
    }

    #[must_use]
    pub fn with_global_db_override(mut self, path: impl Into<PathBuf>) -> Self {
        self.global_db_override = Some(path.into());
        self
    }

    /// The `TraceDecay` data directory every profile store lives under.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The user home agent hosts keep their state under, when known.
    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref()
    }

    /// The home, or a typed error naming the operation that needed it.
    pub fn require_home(&self, operation: &str) -> tracedecay_domain::errors::Result<&Path> {
        self.home()
            .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("{operation} needs the user home, but {HOME_ENV} is not set"),
            })
    }

    /// `XDG_CONFIG_HOME` as the boundary saw it, when set.
    pub fn xdg_config_home(&self) -> Option<&Path> {
        self.xdg_config_home.as_deref()
    }

    /// The per-user configuration directory: an absolute `XDG_CONFIG_HOME`,
    /// else `.config` under the home.
    pub fn config_home(&self) -> Option<PathBuf> {
        self.xdg_config_home
            .as_ref()
            .filter(|path| path.is_absolute())
            .cloned()
            .or_else(|| self.home.as_ref().map(|home| home.join(".config")))
    }

    /// The user-level global database: `global.db` in the data directory
    /// unless `TRACEDECAY_GLOBAL_DB` pinned an explicit path.
    pub fn global_db_path(&self) -> PathBuf {
        self.global_db_override
            .clone()
            .unwrap_or_else(|| self.data_dir.join(GLOBAL_DB_FILENAME))
    }

    /// True when the global database path was pinned explicitly.
    pub fn global_db_path_is_overridden(&self) -> bool {
        self.global_db_override.is_some()
    }

    /// Whether `path` is too broad to be an implicit code-project root: a
    /// filesystem root or this profile's user home.
    ///
    /// Filesystem roots and the user profile commonly contain many
    /// repositories. Treating either as an implicit project can turn MCP
    /// startup freshness work into a full-machine or full-home traversal.
    pub fn is_ambient_project_root(&self, path: &Path) -> bool {
        is_ambient_project_root(self.home(), path)
    }

    /// Walks up from `start` looking for the nearest ancestor that hosts a
    /// project initialised in this profile, or `None` if the filesystem root
    /// is reached without finding one.
    ///
    /// # Canonical local project-root resolution order
    ///
    /// This walk-up is the heart of project-root resolution. Every entry
    /// point that needs a project root should resolve it in this order. New
    /// code must converge on this chain instead of inventing its own:
    ///
    /// 0. **Template pre-filter** (`serve` only, `sanitize_serve_path_arg`):
    ///    an explicit path that is a literal unexpanded `${...}` host template
    ///    variable (e.g. `${workspaceFolder}` from a host that failed to
    ///    expand it) is discarded with a warning and resolution continues as
    ///    if no path was given.
    /// 1. **Explicit path** (`--path`/`-p`, tool `path` argument): used
    ///    verbatim, no discovery, and failure to open is fatal, never silently
    ///    fall back.
    /// 2. **CWD walk-up** (this function via `resolve_path_with_discovery`):
    ///    nearest ancestor of the working directory hosting a path-local
    ///    profile store or, at a worktree root, a repository identity marker.
    ///
    /// `serve` forwards this routing metadata to the managed daemon. MCP
    /// `initialize` roots and registry aliases are resolved there; the proxy
    /// never opens a project or global database and has no in-process
    /// fallback.
    pub fn discover_project_root(&self, start: &Path) -> Option<PathBuf> {
        let mut dir = start.to_path_buf();
        let worktree_root = crate::worktree::git_worktree_root(start);
        loop {
            let at_worktree_root = worktree_root
                .as_ref()
                .is_some_and(|root| paths_same(&dir, root));
            let initialized =
                directory_hosts_initialized_project(&self.data_dir, &dir, at_worktree_root);
            if initialized && !self.is_ambient_project_root(&dir) {
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

    /// Whether `dir` itself is a project root initialised in this profile,
    /// judged verbatim: no ancestor walk and no ambient-root filter. This is
    /// the check for an explicit path (rule 1 of
    /// [`ProfileRoot::discover_project_root`]), where the caller named the
    /// directory and only needs to know whether a project lives there.
    pub fn is_initialized_project_root(&self, dir: &Path) -> bool {
        let at_worktree_root =
            crate::worktree::git_worktree_root(dir).is_some_and(|root| paths_same(dir, &root));
        directory_hosts_initialized_project(&self.data_dir, dir, at_worktree_root)
    }
}

fn var_path(var: &impl Fn(&str) -> Option<OsString>, name: &str) -> Option<PathBuf> {
    var(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn nextest_isolated_user_data_dir(
    var: &impl Fn(&str) -> Option<OsString>,
    path: PathBuf,
) -> PathBuf {
    use std::hash::{Hash, Hasher};

    let Some(test_name) = var("NEXTEST_TEST_NAME").filter(|name| !name.is_empty()) else {
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
    let ci_profile = profile_name == Some("tracedecay-test-profile") && var("CI").is_some();
    if !target_profile && !ci_profile {
        return path;
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    var("NEXTEST_RUN_ID")
        .unwrap_or_default()
        .to_string_lossy()
        .hash(&mut hasher);
    var("NEXTEST_ATTEMPT_ID")
        .unwrap_or_default()
        .to_string_lossy()
        .hash(&mut hasher);
    var("NEXTEST_BINARY_ID")
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

/// [`ProfileRoot::is_ambient_project_root`] for an owner that holds only the
/// user home.
pub fn is_ambient_project_root(home: Option<&Path>, path: &Path) -> bool {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical.parent().is_none()
        || home
            .map(|home| std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf()))
            .is_some_and(|home| home == canonical)
}

fn directory_hosts_initialized_project(
    profile_root: &Path,
    dir: &Path,
    at_worktree_root: bool,
) -> bool {
    crate::storage::has_path_local_profile_store(profile_root, dir)
        || (at_worktree_root && crate::storage::has_repository_identity_marker(dir))
}

fn paths_same(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

pub use tracedecay_domain::source_path_policy::{GENERATED_DIR_SEGMENTS, is_generated_dir_segment};

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
/// Guards serialize on their own lock, so sibling tests that spawn `sh`,
/// `git`, or the product binary keep seeing the ambient environment.
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

#[cfg(test)]
mod profile_root_tests {
    use super::{
        DB_FILENAME, GLOBAL_DB_FILENAME, GLOBAL_DB_PATH_ENV, HOME_ENV, ProfileRoot, TRACEDECAY_DIR,
        USER_DATA_DIR_ENV,
    };
    use crate::storage::{path_local_profile_project_id, profile_sharded_data_root};

    fn enroll(profile: &ProfileRoot, project: &std::path::Path) {
        let store =
            profile_sharded_data_root(profile.data_dir(), &path_local_profile_project_id(project));
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join(DB_FILENAME), b"").unwrap();
    }

    /// Two owners with their own profiles share one process at once; each
    /// discovers the project only its own profile enrolled, and only its own
    /// home counts as an ambient root.
    #[test]
    fn two_profiles_in_one_process_discover_only_their_own_projects() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let project = root.join("project");
        std::fs::create_dir_all(project.join("src")).unwrap();
        let owners = [
            ProfileRoot::under_home(root.join("owner-a")),
            ProfileRoot::under_home(root.join("owner-b")),
        ];
        for owner in &owners {
            std::fs::create_dir_all(owner.home().unwrap()).unwrap();
        }
        enroll(&owners[0], &project);

        let discovered = std::thread::scope(|scope| {
            owners
                .each_ref()
                .map(|owner| {
                    let start = project.join("src");
                    scope.spawn(move || owner.discover_project_root(&start))
                })
                .map(|owner| owner.join().unwrap())
        });

        assert_eq!(discovered, [Some(project.clone()), None]);
        assert_eq!(
            owners[0].data_dir(),
            root.join("owner-a").join(TRACEDECAY_DIR)
        );
        assert!(owners[0].is_ambient_project_root(&root.join("owner-a")));
        assert!(!owners[1].is_ambient_project_root(&root.join("owner-a")));
    }

    fn vars(pairs: &[(&str, &std::path::Path)]) -> impl Fn(&str) -> Option<std::ffi::OsString> {
        let pairs = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.as_os_str().to_owned()))
            .collect::<std::collections::HashMap<_, _>>();
        move |name| pairs.get(name).cloned()
    }

    #[test]
    fn a_process_without_data_dir_or_home_has_no_profile() {
        let error = ProfileRoot::from_vars(|_| None).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "config error: could not resolve user profile data directory: neither \
                 {USER_DATA_DIR_ENV} nor {HOME_ENV} is set"
            )
        );
    }

    #[test]
    fn data_dir_defaults_under_home_and_an_override_wins() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("home");
        let default = ProfileRoot::from_vars(vars(&[(HOME_ENV, &home)])).unwrap();
        assert_eq!(default.data_dir(), home.join(TRACEDECAY_DIR));
        assert_eq!(default.home(), Some(home.as_path()));

        let pinned = root.join("pinned");
        let overridden = ProfileRoot::from_vars(vars(&[
            (HOME_ENV, &home),
            (USER_DATA_DIR_ENV, &pinned),
            (GLOBAL_DB_PATH_ENV, &root.join("global.db")),
        ]))
        .unwrap();
        assert_eq!(overridden.data_dir(), pinned);
        assert_eq!(overridden.home(), Some(home.as_path()));
        assert_eq!(overridden.global_db_path(), root.join("global.db"));
    }

    #[cfg(unix)]
    #[test]
    fn data_dir_canonicalizes_a_symlinked_existing_parent() {
        let temp = tempfile::tempdir().unwrap();
        let real_home = temp.path().join("real-home");
        let linked_home = temp.path().join("linked-home");
        std::fs::create_dir_all(&real_home).unwrap();
        std::os::unix::fs::symlink(&real_home, &linked_home).unwrap();
        let data_dir = linked_home.join(TRACEDECAY_DIR);

        let profile = ProfileRoot::from_vars(vars(&[(USER_DATA_DIR_ENV, &data_dir)])).unwrap();

        assert_eq!(
            profile.data_dir(),
            real_home.canonicalize().unwrap().join(TRACEDECAY_DIR)
        );
    }

    #[test]
    fn nextest_shared_target_profiles_are_isolated_by_test_name() {
        for cargo_profile in ["debug", "perf"] {
            let temp = tempfile::tempdir().unwrap();
            let target = temp.path().join("target");
            std::fs::create_dir_all(target.join(cargo_profile)).unwrap();
            let shared = target.join("test-profile").join(TRACEDECAY_DIR);
            let binary = std::path::Path::new("tracedecay::storage_suite");
            let test_name = std::path::Path::new("storage_suite::isolated_profile");

            let resolved = ProfileRoot::from_vars(vars(&[
                (USER_DATA_DIR_ENV, &shared),
                ("NEXTEST_BINARY_ID", binary),
                ("NEXTEST_TEST_NAME", test_name),
            ]))
            .unwrap();

            let canonical = target
                .canonicalize()
                .unwrap()
                .join("test-profile")
                .join(TRACEDECAY_DIR);
            assert!(
                resolved.data_dir().starts_with(canonical.join("nextest")),
                "{cargo_profile}: {}",
                resolved.data_dir().display()
            );
            assert_ne!(resolved.data_dir(), canonical);
        }
    }

    #[test]
    fn nextest_preserves_an_explicit_temp_profile_override() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("test-profile").join(TRACEDECAY_DIR);
        let test_name = std::path::Path::new("storage_suite::explicit_profile");

        let resolved = ProfileRoot::from_vars(vars(&[
            (USER_DATA_DIR_ENV, &profile),
            ("NEXTEST_TEST_NAME", test_name),
        ]))
        .unwrap();

        assert_eq!(
            resolved.data_dir(),
            temp.path()
                .canonicalize()
                .unwrap()
                .join("test-profile")
                .join(TRACEDECAY_DIR)
        );
    }

    #[test]
    fn implicit_discovery_never_selects_the_user_home() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let profile = ProfileRoot::under_home(&home);
        enroll(&profile, &home);
        let nested = home.join("unrelated/nested");
        std::fs::create_dir_all(&nested).unwrap();

        assert!(profile.is_ambient_project_root(&home));
        assert_eq!(profile.discover_project_root(&nested), None);
    }

    #[test]
    fn global_db_lives_in_the_data_dir_unless_pinned() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let profile = ProfileRoot::new(root.join("data"));
        assert_eq!(
            profile.global_db_path(),
            root.join("data").join(GLOBAL_DB_FILENAME)
        );
        assert!(!profile.global_db_path_is_overridden());

        let pinned = profile.with_global_db_override(root.join("elsewhere.db"));
        assert_eq!(pinned.global_db_path(), root.join("elsewhere.db"));
        assert!(pinned.global_db_path_is_overridden());
    }

    #[test]
    fn config_home_prefers_an_absolute_xdg_directory() {
        let home = ProfileRoot::under_home("/home/owner");
        assert_eq!(
            home.config_home(),
            Some(std::path::PathBuf::from("/home/owner/.config"))
        );
        assert_eq!(
            home.clone().with_xdg_config_home("relative").config_home(),
            Some(std::path::PathBuf::from("/home/owner/.config"))
        );
        assert_eq!(
            home.with_xdg_config_home("/xdg").config_home(),
            Some(std::path::PathBuf::from("/xdg"))
        );
        assert_eq!(ProfileRoot::new("/data").config_home(), None);
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
