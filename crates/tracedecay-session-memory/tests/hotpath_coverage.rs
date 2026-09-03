//! Runtime coverage for session grant and automatic-memory command labels.

use tracedecay_domain::{FactCategoryV1, FactOwnerV1, ProjectId};
use tracedecay_session_memory::context::{
    CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, RequestBudgets,
    session_application_grant_digest,
};
use tracedecay_session_memory::memory::{ProjectMemoryFactAddRequest, automatic_fact_add_command};

#[cfg(feature = "hotpath")]
#[path = "../../../tests/hotpath_report_support.rs"]
mod hotpath_report_support;

#[cfg(feature = "hotpath")]
const EXPECTED_LABELS: &[&str] = &[
    "usecases.context.session_grant",
    "usecases.memory.automatic.command",
];

fn exercise_session_memory() {
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
    .expect("build automatic fact command");
    assert_eq!(
        command.automation_run_id(),
        Some("run_01J4A7P5MQ1X9DX2P9BQNQW75T")
    );
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn measured_session_memory_runs_with_hotpath_off() {
    exercise_session_memory();
}

#[cfg(feature = "hotpath")]
#[test]
fn measured_session_memory_emits_exact_labels() {
    hotpath_report_support::assert_hotpath_report(
        "session-memory-hotpath-coverage",
        "functions-timing",
        EXPECTED_LABELS,
        exercise_session_memory,
    );
}
