use tempfile::TempDir;
use tracedecay_dashboard_api::code_index_freshness_api::{
    CodeGraphServingReadinessV1, CodeIndexWorktreeFreshnessV1,
};

use super::super::branch_publication::{
    BranchPublicationContextV1, branch_generation_work_is_active,
};

#[test]
fn branch_publication_requires_authoritative_project_identity() {
    let project = TempDir::new().expect("project root");
    let store = TempDir::new().expect("store root");

    let error = BranchPublicationContextV1::new(None, project.path(), store.path())
        .expect_err("missing project identity must fail closed");

    assert_eq!(
        error.project_route_context(),
        Some((
            "code_index_scheduler_identity_mismatch",
            false,
            "branch graph publication requires an authoritative project identity",
        ))
    );
}

#[test]
fn pending_graph_activation_keeps_exact_branch_wait_live() {
    let pending = CodeIndexWorktreeFreshnessV1 {
        rebuild_in_flight: false,
        code_graph_serving: Some(CodeGraphServingReadinessV1::Pending),
        ..CodeIndexWorktreeFreshnessV1::default()
    };
    assert!(branch_generation_work_is_active(&pending));

    let terminal = CodeIndexWorktreeFreshnessV1 {
        rebuild_in_flight: false,
        code_graph_serving: Some(CodeGraphServingReadinessV1::Refused {
            reason: "fixture refusal".to_owned(),
        }),
        ..CodeIndexWorktreeFreshnessV1::default()
    };
    assert!(!branch_generation_work_is_active(&terminal));
}
