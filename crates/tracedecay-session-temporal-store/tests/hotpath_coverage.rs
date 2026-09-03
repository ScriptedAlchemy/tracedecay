//! Runtime coverage for canonical session-temporal hydration rendering.

use tracedecay_lcm::contracts::{LcmContentRange, LcmContentSlice, LcmExpandResponse};
use tracedecay_session_temporal_store::render::apply_canonical_content;

#[cfg(feature = "hotpath")]
#[path = "../../../tests/hotpath_report_support.rs"]
mod hotpath_report_support;

#[cfg(feature = "hotpath")]
const EXPECTED_LABELS: &[&str] = &["session_temporal.hydrate.render"];

fn exercise_hydration_render() {
    let expansion = LcmExpandResponse {
        kind: "raw_message".to_owned(),
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
    .expect("render canonical content");
    assert_eq!(rendered.content, "canonical hotpath coverage content");
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn measured_hydration_render_runs_with_hotpath_off() {
    exercise_hydration_render();
}

#[cfg(feature = "hotpath")]
#[test]
fn measured_hydration_render_emits_exact_label() {
    hotpath_report_support::assert_hotpath_report(
        "session-temporal-store-hotpath-coverage",
        "functions-timing",
        EXPECTED_LABELS,
        exercise_hydration_render,
    );
}
