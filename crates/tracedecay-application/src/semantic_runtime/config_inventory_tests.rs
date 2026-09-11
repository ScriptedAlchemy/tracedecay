use std::collections::BTreeMap;

use tracedecay_contracts::ResolvedScope;
use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotId};
use tracedecay_domain::{
    CalibrationProfileId, DiversityPolicy, FusionProfile, ManifestDigest, ProjectId, RepositoryId,
    RetrievalBudget, RetrieverKind, UtcMicros, VectorGenerationIdV1, WorktreeId,
};
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_query::search_quality::{
    DirectEvaluationReportV1, DirectEvaluationStatusV1, DirectProfileEvaluationV1,
    DirectQualityMetricsV1, DirectRatioMetricV1, EvaluationExecutionContractV1,
    OptionalStageMeasurementV1, OptionalStageMeasurementsV1,
};

use crate::config::retrieval::{
    AcceptedRetrievalProfileV1, PassingRetrievalEvaluationV1, RetrievalCompatibilityPinsV1,
    RetrievalProfileCasV1, RetrievalProfileCommitMetadataV1, RetrievalProfileMutationCapabilityV1,
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
                tracedecay_query::search_quality::candidate_output::EvaluationConcurrencyContractV1 {
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
        minimum_calibrated_feature_micros: BTreeMap::new(),
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

/// A grant minted for `revision`, already rechecked against a matching current
/// authorization. Only the configuration revision varies between callers.
fn capability(revision: ConfigurationRevisionId) -> RetrievalProfileMutationCapabilityV1 {
    use tracedecay_configuration::{
        ConfigurationMutationAuthority, CurrentConfigurationMutationAuthorizationV1,
    };
    use tracedecay_domain::configuration::{
        ConfigurationMutationEffectV1, ConfigurationMutationGrantReceiptV1,
        ConfigurationMutationOperationV1, ConfigurationMutationSinkV1,
    };

    let scope_digest = ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
    let policy_digest: tracedecay_domain::AccessPolicyDigest =
        typed(&format!("sha256:{}", "b".repeat(64)));
    RetrievalProfileMutationCapabilityV1::from_current_authorization(
        ConfigurationMutationAuthority {
            receipt: ConfigurationMutationGrantReceiptV1::issue(
                typed("configuration.grant-receipt.scope"),
                typed("configuration.grant.scope"),
                typed("actor.scope"),
                ConfigurationMutationOperationV1::DirectMutation,
                scope_digest.clone(),
                revision,
                1,
                policy_digest.clone(),
                ConfigurationMutationSinkV1::ConfigurationStore,
                ConfigurationMutationEffectV1::CommitConfigurationRevision,
                Some(typed("configuration.idempotency.scope")),
                UtcMicros(1),
                UtcMicros(100),
            )
            .unwrap(),
        },
        CurrentConfigurationMutationAuthorizationV1 {
            grant_revision: 1,
            grant_digest: scope_digest.clone(),
            scope_digest,
            policy_epoch: 1,
            policy_digest,
        },
    )
    .unwrap()
}

fn cas(state: &RetrievalProfileStateV1) -> RetrievalProfileCasV1 {
    RetrievalProfileCasV1 {
        expected_configuration_revision: state.configuration_revision().clone(),
        expected_active_digest: state.active().profile_digest().clone(),
        expected_rollback_digest: state.rollback_profile().map(|p| p.profile_digest().clone()),
    }
}

fn commit_metadata(
    base: ConfigurationRevisionId,
    result: ConfigurationRevisionId,
) -> RetrievalProfileCommitMetadataV1 {
    RetrievalProfileCommitMetadataV1::new(
        ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
        base,
        result,
        UtcMicros(2),
    )
}

fn compat(state: &RetrievalProfileStateV1) -> RetrievalRuntimeCompatibilityV1 {
    RetrievalRuntimeCompatibilityV1 {
        retrieval_ceiling: state.active().profile().retrieval_budget,
        semantic: None,
        semantic_ceiling: None,
        rerank: None,
        rerank_ceiling: None,
    }
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

#[tokio::test]
async fn absent_project_inventory_is_authoritative_until_profile_bootstrap() {
    let directory = tempfile::tempdir().expect("isolated profile");
    let runtime = RegisteredGlobalDbTestRuntime::profile(directory.path())
        .await
        .expect("profile database");
    let project = ProjectId::new("project.absent-inventory").expect("project");
    let store = ProductionSemanticRetrievalConfigurationStoreV1::open(
        runtime.profile_database_arc(),
        scope(&project, "repository.absent", "worktree.absent"),
    )
    .expect("configuration authority");
    assert!(store.current_committed_state().await.unwrap().is_none());
    let inventory = store
        .configuration_inventory_page(
            &SemanticConfigurationInventoryPageRequestV1::first(1).unwrap(),
        )
        .await
        .expect("authoritative empty inventory")
        .complete_receipt
        .expect("complete inventory");
    assert_eq!(inventory.revision(), None);
    assert_eq!(inventory.scope_count(), 0);
    assert_eq!(inventory.root_binding_count(), 0);
    let roots = store
        .configured_vector_roots_page(
            &SemanticConfiguredVectorRootPageRequestV1::first(inventory.clone(), 1).unwrap(),
        )
        .await
        .expect("empty configured roots");
    assert!(roots.roots.is_empty());
    let roots = roots.complete_receipt.expect("complete roots");
    assert_eq!(roots.revision(), None);
    assert_eq!(roots.root_count(), 0);
    assert!(store.current_committed_state().await.unwrap().is_none());

    // Another project's bootstrap cannot invalidate this project's absence.
    let foreign = ProductionSemanticRetrievalConfigurationStoreV1::open(
        runtime.profile_database_arc(),
        scope(
            &ProjectId::new("project.foreign-inventory").unwrap(),
            "repository.foreign",
            "worktree.foreign",
        ),
    )
    .unwrap();
    let (pin, state) = initial_state("a-foreign");
    foreign.install_initial_state(&pin, &state).await.unwrap();
    store
        .configured_vector_roots_page(
            &SemanticConfiguredVectorRootPageRequestV1::first(inventory.clone(), 1).unwrap(),
        )
        .await
        .expect("foreign bootstrap preserves exact absence");

    let (pin, state) = initial_state("b-local");
    store.install_initial_state(&pin, &state).await.unwrap();
    assert!(matches!(
        store
            .configured_vector_roots_page(
                &SemanticConfiguredVectorRootPageRequestV1::first(inventory, 1).unwrap(),
            )
            .await,
        Err(SemanticConfigurationBackendErrorV1::Conflict)
    ));
    let candidate = VectorGenerationIdV1::new(
        ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
    );
    assert!(matches!(
        store
            .is_vector_generation_configured(&roots, &candidate)
            .await,
        Err(SemanticConfigurationBackendErrorV1::Conflict)
    ));
    let published = store
        .configuration_inventory_page(
            &SemanticConfigurationInventoryPageRequestV1::first(1).unwrap(),
        )
        .await
        .unwrap()
        .complete_receipt
        .unwrap();
    assert!(published.revision().is_some());
    assert_eq!(published.scope_count(), 1);
}

#[test]
fn sibling_configuration_commits_preserve_scope_cas_and_reject_stale_same_scope() {
    use crate::config::retrieval::RetrievalProfileActivationErrorV1;

    let (_, mut primary) = initial_state("a");
    let mut sibling = primary.clone();
    let (_, candidate) = initial_state("b");
    let runtime = compat(&primary);
    let first_revision = primary.configuration_revision().clone();
    let sibling_revision = typed::<ConfigurationRevisionId>("configuration.sibling");
    let next_revision = typed::<ConfigurationRevisionId>("configuration.next");
    let initial = cas(&primary);
    primary
        .activate(
            &capability(first_revision.clone()),
            &initial,
            candidate.active().clone(),
            &runtime,
            &runtime,
            commit_metadata(first_revision.clone(), sibling_revision.clone()),
        )
        .unwrap();
    // A project commit must not change the sibling's scope token. Its new grant
    // authorizes the current project revision, while CAS checks its own state.
    sibling
        .activate(
            &capability(sibling_revision.clone()),
            &initial,
            candidate.active().clone(),
            &runtime,
            &runtime,
            commit_metadata(sibling_revision.clone(), next_revision.clone()),
        )
        .unwrap();
    sibling.snapshot().unwrap().into_state().unwrap();
    let committed = sibling.clone();
    assert_eq!(
        sibling.activate(
            &capability(next_revision.clone()),
            &initial,
            candidate.active().clone(),
            &runtime,
            &runtime,
            commit_metadata(next_revision.clone(), typed("configuration.stale")),
        ),
        Err(RetrievalProfileActivationErrorV1::CasConflict),
    );
    assert_eq!(sibling, committed);
    let expected = cas(&sibling);
    assert_eq!(
        sibling.rollback(
            &capability(sibling_revision.clone()),
            &expected,
            &runtime,
            "restore".into(),
            commit_metadata(next_revision.clone(), typed("configuration.denied")),
        ),
        Err(RetrievalProfileActivationErrorV1::Unauthorized),
    );
    assert_eq!(sibling, committed);
    // Unrelated project changes also leave rollback available.
    let settings_revision = typed::<ConfigurationRevisionId>("configuration.settings");
    sibling
        .rollback(
            &capability(settings_revision.clone()),
            &expected,
            &runtime,
            "restore".into(),
            commit_metadata(settings_revision, typed("configuration.restored")),
        )
        .unwrap();
    assert_eq!(sibling.active(), committed.rollback_profile().unwrap());
    sibling.snapshot().unwrap().into_state().unwrap();
    let restored_cas = cas(&sibling);
    let restored_revision = sibling.configuration_revision().clone();
    sibling
        .activate(
            &capability(restored_revision.clone()),
            &restored_cas,
            candidate.active().clone(),
            &runtime,
            &runtime,
            commit_metadata(restored_revision, typed("configuration.reactivated")),
        )
        .unwrap();
    assert_eq!(sibling.active(), committed.active());
    assert_eq!(sibling.rollback_profile(), committed.rollback_profile());
    // Matching digests after activate/rollback/activate must not admit an old CAS.
    let revision = sibling.configuration_revision().clone();
    assert_eq!(
        sibling.rollback(
            &capability(revision.clone()),
            &expected,
            &runtime,
            "stale restore".into(),
            commit_metadata(revision, typed("configuration.stale-aba")),
        ),
        Err(RetrievalProfileActivationErrorV1::CasConflict),
    );
}

/// A transition that lost a race on its own scope must stay a *retryable*
/// refusal.
///
/// Keying the compare-and-swap on the scope's own semantic token (rather than
/// the project configuration revision) removed the revision guard that used to
/// answer `Conflict` here, so the same-scope race fell through to the profile
/// CAS and was reported as a non-retryable rejection. The refusal is still
/// typed and the state is still untouched; only the classification is at risk,
/// and callers retry on `Conflict` alone.
#[tokio::test]
async fn concurrent_same_scope_transition_is_refused_as_a_retryable_conflict() {
    use tracedecay_domain::configuration::{ConfigurationLayerIdV1, SettingKey};
    use tracedecay_global_db::configuration::contracts::DirectConfigurationMutation;

    let directory = tempfile::tempdir().expect("temporary profile");
    let project = ProjectId::new("project.same-scope-race").expect("project");
    let scope = scope(&project, "repository.race", "worktree.race");
    let runtime = RegisteredGlobalDbTestRuntime::profile(&directory.path().join("profile"))
        .await
        .expect("open profile database");
    let store = ProductionSemanticRetrievalConfigurationStoreV1::open(
        runtime.profile_database_arc(),
        scope,
    )
    .expect("configuration store");

    let (base_pin, installed) = initial_state("a");
    store
        .install_initial_state(&base_pin, &installed)
        .await
        .expect("install initial state");
    let (result_pin, candidate) = initial_state("b");
    let compatibility = compat(&installed);
    let mutation = DirectConfigurationMutation::Unset {
        layer: ConfigurationLayerIdV1::Project {
            project_id: project.clone(),
        },
        key: SettingKey::new("semantic.runtime").expect("setting key"),
    };
    let freshness = ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).expect("digest");

    // The winner of the race commits this scope's semantic state; the loser is
    // still holding the compare-and-swap token it read beforehand.
    let mut won = installed.clone();
    won.activate(
        &capability(base_pin.revision_id.clone()),
        &cas(&installed),
        candidate.active().clone(),
        &compatibility,
        &compatibility,
        commit_metadata(base_pin.revision_id.clone(), result_pin.revision_id.clone()),
    )
    .expect("the winning same-scope transition");
    assert_ne!(cas(&won), cas(&installed));

    let refusal = store
        .stage_activation(
            base_pin.clone(),
            result_pin.clone(),
            &capability(base_pin.revision_id.clone()),
            cas(&won),
            candidate.active().clone(),
            &compatibility,
            &compatibility,
            mutation.clone(),
            freshness.clone(),
            UtcMicros(2),
        )
        .await
        .expect_err("a lost same-scope race must not stage");
    assert_eq!(refusal, SemanticConfigurationBackendErrorV1::Conflict);

    // Not every refusal became retryable: an unusable grant is still a typed,
    // non-retryable rejection that names its stage.
    let unauthorized = store
        .stage_activation(
            base_pin.clone(),
            result_pin.clone(),
            &capability(typed::<ConfigurationRevisionId>("configuration.elsewhere")),
            cas(&installed),
            candidate.active().clone(),
            &compatibility,
            &compatibility,
            mutation,
            freshness,
            UtcMicros(2),
        )
        .await
        .expect_err("a grant minted for another revision must not stage");
    assert_eq!(
        unauthorized,
        SemanticConfigurationBackendErrorV1::RejectedAt("stage_activation.state_activate"),
    );

    // Neither refusal moved the scope.
    assert_eq!(
        store
            .current_state_if_present()
            .await
            .expect("read back the scope state"),
        Some(installed),
    );
}
