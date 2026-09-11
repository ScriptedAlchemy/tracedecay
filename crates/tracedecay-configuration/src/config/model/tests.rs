use super::{
    TraceDecayConfig, is_excluded, is_excluded_dir, is_generated_path_segment,
    is_ignored_by_explicit_global_excludes, is_ignored_by_git, is_included, parse_env_bool,
};
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;
use tracedecay_runtime_core::config::{
    GENERATED_DIR_SEGMENTS, PinnedUserDataDir, USER_DATA_DIR_ENV, db_filename,
    discover_project_root, get_project_db_path, get_tracedecay_dir, is_ambient_project_root,
    is_generated_dir_segment, lock_user_data_dir_test_env, user_data_dir,
};
use tracedecay_semantic_contracts::{
    DEFAULT_FASTEMBED_MODEL_ID, SemanticConfig, SemanticProfileSelection,
};

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

    let canonical_profile = target
        .canonicalize()
        .unwrap()
        .join("test-profile/.tracedecay");
    assert!(resolved.starts_with(canonical_profile.join("nextest")));
    assert_ne!(resolved, canonical_profile);
}

#[test]
fn nextest_shared_target_profile_is_isolated_under_the_perf_profile() {
    let _lock = lock_user_data_dir_test_env();
    let root = TempDir::new().unwrap();
    let target = root.path().join("target");
    // A `cargo test-ci` / CI checkout only ever builds `target/perf`.
    fs::create_dir_all(target.join("perf")).unwrap();
    let profile = target.join("test-profile/.tracedecay");
    let _profile = EnvRestore::set(USER_DATA_DIR_ENV, &profile);
    let _binary_id = EnvRestore::set("NEXTEST_BINARY_ID", "tracedecay::storage_suite");
    let _test_name = EnvRestore::set("NEXTEST_TEST_NAME", "storage_suite::perf_profile");

    let resolved = user_data_dir().unwrap();

    let canonical_profile = target
        .canonicalize()
        .unwrap()
        .join("test-profile/.tracedecay");
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

    assert_eq!(
        user_data_dir().unwrap(),
        root.path()
            .canonicalize()
            .unwrap()
            .join("test-profile/.tracedecay")
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
fn semantic_config_defaults_to_offline_healthy_baseline() {
    let config = TraceDecayConfig::default();
    assert_eq!(config.semantic, SemanticConfig::default());
    assert_eq!(
        config.semantic.selected_model.as_deref(),
        Some(DEFAULT_FASTEMBED_MODEL_ID)
    );
    assert!(config.semantic.auto_download);
    assert!(config.semantic.active_profile.is_none());
    assert!(config.semantic.rollback_profile.is_none());
    assert!(config.semantic.validate().is_ok());
    assert!(config.semantic.resources.max_concurrent_sessions >= 1);

    let json = serde_json::to_string(&config).unwrap();
    let parsed: TraceDecayConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.semantic, config.semantic);
}

/// Host-absolute fixture path: `artifact_path` validation requires
/// `Path::is_absolute`, which a bare `/...` literal fails on Windows.
fn absolute_fixture_path(posix: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("C:{}", posix.replace('/', "\\")))
    } else {
        PathBuf::from(posix)
    }
}

#[test]
fn semantic_config_accepts_only_explicit_local_installed_profiles() {
    let local = SemanticProfileSelection {
        profile_id: "code-embedding.v1".to_owned(),
        accepted_profile_digest: tracedecay_domain::ManifestDigest::new(format!(
            "sha256:{}",
            "1".repeat(64)
        ))
        .unwrap(),
        artifact_digest: "a".repeat(64),
        artifact_path: absolute_fixture_path("/var/lib/tracedecay/models/code-embedding"),
    };
    let mut semantic = SemanticConfig {
        active_profile: Some(local.clone()),
        rollback_profile: Some(SemanticProfileSelection {
            profile_id: "code-embedding.previous".to_owned(),
            accepted_profile_digest: tracedecay_domain::ManifestDigest::new(format!(
                "sha256:{}",
                "2".repeat(64)
            ))
            .unwrap(),
            artifact_digest: "b".repeat(64),
            artifact_path: absolute_fixture_path(
                "/var/lib/tracedecay/models/code-embedding-previous",
            ),
        }),
        ..SemanticConfig::default()
    };
    assert!(semantic.validate().is_ok());

    semantic.active_profile.as_mut().unwrap().artifact_path =
        std::path::PathBuf::from("https://models.example/code-embedding");
    assert!(
        semantic.validate().is_err(),
        "runtime configuration must not admit network or ambient-cache discovery"
    );
    semantic.active_profile = Some(local.clone());
    semantic.rollback_profile = Some(local);
    assert!(
        semantic.validate().is_err(),
        "active and rollback selections must remain distinct"
    );
}

#[test]
fn semantic_resource_ceilings_reject_zero_or_incoherent_limits() {
    let mut semantic = SemanticConfig::default();
    semantic.resources.max_threads = 0;
    assert!(semantic.validate().is_err());

    semantic = SemanticConfig::default();
    semantic.resources.max_model_bytes = semantic.resources.max_resident_bytes + 1;
    assert!(semantic.validate().is_err());
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
    assert_eq!(parsed.sync, crate::SyncConfig::default());
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
    assert!(!parsed.sync.watch_linked_worktrees);
    assert_eq!(parsed.sync.backstop_interval_mins, 99);
    // Untouched fields keep their defaults.
    assert_eq!(parsed.sync.watch_debounce_ms, 2000);
    assert_eq!(parsed.sync.max_concurrent_syncs, 2);
    assert!(parsed.sync.read_refresh);
}

#[test]
fn pr_autotrack_defaults_off_and_survives_missing_keys() {
    // Back-compat: a config predating the PR-autotrack keys must default the
    // feature OFF and to the 300s poll cadence.
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
    // A too-small interval is clamped up to the safety floor.
    assert_eq!(
        parsed.sync.effective_auto_track_pr_poll_secs(),
        crate::MIN_AUTO_TRACK_PR_POLL_SECS
    );

    // Serialize → deserialize preserves the raw values.
    let round = serde_json::to_string(&parsed).unwrap();
    let reparsed: TraceDecayConfig = serde_json::from_str(&round).unwrap();
    assert_eq!(reparsed.sync, parsed.sync);
}

#[test]
fn parse_env_bool_shares_canonical_truthy_spellings() {
    for raw in ["1", "true", "TRUE", "yes", "on", " YES "] {
        assert_eq!(parse_env_bool(raw), Some(true), "{raw}");
    }
    for raw in ["0", "false", "FALSE"] {
        assert_eq!(parse_env_bool(raw), Some(false), "{raw}");
    }
    assert_eq!(parse_env_bool("maybe"), None);
}

#[test]
fn pr_autotrack_env_overrides() {
    let _lock = lock_user_data_dir_test_env();
    let _enable = EnvRestore::set("TRACEDECAY_SYNC_AUTO_TRACK_PR_BRANCHES", "true");
    let _poll = EnvRestore::set("TRACEDECAY_SYNC_AUTO_TRACK_PR_POLL_SECS", "120");

    let overridden = crate::SyncConfig::default().with_env_overrides();
    assert!(overridden.auto_track_pr_branches);
    assert_eq!(overridden.auto_track_pr_poll_secs, 120);
}

#[test]
fn sync_config_env_overrides_bool_and_int() {
    let _lock = lock_user_data_dir_test_env();
    let _watch = EnvRestore::set("TRACEDECAY_SYNC_AUTO_WATCH", "false");
    let _linked = EnvRestore::set("TRACEDECAY_SYNC_WATCH_LINKED_WORKTREES", "true");
    let _debounce = EnvRestore::set("TRACEDECAY_SYNC_WATCH_DEBOUNCE_MS", "5000");
    // Unparsable ints/bools are ignored (field keeps its base value).
    let _bad = EnvRestore::set("TRACEDECAY_SYNC_MAX_CONCURRENT_SYNCS", "not-a-number");

    let overridden = crate::SyncConfig::default().with_env_overrides();
    assert!(!overridden.auto_watch);
    assert!(overridden.watch_linked_worktrees);
    assert_eq!(overridden.watch_debounce_ms, 5000);
    assert_eq!(
        overridden.max_concurrent_syncs,
        crate::SyncConfig::default().max_concurrent_syncs
    );
}

#[test]
fn implicit_discovery_never_selects_the_user_profile_root() {
    let _profile = PinnedUserDataDir::new();
    let home = PathBuf::from(std::env::var_os("HOME").expect("pinned HOME"));
    fs::write(get_project_db_path(&home), b"").expect("ambient project marker");
    let nested = home.join("unrelated/nested");
    fs::create_dir_all(&nested).expect("nested directory");

    assert!(is_ambient_project_root(&home));
    assert_eq!(discover_project_root(&nested), None);
}

// ---------------------------------------------------------------------------
// Shared generated/vendored segment list
//
// GENERATED_DIR_SEGMENTS unifies what used to be four independently
// hand-maintained lists: this module's own DEFAULT_EXCLUDE_PATTERNS,
// tracedecay::scan's is_skipped_dir_hint, migrate::inventory's
// should_prune_dir, and mcp::tools::handlers::redundancy's
// is_generated_path. These tests pin the union those four call sites need
// and spot-check that segments unique to one of the formerly-separate lists
// are now recognized everywhere.
// ---------------------------------------------------------------------------

#[test]
fn generated_dir_segments_cover_the_union_all_call_sites_need() {
    // Formerly scan.rs-only (its HINTABLE_DIRS list).
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
    // Formerly migrate::inventory-only addition beyond the scan.rs set.
    assert!(GENERATED_DIR_SEGMENTS.contains(&"target"));
    // Formerly redundancy.rs-only addition beyond the scan.rs set.
    assert!(GENERATED_DIR_SEGMENTS.contains(&".worktrees"));
    // `.git` is intentionally NOT part of the shared list — it stays a
    // site-local addition in migrate::inventory::should_prune_dir (see its
    // doc comment) because it's VCS metadata, not generated/vendored code.
    assert!(!GENERATED_DIR_SEGMENTS.contains(&".git"));
}

#[test]
fn is_generated_dir_segment_delegates_for_segments_unique_to_one_former_list() {
    // Every one of these previously lived in only one of the four lists;
    // is_generated_dir_segment must now recognize all of them.
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
    // Regression guard for the DEFAULT_EXCLUDE_PATTERNS rebuild: target/**
    // previously had no **/target/** nested form (a real drift bug this
    // unification fixes), and .worktrees was never excluded by default at
    // all.
    let config = TraceDecayConfig::default();
    assert!(is_excluded("target/debug/build", &config));
    assert!(is_excluded("crates/sub/target/debug/build", &config));
    assert!(is_excluded(".worktrees/feature/src/lib.rs", &config));
    // Site-local additions (not part of GENERATED_DIR_SEGMENTS) still work.
    assert!(is_excluded(".git/HEAD", &config));
    assert!(is_excluded(".tracedecay/tracedecay.db", &config));
    assert!(is_excluded("bin/cli.js", &config));
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod retention_config_tests {
    use crate::{RetentionConfig, SyncConfig};
    use tracedecay_contracts::storage::compaction::CompactionThresholdConfig;

    #[test]
    fn default_retention_runs_only_safe_bounded_maintenance() {
        let retention = RetentionConfig::default();
        assert!(
            retention.session_lcm.enabled,
            "projection-durable session dedupe enabled by default"
        );
        assert_eq!(retention.session_lcm.offload_after_days, Some(30));
        assert_eq!(retention.session_lcm.drop_after_days, Some(180));
        assert_eq!(retention.session_lcm.dedupe_projected_after_days, Some(30));
        assert_eq!(retention.session_lcm.max_batch_size, 500);
        assert!(
            retention.observation.enabled,
            "released observation evidence maintenance is active by default"
        );
        assert_eq!(retention.observation.anchor_release_after_days, Some(30));
        assert_eq!(
            retention.observation.observation_release_after_days,
            Some(30)
        );
        assert_eq!(
            retention.observation.provenance_release_after_days,
            Some(30)
        );
        assert_eq!(retention.orphan_store_gc_days, Some(30));
        assert_eq!(retention.incident_debris_retention_days, Some(30));
        let compaction = retention.compaction.expect("compaction enabled");
        assert!((compaction.free_page_ratio_threshold - 0.25).abs() < f64::EPSILON);
        assert_eq!(compaction.minimum_reclaimable_bytes, 64 * 1024 * 1024);
        assert_eq!(compaction.max_pages_per_tick, 1024);
        assert_eq!(compaction, CompactionThresholdConfig::default());
        assert!(retention.store_soft_budgets_bytes.is_empty());
        // A default SyncConfig carries the same bounded retention tree.
        assert_eq!(SyncConfig::default().retention, retention);
    }

    #[test]
    fn empty_json_object_deserializes_to_safe_defaults() {
        // A serde-compat empty object (older config with no retention block)
        // must resolve the same safe maintenance policy.
        let retention: RetentionConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(retention, RetentionConfig::default());

        let nested: RetentionConfig =
            serde_json::from_str(r#"{"session_lcm":{},"observation":{}}"#).unwrap();
        assert_eq!(nested, RetentionConfig::default());
        assert!(nested.observation.reclaim_superseded_cursor_advances);
    }

    #[test]
    fn retention_rejects_immediate_collection_and_invalid_compaction_ratio() {
        let retention = RetentionConfig {
            orphan_store_gc_days: Some(0),
            ..RetentionConfig::default()
        };
        assert!(retention.validate().is_err());

        let retention = RetentionConfig {
            incident_debris_retention_days: Some(0),
            ..RetentionConfig::default()
        };
        assert!(retention.validate().is_err());

        let mut retention = RetentionConfig::default();
        retention
            .compaction
            .as_mut()
            .expect("default compaction")
            .free_page_ratio_threshold = 1.01;
        assert!(retention.validate().is_err());
    }

    #[test]
    fn retention_config_json_round_trips_with_windows_set() {
        let json = r#"{
            "session_lcm": { "enabled": true, "drop_after_days": 30 },
            "observation": { "enabled": true, "anchor_release_after_days": 45 },
            "orphan_store_gc_days": 14,
            "incident_debris_retention_days": 21,
            "compaction": { "free_page_ratio_threshold": 0.25, "minimum_reclaimable_bytes": 1000000 },
            "store_soft_budgets_bytes": { "sessions.db": 2000000000 },
            "interval_hours": 12
        }"#;
        let retention: RetentionConfig = serde_json::from_str(json).unwrap();
        assert!(retention.session_lcm.enabled);
        assert_eq!(retention.session_lcm.drop_after_days, Some(30));
        assert!(retention.observation.enabled);
        assert_eq!(retention.observation.anchor_release_after_days, Some(45));
        assert_eq!(retention.orphan_store_gc_days, Some(14));
        assert_eq!(retention.incident_debris_retention_days, Some(21));
        assert_eq!(retention.interval_hours, 12);
        let compaction = retention.compaction.expect("compaction configured");
        assert!((compaction.free_page_ratio_threshold - 0.25).abs() < f64::EPSILON);
        assert_eq!(compaction.minimum_reclaimable_bytes, 1_000_000);
        assert_eq!(
            retention.store_soft_budgets_bytes.get("sessions.db"),
            Some(&2_000_000_000)
        );

        // Re-serialize and re-parse: the tree is stable across a round trip.
        let reserialized = serde_json::to_string(&retention).unwrap();
        let reparsed: RetentionConfig = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(retention, reparsed);
    }
}
