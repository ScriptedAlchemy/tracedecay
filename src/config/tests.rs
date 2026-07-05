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
