//! Direct-report evidence retention regressions.

use std::path::Path;

use crate::{
    GenerateCandidateOutputsOptions, QUERY_BASELINE_PROFILE, checked_in_fixture_root,
    compute_profile_material_digest, evaluate_generated_outputs, generate_candidate_outputs,
    load_candidate_workload,
};

fn direct_fixture_scope(_repo_root: &Path) -> Option<tracedecay_application::ResolvedScope> {
    tracedecay_application::ResolvedScope::new(
        tracedecay_domain::ProjectId::new("project.search-eval-direct-report").ok()?,
        tracedecay_domain::RepositoryId::new("repository.search-eval-direct-report").ok()?,
        tracedecay_domain::WorktreeId::new("worktree.search-eval-direct-report").ok()?,
        None,
    )
    .ok()
}

#[test]
fn baseline_report_retains_raw_fallback_current_and_exact_ten_x_samples() {
    let repo_root = checked_in_fixture_root();
    let workload = load_candidate_workload(
        &repo_root.join("tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json"),
    )
    .expect("checked-in workload");
    let profile_ids = vec![QUERY_BASELINE_PROFILE.to_owned()];
    let generated = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
        repo_root: &repo_root,
        workload_path: None,
        profile_ids: Some(&profile_ids),
        admitted_scope: direct_fixture_scope,
    })
    .expect("generate direct fixture outputs");
    let report = evaluate_generated_outputs(&repo_root, &workload, &generated)
        .expect("evaluate direct fixture outputs");
    let expected_raw_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.search-eval.raw-output-evidence.v1",
        &report.raw_outputs,
    ))
    .expect("hash raw outputs")
    .as_str()
    .to_owned();
    let value = serde_json::to_value(&report).expect("serialize direct report");
    assert_eq!(
        value
            .get("raw_output_digest")
            .and_then(serde_json::Value::as_str),
        Some(expected_raw_digest.as_str())
    );
    assert_eq!(
        value.get("execution_contract"),
        Some(&serde_json::to_value(&workload.execution_contract).expect("serialize execution"))
    );
    assert_eq!(
        value
            .get("profile_material_digests")
            .and_then(serde_json::Value::as_object)
            .and_then(|digests| digests.get(QUERY_BASELINE_PROFILE))
            .and_then(serde_json::Value::as_str),
        Some(
            compute_profile_material_digest(
                workload
                    .profile_matrix
                    .iter()
                    .find(|profile| profile.profile_id == QUERY_BASELINE_PROFILE)
                    .expect("query baseline profile"),
            )
            .expect("query baseline digest")
            .as_str()
        )
    );
    let raw_outputs = value
        .get("raw_outputs")
        .and_then(serde_json::Value::as_array)
        .expect("direct report retains raw candidate outputs");

    assert_eq!(raw_outputs.len(), 2);
    for output in raw_outputs {
        let resources = output
            .get("resources")
            .and_then(serde_json::Value::as_object)
            .expect("raw output resources");
        assert_eq!(resources.len(), 2);
        let current = resources
            .get("current")
            .and_then(|sample| sample.get("eligible_chunks"))
            .and_then(serde_json::Value::as_u64)
            .expect("current eligible chunks");
        let ten_x = resources
            .get("10x")
            .and_then(|sample| sample.get("eligible_chunks"))
            .and_then(serde_json::Value::as_u64)
            .expect("10x eligible chunks");
        assert_eq!(ten_x, current * 10);
    }
}

#[test]
fn baseline_report_is_self_validating_but_not_activation_evidence() {
    let repo_root = checked_in_fixture_root();
    let workload = load_candidate_workload(
        &repo_root.join("tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json"),
    )
    .expect("checked-in workload");
    let profile_ids = vec![QUERY_BASELINE_PROFILE.to_owned()];
    let generated = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
        repo_root: &repo_root,
        workload_path: None,
        profile_ids: Some(&profile_ids),
        admitted_scope: direct_fixture_scope,
    })
    .expect("generate direct fixture outputs");
    let report = evaluate_generated_outputs(&repo_root, &workload, &generated)
        .expect("evaluate direct fixture outputs");

    report
        .validate_against(&repo_root, &workload)
        .expect("baseline evidence remains self-validating");
    let activation_error = report
        .validate_for_activation(&repo_root, &workload)
        .expect_err("baseline-only report cannot stand in for native activation evidence");
    assert!(
        activation_error.to_string().contains("native"),
        "unexpected activation refusal: {activation_error}"
    );

    let mut tampered = report.clone();
    tampered.raw_output_digest = "sha256:tampered".to_owned();
    let raw_error = tampered
        .validate_against(&repo_root, &workload)
        .expect_err("raw output digest must bind the retained outputs");
    assert!(raw_error.to_string().contains("raw output digest"));

    let mut value = serde_json::to_value(&report).expect("serialize report");
    value
        .as_object_mut()
        .expect("serialized report object")
        .insert("unexpected".to_owned(), serde_json::Value::Null);
    assert!(serde_json::from_value::<crate::DirectEvaluationReportV1>(value).is_err());

    let mut nested = serde_json::to_value(&report).expect("serialize nested report");
    nested["profiles"][0]
        .as_object_mut()
        .expect("serialized profile object")
        .insert("unexpected".to_owned(), serde_json::Value::Null);
    assert!(serde_json::from_value::<crate::DirectEvaluationReportV1>(nested).is_err());
}

#[test]
fn native_activation_rejects_sqlite_vector_measurement_provenance() {
    let stale = "linux-procfs-v1;projection=DatabaseVectorEvaluationStoreV1(SQLite-CAS)";
    assert!(crate::report::validate_native_measurement_method(stale).is_err());
    assert!(
        crate::report::validate_native_measurement_method(
            "linux-procfs-v1;projection=canonical-graph-prepared-generation-v1"
        )
        .is_ok()
    );
    assert!(
        crate::report::validate_native_measurement_method(
            "linux-procfs-v1;projection-cases=prepare_semantic_evaluation_projection\
             +GraphVectorGenerationStoreV1(isolated-in-memory-graph,watermark-CAS)"
        )
        .is_ok()
    );
}
