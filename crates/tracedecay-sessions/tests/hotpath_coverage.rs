//! Runtime coverage for session normalization and transcript discovery labels.

use serde_json::json;
use tracedecay_sessions::runtime::shared::content_storage_text_and_tools;
use tracedecay_sessions::runtime::source::{
    TranscriptDiscoveryBounds, collect_files_with_ext_bounded,
};

#[cfg(feature = "hotpath")]
#[path = "../../../tests/hotpath_report_support.rs"]
mod hotpath_report_support;

#[cfg(feature = "hotpath")]
const EXPECTED_LABELS: &[&str] = &[
    "sessions.shared.content_storage",
    "sessions.source.discover_files",
];

fn exercise_session_ingest() {
    let content = json!([
        { "type": "text", "text": "hotpath coverage fixture" },
        { "type": "tool_use", "name": "Read", "id": "tool.1", "input": { "path": "a.rs" } },
    ]);
    let (text, tools) = content_storage_text_and_tools(&content, None);
    assert!(!text.is_empty());
    assert_eq!(tools, vec!["Read".to_owned()]);

    let directory = tempfile::tempdir().expect("create transcript fixture");
    for ordinal in 0..3 {
        std::fs::write(
            directory.path().join(format!("session-{ordinal}.jsonl")),
            b"{\"type\":\"user\"}\n",
        )
        .expect("write transcript fixture");
    }
    let discovery = collect_files_with_ext_bounded(
        directory.path(),
        "jsonl",
        1,
        TranscriptDiscoveryBounds::from_discovered_units(16),
    );
    assert_eq!(discovery.paths.len(), 3);
    assert!(discovery.truncated.is_none());
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn measured_session_ingest_runs_with_hotpath_off() {
    exercise_session_ingest();
}

#[cfg(feature = "hotpath")]
#[test]
fn measured_session_ingest_emits_exact_labels() {
    hotpath_report_support::assert_hotpath_report(
        "sessions-hotpath-coverage",
        "functions-timing",
        EXPECTED_LABELS,
        exercise_session_ingest,
    );
}
