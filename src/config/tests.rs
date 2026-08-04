use super::{
    GENERATED_DIR_SEGMENTS, TraceDecayConfig, USER_DATA_DIR_ENV, db_filename, get_project_db_path,
    get_tracedecay_dir, is_excluded, is_excluded_dir, is_generated_dir_segment,
    is_generated_path_segment, is_ignored_by_explicit_global_excludes, is_ignored_by_git,
    is_included, lock_user_data_dir_test_env, user_data_dir,
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
    assert!(parsed.sync.auto_init);
}

#[test]
fn default_configuration_does_not_advertise_retired_remote_brain_settings() {
    let rendered = serde_json::to_string(&TraceDecayConfig::default())
        .expect("serialize default configuration")
        .to_lowercase();

    for retired_setting in [
        "remote_brain",
        "remote_authority",
        "remote_enrollment",
        "remote_replay",
        "remote_recovery",
    ] {
        assert!(
            !rendered.contains(retired_setting),
            "default configuration advertised retired {retired_setting:?}: {rendered}"
        );
    }
}

#[test]
fn semantic_config_defaults_to_offline_healthy_baseline() {
    let config = TraceDecayConfig::default();
    assert_eq!(config.semantic, super::SemanticConfig::default());
    assert_eq!(
        config.semantic.selected_model.as_deref(),
        Some(super::DEFAULT_FASTEMBED_MODEL_ID)
    );
    assert!(config.semantic.auto_download);
    assert!(config.semantic.active_profile.is_none());
    assert!(config.semantic.rollback_profile.is_none());
    assert!(config.semantic.validate().is_ok());
    let catalog = crate::semantic_code::production_fastembed_catalog();
    let model = catalog
        .get(super::DEFAULT_FASTEMBED_MODEL_ID)
        .expect("default semantic model is cataloged");
    let model_bytes = model.members.get("model").expect("model member").length;
    assert!(config.semantic.resources.max_model_bytes >= model_bytes);
    assert!(config.semantic.resources.max_resident_bytes >= model_bytes.saturating_mul(2));
    // Concurrent sessions are host-derived sizing (the serving reservation
    // divided by the pinned intra-op width), not a fixed constant. Only the
    // floor is a contract: every host embeds with at least one session.
    assert_eq!(
        config.semantic.resources.max_concurrent_sessions,
        tracedecay_semantic::embedding_parallelism::default_max_concurrent_sessions(),
    );
    assert!(config.semantic.resources.max_concurrent_sessions >= 1);

    let json = serde_json::to_string(&config).unwrap();
    let parsed: TraceDecayConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.semantic, config.semantic);
}

#[test]
fn semantic_config_rejects_uncataloged_model_ids() {
    let mut semantic = super::SemanticConfig {
        selected_model: Some("NotInCatalog".to_owned()),
        ..Default::default()
    };
    assert!(semantic.validate().is_err());
    semantic.selected_model = None;
    assert!(semantic.validate().is_ok());
}

#[test]
fn semantic_config_accepts_only_explicit_local_installed_profiles() {
    let local = super::SemanticProfileSelection {
        profile_id: "code-embedding.v1".to_owned(),
        accepted_profile_digest: tracedecay_domain::ManifestDigest::new(format!(
            "sha256:{}",
            "1".repeat(64)
        ))
        .unwrap(),
        artifact_digest: "a".repeat(64),
        artifact_path: std::path::PathBuf::from("/var/lib/tracedecay/models/code-embedding"),
    };
    let mut semantic = super::SemanticConfig {
        active_profile: Some(local.clone()),
        rollback_profile: Some(super::SemanticProfileSelection {
            profile_id: "code-embedding.previous".to_owned(),
            accepted_profile_digest: tracedecay_domain::ManifestDigest::new(format!(
                "sha256:{}",
                "2".repeat(64)
            ))
            .unwrap(),
            artifact_digest: "b".repeat(64),
            artifact_path: std::path::PathBuf::from(
                "/var/lib/tracedecay/models/code-embedding-previous",
            ),
        }),
        ..super::SemanticConfig::default()
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
    let mut semantic = super::SemanticConfig::default();
    semantic.resources.max_threads = 0;
    assert!(semantic.validate().is_err());

    semantic = super::SemanticConfig::default();
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
        super::MIN_AUTO_TRACK_PR_POLL_SECS
    );

    // Serialize → deserialize preserves the raw values.
    let round = serde_json::to_string(&parsed).unwrap();
    let reparsed: TraceDecayConfig = serde_json::from_str(&round).unwrap();
    assert_eq!(reparsed.sync, parsed.sync);
}

#[test]
fn pr_autotrack_env_overrides() {
    let _lock = lock_user_data_dir_test_env();
    let _enable = EnvRestore::set("TRACEDECAY_SYNC_AUTO_TRACK_PR_BRANCHES", "true");
    let _poll = EnvRestore::set("TRACEDECAY_SYNC_AUTO_TRACK_PR_POLL_SECS", "120");

    let overridden = super::SyncConfig::default().with_env_overrides();
    assert!(overridden.auto_track_pr_branches);
    assert_eq!(overridden.auto_track_pr_poll_secs, 120);
}

#[test]
fn sync_config_env_overrides_bool_and_int() {
    let _lock = lock_user_data_dir_test_env();
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

#[tokio::test]
async fn discover_project_root_with_identity_does_not_open_registry_only_store() {
    let _profile = super::PinnedUserDataDir::new();
    let profile_root = crate::storage::default_profile_root().unwrap();

    let gdb =
        crate::application::host_admission::HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .unwrap();

    let project_dir = TempDir::new().unwrap();
    let project_root = project_dir.path().canonicalize().unwrap();

    let project_id = "proj_identity_only";
    gdb.upsert_code_project(project_id, &project_root, None, None, None)
        .await
        .unwrap();
    gdb.upsert_store_instance(crate::global_db::StoreInstanceUpsert {
        store_id: "store_identity_only".to_string(),
        project_id: project_id.to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: format!("projects/{project_id}"),
        manifest_relpath: Some(format!("projects/{project_id}/store_manifest.json")),
        last_verified_at: Some(100),
        last_write_at: Some(101),
    })
    .await
    .unwrap();

    let layout = crate::storage::profile_sharded_layout(
        &project_root,
        &profile_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.to_string(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    fs::create_dir_all(layout.graph_db_path.parent().unwrap()).unwrap();
    fs::write(&layout.graph_db_path, b"").unwrap();

    let status = Command::new("git")
        .arg("init")
        .arg(&project_root)
        .status()
        .unwrap();
    assert!(status.success(), "git init failed");

    assert!(
        super::discover_project_root(&project_root).is_none(),
        "sync discover_project_root must not see a global-only store"
    );

    assert!(
        super::discover_project_root_with_identity(&project_root)
            .await
            .is_none(),
        "process-local discovery must leave registry-only aliases to the daemon"
    );
    let nested = project_root.join("crates/inner");
    fs::create_dir_all(&nested).unwrap();
    assert!(
        super::discover_project_root_with_identity(&nested)
            .await
            .is_none(),
        "nested discovery must not open the global registry"
    );

    let bare = TempDir::new().unwrap();
    let bare_root = bare.path().canonicalize().unwrap();
    assert!(
        super::discover_project_root_with_identity(&bare_root)
            .await
            .is_none(),
        "a directory with no store must not resolve"
    );
}

#[tokio::test]
async fn config_path_with_identity_does_not_open_registry_without_enrollment() {
    let _profile = super::PinnedUserDataDir::new();
    let profile_root = crate::storage::default_profile_root().unwrap();
    let gdb =
        crate::application::host_admission::HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .unwrap();

    let project_dir = TempDir::new().unwrap();
    let project_root = project_dir.path().canonicalize().unwrap();
    let status = Command::new("git")
        .arg("init")
        .arg(&project_root)
        .status()
        .unwrap();
    assert!(status.success(), "git init failed");

    let project_id = "proj_config_identity";
    let git_common_dir = crate::worktree::git_common_dir(&project_root);
    gdb.upsert_code_project(
        project_id,
        &project_root,
        git_common_dir.as_deref(),
        None,
        None,
    )
    .await
    .unwrap();
    gdb.upsert_store_instance(crate::global_db::StoreInstanceUpsert {
        store_id: "store_config_identity".to_string(),
        project_id: project_id.to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: format!("projects/{project_id}"),
        manifest_relpath: Some(format!("projects/{project_id}/store_manifest.json")),
        last_verified_at: Some(100),
        last_write_at: Some(101),
    })
    .await
    .unwrap();
    let identity_layout = crate::storage::profile_sharded_layout(
        &project_root,
        &profile_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.to_string(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    super::save_config_to_path(
        &identity_layout.config_path,
        &TraceDecayConfig {
            root_dir: "identity-config".to_string(),
            ..TraceDecayConfig::default()
        },
    )
    .unwrap();

    assert_eq!(
        super::get_config_path_with_identity(&project_root).await,
        super::get_config_path(&project_root)
    );
    assert_eq!(
        super::load_config_with_identity(&project_root)
            .await
            .unwrap()
            .root_dir,
        project_root.to_string_lossy()
    );
}

#[tokio::test]
async fn discover_project_root_with_identity_does_not_bind_non_git_child_to_parent_store() {
    let _profile = super::PinnedUserDataDir::new();
    let profile_root = crate::storage::default_profile_root().unwrap();
    let gdb =
        crate::application::host_admission::HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .unwrap();

    let parent_dir = TempDir::new().unwrap();
    let parent_root = parent_dir.path().canonicalize().unwrap();
    let project_id = "proj_parent_identity_only";
    gdb.upsert_code_project(project_id, &parent_root, None, None, None)
        .await
        .unwrap();
    gdb.upsert_store_instance(crate::global_db::StoreInstanceUpsert {
        store_id: "store_parent_identity_only".to_string(),
        project_id: project_id.to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: format!("projects/{project_id}"),
        manifest_relpath: Some(format!("projects/{project_id}/store_manifest.json")),
        last_verified_at: Some(100),
        last_write_at: Some(101),
    })
    .await
    .unwrap();
    let layout = crate::storage::profile_sharded_layout(
        &parent_root,
        &profile_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.to_string(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    fs::create_dir_all(layout.graph_db_path.parent().unwrap()).unwrap();
    fs::write(&layout.graph_db_path, b"").unwrap();

    let child = parent_root.join("scratch/deep");
    fs::create_dir_all(&child).unwrap();

    assert_eq!(
        super::discover_project_root_with_identity(&child).await,
        None,
        "non-git scratch directories must not inherit initialized parent stores"
    );
}

#[tokio::test]
async fn discover_project_root_with_identity_preserves_sync_fast_path() {
    let _profile = super::PinnedUserDataDir::new();
    let project_dir = TempDir::new().unwrap();
    let project_root = project_dir.path().canonicalize().unwrap();

    let db_dir = super::get_tracedecay_dir(&project_root);
    fs::create_dir_all(&db_dir).unwrap();
    fs::write(super::get_project_db_path(&project_root), b"").unwrap();

    let sync = super::discover_project_root(&project_root);
    assert!(sync.is_some(), "sync resolver must see a repo-local db");
    assert_eq!(
        super::discover_project_root_with_identity(&project_root).await,
        sync,
        "identity wrapper fast path must equal the sync result"
    );
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

// ---------------------------------------------------------------------------
// PR11 topology policy resolution
//
// The sole resolver produces the pinned snapshot and src/config/topology.rs
// extracts its one complete work-topology policy, failing closed on every
// invalid or unsupported combination without adapter-local defaults.
// ---------------------------------------------------------------------------

mod topology_resolution {
    use std::collections::BTreeMap;

    use tracedecay_domain::configuration::{
        BranchTopologyKindV1, ConfigurationLayerIdV1, ConfigurationSnapshotV1,
        ConfigurationValueKindV1, ConfigurationValueV1, RestartRequirementV1, SettingKey,
        SettingScopeV1, SettingSensitivityV1, WORK_TOPOLOGY_POLICY_SETTING_KEY,
        WorkTopologyPolicyV1, safe_work_topology_policy_v1,
    };
    use tracedecay_domain::{ManifestDigest, ProjectId, UserProfileId};

    use crate::config::registry::ConfigurationRegistry;
    use crate::config::resolver::{ConfigurationLayerV1, resolve_configuration};
    use crate::config::topology::{
        TopologyConfigurationError, resolved_work_topology_policy,
        safe_default_work_topology_policy,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("fixture id is canonical")
    }

    fn topology_key() -> SettingKey {
        SettingKey::new(WORK_TOPOLOGY_POLICY_SETTING_KEY).unwrap()
    }

    fn project_layer(policy: WorkTopologyPolicyV1) -> ConfigurationLayerV1 {
        ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Project {
                project_id: id::<ProjectId>("project.fixture"),
            },
            revision_id: id("revision.project.1"),
            entries: BTreeMap::from([(
                topology_key(),
                ConfigurationValueV1::WorkTopologyPolicy(Box::new(policy)),
            )]),
        }
    }

    #[test]
    fn registry_default_is_the_domain_safe_default() {
        let registry = ConfigurationRegistry::core().unwrap();
        let definition = registry.definition(&topology_key()).unwrap();
        assert_eq!(
            definition.value_kind,
            ConfigurationValueKindV1::WorkTopologyPolicy
        );
        assert_eq!(definition.sensitivity, SettingSensitivityV1::Sensitive);
        assert_eq!(definition.scope, SettingScopeV1::Project);
        assert_eq!(
            definition.restart_requirement,
            RestartRequirementV1::DaemonRestart
        );
        let ConfigurationValueV1::WorkTopologyPolicy(default) = &definition.default_value else {
            panic!("registry default must be a typed topology policy");
        };
        let safe = safe_work_topology_policy_v1();
        assert_eq!(**default, safe);
        assert_eq!(
            default.compute_digest().unwrap(),
            safe.compute_digest().unwrap()
        );
    }

    #[test]
    fn resolves_safe_default_when_no_layer_overrides() {
        let registry = ConfigurationRegistry::core().unwrap();
        let snapshot = resolve_configuration(&registry, &[]).unwrap().snapshot;
        let resolved = resolved_work_topology_policy(&snapshot).unwrap();
        let safe = safe_default_work_topology_policy();
        assert_eq!(*resolved, safe);
        assert_eq!(
            resolved.compute_digest().unwrap(),
            safe.compute_digest().unwrap()
        );
    }

    #[test]
    fn project_layer_override_wins_with_its_own_digest() {
        let registry = ConfigurationRegistry::core().unwrap();
        let mut replacement = safe_work_topology_policy_v1();
        replacement
            .branch_topology
            .allowed
            .insert(BranchTopologyKindV1::LocalStack);
        replacement.validate().unwrap();

        let resolution =
            resolve_configuration(&registry, &[project_layer(replacement.clone())]).unwrap();
        let resolved = resolved_work_topology_policy(&resolution.snapshot).unwrap();
        assert_eq!(*resolved, replacement);
        assert_ne!(*resolved, safe_work_topology_policy_v1());
        assert_eq!(
            resolved.compute_digest().unwrap(),
            replacement.compute_digest().unwrap()
        );

        // The behavior digest changes with the override even though the
        // resolution path is identical.
        let baseline = resolve_configuration(&registry, &[]).unwrap();
        let moved = resolve_configuration(&registry, &[project_layer(replacement)]).unwrap();
        assert_ne!(
            baseline.snapshot.effective_behavior_digest,
            moved.snapshot.effective_behavior_digest
        );
    }

    #[test]
    fn user_profile_layer_cannot_override_project_scoped_topology() {
        let registry = ConfigurationRegistry::core().unwrap();
        let layer = ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::UserProfile {
                profile_id: id::<UserProfileId>("profile.fixture"),
            },
            revision_id: id("revision.profile.1"),
            entries: BTreeMap::from([(
                topology_key(),
                ConfigurationValueV1::WorkTopologyPolicy(Box::new(safe_work_topology_policy_v1())),
            )]),
        };
        assert!(resolve_configuration(&registry, &[layer]).is_err());
    }

    #[test]
    fn reserved_default_layer_injection_is_rejected() {
        let registry = ConfigurationRegistry::core().unwrap();
        let layer = ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Default,
            revision_id: id("revision.adapter.default"),
            entries: BTreeMap::from([(
                topology_key(),
                ConfigurationValueV1::WorkTopologyPolicy(Box::new(safe_work_topology_policy_v1())),
            )]),
        };
        assert!(resolve_configuration(&registry, &[layer]).is_err());
    }

    #[test]
    fn wrong_value_kind_fails_closed() {
        let registry = ConfigurationRegistry::core().unwrap();
        let layer = ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Project {
                project_id: id::<ProjectId>("project.fixture"),
            },
            revision_id: id("revision.project.1"),
            entries: BTreeMap::from([(
                topology_key(),
                ConfigurationValueV1::Text("permissive".to_owned()),
            )]),
        };
        assert!(resolve_configuration(&registry, &[layer]).is_err());
    }

    #[test]
    fn invalid_or_unsupported_policy_in_layer_fails_closed() {
        let registry = ConfigurationRegistry::core().unwrap();

        // No protected-ref rules at all.
        let mut unprotected = safe_work_topology_policy_v1();
        unprotected.protected_refs.clear();
        assert!(resolve_configuration(&registry, &[project_layer(unprotected)]).is_err());

        // Unsupported schema version.
        let mut future = safe_work_topology_policy_v1();
        future.schema_version = 2;
        assert!(resolve_configuration(&registry, &[project_layer(future)]).is_err());
    }

    #[test]
    fn snapshot_resolution_requires_the_typed_policy_value() {
        // Missing key fails closed rather than inventing a default.
        let empty = ConfigurationSnapshotV1::new(BTreeMap::new(), BTreeMap::new()).unwrap();
        assert!(matches!(
            resolved_work_topology_policy(&empty),
            Err(TopologyConfigurationError::MissingTopologyPolicy)
        ));

        // A mistyped value at the topology key fails closed.
        let registry = ConfigurationRegistry::core().unwrap();
        let default_candidate = resolve_configuration(&registry, &[])
            .unwrap()
            .settings
            .get(&topology_key())
            .unwrap()
            .candidates
            .first()
            .unwrap()
            .clone();
        let mistyped = ConfigurationSnapshotV1::new(
            BTreeMap::from([(
                topology_key(),
                ConfigurationValueV1::Text("permissive".to_owned()),
            )]),
            BTreeMap::from([(topology_key(), vec![default_candidate])]),
        )
        .unwrap();
        assert!(matches!(
            resolved_work_topology_policy(&mistyped),
            Err(TopologyConfigurationError::WrongTopologyValue)
        ));

        // A tampered snapshot identity fails closed before the value is read.
        let mut snapshot = resolve_configuration(&registry, &[]).unwrap().snapshot;
        snapshot.effective_behavior_digest =
            ManifestDigest::new(format!("sha256:{}", "0".repeat(64))).unwrap();
        assert!(matches!(
            resolved_work_topology_policy(&snapshot),
            Err(TopologyConfigurationError::Domain(_))
        ));
    }
}

// ---------------------------------------------------------------------------
// PR11 legacy configuration migration input
//
// The decoder is read-only: it receives raw JSON and an explicit environment
// map, builds typed/redacted inputs, and lets the sole resolver apply the
// documented source order. Runtime adapters consume published snapshots;
// legacy reads remain migration/diagnostic inputs rather than a write path.
// ---------------------------------------------------------------------------

mod legacy_configuration_migration_input {
    use std::collections::BTreeMap;

    use tracedecay_domain::ProjectId;
    use tracedecay_domain::configuration::{
        ConfigurationLayerIdV1, ConfigurationValueKindV1, ConfigurationValueV1,
        DIAGNOSTICS_PREWARM_SETTING_KEY, INDEX_EXCLUDE_SETTING_KEY, INDEX_INCLUDE_SETTING_KEY,
        INDEX_MAX_FILE_SIZE_SETTING_KEY, LEGACY_CONFIG_JSON_SETTING_KEYS_V1,
        SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY, SYNC_AUTO_WATCH_SETTING_KEY, SettingKey,
        SettingScopeV1,
    };

    use crate::config::registry::ConfigurationRegistry;
    use crate::config::resolver::resolve_configuration;
    use crate::config::{
        LegacyConfigurationDecodeTargetV1, decode_legacy_config_json,
        decode_legacy_configuration_inputs, decode_legacy_environment_overrides,
        resolve_legacy_configuration_inputs,
    };
    use crate::global_db::configuration::migration::{
        ConfigurationMigrationQuarantineReasonV1, ReadonlyLegacyConfigurationInputsV1,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("fixture id is canonical")
    }

    fn target() -> LegacyConfigurationDecodeTargetV1 {
        LegacyConfigurationDecodeTargetV1 {
            target_layer: ConfigurationLayerIdV1::Project {
                project_id: id::<ProjectId>("project.legacy-config"),
            },
            target_revision_id: id("revision.legacy-config"),
        }
    }

    fn legacy_values(
        config: &super::TraceDecayConfig,
    ) -> BTreeMap<SettingKey, ConfigurationValueV1> {
        let sync = &config.sync;
        BTreeMap::from([
            (
                SettingKey::new(INDEX_EXCLUDE_SETTING_KEY).unwrap(),
                ConfigurationValueV1::StringList(config.exclude.clone()),
            ),
            (
                SettingKey::new(INDEX_INCLUDE_SETTING_KEY).unwrap(),
                ConfigurationValueV1::StringList(config.include.clone()),
            ),
            (
                SettingKey::new(INDEX_MAX_FILE_SIZE_SETTING_KEY).unwrap(),
                ConfigurationValueV1::Unsigned(config.max_file_size),
            ),
            (
                SettingKey::new("index.extract_docstrings.v1").unwrap(),
                ConfigurationValueV1::Boolean(config.extract_docstrings),
            ),
            (
                SettingKey::new("index.track_call_sites.v1").unwrap(),
                ConfigurationValueV1::Boolean(config.track_call_sites),
            ),
            (
                SettingKey::new("index.git_ignore.v1").unwrap(),
                ConfigurationValueV1::Boolean(config.git_ignore),
            ),
            (
                SettingKey::new(DIAGNOSTICS_PREWARM_SETTING_KEY).unwrap(),
                ConfigurationValueV1::Boolean(config.diagnostics_prewarm),
            ),
            (
                SettingKey::new("sync.auto_watch.v1").unwrap(),
                ConfigurationValueV1::Boolean(sync.auto_watch),
            ),
            (
                SettingKey::new("sync.watch_debounce_ms.v1").unwrap(),
                ConfigurationValueV1::Unsigned(sync.watch_debounce_ms),
            ),
            (
                SettingKey::new("sync.watch_max_delay_ms.v1").unwrap(),
                ConfigurationValueV1::Unsigned(sync.watch_max_delay_ms),
            ),
            (
                SettingKey::new("sync.watch_max_projects.v1").unwrap(),
                ConfigurationValueV1::Unsigned(sync.watch_max_projects as u64),
            ),
            (
                SettingKey::new("sync.read_refresh.v1").unwrap(),
                ConfigurationValueV1::Boolean(sync.read_refresh),
            ),
            (
                SettingKey::new("sync.read_cooldown_secs.v1").unwrap(),
                ConfigurationValueV1::Unsigned(sync.read_cooldown_secs),
            ),
            (
                SettingKey::new("sync.session_start_sync.v1").unwrap(),
                ConfigurationValueV1::Boolean(sync.session_start_sync),
            ),
            (
                SettingKey::new("sync.session_start_stale_threshold_secs.v1").unwrap(),
                ConfigurationValueV1::Unsigned(sync.session_start_stale_threshold_secs),
            ),
            (
                SettingKey::new("sync.backstop_interval_mins.v1").unwrap(),
                ConfigurationValueV1::Unsigned(sync.backstop_interval_mins),
            ),
            (
                SettingKey::new("sync.full_sync_escalation_files.v1").unwrap(),
                ConfigurationValueV1::Unsigned(sync.full_sync_escalation_files as u64),
            ),
            (
                SettingKey::new("sync.max_concurrent_syncs.v1").unwrap(),
                ConfigurationValueV1::Unsigned(sync.max_concurrent_syncs as u64),
            ),
            (
                SettingKey::new("sync.branch_gc_days.v1").unwrap(),
                ConfigurationValueV1::Unsigned(sync.branch_gc_days),
            ),
            (
                SettingKey::new("sync.orphan_db_gc_days.v1").unwrap(),
                ConfigurationValueV1::Unsigned(sync.orphan_db_gc_days),
            ),
            (
                SettingKey::new("sync.auto_init.v1").unwrap(),
                ConfigurationValueV1::Boolean(sync.auto_init),
            ),
            (
                SettingKey::new("sync.auto_track_pr_branches.v1").unwrap(),
                ConfigurationValueV1::Boolean(sync.auto_track_pr_branches),
            ),
            (
                SettingKey::new(SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY).unwrap(),
                ConfigurationValueV1::Unsigned(
                    sync.auto_track_pr_poll_secs
                        .max(crate::config::MIN_AUTO_TRACK_PR_POLL_SECS),
                ),
            ),
            (
                SettingKey::new("telemetry.timings.v1").unwrap(),
                ConfigurationValueV1::Boolean(config.telemetry.timings),
            ),
        ])
    }

    #[test]
    fn registry_has_every_legacy_scalar_definition_with_project_scope() {
        let registry = ConfigurationRegistry::core().unwrap();
        let config = super::TraceDecayConfig::default();
        let values = legacy_values(&config);
        assert_eq!(values.len(), LEGACY_CONFIG_JSON_SETTING_KEYS_V1.len());

        for key in LEGACY_CONFIG_JSON_SETTING_KEYS_V1 {
            let key = SettingKey::new(*key).unwrap();
            let definition = registry.definition(&key).unwrap();
            assert_eq!(definition.scope, SettingScopeV1::Project);
            assert_eq!(definition.default_value, values[&key]);
        }
        assert_eq!(
            registry
                .definition(&SettingKey::new(INDEX_INCLUDE_SETTING_KEY).unwrap())
                .unwrap()
                .value_kind,
            ConfigurationValueKindV1::StringList
        );
        assert_eq!(
            registry
                .definition(&SettingKey::new(INDEX_MAX_FILE_SIZE_SETTING_KEY).unwrap())
                .unwrap()
                .value_kind,
            ConfigurationValueKindV1::Unsigned
        );
    }

    #[test]
    fn missing_legacy_fields_resolve_to_the_current_default_behavior_digest() {
        let registry = ConfigurationRegistry::core().unwrap();
        let inputs = decode_legacy_configuration_inputs("{}", &BTreeMap::new(), &target()).unwrap();
        let migrated = resolve_legacy_configuration_inputs(&registry, &inputs).unwrap();
        let baseline = resolve_configuration(&registry, &[]).unwrap();

        assert_eq!(
            migrated.snapshot.effective_behavior_digest,
            baseline.snapshot.effective_behavior_digest,
            "typed defaults must preserve the legacy behavior fixture"
        );
    }

    #[test]
    fn decoder_preserves_known_fields_and_quarantines_root_unknown_and_undecodable_values() {
        let raw = r#"{
            "root_dir": "/private/repo",
            "exclude": ["src/generated/**"],
            "max_file_size": "not-a-number",
            "sync": { "auto_watch": "not-a-bool", "future_sync": true },
            "telemetry": { "timings": "not-a-bool" },
            "future_top_level": 1
        }"#;
        let input = decode_legacy_config_json(raw, &target()).unwrap();
        let reasons: Vec<_> = input
            .entries
            .iter()
            .filter_map(|entry| entry.quarantine_reason)
            .collect();

        assert_eq!(
            input
                .entries
                .first()
                .and_then(|entry| entry.quarantine_reason),
            Some(ConfigurationMigrationQuarantineReasonV1::PathDerivedAuthority),
            "root_dir must never become authority"
        );
        assert!(reasons.contains(&ConfigurationMigrationQuarantineReasonV1::Undecodable));
        assert!(reasons.contains(&ConfigurationMigrationQuarantineReasonV1::UnknownKey));
        assert!(input.entries.iter().any(|entry| {
            entry
                .setting_key
                .as_ref()
                .is_some_and(|key| key.as_str() == INDEX_EXCLUDE_SETTING_KEY)
                && entry.value
                    == Some(ConfigurationValueV1::StringList(vec![
                        "src/generated/**".to_owned(),
                    ]))
        }));

        let serialized = serde_json::to_string(&input).unwrap();
        assert!(!serialized.contains("/private/repo"));
        assert!(!serialized.contains("root_dir\""));
    }

    #[test]
    fn environment_is_an_explicit_higher_precedence_resolution_input() {
        let raw = r#"{
            "diagnostics_prewarm": false,
            "sync": { "auto_watch": true }
        }"#;
        let environment = BTreeMap::from([
            (
                "TRACEDECAY_DIAGNOSTICS_PREWARM".to_owned(),
                "true".to_owned(),
            ),
            ("TRACEDECAY_SYNC_AUTO_WATCH".to_owned(), "false".to_owned()),
        ]);
        let registry = ConfigurationRegistry::core().unwrap();
        let inputs = decode_legacy_configuration_inputs(raw, &environment, &target()).unwrap();
        let resolution = resolve_legacy_configuration_inputs(&registry, &inputs).unwrap();

        assert_eq!(
            resolution
                .settings
                .get(&SettingKey::new(SYNC_AUTO_WATCH_SETTING_KEY).unwrap())
                .unwrap()
                .effective_value,
            ConfigurationValueV1::Boolean(false)
        );
        assert_eq!(
            resolution
                .settings
                .get(&SettingKey::new(DIAGNOSTICS_PREWARM_SETTING_KEY).unwrap())
                .unwrap()
                .effective_value,
            ConfigurationValueV1::Boolean(true)
        );

        let candidates = &resolution
            .settings
            .get(&SettingKey::new(SYNC_AUTO_WATCH_SETTING_KEY).unwrap())
            .unwrap()
            .candidates;
        assert_eq!(candidates.len(), 3);
        assert_eq!(
            candidates
                .last()
                .and_then(|candidate| candidate.safe_reason.as_deref()),
            Some("highest_valid_legacy_environment")
        );
        assert_eq!(
            candidates[1].safe_reason.as_deref(),
            Some("higher_precedence_legacy_environment")
        );
    }

    #[test]
    fn input_digests_are_idempotent_and_source_order_is_enforced() {
        let raw = r#"{"include":[".github/**"],"sync":{"auto_watch":false}}"#;
        let environment =
            BTreeMap::from([("TRACEDECAY_SYNC_AUTO_WATCH".to_owned(), "true".to_owned())]);
        let first = decode_legacy_configuration_inputs(raw, &environment, &target()).unwrap();
        let second = decode_legacy_configuration_inputs(raw, &environment, &target()).unwrap();
        let reordered = decode_legacy_configuration_inputs(
            r#"{"sync":{"auto_watch":false},"include":[".github/**"]}"#,
            &environment,
            &target(),
        )
        .unwrap();
        assert_eq!(
            first.snapshot_digest().unwrap(),
            second.snapshot_digest().unwrap()
        );
        assert_eq!(
            first.snapshot_digest().unwrap(),
            reordered.snapshot_digest().unwrap(),
            "JSON object ordering is not migration provenance"
        );
        assert_eq!(
            first.inputs[0].snapshot_digest().unwrap(),
            second.inputs[0].snapshot_digest().unwrap()
        );
        assert_eq!(
            first.inputs[1].snapshot_digest().unwrap(),
            second.inputs[1].snapshot_digest().unwrap()
        );

        let mut unordered = first.clone();
        unordered.inputs.swap(0, 1);
        assert!(unordered.validate().is_err());

        let malformed = decode_legacy_environment_overrides(
            &BTreeMap::from([
                (
                    "TRACEDECAY_SYNC_MAX_CONCURRENT_SYNCS".to_owned(),
                    "bad".to_owned(),
                ),
                ("TRACEDECAY_FUTURE_CONFIG".to_owned(), "1".to_owned()),
            ]),
            &target(),
        )
        .unwrap();
        assert!(malformed.entries.iter().all(|entry| entry.value.is_none()));
        assert!(malformed.entries.iter().any(|entry| {
            entry.quarantine_reason == Some(ConfigurationMigrationQuarantineReasonV1::Undecodable)
        }));
        assert!(malformed.entries.iter().any(|entry| {
            entry.quarantine_reason == Some(ConfigurationMigrationQuarantineReasonV1::UnknownKey)
        }));

        let empty = ReadonlyLegacyConfigurationInputsV1 { inputs: Vec::new() };
        assert!(empty.validate().is_err());
    }
}

mod runtime_configuration_cutover {
    use std::collections::BTreeMap;
    #[cfg(unix)]
    use std::process::Command;
    use std::sync::Mutex;

    use tempfile::TempDir;
    use tracedecay_domain::configuration::{
        ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationValueV1,
        SOURCE_BINDINGS_SETTING_KEY, SYNC_AUTO_WATCH_SETTING_KEY, SettingKey,
    };
    use tracedecay_domain::{ProjectId, UtcMicros};
    use tracedecay_usecases::config::{
        ConfigurationDaemonClient as UsecaseConfigurationDaemonClient,
        PinnedRuntimeConfiguration as UsecasePinnedRuntimeConfiguration,
        RuntimeConfigurationFuture as UsecaseRuntimeConfigurationFuture,
        RuntimeConfigurationTarget as UsecaseRuntimeConfigurationTarget,
    };

    use crate::application::configuration::{
        ConfigurationControlStore, DirectConfigurationMutation,
    };
    use crate::application::host_admission::HostAdmissionTestRuntimeV1;
    use crate::config::registry::ConfigurationRegistry;
    use crate::config::resolver::{ConfigurationLayerV1, resolve_configuration};
    use crate::config::{
        LegacyConfigurationDecodeTargetV1, decode_legacy_configuration_inputs,
        resolve_legacy_configuration_inputs,
    };
    use crate::config::{
        PinnedRuntimeConfiguration, RuntimeConfigurationCache, RuntimeConfigurationTarget,
        TraceDecayConfig, cached_runtime_configuration, cached_sync_config,
        cached_telemetry_config, commit_runtime_configuration_mutation,
        direct_mutation_for_runtime_config_diff, install_pinned_runtime_configuration,
        mutate_pinned_runtime_configuration, runtime_configuration_for_layout,
    };

    fn project_id(value: &str) -> ProjectId {
        ProjectId::new(value.to_owned()).expect("fixture project id is canonical")
    }

    fn revision_id(value: &str) -> ConfigurationRevisionId {
        ConfigurationRevisionId::new(value).expect("fixture revision id is canonical")
    }

    struct RecordingDaemonClient {
        next: UsecasePinnedRuntimeConfiguration,
        calls: Mutex<
            Vec<(
                UsecaseRuntimeConfigurationTarget,
                DirectConfigurationMutation,
                ConfigurationRevisionId,
            )>,
        >,
    }

    impl UsecaseConfigurationDaemonClient for RecordingDaemonClient {
        fn mutate_direct(
            &self,
            target: UsecaseRuntimeConfigurationTarget,
            mutation: DirectConfigurationMutation,
            expected_revision: ConfigurationRevisionId,
        ) -> UsecaseRuntimeConfigurationFuture<'_, UsecasePinnedRuntimeConfiguration> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((target, mutation, expected_revision));
            let next = self.next.clone();
            Box::pin(async move { Ok(next) })
        }
    }

    #[test]
    fn pinned_runtime_materialization_preserves_explicit_environment_precedence() {
        let project_id = project_id("project.runtime-env-precedence");
        let revision_id = revision_id("revision.runtime-env-precedence");
        let target = LegacyConfigurationDecodeTargetV1 {
            target_layer: ConfigurationLayerIdV1::Project {
                project_id: project_id.clone(),
            },
            target_revision_id: revision_id.clone(),
        };
        let inputs = decode_legacy_configuration_inputs(
            r#"{
                "root_dir": "/untrusted/legacy-root",
                "diagnostics_prewarm": false,
                "sync": { "auto_watch": true }
            }"#,
            &BTreeMap::from([
                (
                    "TRACEDECAY_DIAGNOSTICS_PREWARM".to_owned(),
                    "true".to_owned(),
                ),
                ("TRACEDECAY_SYNC_AUTO_WATCH".to_owned(), "false".to_owned()),
            ]),
            &target,
        )
        .expect("legacy input decodes");
        let resolution = resolve_legacy_configuration_inputs(
            &ConfigurationRegistry::core().expect("registry is available"),
            &inputs,
        )
        .expect("explicit environment layer resolves");
        let root = TempDir::new().expect("temporary project root");
        let pinned = PinnedRuntimeConfiguration::new(
            RuntimeConfigurationTarget {
                project_id: project_id.clone(),
                project_root: root.path().to_path_buf(),
            },
            revision_id,
            resolution.snapshot,
        )
        .expect("resolved snapshot materializes");

        assert_eq!(pinned.target.project_id, project_id);
        assert_eq!(
            pinned.config.root_dir,
            root.path().to_string_lossy().to_string()
        );
        assert_ne!(pinned.config.root_dir, "/untrusted/legacy-root");
        assert!(pinned.config.diagnostics_prewarm);
        assert!(!pinned.config.sync.auto_watch);
    }

    #[test]
    fn runtime_configuration_diff_is_typed_and_rejects_legacy_metadata() {
        let project_id = project_id("project.runtime-mutation");
        let before = TraceDecayConfig::default();
        let mut after = before.clone();
        after.git_ignore = false;
        after.sync.auto_watch = false;

        let mutation = direct_mutation_for_runtime_config_diff(&project_id, &before, &after)
            .expect("runtime fields have typed settings")
            .expect("changed settings require a mutation");
        let DirectConfigurationMutation::Batch { mutations } = mutation else {
            panic!("runtime configuration changes must be batched typed mutations");
        };
        assert_eq!(mutations.len(), 2);
        assert!(mutations.iter().any(|mutation| {
            matches!(
                mutation,
                DirectConfigurationMutation::Set {
                    layer: ConfigurationLayerIdV1::Project { project_id: target },
                    key,
                    value: ConfigurationValueV1::Boolean(false),
                } if target == &project_id && key.as_str() == "index.git_ignore.v1"
            )
        }));
        assert!(mutations.iter().any(|mutation| {
            matches!(
                mutation,
                DirectConfigurationMutation::Set {
                    layer: ConfigurationLayerIdV1::Project { project_id: target },
                    key,
                    value: ConfigurationValueV1::Boolean(false),
                } if target == &project_id && key.as_str() == SYNC_AUTO_WATCH_SETTING_KEY
            )
        }));

        let mut metadata_change = before;
        metadata_change.root_dir = "/not-an-authority".to_owned();
        assert!(
            direct_mutation_for_runtime_config_diff(
                &project_id,
                &TraceDecayConfig::default(),
                &metadata_change
            )
            .is_err(),
            "root_dir is migration metadata and cannot enter the control plane"
        );
    }

    #[test]
    fn semantic_runtime_selection_changes_atomically_as_one_typed_setting() {
        let project_id = project_id("project.semantic-runtime");
        let before = TraceDecayConfig::default();
        let mut after = before.clone();
        after.semantic.active_profile = Some(crate::config::SemanticProfileSelection {
            profile_id: "code-embedding.v1".to_owned(),
            accepted_profile_digest: tracedecay_domain::ManifestDigest::new(format!(
                "sha256:{}",
                "1".repeat(64)
            ))
            .unwrap(),
            artifact_digest: "a".repeat(64),
            artifact_path: std::path::PathBuf::from("/var/lib/tracedecay/models/code-embedding"),
        });

        let mutation = direct_mutation_for_runtime_config_diff(&project_id, &before, &after)
            .expect("semantic runtime configuration is valid")
            .expect("semantic selection change requires a mutation");
        let DirectConfigurationMutation::Batch { mutations } = mutation else {
            panic!("semantic selection must use the typed batch boundary");
        };
        assert_eq!(mutations.len(), 1);
        let DirectConfigurationMutation::Set { key, value, .. } = &mutations[0] else {
            panic!("semantic selection must be one atomic set");
        };
        assert_eq!(key.as_str(), crate::config::SEMANTIC_RUNTIME_SETTING_KEY);
        let ConfigurationValueV1::Text(encoded) = value else {
            panic!("semantic selection must use the canonical text value");
        };
        let decoded: crate::config::SemanticConfig = serde_json::from_str(encoded).unwrap();
        assert_eq!(decoded, after.semantic);
    }

    #[tokio::test]
    async fn daemon_mutation_response_is_retargeted_and_published_without_legacy_write() {
        let project_id = project_id("project.runtime-daemon-client");
        let root = TempDir::new().expect("temporary project root");
        let returned_root = TempDir::new().expect("temporary daemon response root");
        let registry = ConfigurationRegistry::core().expect("registry is available");
        let current_revision = revision_id("revision.runtime-daemon-client.current");
        let next_revision = revision_id("revision.runtime-daemon-client.next");
        let current = PinnedRuntimeConfiguration::new(
            RuntimeConfigurationTarget {
                project_id: project_id.clone(),
                project_root: root.path().to_path_buf(),
            },
            current_revision.clone(),
            resolve_configuration(&registry, &[])
                .expect("defaults resolve")
                .snapshot,
        )
        .expect("default snapshot materializes");
        let mut updated = current.config.clone();
        updated.git_ignore = false;
        let mutation =
            direct_mutation_for_runtime_config_diff(&project_id, &current.config, &updated)
                .expect("runtime fields have typed settings")
                .expect("gitignore update requires a mutation");
        let expected_mutation = mutation.clone();
        let next = UsecasePinnedRuntimeConfiguration::new(
            UsecaseRuntimeConfigurationTarget {
                project_id: project_id.clone(),
                project_root: returned_root.path().to_path_buf(),
            },
            next_revision.clone(),
            resolve_configuration(
                &registry,
                &[ConfigurationLayerV1 {
                    layer: ConfigurationLayerIdV1::Project {
                        project_id: project_id.clone(),
                    },
                    revision_id: next_revision.clone(),
                    entries: BTreeMap::from([(
                        SettingKey::new("index.git_ignore.v1").expect("known setting key"),
                        ConfigurationValueV1::Boolean(false),
                    )]),
                }],
            )
            .expect("updated project layer resolves")
            .snapshot,
        )
        .expect("updated snapshot materializes");
        let client = RecordingDaemonClient {
            next,
            calls: Mutex::new(Vec::new()),
        };

        let published = commit_runtime_configuration_mutation(&client, &current, mutation)
            .await
            .expect("daemon response is accepted");

        let calls = client
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.project_id, current.target.project_id);
        assert_eq!(calls[0].0.project_root, current.target.project_root);
        assert_eq!(calls[0].1, expected_mutation);
        assert_eq!(calls[0].2, current_revision);
        assert_eq!(published.revision_id, next_revision);
        assert_eq!(published.target.project_root, root.path().to_path_buf());
        assert_eq!(
            published.config.root_dir,
            root.path().to_string_lossy().to_string(),
            "the daemon response root is non-authoritative"
        );
        assert!(!published.config.git_ignore);
        assert_eq!(
            cached_runtime_configuration(root.path())
                .expect("published cache entry")
                .revision_id,
            next_revision
        );
        assert!(
            !root.path().join(".tracedecay").join("config.json").exists(),
            "typed daemon mutation must not write config.json"
        );
    }

    #[tokio::test]
    async fn missing_daemon_client_fails_closed_without_legacy_file_fallback() {
        let project_id = project_id("project.runtime-fail-closed");
        let root = TempDir::new().expect("temporary project root");
        let legacy_path = root.path().join("config.json");
        let legacy_contents = r#"{"git_ignore":false,"root_dir":"/legacy"}"#;
        std::fs::write(&legacy_path, legacy_contents).expect("write legacy fixture");
        let snapshot = resolve_configuration(
            &ConfigurationRegistry::core().expect("registry is available"),
            &[],
        )
        .expect("defaults resolve")
        .snapshot;
        let current = PinnedRuntimeConfiguration::new(
            RuntimeConfigurationTarget {
                project_id,
                project_root: root.path().to_path_buf(),
            },
            revision_id("revision.runtime-fail-closed"),
            snapshot,
        )
        .expect("default snapshot materializes");
        let mut updated = current.config.clone();
        updated.git_ignore = !updated.git_ignore;

        let error = mutate_pinned_runtime_configuration(&current, updated)
            .await
            .expect_err("missing control-plane client must reject mutation");
        assert!(
            error
                .to_string()
                .contains("daemon control-plane client is not installed"),
            "unexpected mutation error: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(&legacy_path).expect("legacy fixture remains readable"),
            legacy_contents,
            "missing authority must never fall back to config.json"
        );
    }

    #[test]
    fn cached_runtime_reads_ignore_legacy_input_after_publication() {
        let project_id = project_id("project.runtime-cache-only");
        let root = TempDir::new().expect("temporary project root");
        let snapshot = resolve_configuration(
            &ConfigurationRegistry::core().expect("registry is available"),
            &[],
        )
        .expect("defaults resolve")
        .snapshot;
        let pinned = PinnedRuntimeConfiguration::new(
            RuntimeConfigurationTarget {
                project_id,
                project_root: root.path().to_path_buf(),
            },
            revision_id("revision.runtime-cache-only"),
            snapshot,
        )
        .expect("default snapshot materializes");
        install_pinned_runtime_configuration(pinned).expect("publish pinned snapshot");

        let legacy_dir = root.path().join(".tracedecay");
        std::fs::create_dir_all(&legacy_dir).expect("create legacy fixture directory");
        std::fs::write(
            legacy_dir.join("config.json"),
            r#"{"root_dir":"/legacy","telemetry":{"timings":false},"sync":{"auto_watch":false}}"#,
        )
        .expect("write conflicting legacy input");

        assert!(
            cached_telemetry_config(root.path())
                .expect("cache lookup")
                .timings,
            "hook-safe telemetry lookup must use the published snapshot"
        );
        assert!(
            cached_sync_config(root.path())
                .expect("cache lookup")
                .auto_watch,
            "hook-safe sync lookup must use the published snapshot"
        );
        assert_eq!(
            cached_runtime_configuration(root.path())
                .expect("cache lookup")
                .config
                .root_dir,
            root.path().to_string_lossy().to_string(),
            "root metadata comes from the non-authoritative published route"
        );
    }

    #[test]
    fn runtime_cache_retargets_legacy_root_metadata_per_cached_root() {
        let project_id = project_id("project.runtime-cache-retarget");
        let root = TempDir::new().expect("temporary project root");
        let first_root = root.path().join("first-worktree");
        let second_root = root.path().join("second-worktree");
        std::fs::create_dir_all(&first_root).expect("create first root");
        std::fs::create_dir_all(&second_root).expect("create second root");
        let snapshot = resolve_configuration(
            &ConfigurationRegistry::core().expect("registry is available"),
            &[],
        )
        .expect("defaults resolve")
        .snapshot;
        let revision_id = revision_id("revision.runtime-cache-retarget");
        let cache = RuntimeConfigurationCache::default();
        cache
            .insert(
                PinnedRuntimeConfiguration::new(
                    RuntimeConfigurationTarget {
                        project_id: project_id.clone(),
                        project_root: first_root.clone(),
                    },
                    revision_id.clone(),
                    snapshot.clone(),
                )
                .expect("first snapshot materializes"),
            )
            .expect("publish first root");
        cache
            .insert(
                PinnedRuntimeConfiguration::new(
                    RuntimeConfigurationTarget {
                        project_id: project_id.clone(),
                        project_root: second_root.clone(),
                    },
                    revision_id,
                    snapshot,
                )
                .expect("second snapshot materializes"),
            )
            .expect("publish second root");

        let first = cache.for_root(&first_root).expect("first root lookup");
        let second = cache.for_root(&second_root).expect("second root lookup");
        assert_eq!(first.target.project_id, project_id);
        assert_eq!(second.target.project_id, project_id);
        assert_eq!(first.target.project_root, first_root);
        assert_eq!(second.target.project_root, second_root);
        assert_ne!(first.config.root_dir, second.config.root_dir);
    }

    #[tokio::test]
    async fn ensure_runtime_configuration_persists_initial_resolution_when_cache_is_empty() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary project root");
        crate::storage::write_enrollment_marker(
            root.path(),
            &crate::storage::EnrollmentMarker {
                project_id: "proj_ensure_runtime_bootstrap".to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .expect("write enrollment marker");
        let layout = crate::storage::resolve_layout_for_current_profile(root.path())
            .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");

        assert!(
            runtime_configuration_for_layout(root.path(), &layout).is_err(),
            "fail-closed lookup must reject an unpublished project"
        );

        let runtime = HostAdmissionTestRuntimeV1::project(
            crate::storage::default_profile_root().unwrap(),
            root.path(),
            project_id("proj_ensure_runtime_bootstrap"),
        )
        .await
        .expect("open retained project runtime");
        let pinned = runtime
            .ensure_runtime_configuration_for_test(root.path(), &layout)
            .await
            .expect("cold open persists and publishes a resolved revision");
        assert_eq!(
            pinned.target.project_id.as_str(),
            "proj_ensure_runtime_bootstrap"
        );
        assert_ne!(
            pinned.revision_id.as_str(),
            "configuration.bootstrap.default.v1",
            "the synthetic bootstrap revision is not a durable runtime authority"
        );
        assert!(
            layout.sessions_db_path.is_file(),
            "initial resolution must be committed to the retained project store"
        );

        let reopened = runtime
            .ensure_runtime_configuration_for_test(root.path(), &layout)
            .await
            .expect("reopen loads the durable current revision");
        assert_eq!(reopened.revision_id, pinned.revision_id);
        assert_eq!(reopened.snapshot, pinned.snapshot);
        assert!(
            runtime_configuration_for_layout(root.path(), &layout).is_ok(),
            "after ensure, fail-closed lookup must see the published pin"
        );
    }

    #[tokio::test]
    async fn ensure_runtime_configuration_repairs_pre_binding_revision_forward_only() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary project root");
        let project_id = project_id("proj_runtime_binding_repair");
        crate::storage::write_enrollment_marker(
            root.path(),
            &crate::storage::EnrollmentMarker {
                project_id: project_id.as_str().to_owned(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .expect("write enrollment marker");
        let layout = crate::storage::resolve_layout_for_current_profile(root.path())
            .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");
        let runtime = HostAdmissionTestRuntimeV1::project(
            crate::storage::default_profile_root().unwrap(),
            root.path(),
            project_id.clone(),
        )
        .await
        .expect("open retained project runtime");
        let database = runtime
            .registered_database_arc(
                crate::application::host_admission::HostAdmissionScope::Project,
            )
            .expect("bind registered project database");
        let store =
            crate::global_db::configuration::GlobalDbConfigurationControlStore::new_registered(
                database.as_ref(),
            );
        let old_revision = revision_id("configuration.initial.pre-binding");
        let target = LegacyConfigurationDecodeTargetV1 {
            target_layer: ConfigurationLayerIdV1::Project {
                project_id: project_id.clone(),
            },
            target_revision_id: old_revision.clone(),
        };
        let inputs = decode_legacy_configuration_inputs(
            r#"{"sync":{"auto_watch":false}}"#,
            &BTreeMap::new(),
            &target,
        )
        .expect("decode pre-binding configuration");
        crate::global_db::configuration::migrate_legacy_configuration_inputs(
            &ConfigurationRegistry::core().expect("configuration registry"),
            &inputs,
            &store,
            UtcMicros(1),
        )
        .await
        .expect("seed pre-binding durable revision");
        let seeded = store.current().await.expect("read seeded configuration");
        assert_eq!(seeded.revision_id, old_revision);
        let bindings_key =
            SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).expect("source bindings key");
        assert_eq!(
            seeded.snapshot.effective_values.get(&bindings_key),
            Some(&ConfigurationValueV1::SourceBindings(Vec::new())),
            "fixture must reproduce a durable revision created before daemon binding genesis"
        );

        let repaired = runtime
            .ensure_runtime_configuration_for_test(root.path(), &layout)
            .await
            .expect("registered daemon authority repairs the durable binding");
        let expected = crate::config::scope_control::daemon_owned_project_source_binding(
            &project_id,
            root.path(),
        )
        .expect("derive expected daemon binding");
        assert_ne!(
            repaired.revision_id, old_revision,
            "repair must append a forward child revision"
        );
        assert_eq!(
            repaired.snapshot.effective_values.get(&bindings_key),
            Some(&ConfigurationValueV1::SourceBindings(vec![expected])),
        );

        let reopened = runtime
            .ensure_runtime_configuration_for_test(root.path(), &layout)
            .await
            .expect("binding repair is idempotent");
        assert_eq!(reopened.revision_id, repaired.revision_id);
        assert_eq!(reopened.snapshot, repaired.snapshot);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ensure_runtime_configuration_keeps_binding_revision_across_linked_worktrees() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary root");
        let primary = root.path().join("primary");
        let linked = root.path().join("linked");
        std::fs::create_dir_all(&primary).expect("create primary root");
        let git = |cwd: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "TraceDecay Test")
                .env("GIT_AUTHOR_EMAIL", "test@tracedecay.local")
                .env("GIT_COMMITTER_NAME", "TraceDecay Test")
                .env("GIT_COMMITTER_EMAIL", "test@tracedecay.local")
                .output()
                .expect("run git");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&primary, &["init", "-b", "main", "--quiet"]);
        std::fs::write(primary.join("README.md"), "primary\n").expect("fixture");
        git(&primary, &["add", "README.md"]);
        git(&primary, &["commit", "-m", "fixture", "--quiet"]);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "feature/linked",
                linked.to_str().expect("linked path"),
                "HEAD",
            ],
        );

        let project_id = project_id("proj_runtime_linked_binding");
        crate::storage::write_enrollment_marker(
            &primary,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.as_str().to_owned(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .expect("write enrollment marker");
        let layout = crate::storage::resolve_layout_for_current_profile(&primary)
            .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");
        let runtime = HostAdmissionTestRuntimeV1::project(
            crate::storage::default_profile_root().unwrap(),
            &primary,
            project_id.clone(),
        )
        .await
        .expect("open retained project runtime");

        let primary_configuration = runtime
            .ensure_runtime_configuration_for_test(&primary, &layout)
            .await
            .expect("open primary configuration");
        let linked_configuration = runtime
            .ensure_runtime_configuration_for_test(&linked, &layout)
            .await
            .expect("open linked configuration");
        let reopened_primary = runtime
            .ensure_runtime_configuration_for_test(&primary, &layout)
            .await
            .expect("reopen primary configuration");

        assert_eq!(
            linked_configuration.revision_id, primary_configuration.revision_id,
            "linked open must not rebind the shared repository authority"
        );
        assert_eq!(
            reopened_primary.revision_id, primary_configuration.revision_id,
            "returning to the primary must not repair linked-worktree churn"
        );
        assert_eq!(
            crate::config::scope_control::daemon_owned_project_source_binding(
                &project_id,
                &primary,
            )
            .expect("primary binding"),
            crate::config::scope_control::daemon_owned_project_source_binding(
                &project_id,
                &linked,
            )
            .expect("linked binding"),
        );
    }

    #[tokio::test]
    async fn resolve_runtime_configuration_pins_registered_project_when_cache_is_cold() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary project root");
        crate::storage::write_enrollment_marker(
            root.path(),
            &crate::storage::EnrollmentMarker {
                project_id: "proj_resolve_cold_cache".to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .expect("write enrollment marker");
        let layout = crate::storage::resolve_layout_for_current_profile(root.path())
            .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");

        // A freshly registered project has no pinned snapshot in this process's
        // cache — exactly the state a daemon is in for a project it has not yet
        // opened, or for any project after a restart. The fail-closed hook-path
        // lookup rejects it.
        assert!(
            runtime_configuration_for_layout(root.path(), &layout).is_err(),
            "cold cache must fail the fail-closed lookup before an on-demand resolve"
        );

        // The daemon authority path resolves and pins on demand instead of
        // erroring, so branch administration and other daemon operations run.
        let runtime = HostAdmissionTestRuntimeV1::project(
            crate::storage::default_profile_root().unwrap(),
            root.path(),
            project_id("proj_resolve_cold_cache"),
        )
        .await
        .expect("open retained project runtime");
        let pinned = runtime
            .resolve_runtime_configuration_for_test(root.path(), &layout)
            .await
            .expect("daemon resolve pins a registered project on demand");
        assert_eq!(pinned.target.project_id.as_str(), "proj_resolve_cold_cache");

        // After the resolve, even the fail-closed lookup sees the published pin,
        // so a subsequent daemon operation no longer hits the cold-cache error.
        assert!(
            runtime_configuration_for_layout(root.path(), &layout).is_ok(),
            "on-demand resolve must publish a pin the fail-closed lookup can read"
        );

        // A second resolve is idempotent and returns the same authority.
        let reresolved = runtime
            .resolve_runtime_configuration_for_test(root.path(), &layout)
            .await
            .expect("second daemon resolve reuses the published pin");
        assert_eq!(reresolved.revision_id, pinned.revision_id);
        assert_eq!(reresolved.snapshot, pinned.snapshot);
    }

    #[tokio::test]
    async fn resolve_runtime_configuration_errors_typed_when_authority_is_unresolvable() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary project root");
        crate::storage::write_enrollment_marker(
            root.path(),
            &crate::storage::EnrollmentMarker {
                project_id: "proj_resolve_unresolvable".to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .expect("write enrollment marker");
        let mut layout = crate::storage::resolve_layout_for_current_profile(root.path())
            .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");

        let runtime = HostAdmissionTestRuntimeV1::project(
            crate::storage::default_profile_root().unwrap(),
            root.path(),
            project_id("proj_resolve_unresolvable"),
        )
        .await
        .expect("open retained project runtime");
        // Strip the authoritative project identity: a layout with no project id
        // has no configuration authority to resolve, and on-demand resolution
        // must surface a typed error rather than fabricate one.
        layout.identity.project_id = None;

        let error = runtime
            .resolve_runtime_configuration_for_test(root.path(), &layout)
            .await
            .expect_err("a layout without project identity has no resolvable authority");
        assert!(
            matches!(error, crate::errors::TraceDecayError::Config { .. }),
            "genuine unavailability must stay a typed configuration error, got {error:?}"
        );
    }

    #[tokio::test]
    async fn read_only_open_serves_registry_defaults_for_uninitialized_store() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let root = TempDir::new().expect("temporary project root");
        crate::storage::write_enrollment_marker(
            root.path(),
            &crate::storage::EnrollmentMarker {
                project_id: "proj_read_only_uninitialized".to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .expect("write enrollment marker");
        let layout = crate::storage::resolve_layout_for_current_profile(root.path())
            .expect("resolve store layout");
        std::fs::create_dir_all(&layout.data_root).expect("create data root");
        if let Some(parent) = layout.sessions_db_path.parent() {
            std::fs::create_dir_all(parent).expect("create sessions db parent");
        }

        // Materialize the durable store schema without ever seeding a
        // configuration revision — the state a consolidated destination store is
        // left in after a repository move, when its configuration authority was
        // never migrated in.
        let runtime = HostAdmissionTestRuntimeV1::project(
            crate::storage::default_profile_root().unwrap(),
            root.path(),
            project_id("proj_read_only_uninitialized"),
        )
        .await
        .expect("open retained project runtime");
        // A read-only reopen must degrade to the registry-default snapshot rather
        // than hard-erroring on the absent current revision. This is what lets a
        // moved, consolidated project be inspected read-only.
        let configuration = runtime
            .load_runtime_configuration_read_only_for_test(root.path(), &layout)
            .await
            .expect("read-only open serves registry defaults for an uninitialized store");
        assert_eq!(
            configuration.target.project_id.as_str(),
            "proj_read_only_uninitialized"
        );
        assert_eq!(
            configuration.revision_id.as_str(),
            "configuration.read_only.default.v1",
            "an uninitialized store must resolve the read-only registry default revision"
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod retention_config_tests {
    use crate::config::{CompactionThresholdConfig, RetentionConfig, SyncConfig};

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
