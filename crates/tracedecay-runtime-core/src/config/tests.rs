use super::{
    GENERATED_DIR_SEGMENTS, SyncConfig, TraceDecayConfig, USER_DATA_DIR_ENV, canonicalize_data_dir,
    db_filename, get_project_db_path, get_tracedecay_dir, is_excluded, is_excluded_dir,
    is_generated_dir_segment, is_generated_path_segment, is_ignored_by_explicit_global_excludes,
    is_ignored_by_git, is_included, lock_user_data_dir_test_env, user_data_dir,
};
use std::ffi::OsString;
use std::fs;
use std::process::Command;
use tempfile::TempDir;

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
fn project_data_paths_use_the_current_layout() {
    let root = TempDir::new().expect("temporary root");

    assert_eq!(
        get_tracedecay_dir(root.path()),
        root.path().join(".tracedecay")
    );
    assert_eq!(
        get_project_db_path(root.path()),
        root.path().join(".tracedecay/tracedecay.db")
    );
    assert_eq!(
        db_filename(&root.path().join(".tracedecay")),
        "tracedecay.db"
    );
}

#[test]
fn include_and_exclude_patterns_remain_independent() {
    let config = TraceDecayConfig {
        include: vec![".github/**".to_string()],
        ..TraceDecayConfig::default()
    };

    assert!(is_included(".github/workflows/ci.yml", &config));
    assert!(!is_included("src/main.rs", &config));
    assert!(is_excluded_dir("node_modules", &config));
}

#[test]
fn sync_defaults_round_trip() {
    let config = TraceDecayConfig::default();
    let serialized = serde_json::to_string(&config).expect("serialize default config");
    let reparsed: TraceDecayConfig =
        serde_json::from_str(&serialized).expect("deserialize default config");

    assert_eq!(reparsed.sync, config.sync);
    assert_eq!(reparsed.sync, SyncConfig::default());
}

#[test]
fn generated_directory_segments_cover_build_artifacts() {
    for segment in ["node_modules", "target", "vendor", "__pycache__"] {
        assert!(is_generated_dir_segment(segment), "{segment}");
    }
    assert!(!is_generated_dir_segment("src"));
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
    let _lock = lock_user_data_dir_test_env();
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
fn nextest_shared_target_profile_is_isolated_by_test_name() {
    let _lock = lock_user_data_dir_test_env();
    let root = TempDir::new().unwrap();
    let target = root.path().join("target");
    fs::create_dir_all(target.join("debug")).unwrap();
    let profile = target.join("test-profile/.tracedecay");
    let _profile = EnvRestore::set(USER_DATA_DIR_ENV, &profile);
    let _binary_id = EnvRestore::set("NEXTEST_BINARY_ID", "tracedecay::storage_suite");
    let _test_name = EnvRestore::set("NEXTEST_TEST_NAME", "storage_suite::isolated_profile");

    let resolved = user_data_dir().unwrap();
    let canonical_profile = canonicalize_data_dir(profile);
    assert!(resolved.starts_with(canonical_profile.join("nextest")));
    assert_ne!(resolved, canonical_profile);
}

#[test]
fn nextest_preserves_explicit_temp_profile_override() {
    let _lock = lock_user_data_dir_test_env();
    let root = TempDir::new().unwrap();
    let profile = root.path().join("test-profile/.tracedecay");
    let _profile = EnvRestore::set(USER_DATA_DIR_ENV, &profile);
    let _test_name = EnvRestore::set("NEXTEST_TEST_NAME", "storage_suite::explicit_profile");

    assert_eq!(user_data_dir().unwrap(), canonicalize_data_dir(profile));
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
    assert!(is_excluded("node_modules/express/index.js", &config));
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
    let config = TraceDecayConfig::default();
    assert!(is_excluded("node_modules/_", &config));
    assert!(is_excluded("projectA/node_modules/_", &config));
}

#[test]
fn test_is_excluded_dir_bare_pattern() {
    let config = TraceDecayConfig {
        exclude: vec!["**/dist".to_string()],
        ..TraceDecayConfig::default()
    };
    assert!(is_excluded_dir("dist", &config));
    assert!(is_excluded_dir("packages/web/dist", &config));
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

    assert_eq!(is_ignored_by_git(&repo, Some(&git_config)), Some(true));
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

    assert_eq!(
        is_ignored_by_explicit_global_excludes(&repo, &git_config),
        Some(true)
    );
}

#[test]
fn sync_config_defaults_round_trip() {
    let config = TraceDecayConfig::default();
    let json = serde_json::to_string(&config).unwrap();
    let parsed: TraceDecayConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(config.sync, parsed.sync);
    assert_eq!(parsed.sync, SyncConfig::default());
    assert!(parsed.sync.auto_watch);
    assert_eq!(parsed.sync.watch_debounce_ms, 2000);
    assert_eq!(parsed.sync.full_sync_escalation_files, 500);
    assert_eq!(parsed.sync.max_concurrent_syncs, 2);
    assert!(parsed.sync.auto_init);
}

#[test]
fn telemetry_timing_defaults_on_and_round_trips() {
    let config = TraceDecayConfig::default();
    assert!(config.telemetry.timings);
    let json = serde_json::to_string(&config).unwrap();
    let parsed: TraceDecayConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.telemetry, super::TelemetryConfig::default());

    let legacy = r#"{
        "version": 1,
        "root_dir": "/tmp/proj",
        "exclude": [],
        "max_file_size": 1048576,
        "extract_docstrings": true,
        "track_call_sites": true
    }"#;
    let parsed: TraceDecayConfig = serde_json::from_str(legacy).unwrap();
    assert!(parsed.telemetry.timings);

    let disabled = r#"{
        "version": 1,
        "root_dir": "/tmp/proj",
        "exclude": [],
        "max_file_size": 1048576,
        "extract_docstrings": true,
        "track_call_sites": true,
        "telemetry": { "timings": false }
    }"#;
    let parsed: TraceDecayConfig = serde_json::from_str(disabled).unwrap();
    assert!(!parsed.telemetry.timings);
}

#[test]
fn diagnostics_prewarm_round_trips_and_defaults_off() {
    let config = TraceDecayConfig::default();
    assert!(!config.diagnostics_prewarm, "prewarm must default off");
    let json = serde_json::to_string(&config).unwrap();
    let parsed: TraceDecayConfig = serde_json::from_str(&json).unwrap();
    assert!(!parsed.diagnostics_prewarm);

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
    let json = r#"{
        "version": 1,
        "root_dir": "/tmp/proj",
        "exclude": [],
        "max_file_size": 1048576,
        "extract_docstrings": true,
        "track_call_sites": true
    }"#;
    let parsed: TraceDecayConfig = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.sync, SyncConfig::default());
}

#[test]
fn partial_sync_table_fills_missing_fields_with_defaults() {
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
    assert_eq!(parsed.sync.watch_debounce_ms, 2000);
    assert_eq!(parsed.sync.max_concurrent_syncs, 2);
    assert!(parsed.sync.read_refresh);
}

#[test]
fn pr_autotrack_defaults_off_and_survives_missing_keys() {
    let json = r#"{
        "version": 1,
        "root_dir": "/tmp/proj",
        "exclude": [],
        "max_file_size": 1048576,
        "extract_docstrings": true,
        "track_call_sites": true,
        "sync": { "auto_watch": true }
    }"#;
    let parsed: TraceDecayConfig = serde_json::from_str(json).unwrap();
    assert!(!parsed.sync.auto_track_pr_branches);
    assert_eq!(parsed.sync.auto_track_pr_poll_secs, 300);
    assert_eq!(parsed.sync.effective_auto_track_pr_poll_secs(), 300);
}

#[test]
fn pr_autotrack_round_trips_and_clamps_poll_floor() {
    let json = r#"{
        "version": 1,
        "root_dir": "/tmp/proj",
        "exclude": [],
        "max_file_size": 1048576,
        "extract_docstrings": true,
        "track_call_sites": true,
        "sync": { "auto_track_pr_branches": true, "auto_track_pr_poll_secs": 5 }
    }"#;
    let parsed: TraceDecayConfig = serde_json::from_str(json).unwrap();
    assert!(parsed.sync.auto_track_pr_branches);
    assert_eq!(parsed.sync.auto_track_pr_poll_secs, 5);
    assert_eq!(
        parsed.sync.effective_auto_track_pr_poll_secs(),
        super::MIN_AUTO_TRACK_PR_POLL_SECS
    );

    let round = serde_json::to_string(&parsed).unwrap();
    let reparsed: TraceDecayConfig = serde_json::from_str(&round).unwrap();
    assert_eq!(reparsed.sync, parsed.sync);
}

#[test]
fn pr_autotrack_env_overrides() {
    let _lock = lock_user_data_dir_test_env();
    let _enable = EnvRestore::set("TRACEDECAY_SYNC_AUTO_TRACK_PR_BRANCHES", "true");
    let _poll = EnvRestore::set("TRACEDECAY_SYNC_AUTO_TRACK_PR_POLL_SECS", "120");

    let overridden = SyncConfig::default().with_env_overrides();
    assert!(overridden.auto_track_pr_branches);
    assert_eq!(overridden.auto_track_pr_poll_secs, 120);
}

#[test]
fn sync_config_env_overrides_bool_and_int() {
    let _lock = lock_user_data_dir_test_env();
    let _watch = EnvRestore::set("TRACEDECAY_SYNC_AUTO_WATCH", "false");
    let _debounce = EnvRestore::set("TRACEDECAY_SYNC_WATCH_DEBOUNCE_MS", "5000");
    let _bad = EnvRestore::set("TRACEDECAY_SYNC_MAX_CONCURRENT_SYNCS", "not-a-number");

    let overridden = SyncConfig::default().with_env_overrides();
    assert!(!overridden.auto_watch);
    assert_eq!(overridden.watch_debounce_ms, 5000);
    assert_eq!(
        overridden.max_concurrent_syncs,
        SyncConfig::default().max_concurrent_syncs
    );
}

#[test]
fn generated_dir_segments_cover_the_union_all_call_sites_need() {
    for segment in [
        "node_modules",
        "vendor",
        "build",
        "dist",
        "out",
        "coverage",
        ".cache",
        ".next",
        ".turbo",
        ".gradle",
        ".venv",
        "venv",
        "__pycache__",
    ] {
        assert!(
            GENERATED_DIR_SEGMENTS.contains(&segment),
            "{segment} (from scan.rs's old list) missing from GENERATED_DIR_SEGMENTS"
        );
    }
    assert!(GENERATED_DIR_SEGMENTS.contains(&"target"));
    assert!(GENERATED_DIR_SEGMENTS.contains(&".worktrees"));
    assert!(!GENERATED_DIR_SEGMENTS.contains(&".git"));
}

#[test]
fn is_generated_dir_segment_delegates_for_segments_unique_to_one_former_list() {
    for segment in ["target", ".worktrees", "coverage", ".venv", "__pycache__"] {
        assert!(
            is_generated_dir_segment(segment),
            "{segment} should be recognized as a generated/vendored segment"
        );
    }
    assert!(!is_generated_dir_segment("src"));
    assert!(!is_generated_dir_segment("builder"));
}

#[test]
fn is_generated_path_segment_matches_segments_and_minified_suffix() {
    assert!(is_generated_path_segment("packages/web/target/debug/x"));
    assert!(is_generated_path_segment(".worktrees/feature/src/lib.rs"));
    assert!(is_generated_path_segment("assets/app.min.js"));
    assert!(is_generated_path_segment("assets/app.min.css"));
    assert!(!is_generated_path_segment("src/redundancy.rs"));
    assert!(!is_generated_path_segment("builder/mod.rs"));
}

#[test]
fn default_excludes_still_catch_target_and_worktrees() {
    let config = TraceDecayConfig::default();
    assert!(is_excluded("target/debug/build", &config));
    assert!(is_excluded("crates/sub/target/debug/build", &config));
    assert!(is_excluded(".worktrees/feature/src/lib.rs", &config));
    assert!(is_excluded(".git/HEAD", &config));
    assert!(is_excluded(".tracedecay/tracedecay.db", &config));
    assert!(is_excluded("bin/cli.js", &config));
}
