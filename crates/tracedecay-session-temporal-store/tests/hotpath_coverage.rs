//! Unguarded hotpath contract for `tracedecay-session-temporal-store`.
//!
//! With either feature configuration, setting report environment variables
//! alone must not create a report without a process-boundary guard.

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

mod unguarded {
    use std::path::Path;

    /// The workload behaves identically and report environment is ignored
    /// until a process-boundary guard is installed.
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
            "unguarded workload must never write a hotpath report"
        );
    }
}
