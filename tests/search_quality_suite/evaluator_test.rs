use std::fs;

use tracedecay::search_eval::{CompareOptions, compare, validate_fixture_root};
use tracedecay_domain::{
    CandidateListV1, EvalOutcomeV1, EvalRunScopeV1, FixtureAuthorityV1, RetrieverLaneId, RunId,
    RunManifestV1, SavedCandidateSetDigest, SavedCandidateSetV1,
};

use crate::fixtures;

#[test]
fn canonical_validator_accepts_only_the_frozen_real_fixture_packet() {
    let validation = validate_fixture_root(&fixtures::fixture_root()).unwrap();
    assert_eq!(validation.authority, FixtureAuthorityV1::ContractOnly);
    assert_eq!(
        validation.fixture_manifest_digest.as_str(),
        "sha256:5da56cc98447ff962e62ad0cb2757e0c4936f514dfa82265c5402bcc2b56a4f4"
    );
    assert_eq!(
        validation.run_manifest_digest.as_str(),
        "sha256:3db745fc343cbfc00e9bbb173979474bdd3d324e5f5bf87c65f2357e803f3fe1"
    );
}

#[test]
fn contract_only_compare_blocks_without_opening_holdout_or_creating_promotion() {
    let temp = tempfile::tempdir().unwrap();

    let result = compare(&CompareOptions {
        fixture_root: fixtures::fixture_root(),
        run_manifest: None,
        output_root: temp.path().join("runs"),
        holdout_labels: None,
        saved_candidates: None,
        required_outcome: Some(EvalOutcomeV1::Accepted),
    })
    .unwrap();

    assert_eq!(result.outcome, EvalOutcomeV1::Blocked);
    assert!(!result.requirement_satisfied);
    assert_eq!(result.blocked_on.len(), 1);
    assert_eq!(
        result.blocked_on[0].locator,
        "direct-filesystem://search-quality/holdout/judgments-v1"
    );
    assert!(
        !result
            .report_path
            .parent()
            .unwrap()
            .join("promotion-v1.json")
            .exists()
    );
}

#[test]
fn evaluator_rejects_run_ids_that_are_not_single_path_components() {
    let temp = tempfile::tempdir().unwrap();
    let mut run: RunManifestV1 = serde_json::from_slice(
        &fs::read(fixtures::fixture_root().join("run-contract-v1.json")).unwrap(),
    )
    .unwrap();
    run.run_id = RunId::new("../escape").unwrap();
    run.digest = run.compute_digest().unwrap();
    let run_path = temp.path().join("run.json");
    fs::write(&run_path, serde_json::to_vec_pretty(&run).unwrap()).unwrap();

    let error = compare(&CompareOptions {
        fixture_root: fixtures::fixture_root(),
        run_manifest: Some(run_path),
        output_root: temp.path().join("runs"),
        holdout_labels: None,
        saved_candidates: None,
        required_outcome: Some(EvalOutcomeV1::Accepted),
    })
    .unwrap_err();

    assert!(error.to_string().contains("single filesystem component"));
    assert!(!temp.path().join("escape").exists());
}

#[test]
fn compare_reports_each_lane_ablation_from_saved_candidate_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let workload = fixtures::load_workload();
    let run = fixtures::load_fixture_bundle().run;
    let exact = RetrieverLaneId::new("exact").unwrap();
    let lexical = RetrieverLaneId::new("lexical").unwrap();
    let candidate_lists = workload
        .development_queries()
        .flat_map(|query| {
            [&exact, &lexical].map(|lane| CandidateListV1 {
                query_id: query.query_id.clone(),
                lane: lane.clone(),
                candidates: Vec::new(),
            })
        })
        .collect();
    let mut saved = SavedCandidateSetV1 {
        schema_revision: 1,
        run_id: run.run_id.clone(),
        run_manifest_digest: run.digest.clone(),
        scope: EvalRunScopeV1::Development,
        workload_digest: workload.digest.clone(),
        candidate_lists,
        digest: SavedCandidateSetDigest::new(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap(),
    };
    saved.digest = saved.compute_digest().unwrap();
    let saved_path = temp.path().join("saved-candidates.json");
    fs::write(&saved_path, serde_json::to_vec_pretty(&saved).unwrap()).unwrap();

    let result = compare(&CompareOptions {
        fixture_root: fixtures::fixture_root(),
        run_manifest: None,
        output_root: temp.path().join("runs"),
        holdout_labels: None,
        saved_candidates: Some(saved_path),
        required_outcome: Some(EvalOutcomeV1::Accepted),
    })
    .unwrap();

    assert_eq!(
        result
            .saved_candidate_ablations
            .iter()
            .map(|ablation| ablation.disabled_lane.as_str())
            .collect::<Vec<_>>(),
        vec!["exact", "lexical"]
    );
}
