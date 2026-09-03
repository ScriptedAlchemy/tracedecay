//! Runtime coverage for LCM security scanning and overflow policy labels.

use serde_json::json;
use tracedecay_lcm::compression_policy::{
    OverflowRecoveryCapInput, overflow_recovery_assembly_cap,
};
use tracedecay_lcm::security::{long_base64_run_spans, quarantine_reason};

#[cfg(feature = "hotpath")]
#[path = "../../../tests/hotpath_report_support.rs"]
mod hotpath_report_support;

#[cfg(feature = "hotpath")]
const EXPECTED_LABELS: &[&str] = &[
    "sessions.lcm.scan_base64",
    "sessions.lcm.scan_repetition",
    "sessions.lcm.overflow_cap",
];

fn exercise_lcm_policy() {
    let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let spans = long_base64_run_spans(&alphabet.repeat(64));
    assert!(!spans.is_empty(), "fixture must contain a long base64 run");

    let repeated =
        "same repeated assistant diagnostic segment with very low novelty.\n".repeat(1_200);
    assert_eq!(
        quarantine_reason("assistant", Some("message"), &repeated),
        Some("high_repetition")
    );

    let cap = overflow_recovery_assembly_cap(OverflowRecoveryCapInput {
        current_tokens: Some(8),
        max_assembly_tokens: Some(10),
        messages: &[json!({ "content": "two tokens" })],
    });
    assert!(cap.is_some(), "bounded overflow input must produce a cap");
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn measured_lcm_policy_runs_with_hotpath_off() {
    exercise_lcm_policy();
}

#[cfg(feature = "hotpath")]
#[test]
fn measured_lcm_policy_emits_exact_labels() {
    hotpath_report_support::assert_hotpath_report(
        "lcm-hotpath-coverage",
        "functions-timing",
        EXPECTED_LABELS,
        exercise_lcm_policy,
    );
}
