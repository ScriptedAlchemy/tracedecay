use super::is_generated_path_segment;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;
use tracedecay_runtime_core::config::{
    PinnedUserDataDir, USER_DATA_DIR_ENV, discover_project_root, get_tracedecay_dir,
    is_ambient_project_root, is_generated_dir_segment, lock_user_data_dir_test_env, user_data_dir,
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
fn implicit_discovery_never_selects_the_user_profile_root() {
    let _profile = PinnedUserDataDir::new();
    let home = PathBuf::from(std::env::var_os("HOME").expect("pinned HOME"));
    let home_store = tracedecay_runtime_core::storage::default_profile_sharded_layout(
        &home,
        &user_data_dir().expect("pinned profile"),
    )
    .expect("home store layout");
    fs::create_dir_all(&home_store.data_root).expect("home store root");
    fs::write(&home_store.graph_db_path, b"").expect("ambient project marker");
    let nested = home.join("unrelated/nested");
    fs::create_dir_all(&nested).expect("nested directory");

    assert!(is_ambient_project_root(&home));
    assert_eq!(discover_project_root(&nested), None);
}

// ---------------------------------------------------------------------------
// Shared generated/vendored segment list
//
// GENERATED_DIR_SEGMENTS is the one list shared by the registry's default
// excludes, scan, and migrate inventory paths.
// ---------------------------------------------------------------------------

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
    assert!(is_generated_path_segment("web/node_modules/react/index.js"));
    assert!(is_generated_path_segment(".worktrees/feature/src/lib.rs"));
    assert!(is_generated_path_segment("assets/app.min.js"));
    assert!(is_generated_path_segment("assets/app.min.css"));
    assert!(!is_generated_path_segment("src/helpers.rs"));
    assert!(!is_generated_path_segment("builder/mod.rs"));
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod retention_config_tests {
    use crate::RetentionConfig;

    #[test]
    fn empty_json_object_deserializes_to_safe_defaults() {
        let retention: RetentionConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(retention, RetentionConfig::default());

        let nested: RetentionConfig =
            serde_json::from_str(r#"{"session_lcm":{},"observation":{}}"#).unwrap();
        assert_eq!(nested, RetentionConfig::default());
        assert!(nested.observation.reclaim_superseded_cursor_advances);
    }

    #[test]
    fn retention_config_json_round_trips_with_windows_set() {
        let json = r#"{
            "session_lcm": { "enabled": true, "drop_after_days": 30 },
            "observation": { "enabled": true, "anchor_release_after_days": 45 },
            "orphan_store_gc_days": 14,
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

/// An explicit `.` or relative path names a directory relative to the CLI's
/// working directory, not to whatever directory the daemon happens to run in.
#[test]
fn explicit_relative_path_is_anchored_to_the_cli_working_directory() {
    let cwd = std::env::current_dir().unwrap();

    let dot = super::resolve_path_with_discovery(Some(".".to_string()));
    assert!(dot.is_absolute());
    assert_eq!(dot, cwd.join("."));
    assert_eq!(
        super::resolve_path_with_discovery(Some("nested/project".to_string())),
        cwd.join("nested/project")
    );
    let absolute = TempDir::new().unwrap();
    assert_eq!(
        super::resolve_path_with_discovery(Some(absolute.path().display().to_string())),
        absolute.path()
    );
}
