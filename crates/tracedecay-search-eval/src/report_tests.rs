//! Direct-report evidence retention regressions.

use std::path::Path;

use tracedecay_query::search_quality::{
    DirectEvaluationReportV1, QUERY_BASELINE_PROFILE, compute_profile_material_digest,
    evaluate_generated_outputs, load_candidate_workload,
};

use crate::{GenerateCandidateOutputsOptions, generate_candidate_outputs};

const BASELINE_REPORT_RESOURCE_CHILD_ENV: &str = "TRACEDECAY_BASELINE_REPORT_RESOURCE_CHILD";

fn direct_fixture_scope(_repo_root: &Path) -> Option<tracedecay_contracts::ResolvedScope> {
    tracedecay_contracts::ResolvedScope::new(
        tracedecay_domain::ProjectId::new("project.search-eval-direct-report").ok()?,
        tracedecay_domain::RepositoryId::new("repository.search-eval-direct-report").ok()?,
        tracedecay_domain::WorktreeId::new("worktree.search-eval-direct-report").ok()?,
        None,
    )
    .ok()
}

#[test]
fn baseline_report_retains_raw_fallback_current_and_exact_ten_x_samples() {
    // The workload pins historical commits that survive only as unreachable
    // objects on the integration branch; the fixture clone backfills the
    // checked-in evaluator pack so the historical query resolves here as it
    // does in the daemon.
    let fixture = crate::candidate_output::tests::authenticated_repo_fixture();
    let repo_root = fixture.root.clone();
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
    // Semantic activation refuses any report whose query fallback drifted from
    // the checked-in pin, and the operator journey that would notice is skipped
    // without the FastEmbed distribution fixture. Gate the pin here, where the
    // ordinary lane always runs it: production retrieval changes must land with
    // a re-pinned workload or they silently break `semantic activate`.
    for profile in &report.profiles {
        let observed = generated
            .outputs
            .iter()
            .find(|output| {
                output.profile_id == profile.profile_id && output.partition == profile.partition
            })
            .map(|output| {
                format!(
                    "observed {} vs pinned {}",
                    output.query_fallback_digest, output.expected_query_fallback_digest
                )
            })
            .unwrap_or_else(|| "no generated output for this profile".to_owned());
        assert!(
            profile.fallback_matches_expected,
            "{}:{} query fallback digest drifted from \
             `expected_query_fallback_digests.{}` in \
             tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json \
             ({observed}). Confirm the new query results are intended, then re-pin \
             both workload copies, packaged::WORKLOAD_SHA256, the workload digest \
             pins, and regenerate the packaged native qualification.",
            profile.profile_id, profile.partition, profile.partition
        );
    }
    // Every recall label above is satisfied, so a non-`Pass` status here is
    // absent resource evidence, not a measured quality failure. Print the
    // exact incomplete observation instead of leaving the reader to assume
    // peak RSS is the only field a host failed to report.
    assert_eq!(
        report.status,
        crate::DirectEvaluationStatusV1::Pass,
        "the checked-in query-fallback baseline must keep passing the labels \
         activation requires: {} profiles={}",
        report.pending_diagnostic(),
        serde_json::to_string(&report.profiles).expect("serialize profile evaluations")
    );

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
    if std::env::var_os(BASELINE_REPORT_RESOURCE_CHILD_ENV).is_none() {
        let output = std::process::Command::new(
            std::env::current_exe().expect("report test binary has a current executable"),
        )
        .args([
            "--exact",
            "report_tests::baseline_report_is_self_validating_but_not_activation_evidence",
            "--nocapture",
        ])
        .env(BASELINE_REPORT_RESOURCE_CHILD_ENV, "1")
        .output()
        .expect("run baseline report in a dedicated process");
        assert!(
            output.status.success(),
            "dedicated baseline report failed:\\nstdout:\\n{}\\nstderr:\\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    // The workload pins historical commits that survive only as unreachable
    // objects on the integration branch; the fixture clone backfills the
    // checked-in evaluator pack so the historical query resolves here as it
    // does in the daemon.
    let fixture = crate::candidate_output::tests::authenticated_repo_fixture();
    let repo_root = fixture.root.clone();
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
    // Self-validation and the not-activation-evidence rule are separate
    // contracts. Incomplete resource evidence refuses at the earlier status
    // gate and would mask the refusal this test is about, so name it first
    // with the exact observation the host failed to report.
    assert_eq!(
        report.status,
        crate::DirectEvaluationStatusV1::Pass,
        "resource evidence is incomplete, so the activation refusal below would \
         be the status gate rather than the missing native evidence: {}",
        report.pending_diagnostic()
    );
    let activation_error = report
        .validate_for_activation(&repo_root, &workload)
        .expect_err("baseline-only report cannot stand in for native activation evidence");
    assert!(
        activation_error
            .to_string()
            .contains("no native current/10x resource evidence"),
        "a baseline report must be refused for lacking native evidence, not for \
         any other reason that happens to mention `native`: {activation_error}"
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
    assert!(serde_json::from_value::<DirectEvaluationReportV1>(value).is_err());

    let mut nested = serde_json::to_value(&report).expect("serialize nested report");
    nested["profiles"][0]
        .as_object_mut()
        .expect("serialized profile object")
        .insert("unexpected".to_owned(), serde_json::Value::Null);
    assert!(serde_json::from_value::<DirectEvaluationReportV1>(nested).is_err());
}
