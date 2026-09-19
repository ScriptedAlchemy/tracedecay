//! Direct-report evidence retention regressions.

use std::path::Path;

use tracedecay_query::search_quality::{
    DirectEvaluationReportV1, QUERY_BASELINE_PROFILE, compute_profile_material_digest,
    evaluate_generated_outputs,
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
    // The packaged root carries the checked-in evaluator object pack, so the
    // historical query resolves here as it does in the daemon.
    let fixture = crate::candidate_output::tests::packaged_fixture();
    let repo_root = fixture.root();
    let workload = fixture.workload();
    let profile_ids = vec![QUERY_BASELINE_PROFILE.to_owned()];
    let generated = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
        repo_root,
        workload_path: None,
        profile_ids: Some(&profile_ids),
        admitted_scope: direct_fixture_scope,
    })
    .expect("generate direct fixture outputs");
    let report = evaluate_generated_outputs(repo_root, workload, &generated)
        .expect("evaluate direct fixture outputs");
    // A ranking change must move the receipt. A generation reseal must not:
    // the receipt hashes ordered rows and lane coverage, not the sealed
    // generation those rows were bound under.
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
            "{}:{} ranking receipt drifted from \
             `expected_query_fallback_digests.{}` in \
             crates/tracedecay-query/assets/runtime-root/tests/fixtures/search_quality/query-lexical-graph-workload-v1.json \
             ({observed}). The receipt binds ordered ranking rows and lane coverage, \
             not generation or extractor-revision identity. Re-pin only that receipt \
             when the ranking itself changed; do not touch the workload identity pin.",
            profile.profile_id, profile.partition, profile.partition
        );
    }
    // Exact/protected retrieval is complete; the lexical lanes miss a fixed
    // set of conceptual needs, so the baseline report is a truthful `Fail`.
    assert_eq!(
        report.status,
        crate::DirectEvaluationStatusV1::Fail,
        "the query-fallback baseline must report its conceptual misses"
    );
    let failed_queries = report
        .profiles
        .iter()
        .flat_map(|profile| profile.queries.iter())
        .filter(|query| query.status == crate::DirectEvaluationStatusV1::Fail)
        .map(|query| query.query_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        failed_queries,
        [
            "train-015",
            "train-016",
            "train-019",
            "train-020",
            "train-025",
            "train-026",
            "train-027",
            "train-029",
            "train-031",
            "train-033",
            "validation-015",
            "validation-017",
            "validation-023",
            "validation-024",
            "validation-025",
            "validation-027",
            "validation-028",
        ]
    );
    assert!(report.profiles.iter().all(|profile| {
        profile.quality.protected_recall_at_10.denominator > 0
            && profile.quality.protected_recall_at_10.numerator
                == profile.quality.protected_recall_at_10.denominator
    }));

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

/// The report is evidence about labels, not authority over retrieval: its
/// status vocabulary cannot express qualification or activation, unknown
/// activation fields are refused on the wire, and workload documents may not
/// be re-read as the candidate evidence an evaluator run produces. See
/// `docs/development/search-quality-direct-evaluation.md`.
#[test]
fn direct_report_is_evidence_only_and_owns_its_candidate_schema() {
    let fixture = crate::candidate_output::tests::packaged_fixture();
    let repo_root = fixture.root();
    let workload = fixture.workload();
    let profile_ids = vec![QUERY_BASELINE_PROFILE.to_owned()];
    let generated = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
        repo_root,
        workload_path: None,
        profile_ids: Some(&profile_ids),
        admitted_scope: direct_fixture_scope,
    })
    .expect("generate direct fixture outputs");
    let report = evaluate_generated_outputs(repo_root, workload, &generated)
        .expect("evaluate direct fixture outputs");

    // Closed evidence statuses only. An activation claim such as
    // status:"qualified" is not a representable DirectEvaluationStatusV1.
    assert!(matches!(
        report.status,
        crate::DirectEvaluationStatusV1::Pass
            | crate::DirectEvaluationStatusV1::Fail
            | crate::DirectEvaluationStatusV1::Pending
    ));
    for profile in &report.profiles {
        assert!(matches!(
            profile.status,
            crate::DirectEvaluationStatusV1::Pass
                | crate::DirectEvaluationStatusV1::Fail
                | crate::DirectEvaluationStatusV1::Pending
        ));
        // Offline and cancellation were aliases of observations already on
        // the profile (fallback match, and a fail-closed generate proof).
        // They are not separate report fields.
        let profile_value = serde_json::to_value(profile).expect("profile serializes");
        assert!(profile_value.get("offline").is_none());
        assert!(profile_value.get("cancellation_bounded").is_none());
    }

    let value = serde_json::to_value(&report).expect("serialize direct report");
    let mut activation_claim = value.clone();
    activation_claim
        .as_object_mut()
        .expect("report object")
        .insert(
            "activation".to_owned(),
            serde_json::json!({"status": "qualified"}),
        );
    serde_json::from_value::<DirectEvaluationReportV1>(activation_claim)
        .expect_err("an activation object is an unknown field, not evidence");

    let mut qualified_status = value.clone();
    qualified_status["status"] = serde_json::json!("qualified");
    serde_json::from_value::<DirectEvaluationReportV1>(qualified_status)
        .expect_err("status:\"qualified\" is not an evidence status");

    // Unrelated future keys are not activation claims; deny_unknown_fields
    // refuses them as schema, not because their names contain "accepted".
    let mut unrelated = value;
    unrelated
        .as_object_mut()
        .expect("report object")
        .insert("accepted_languages".to_owned(), serde_json::json!(["rust"]));
    let unrelated_error = serde_json::from_value::<DirectEvaluationReportV1>(unrelated)
        .expect_err("unknown fields are refused by schema")
        .to_string();
    assert!(
        unrelated_error.contains("accepted_languages"),
        "{unrelated_error}"
    );

    // Workload documents are schema 1; candidate evidence is schema 2. The two
    // schemas have separate owners, and the gate refuses to conflate them.
    assert_eq!(workload.schema_version, 1);
    assert!(
        report
            .raw_outputs
            .iter()
            .all(|output| output.schema_version == 2)
    );
    let mut workload_schema = generated.clone();
    for output in &mut workload_schema.outputs {
        output.schema_version = workload.schema_version;
    }
    let error = evaluate_generated_outputs(repo_root, workload, &workload_schema)
        .expect_err("schema-1 evidence is refused rather than reinterpreted")
        .to_string();
    assert!(
        error.contains("unsupported candidate output schema"),
        "{error}"
    );
}

#[test]
fn baseline_report_is_self_validating_and_refuses_conceptual_misses() {
    if std::env::var_os(BASELINE_REPORT_RESOURCE_CHILD_ENV).is_none() {
        let output = std::process::Command::new(
            std::env::current_exe().expect("report test binary has a current executable"),
        )
        .args([
            "--exact",
            "report_tests::baseline_report_is_self_validating_and_refuses_conceptual_misses",
            "--nocapture",
        ])
        .env(BASELINE_REPORT_RESOURCE_CHILD_ENV, "1")
        .output()
        .expect("run baseline report in a dedicated process");
        assert!(
            output.status.success(),
            "dedicated baseline report failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let fixture = crate::candidate_output::tests::packaged_fixture();
    let repo_root = fixture.root();
    let workload = fixture.workload();
    let profile_ids = vec![QUERY_BASELINE_PROFILE.to_owned()];
    let generated = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
        repo_root,
        workload_path: None,
        profile_ids: Some(&profile_ids),
        admitted_scope: direct_fixture_scope,
    })
    .expect("generate direct fixture outputs");
    let report = evaluate_generated_outputs(repo_root, workload, &generated)
        .expect("evaluate direct fixture outputs");

    report
        .validate_against(repo_root, workload)
        .expect("baseline evidence remains self-validating");
    assert_eq!(
        report.status,
        crate::DirectEvaluationStatusV1::Fail,
        "the independently selected conceptual queries must retain lexical headroom"
    );

    let mut tampered = report.clone();
    tampered.raw_output_digest = "sha256:tampered".to_owned();
    let raw_error = tampered
        .validate_against(repo_root, workload)
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
