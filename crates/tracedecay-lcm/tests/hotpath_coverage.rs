//! Hotpath coverage contract for `tracedecay-lcm`.
//!
//! Feature-off (default build): every hotpath macro must be a no-op — no
//! report file even when the report environment is set.
//!
//! Feature-on (`--features hotpath`): a process-boundary guard must capture
//! this crate's measured security-scan and compression-policy sites in a
//! functions-timing report, proving the instrumentation is real rather than
//! dead configuration.

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

#[cfg(not(feature = "hotpath"))]
mod feature_off {
    use std::path::Path;

    /// With the feature off the macros expand to their primary expression:
    /// the workload behaves identically and the report environment is ignored.
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
    fn guard_report_captures_measured_policy_sites() {
        // SAFETY: set before the first guard build in this process, which is
        // the only reader; the metrics listener must stay off in tests.
        unsafe { std::env::set_var("HOTPATH_METRICS_SERVER_OFF", "1") };
        let report = Path::new(env!("CARGO_TARGET_TMPDIR")).join("lcm-hotpath-on.json");
        let _ = std::fs::remove_file(&report);

        {
            let _guard = hotpath::HotpathGuardBuilder::new("lcm-hotpath-coverage")
                .format(hotpath::Format::Json)
                .output_path(&report)
                .report("functions-timing")
                .build();
            assert!(super::run_lcm_policy_workload() > 0);
        }

        let report_text =
            std::fs::read_to_string(&report).expect("feature-on guard drop must write a report");
        for label in [
            "sessions.lcm.scan_base64",
            "sessions.lcm.scan_repetition",
            "sessions.lcm.overflow_cap",
        ] {
            assert!(
                report_text.contains(label),
                "hotpath report must capture measured site `{label}`: {report_text}"
            );
        }
    }
}
