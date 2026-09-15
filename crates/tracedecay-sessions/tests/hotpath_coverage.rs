//! Hotpath coverage contract for `tracedecay-sessions`.
//!
//! Feature-off (default build): every hotpath macro must be a no-op — no
//! report file even when the report environment is set.

#[cfg(not(feature = "hotpath"))]
mod feature_off {
    use std::path::Path;

    use serde_json::json;
    use tracedecay_sessions::runtime::shared::content_storage_text_and_tools;
    use tracedecay_sessions::runtime::source::{
        TranscriptDiscoveryBounds, collect_files_with_ext_bounded,
    };

    /// Deterministic, daemon-free workload that reaches this crate's measured
    /// sites: `sessions.shared.content_storage` and
    /// `sessions.source.discover_files`.
    fn run_sessions_workload() -> usize {
        let content = json!([
            { "type": "text", "text": "hotpath coverage fixture" },
            { "type": "tool_use", "name": "Read", "id": "tool.1", "input": { "path": "a.rs" } },
        ]);
        let (text, tools) = content_storage_text_and_tools(&content, None);
        assert!(!text.is_empty());
        assert_eq!(tools, vec!["Read".to_string()]);

        let temp = tempfile::tempdir().expect("create discovery fixture dir");
        for ordinal in 0..3 {
            std::fs::write(
                temp.path().join(format!("session-{ordinal}.jsonl")),
                b"{\"type\":\"user\"}\n",
            )
            .expect("write discovery fixture file");
        }
        let report = collect_files_with_ext_bounded(
            temp.path(),
            "jsonl",
            1,
            TranscriptDiscoveryBounds::from_discovered_units(16),
        );
        assert_eq!(report.paths.len(), 3);
        assert!(report.truncated.is_none());

        report.paths.len()
    }

    /// With the feature off the macros expand to their primary expression:
    /// the workload behaves identically and the report environment is ignored.
    #[test]
    fn workload_is_a_no_op_for_profiling() {
        let report = Path::new(env!("CARGO_TARGET_TMPDIR")).join("sessions-hotpath-off.json");
        let _ = std::fs::remove_file(&report);
        // SAFETY: single-threaded with respect to readers — the feature-off
        // build contains no hotpath runtime and nothing else in this test
        // binary reads these variables.
        unsafe {
            std::env::set_var("HOTPATH_OUTPUT_FORMAT", "json");
            std::env::set_var("HOTPATH_OUTPUT_PATH", &report);
        }

        assert!(run_sessions_workload() > 0);

        assert!(
            !report.exists(),
            "feature-off build must never write a hotpath report"
        );
    }
}
