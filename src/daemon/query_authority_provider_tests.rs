use super::*;
use crate::application::semantic_runtime::{
    CommittedRetrievalProfileStateV1, SemanticActivationCommandV1, SemanticActivationReceiptV1,
    SemanticActivationRequestV1, SemanticConfigurationPinV1, SemanticCurrentLinkedActivationV1,
    project_semantic_redundancy_revision,
};
use crate::config::retrieval::{
    PassingRetrievalEvaluationV1, RetrievalCompatibilityPinsV1, RetrievalProfileAuditEventV1,
    RetrievalProfileStateSnapshotV1, RetrievalRuntimeCompatibilityV1, SemanticCompatibilityPinsV1,
    SemanticResourceRequirementV1,
};
use crate::search_eval::{
    DirectEvaluationReportV1, DirectEvaluationStatusV1, DirectProfileEvaluationV1,
    DirectQualityMetricsV1, DirectRatioMetricV1, EvaluationExecutionContractV1,
    OptionalStageMeasurementV1, OptionalStageMeasurementsV1,
};
use std::{collections::BTreeMap, path::Path, process::Command, time::Duration};
use tempfile::TempDir;
use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotId};
use tracedecay_domain::{
    CalibrationProfileId, ChunkerRevision, CodeGenerationId, ComponentRevision, DiversityPolicy,
    EmbeddingDeviceClassV1, EmbeddingMetricV1, EmbeddingNormalizationV1, EmbeddingPoolingV1,
    EmbeddingPrecisionV1, EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1,
    FreshnessVectorDigest, FusionProfile, ManifestDigest, ProjectId, RetrievalBudget, UtcMicros,
    VectorGenerationIdV1, canonical_sha256,
};
use tracedecay_domain::{
    EphemeralSanitizedQueryViewV1, PrincipalId, QueryNormalizationRevision, RepositoryId,
    RetrievalRequest, RetrievalScope, RetrievalSnapshot, RetrieverBatch, RetrieverOutcome,
    SanitizerRevision, SingleRootScopeV1, TemporalModeV1, VectorWatermark, WorktreeId,
};
use tracedecay_query::retrieval::semantic::SemanticCalibrationProfileV1;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).expect("fixture id")
}

fn passing_report(evaluated_profile_id: &str) -> DirectEvaluationReportV1 {
    let empty_ratio = || DirectRatioMetricV1 {
        numerator: 0,
        denominator: 0,
        ppm: 0,
    };
    let row = |partition: &str| DirectProfileEvaluationV1 {
        profile_id: evaluated_profile_id.to_owned(),
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
        workload_digest: "workload".to_owned(),
        corpus_digest: "corpus".to_owned(),
        fixture_source_repository_commit: "commit".to_owned(),
        fixture_source_repository_tree: "tree".to_owned(),
        execution_contract: EvaluationExecutionContractV1 {
            exact_file_count: 0,
            exact_corpus_bytes: 0,
            exact_eligible_chunks_current: 0,
            exact_eligible_chunks_10x: 0,
            exact_query_count: 0,
            model_revision: "model.aggregate-only-test.v1".to_owned(),
            projection_revision: "projection.aggregate-only-test.v1".to_owned(),
            fusion_revision: "fusion.aggregate-only-test.v1".to_owned(),
            runtime_revision: "runtime.aggregate-only-test.v1".to_owned(),
            cache_state: "empty".to_owned(),
            concurrency: crate::search_eval::candidate_output::EvaluationConcurrencyContractV1 {
                query_workers: 1,
                projection_workers: 1,
                query_execution: "serial".to_owned(),
            },
        },
        profile_material_digests: BTreeMap::new(),
        raw_output_digest: "sha256:aggregate-only-test".to_owned(),
        raw_outputs: Vec::new(),
        profiles: vec![row("train"), row("validation")],
    }
}

pub(crate) fn accepted_profile(
    evaluated_profile_id: &str,
    lanes: &[RetrieverKind],
) -> AcceptedRetrievalProfileV1 {
    accepted_profile_with_compatibility(
        evaluated_profile_id,
        lanes,
        RetrievalCompatibilityPinsV1::default(),
    )
}

fn accepted_profile_with_compatibility(
    evaluated_profile_id: &str,
    lanes: &[RetrieverKind],
    compatibility: RetrievalCompatibilityPinsV1,
) -> AcceptedRetrievalProfileV1 {
    let evaluation = PassingRetrievalEvaluationV1::from_report(
        &passing_report(evaluated_profile_id),
        evaluated_profile_id,
    )
    .expect("passing evaluation");
    let profile = FusionProfile {
        profile_id: id(&format!("profile.{evaluated_profile_id}")),
        evaluation_result_anchor: evaluation.evaluation_anchor().clone(),
        calibrations: lanes
            .iter()
            .copied()
            .map(|lane| {
                (
                    lane,
                    id::<CalibrationProfileId>(&format!(
                        "calibration.{}.{}",
                        lane.as_str(),
                        evaluated_profile_id
                    )),
                )
            })
            .collect(),
        score_domain_calibrations: BTreeMap::new(),
        weights_micros: lanes.iter().copied().map(|lane| (lane, 1)).collect(),
        diversity_policy_id: id(&format!("diversity.{evaluated_profile_id}")),
        rerank_policy_id: None,
        retrieval_budget: RetrievalBudget {
            max_candidates_per_lane: 8,
            max_fused_candidates: 8,
            max_hydrated_results: 4,
            max_hydration_bytes: 4096,
            deadline_micros: None,
        },
    };
    let diversity = DiversityPolicy {
        policy_id: profile.diversity_policy_id.clone(),
        evaluation_result_anchor: Some(profile.evaluation_result_anchor.clone()),
        per_source_namespace: None,
        per_source_instance: None,
        per_repository: None,
        per_file: None,
        per_session_or_thread: None,
        per_copy_cluster: None,
        per_evidence_role: None,
    };
    AcceptedRetrievalProfileV1::new(profile, diversity, None, compatibility, evaluation)
        .expect("accepted profile")
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
}

fn semantic_pins() -> SemanticCompatibilityPinsV1 {
    let artifact = digest('a');
    let projection = EmbeddingProjectionKeyV1 {
        model_artifact_digest: artifact.clone(),
        tokenizer_digest: digest('b'),
        config_digest: digest('c'),
        query_instruction_digest: None,
        document_instruction_digest: None,
        pooling: EmbeddingPoolingV1::Mean,
        truncation_side: EmbeddingTruncationSideV1::Right,
        truncation_length: 128,
        runtime_backend: "fastembed-ort".to_owned(),
        runtime_build_revision: "runtime.query-activation-test.v1".to_owned(),
        device_class: EmbeddingDeviceClassV1::Cpu,
        dimensions: 4,
        metric: EmbeddingMetricV1::Cosine,
        normalization: EmbeddingNormalizationV1::L2,
        precision: EmbeddingPrecisionV1::Fp32,
        chunk_schema_revision: "code-search-chunk.v1".to_owned(),
        chunker_revision: id::<ChunkerRevision>("chunker.query-activation-test.v1"),
        privacy_domain: id("privacy.query-activation-test"),
        privacy_key_epoch: 1,
    }
    .admit()
    .expect("admitted semantic projection");
    let vector_generation_id = VectorGenerationIdV1::new(digest('d'));
    SemanticCompatibilityPinsV1 {
        implementation_revision: ComponentRevision::new("semantic.query-activation-test.v1")
            .expect("implementation revision"),
        fusion_revision: ComponentRevision::new("fusion.query-activation-test.v1")
            .expect("fusion revision"),
        artifact_manifest_digest: artifact,
        runtime_compatibility_digest: digest('e'),
        search_index_key: tracedecay_domain::SemanticSearchIndexProfileV1::exact_flat_v1()
            .and_then(|profile| profile.index_key())
            .expect("search index key"),
        calibration: SemanticCalibrationProfileV1 {
            calibration_profile_id: id("calibration.semantic.semantic-active"),
            cohort_digest: digest('f'),
            projection_key: projection.projection_key().clone(),
            vector_generation: vector_generation_id.clone(),
            capability_manifest_digest: digest('1'),
            maximum_distance_micros: 2_000_000,
            minimum_margin_micros: 0,
        },
        projection,
        vector_generation_id,
        resources: SemanticResourceRequirementV1 {
            model_bytes: 10,
            tokenizer_bytes: 5,
            resident_bytes: 20,
            threads: 2,
            max_concurrent_sessions: 1,
            batch_size: 4,
            sequence_length: 128,
            load_deadline_ms: 1_000,
        },
    }
}

fn semantic_committed_state(scope: ResolvedScope) -> CommittedRetrievalProfileStateV1 {
    let query = accepted_profile("query-baseline", &RetrieverKind::QUERY_FALLBACK_LANES);
    let pins = semantic_pins();
    let semantic = accepted_profile_with_compatibility(
        "semantic-active",
        &[
            RetrieverKind::ExactLiteral,
            RetrieverKind::Lexical,
            RetrieverKind::Graph,
            RetrieverKind::Semantic,
        ],
        RetrievalCompatibilityPinsV1 {
            semantic: Some(pins.clone()),
            rerank: None,
        },
    );
    let base_revision = id::<ConfigurationRevisionId>("configuration.query-activation-test.1");
    let result_revision = id::<ConfigurationRevisionId>("configuration.query-activation-test.2");
    let actor_id = id("actor.query-activation-test");
    let operation = RetrievalProfileAuditOperationV1::Activate;
    let freshness_vector_digest = digest('2');
    let occurred_at = UtcMicros(20);
    let audit = RetrievalProfileAuditEventV1 {
        event_id: canonical_sha256(&(
            "tracedecay.retrieval.profile-audit.v1",
            &actor_id,
            &operation,
            &query.profile().profile_id,
            &semantic.profile().profile_id,
            query.profile_digest(),
            semantic.profile_digest(),
            &semantic.profile().evaluation_result_anchor,
            &freshness_vector_digest,
            &base_revision,
            &result_revision,
            occurred_at,
        ))
        .expect("audit digest"),
        actor_id,
        operation,
        prior_active_profile_id: query.profile().profile_id.clone(),
        resulting_active_profile_id: semantic.profile().profile_id.clone(),
        prior_active_digest: query.profile_digest().clone(),
        resulting_active_digest: semantic.profile_digest().clone(),
        evaluation_anchor: semantic.profile().evaluation_result_anchor.clone(),
        freshness_vector_digest,
        base_revision,
        result_revision: result_revision.clone(),
        occurred_at,
    };
    let state = serde_json::from_value::<RetrievalProfileStateSnapshotV1>(serde_json::json!({
        "configuration_revision": result_revision,
        "active": semantic,
        "rollback": query,
        "audit": [audit],
    }))
    .expect("persisted semantic retrieval state")
    .into_state()
    .expect("semantic retrieval state");
    let configuration = SemanticConfigurationPinV1 {
        revision_id: state.configuration_revision().clone(),
        snapshot_id: id::<ConfigurationSnapshotId>("configuration.snapshot.query-activation-test"),
        effective_behavior_digest: digest('3'),
    };
    let command = SemanticActivationCommandV1::new(
        configuration,
        SemanticActivationRequestV1::new(pins.vector_generation_id.clone(), None, None)
            .expect("semantic activation request"),
    )
    .expect("semantic activation command");
    let receipt = SemanticActivationReceiptV1::issue(&command, UtcMicros(30))
        .expect("semantic activation receipt");
    CommittedRetrievalProfileStateV1 {
        epoch: 2,
        transition_digest: state
            .audit()
            .last()
            .expect("semantic transition audit")
            .event_id
            .clone(),
        scope,
        state,
        current_activation: Some(
            SemanticCurrentLinkedActivationV1::new(receipt, pins)
                .expect("current semantic activation"),
        ),
    }
}

fn query_rollback_committed_state(
    prior: &CommittedRetrievalProfileStateV1,
) -> CommittedRetrievalProfileStateV1 {
    let semantic = prior.state.active().clone();
    let query = prior
        .state
        .rollback_profile()
        .expect("query rollback profile")
        .clone();
    let base_revision = prior.state.configuration_revision().clone();
    let result_revision = id::<ConfigurationRevisionId>("configuration.query-activation-test.3");
    let actor_id = id("actor.query-activation-test");
    let operation = RetrievalProfileAuditOperationV1::Rollback {
        trigger: "operator rollback".to_owned(),
    };
    let freshness_vector_digest = digest('4');
    let occurred_at = UtcMicros(40);
    let audit = RetrievalProfileAuditEventV1 {
        event_id: canonical_sha256(&(
            "tracedecay.retrieval.profile-audit.v1",
            &actor_id,
            &operation,
            &semantic.profile().profile_id,
            &query.profile().profile_id,
            semantic.profile_digest(),
            query.profile_digest(),
            &query.profile().evaluation_result_anchor,
            &freshness_vector_digest,
            &base_revision,
            &result_revision,
            occurred_at,
        ))
        .expect("rollback audit digest"),
        actor_id,
        operation,
        prior_active_profile_id: semantic.profile().profile_id.clone(),
        resulting_active_profile_id: query.profile().profile_id.clone(),
        prior_active_digest: semantic.profile_digest().clone(),
        resulting_active_digest: query.profile_digest().clone(),
        evaluation_anchor: query.profile().evaluation_result_anchor.clone(),
        freshness_vector_digest,
        base_revision,
        result_revision: result_revision.clone(),
        occurred_at,
    };
    let mut audit_history = prior.state.audit().to_vec();
    audit_history.push(audit);
    let state = serde_json::from_value::<RetrievalProfileStateSnapshotV1>(serde_json::json!({
        "configuration_revision": result_revision,
        "active": query,
        "rollback": semantic,
        "audit": audit_history,
    }))
    .expect("persisted query rollback state")
    .into_state()
    .expect("query rollback state");
    CommittedRetrievalProfileStateV1 {
        epoch: prior.epoch + 1,
        transition_digest: state
            .audit()
            .last()
            .expect("rollback transition audit")
            .event_id
            .clone(),
        scope: prior.scope.clone(),
        state,
        current_activation: None,
    }
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(args)
        .status()
        .expect("run git fixture command");
    assert!(status.success(), "git fixture command failed: {args:?}");
}

#[test]
fn unavailable_provider_status_contains_no_key_material() {
    let provider = DaemonQueryAuthorityProviderV1::default();

    assert_eq!(
        provider.status(None),
        QueryAuthorityProviderStatusV1::Unavailable {
            reason: QueryAuthorityUnavailableReasonV1::ActivationUnavailable,
        }
    );
    assert!(format!("{provider:?}").contains("REDACTED"));
}

#[test]
fn semantic_activation_selects_exact_query_rollback_profile() {
    let query = accepted_profile("query-baseline", &RetrieverKind::QUERY_FALLBACK_LANES);
    let semantic_active = accepted_profile(
        "semantic-active",
        &[RetrieverKind::ExactLiteral, RetrieverKind::Lexical],
    );

    let selected = exact_query_profile_from_slots(&semantic_active, Some(&query))
        .expect("rollback query profile");

    assert_eq!(selected.profile().profile_id, query.profile().profile_id);
}

#[tokio::test]
async fn evaluated_initial_query_state_is_available_without_a_fake_activation_event() {
    let provider = DaemonQueryAuthorityProviderV1::default();
    let scope = ResolvedScope::new(
        id("project.initial"),
        id("repository.initial"),
        id("worktree.initial"),
        Some(id("refs/heads/main")),
    )
    .expect("scope");
    let query = accepted_profile("query-baseline", &RetrieverKind::QUERY_FALLBACK_LANES);
    let state = RetrievalProfileStateV1::new(
        id::<ConfigurationRevisionId>("configuration.query-initial.1"),
        query.clone(),
        &RetrievalRuntimeCompatibilityV1 {
            retrieval_ceiling: RetrievalBudget {
                max_candidates_per_lane: 32,
                max_fused_candidates: 32,
                max_hydrated_results: 16,
                max_hydration_bytes: 65_536,
                deadline_micros: None,
            },
            semantic: None,
            semantic_ceiling: None,
            rerank: None,
            rerank_ceiling: None,
        },
    )
    .expect("initial state");

    let directory = TempDir::new().expect("temporary cursor store");
    let profile_root = directory.path().join("profile");
    let identity =
        crate::daemon::profile_identity::load_or_create(&profile_root).expect("profile identity");
    let _scope_guard = crate::db::enter_daemon_database_scope(&profile_root, 1, "query-initial")
        .expect("database scope");
    let session_registry =
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            identity,
        )
        .await
        .expect("session registry");
    let database = session_registry
        .profile_sessions()
        .await
        .expect("session database");
    let cursor_keys = Arc::new(
        database
            .load_session_cursor_key_provider_result()
            .await
            .expect("cursor keys"),
    );
    let status = provider
        .install_evaluated_initial_state(scope.clone(), state, cursor_keys)
        .expect("evaluated initial state");

    assert!(matches!(
        status,
        QueryAuthorityProviderStatusV1::Available { profile_id, .. }
            if profile_id == query.profile().profile_id
    ));
}

#[test]
fn semantic_rollback_selects_restored_exact_query_active_profile() {
    let query = accepted_profile("query-baseline", &RetrieverKind::QUERY_FALLBACK_LANES);
    let prior_semantic = accepted_profile(
        "semantic-prior",
        &[RetrieverKind::ExactLiteral, RetrieverKind::Lexical],
    );

    let selected = exact_query_profile_from_slots(&query, Some(&prior_semantic))
        .expect("active query profile");

    assert_eq!(selected.profile().profile_id, query.profile().profile_id);
}

#[test]
fn zero_or_multiple_exact_query_profiles_fail_closed() {
    let non_query = accepted_profile(
        "semantic-active",
        &[RetrieverKind::ExactLiteral, RetrieverKind::Lexical],
    );
    assert!(matches!(
        exact_query_profile_from_slots(&non_query, None),
        Err(QueryAuthorityUnavailableReasonV1::InvalidActivatedProfile)
    ));

    let first = accepted_profile("query-first", &RetrieverKind::QUERY_FALLBACK_LANES);
    let second = accepted_profile("query-second", &RetrieverKind::QUERY_FALLBACK_LANES);
    assert!(matches!(
        exact_query_profile_from_slots(&first, Some(&second)),
        Err(QueryAuthorityUnavailableReasonV1::AmbiguousActivatedProfile)
    ));
}

#[path = "query_authority_provider_activation_tests.rs"]
mod activation_tests;
