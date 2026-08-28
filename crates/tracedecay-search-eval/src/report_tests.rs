//! Direct-report evidence retention regressions.

use std::path::Path;

use crate::{
    DirectEvaluationStatusV1, DirectQueryEvaluationV1, DirectQueryQualityV1, DirectRatioMetricV1,
    GenerateCandidateOutputsOptions, QUERY_BASELINE_PROFILE, checked_in_fixture_root,
    compute_profile_material_digest, evaluate_generated_outputs, generate_candidate_outputs,
    load_candidate_workload,
};

const BASELINE_REPORT_RESOURCE_CHILD_ENV: &str = "TRACEDECAY_BASELINE_REPORT_RESOURCE_CHILD";

fn direct_fixture_scope(_repo_root: &Path) -> Option<tracedecay_application::ResolvedScope> {
    tracedecay_application::ResolvedScope::new(
        tracedecay_domain::ProjectId::new("project.search-eval-direct-report").ok()?,
        tracedecay_domain::RepositoryId::new("repository.search-eval-direct-report").ok()?,
        tracedecay_domain::WorktreeId::new("worktree.search-eval-direct-report").ok()?,
        None,
    )
    .ok()
}

fn diagnostic_query(query_id: &str, first_useful_rank: u32) -> DirectQueryEvaluationV1 {
    let zero = DirectRatioMetricV1 {
        numerator: 0,
        denominator: 0,
        ppm: 0,
    };
    DirectQueryEvaluationV1 {
        query_id: query_id.to_owned(),
        strata: vec!["natural_language".to_owned()],
        protected: false,
        first_useful_rank: Some(first_useful_rank),
        returned_candidates: 2,
        wrong_scope_hits: 0,
        forbidden_hits: 0,
        expected_no_result: false,
        quality: DirectQueryQualityV1 {
            recall_at_10: zero.clone(),
            precision_at_10: zero.clone(),
            reciprocal_rank_ppm: 0,
            ndcg_at_10_ppm: 0,
            duplicate_rate: zero,
        },
        status: DirectEvaluationStatusV1::Pass,
    }
}

#[test]
fn pairwise_diagnostic_prioritizes_queries_with_improvement_headroom() {
    let candidate = vec![
        diagnostic_query("already-perfect", 1),
        diagnostic_query("can-improve", 2),
    ];
    let baseline = candidate.clone();

    let ordered = crate::report::pairwise_query_pairs(&candidate, &baseline);

    assert_eq!(ordered.len(), 2);
    assert_eq!(ordered[0].0.query_id, "can-improve");
    assert_eq!(ordered[1].0.query_id, "already-perfect");
}

#[test]
fn semantic_distance_summary_exposes_absolute_confidence_and_ambiguity() {
    assert_eq!(
        crate::report::semantic_distance_summary([325_542_266, 325_542_266, 400_000_000]),
        "semantic_candidates=3,top_distance=325542266,second_distance=325542266,top_margin=0"
    );
    assert_eq!(
        crate::report::semantic_distance_summary(std::iter::empty()),
        "semantic_candidates=0,top_distance=none,second_distance=none,top_margin=none"
    );
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

mod semantic_cut_qualification {
    use crate::candidate_output::{
        HistoricalQueryExecutionV1, ProfileSpecV1, QueryCandidateRowV1, RankedCandidateRowV1,
        SemanticCandidateScoreRowV1, WorkloadQueryV1,
    };
    use crate::report::{
        PartitionedLabelledScoresV1, labelled_scores_for_query,
        require_declared_cut_matches_derivation,
    };
    use crate::semantic_cut::{
        LabelledSemanticRelevanceV1, LabelledSemanticScoreV1, SemanticCutV1,
        derive_and_validate_semantic_cut,
    };

    fn candidate(anchor: &str, document_id: &str) -> RankedCandidateRowV1 {
        RankedCandidateRowV1 {
            anchor: anchor.to_owned(),
            anchors: vec![anchor.to_owned()],
            scope: "research".to_owned(),
            document_id: document_id.to_owned(),
            tier: "approximate".to_owned(),
        }
    }

    fn scored(anchor: &str, document_id: &str, ppm: u32) -> SemanticCandidateScoreRowV1 {
        SemanticCandidateScoreRowV1 {
            candidate: candidate(anchor, document_id),
            calibrated_feature_micros: ppm,
        }
    }

    fn query(query_id: &str, label: serde_json::Value) -> WorkloadQueryV1 {
        WorkloadQueryV1 {
            query_id: query_id.to_owned(),
            partition: "train".to_owned(),
            strata: vec!["natural_language".to_owned()],
            query: query_id.to_owned(),
            allowed_scopes: vec!["research".to_owned()],
            historical_commit: None,
            label: Some(label),
        }
    }

    fn row(
        query_id: &str,
        semantic_scores: Vec<SemanticCandidateScoreRowV1>,
    ) -> QueryCandidateRowV1 {
        QueryCandidateRowV1 {
            query_id: query_id.to_owned(),
            ranked: Vec::new(),
            abstained: true,
            historical: HistoricalQueryExecutionV1::NotRequested,
            native: None,
            semantic_scores,
        }
    }

    /// The labelled anchor is a positive, an explicitly forbidden document is a
    /// negative, and a candidate the label says nothing about carries no
    /// judgment at all rather than being guessed into one side.
    #[test]
    fn a_labelled_query_scores_its_anchors_and_forbidden_documents_only() {
        let labelled = labelled_scores_for_query(
            &query(
                "train-007",
                serde_json::json!({
                    "anchors": ["canonical::canonical_sha256"],
                    "forbidden_documents": ["session"],
                }),
            ),
            &row(
                "train-007",
                vec![
                    scored("canonical::canonical_sha256", "canonical", 780_000),
                    scored("session::SessionId", "session", 420_000),
                    scored("repository::evidence", "repository", 510_000),
                ],
            ),
        )
        .expect("labelled scores");

        assert_eq!(
            labelled.len(),
            2,
            "the unlabelled candidate carries no judgment"
        );
        assert_eq!(labelled[0].calibrated_feature_micros, 780_000);
        assert_eq!(labelled[0].relevance, LabelledSemanticRelevanceV1::Positive);
        assert_eq!(labelled[1].calibrated_feature_micros, 420_000);
        assert_eq!(labelled[1].relevance, LabelledSemanticRelevanceV1::Negative);
        assert_eq!(labelled[0].strata, vec!["natural_language".to_owned()]);
    }

    /// A `no_answer` query names no relevant anchor, so every candidate the
    /// semantic lane returned for it is a negative. These are the workload's
    /// densest source of labelled negatives.
    #[test]
    fn a_no_answer_query_scores_every_candidate_as_a_negative() {
        let labelled = labelled_scores_for_query(
            &query(
                "train-009",
                serde_json::json!({
                    "anchors": [],
                    "absence_literal": "zzqxv_owner_train_absent_9347",
                }),
            ),
            &row(
                "train-009",
                vec![
                    scored("canonical::canonical_sha256", "canonical", 310_000),
                    scored("repository::evidence", "repository", 288_000),
                ],
            ),
        )
        .expect("labelled scores");

        assert_eq!(labelled.len(), 2);
        assert!(
            labelled
                .iter()
                .all(|score| score.relevance == LabelledSemanticRelevanceV1::Negative),
            "a query with no correct answer cannot produce a correct candidate"
        );
    }

    fn separated_scores(floor_ppm: u32) -> Vec<LabelledSemanticScoreV1> {
        (0..12_u32)
            .flat_map(|index| {
                let step = index * 10_000;
                [
                    LabelledSemanticScoreV1 {
                        query_id: format!("q-{index:03}"),
                        strata: vec!["natural_language".to_owned()],
                        calibrated_feature_micros: floor_ppm + step,
                        relevance: LabelledSemanticRelevanceV1::Positive,
                    },
                    LabelledSemanticScoreV1 {
                        query_id: format!("q-{index:03}-absent"),
                        strata: vec!["no_answer".to_owned()],
                        calibrated_feature_micros: floor_ppm - 200_000 + step,
                        relevance: LabelledSemanticRelevanceV1::Negative,
                    },
                ]
            })
            .collect()
    }

    fn profile(semantic_cut: SemanticCutV1) -> ProfileSpecV1 {
        ProfileSpecV1 {
            profile_id: "hybrid-conservative".to_owned(),
            lexical_weight_ppm: 1_000_000,
            graph_weight_ppm: 250_000,
            semantic_weight_ppm: 250_000,
            rerank_weight_ppm: 0,
            semantic_cut,
            rerank_policy: None,
        }
    }

    fn labelled(floor_ppm: u32) -> PartitionedLabelledScoresV1 {
        PartitionedLabelledScoresV1 {
            train: separated_scores(floor_ppm),
            validation: separated_scores(floor_ppm),
        }
    }

    #[test]
    fn a_profile_declaring_the_measured_derivation_qualifies() {
        let scores = labelled(700_000);
        let derived = derive_and_validate_semantic_cut(&scores.train, &scores.validation)
            .expect("the measured population derives a cut");
        assert_eq!(derived.threshold_ppm(), 700_000);

        require_declared_cut_matches_derivation(&profile(derived), &scores)
            .expect("a declaration that is the derivation qualifies");
    }

    /// The anti-tuning property, stated as a test: a hand-written cut cannot
    /// survive a run whose own scores derive something else.
    #[test]
    fn a_hand_tuned_cut_cannot_survive_the_run_that_measures_it() {
        let scores = labelled(700_000);
        let honest =
            derive_and_validate_semantic_cut(&scores.train, &scores.validation).expect("derived");
        let SemanticCutV1::Derived { provenance, .. } = honest else {
            panic!("expected a derived cut");
        };

        // The exact edit history this replaces: a number moved to clear a gate,
        // keeping the provenance of the honest derivation around it.
        let tuned = SemanticCutV1::Derived {
            threshold_ppm: 635_000,
            provenance,
        };

        let error = require_declared_cut_matches_derivation(&profile(tuned), &scores)
            .expect_err("a tuned cut must not qualify");
        let rendered = error.to_string();
        for expected in [
            "declares semantic cut 635000 ppm",
            "derive 700000 ppm",
            "do not edit the cut by hand",
        ] {
            assert!(
                rendered.contains(expected),
                "{expected:?} missing from {rendered:?}"
            );
        }
    }

    /// A profile that has never been calibrated cannot silently keep admitting
    /// everything once a run measures a real separation: qualification says so
    /// and names what to stamp.
    #[test]
    fn an_unmeasured_declaration_fails_once_a_run_measures_a_separation() {
        let scores = labelled(700_000);
        let error =
            require_declared_cut_matches_derivation(&profile(SemanticCutV1::Unmeasured), &scores)
                .expect_err("an unmeasured declaration cannot outlive its measurement");
        let rendered = error.to_string();
        assert!(rendered.contains("declares semantic cut 0 ppm (unmeasured)"));
        assert!(rendered.contains("derive 700000 ppm (derived)"));
    }

    /// A cut that does not generalize fails with the held-out diagnostic rather
    /// than being quietly replaced by a constant.
    #[test]
    fn a_cut_that_fails_on_validation_reports_the_stratum_it_lost() {
        let mut scores = labelled(700_000);
        scores.validation.push(LabelledSemanticScoreV1 {
            query_id: "validation-006".to_owned(),
            strata: vec!["natural_language".to_owned()],
            calibrated_feature_micros: 410_000,
            relevance: LabelledSemanticRelevanceV1::Positive,
        });

        let error =
            require_declared_cut_matches_derivation(&profile(SemanticCutV1::Unmeasured), &scores)
                .expect_err("a cut that drops a held-out positive must not qualify");
        let rendered = error.to_string();
        assert!(rendered.contains("does not hold on the held-out partition"));
        assert!(rendered.contains("worst stratum natural_language"));
        assert!(rendered.contains("410000 ppm"));
    }
}
