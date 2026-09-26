use super::is_generated_path_segment;
use std::fs;
use tempfile::TempDir;
use tracedecay_runtime_core::config::{ProfileRoot, get_tracedecay_dir, is_generated_dir_segment};

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

    let profile = ProfileRoot::new(cwd.join("unused-profile"));
    let dot = super::resolve_path_with_discovery(&profile, Some(".".to_string()));
    assert!(dot.is_absolute());
    assert_eq!(dot, cwd.join("."));
    assert_eq!(
        super::resolve_path_with_discovery(&profile, Some("nested/project".to_string())),
        cwd.join("nested/project")
    );
    let absolute = TempDir::new().unwrap();
    assert_eq!(
        super::resolve_path_with_discovery(&profile, Some(absolute.path().display().to_string())),
        absolute.path()
    );
}
