//! User-level configuration stored in the `TraceDecay` user data directory.
//!
//! All fields have defaults so a missing file or missing fields are handled
//! gracefully. Unknown fields are preserved for forward compatibility.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use tracedecay_automation::config::AutomationConfig;
use tracedecay_runtime_core::config::user_data_dir;
use tracedecay_runtime_core::storage::{
    acquire_sidecar_lock_blocking, append_lock_path, retry_transient_file_op,
};

/// User-level tracedecay configuration.
#[derive(Debug, Serialize, Deserialize)]
pub struct UserConfig {
    /// Whether to upload pending tokens to the optional worldwide counter.
    #[serde(default)]
    pub upload_enabled: bool,

    /// Tokens accumulated locally, not yet uploaded.
    #[serde(default)]
    pub pending_upload: u64,

    /// UNIX timestamp of last successful upload.
    #[serde(default)]
    pub last_upload_at: i64,

    /// Cached worldwide total from last fetch.
    #[serde(default)]
    pub last_worldwide_total: u64,

    /// UNIX timestamp of last worldwide total fetch.
    #[serde(default)]
    pub last_worldwide_fetch_at: i64,

    /// UNIX timestamp of last flush attempt (success or failure).
    #[serde(default)]
    pub last_flush_attempt_at: i64,

    /// Cached latest version from GitHub releases.
    #[serde(default)]
    pub cached_latest_version: String,

    /// UNIX timestamp of last version check.
    #[serde(default)]
    pub last_version_check_at: i64,

    /// UNIX timestamp of last version-update warning shown to the user.
    #[serde(default)]
    pub last_version_warning_at: i64,

    /// Agent integrations that have been installed (e.g. `["claude", "gemini"]`).
    #[serde(default)]
    pub installed_agents: Vec<String>,

    /// Debounce duration for the embedded MCP file watcher (e.g. "2s", "15s", "1m").
    #[serde(default = "default_watcher_debounce", alias = "daemon_debounce")]
    pub watcher_debounce: String,

    /// Cached country flags from the worldwide counter.
    #[serde(default)]
    pub cached_country_flags: Vec<String>,

    /// UNIX timestamp of last country flags fetch.
    #[serde(default)]
    pub last_flags_fetch_at: i64,

    /// UNIX timestamp of last `LiteLLM` pricing fetch.
    #[serde(default)]
    pub last_pricing_fetch_at: i64,

    /// Version that last ran `install` or `reinstall`. Used to trigger a
    /// silent reinstall when the binary is upgraded.
    #[serde(default)]
    pub last_installed_version: String,

    /// Version of the *previously running* tracedecay binary, recorded by
    /// `tracedecay upgrade` / `channel switch` just before the binary is
    /// replaced. The *new* binary reads this on startup and decides whether
    /// reinstall is required for the transition (patch-only bumps are
    /// no-ops; minor/major bumps re-register agents). Always updated to the
    /// running version after the decision is made.
    #[serde(default)]
    pub previous_version: String,

    /// Per-file extraction timeout in seconds. The worker is killed and
    /// the file is recorded in `SyncResult.skipped_paths` if a single
    /// file's extraction takes longer. Bounds the worst case from any
    /// pathological grammar / input combo.
    #[serde(default = "default_extraction_timeout_secs")]
    pub extraction_timeout_secs: u64,

    /// Global defaults for self-improvement automation. Project/profile
    /// dashboard sidecars may override these values.
    #[serde(default, skip_serializing_if = "AutomationConfig::is_default")]
    pub automation: AutomationConfig,

    /// Whether lifecycle hooks inject fact-store memory into agent context
    /// (session digests, prompt-gated recall, the Cursor memory rule).
    /// The `TRACEDECAY_MEMORY_INJECTION` env var overrides this at runtime.
    #[serde(default = "default_true")]
    pub memory_injection_enabled: bool,

    /// Unknown user config keys preserved for forward compatibility.
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

fn default_true() -> bool {
    true
}

fn default_watcher_debounce() -> String {
    "2s".to_string()
}

fn default_extraction_timeout_secs() -> u64 {
    60
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            upload_enabled: false,
            pending_upload: 0,
            last_upload_at: 0,
            last_worldwide_total: 0,
            last_worldwide_fetch_at: 0,
            last_flush_attempt_at: 0,
            cached_latest_version: String::new(),
            last_version_check_at: 0,
            last_version_warning_at: 0,
            installed_agents: Vec::new(),
            watcher_debounce: default_watcher_debounce(),
            cached_country_flags: Vec::new(),
            last_flags_fetch_at: 0,
            last_pricing_fetch_at: 0,
            last_installed_version: String::new(),
            previous_version: String::new(),
            extraction_timeout_secs: default_extraction_timeout_secs(),
            automation: AutomationConfig::default(),
            memory_injection_enabled: true,
            extra: BTreeMap::new(),
        }
    }
}

/// Returns the path to the user-level config file.
pub fn config_path() -> Option<PathBuf> {
    user_data_dir().map(|dir| dir.join("config.toml"))
}

/// Whether the user config explicitly contains an `[automation]` table.
/// Missing automation configuration is distinct from an explicit disabled
/// configuration for profile-level projectless self-improvement.
pub fn automation_is_configured() -> bool {
    let Some(path) = config_path() else {
        return false;
    };
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    toml::from_str::<toml::Value>(&contents)
        .ok()
        .and_then(|value| value.as_table().cloned())
        .is_some_and(|table| table.contains_key("automation"))
}

/// Errors returned by [`UserConfig::save`] / [`UserConfig::save_with_recovery`].
///
/// Distinguishes the ways a save can fail so callers can surface an actionable
/// message instead of a bare boolean. The corrupt-existing-file case carries
/// the path and the TOML parse error (whose message includes the line/column),
/// so a user can find and fix — or delete — the offending file.
#[derive(Debug)]
pub enum ConfigSaveError {
    /// The user data directory could not be resolved, so there is no path to
    /// write to.
    PathUnavailable,
    /// The existing config file is present but could not be read.
    ExistingUnreadable { path: PathBuf, source: io::Error },
    /// The existing config file is present but is not valid TOML. It is left
    /// untouched (never clobbered) unless recovery was requested.
    CorruptExisting {
        path: PathBuf,
        line: Option<usize>,
        message: String,
    },
    /// Serializing the in-memory config to TOML failed.
    Serialize { message: String },
    /// Creating the parent directory, writing the temp file, or renaming it
    /// over the target failed.
    Io {
        path: PathBuf,
        message: String,
        source: io::Error,
    },
    /// Acquiring the sidecar write lock failed.
    Lock { path: PathBuf, source: io::Error },
}

impl ConfigSaveError {
    /// True when the failure is a corrupt existing file that was left intact.
    #[must_use]
    pub fn is_corrupt(&self) -> bool {
        matches!(self, Self::CorruptExisting { .. })
    }
}

impl std::fmt::Display for ConfigSaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PathUnavailable => write!(
                f,
                "cannot resolve the tracedecay user config path (no user data directory)"
            ),
            Self::ExistingUnreadable { path, source } => {
                write!(
                    f,
                    "cannot read existing config file {}: {source}",
                    path.display()
                )
            }
            Self::CorruptExisting {
                path,
                line,
                message,
            } => match line {
                Some(line) => write!(
                    f,
                    "config file {} is corrupt at line {line}: {message} \
                     — back it up or delete it to regenerate",
                    path.display()
                ),
                None => write!(
                    f,
                    "config file {} is corrupt: {message} \
                     — back it up or delete it to regenerate",
                    path.display()
                ),
            },
            Self::Serialize { message } => {
                write!(f, "failed to serialize config to TOML: {message}")
            }
            Self::Io {
                path,
                message,
                source,
            } => write!(f, "{message} ({}): {source}", path.display()),
            Self::Lock { path, source } => {
                write!(
                    f,
                    "failed to acquire config write lock {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for ConfigSaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ExistingUnreadable { source, .. }
            | Self::Io { source, .. }
            | Self::Lock { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Sibling temp path in the same directory as `path`, used for the atomic
/// write-then-rename. Includes pid and a nanosecond stamp so a stale temp from
/// a crashed writer never collides with a live one.
fn temp_write_path(path: &Path) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_else(|| std::ffi::OsString::from("config.toml"));
    name.push(format!(".tmp-{pid}-{unique}"));
    path.with_file_name(name)
}

/// Quarantine path (`config.toml.corrupt-<unix-ts>`) for a corrupt config file
/// preserved during recovery. Mirrors the branch-meta quarantine naming in
/// `src/storage.rs` / `src/doctor/heal.rs`.
fn corrupt_backup_path(path: &Path) -> PathBuf {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_else(|| std::ffi::OsString::from("config.toml"));
    name.push(format!(".corrupt-{now}"));
    path.with_file_name(name)
}

/// Best-effort 1-based line number for a TOML parse error, derived from the
/// error's byte span. `None` when the span is unavailable; the error's own
/// message still carries the line/column in that case.
fn parse_error_line(contents: &str, err: &toml::de::Error) -> Option<usize> {
    let span = err.span()?;
    let end = span.start.min(contents.len());
    Some(contents[..end].bytes().filter(|&b| b == b'\n').count() + 1)
}

/// Paths for which a corrupt-config warning has already been printed this
/// process, so a hot loader (dashboard handlers, the daemon's per-request
/// config read) doesn't spam stderr once per call.
fn warned_corrupt_config_paths() -> &'static Mutex<HashSet<PathBuf>> {
    static WARNED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    WARNED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Parses `contents` (read from `path`) as `T`, returning the default and
/// printing a one-time-per-path warning if the TOML is corrupt.
///
/// Shared by [`UserConfig::load`] and the daemon's per-client config loader
/// (`user_config_for_client` in `src/daemon.rs`) so both silently-defaulting
/// readers agree on what "corrupt" means and on not spamming stderr.
#[doc(hidden)]
pub fn parse_or_warn_default<T>(path: &Path, contents: &str) -> T
where
    T: Default + serde::de::DeserializeOwned,
{
    match toml::from_str(contents) {
        Ok(value) => value,
        Err(err) => {
            let warned = warned_corrupt_config_paths();
            let mut seen = warned
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if seen.insert(path.to_path_buf()) {
                eprintln!(
                    "warning: could not parse config '{}' ({err}); using defaults",
                    path.display()
                );
            }
            T::default()
        }
    }
}

impl UserConfig {
    /// Loads the user-level config file.
    /// Returns defaults if the file is missing or unreadable. A present but
    /// unparseable file prints a one-time warning to stderr (see
    /// [`parse_or_warn_default`]) instead of silently defaulting.
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        parse_or_warn_default(&path, &contents)
    }

    /// Saves the user-level config file atomically.
    ///
    /// The in-memory config is serialized up front so a serialize failure never
    /// touches the existing file. Writers are serialized across threads and
    /// processes (daemon, MCP servers, CLI all write this file) with a sidecar
    /// `<config>.lock`, mirroring the append lock in `src/storage.rs`: the lock
    /// is taken on a dedicated read/write handle, never on the target file — see
    /// the `LockFileEx` note there. The fresh config is written to a temp file
    /// in the same directory and renamed over `config.toml`, so a concurrent
    /// reader never observes a torn write.
    ///
    /// If the existing file is present but unparseable it is left untouched and
    /// [`ConfigSaveError::CorruptExisting`] is returned (carrying the path and
    /// the parse error's line). Use [`UserConfig::save_with_recovery`] from
    /// explicit config-set commands to quarantine a corrupt file and regenerate.
    pub fn save(&self) -> std::result::Result<(), ConfigSaveError> {
        self.save_inner(false).map(|_| ())
    }

    /// Like [`UserConfig::save`], but self-heals a corrupt existing file.
    ///
    /// When the existing file is unparseable it is renamed to
    /// `config.toml.corrupt-<unix-ts>` (preserving the evidence) and the fresh
    /// in-memory config is written in its place. Returns `Ok(Some(backup_path))`
    /// when a corrupt file was quarantined, `Ok(None)` for an ordinary save.
    ///
    /// Only call this from explicit, user-driven config-set entry points.
    /// Because [`UserConfig::load`] silently returns defaults for a corrupt
    /// file, a background saver's in-memory config after a corrupt load is
    /// mostly defaults, so clobbering there would discard real user data
    /// (upload counters, installed agents, version markers). Config-set commands
    /// set the value the user just asked for, so regenerating is the safe,
    /// unbricking choice.
    pub fn save_with_recovery(&self) -> std::result::Result<Option<PathBuf>, ConfigSaveError> {
        self.save_inner(true)
    }

    fn save_inner(&self, recover: bool) -> std::result::Result<Option<PathBuf>, ConfigSaveError> {
        let Some(path) = config_path() else {
            return Err(ConfigSaveError::PathUnavailable);
        };

        // Serialize first: a serialize failure must never mutate the filesystem
        // or truncate the existing config.
        let contents = toml::to_string_pretty(self).map_err(|err| ConfigSaveError::Serialize {
            message: err.to_string(),
        })?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigSaveError::Io {
                path: parent.to_path_buf(),
                message: "failed to create config directory".to_string(),
                source,
            })?;
        }

        // Serialize concurrent writers on a dedicated sidecar lock handle; never
        // lock the target handle (see the sidecar-lock module note in
        // `src/storage.rs`).
        let lock_path = append_lock_path(&path);
        let lock_file =
            acquire_sidecar_lock_blocking(&lock_path).map_err(|source| ConfigSaveError::Lock {
                path: lock_path.clone(),
                source,
            })?;

        let result = Self::write_locked(&path, &contents, recover);
        let _ = lock_file.unlock();
        result
    }

    fn write_locked(
        path: &Path,
        contents: &str,
        recover: bool,
    ) -> std::result::Result<Option<PathBuf>, ConfigSaveError> {
        let mut backup: Option<PathBuf> = None;
        if path.exists() {
            match fs::read_to_string(path) {
                Ok(existing) => {
                    if let Err(err) = toml::from_str::<Self>(&existing) {
                        if recover {
                            let backup_path = corrupt_backup_path(path);
                            fs::rename(path, &backup_path).map_err(|source| {
                                ConfigSaveError::Io {
                                    path: backup_path.clone(),
                                    message: "failed to quarantine corrupt config file".to_string(),
                                    source,
                                }
                            })?;
                            backup = Some(backup_path);
                        } else {
                            return Err(ConfigSaveError::CorruptExisting {
                                path: path.to_path_buf(),
                                line: parse_error_line(&existing, &err),
                                message: err.to_string(),
                            });
                        }
                    }
                }
                Err(source) => {
                    return Err(ConfigSaveError::ExistingUnreadable {
                        path: path.to_path_buf(),
                        source,
                    });
                }
            }
        }

        // Atomic replace: write a temp file in the same directory, then rename
        // it over the target. `rename` is atomic on POSIX and Windows, so a
        // concurrent reader always sees either the old or the new file whole.
        let temp_path = temp_write_path(path);
        retry_transient_file_op(|| {
            fs::write(&temp_path, contents)?;
            fs::rename(&temp_path, path)
        })
        .map_err(|source| {
            let _ = fs::remove_file(&temp_path);
            ConfigSaveError::Io {
                path: path.to_path_buf(),
                message: "failed to write config file".to_string(),
                source,
            }
        })?;

        Ok(backup)
    }

    /// Saves only when the user-level config file already exists.
    ///
    /// This lets repo-local commands update an existing user profile without
    /// creating one as an incidental side effect. A missing file is a no-op and
    /// returns `Ok(())`; a present-but-corrupt file surfaces the same
    /// [`ConfigSaveError::CorruptExisting`] as [`UserConfig::save`].
    pub fn save_if_exists(&self) -> std::result::Result<(), ConfigSaveError> {
        if !Self::exists() {
            return Ok(());
        }
        self.save()
    }

    /// Returns true if this is a fresh config (file did not exist before).
    pub fn is_fresh() -> bool {
        config_path().is_none_or(|p| !p.exists())
    }

    /// Returns true when the user-level config file already exists.
    pub fn exists() -> bool {
        config_path().is_some_and(|p| p.exists())
    }

    /// Marks `running` as fully installed by advancing both version markers,
    /// returning whether anything changed. This is the single home of the
    /// marker-advancement protocol: only a completed full agent install pass
    /// (the startup silent reinstall, or `post-update`'s reinstall step) may
    /// record its version here, so the next startup's maintenance knows the
    /// work does not need repeating.
    pub fn mark_version_installed(&mut self, running: &str) -> bool {
        if self.previous_version == running && self.last_installed_version == running {
            return false;
        }
        self.previous_version = running.to_string();
        self.last_installed_version = running.to_string();
        true
    }
}

/// Parse a human-readable duration string like "15s" or "1m" into a Duration.
pub fn parse_duration(s: &str) -> Option<std::time::Duration> {
    let s = s.trim();
    if let Some(secs) = s.strip_suffix('s') {
        secs.trim()
            .parse::<u64>()
            .ok()
            .map(std::time::Duration::from_secs)
    } else if let Some(mins) = s.strip_suffix('m') {
        mins.trim()
            .parse::<u64>()
            .ok()
            .map(|m| std::time::Duration::from_secs(m * 60))
    } else {
        s.parse::<u64>().ok().map(std::time::Duration::from_secs)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::duration_suboptimal_units
)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::time::Duration;
    use tempfile::TempDir;
    use tracedecay_runtime_core::config::{USER_DATA_DIR_ENV, lock_user_data_dir_test_env};

    struct EnvRestore {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvRestore {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(previous) => std::env::set_var(self.key, previous),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(parse_duration("15s"), Some(Duration::from_secs(15)));
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration(" 5s "), Some(Duration::from_secs(5)));
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(parse_duration("1m"), Some(Duration::from_secs(60)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
    }

    #[test]
    fn parse_duration_bare_number() {
        assert_eq!(parse_duration("10"), Some(Duration::from_secs(10)));
    }

    #[test]
    fn parse_duration_invalid() {
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("1h"), None);
    }

    #[test]
    fn save_preserves_existing_corrupt_config_file() {
        let _lock = lock_user_data_dir_test_env();
        let temp = TempDir::new().unwrap();
        let _env = EnvRestore::set(USER_DATA_DIR_ENV, temp.path());
        let path = config_path().expect("config path should resolve");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "installed_agents = [\"claude\"]\nautomation =";
        std::fs::write(&path, original).unwrap();

        let mut config = UserConfig::load();
        config.upload_enabled = false;

        let err = config
            .save()
            .expect_err("saving must fail when the existing file is corrupt");
        assert!(
            err.is_corrupt(),
            "expected a corrupt-file error, got: {err}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn save_reports_torn_line_with_path_and_line_number() {
        let _lock = lock_user_data_dir_test_env();
        let temp = TempDir::new().unwrap();
        let _env = EnvRestore::set(USER_DATA_DIR_ENV, temp.path());
        let path = config_path().expect("config path should resolve");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Reproduce the exact torn-write seen in the wild: a valid line followed
        // by a bare " true" orphan with no key.
        let torn = "upload_enabled = false\n true";
        std::fs::write(&path, torn).unwrap();

        let config = UserConfig::load();
        let err = config
            .save()
            .expect_err("torn config must not save via the plain path");
        assert!(err.is_corrupt(), "expected corrupt error, got: {err}");
        let message = err.to_string();
        assert!(
            message.contains(&path.display().to_string()),
            "error should name the file path: {message}"
        );
        assert!(
            message.contains("line 2") || message.contains("line "),
            "error should carry a line number: {message}"
        );
        // The corrupt file is preserved untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), torn);
    }

    #[test]
    fn save_with_recovery_backs_up_and_regenerates() {
        let _lock = lock_user_data_dir_test_env();
        let temp = TempDir::new().unwrap();
        let _env = EnvRestore::set(USER_DATA_DIR_ENV, temp.path());
        let path = config_path().expect("config path should resolve");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let torn = "upload_enabled = false\n true";
        std::fs::write(&path, torn).unwrap();

        let mut config = UserConfig::load();
        config.upload_enabled = true;
        let backup = config
            .save_with_recovery()
            .expect("recovery save should succeed")
            .expect("a corrupt file should have been quarantined");

        // The corrupt content is preserved at the backup path.
        assert!(
            backup
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("config.toml.corrupt-")),
            "backup should be config.toml.corrupt-<ts>: {backup:?}"
        );
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), torn);

        // The regenerated file parses and reflects the in-memory value.
        let saved = std::fs::read_to_string(&path).unwrap();
        let reparsed: UserConfig = toml::from_str(&saved).expect("regenerated config parses");
        assert!(reparsed.upload_enabled);

        // A subsequent ordinary save now succeeds (no longer bricked).
        config.save().expect("save after recovery should succeed");
    }

    #[test]
    fn save_regenerates_when_no_file_exists() {
        let _lock = lock_user_data_dir_test_env();
        let temp = TempDir::new().unwrap();
        let _env = EnvRestore::set(USER_DATA_DIR_ENV, temp.path());
        let path = config_path().expect("config path should resolve");

        let config = UserConfig::default();
        config.save().expect("save should create a fresh file");
        let saved = std::fs::read_to_string(&path).unwrap();
        toml::from_str::<UserConfig>(&saved).expect("fresh config parses");
    }

    #[test]
    fn save_reports_unreadable_existing_file() {
        let _lock = lock_user_data_dir_test_env();
        let temp = TempDir::new().unwrap();
        let _env = EnvRestore::set(USER_DATA_DIR_ENV, temp.path());
        let path = config_path().expect("config path should resolve");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // A directory where the config file should be: exists(), but reading it
        // yields an I/O error rather than a parse error.
        std::fs::create_dir_all(&path).unwrap();

        let config = UserConfig::default();
        let err = config
            .save()
            .expect_err("an unreadable existing path must not save");
        assert!(
            matches!(err, ConfigSaveError::ExistingUnreadable { .. }),
            "expected ExistingUnreadable, got: {err}"
        );
    }

    #[test]
    fn path_unavailable_error_displays() {
        let err = ConfigSaveError::PathUnavailable;
        assert!(err.to_string().contains("user config path"));
    }

    #[test]
    fn concurrent_saves_always_leave_a_parseable_file() {
        let _lock = lock_user_data_dir_test_env();
        let temp = TempDir::new().unwrap();
        let _env = EnvRestore::set(USER_DATA_DIR_ENV, temp.path());
        let path = config_path().expect("config path should resolve");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let handles: Vec<_> = (0..8u64)
            .map(|thread_idx| {
                std::thread::spawn(move || {
                    for i in 0..20u64 {
                        let config = UserConfig {
                            pending_upload: thread_idx * 100 + i,
                            ..UserConfig::default()
                        };
                        // Every write must succeed and leave a parseable file.
                        config.save().expect("concurrent save should succeed");
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("writer thread should not panic");
        }

        let saved = std::fs::read_to_string(&path).unwrap();
        toml::from_str::<UserConfig>(&saved)
            .expect("file must be parseable after concurrent saves");
    }

    #[test]
    fn concurrent_reader_never_observes_a_torn_write() {
        let _lock = lock_user_data_dir_test_env();
        let temp = TempDir::new().unwrap();
        let _env = EnvRestore::set(USER_DATA_DIR_ENV, temp.path());
        let path = config_path().expect("config path should resolve");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Seed a valid file so the reader always has something to read.
        UserConfig::default().save().expect("seed save");

        let reader_path = path.clone();
        let reader = std::thread::spawn(move || {
            for _ in 0..300 {
                if let Ok(contents) = std::fs::read_to_string(&reader_path) {
                    if !contents.is_empty() {
                        toml::from_str::<UserConfig>(&contents).unwrap_or_else(|err| {
                            panic!("reader observed a torn/partial config: {err}\n{contents}")
                        });
                    }
                }
            }
        });

        for i in 0..150u64 {
            let config = UserConfig {
                pending_upload: i,
                ..UserConfig::default()
            };
            config.save().expect("writer save should succeed");
        }
        reader.join().expect("reader thread should not panic");
    }

    #[test]
    fn save_preserves_unknown_config_keys() {
        let _lock = lock_user_data_dir_test_env();
        let temp = TempDir::new().unwrap();
        let _env = EnvRestore::set(USER_DATA_DIR_ENV, temp.path());
        let path = config_path().expect("config path should resolve");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "upload_enabled = true\nfuture_key = \"keep-me\"\n[future_table]\nflag = true\n",
        )
        .unwrap();

        let mut config = UserConfig::load();
        config.upload_enabled = false;

        config
            .save()
            .expect("save should succeed with a valid existing file");
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("future_key = \"keep-me\""));
        assert!(saved.contains("[future_table]"));
        assert!(saved.contains("flag = true"));
        assert!(saved.contains("upload_enabled = false"));
    }
}
