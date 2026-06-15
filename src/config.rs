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

/// Legacy (pre-rebrand) data directory name. Projects that already have a
/// `.tokensave/` dir keep using it as-is — read and write, no auto-migration.
pub const LEGACY_TOKENSAVE_DIR: &str = ".tokensave";

/// Project graph database filename inside a `.tracedecay/` data dir.
pub const DB_FILENAME: &str = "tracedecay.db";

/// Project graph database filename inside a legacy `.tokensave/` data dir.
pub const LEGACY_DB_FILENAME: &str = "tokensave.db";

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
    /// Glob patterns for hidden (dot-prefixed) paths to include despite the
    /// default hidden-directory filter.  For example, `[".github/**"]` indexes
    /// files under `.github/` that would otherwise be skipped.
    #[serde(default)]
    pub include: Vec<String>,
    /// Maximum file size in bytes; files larger than this are skipped.
    pub max_file_size: u64,
    /// Whether to extract doc comments from source files.
    pub extract_docstrings: bool,
    /// Whether to track call-site locations for edges.
    pub track_call_sites: bool,
    /// Whether to respect `.gitignore` rules when scanning files.
    #[serde(default)]
    pub git_ignore: bool,
}

impl Default for TraceDecayConfig {
    fn default() -> Self {
        Self {
            version: 1,
            root_dir: String::new(),
            exclude: vec![
                "target/**".to_string(),
                ".git/**".to_string(),
                ".tracedecay/**".to_string(),
                // Legacy data-dir name — still excluded so pre-rebrand
                // projects never index their own store.
                ".tokensave/**".to_string(),
                "**/node_modules/**".to_string(),
                "vendor/**".to_string(),
                "**/*.min.*".to_string(),
                "bin/**".to_string(),
                "build/**".to_string(),
                "out/**".to_string(),
                ".gradle/**".to_string(),
            ],
            include: Vec::new(),
            max_file_size: 1_048_576,
            extract_docstrings: true,
            track_call_sites: true,
            git_ignore: false,
        }
    }
}

/// Returns the data directory for the given project root.
///
/// This is the single point of brand-dir resolution: prefer `.tracedecay/`;
/// when it does not exist but a legacy `.tokensave/` dir does, use the legacy
/// dir as-is (read and write — existing user data is never migrated or
/// renamed). New installs (neither dir present) get `.tracedecay/`.
pub fn get_tracedecay_dir(project_root: &Path) -> PathBuf {
    let primary = project_root.join(TRACEDECAY_DIR);
    if primary.is_dir() {
        return primary;
    }
    let legacy = project_root.join(LEGACY_TOKENSAVE_DIR);
    if legacy.is_dir() {
        return legacy;
    }
    primary
}

/// Name of the active data directory for this project root: `.tracedecay`,
/// or `.tokensave` when the project still uses a legacy dir.
pub fn active_data_dir_name(project_root: &Path) -> &'static str {
    if get_tracedecay_dir(project_root)
        .file_name()
        .is_some_and(|n| n == LEGACY_TOKENSAVE_DIR)
    {
        LEGACY_TOKENSAVE_DIR
    } else {
        TRACEDECAY_DIR
    }
}

/// Database filename appropriate for the given data directory: legacy
/// `.tokensave/` dirs keep `tokensave.db`, everything else uses
/// `tracedecay.db`.
pub fn db_filename(data_dir: &Path) -> &'static str {
    if data_dir
        .file_name()
        .is_some_and(|n| n == LEGACY_TOKENSAVE_DIR)
    {
        LEGACY_DB_FILENAME
    } else {
        DB_FILENAME
    }
}

/// Full path to the project graph database, brand-fallback aware:
/// `.tracedecay/tracedecay.db`, or `.tokensave/tokensave.db` for legacy
/// projects.
pub fn get_project_db_path(project_root: &Path) -> PathBuf {
    let dir = get_tracedecay_dir(project_root);
    let file = db_filename(&dir);
    dir.join(file)
}

/// User-level data directory: `~/.tracedecay`, falling back to an existing
/// legacy `~/.tokensave` (used as-is), defaulting to `~/.tracedecay` when
/// neither exists yet.
pub fn user_data_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let primary = home.join(TRACEDECAY_DIR);
    if primary.is_dir() {
        return Some(primary);
    }
    let legacy = home.join(LEGACY_TOKENSAVE_DIR);
    if legacy.is_dir() {
        return Some(legacy);
    }
    Some(primary)
}

/// Reads the `TRACEDECAY_<suffix>` environment variable, falling back to the
/// legacy `TOKENSAVE_<suffix>` name so pre-rebrand setups keep working.
pub fn brand_env(suffix: &str) -> Option<String> {
    std::env::var(format!("TRACEDECAY_{suffix}"))
        .or_else(|_| std::env::var(format!("TOKENSAVE_{suffix}")))
        .ok()
}

/// Returns the path to the configuration file (`config.json`) within the
/// resolved data directory.
pub fn get_config_path(project_root: &Path) -> PathBuf {
    get_tracedecay_dir(project_root).join(CONFIG_FILENAME)
}

/// Loads the configuration from disk.
///
/// If the configuration file does not exist, returns a default configuration
/// with `root_dir` set to the given project root.
pub fn load_config(project_root: &Path) -> Result<TraceDecayConfig> {
    let config_path = get_config_path(project_root);

    if !config_path.exists() {
        return Ok(TraceDecayConfig {
            root_dir: project_root.to_string_lossy().to_string(),
            ..TraceDecayConfig::default()
        });
    }

    let contents = fs::read_to_string(&config_path).map_err(|e| TraceDecayError::Config {
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
    let data_dir = get_tracedecay_dir(project_root);
    fs::create_dir_all(&data_dir).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to create tracedecay directory '{}': {}",
            data_dir.display(),
            e
        ),
    })?;

    let config_path = get_config_path(project_root);
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

    fs::rename(&tmp_path, &config_path).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to rename temporary config file '{}' to '{}': {}",
            tmp_path.display(),
            config_path.display(),
            e
        ),
    })?;

    Ok(())
}

/// Returns `true` if the active data dir (`.tracedecay`, or legacy
/// `.tokensave`) is ignored by Git for this project.
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
    let dir_name = active_data_dir_name(project_path);
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(project_path)
        .arg("check-ignore")
        .arg("-q")
        .arg(format!("{dir_name}/"))
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    if let Some(path) = git_config_global {
        command.env("GIT_CONFIG_GLOBAL", path);
    }

    let status = command.status().ok()?;

    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
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

/// Appends the active data-dir name (`.tracedecay`, or legacy `.tokensave`)
/// to the project's `.gitignore`, creating the file if needed. Ensures the
/// entry starts on its own line (adds a trailing newline to existing content
/// if missing).
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
    match path {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

/// Walks from `start` upward looking for an initialised project database
/// (`.tracedecay/tracedecay.db`, or legacy `.tokensave/tokensave.db`).
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
/// 1. **Explicit path** (`--path`/`-p`, tool `path` argument): used verbatim,
///    no discovery, and failure to open is fatal — never silently fall back.
/// 2. **CWD walk-up** (this function via [`resolve_path_with_discovery`]):
///    nearest ancestor of the working directory containing an initialised
///    project database (see [`get_project_db_path`]).
/// 3. **MCP `initialize` roots** (`serve` only,
///    `serve::resolve_serve_from_mcp_roots`): each workspace root the editor
///    advertises is tried verbatim against registered projects, then walked
///    up via this function.
/// 4. **Global DB registry** (`serve` only,
///    `serve::resolve_serve_from_global_db`): a single registered project
///    wins outright; among several, the deepest registered ancestor of cwd
///    wins, then the shallowest registered descendant; ties are reported as
///    ambiguous and require an explicit path.
pub fn discover_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if get_project_db_path(&dir).exists() {
            return Some(dir);
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
        discover_project_root(&cwd).unwrap_or(cwd)
    }
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
        is_ignored_by_git, is_included, TraceDecayConfig,
    };
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

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
    fn test_data_dir_falls_back_to_existing_legacy_dir() {
        let root = TempDir::new().unwrap();
        fs::create_dir(root.path().join(".tokensave")).unwrap();
        assert_eq!(
            get_tracedecay_dir(root.path()),
            root.path().join(".tokensave")
        );
        assert_eq!(
            get_project_db_path(root.path()),
            root.path().join(".tokensave/tokensave.db")
        );
    }

    #[test]
    fn test_data_dir_prefers_tracedecay_when_both_exist() {
        let root = TempDir::new().unwrap();
        fs::create_dir(root.path().join(".tokensave")).unwrap();
        fs::create_dir(root.path().join(".tracedecay")).unwrap();
        assert_eq!(
            get_tracedecay_dir(root.path()),
            root.path().join(".tracedecay")
        );
    }

    #[test]
    fn test_db_filename_tracks_dir_brand() {
        assert_eq!(
            db_filename(std::path::Path::new("/p/.tracedecay")),
            "tracedecay.db"
        );
        assert_eq!(
            db_filename(std::path::Path::new("/p/.tokensave")),
            "tokensave.db"
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
    fn test_include_does_not_override_exclude() {
        let config = TraceDecayConfig {
            include: vec![".config/**".to_string()],
            exclude: vec![".config/secret/**".to_string()],
            ..TraceDecayConfig::default()
        };
        // Included by include glob
        assert!(is_included(".config/secret/key.rs", &config));
        // But also matched by exclude glob
        assert!(is_excluded(".config/secret/key.rs", &config));
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

        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .arg("init")
            .arg("-q")
            .status()
            .unwrap();

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
}
