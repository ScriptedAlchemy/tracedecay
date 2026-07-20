use std::fs;
use std::path::{Path, PathBuf};

use tracedecay_domain::{
    CandidateListV1, DecisionOwnerId, EvalCandidateAnchorV1, EvalCandidateV1, EvalOutcomeV1,
    EvalPartitionV1, EvalQueryV1, EvalRunScopeV1, EvaluationFixtureBundleV1, EvidenceIndexV1,
    FixtureAuthorityV1, FixtureManifestV1, HoldoutRevealCapabilityV1, LabelSetDigest, LabelSetId,
    LabelSetV1, QueryWorkloadV1, RelevanceJudgmentV1, RetrieverLaneId, RunManifestV1,
    SavedCandidateSetDigest, SavedCandidateSetV1, WorkloadDigest,
};

const ZERO_DIGEST: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/search_quality")
}

fn read_json<T: serde::de::DeserializeOwned>(name: &str) -> T {
    serde_json::from_slice(
        &fs::read(fixture_root().join(name)).unwrap_or_else(|error| panic!("{name}: {error}")),
    )
    .unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn read_jsonl<T: serde::de::DeserializeOwned>(name: &str) -> Vec<T> {
    fs::read_to_string(fixture_root().join(name))
        .unwrap_or_else(|error| panic!("{name}: {error}"))
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn workload() -> QueryWorkloadV1 {
    let mut workload = QueryWorkloadV1 {
        revision: 1,
        queries: read_jsonl::<EvalQueryV1>("queries-v1.jsonl"),
        digest: WorkloadDigest::new(ZERO_DIGEST).unwrap(),
    };
    workload.digest = workload.compute_digest().unwrap();
    workload
}

fn development_labels() -> LabelSetV1 {
    let mut labels = LabelSetV1 {
        label_set_id: LabelSetId::new("labels-development-v1").unwrap(),
        revision: 1,
        partition: EvalPartitionV1::Development,
        judgments: read_jsonl::<RelevanceJudgmentV1>("judgments-development-v1.jsonl"),
        digest: LabelSetDigest::new(ZERO_DIGEST).unwrap(),
    };
    labels.digest = labels.compute_digest().unwrap();
    labels
}

fn fixture_bundle() -> EvaluationFixtureBundleV1 {
    EvaluationFixtureBundleV1 {
        manifest: read_json("fixture-manifest-v1.json"),
        workload: workload(),
        snapshots: read_jsonl("snapshots-v1.jsonl"),
        temporal_events: read_jsonl("temporal-events-v1.jsonl"),
        context_spans: read_jsonl("context-spans-v1.jsonl"),
        tasks: read_jsonl("tasks-v1.jsonl"),
        authorization_canaries: read_jsonl("authorization-canaries-v1.jsonl"),
        exact_admission_oracles: read_jsonl("exact-admission-oracles-v1.jsonl"),
        contamination_partitions: read_json("contamination-partitions-v1.json"),
        development_labels: development_labels(),
        run: read_json("run-contract-v1.json"),
        evidence_index: read_json::<EvidenceIndexV1>("evidence-index.json"),
    }
}

fn locked_manifest(fixture: &FixtureManifestV1, workload: &QueryWorkloadV1) -> RunManifestV1 {
    let mut run: RunManifestV1 = read_json("run-contract-v1.json");
    run.run_id = tracedecay_domain::RunId::new("run-search-quality-locked-v1").unwrap();
    run.revision = 1;
    run.scope = EvalRunScopeV1::Locked;
    run.authority = FixtureAuthorityV1::LockedQuality;
    run.candidate_revision = "pr9-exact-lexical-graph-candidate-v1".to_string();
    run.profile_matrix = vec![
        "baseline".to_string(),
        "candidate".to_string(),
        "candidate-minus-exact".to_string(),
        "candidate-minus-lexical".to_string(),
        "candidate-minus-graph".to_string(),
    ];
    run.cache_states = vec!["cold".to_string(), "warm".to_string()];
    run.execution_order = workload
        .sealed_holdout_queries()
        .map(|query| query.query_id.clone())
        .collect();
    run.locked_outcomes_accessed = false;
    run.holdout_seal_digest = fixture.holdout_seal.seal_digest.clone();
    run.digest = run.compute_digest().unwrap();
    run
}

#[test]
fn pre_reveal_validation_rejects_contract_only_authority() {
    let bundle = fixture_bundle();
    let error = bundle
        .run
        .validate_pre_reveal(&bundle.manifest, &bundle.workload)
        .unwrap_err();
    assert!(error.to_string().contains("locked-quality"));
}

#[test]
fn reveal_capability_binds_locator_seal_and_frozen_run_digest() {
    let mut manifest: FixtureManifestV1 = read_json("fixture-manifest-v1.json");
    manifest.authority = FixtureAuthorityV1::LockedQuality;
    let workload = workload();
    let run = locked_manifest(&manifest, &workload);
    run.validate_pre_reveal(&manifest, &workload).unwrap();

    let capability = HoldoutRevealCapabilityV1 {
        schema_revision: 1,
        locator: manifest.holdout_seal.locator.clone(),
        signature_locator: manifest.holdout_seal.signature_locator.clone(),
        seal_digest: manifest.holdout_seal.seal_digest.clone(),
        run_id: run.run_id.clone(),
        run_manifest_digest: run.digest.clone(),
        revealed_by: DecisionOwnerId::new("owner-search-quality-lead").unwrap(),
        sealed_labels_path: "/authorized/holdout/judgments-v1.jsonl".to_string(),
        signed_envelope_path: "/authorized/holdout/judgments-v1.signature".to_string(),
    };

    let receipt = capability
        .issue_receipt(&run, &manifest.holdout_seal, &manifest.decision_owners)
        .unwrap();
    assert_eq!(receipt.run_id, run.run_id);
    assert_eq!(receipt.run_manifest_digest, run.digest);
    assert_eq!(receipt.seal_digest, manifest.holdout_seal.seal_digest);

    let mut wrong_run = run.clone();
    wrong_run.candidate_revision.push_str("-changed-after-lock");
    wrong_run.digest = wrong_run.compute_digest().unwrap();
    assert!(
        capability
            .issue_receipt(
                &wrong_run,
                &manifest.holdout_seal,
                &manifest.decision_owners,
            )
            .unwrap_err()
            .to_string()
            .contains("run manifest digest")
    );
}

#[test]
fn saved_candidate_ablations_filter_frozen_lists_without_mutating_them() {
    let workload = workload();
    let run: RunManifestV1 = read_json("run-contract-v1.json");
    let exact = RetrieverLaneId::new("exact").unwrap();
    let lexical = RetrieverLaneId::new("lexical").unwrap();
    let lists: Vec<_> = workload
        .development_queries()
        .flat_map(|query| {
            [
                CandidateListV1 {
                    query_id: query.query_id.clone(),
                    lane: exact.clone(),
                    candidates: vec![EvalCandidateV1 {
                        anchor: EvalCandidateAnchorV1 {
                            document_id: tracedecay_domain::CorpusDocumentId::new(
                                "doc-research-time",
                            )
                            .unwrap(),
                            symbol: Some("UtcMicros".to_string()),
                        },
                        ordinal_rank: 0,
                    }],
                },
                CandidateListV1 {
                    query_id: query.query_id.clone(),
                    lane: lexical.clone(),
                    candidates: Vec::new(),
                },
            ]
        })
        .collect();
    let mut saved = SavedCandidateSetV1 {
        schema_revision: 1,
        run_id: run.run_id.clone(),
        run_manifest_digest: run.digest.clone(),
        scope: EvalRunScopeV1::Development,
        workload_digest: workload.digest.clone(),
        candidate_lists: lists.clone(),
        digest: SavedCandidateSetDigest::new(ZERO_DIGEST).unwrap(),
    };
    saved.digest = saved.compute_digest().unwrap();
    saved.validate_for_run(&run, &workload).unwrap();

    let ablated = saved.ablate_lanes(std::slice::from_ref(&lexical)).unwrap();
    assert_eq!(
        ablated,
        lists
            .iter()
            .filter(|list| list.lane == exact)
            .cloned()
            .collect::<Vec<_>>()
    );
    assert_eq!(saved.candidate_lists, lists);
    assert!(
        saved
            .ablate_lanes(&[RetrieverLaneId::new("semantic").unwrap()])
            .unwrap_err()
            .to_string()
            .contains("unknown saved-candidate lane")
    );
}

#[test]
fn contamination_membership_is_bidirectional() {
    let mut bundle = fixture_bundle();
    let extra_query = bundle.workload.queries[0].query_id.clone();
    bundle.contamination_partitions.groups[0]
        .query_ids
        .retain(|query_id| query_id != &extra_query);
    bundle.contamination_partitions.groups[1]
        .query_ids
        .push(extra_query);
    assert!(
        bundle
            .validate()
            .unwrap_err()
            .to_string()
            .contains("contamination membership")
    );
}

#[test]
fn terminal_outcome_strings_are_exact_and_round_trip() {
    for (outcome, expected) in [
        (EvalOutcomeV1::InvalidRun, "invalid_run"),
        (EvalOutcomeV1::Blocked, "blocked"),
        (EvalOutcomeV1::Rejected, "rejected"),
        (EvalOutcomeV1::Inconclusive, "inconclusive"),
        (
            EvalOutcomeV1::RuntimeFallbackObserved,
            "runtime_fallback_observed",
        ),
        (EvalOutcomeV1::Accepted, "accepted"),
    ] {
        assert_eq!(outcome.as_str(), expected);
        assert_eq!(expected.parse::<EvalOutcomeV1>().unwrap(), outcome);
    }
}
