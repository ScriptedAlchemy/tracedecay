use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use glob::Pattern;
use serde::{Deserialize, Serialize};

use crate::errors::{Result, TraceDecayError};

/// Name of the configuration file stored inside the data directory.
pub const CONFIG_FILENAME: &str = "config.json";

/// Name of the hidden directory used to store `TraceDecay` metadata.
pub const TRACEDECAY_DIR: &str = ".tracedecay";

/// Environment variable that pins the user-level `TraceDecay` data directory.
pub const USER_DATA_DIR_ENV: &str = "TRACEDECAY_DATA_DIR";

/// Project graph database filename inside a `.tracedecay/` data dir.
pub const DB_FILENAME: &str = "tracedecay.db";

const DEFAULT_EXCLUDE_PATTERNS: &[&str] = &[
    "target/**",
    ".git/**",
    ".tracedecay/**",
    "**/node_modules/**",
    "vendor/**",
    "**/vendor/**",
    "**/*.min.*",
    "bin/**",
    "build/**",
    "**/build/**",
    "dist/**",
    "**/dist/**",
    "out/**",
    "**/out/**",
    "coverage/**",
    "**/coverage/**",
    ".cache/**",
    "**/.cache/**",
    ".next/**",
    "**/.next/**",
    ".turbo/**",
    "**/.turbo/**",
    ".gradle/**",
    "**/.gradle/**",
    ".venv/**",
    "**/.venv/**",
    "venv/**",
    "**/venv/**",
    "**/__pycache__/**",
];

/// Configuration for a `TraceDecay` project.
///
/// Controls which files are indexed, size limits, and feature toggles.
/// Language inclusion is derived automatically from the installed
/// `LanguageExtractor` set — only exclude patterns live in the config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceDecayConfig {
    /// Schema version of the configuration.
    pub version: u32,
    /// Root directory of the project being indexed.
    pub root_dir: String,
    /// Glob patterns for files to exclude during indexing.
    pub exclude: Vec<String>,
    /// Glob patterns for paths to include despite the default hidden-directory,
    /// generated-directory, and gitignore filters. For example,
    /// `[".github/**"]` indexes files under `.github/` that would otherwise be
    /// skipped.
    #[serde(default)]
    pub include: Vec<String>,
    /// Maximum file size in bytes; files larger than this are skipped.
    pub max_file_size: u64,
    /// Whether to extract doc comments from source files.
    pub extract_docstrings: bool,
    /// Whether to track call-site locations for edges.
    pub track_call_sites: bool,
    /// Whether to respect `.gitignore` rules when scanning files.
    #[serde(default = "default_git_ignore")]
    pub git_ignore: bool,
    /// Whether a cold `tracedecay_diagnostics` call prewarms in the background
    /// (detached dependency build + immediate `warming` status) instead of
    /// blocking for minutes. `TRACEDECAY_DIAGNOSTICS_PREWARM` overrides when it
    /// parses as a bool (env wins). Off by default.
    #[serde(default)]
    pub diagnostics_prewarm: bool,
    /// Index-freshness auto-sync settings (git-metadata watcher, serve-stale,
    /// branch lifecycle). Absent in older `config.json` files, so defaulted.
    #[serde(default)]
    pub sync: SyncConfig,
}

fn default_git_ignore() -> bool {
    true
}

fn default_sync_auto_watch() -> bool {
    true
}
fn default_sync_watch_debounce_ms() -> u64 {
    2000
}
fn default_sync_watch_max_delay_ms() -> u64 {
    30000
}
fn default_sync_watch_max_projects() -> usize {
    32
}
fn default_sync_read_refresh() -> bool {
    true
}
fn default_sync_read_cooldown_secs() -> u64 {
    30
}
fn default_sync_session_start_sync() -> bool {
    true
}
fn default_sync_session_start_stale_threshold_secs() -> u64 {
    600
}
fn default_sync_backstop_interval_mins() -> u64 {
    15
}
fn default_sync_full_sync_escalation_files() -> usize {
    500
}
fn default_sync_max_concurrent_syncs() -> usize {
    2
}
fn default_sync_branch_gc_days() -> u64 {
    14
}
fn default_sync_orphan_db_gc_days() -> u64 {
    7
}
fn default_sync_auto_init() -> bool {
    false
}

/// Auto-sync / index-freshness knobs, exposed as the `[sync]` table in
/// `config.json` and overridable via `TRACEDECAY_SYNC_*` environment
/// variables (see [`SyncConfig::with_env_overrides`]).
///
/// Every field carries a `#[serde(default = ...)]` so that a partial JSON
/// object (only some keys present) still deserializes, and a missing `sync`
/// key entirely falls back to [`SyncConfig::default`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncConfig {
    /// Enable the daemon git-metadata watcher.
    #[serde(default = "default_sync_auto_watch")]
    pub auto_watch: bool,
    /// Per-project quiet-period debounce before a watcher-triggered sync (ms).
    #[serde(default = "default_sync_watch_debounce_ms")]
    pub watch_debounce_ms: u64,
    /// Maximum time a watcher-triggered sync can be deferred by debounce (ms).
    #[serde(default = "default_sync_watch_max_delay_ms")]
    pub watch_max_delay_ms: u64,
    /// Maximum number of recently-seen projects the watcher registers.
    #[serde(default = "default_sync_watch_max_projects")]
    pub watch_max_projects: usize,
    /// Enable non-blocking sync-on-read for query tools.
    #[serde(default = "default_sync_read_refresh")]
    pub read_refresh: bool,
    /// Cooldown between read-triggered background refreshes (seconds).
    #[serde(default = "default_sync_read_cooldown_secs")]
    pub read_cooldown_secs: u64,
    /// Fire a catch-up sync on session start.
    #[serde(default = "default_sync_session_start_sync")]
    pub session_start_sync: bool,
    /// Staleness threshold above which session-start sync runs (seconds).
    #[serde(default = "default_sync_session_start_stale_threshold_secs")]
    pub session_start_stale_threshold_secs: u64,
    /// Daemon backstop scheduler interval (minutes); 0 disables it.
    #[serde(default = "default_sync_backstop_interval_mins")]
    pub backstop_interval_mins: u64,
    /// Diff-scoped syncs above this many changed files escalate to a full sync.
    #[serde(default = "default_sync_full_sync_escalation_files")]
    pub full_sync_escalation_files: usize,
    /// Daemon-wide cap on concurrent syncs.
    #[serde(default = "default_sync_max_concurrent_syncs")]
    pub max_concurrent_syncs: usize,
    /// Grace period before a dead tracked-branch store is GC'd (days).
    #[serde(default = "default_sync_branch_gc_days")]
    pub branch_gc_days: u64,
    /// Grace period before an orphan branch DB is GC'd (days).
    #[serde(default = "default_sync_orphan_db_gc_days")]
    pub orphan_db_gc_days: u64,
    /// Auto-initialise never-indexed repos on first contact.
    #[serde(default = "default_sync_auto_init")]
    pub auto_init: bool,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            auto_watch: default_sync_auto_watch(),
            watch_debounce_ms: default_sync_watch_debounce_ms(),
            watch_max_delay_ms: default_sync_watch_max_delay_ms(),
            watch_max_projects: default_sync_watch_max_projects(),
            read_refresh: default_sync_read_refresh(),
            read_cooldown_secs: default_sync_read_cooldown_secs(),
            session_start_sync: default_sync_session_start_sync(),
            session_start_stale_threshold_secs: default_sync_session_start_stale_threshold_secs(),
            backstop_interval_mins: default_sync_backstop_interval_mins(),
            full_sync_escalation_files: default_sync_full_sync_escalation_files(),
            max_concurrent_syncs: default_sync_max_concurrent_syncs(),
            branch_gc_days: default_sync_branch_gc_days(),
            orphan_db_gc_days: default_sync_orphan_db_gc_days(),
            auto_init: default_sync_auto_init(),
        }
    }
}

/// Parses a boolean env value: `1`/`true` => true, `0`/`false` => false
/// (case-insensitive). Any other value is ignored (returns `None`).
fn parse_env_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

/// Reads a `TRACEDECAY_<suffix>` env var and parses it as a bool.
pub(crate) fn env_bool(suffix: &str) -> Option<bool> {
    brand_env(suffix).as_deref().and_then(parse_env_bool)
}

/// Reads a `TRACEDECAY_<suffix>` env var and parses it as an integer of the
/// caller's choosing.
fn env_parse<T: std::str::FromStr>(suffix: &str) -> Option<T> {
    brand_env(suffix)
        .as_deref()
        .and_then(|raw| raw.trim().parse::<T>().ok())
}

impl SyncConfig {
    /// Applies `TRACEDECAY_SYNC_*` environment overrides on top of `self`,
    /// leaving any field whose env var is unset or unparsable untouched.
    #[must_use]
    pub fn with_env_overrides(mut self) -> Self {
        if let Some(value) = env_bool("SYNC_AUTO_WATCH") {
            self.auto_watch = value;
        }
        if let Some(value) = env_parse("SYNC_WATCH_DEBOUNCE_MS") {
            self.watch_debounce_ms = value;
        }
        if let Some(value) = env_parse("SYNC_WATCH_MAX_DELAY_MS") {
            self.watch_max_delay_ms = value;
        }
        if let Some(value) = env_parse("SYNC_WATCH_MAX_PROJECTS") {
            self.watch_max_projects = value;
        }
        if let Some(value) = env_bool("SYNC_READ_REFRESH") {
            self.read_refresh = value;
        }
        if let Some(value) = env_parse("SYNC_READ_COOLDOWN_SECS") {
            self.read_cooldown_secs = value;
        }
        if let Some(value) = env_bool("SYNC_SESSION_START_SYNC") {
            self.session_start_sync = value;
        }
        if let Some(value) = env_parse("SYNC_SESSION_START_STALE_THRESHOLD_SECS") {
            self.session_start_stale_threshold_secs = value;
        }
        if let Some(value) = env_parse("SYNC_BACKSTOP_INTERVAL_MINS") {
            self.backstop_interval_mins = value;
        }
        if let Some(value) = env_parse("SYNC_FULL_SYNC_ESCALATION_FILES") {
            self.full_sync_escalation_files = value;
        }
        if let Some(value) = env_parse("SYNC_MAX_CONCURRENT_SYNCS") {
            self.max_concurrent_syncs = value;
        }
        if let Some(value) = env_parse("SYNC_BRANCH_GC_DAYS") {
            self.branch_gc_days = value;
        }
        if let Some(value) = env_parse("SYNC_ORPHAN_DB_GC_DAYS") {
            self.orphan_db_gc_days = value;
        }
        if let Some(value) = env_bool("SYNC_AUTO_INIT") {
            self.auto_init = value;
        }
        self
    }
}

/// Loads the `[sync]` config for a project (falling back to defaults on any
/// load error) and applies `TRACEDECAY_SYNC_*` environment overrides.
pub fn load_sync_config(project_root: &Path) -> SyncConfig {
    load_config(project_root)
        .map(|config| config.sync)
        .unwrap_or_default()
        .with_env_overrides()
}

impl Default for TraceDecayConfig {
    fn default() -> Self {
        Self {
            version: 1,
            root_dir: String::new(),
            exclude: DEFAULT_EXCLUDE_PATTERNS
                .iter()
                .map(|pattern| (*pattern).to_string())
                .collect(),
            include: Vec::new(),
            max_file_size: 1_048_576,
            extract_docstrings: true,
            track_call_sites: true,
            git_ignore: default_git_ignore(),
            diagnostics_prewarm: false,
            sync: SyncConfig::default(),
        }
    }
}

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
/// Normal runtime graph storage resolves through `crate::storage::StoreLayout`
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
        return Some(canonicalize_data_dir(PathBuf::from(path)));
    }
    let home = dirs::home_dir()?;
    Some(canonicalize_data_dir(home.join(TRACEDECAY_DIR)))
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

/// Reads the `TRACEDECAY_<suffix>` environment variable.
pub fn brand_env(suffix: &str) -> Option<String> {
    std::env::var(format!("TRACEDECAY_{suffix}")).ok()
}

/// Returns the path to the configuration file (`config.json`) within the
/// resolved data directory.
pub fn get_config_path(project_root: &Path) -> PathBuf {
    if let Ok(layout) = crate::storage::resolve_layout_for_current_profile(project_root) {
        return layout.config_path;
    }
    get_tracedecay_dir(project_root).join(CONFIG_FILENAME)
}

/// Loads the configuration from disk.
///
/// If the configuration file does not exist, returns a default configuration
/// with `root_dir` set to the given project root.
pub fn load_config(project_root: &Path) -> Result<TraceDecayConfig> {
    let config_path = get_config_path(project_root);
    load_config_from_path(project_root, &config_path)
}

/// Loads configuration from an explicit config path while preserving the
/// project root used for default config values.
pub fn load_config_from_path(project_root: &Path, config_path: &Path) -> Result<TraceDecayConfig> {
    if !config_path.exists() {
        return Ok(TraceDecayConfig {
            root_dir: project_root.to_string_lossy().to_string(),
            ..TraceDecayConfig::default()
        });
    }

    let contents = fs::read_to_string(config_path).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to read config file '{}': {}",
            config_path.display(),
            e
        ),
    })?;

    let config: TraceDecayConfig =
        serde_json::from_str(&contents).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to parse config file '{}': {}",
                config_path.display(),
                e
            ),
        })?;

    Ok(config)
}

/// Saves the configuration to disk using an atomic write.
///
/// Writes to a temporary file first and then renames it to the final location,
/// ensuring that a partial write never corrupts the configuration.
pub fn save_config(project_root: &Path, config: &TraceDecayConfig) -> Result<()> {
    let config_path = get_config_path(project_root);
    save_config_to_path(&config_path, config)
}

pub fn save_config_to_path(config_path: &Path, config: &TraceDecayConfig) -> Result<()> {
    let data_dir = config_path
        .parent()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "configuration path '{}' has no parent directory",
                config_path.display()
            ),
        })?;
    fs::create_dir_all(data_dir).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to create tracedecay directory '{}': {}",
            data_dir.display(),
            e
        ),
    })?;

    let tmp_path = config_path.with_extension("tmp");

    let json = serde_json::to_string_pretty(config).map_err(|e| TraceDecayError::Config {
        message: format!("failed to serialize config: {e}"),
    })?;

    fs::write(&tmp_path, &json).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to write temporary config file '{}': {}",
            tmp_path.display(),
            e
        ),
    })?;

    fs::rename(&tmp_path, config_path).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to rename temporary config file '{}' to '{}': {}",
            tmp_path.display(),
            config_path.display(),
            e
        ),
    })?;

    Ok(())
}

/// Returns `true` if the project marker dir (`.tracedecay`) is ignored by Git
/// for this project.
///
/// This respects the repository `.gitignore`, `.git/info/exclude`, and the
/// user's global excludes file via `git check-ignore`. If Git cannot answer
/// (for example outside a Git repository), falls back to checking the local
/// `.gitignore` file only.
pub fn is_in_gitignore(project_path: &Path) -> bool {
    if let Some(is_ignored) = is_ignored_by_git(project_path, None) {
        return is_ignored;
    }

    is_in_local_gitignore(project_path)
}

fn is_ignored_by_git(project_path: &Path, git_config_global: Option<&Path>) -> Option<bool> {
    let fallback_global_excludes = || {
        git_config_global
            .and_then(|path| is_ignored_by_explicit_global_excludes(project_path, path))
    };
    let dir_name = active_data_dir_name(project_path);
    let mut command = Command::new(crate::git::git_program());
    command
        .arg("-C")
        .arg(project_path)
        .arg("check-ignore")
        .arg("-q")
        .arg(format!("{dir_name}/"))
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    if let Some(path) = git_config_global {
        command.env_clear();
        command.env("PATH", git_subprocess_path());
        command.env("GIT_CONFIG_GLOBAL", path);
        command.env("GIT_CONFIG_NOSYSTEM", "1");
    }

    let Ok(status) = command.status() else {
        return fallback_global_excludes();
    };

    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => fallback_global_excludes(),
    }
}

fn is_ignored_by_explicit_global_excludes(
    project_path: &Path,
    git_config_global: &Path,
) -> Option<bool> {
    let config = fs::read_to_string(git_config_global).ok()?;
    let excludes_file = config.lines().find_map(|line| {
        let trimmed = line.trim();
        let (key, value) = trimmed.split_once('=')?;
        (key.trim() == "excludesFile").then(|| PathBuf::from(value.trim()))
    })?;
    let excludes = fs::read_to_string(excludes_file).ok()?;
    let dir_name = active_data_dir_name(project_path);
    let dir_pattern = format!("{dir_name}/");
    Some(excludes.lines().any(|line| {
        let trimmed = line.trim();
        !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && (trimmed == dir_name || trimmed == dir_pattern)
    }))
}

#[cfg(test)]
fn git_subprocess_path() -> OsString {
    std::env::var_os("PATH").unwrap_or_else(|| {
        #[cfg(windows)]
        {
            OsString::new()
        }
        #[cfg(not(windows))]
        {
            OsString::from("/usr/bin:/bin")
        }
    })
}

#[cfg(not(test))]
fn git_subprocess_path() -> OsString {
    std::env::var_os("PATH").unwrap_or_default()
}

fn is_in_local_gitignore(project_path: &Path) -> bool {
    let dir_name = active_data_dir_name(project_path);
    let gitignore = project_path.join(".gitignore");
    match fs::read_to_string(&gitignore) {
        Ok(content) => content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed == dir_name
                || trimmed == format!("{dir_name}/")
                || trimmed == format!("/{dir_name}")
        }),
        Err(_) => false,
    }
}

/// Appends the project marker dir name (`.tracedecay`) to the project's
/// `.gitignore`, creating the file if needed. Ensures the entry starts on its
/// own line (adds a trailing newline to existing content if missing).
pub fn add_to_gitignore(project_path: &Path) {
    let dir_name = active_data_dir_name(project_path);
    let gitignore = project_path.join(".gitignore");
    let mut content = fs::read_to_string(&gitignore).unwrap_or_default();
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(dir_name);
    content.push('\n');
    if let Err(e) = fs::write(&gitignore, content) {
        eprintln!("warning: failed to update .gitignore: {e}");
    }
}

/// Resolves a CLI path argument to an absolute `PathBuf`.
///
/// If `path` is `Some`, uses that value; otherwise falls back to the current
/// working directory.
pub fn resolve_path(path: Option<String>) -> PathBuf {
    let path = match path {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    absolutize_path(path)
}

fn absolutize_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

/// Walks from `start` upward looking for an initialised repo marker
/// (`.tracedecay/tracedecay.db`) or a profile-storage enrollment marker
/// (`.tracedecay/enrollment.json`).
///
/// Returns the first ancestor directory (inclusive) that contains an
/// initialised `TraceDecay` project, or `None` if the filesystem root is
/// reached without finding one.
///
/// # Canonical project-root resolution order
///
/// This walk-up is the heart of project-root resolution. Every entry point
/// that needs a project root should resolve it in this order — new code must
/// converge on this chain instead of inventing its own:
///
/// 0. **Template pre-filter** (`serve` only,
///    `serve::sanitize_serve_path_arg`): an explicit path that is a literal
///    unexpanded `${...}` host template variable (e.g. `${workspaceFolder}`
///    from a host that failed to expand it) is discarded with a warning and
///    resolution continues as if no path was given — except that step 4 then
///    requires a unique registered project
///    (`serve::ServeGlobalDbMatch::UniqueOnly`), because the host's spawn
///    directory says nothing about the intended workspace.
/// 1. **Explicit path** (`--path`/`-p`, tool `path` argument): used verbatim,
///    no discovery, and failure to open is fatal — never silently fall back.
/// 2. **CWD walk-up** (this function via [`resolve_path_with_discovery`]):
///    nearest ancestor of the working directory containing an initialised
///    project database (see [`get_project_db_path`]).
/// 3. **MCP `initialize` roots** (`serve` only,
///    `serve::ServeProjectResolver`): each workspace root the editor
///    advertises is tried verbatim against registered projects, then walked
///    up via this function.
/// 4. **Global DB registry** (`serve` only,
///    `serve::resolve_serve_from_global_db`): a single registered project
///    wins outright; among several, the deepest registered ancestor of cwd
///    wins, then the shallowest registered descendant; ties are reported as
///    ambiguous and require an explicit path.
pub fn discover_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    let worktree_root = crate::worktree::git_worktree_root(start);
    loop {
        if has_project_database(&dir)
            || crate::storage::has_enrollment_marker(&dir)
            || crate::storage::resolve_layout_for_current_profile(&dir).is_ok_and(|layout| {
                layout.storage_mode == crate::storage::StorageMode::ProfileSharded
                    && layout.graph_db_path.exists()
            })
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

/// Like [`resolve_path`], but when `path` is `None` it walks up from `cwd`
/// to find the nearest initialised `TraceDecay` project before falling back to
/// `cwd` itself.
///
/// Used by `serve`, `sync`, and `status`. NOT used by `init` (which must
/// create a fresh project at the target directory).
pub fn resolve_path_with_discovery(path: Option<String>) -> PathBuf {
    if let Some(p) = path {
        PathBuf::from(p)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        discover_project_root(&cwd)
            .or_else(|| crate::worktree::git_worktree_root(&cwd))
            .unwrap_or(cwd)
    }
}

fn paths_same(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

/// Returns `true` if the path matches any of the configured `include` patterns.
///
/// This is used to allow hidden (dot-prefixed) directories that would
/// otherwise be skipped by the file walker.
pub fn is_included(path: &str, config: &TraceDecayConfig) -> bool {
    let match_opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };

    for pattern_str in &config.include {
        if let Ok(pattern) = Pattern::new(pattern_str) {
            if pattern.matches_with(path, match_opts) {
                return true;
            }
        }
    }

    false
}

/// Returns `true` if a directory should be entered because it or one of its
/// descendants matches an explicit include glob.
pub fn is_included_dir(dir_path: &str, config: &TraceDecayConfig) -> bool {
    let match_opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };

    for pattern_str in &config.include {
        if let Ok(pattern) = Pattern::new(pattern_str) {
            if pattern.matches_with(dir_path, match_opts)
                || pattern.matches_with(&format!("{dir_path}/_"), match_opts)
            {
                return true;
            }
        }
    }

    false
}

/// Returns `true` if a directory should be pruned during scanning.
///
/// Matches `dir/_` against exclude patterns (for `dir/**`-style globs) and
/// also matches `dir` itself (for bare `**/dirname`-style globs).  This
/// ensures that patterns like `**/node_modules` and `**/node_modules/**`
/// both trigger directory pruning in `scan_files_walkdir`.
pub fn is_excluded_dir(dir_path: &str, config: &TraceDecayConfig) -> bool {
    let match_opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };

    for pattern_str in &config.exclude {
        if let Ok(pattern) = Pattern::new(pattern_str) {
            // Try both the dummy-file probe (catches dir/**) and the bare
            // directory path (catches **/dirname).
            if pattern.matches_with(&format!("{dir_path}/_"), match_opts)
                || pattern.matches_with(dir_path, match_opts)
            {
                return true;
            }
        }
    }

    false
}

/// Returns `true` if the file matches any of the configured exclude patterns.
pub fn is_excluded(file_path: &str, config: &TraceDecayConfig) -> bool {
    let match_opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };

    for pattern_str in &config.exclude {
        if let Ok(pattern) = Pattern::new(pattern_str) {
            if pattern.matches_with(file_path, match_opts) {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        db_filename, get_project_db_path, get_tracedecay_dir, is_excluded, is_excluded_dir,
        is_ignored_by_explicit_global_excludes, is_ignored_by_git, is_included, user_data_dir,
        TraceDecayConfig, USER_DATA_DIR_ENV,
    };
    use std::ffi::OsString;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    static USER_DATA_DIR_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvRestore {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvRestore {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(previous) => std::env::set_var(self.key, previous),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn test_data_dir_defaults_to_tracedecay_for_new_installs() {
        let root = TempDir::new().unwrap();
        assert_eq!(
            get_tracedecay_dir(root.path()),
            root.path().join(".tracedecay")
        );
        assert_eq!(
            get_project_db_path(root.path()),
            root.path().join(".tracedecay/tracedecay.db")
        );
    }

    #[test]
    fn test_data_dir_uses_tracedecay_when_present() {
        let root = TempDir::new().unwrap();
        fs::create_dir(root.path().join(".tracedecay")).unwrap();
        assert_eq!(
            get_tracedecay_dir(root.path()),
            root.path().join(".tracedecay")
        );
    }

    #[cfg(unix)]
    #[test]
    fn user_data_dir_canonicalizes_symlinked_existing_parent() {
        let _lock = USER_DATA_DIR_ENV_LOCK.lock().unwrap();
        let root = TempDir::new().unwrap();
        let real_home = root.path().join("real-home");
        let linked_home = root.path().join("linked-home");
        fs::create_dir_all(&real_home).unwrap();
        std::os::unix::fs::symlink(&real_home, &linked_home).unwrap();
        let _env = EnvRestore::set(USER_DATA_DIR_ENV, linked_home.join(".tracedecay"));

        assert_eq!(
            user_data_dir().unwrap(),
            real_home.canonicalize().unwrap().join(".tracedecay")
        );
    }

    #[test]
    fn test_db_filename_tracks_dir_brand() {
        assert_eq!(
            db_filename(std::path::Path::new("/p/.tracedecay")),
            "tracedecay.db"
        );
    }

    #[test]
    fn test_is_included_matches_glob() {
        let config = TraceDecayConfig {
            include: vec![".github/**".to_string()],
            ..TraceDecayConfig::default()
        };
        assert!(is_included(".github/workflows/ci.yml", &config));
        assert!(is_included(".github/scripts/build.sh", &config));
        assert!(!is_included(".vscode/settings.json", &config));
        assert!(!is_included("src/main.rs", &config));
    }

    #[test]
    fn test_is_included_empty_matches_nothing() {
        let config = TraceDecayConfig::default();
        assert!(!is_included(".github/workflows/ci.yml", &config));
    }

    #[test]
    fn test_include_records_explicit_override_even_when_excluded() {
        let config = TraceDecayConfig {
            include: vec![".config/**".to_string()],
            exclude: vec![".config/secret/**".to_string()],
            ..TraceDecayConfig::default()
        };
        assert!(is_included(".config/secret/key.rs", &config));
        assert!(is_excluded(".config/secret/key.rs", &config));
    }

    #[test]
    fn test_default_gitignore_is_enabled() {
        let config = TraceDecayConfig::default();
        assert!(config.git_ignore);
    }

    #[test]
    fn test_default_excludes_nested_node_modules() {
        let config = TraceDecayConfig::default();
        // Top-level node_modules — should be excluded
        assert!(is_excluded("node_modules/express/index.js", &config));
        // Nested node_modules inside a sub-project — must also be excluded
        assert!(is_excluded(
            "projectA/node_modules/express/index.js",
            &config
        ));
        assert!(is_excluded(
            "packages/web/node_modules/react/index.js",
            &config
        ));
        assert!(is_excluded("dist/main.js", &config));
        assert!(is_excluded("packages/web/dist/main.js", &config));
        assert!(is_excluded("coverage/lcov.js", &config));
        assert!(is_excluded("packages/web/.next/server/app.js", &config));
    }

    #[test]
    fn test_dir_pruning_pattern_matches_nested_dirs() {
        // scan_files_walkdir checks is_excluded("{dir}/_") for directory pruning.
        // Patterns like **/node_modules/** must match the dummy-file probe.
        let config = TraceDecayConfig::default();
        assert!(is_excluded("node_modules/_", &config));
        assert!(is_excluded("projectA/node_modules/_", &config));
    }

    #[test]
    fn test_is_excluded_dir_bare_pattern() {
        // Users may write "**/node_modules" (no trailing /**).
        // is_excluded_dir should match both bare and /**-suffixed patterns.
        let config = TraceDecayConfig {
            exclude: vec!["**/dist".to_string()],
            ..TraceDecayConfig::default()
        };
        assert!(is_excluded_dir("dist", &config));
        assert!(is_excluded_dir("packages/web/dist", &config));
        // Files inside dist should still be caught by accept_file's is_excluded
        // but dir pruning prevents even walking into the directory.
    }

    #[test]
    fn test_is_in_gitignore_respects_global_excludes_file() {
        let sandbox = TempDir::new().unwrap();
        let repo = sandbox.path().join("repo");
        fs::create_dir(&repo).unwrap();

        let mut init = Command::new("git");
        init.env_clear().env("PATH", super::git_subprocess_path());
        let init_status = init
            .arg("-C")
            .arg(&repo)
            .arg("init")
            .arg("-q")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(init_status.success(), "git init should succeed");

        let excludes = sandbox.path().join("global_ignore");
        fs::write(&excludes, ".tracedecay\n").unwrap();

        let git_config = sandbox.path().join("gitconfig");
        let excludes_value = excludes.to_string_lossy().replace('\\', "/");
        fs::write(
            &git_config,
            format!("[core]\n\texcludesFile = {excludes_value}\n"),
        )
        .unwrap();

        let ignored = is_ignored_by_git(&repo, Some(&git_config));

        assert_eq!(ignored, Some(true));
    }

    #[test]
    fn test_explicit_global_excludes_ignores_comments_and_blank_lines() {
        let sandbox = TempDir::new().unwrap();
        let repo = sandbox.path().join("repo");
        fs::create_dir(&repo).unwrap();

        let excludes = sandbox.path().join("global_ignore");
        fs::write(&excludes, "\n# comment\n.tracedecay/\n").unwrap();

        let git_config = sandbox.path().join("gitconfig");
        let excludes_value = excludes.to_string_lossy().replace('\\', "/");
        fs::write(
            &git_config,
            format!("[core]\n\texcludesFile = {excludes_value}\n"),
        )
        .unwrap();

        let ignored = is_ignored_by_explicit_global_excludes(&repo, &git_config);

        assert_eq!(ignored, Some(true));
    }

    #[test]
    fn sync_config_defaults_round_trip() {
        let config = TraceDecayConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: TraceDecayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.sync, parsed.sync);
        assert_eq!(parsed.sync, super::SyncConfig::default());
        // Spot-check a few of the documented defaults.
        assert!(parsed.sync.auto_watch);
        assert_eq!(parsed.sync.watch_debounce_ms, 2000);
        assert_eq!(parsed.sync.full_sync_escalation_files, 500);
        assert_eq!(parsed.sync.max_concurrent_syncs, 2);
        assert!(!parsed.sync.auto_init);
    }

    #[test]
    fn diagnostics_prewarm_round_trips_and_defaults_off() {
        let config = TraceDecayConfig::default();
        assert!(!config.diagnostics_prewarm, "prewarm must default off");
        let json = serde_json::to_string(&config).unwrap();
        let parsed: TraceDecayConfig = serde_json::from_str(&json).unwrap();
        assert!(!parsed.diagnostics_prewarm);

        // Explicit true round-trips, and old configs without the key default.
        let mut on = config.clone();
        on.diagnostics_prewarm = true;
        let parsed: TraceDecayConfig =
            serde_json::from_str(&serde_json::to_string(&on).unwrap()).unwrap();
        assert!(parsed.diagnostics_prewarm);
        let legacy = r#"{
            "version": 1,
            "root_dir": "/tmp/proj",
            "exclude": [],
            "max_file_size": 1048576,
            "extract_docstrings": true,
            "track_call_sites": true
        }"#;
        let parsed: TraceDecayConfig = serde_json::from_str(legacy).unwrap();
        assert!(!parsed.diagnostics_prewarm);
    }

    #[test]
    fn config_without_sync_key_deserializes_to_default_sync() {
        // Old config.json files predate the `sync` table; the field-level
        // `#[serde(default)]` must fill it in.
        let json = r#"{
            "version": 1,
            "root_dir": "/tmp/proj",
            "exclude": [],
            "max_file_size": 1048576,
            "extract_docstrings": true,
            "track_call_sites": true
        }"#;
        let parsed: TraceDecayConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.sync, super::SyncConfig::default());
    }

    #[test]
    fn partial_sync_table_fills_missing_fields_with_defaults() {
        // Only two sync keys present; every other field must default.
        let json = r#"{
            "version": 1,
            "root_dir": "/tmp/proj",
            "exclude": [],
            "max_file_size": 1048576,
            "extract_docstrings": true,
            "track_call_sites": true,
            "sync": { "auto_watch": false, "backstop_interval_mins": 99 }
        }"#;
        let parsed: TraceDecayConfig = serde_json::from_str(json).unwrap();
        assert!(!parsed.sync.auto_watch);
        assert_eq!(parsed.sync.backstop_interval_mins, 99);
        // Untouched fields keep their defaults.
        assert_eq!(parsed.sync.watch_debounce_ms, 2000);
        assert_eq!(parsed.sync.max_concurrent_syncs, 2);
        assert!(parsed.sync.read_refresh);
    }

    #[test]
    fn sync_config_env_overrides_bool_and_int() {
        let _lock = USER_DATA_DIR_ENV_LOCK.lock().unwrap();
        let _watch = EnvRestore::set("TRACEDECAY_SYNC_AUTO_WATCH", "false");
        let _debounce = EnvRestore::set("TRACEDECAY_SYNC_WATCH_DEBOUNCE_MS", "5000");
        // Unparsable ints/bools are ignored (field keeps its base value).
        let _bad = EnvRestore::set("TRACEDECAY_SYNC_MAX_CONCURRENT_SYNCS", "not-a-number");

        let overridden = super::SyncConfig::default().with_env_overrides();
        assert!(!overridden.auto_watch);
        assert_eq!(overridden.watch_debounce_ms, 5000);
        assert_eq!(
            overridden.max_concurrent_syncs,
            super::SyncConfig::default().max_concurrent_syncs
        );
    }
}
