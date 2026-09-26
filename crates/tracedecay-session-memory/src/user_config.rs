//! User-level configuration stored in the `TraceDecay` user data directory.
//!
//! All fields have defaults so a missing file or missing fields are handled
//! gracefully. Keys other profile readers own (e.g. GitHub repositories) are
//! preserved; retired keys are dropped at load and erased by the next write.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tracedecay_domain::canonical_text::default_true;

use tracedecay_automation::config::AutomationConfig;
use tracedecay_runtime_core::storage::{append_lock_path, retry_transient_file_op};

/// Keys canonical `user.*` configuration settings own. They are never read
/// from this file.
const RETIRED_KEYS: [&str; 3] = [
    "upload_enabled",
    "watcher_debounce",
    "extraction_timeout_secs",
];

/// User-level tracedecay configuration.
#[derive(Debug, Serialize, Deserialize)]
pub struct UserConfig {
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

    /// Per-agent dashboard integration policy. Missing entries retain the
    /// historical default of installing the dashboard integration.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agent_dashboard_enabled: BTreeMap<String, bool>,

    /// Cached country flags from the worldwide counter.
    #[serde(default)]
    pub cached_country_flags: Vec<String>,

    /// UNIX timestamp of last country flags fetch.
    #[serde(default)]
    pub last_flags_fetch_at: i64,

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

    /// Global defaults for self-improvement automation. Project/profile
    /// dashboard sidecars may override these values.
    #[serde(default, skip_serializing_if = "AutomationConfig::is_default")]
    pub automation: AutomationConfig,

    /// Whether lifecycle hooks inject fact-store memory into agent context
    /// (session digests, prompt-gated recall, the Cursor memory rule).
    /// The `TRACEDECAY_MEMORY_INJECTION` env var overrides this at runtime.
    #[serde(default = "default_true")]
    pub memory_injection_enabled: bool,

    /// Keys other profile readers own, preserved across saves.
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            pending_upload: 0,
            last_upload_at: 0,
            last_worldwide_total: 0,
            last_worldwide_fetch_at: 0,
            last_flush_attempt_at: 0,
            cached_latest_version: String::new(),
            last_version_check_at: 0,
            last_version_warning_at: 0,
            installed_agents: Vec::new(),
            agent_dashboard_enabled: BTreeMap::new(),
            cached_country_flags: Vec::new(),
            last_flags_fetch_at: 0,
            last_installed_version: String::new(),
            previous_version: String::new(),
            automation: AutomationConfig::default(),
            memory_injection_enabled: true,
            extra: BTreeMap::new(),
        }
    }
}

/// The user-level config file of the profile whose data directory is
/// `profile_root`.
pub fn config_path(profile_root: &Path) -> PathBuf {
    profile_root.join("config.toml")
}

/// Errors returned by strict loads and configuration saves.
///
/// Distinguishes the ways a save can fail so callers can surface an actionable
/// message instead of a bare boolean. The corrupt-existing-file case carries
/// the path and the TOML parse error (whose message includes the line/column),
/// so a user can find and fix, or delete, the offending file.
#[derive(Debug)]
pub enum ConfigSaveError {
    /// The existing config file is present but could not be read.
    ExistingUnreadable { path: PathBuf, source: io::Error },
    /// The existing config file is present but is not valid TOML. It is left
    /// in place untouched and nothing is copied beside it.
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

impl std::fmt::Display for ConfigSaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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
                    "config file {} is corrupt at line {line}: {message}. \
                     Fix it, or delete it to regenerate defaults",
                    path.display()
                ),
                None => write!(
                    f,
                    "config file {} is corrupt: {message}. \
                     Fix it, or delete it to regenerate defaults",
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
        .map_or(0, |d| d.as_nanos());
    let pid = std::process::id();
    let mut name = path.file_name().map_or_else(
        || std::ffi::OsString::from("config.toml"),
        std::ffi::OsStr::to_os_string,
    );
    name.push(format!(".tmp-{pid}-{unique}"));
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
/// Shared by [`UserConfig::load`] call sites so silently-defaulting readers
/// agree on what "corrupt" means and on not spamming stderr.
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
    /// Returns the persisted dashboard policy for an agent.
    ///
    /// Existing configs predate this field, so absence means enabled.
    pub fn dashboard_enabled_for_agent(&self, agent_id: &str) -> bool {
        self.agent_dashboard_enabled
            .get(agent_id)
            .copied()
            .unwrap_or(true)
    }

    /// Loads the user-level config file.
    /// Returns defaults if the file is missing or unreadable. A present but
    /// unparseable file prints a one-time warning to stderr (see
    /// [`parse_or_warn_default`]) instead of silently defaulting.
    pub fn load(profile_root: &Path) -> Self {
        let path = config_path(profile_root);
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        parse_or_warn_default::<Self>(&path, &contents).without_retired_keys()
    }

    fn without_retired_keys(mut self) -> Self {
        for key in RETIRED_KEYS {
            self.extra.remove(key);
        }
        self
    }

    /// Loads configuration without substituting defaults for an unreadable or
    /// malformed persisted file.
    ///
    /// Lifecycle callers use this when a missing policy would enable host
    /// behavior: corruption must stop the operation rather than silently turn
    /// an opt-out back on. A genuinely missing file still means defaults.
    pub fn load_strict(profile_root: &Path) -> std::result::Result<Self, ConfigSaveError> {
        let path = config_path(profile_root);
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(ConfigSaveError::ExistingUnreadable { path, source });
            }
        };
        toml::from_str::<Self>(&contents)
            .map(Self::without_retired_keys)
            .map_err(|error| ConfigSaveError::CorruptExisting {
                path,
                line: parse_error_line(&contents, &error),
                message: error.to_string(),
            })
    }

    /// Saves the user-level config file atomically.
    ///
    /// The in-memory config is serialized up front so a serialize failure never
    /// touches the existing file. Writers are serialized across threads and
    /// processes (daemon, MCP servers, CLI all write this file) with a sidecar
    /// `<config>.lock`, mirroring the append lock in `src/storage.rs`: the lock
    /// is taken on a dedicated read/write handle, never on the target file, see
    /// the `LockFileEx` note there. The fresh config is written to a temp file
    /// in the same directory and renamed over `config.toml`, so a concurrent
    /// reader never observes a torn write.
    ///
    /// If the existing file is present but unparseable,
    /// [`ConfigSaveError::CorruptExisting`] is returned (carrying the path and
    /// the parse error's line) before anything is created or written: the
    /// corrupt file stays in place for the operator and nothing is copied.
    pub fn save(&self, profile_root: &Path) -> std::result::Result<(), ConfigSaveError> {
        let path = config_path(profile_root);

        // Serialize first: a serialize failure must never mutate the filesystem
        // or truncate the existing config.
        let contents = toml::to_string_pretty(self).map_err(|err| ConfigSaveError::Serialize {
            message: err.to_string(),
        })?;

        // Checked outside the lock: every writer holds it and renames a whole
        // parseable file into place, so only an outside editor, which no lock
        // excludes, can corrupt the file between this check and the rename.
        Self::refuse_unparseable_existing(&path)?;

        if let Some(parent) = path.parent() {
            tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(parent).map_err(
                |source| ConfigSaveError::Io {
                    path: parent.to_path_buf(),
                    message: "failed to create config directory".to_string(),
                    source,
                },
            )?;
        }

        // Serialize concurrent writers on a dedicated sidecar lock handle; never
        // lock the target handle (see the sidecar-lock module note in
        // `src/storage.rs`).
        let lock_path = append_lock_path(&path);
        let lock_file = tracedecay_runtime_core::storage::acquire_sidecar_lock_blocking(&lock_path)
            .map_err(|source| ConfigSaveError::Lock {
                path: lock_path.clone(),
                source,
            })?;

        let result = Self::write_locked(&path, &contents);
        drop(lock_file);
        result
    }

    fn refuse_unparseable_existing(path: &Path) -> std::result::Result<(), ConfigSaveError> {
        let existing = match fs::read_to_string(path) {
            Ok(existing) => existing,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(ConfigSaveError::ExistingUnreadable {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        toml::from_str::<Self>(&existing)
            .map(|_| ())
            .map_err(|err| ConfigSaveError::CorruptExisting {
                path: path.to_path_buf(),
                line: parse_error_line(&existing, &err),
                message: err.to_string(),
            })
    }

    fn write_locked(path: &Path, contents: &str) -> std::result::Result<(), ConfigSaveError> {
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
        })
    }

    /// Saves only when the user-level config file already exists.
    ///
    /// This lets repo-local commands update an existing user profile without
    /// creating one as an incidental side effect. A missing file is a no-op and
    /// returns `Ok(())`; a present-but-corrupt file surfaces the same
    /// [`ConfigSaveError::CorruptExisting`] as [`UserConfig::save`].
    pub fn save_if_exists(&self, profile_root: &Path) -> std::result::Result<(), ConfigSaveError> {
        if !Self::exists(profile_root) {
            return Ok(());
        }
        self.save(profile_root)
    }

    /// Returns true when the user-level config file already exists.
    pub fn exists(profile_root: &Path) -> bool {
        config_path(profile_root).exists()
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
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn parse_duration_invalid() {
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("1h"), None);
    }

    #[test]
    fn corrupt_config_is_a_typed_error_left_in_place_with_nothing_copied() {
        let temp = TempDir::new().unwrap();
        let profile = temp.path();
        let path = config_path(profile);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // The torn write seen in the wild: a valid line followed by a bare
        // " true" orphan with no key.
        let torn = "pending_upload = 0\n true";
        std::fs::write(&path, torn).unwrap();
        let entries = || {
            let mut names = std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect::<Vec<_>>();
            names.sort();
            names
        };
        let config = UserConfig {
            pending_upload: 7,
            ..UserConfig::default()
        };

        let save_error = config
            .save(profile)
            .expect_err("a corrupt config must not save");

        assert_eq!(entries(), vec!["config.toml".to_owned()]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), torn);
        let expected = format!(
            "config file {} is corrupt at line 2: TOML parse error at line 2, column 6\n  |\n2 |  true\n  |      ^\nkey with no value, expected `=`\n. Fix it, or delete it to regenerate defaults",
            path.display()
        );
        assert_eq!(save_error.to_string(), expected);
        let load_error =
            UserConfig::load_strict(profile).expect_err("a corrupt config must not load");
        assert_eq!(load_error.to_string(), expected);
        assert_eq!(entries(), vec!["config.toml".to_owned()]);

        std::fs::write(&path, "pending_upload = 0\n").unwrap();
        config.save(profile).expect("a parseable config saves");
        assert_eq!(UserConfig::load_strict(profile).unwrap().pending_upload, 7);
    }

    #[test]
    fn save_regenerates_when_no_file_exists() {
        let temp = TempDir::new().unwrap();
        let profile_root = temp.path().join("profile");
        let profile = profile_root.as_path();
        let path = config_path(profile);

        let config = UserConfig::default();
        config
            .save(profile)
            .expect("save should create a fresh file");
        let saved = std::fs::read_to_string(&path).unwrap();
        toml::from_str::<UserConfig>(&saved).expect("fresh config parses");
        #[cfg(unix)]
        {
            assert_eq!(
                std::fs::metadata(profile_root)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn save_reports_unreadable_existing_file() {
        let temp = TempDir::new().unwrap();
        let profile = temp.path();
        let path = config_path(profile);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // A directory where the config file should be: exists(), but reading it
        // yields an I/O error rather than a parse error.
        std::fs::create_dir_all(&path).unwrap();

        let config = UserConfig::default();
        let err = config
            .save(profile)
            .expect_err("an unreadable existing path must not save");
        assert!(
            matches!(err, ConfigSaveError::ExistingUnreadable { .. }),
            "expected ExistingUnreadable, got: {err}"
        );
    }

    #[test]
    fn concurrent_saves_always_leave_a_parseable_file() {
        let temp = TempDir::new().unwrap();
        let profile = temp.path();
        let path = config_path(profile);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let handles: Vec<_> = (0..8u64)
            .map(|thread_idx| {
                let profile = profile.to_path_buf();
                std::thread::spawn(move || {
                    for i in 0..20u64 {
                        let config = UserConfig {
                            pending_upload: thread_idx * 100 + i,
                            ..UserConfig::default()
                        };
                        // Every write must succeed and leave a parseable file.
                        config
                            .save(&profile)
                            .expect("concurrent save should succeed");
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
        let temp = TempDir::new().unwrap();
        let profile = temp.path();
        let path = config_path(profile);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Seed a valid file so the reader always has something to read.
        UserConfig::default().save(profile).expect("seed save");

        let reader_path = path.clone();
        let reader = std::thread::spawn(move || {
            for _ in 0..300 {
                if let Ok(contents) = std::fs::read_to_string(&reader_path)
                    && !contents.is_empty()
                {
                    toml::from_str::<UserConfig>(&contents).unwrap_or_else(|err| {
                        panic!("reader observed a torn/partial config: {err}\n{contents}")
                    });
                }
            }
        });

        for i in 0..150u64 {
            let config = UserConfig {
                pending_upload: i,
                ..UserConfig::default()
            };
            config.save(profile).expect("writer save should succeed");
        }
        reader.join().expect("reader thread should not panic");
    }

    #[test]
    fn save_erases_retired_keys_and_preserves_other_readers_keys() {
        let temp = TempDir::new().unwrap();
        let profile = temp.path();
        let path = config_path(profile);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "upload_enabled = true\nwatcher_debounce = \"9s\"\nextraction_timeout_secs = 5\n\
             future_key = \"keep-me\"\n[future_table]\nflag = true\n",
        )
        .unwrap();

        let mut config = UserConfig::load(profile);
        config.pending_upload = 3;

        config
            .save(profile)
            .expect("save should succeed with a valid existing file");
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("future_key = \"keep-me\""));
        assert!(saved.contains("[future_table]"));
        assert!(saved.contains("flag = true"));
        assert!(saved.contains("pending_upload = 3"));
        for retired in RETIRED_KEYS {
            assert!(
                !saved.contains(retired),
                "{retired} survived a write:\n{saved}"
            );
        }
    }

    #[test]
    fn strict_load_rejects_corrupt_dashboard_policy_instead_of_enabling_it() {
        let temp = TempDir::new().unwrap();
        let profile = temp.path();
        let path = config_path(profile);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "installed_agents = [\"hermes\"]\nagent_dashboard_enabled = { hermes = false\n",
        )
        .unwrap();

        assert!(matches!(
            UserConfig::load_strict(profile),
            Err(ConfigSaveError::CorruptExisting { .. })
        ));
    }
}
