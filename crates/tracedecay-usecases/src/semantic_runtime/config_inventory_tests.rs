use std::collections::BTreeMap;

use tracedecay_application::ResolvedScope;
use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotId};
use tracedecay_domain::{
    CalibrationProfileId, DiversityPolicy, FusionProfile, ManifestDigest, ProjectId, RepositoryId,
    RetrievalBudget, RetrieverKind, VectorGenerationIdV1, WorktreeId,
};
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_search_eval::{
    DirectEvaluationReportV1, DirectEvaluationStatusV1, DirectProfileEvaluationV1,
    DirectQualityMetricsV1, DirectRatioMetricV1, EvaluationExecutionContractV1,
    OptionalStageMeasurementV1, OptionalStageMeasurementsV1,
};

use crate::config::retrieval::{
    AcceptedRetrievalProfileV1, PassingRetrievalEvaluationV1, RetrievalCompatibilityPinsV1,
    RetrievalProfileStateV1, RetrievalRuntimeCompatibilityV1,
};
use crate::semantic_runtime::{
    ProductionSemanticRetrievalConfigurationStoreV1, SemanticConfigurationBackendErrorV1,
    SemanticConfigurationInventoryPageRequestV1, SemanticConfigurationPinV1,
    SemanticConfiguredVectorRootPageRequestV1,
};

fn typed<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("typed fixture identity")
}

fn passing_report(profile_id: &str) -> DirectEvaluationReportV1 {
    let empty_ratio = || DirectRatioMetricV1 {
        numerator: 0,
        denominator: 0,
        ppm: 0,
    };
    let row = |partition: &str| DirectProfileEvaluationV1 {
        profile_id: profile_id.to_owned(),
        partition: partition.to_owned(),
        query_count: 0,
        failed_queries: 0,
        fallback_stable: true,
        fallback_matches_expected: true,
        cancellation_bounded: true,
        offline: true,
        resource_status: DirectEvaluationStatusV1::Pass,
        optional_stages: OptionalStageMeasurementsV1 {
            semantic: OptionalStageMeasurementV1::NotRequested,
            rerank: OptionalStageMeasurementV1::NotRequested,
        },
        quality: DirectQualityMetricsV1 {
            relevant_query_count: 0,
            recall_at_10: empty_ratio(),
            precision_at_10: empty_ratio(),
            mean_reciprocal_rank_ppm: 0,
            ndcg_at_10_ppm: 0,
            duplicate_rate: empty_ratio(),
            protected_recall_at_10: empty_ratio(),
            strata: Vec::new(),
            worst_stratum: None,
        },
        status: DirectEvaluationStatusV1::Pass,
        queries: Vec::new(),
    };
    DirectEvaluationReportV1 {
        command: "compare".to_owned(),
        status: DirectEvaluationStatusV1::Pass,
        workload_digest: "workload.inventory-test".to_owned(),
        corpus_digest: "corpus.inventory-test".to_owned(),
        fixture_source_repository_commit: "commit.inventory-test".to_owned(),
        fixture_source_repository_tree: "tree.inventory-test".to_owned(),
        execution_contract: EvaluationExecutionContractV1 {
            exact_file_count: 0,
            exact_corpus_bytes: 0,
            exact_eligible_chunks_current: 0,
            exact_eligible_chunks_10x: 0,
            exact_query_count: 0,
            model_revision: "model.inventory-test.v1".to_owned(),
            projection_revision: "projection.inventory-test.v1".to_owned(),
            fusion_revision: "fusion.inventory-test.v1".to_owned(),
            runtime_revision: "runtime.inventory-test.v1".to_owned(),
            cache_state: "empty".to_owned(),
            concurrency:
                tracedecay_search_eval::candidate_output::EvaluationConcurrencyContractV1 {
                    query_workers: 1,
                    projection_workers: 1,
                    query_execution: "serial".to_owned(),
                },
        },
        profile_material_digests: BTreeMap::new(),
        raw_output_digest: "sha256:inventory-test".to_owned(),
        raw_outputs: Vec::new(),
        profiles: vec![row("train"), row("validation")],
    }
}

fn initial_state(label: &str) -> (SemanticConfigurationPinV1, RetrievalProfileStateV1) {
    let evaluation =
        PassingRetrievalEvaluationV1::from_report(&passing_report(label), label).expect("passing");
    let budget = RetrievalBudget {
        max_candidates_per_lane: 8,
        max_fused_candidates: 8,
        max_hydrated_results: 4,
        max_hydration_bytes: 4096,
        deadline_micros: None,
    };
    let profile = FusionProfile {
        profile_id: typed(&format!("profile.{label}")),
        evaluation_result_anchor: evaluation.evaluation_anchor().clone(),
        calibrations: RetrieverKind::QUERY_FALLBACK_LANES
            .into_iter()
            .map(|lane| {
                (
                    lane,
                    typed::<CalibrationProfileId>(&format!(
                        "calibration.{}.{}",
                        lane.as_str(),
                        label
                    )),
                )
            })
            .collect(),
        score_domain_calibrations: BTreeMap::new(),
        weights_micros: RetrieverKind::QUERY_FALLBACK_LANES
            .into_iter()
            .map(|lane| (lane, 1))
            .collect(),
        diversity_policy_id: typed(&format!("diversity.{label}")),
        rerank_policy_id: None,
        retrieval_budget: budget,
    };
    let accepted = AcceptedRetrievalProfileV1::new(
        profile.clone(),
        DiversityPolicy {
            policy_id: profile.diversity_policy_id.clone(),
            evaluation_result_anchor: Some(profile.evaluation_result_anchor.clone()),
            per_source_namespace: None,
            per_source_instance: None,
            per_repository: None,
            per_file: None,
            per_session_or_thread: None,
            per_copy_cluster: None,
            per_evidence_role: None,
        },
        None,
        RetrievalCompatibilityPinsV1::default(),
        evaluation,
    )
    .expect("accepted query fallback");
    let revision = typed::<ConfigurationRevisionId>(&format!("configuration.{label}"));
    let state = RetrievalProfileStateV1::new(
        revision.clone(),
        accepted,
        &RetrievalRuntimeCompatibilityV1 {
            retrieval_ceiling: budget,
            semantic: None,
            semantic_ceiling: None,
            rerank: None,
            rerank_ceiling: None,
        },
    )
    .expect("initial state");
    (
        SemanticConfigurationPinV1 {
            revision_id: revision,
            snapshot_id: typed::<ConfigurationSnapshotId>(&format!("snapshot.{label}")),
            effective_behavior_digest: ManifestDigest::new(format!(
                "sha256:{}",
                label.chars().next().expect("label").to_string().repeat(64)
            ))
            .expect("digest"),
        },
        state,
    )
}

fn scope(project: &ProjectId, repository: &str, worktree: &str) -> ResolvedScope {
    ResolvedScope::new(
        project.clone(),
        RepositoryId::new(repository).expect("repository"),
        WorktreeId::new(worktree).expect("worktree"),
        None,
    )
    .expect("scope")
}

#[tokio::test]
async fn project_inventory_survives_restart_and_foreign_project_churn() {
    let directory = tempfile::tempdir().expect("temporary profile");
    let profile_root = directory.path().join("profile");
    let project_a = ProjectId::new("project.inventory-a").expect("project");
    let project_b = ProjectId::new("project.inventory-b").expect("project");
    let scope_a1 = scope(&project_a, "repository.inventory", "worktree.inventory-a1");
    let scope_a2 = scope(&project_a, "repository.inventory", "worktree.inventory-a2");
    let scope_b1 = scope(&project_b, "repository.foreign", "worktree.inventory-b1");
    let scope_b2 = scope(&project_b, "repository.foreign", "worktree.inventory-b2");

    let runtime = RegisteredGlobalDbTestRuntime::profile(&profile_root)
        .await
        .expect("open profile database");
    let database = runtime.profile_database_arc();
    for (scope, label) in [(&scope_a1, "a1"), (&scope_a2, "a2"), (&scope_b1, "b1")] {
        let store =
            ProductionSemanticRetrievalConfigurationStoreV1::open(database.clone(), scope.clone())
                .expect("configuration store");
        let (pin, state) = initial_state(label);
        store
            .install_initial_state(&pin, &state)
            .await
            .expect("install initial state");
    }
    let store_a =
        ProductionSemanticRetrievalConfigurationStoreV1::open(database.clone(), scope_a1.clone())
            .expect("project A inventory");
    let first = store_a
        .configuration_inventory_page(
            &SemanticConfigurationInventoryPageRequestV1::first(1).expect("request"),
        )
        .await
        .expect("first project A page");
    let cursor = first.continuation.expect("second project A scope");

    let store_b =
        ProductionSemanticRetrievalConfigurationStoreV1::open(database.clone(), scope_b2.clone())
            .expect("project B store");
    let (pin_b2, state_b2) = initial_state("b2");
    store_b
        .install_initial_state(&pin_b2, &state_b2)
        .await
        .expect("foreign project mutation");
    drop((store_a, store_b, database, runtime));

    let restarted = RegisteredGlobalDbTestRuntime::profile(&profile_root)
        .await
        .expect("restart profile database");
    let restarted_store = ProductionSemanticRetrievalConfigurationStoreV1::open(
        restarted.profile_database_arc(),
        scope_a1,
    )
    .expect("restarted project A store");
    let final_page = restarted_store
        .configuration_inventory_page(
            &SemanticConfigurationInventoryPageRequestV1::after(cursor, 1).expect("continuation"),
        )
        .await
        .expect("resume project A inventory");
    let receipt = final_page.complete_receipt.expect("complete inventory");
    assert_eq!(receipt.scope_count(), 2);
    assert_eq!(receipt.root_binding_count(), 0);

    let stale_receipt = receipt.clone();
    let root_page = restarted_store
        .configured_vector_roots_page(
            &SemanticConfiguredVectorRootPageRequestV1::first(receipt, 1).expect("root request"),
        )
        .await
        .expect("root inventory");
    assert!(root_page.roots.is_empty());
    let root_receipt = root_page.complete_receipt.expect("complete roots");
    assert_eq!(root_receipt.root_count(), 0);

    let scope_a3 = scope(&project_a, "repository.inventory", "worktree.inventory-a3");
    let changed_store = ProductionSemanticRetrievalConfigurationStoreV1::open(
        restarted.profile_database_arc(),
        scope_a3,
    )
    .expect("changed project store");
    let (pin_a3, state_a3) = initial_state("a3");
    changed_store
        .install_initial_state(&pin_a3, &state_a3)
        .await
        .expect("same-project mutation");
    assert_eq!(
        restarted_store
            .configuration_inventory_page(
                &SemanticConfigurationInventoryPageRequestV1::first(1).expect("fresh request")
            )
            .await
            .expect("fresh inventory")
            .scanned_scopes,
        1
    );
    assert_eq!(
        restarted_store
            .configured_vector_roots_page(
                &SemanticConfiguredVectorRootPageRequestV1::first(stale_receipt, 1,)
                    .expect("stale root request"),
            )
            .await
            .expect_err("same-project mutation invalidates receipt"),
        SemanticConfigurationBackendErrorV1::Conflict
    );
    assert_eq!(
        restarted_store
            .is_vector_generation_configured(
                &root_receipt,
                &VectorGenerationIdV1::new(
                    ManifestDigest::new(format!("sha256:{}", "f".repeat(64))).expect("generation"),
                ),
            )
            .await
            .expect_err("same-project mutation invalidates root receipt"),
        SemanticConfigurationBackendErrorV1::Conflict
    );
}
