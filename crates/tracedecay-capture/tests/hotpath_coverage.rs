//! Runtime coverage for the capture parser's production Hotpath labels.

use serde_json::json;
use tracedecay_capture::parse_claude_record_v1;
use tracedecay_domain::ClaudeByteRangeV1;

#[cfg(feature = "hotpath")]
#[path = "../../../tests/hotpath_report_support.rs"]
mod hotpath_report_support;

#[cfg(feature = "hotpath")]
const EXPECTED_LABELS: &[&str] = &["capture.parse.record", "capture.parse.record_digest"];

fn exercise_capture_parser() {
    let record = serde_json::to_vec(&json!({
        "type": "assistant",
        "message": { "content": "hotpath coverage fixture" },
    }))
    .expect("serialize Claude record fixture");
    let range = ClaudeByteRangeV1::new(0, record.len() as u64).expect("valid byte range");
    let parsed = parse_claude_record_v1(&record, range).expect("parse Claude record fixture");
    assert_eq!(parsed.encoded_len(), record.len());
    assert_eq!(
        parsed.value()["message"]["content"],
        "hotpath coverage fixture"
    );
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn measured_capture_parser_runs_with_hotpath_off() {
    exercise_capture_parser();
}

#[cfg(feature = "hotpath")]
#[test]
fn measured_capture_parser_emits_exact_labels() {
    hotpath_report_support::assert_hotpath_report(
        "capture-hotpath-coverage",
        "functions-timing",
        EXPECTED_LABELS,
        exercise_capture_parser,
    );
}
