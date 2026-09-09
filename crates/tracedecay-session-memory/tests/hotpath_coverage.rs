//! Unguarded hotpath contract for `tracedecay-session-memory`.
//!
//! With either feature configuration, setting report environment variables
//! alone must not create a report without a process-boundary guard.

use tracedecay_domain::{FactCategoryV1, FactOwnerV1, ProjectId};
use tracedecay_session_memory::context::{
    CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, RequestBudgets,
    session_application_grant_digest,
};
use tracedecay_session_memory::memory::{ProjectMemoryFactAddRequest, automatic_fact_add_command};

/// Deterministic, daemon-free workload that reaches this crate's measured
/// sites: `usecases.context.session_grant` and
/// `usecases.memory.automatic.command`.
fn run_session_memory_workload() -> usize {
    const DIGEST: [u8; 32] = [0x5a; 32];
    let budgets =
        RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).expect("non-zero request budgets");
    let cancellation = CancellationToken::for_application_request("request.hotpath-coverage");
    session_application_grant_digest(
        CapabilityDigest::new(DIGEST),
        PolicyDigest::new(DIGEST),
        ConfigurationDigest::new(DIGEST),
        &cancellation,
        budgets,
    )
    .expect("derive session application grant digest");

    let owner = FactOwnerV1::Project {
        project_id: ProjectId::new("project.memory.hotpath-coverage").expect("valid project id"),
    };
    let request = ProjectMemoryFactAddRequest {
        content: "canonical hotpath coverage fixture".into(),
        category: FactCategoryV1::Project,
        source_label: None,
        tags: Vec::new(),
        entities: Vec::new(),
        trust: None,
        metadata: serde_json::json!({}),
    };
    let command = automatic_fact_add_command(
        owner,
        request,
        "run_01J4A7P5MQ1X9DX2P9BQNQW75T",
        "automatic-fact-hotpath-coverage",
        None,
    )
    .expect("build automatic fact add command");
    assert_eq!(
        command.automation_run_id(),
        Some("run_01J4A7P5MQ1X9DX2P9BQNQW75T")
    );

    1
}

mod unguarded {
    use std::path::Path;

    /// The workload behaves identically and report environment is ignored
    /// until a process-boundary guard is installed.
    #[test]
    fn workload_is_a_no_op_for_profiling() {
        let report = Path::new(env!("CARGO_TARGET_TMPDIR")).join("session-memory-hotpath-off.json");
        let _ = std::fs::remove_file(&report);
        // SAFETY: single-threaded with respect to readers — the feature-off
        // build contains no hotpath runtime and nothing else in this test
        // binary reads these variables.
        unsafe {
            std::env::set_var("HOTPATH_OUTPUT_FORMAT", "json");
            std::env::set_var("HOTPATH_OUTPUT_PATH", &report);
        }

        assert!(super::run_session_memory_workload() > 0);

        assert!(
            !report.exists(),
            "unguarded workload must never write a hotpath report"
        );
    }
}
