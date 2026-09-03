//! Hotpath coverage contract for `tracedecay-capture`.
//!
//! Feature-off (default build): every hotpath macro must be a no-op — no
//! report file even when the report environment is set.
//!
//! Feature-on (`--features hotpath`): a process-boundary guard must capture
//! this crate's measured parse sites in a functions-timing report, proving
//! the instrumentation is real rather than dead configuration.

use serde_json::json;
use tracedecay_capture::parse_claude_record_v1;
use tracedecay_domain::ClaudeByteRangeV1;

/// Deterministic, daemon-free workload that reaches this crate's measured
/// parse path (`capture.parse.record` and `capture.parse.record_digest`).
fn run_capture_parse_workload() -> usize {
    let record = serde_json::to_vec(&json!({
        "type": "assistant",
        "message": { "content": "hotpath coverage fixture" },
    }))
    .expect("serialize claude record fixture");
    let range = ClaudeByteRangeV1::new(0, record.len() as u64).expect("valid fixture byte range");
    let parsed = parse_claude_record_v1(&record, range).expect("parse claude record fixture");
    assert_eq!(parsed.encoded_len(), record.len());
    assert_eq!(
        parsed.value()["message"]["content"],
        "hotpath coverage fixture"
    );
    parsed.encoded_len()
}

#[cfg(not(feature = "hotpath"))]
mod feature_off {
    use std::path::Path;

    /// With the feature off the macros expand to their primary expression:
    /// the workload behaves identically and the report environment is ignored.
    #[test]
    fn workload_is_a_no_op_for_profiling() {
        let report = Path::new(env!("CARGO_TARGET_TMPDIR")).join("capture-hotpath-off.json");
        let _ = std::fs::remove_file(&report);
        // SAFETY: single-threaded with respect to readers — the feature-off
        // build contains no hotpath runtime and nothing else in this test
        // binary reads these variables.
        unsafe {
            std::env::set_var("HOTPATH_OUTPUT_FORMAT", "json");
            std::env::set_var("HOTPATH_OUTPUT_PATH", &report);
        }

        assert!(super::run_capture_parse_workload() > 0);

        assert!(
            !report.exists(),
            "feature-off build must never write a hotpath report"
        );
    }
}

#[cfg(feature = "hotpath")]
mod feature_on {
    use std::path::Path;

    /// A guard-scoped run of the same workload must record this crate's
    /// measured sites, proving `--features hotpath` produces live
    /// instrumentation and not an empty report.
    #[test]
    fn guard_report_captures_measured_parse_sites() {
        // SAFETY: set before the first guard build in this process, which is
        // the only reader; the metrics listener must stay off in tests.
        unsafe { std::env::set_var("HOTPATH_METRICS_SERVER_OFF", "1") };
        let report = Path::new(env!("CARGO_TARGET_TMPDIR")).join("capture-hotpath-on.json");
        let _ = std::fs::remove_file(&report);

        {
            let _guard = hotpath::HotpathGuardBuilder::new("capture-hotpath-coverage")
                .format(hotpath::Format::Json)
                .output_path(&report)
                .report("functions-timing")
                .build();
            assert!(super::run_capture_parse_workload() > 0);
        }

        let report_text =
            std::fs::read_to_string(&report).expect("feature-on guard drop must write a report");
        for label in ["capture.parse.record", "capture.parse.record_digest"] {
            assert!(
                report_text.contains(label),
                "hotpath report must capture measured site `{label}`: {report_text}"
            );
        }
    }
}
