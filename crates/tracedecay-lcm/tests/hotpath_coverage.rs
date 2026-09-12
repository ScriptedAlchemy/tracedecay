// Its own test binary, not an lcm_suite module: it sets HOTPATH_* process
// environment variables in-process.
//! Unguarded hotpath contract for `tracedecay-lcm`.
//!
//! With either feature configuration, setting report environment variables
//! alone must not create a report without a process-boundary guard.

use serde_json::json;
use tracedecay_lcm::compression_policy::{
    OverflowRecoveryCapInput, overflow_recovery_assembly_cap,
};
use tracedecay_lcm::security::{long_base64_run_spans, quarantine_reason};

/// Deterministic, daemon-free workload that reaches this crate's measured
/// sites: `sessions.lcm.scan_base64`, `sessions.lcm.scan_repetition`, and
/// `sessions.lcm.overflow_cap`.
fn run_lcm_policy_workload() -> usize {
    let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let base64_run = alphabet.repeat(64);
    let spans = long_base64_run_spans(&base64_run);
    assert!(!spans.is_empty(), "fixture must contain a long base64 run");

    let repeated =
        "same repeated assistant diagnostic segment with very low novelty.\n".repeat(1_200);
    assert_eq!(
        quarantine_reason("assistant", Some("message"), &repeated),
        Some("high_repetition"),
    );

    let cap = overflow_recovery_assembly_cap(OverflowRecoveryCapInput {
        current_tokens: Some(8),
        max_assembly_tokens: Some(10),
        messages: &[json!({ "content": "two tokens" })],
    });
    assert!(cap.is_some(), "bounded overflow input must produce a cap");

    spans.len()
}

mod unguarded {
    use std::path::Path;

    /// The workload behaves identically and report environment is ignored
    /// until a process-boundary guard is installed.
    #[test]
    fn workload_is_a_no_op_for_profiling() {
        let report = Path::new(env!("CARGO_TARGET_TMPDIR")).join("lcm-hotpath-off.json");
        let _ = std::fs::remove_file(&report);
        // SAFETY: single-threaded with respect to readers — the feature-off
        // build contains no hotpath runtime and nothing else in this test
        // binary reads these variables.
        unsafe {
            std::env::set_var("HOTPATH_OUTPUT_FORMAT", "json");
            std::env::set_var("HOTPATH_OUTPUT_PATH", &report);
        }

        assert!(super::run_lcm_policy_workload() > 0);

        assert!(
            !report.exists(),
            "unguarded workload must never write a hotpath report"
        );
    }
}
