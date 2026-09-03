//! Hotpath coverage contract for `tracedecay-session-temporal-store`.
//!
//! Feature-off (default build): every hotpath macro must be a no-op — no
//! report file even when the report environment is set.
//!
//! Feature-on (`--features hotpath`): a process-boundary guard must capture
//! this crate's measured hydration-render site in a functions-timing report,
//! proving the instrumentation is real rather than dead configuration.

use tracedecay_lcm::contracts::{LcmContentRange, LcmContentSlice, LcmExpandResponse};
use tracedecay_session_temporal_store::render::apply_canonical_content;

/// Deterministic, daemon-free workload that reaches this crate's measured
/// site `session_temporal.hydrate.render`.
fn run_hydration_render_workload() -> usize {
    let expansion = LcmExpandResponse {
        kind: "raw_message".to_string(),
        content: String::new(),
        content_range: LcmContentRange {
            offset: 0,
            limit: 64,
            returned_chars: 0,
            total_chars: 0,
            truncated: false,
        },
        raw_message: None,
        raw_message_metadata: None,
        summary_node: None,
        summary_sources: Vec::new(),
        payload_ref: None,
        from_current_session: None,
        externalized_note: None,
        source_pagination: None,
    };

    let rendered = apply_canonical_content(
        expansion,
        LcmContentSlice {
            offset: 0,
            limit: 64,
        },
        "canonical hotpath coverage content",
    )
    .expect("render canonical content slice");
    assert_eq!(rendered.content, "canonical hotpath coverage content");
    rendered.content.len()
}

#[cfg(not(feature = "hotpath"))]
mod feature_off {
    use std::path::Path;

    /// With the feature off the macros expand to their primary expression:
    /// the workload behaves identically and the report environment is ignored.
    #[test]
    fn workload_is_a_no_op_for_profiling() {
        let report = Path::new(env!("CARGO_TARGET_TMPDIR")).join("temporal-store-hotpath-off.json");
        let _ = std::fs::remove_file(&report);
        // SAFETY: single-threaded with respect to readers — the feature-off
        // build contains no hotpath runtime and nothing else in this test
        // binary reads these variables.
        unsafe {
            std::env::set_var("HOTPATH_OUTPUT_FORMAT", "json");
            std::env::set_var("HOTPATH_OUTPUT_PATH", &report);
        }

        assert!(super::run_hydration_render_workload() > 0);

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
    /// measured site, proving `--features hotpath` produces live
    /// instrumentation and not an empty report.
    #[test]
    fn guard_report_captures_measured_render_site() {
        // SAFETY: set before the first guard build in this process, which is
        // the only reader; the metrics listener must stay off in tests.
        unsafe { std::env::set_var("HOTPATH_METRICS_SERVER_OFF", "1") };
        let report = Path::new(env!("CARGO_TARGET_TMPDIR")).join("temporal-store-hotpath-on.json");
        let _ = std::fs::remove_file(&report);

        {
            let _guard = hotpath::HotpathGuardBuilder::new("temporal-store-hotpath-coverage")
                .format(hotpath::Format::Json)
                .output_path(&report)
                .report("functions-timing")
                .build();
            assert!(super::run_hydration_render_workload() > 0);
        }

        let report_text =
            std::fs::read_to_string(&report).expect("feature-on guard drop must write a report");
        assert!(
            report_text.contains("session_temporal.hydrate.render"),
            "hotpath report must capture measured site `session_temporal.hydrate.render`: \
             {report_text}"
        );
    }
}
