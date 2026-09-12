use std::collections::BTreeMap;
#[cfg(all(feature = "semantic-fastembed", not(windows)))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;
use tracedecay_domain::UtcMicros;

use tokio::sync::oneshot;
use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotId};
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, AuthorizationRevision, BoundedSanitizedText,
    CalibrationProfileId, ChangedCodeChunkSetV1, ChunkerRevision, CodeGenerationId,
    CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchChunkV1,
    ContentDigest, EphemeralSanitizedQueryViewV1, FallbackSubpayloadDigest, FileOccurrenceId,
    FusionProfileId, LanguageDescriptorRevision, ManifestDigest, PolicyRevisionId, PrincipalId,
    ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionReplayReasonV1, PublicRetrieverStatus,
    QueryDigest, QueryMac, QueryNormalizationRevision, RepositoryId, RetrievalRequest,
    RetrievalScope, RetrievalSnapshot, RetrieverKind, SanitizerRevision, SensitivityDecision,
    SensitivityLevelV1, SingleRootScopeV1, SourceSpan, TemporalModeV1, VectorGenerationIdV1,
    VectorWatermark,
};

use tracedecay_semantic::{
    DaemonSemanticRuntimeHandleV1, FastEmbedSemanticGenerationRequestV1,
    PreparedSemanticRuntimeCommitV1, SemanticRuntimeWorkV1,
};
use tracedecay_semantic_contracts::{
    SemanticGenerationPointerV1, SemanticRuntimeScheduleFailureV1, SemanticRuntimeScheduleStatusV1,
};

use super::super::ports::{
    SemanticActivationCommandV1, SemanticActivationReceiptV1, SemanticConfigurationPinV1,
    SemanticRuntimeBackendV1, SemanticRuntimeStateV1,
};
use super::application_status::application_status_from_projection;
use super::daemon_backend::DaemonSemanticRuntimeBackendV1;
use super::evaluation_generation::SemanticEvaluationExecutionControlV1;
use super::evaluation_support::{
    EVALUATION_SEMANTIC_MINIMUM_MARGIN_MICROS_V1, block_on_semantic_evaluation,
    canonical_evaluation_calibration, certify_evaluation_target_compatibility,
    evaluation_projection_plan_from_canonical_chunks, validate_evaluation_target_search_index,
};
use super::published_vector_read::{PublishedSemanticAnnBindingV1, semantic_candidate_identity};
use super::search_composition::compose_application_semantic_search;
use super::*;
use crate::store::vector_generations::VectorGenerationPlanV1;
use std::collections::BTreeSet;
use tracedecay_code_index::projection::expected_request_digest;
use tracedecay_domain::{
    ComponentRevision, LogicalEvidenceId, QueryFallbackSubpayload, RetrievalAnchorId,
    SemanticSearchIndexProfileV1, SourceOccurrenceId,
};
use tracedecay_query::retrieval::ports::{RetrievalExecutionControl, RetrievalPortError};
use tracedecay_query::retrieval::semantic::{
    CompleteSemanticGenerationV1, SemanticAnnIndexStateV1, SemanticCalibrationProfileV1,
    SemanticQueryModeV1, SemanticQueryServiceOutcomeV1, SemanticRetrievalRequestV1,
    SemanticVectorReadPort,
};
#[cfg(all(feature = "semantic-fastembed", not(windows)))]
use tracedecay_query::retrieval::semantic::{
    SemanticVectorReadRequestV1, SemanticVectorRecordV1, SemanticVectorScanSummaryV1,
};

fn source_generation(value: char) -> CodeGenerationId {
    CodeGenerationId::new(format!("code-generation.{value}")).expect("source generation")
}

fn documents(value: char) -> Arc<EmbeddingDocumentComposerV1> {
    let index = tracedecay_code_index::lineage::GenerationSymbolIndexV1::new(
        source_generation(value),
        Vec::new(),
    )
    .expect("empty symbol index");
    Arc::new(EmbeddingDocumentComposerV1::new(
        EmbeddingSymbolContextIndexV1::from_generation_symbols(&index),
    ))
}

fn vector_generation(value: char) -> VectorGenerationIdV1 {
    VectorGenerationIdV1::new(
        canonical_sha256(&("semantic.test.vector-generation", value)).expect("manifest digest"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn blocking_evaluation_drives_async_projection_on_daemon_runtime() {
    let observed = tokio::task::spawn_blocking(|| {
        block_on_semantic_evaluation(async { Ok::<_, SemanticRuntimeScheduleFailureV1>(7) })
    })
    .await
    .expect("blocking evaluator joins");

    assert_eq!(observed, Ok(7));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn saved_generation_dispatch_retains_daemon_runtime_across_blocking_handoff() {
    let runtime = tokio::runtime::Handle::try_current()
        .expect("daemon runtime is available while the hook is constructed");
    let (observed_tx, observed_rx) = oneshot::channel();

    tokio::task::spawn_blocking(move || {
        runtime.spawn(async move {
            let _ = observed_tx.send(());
        });
    })
    .await
    .expect("blocking serving-generation handoff joins");

    tokio::time::timeout(Duration::from_secs(1), observed_rx)
        .await
        .expect("captured runtime dispatch remains live")
        .expect("semantic dispatch reports completion");
}

#[test]
fn ann_binding_is_unsupported_for_exact_flat_and_missing_for_unbound_ann() {
    let exact_flat = search_index_key();
    assert!(matches!(
        PublishedSemanticAnnBindingV1::bind(exact_flat, None, &[])
            .expect("exact-flat ports bind without an index"),
        PublishedSemanticAnnBindingV1::Unavailable(SemanticAnnIndexStateV1::Unsupported)
    ));

    let ann = SemanticSearchIndexProfileV1::ann_hnsw_exact_rescore_v1()
        .and_then(|profile| profile.index_key())
        .expect("ann search index key");
    assert!(matches!(
        PublishedSemanticAnnBindingV1::bind(&ann, None, &[])
            .expect("a consulted store without an index binds as missing"),
        PublishedSemanticAnnBindingV1::Unavailable(SemanticAnnIndexStateV1::Missing)
    ));
}

#[test]
fn preacceptance_target_requires_a_canonical_search_index() {
    let exact_flat = search_index_key().clone();
    assert_eq!(validate_evaluation_target_search_index(&exact_flat), Ok(()));

    let ann = SemanticSearchIndexProfileV1::ann_hnsw_exact_rescore_v1()
        .and_then(|profile| profile.index_key())
        .expect("ann search index key");
    assert_eq!(validate_evaluation_target_search_index(&ann), Ok(()));

    let mut wrong = exact_flat;
    wrong.schema_revision = "semantic-search-index.v0".to_owned();
    assert_eq!(
        validate_evaluation_target_search_index(&wrong),
        Err(SemanticRuntimeBackendErrorV1::Rejected)
    );
}

fn test_digest(value: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", value.to_string().repeat(64))).expect("test digest")
}

fn evaluation_target_pins(
    source_generation: &CodeGenerationId,
    source_manifest_digest: &ManifestDigest,
    capability_manifest_digest: &ManifestDigest,
) -> crate::config::retrieval::SemanticCompatibilityPinsV1 {
    let projection = tracedecay_semantic::session_pool::test_support::authority()
        .projection()
        .clone();
    let vector_generation_id = vector_generation('v');
    let mut candidate = crate::config::retrieval::SemanticCompatibilityPinsV1 {
        implementation_revision: ComponentRevision::new("semantic.fastembed.production.v1")
            .expect("implementation revision"),
        fusion_revision: ComponentRevision::new(
            tracedecay_query::retrieval::QUERY_RANKING_REVISION_V1,
        )
        .expect("canonical fusion revision"),
        artifact_manifest_digest: projection.embedding_key().model_artifact_digest.clone(),
        runtime_compatibility_digest: test_digest('b'),
        projection,
        search_index_key: search_index_key().clone(),
        vector_generation_id: vector_generation_id.clone(),
        calibration: SemanticCalibrationProfileV1 {
            calibration_profile_id: CalibrationProfileId::new("calibration.semantic.runtime.v1")
                .expect("calibration profile"),
            cohort_digest: test_digest('c'),
            projection_key: projection_key(),
            vector_generation: vector_generation_id,
            capability_manifest_digest: capability_manifest_digest.clone(),
            maximum_distance_micros: UNCALIBRATED_MAXIMUM_DISTANCE_MICROS,
            minimum_margin_micros: EVALUATION_SEMANTIC_MINIMUM_MARGIN_MICROS_V1,
        },
        resources: crate::config::retrieval::SemanticResourceRequirementV1 {
            model_bytes: 1,
            tokenizer_bytes: 1,
            resident_bytes: 1,
            threads: 1,
            max_concurrent_sessions: 1,
            batch_size: 1,
            sequence_length: 1,
            load_deadline_ms: 1,
        },
    };
    candidate.calibration = canonical_evaluation_calibration(
        &candidate,
        source_generation,
        source_manifest_digest,
        capability_manifest_digest,
        UNCALIBRATED_MAXIMUM_DISTANCE_MICROS,
    )
    .expect("canonical calibration");
    candidate
}

/// The acceptance bound must come from the generation's measurement, not
/// from a constant baked into the calibration constructor.
#[test]
fn canonical_calibration_carries_the_measured_bound() {
    let source_generation = source_generation('m');
    let source_manifest_digest = test_digest('d');
    let capability_manifest_digest = test_digest('e');
    let candidate = evaluation_target_pins(
        &source_generation,
        &source_manifest_digest,
        &capability_manifest_digest,
    );

    // Two different measurements of the same identity must produce two
    // different bounds; a constant would produce one.
    for measured in [123_456_789_i64, 987_654_321_i64] {
        let calibration = canonical_evaluation_calibration(
            &candidate,
            &source_generation,
            &source_manifest_digest,
            &capability_manifest_digest,
            measured,
        )
        .expect("canonical calibration");
        assert_eq!(calibration.maximum_distance_micros, measured);
    }
}

/// Certification binds the candidate to what its generation measures, so a
/// candidate carrying any other bound is rejected.
#[test]
fn preacceptance_rejects_a_bound_the_generation_did_not_measure() {
    let source_generation = source_generation('b');
    let source_manifest_digest = test_digest('d');
    let capability_manifest_digest = test_digest('e');
    let candidate = evaluation_target_pins(
        &source_generation,
        &source_manifest_digest,
        &capability_manifest_digest,
    );

    // The candidate was minted against UNCALIBRATED_MAXIMUM_DISTANCE_MICROS.
    assert_eq!(
        certify_evaluation_target_compatibility(
            &candidate,
            &source_generation,
            &source_manifest_digest,
            &capability_manifest_digest,
            UNCALIBRATED_MAXIMUM_DISTANCE_MICROS,
        )
        .map(|certified| certified.calibration.maximum_distance_micros),
        Ok(UNCALIBRATED_MAXIMUM_DISTANCE_MICROS)
    );

    // A generation that measures something else does not certify it.
    assert_eq!(
        certify_evaluation_target_compatibility(
            &candidate,
            &source_generation,
            &source_manifest_digest,
            &capability_manifest_digest,
            UNCALIBRATED_MAXIMUM_DISTANCE_MICROS - 1,
        ),
        Err(SemanticRuntimeBackendErrorV1::Rejected)
    );
}

#[test]
fn preacceptance_preserves_the_evaluated_calibration_profile() {
    let source_generation = source_generation('c');
    let source_manifest_digest = test_digest('d');
    let capability_manifest_digest = test_digest('e');
    let mut candidate = evaluation_target_pins(
        &source_generation,
        &source_manifest_digest,
        &capability_manifest_digest,
    );
    candidate.calibration.calibration_profile_id =
        CalibrationProfileId::new("calibration.semantic.hybrid-conservative")
            .expect("evaluated calibration profile");

    let certified = certify_evaluation_target_compatibility(
        &candidate,
        &source_generation,
        &source_manifest_digest,
        &capability_manifest_digest,
        UNCALIBRATED_MAXIMUM_DISTANCE_MICROS,
    )
    .expect("evaluated calibration remains independently certifiable");

    assert_eq!(
        certified.calibration.calibration_profile_id,
        candidate.calibration.calibration_profile_id
    );
}

/// Regression guard: the unmeasured fallback must stay inside the range the
/// redundancy authority accepts. An out-of-range bound (the former
/// `i64::MAX`) silently disarms the semantic redundancy tier.
#[test]
fn the_uncalibrated_fallback_stays_in_the_redundancy_accepted_range() {
    assert!(
        (0..=super::super::acceptance_calibration::MAX_COSINE_DISTANCE_MICROS)
            .contains(&UNCALIBRATED_MAXIMUM_DISTANCE_MICROS)
    );
}

#[test]
fn preacceptance_rejects_foreign_fusion_revision() {
    let source_generation = source_generation('f');
    let source_manifest_digest = test_digest('d');
    let capability_manifest_digest = test_digest('e');
    let mut candidate = evaluation_target_pins(
        &source_generation,
        &source_manifest_digest,
        &capability_manifest_digest,
    );
    candidate.fusion_revision =
        ComponentRevision::new("ranking.foreign.v1").expect("foreign fusion revision");

    assert_eq!(
        certify_evaluation_target_compatibility(
            &candidate,
            &source_generation,
            &source_manifest_digest,
            &capability_manifest_digest,
            UNCALIBRATED_MAXIMUM_DISTANCE_MICROS,
        ),
        Err(SemanticRuntimeBackendErrorV1::Rejected)
    );
}

#[test]
fn preacceptance_rejects_foreign_calibration_cohort_and_thresholds() {
    let source_generation = source_generation('c');
    let source_manifest_digest = test_digest('d');
    let capability_manifest_digest = test_digest('e');
    let candidate = evaluation_target_pins(
        &source_generation,
        &source_manifest_digest,
        &capability_manifest_digest,
    );

    let mut wrong_cohort = candidate.clone();
    wrong_cohort.calibration.cohort_digest = test_digest('f');
    assert_eq!(
        certify_evaluation_target_compatibility(
            &wrong_cohort,
            &source_generation,
            &source_manifest_digest,
            &capability_manifest_digest,
            UNCALIBRATED_MAXIMUM_DISTANCE_MICROS,
        ),
        Err(SemanticRuntimeBackendErrorV1::Rejected)
    );

    let mut wrong_thresholds = candidate;
    wrong_thresholds.calibration.minimum_margin_micros = 1;
    assert_eq!(
        certify_evaluation_target_compatibility(
            &wrong_thresholds,
            &source_generation,
            &source_manifest_digest,
            &capability_manifest_digest,
            UNCALIBRATED_MAXIMUM_DISTANCE_MICROS,
        ),
        Err(SemanticRuntimeBackendErrorV1::Rejected)
    );
}

#[test]
fn deadline_interrupts_semantic_and_rerank_evaluation_cooperatively() {
    struct DeadlineCancellation;

    impl tracedecay_semantic::SemanticExecutionAuthority for DeadlineCancellation {
        fn interruption(&self) -> Option<SemanticExecutionInterruptionV1> {
            Some(SemanticExecutionInterruptionV1::DeadlineExceeded)
        }
    }

    impl tracedecay_semantic::SemanticEvaluationCancellationV1 for DeadlineCancellation {}

    let control = SemanticEvaluationExecutionControlV1 {
        started: std::time::Instant::now(),
        cancellation: Arc::new(DeadlineCancellation),
    };

    assert!(RetrievalExecutionControl::is_cancelled(&control));
    assert!(RetrievalExecutionControl::is_cancelled(&control));
}

fn projection_key() -> ProjectionKeyV1 {
    // Derive the projection key from the same admitted authority the query
    // runtime binds against so `profile_digest` is the canonical digest the
    // embedding projection produces, rather than a non-canonical placeholder.
    tracedecay_semantic::session_pool::test_support::authority()
        .projection()
        .projection_key()
        .clone()
}

fn search_index_key() -> &'static SemanticSearchIndexKeyV1 {
    static KEY: std::sync::OnceLock<SemanticSearchIndexKeyV1> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        SemanticSearchIndexProfileV1::exact_flat_v1()
            .and_then(|profile| profile.index_key())
            .expect("exact-flat search index key")
    })
}

#[test]
fn retained_vector_cache_matches_complete_query_identity() {
    let source = source_generation('r');
    let vector = vector_generation('r');
    let capability = test_digest('f');
    let port = Arc::new(PublishedSemanticVectorReadPortV1 {
        generation: vector.clone(),
        projection_key: projection_key(),
        search_index_key: search_index_key().clone(),
        source_generation: source.clone(),
        capability_manifest_digest: capability.clone(),
        source_coherence: SemanticSourceCoherenceV1::ExactGeneration,
        rows: Vec::new(),
        ann: PublishedSemanticAnnBindingV1::Unavailable(SemanticAnnIndexStateV1::Unsupported),
    });
    let cached = CachedPublishedVectorsV1 {
        generation: vector.clone(),
        search_index_key: search_index_key().clone(),
        source_generation: source.clone(),
        port,
    };

    assert!(cached.matches(
        &vector,
        &projection_key(),
        search_index_key(),
        &source,
        &capability,
    ));
    assert!(!cached.matches(
        &vector_generation('s'),
        &projection_key(),
        search_index_key(),
        &source,
        &capability,
    ));
    assert!(!cached.matches(
        &vector,
        &projection_key(),
        search_index_key(),
        &source_generation('s'),
        &capability,
    ));
    assert!(!cached.matches(
        &vector,
        &projection_key(),
        search_index_key(),
        &source,
        &test_digest('e'),
    ));
}

fn pointer(vector: char, source: char) -> SemanticGenerationPointerV1 {
    SemanticGenerationPointerV1 {
        generation: vector_generation(vector),
        source_generation: source_generation(source),
        projection_key: projection_key(),
    }
}

fn configuration_pin() -> SemanticConfigurationPinV1 {
    SemanticConfigurationPinV1 {
        revision_id: ConfigurationRevisionId::try_from(
            "configuration.revision.semantic-test".to_owned(),
        )
        .expect("configuration revision"),
        snapshot_id: ConfigurationSnapshotId::try_from(
            "configuration.snapshot.semantic-test".to_owned(),
        )
        .expect("configuration snapshot"),
        effective_behavior_digest: ManifestDigest::new(format!("sha256:{}", "e".repeat(64)))
            .expect("configuration digest"),
    }
}

fn composition_request<'a>(
    query_view: &'a EphemeralSanitizedQueryViewV1,
    projection: &'a AdmittedEmbeddingProjectionKeyV1,
    source: CodeGenerationId,
    vector: VectorGenerationIdV1,
) -> SemanticRetrievalRequestV1<'a> {
    let query_digest = QueryDigest::new(
        projection.privacy_domain().clone(),
        projection.privacy_key_epoch(),
        QueryMac::new(format!("hmac-sha256:{}", "33".repeat(32))).expect("query MAC"),
    );
    let budget = tracedecay_domain::RetrievalBudget {
        max_candidates_per_lane: 8,
        max_fused_candidates: 16,
        max_hydrated_results: 8,
        max_hydration_bytes: 65_536,
        deadline_micros: None,
    };
    SemanticRetrievalRequestV1 {
        base: RetrievalRequest {
            principal: PrincipalId::try_from("principal.fixture".to_owned()).expect("principal"),
            scope: RetrievalScope {
                privacy_domain: projection.privacy_domain().clone(),
                root: SingleRootScopeV1 {
                    repository: RepositoryId::try_from("repository.fixture".to_owned())
                        .expect("repository"),
                    worktree: None,
                    reference: None,
                },
            },
            temporal_mode: TemporalModeV1::Current,
            snapshot: RetrievalSnapshot {
                watermarks: VectorWatermark::default(),
                freshness_digest: tracedecay_domain::FreshnessVectorDigest::try_from(format!(
                    "sha256:{}",
                    "a".repeat(64)
                ))
                .expect("freshness"),
                authorization_revision: AuthorizationRevision::try_from(
                    "authorization.v1".to_owned(),
                )
                .expect("authorization"),
                captured_at: UtcMicros(1),
            },
            profile_id: FusionProfileId::try_from("profile.semantic.v1".to_owned())
                .expect("profile"),
            budget,
        },
        query_digest,
        query_view,
        projection,
        search_index_key: search_index_key(),
        capability_manifest_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64)))
            .expect("capability"),
        vector_generation: vector,
        code_generation: source,
        budget,
    }
}

fn composition_fallback() -> Arc<QueryFallbackSubpayload> {
    let mut fallback = QueryFallbackSubpayload {
        profile_id: FusionProfileId::try_from("profile.query.semantic-contract.v1".to_owned())
            .expect("profile"),
        ordered_candidates: Vec::new(),
        public_fallback_lane_coverage: BTreeMap::from([
            (RetrieverKind::ExactLiteral, PublicRetrieverStatus::Complete),
            (RetrieverKind::Lexical, PublicRetrieverStatus::Complete),
            (RetrieverKind::Graph, PublicRetrieverStatus::Complete),
        ]),
        freshness: Vec::new(),
        cursor: None,
        digest: FallbackSubpayloadDigest::new(format!("sha256:{}", "0".repeat(64)))
            .unwrap_or_else(|_| panic!("digest")),
    };
    fallback.digest = fallback.compute_digest().expect("fallback digest");
    Arc::new(fallback)
}

#[cfg(all(feature = "semantic-fastembed", not(windows)))]
fn composition_calibration(
    request: &SemanticRetrievalRequestV1<'_>,
) -> SemanticCalibrationProfileV1 {
    SemanticCalibrationProfileV1 {
        calibration_profile_id: CalibrationProfileId::try_from(
            "calibration.semantic.fixture.v1".to_owned(),
        )
        .expect("calibration profile"),
        cohort_digest: ManifestDigest::new(format!("sha256:{}", "7".repeat(64)))
            .expect("cohort digest"),
        projection_key: request.projection.projection_key().clone(),
        vector_generation: request.vector_generation.clone(),
        capability_manifest_digest: request.capability_manifest_digest.clone(),
        maximum_distance_micros: i64::MAX,
        minimum_margin_micros: 0,
    }
}

fn projection_request(source: char) -> ProjectionBatchRequestV1 {
    ProjectionBatchRequestV1 {
        request_digest: ManifestDigest::new(format!("sha256:{}", "c".repeat(64)))
            .expect("request digest"),
        changes: ChangedCodeChunkSetV1 {
            from_generation: None,
            to_generation: source_generation(source),
            manifest_digest: ManifestDigest::new(format!("sha256:{}", "d".repeat(64)))
                .expect("source manifest"),
            added_or_changed: Vec::new(),
            deleted: Vec::new(),
            reused: Vec::new(),
        },
        previous_projection_key: None,
        target_projection_key: projection_key(),
        replay_reason: ProjectionReplayReasonV1::SourceEdit,
    }
}

fn canonical_chunk(source: &CodeGenerationId, value: char) -> CodeSearchChunkV1 {
    CodeSearchChunkV1 {
        id: CodeSearchChunkId::new(format!("chunk.v1.{value}")).expect("chunk id"),
        anchor: CodeSearchChunkAnchorV1 {
            generation_id: source.clone(),
            file_occurrence_id: FileOccurrenceId::new(format!("{value}.rs"))
                .expect("file occurrence"),
            symbol_occurrence_id: None,
            parent_chunk_id: None,
            source_span: SourceSpan {
                start_byte: 0,
                end_byte: 4,
            },
            grain: CodeSearchChunkGrainV1::FileWindow,
            ordinal: 0,
        },
        content_digest: ContentDigest::new(format!("sha256:{}", value.to_string().repeat(64)))
            .expect("content digest"),
        language_descriptor_revision: LanguageDescriptorRevision::new("rust.v1")
            .expect("language descriptor"),
        chunker_revision: ChunkerRevision::new("chunker.v1").expect("chunker revision"),
        sanitizer_revision: SanitizerRevision::new("sanitizer.v1").expect("sanitizer revision"),
        sensitivity: SensitivityDecision {
            level: SensitivityLevelV1::Public,
            policy_revision: PolicyRevisionId::new("policy.v1").expect("policy revision"),
        },
        exact_terms: Vec::new(),
        subtokens: Vec::new(),
        sanitized_text: BoundedSanitizedText::new("code").expect("sanitized text"),
    }
}

#[test]
fn symbol_backed_semantic_identity_dedupes_with_lexical_evidence() {
    let source = source_generation('a');
    let mut chunk = canonical_chunk(&source, 'a');
    let symbol =
        tracedecay_domain::SymbolOccurrenceId::new("symbol.fixture").expect("symbol occurrence");
    chunk.anchor.symbol_occurrence_id = Some(symbol.clone());

    let (semantic_anchor, semantic_logical_evidence, source_occurrence) =
        semantic_candidate_identity(&chunk).expect("semantic identity");
    let lexical_evidence = format!("code-symbol:{}", symbol.as_str());
    let lexical_anchor = RetrievalAnchorId::new(lexical_evidence.clone()).expect("lexical anchor");
    let mixed_anchors = BTreeSet::from([semantic_anchor.clone(), lexical_anchor]);

    assert_eq!(mixed_anchors.len(), 1);
    assert_eq!(
        semantic_anchor,
        RetrievalAnchorId::new(lexical_evidence.clone()).expect("semantic anchor")
    );
    assert_eq!(
        semantic_logical_evidence,
        LogicalEvidenceId::new(lexical_evidence).expect("logical evidence")
    );
    assert_eq!(
        source_occurrence,
        SourceOccurrenceId::new(format!("code-chunk:{}", chunk.id.as_str()))
            .expect("source occurrence")
    );
}

#[test]
fn evaluation_plan_uses_canonical_generation_chunk_order() {
    let source = source_generation('o');
    let alpha = canonical_chunk(&source, 'a');
    let beta = canonical_chunk(&source, 'b');
    let request = projection_request('o');

    let plan = evaluation_projection_plan_from_canonical_chunks(
        &[Arc::new(alpha.clone()), Arc::new(beta.clone())],
        &request,
        None,
    );

    assert_eq!(
        plan.expected_chunk_ids.as_slice(),
        &[alpha.id.clone(), beta.id.clone()]
    );
}

#[tokio::test]
async fn saved_edit_schedules_fastembed_without_blocking_exact_search() {
    let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("semantic handle");
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let exact_ready = AtomicBool::new(false);

    let request = FastEmbedSemanticGenerationRequestV1::new(
        source_generation('a'),
        projection_request('a'),
        Vec::new(),
        documents('a'),
        SEMANTIC_EMBEDS_PER_COMMIT,
        move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
            Err(SemanticRuntimeScheduleFailureV1::Projection)
        },
        || async { Ok(SemanticProjectionResumeOutcomeV1::ReplayFromStart) },
        |_prepared| async { Ok(()) },
        move || async move { Err(SemanticRuntimeScheduleFailureV1::Publication) },
    )
    .expect("saved generation request");
    assert!(handle.schedule_generation(request));
    started_rx.await.expect("background schedule started");

    // Ordinary exact search proceeds while FastEmbed work is parked.
    exact_ready.store(true, Ordering::SeqCst);
    assert!(exact_ready.load(Ordering::SeqCst));
    assert!(matches!(
        handle.status(),
        SemanticRuntimeScheduleStatusV1::Indexing { .. }
    ));
    release_tx.send(()).expect("release artifact loader");
}

#[tokio::test]
async fn runtime_reports_semantic_indexing_progress() {
    let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
    let (started_tx, started_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    handle.schedule(SemanticRuntimeWorkV1::new(
        source_generation('a'),
        4,
        move |progress| async move {
            progress.set_completed_units(2);
            let _ = started_tx.send(());
            let _ = release_rx.await;
            Err(SemanticRuntimeScheduleFailureV1::Projection)
        },
    ));
    started_rx.await.expect("indexing started");
    let projection = handle.status_projection();
    let status = application_status_from_projection(&projection, None, None);
    match status.state {
        SemanticRuntimeStateV1::Indexing {
            completed_units,
            total_units,
            ..
        } => {
            assert_eq!(completed_units, 2);
            assert_eq!(total_units, 4);
        }
        other => panic!("expected indexing status, got {other:?}"),
    }
    let _ = release_tx.send(());
}

#[tokio::test]
async fn runtime_reports_degraded_reason_and_prior_generation() {
    let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
    let prior_pointer = pointer('a', 'a');
    let prior = prior_pointer.generation.clone();
    handle.schedule(SemanticRuntimeWorkV1::new(
        source_generation('a'),
        1,
        move |_progress| async move {
            Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                Ok(prior_pointer)
            }))
        },
    ));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while handle.current().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("prior generation published");

    handle.schedule(SemanticRuntimeWorkV1::new(
        source_generation('b'),
        1,
        move |_progress| async move { Err(SemanticRuntimeScheduleFailureV1::Artifact) },
    ));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if matches!(
                handle.status(),
                SemanticRuntimeScheduleStatusV1::Failed { .. }
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failure observed");

    let projection = handle.status_projection();
    assert_eq!(
        projection.degraded_reason,
        Some(SemanticFallbackReasonV1::ArtifactUnavailable)
    );
    assert_eq!(projection.prior_generation.as_ref(), Some(&prior));
    let status = application_status_from_projection(&projection, None, None);
    match status.state {
        SemanticRuntimeStateV1::Degraded {
            active_generation,
            reason,
        } => {
            assert_eq!(active_generation.as_ref(), Some(&prior));
            assert_eq!(reason, SemanticFallbackReasonV1::ArtifactUnavailable);
        }
        other => panic!("expected degraded status, got {other:?}"),
    }
    // Prior generation remains queryable / current for compatible reads.
    assert_eq!(
        handle.current().map(|pointer| pointer.generation),
        Some(prior)
    );
}

// Binding a query runtime requires the concrete FastEmbed runtime; the
// compiled-out stub fails compatibility verification by design.
#[cfg(all(feature = "semantic-fastembed", not(windows)))]
#[tokio::test]
async fn atomically_current_generation_enables_semantic_lane() {
    let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
    let published = pointer('c', 'c');
    let source = published.source_generation.clone();
    let vector = published.generation.clone();
    let projection_key = published.projection_key.clone();
    handle.schedule(SemanticRuntimeWorkV1::new(
        source_generation('c'),
        1,
        move |_progress| async move {
            Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                Ok(published)
            }))
        },
    ));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while handle.current().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("current generation published");

    // Pointer alone is insufficient — application search needs a bound query runtime.
    assert!(
        handle
            .query_factory(&source, &vector, &projection_key)
            .is_none(),
        "query_factory must stay closed until the query runtime is bound"
    );
    handle
        .bind_query_runtime_for_current(std::sync::Arc::new(
            tracedecay_semantic::session_pool::test_support::authority(),
        ))
        .expect("bind query runtime for current generation");

    assert!(
        handle
            .query_factory(&source, &vector, &projection_key)
            .is_some(),
        "exact warmed committed generation must enable query_factory"
    );
    assert!(
        handle
            .query_factory(&source_generation('x'), &vector, &projection_key)
            .is_none(),
        "incompatible source must not enable semantics"
    );
    let observed_pointer = SemanticGenerationPointerV1 {
        generation: vector.clone(),
        source_generation: source.clone(),
        projection_key: projection_key.clone(),
    };
    let exact_observation = handle
        .prepare_current_observation(&observed_pointer)
        .expect("prepare exact warmed-cache observation");
    let ready_publications = AtomicUsize::new(0);
    assert!(
        commit_current_observation_and_then(&handle, exact_observation, || {
            ready_publications.fetch_add(1, Ordering::SeqCst);
        }),
        "unchanged exact cache observation must commit"
    );
    assert_eq!(
        ready_publications.load(Ordering::SeqCst),
        1,
        "successful observation CAS must publish lifecycle readiness"
    );
    let stale_observation = handle
        .prepare_current_observation(&observed_pointer)
        .expect("prepare cache observation before concurrent unbind");
    assert!(handle.unbind_query_runtime_if_current(&vector));
    assert!(
        !commit_current_observation_and_then(&handle, stale_observation, || {
            ready_publications.fetch_add(1, Ordering::SeqCst);
        }),
        "cache observation must fail CAS after a concurrent transition"
    );
    assert_eq!(
        ready_publications.load(Ordering::SeqCst),
        1,
        "stale observation CAS must not publish false lifecycle readiness"
    );

    let backend = DaemonSemanticRuntimeBackendV1::new(handle.clone());
    let status = backend.application_status();
    assert!(matches!(
        status.route(),
        crate::semantic_runtime::SemanticRuntimeRouteV1::LexicalFallback { .. }
    ));
}

// Binding a query runtime requires the concrete FastEmbed runtime; the
// compiled-out stub fails compatibility verification by design.
#[cfg(all(feature = "semantic-fastembed", not(windows)))]
#[tokio::test]
async fn live_request_cancellation_reaches_query_runtime_before_vector_scan() {
    struct PanicVectors;

    impl SemanticVectorReadPort for PanicVectors {
        fn scan_exact_flat(
            &self,
            _request: SemanticVectorReadRequestV1<'_>,
            _examine: &mut dyn FnMut() -> Result<(), RetrievalPortError>,
            _visit: &mut dyn FnMut(&SemanticVectorRecordV1) -> Result<(), RetrievalPortError>,
        ) -> Result<SemanticVectorScanSummaryV1, RetrievalPortError> {
            panic!("cancelled query runtime must not scan vectors")
        }
    }

    struct CancelAtRuntimeBoundary {
        checks: AtomicUsize,
    }

    impl RetrievalExecutionControl for CancelAtRuntimeBoundary {
        fn is_cancelled(&self) -> bool {
            self.checks.fetch_add(1, Ordering::SeqCst) != 0
        }

        fn elapsed_micros(&self) -> u64 {
            0
        }
    }

    let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
    let published = pointer('q', 'q');
    let source = published.source_generation.clone();
    let vector = published.generation.clone();
    handle.schedule(SemanticRuntimeWorkV1::new(
        source.clone(),
        1,
        move |_progress| async move {
            Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                Ok(published)
            }))
        },
    ));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while handle.current().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("current generation published");
    let authority = tracedecay_semantic::session_pool::test_support::authority();
    handle
        .bind_query_runtime_for_current(Arc::new(authority.clone()))
        .expect("bind query runtime");

    let query_view = EphemeralSanitizedQueryViewV1::sanitize(
        "cancel before session acquisition",
        SanitizerRevision::try_from("sanitizer.v1".to_owned()).expect("sanitizer"),
        QueryNormalizationRevision::try_from("normalizer.v1".to_owned()).expect("normalizer"),
    )
    .expect("query view");
    let request = composition_request(&query_view, authority.projection(), source, vector);
    let complete = CompleteSemanticGenerationV1::new(
        request.projection.projection_key().clone(),
        request.search_index_key.clone(),
        request.vector_generation.clone(),
        request.code_generation.clone(),
        request.capability_manifest_digest.clone(),
    )
    .expect("complete generation");
    let calibration = composition_calibration(&request);
    let control = CancelAtRuntimeBoundary {
        checks: AtomicUsize::new(0),
    };

    let outcome = compose_application_semantic_search(ApplicationSemanticSearchParametersV1 {
        handle: &handle,
        request: &request,
        generation: &complete,
        calibration: Some(&calibration),
        vectors: &PanicVectors,
        control: &control,
        mode: SemanticQueryModeV1::FallbackAllowed,
        fallback: composition_fallback(),
        source_coherence: SemanticSourceCoherenceV1::ExactGeneration,
    })
    .expect("cancelled semantic composition");

    assert!(matches!(
        outcome,
        SemanticQueryServiceOutcomeV1::Fallback {
            abstention: tracedecay_query::retrieval::semantic::SemanticAbstentionV1::Cancelled,
            ..
        }
    ));
    assert!(
        control.checks.load(Ordering::SeqCst) >= 2,
        "query runtime must poll the same live request control"
    );
}

#[tokio::test]
async fn current_scheduler_pointer_without_persisted_receipt_stays_degraded() {
    let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
    let published = pointer('r', 'r');
    let generation = published.generation.clone();
    handle.schedule(SemanticRuntimeWorkV1::new(
        source_generation('r'),
        1,
        move |_progress| async move {
            Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                Ok(published)
            }))
        },
    ));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while handle.current().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("current generation published");

    let status = application_status_from_projection(
        &handle.status_projection(),
        Some(configuration_pin()),
        None,
    );
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    let restarted_status = application_status_from_projection(
        &handle.status_projection(),
        Some(configuration_pin()),
        None,
    );
    assert_eq!(
        status.state,
        SemanticRuntimeStateV1::Degraded {
            active_generation: Some(generation),
            // Never activated: a cause-bearing pre-activation state, not
            // an invalid runtime status.
            reason: SemanticFallbackReasonV1::NotActivated,
        }
    );
    assert_eq!(
        restarted_status, status,
        "status must not synthesize a time-varying activation receipt"
    );
}

#[tokio::test]
async fn remounted_scheduler_current_reattaches_the_durable_ready_receipt() {
    let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
    let published = pointer('s', 's');
    let generation = published.generation.clone();
    handle.schedule(SemanticRuntimeWorkV1::new(
        source_generation('s'),
        1,
        move |_progress| async move {
            Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                Ok(published)
            }))
        },
    ));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while handle.current().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("current generation published");

    let pin = configuration_pin();
    let receipt = SemanticActivationReceiptV1::issue(
        &SemanticActivationCommandV1::new(
            pin.clone(),
            SemanticActivationRequestV1::new(generation.clone(), None, None)
                .expect("activation request"),
        )
        .expect("activation command"),
        UtcMicros(10),
    )
    .expect("durable activation receipt");

    let remounted = application_status_from_projection(
        &handle.status_projection(),
        Some(pin.clone()),
        Some(receipt.clone()),
    );
    assert_eq!(
        remounted.state,
        SemanticRuntimeStateV1::Current {
            receipt: receipt.clone()
        },
        "restart remount must reattach the durable Ready receipt"
    );
    remounted.validate().expect("ready runtime status");
    assert_eq!(
        remounted.route(),
        crate::semantic_runtime::SemanticRuntimeRouteV1::Semantic {
            generation,
            activation_receipt_digest: receipt.receipt_digest,
        }
    );

    let foreign = pointer('t', 't').generation;
    let mismatched = SemanticActivationReceiptV1::issue(
        &SemanticActivationCommandV1::new(
            pin.clone(),
            SemanticActivationRequestV1::new(foreign, None, None).expect("foreign request"),
        )
        .expect("foreign command"),
        UtcMicros(11),
    )
    .expect("foreign receipt");
    let refused = application_status_from_projection(
        &handle.status_projection(),
        Some(pin),
        Some(mismatched),
    );
    assert!(
        matches!(
            refused.state,
            SemanticRuntimeStateV1::Degraded {
                reason: SemanticFallbackReasonV1::InvalidRuntimeStatus,
                ..
            }
        ),
        "a receipt for a different generation must stay a typed missing-Ready state"
    );
}

#[tokio::test]
async fn activation_without_persisted_scheduler_receipt_is_unavailable() {
    let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
    let published = pointer('a', 'a');
    let generation = published.generation.clone();
    handle.schedule(SemanticRuntimeWorkV1::new(
        source_generation('a'),
        1,
        move |_progress| async move {
            Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                Ok(published)
            }))
        },
    ));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while handle.current().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("current generation published");

    let pin = configuration_pin();
    let command = SemanticActivationCommandV1::new(
        pin,
        SemanticActivationRequestV1::new(generation, None, None).expect("activation request"),
    )
    .expect("activation command");
    let backend = DaemonSemanticRuntimeBackendV1::new(handle);

    assert_eq!(
        backend.activate(&command).await,
        Err(SemanticRuntimeBackendErrorV1::Unavailable)
    );
}

#[tokio::test]
async fn compose_application_search_skips_retriever_while_indexing() {
    let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
    let (started_tx, started_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    handle.schedule(SemanticRuntimeWorkV1::new(
        source_generation('i'),
        2,
        move |progress| async move {
            progress.set_completed_units(1);
            let _ = started_tx.send(());
            let _ = release_rx.await;
            Err(SemanticRuntimeScheduleFailureV1::Projection)
        },
    ));
    started_rx.await.expect("indexing started");

    struct PanicVectors;
    impl SemanticVectorReadPort for PanicVectors {
        fn scan_exact_flat(
            &self,
            _request: tracedecay_query::retrieval::semantic::SemanticVectorReadRequestV1<'_>,
            _examine: &mut dyn FnMut() -> Result<(), RetrievalPortError>,
            _visit: &mut dyn FnMut(
                &tracedecay_query::retrieval::semantic::SemanticVectorRecordV1,
            ) -> Result<(), RetrievalPortError>,
        ) -> Result<
            tracedecay_query::retrieval::semantic::SemanticVectorScanSummaryV1,
            RetrievalPortError,
        > {
            panic!("indexing composition must not scan vectors")
        }
    }
    struct IdleControl;
    impl RetrievalExecutionControl for IdleControl {
        fn is_cancelled(&self) -> bool {
            false
        }
        fn elapsed_micros(&self) -> u64 {
            0
        }
    }

    let authority = tracedecay_semantic::session_pool::test_support::authority();
    let source = source_generation('i');
    let vector = vector_generation('i');
    let query_view = EphemeralSanitizedQueryViewV1::sanitize(
        "compose while indexing",
        SanitizerRevision::try_from("sanitizer.v1".to_owned()).expect("sanitizer"),
        QueryNormalizationRevision::try_from("normalizer.v1".to_owned()).expect("normalizer"),
    )
    .expect("query view");
    let request = composition_request(&query_view, authority.projection(), source, vector);
    let complete = CompleteSemanticGenerationV1::new(
        request.projection.projection_key().clone(),
        request.search_index_key.clone(),
        request.vector_generation.clone(),
        request.code_generation.clone(),
        request.capability_manifest_digest.clone(),
    )
    .expect("complete generation");

    let outcome = compose_application_semantic_search(ApplicationSemanticSearchParametersV1 {
        handle: &handle,
        request: &request,
        generation: &complete,
        calibration: None,
        vectors: &PanicVectors,
        control: &IdleControl,
        mode: SemanticQueryModeV1::FallbackAllowed,
        fallback: composition_fallback(),
        source_coherence: SemanticSourceCoherenceV1::ExactGeneration,
    })
    .expect("compose while indexing");
    assert!(matches!(
        outcome,
        SemanticQueryServiceOutcomeV1::Fallback { .. }
    ));
    let _ = release_tx.send(());
}

/// Source-identity contract for #753: semantic readiness is decided by the
/// exact evaluated source content identity plus model/profile pins, never
/// by the monotonic code-generation identifier alone. Real sealed
/// generations are built through the production owner so the corpus
/// identities under test are the ones the daemon actually seals.
mod source_identity_contract {
    use std::collections::BTreeSet;
    use std::sync::Mutex as StdMutex;

    use tracedecay_code_index::chunks::content_digest as bytes_content_digest;
    use tracedecay_code_index::production::{
        CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
        CodeIndexExecutionControlV1, CodeIndexGenerationScopeV1, CodeIndexProductionConfigV1,
        CodeIndexProductionOwnerV1, CodeIndexPublicationStoreErrorV1,
        CodeIndexRepositoryParseIdentityV1,
    };
    use tracedecay_code_index::projection::{
        ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionReceiptBuilderV1,
        ProjectionSinkErrorV1, ProjectionSinkReceiptV1,
    };
    use tracedecay_domain::{
        ChangedCodeChunkV1, CommitId, EmbeddingDeviceClassV1, EmbeddingMetricV1,
        EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1,
        EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1, LanguageId, PrivacyDomainId,
        ProjectId, ProjectionOperationV1, ProjectionOutcomeV1, RefId, RepositoryDirtyStateV1,
        SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
        SnapshotFileDispositionV1, TreeId, WorktreeId,
    };
    use tracedecay_semantic::projector::{
        CanonicalChunkVectorEncoderV1, prepare_vector_generation,
    };

    use super::*;
    use crate::store::vector_generations::VectorGenerationStateMachineV1;

    fn fixture_id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("canonical fixture identity")
    }

    #[derive(Clone, Default)]
    struct InMemoryPublicationStore {
        active: Arc<
            StdMutex<BTreeMap<CodeIndexGenerationScopeV1, Arc<CodeIndexPublishedGenerationV1>>>,
        >,
    }

    impl CodeIndexAtomicPublicationPort for InMemoryPublicationStore {
        fn load_active(
            &self,
            scope: &CodeIndexGenerationScopeV1,
        ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1>
        {
            Ok(self
                .active
                .lock()
                .expect("publication lock")
                .get(scope)
                .map(Arc::clone))
        }

        fn publish_atomically(
            &mut self,
            scope: &CodeIndexGenerationScopeV1,
            expected_active_generation: Option<&CodeGenerationId>,
            generation: Arc<CodeIndexPublishedGenerationV1>,
        ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
            let mut active = self.active.lock().expect("publication lock");
            if active
                .get(scope)
                .map(|current| current.manifest().generation_id.clone())
                .as_ref()
                != expected_active_generation
            {
                return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
            }
            active.insert(scope.clone(), generation);
            Ok(())
        }
    }

    #[derive(Default)]
    struct ApplyingProjectionSink;

    impl CodeChunkProjectionSink for ApplyingProjectionSink {
        fn project_changed_chunks(
            &mut self,
            request: &ProjectionBatchRequestV1,
            receipt_builder: ProjectionReceiptBuilderV1<'_>,
        ) -> Result<ProjectionSinkReceiptV1, ProjectionSinkErrorV1> {
            let mut decisions: Vec<ChunkProjectionDecisionV1> = request
                .changes
                .added_or_changed
                .iter()
                .map(|change| ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: change.current_digest.clone(),
                    operation: if change.prior_digest.is_some() {
                        ProjectionOperationV1::Updated
                    } else {
                        ProjectionOperationV1::Added
                    },
                    outcome: ProjectionOutcomeV1::Applied,
                    output_digest: change.current_digest.clone(),
                })
                .collect();
            decisions.extend(request.changes.deleted.iter().map(|change| {
                ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: None,
                    operation: ProjectionOperationV1::Deleted,
                    outcome: ProjectionOutcomeV1::Applied,
                    output_digest: None,
                }
            }));
            decisions.extend(request.changes.reused.iter().map(|change| {
                ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: change.current_digest.clone(),
                    operation: ProjectionOperationV1::Reused,
                    outcome: ProjectionOutcomeV1::Reused,
                    output_digest: None,
                }
            }));
            receipt_builder
                .build(&decisions)
                .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
        }
    }

    struct ActiveControl;

    impl CodeIndexExecutionControlV1 for ActiveControl {
        fn is_cancelled(&self) -> bool {
            false
        }

        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }

    fn corpus_config() -> CodeIndexProductionConfigV1 {
        CodeIndexProductionConfigV1 {
            project_id: fixture_id::<ProjectId>("project.source-identity"),
            repository: fixture_id::<RepositoryId>("repository.source-identity"),
            sanitizer_revision: fixture_id::<SanitizerRevision>("sanitizer.v1"),
            policy_revision: fixture_id::<PolicyRevisionId>("policy.v1"),
            chunker_revision: fixture_id::<ChunkerRevision>("chunker.v2"),
            privacy_domain: fixture_id::<PrivacyDomainId>("privacy.source-identity"),
            privacy_key_epoch: 7,
            max_snapshot_age_micros: None,
        }
    }

    fn corpus_build_request(
        source: &str,
        revision: &str,
        file_occurrence: &str,
        sealed_at: i64,
        mark_changed: bool,
    ) -> CodeIndexBuildRequestV1 {
        let bytes = source.as_bytes().to_vec();
        let file = SanitizedCodeFileV1 {
            file_occurrence_id: fixture_id::<FileOccurrenceId>(file_occurrence),
            logical_path: "src/lib.rs".to_owned(),
            language: Some(fixture_id::<LanguageId>("rust")),
            content_digest: bytes_content_digest(&bytes),
            disposition: SnapshotFileDispositionV1::Present,
        };
        let mut changed_files = BTreeSet::new();
        if mark_changed {
            changed_files.insert("src/lib.rs".to_owned());
        }
        CodeIndexBuildRequestV1 {
            snapshot: SanitizedCodeSnapshotV1 {
                repository: fixture_id::<RepositoryId>("repository.source-identity"),
                worktree: Some(fixture_id::<WorktreeId>("worktree.source-identity")),
                reference: Some(fixture_id::<RefId>("refs/heads/source-identity")),
                source_revision: Some(fixture_id::<CommitId>(revision)),
                sanitizer_revision: fixture_id::<SanitizerRevision>("sanitizer.v1"),
                sanitization_receipts: vec![fixture_id::<SanitizationReceiptId>(
                    "receipt.source-identity",
                )],
                content_identity: bytes_content_digest(&bytes),
                captured_at: UtcMicros(sealed_at),
                files: vec![file.clone()],
            },
            captured_files: vec![CodeIndexCapturedFileV1 {
                file_occurrence_id: file.file_occurrence_id,
                sanitized_bytes: bytes.into(),
                sensitivity_level: SensitivityLevelV1::Public,
            }],
            changed_files,
            invalidations: BTreeSet::new(),
            ignored_source_admissions: Vec::new(),
            repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
                tree: Some(fixture_id::<TreeId>(&format!("tree.{revision}"))),
                dirty: RepositoryDirtyStateV1::Dirty,
            },
            sealed_at: UtcMicros(sealed_at),
            target_projection_key: ProjectionKeyV1 {
                kind: tracedecay_domain::ProjectionKindV1::Lexical,
                schema_revision: "lexical.source-identity.v1".to_owned(),
                profile_digest: test_digest('e'),
            },
        }
    }

    struct DeterministicEncoder;

    /// Fixture tokenizer: one token per whitespace-separated word, capped at the
    /// admitted truncation length. The double has no model; grouping only needs a
    /// length that varies with the document and can be predicted from a fixture.
    impl tracedecay_semantic::projector::CanonicalChunkTokenLengthsV1 for DeterministicEncoder {
        fn document_token_lengths(
            &mut self,
            key: &EmbeddingProjectionKeyV1,
            chunks: &[&CodeSearchChunkV1],
        ) -> Result<Vec<usize>, String> {
            let truncation_length = key.truncation_length as usize;
            Ok(chunks
                .iter()
                .map(|chunk| {
                    chunk
                        .sanitized_text
                        .as_str()
                        .split_whitespace()
                        .count()
                        .clamp(1, truncation_length)
                })
                .collect())
        }
    }

    impl CanonicalChunkVectorEncoderV1 for DeterministicEncoder {
        fn encode(
            &mut self,
            key: &EmbeddingProjectionKeyV1,
            chunk: &CodeSearchChunkV1,
        ) -> Result<Vec<f32>, String> {
            let seed = chunk
                .sanitized_text
                .as_str()
                .bytes()
                .fold(0u32, |sum, byte| sum.wrapping_add(u32::from(byte)));
            Ok((0..key.dimensions as usize)
                .map(|index| (seed.wrapping_add(index as u32) % 101) as f32 / 101.0)
                .collect())
        }
    }

    fn corpus_embedding_key(chunker_revision: ChunkerRevision) -> EmbeddingProjectionKeyV1 {
        EmbeddingProjectionKeyV1 {
            model_artifact_digest: test_digest('1'),
            tokenizer_digest: test_digest('2'),
            config_digest: test_digest('3'),
            query_instruction_digest: None,
            document_instruction_digest: None,
            document_composition: tracedecay_domain::EmbeddingDocumentCompositionV1::SanitizedText,
            pooling: EmbeddingPoolingV1::Mean,
            truncation_side: EmbeddingTruncationSideV1::Right,
            truncation_length: 512,
            inference_batch_size: 8,
            inference_batch_bytes: 16 * 1024,
            runtime_backend: "fastembed-ort".to_owned(),
            runtime_build_revision: "ort-source-identity-1".to_owned(),
            device_class: EmbeddingDeviceClassV1::Cpu,
            execution_provider: tracedecay_domain::EmbeddingExecutionProviderV1::Cpu,
            dimensions: 4,
            metric: EmbeddingMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            precision: EmbeddingPrecisionV1::Fp32,
            chunk_schema_revision: "code-search-chunk.v1".to_owned(),
            chunker_revision,
            privacy_domain: fixture_id::<PrivacyDomainId>("privacy.source-identity"),
            privacy_key_epoch: 7,
        }
    }

    /// Build the sealed publications the contract is decided against:
    /// `first` and `republished` seal byte-identical trees under different
    /// commits (distinct generation identifiers, one source truth), and
    /// `edited` seals genuinely different bytes.
    fn sealed_publications() -> (
        Arc<CodeIndexPublishedGenerationV1>,
        Arc<CodeIndexPublishedGenerationV1>,
        Arc<CodeIndexPublishedGenerationV1>,
    ) {
        let mut owner = CodeIndexProductionOwnerV1::new(
            corpus_config(),
            InMemoryPublicationStore::default(),
            ApplyingProjectionSink,
        )
        .expect("production owner");
        let source = "fn semantic_probe() -> u32 { 1 }\nfn stable() -> u32 { 2 }\n";
        let first = owner
            .build_and_publish(
                corpus_build_request(source, "commit.1", "file.corpus.1", 1_000_000, false),
                &ActiveControl,
            )
            .expect("first sealed publication");
        let republished = owner
            .build_and_publish(
                corpus_build_request(source, "commit.2", "file.corpus.1", 2_000_000, true),
                &ActiveControl,
            )
            .expect("same-content republication");
        let edited = owner
            .build_and_publish(
                corpus_build_request(
                    "fn semantic_probe() -> u32 { 99 }\nfn stable() -> u32 { 2 }\n",
                    "commit.3",
                    "file.corpus.3",
                    3_000_000,
                    true,
                ),
                &ActiveControl,
            )
            .expect("edited publication");
        (first, republished, edited)
    }

    /// Project real vectors for `code` through the nominal projector and
    /// state machine, exactly as the daemon stages them.
    fn published_vectors_for(code: &CodeIndexPublishedGenerationV1) -> PublishedVectorGenerationV1 {
        let chunks = code.chunks().chunks();
        assert!(
            !chunks.is_empty(),
            "the sealed fixture generation must chunk its source"
        );
        let key = corpus_embedding_key(chunks[0].chunker_revision.clone());
        let admitted = key.admit().expect("admitted embedding projection");
        let mut changes = ChangedCodeChunkSetV1 {
            from_generation: None,
            to_generation: code.manifest().generation_id.clone(),
            manifest_digest: test_digest('0'),
            added_or_changed: chunks
                .iter()
                .map(|chunk| ChangedCodeChunkV1 {
                    chunk_id: chunk.id.clone(),
                    prior_digest: None,
                    current_digest: Some(chunk.content_digest.clone()),
                })
                .collect(),
            deleted: Vec::new(),
            reused: Vec::new(),
        };
        changes.manifest_digest = changes.compute_digest().expect("changed-set digest");
        let mut request = ProjectionBatchRequestV1 {
            request_digest: test_digest('0'),
            changes,
            previous_projection_key: None,
            target_projection_key: admitted.projection_key().clone(),
            replay_reason: ProjectionReplayReasonV1::InitialProjection,
        };
        request.request_digest =
            expected_request_digest(&request).expect("projection request digest");
        let prepared =
            prepare_vector_generation(&admitted, request, chunks, &mut DeterministicEncoder)
                .expect("prepared projection");
        let mut machine = VectorGenerationStateMachineV1::new();
        let build = machine
            .begin_generation(VectorGenerationPlanV1 {
                target_projection_key: admitted.projection_key().clone(),
                source_generation: code.manifest().generation_id.clone(),
                source_manifest_digest: prepared.receipt.source_manifest_digest.clone(),
                expected_chunk_ids: chunks
                    .iter()
                    .map(|chunk| chunk.id.clone())
                    .collect::<Vec<_>>()
                    .into(),
                base_generation: None,
            })
            .expect("staged vector generation");
        machine
            .commit_batch(&build, None, prepared)
            .expect("committed projection batch");
        let publication = machine
            .publish_generation(&build)
            .expect("published vector generation");
        machine
            .generation(&publication.generation_id)
            .expect("readable published vector generation")
            .clone()
    }

    /// #753 success test 1: a republication of byte-identical source under
    /// a new code-generation identifier must not invalidate the evaluated
    /// semantic generation. The proof is the sealed corpus identity, not
    /// the monotonic identifier.
    #[test]
    fn a_same_content_republication_keeps_the_semantic_generation_valid() {
        let (first, republished, _) = sealed_publications();
        assert_ne!(
            first.manifest().generation_id,
            republished.manifest().generation_id,
            "the republication must mint a new physical identifier"
        );
        assert_eq!(
            first.snapshot().content_identity,
            republished.snapshot().content_identity,
            "the republication must seal the same source truth"
        );
        let vectors = published_vectors_for(&first);
        assert_eq!(vectors.source_generation(), &first.manifest().generation_id);
        let first_commitments = first
            .manifest()
            .source_commitments
            .as_ref()
            .expect("first source commitments");
        let republished_commitments = republished
            .manifest()
            .source_commitments
            .as_ref()
            .expect("republished source commitments");
        assert_eq!(
            vectors
                .accepted_source_full_replay_digest()
                .expect("authenticated vector source"),
            first_commitments.full_replay_digest
        );
        assert_eq!(
            first_commitments.full_replay_digest,
            republished_commitments.full_replay_digest
        );

        assert_eq!(
            semantic_source_coherence(&vectors, republished.manifest()),
            SemanticSourceCoherenceOutcomeV1::Coherent(
                SemanticSourceCoherenceV1::ProvenSourceContent
            ),
            "a generation-id change alone must not invalidate the semantic generation"
        );

        // Exact-generation admission stays strict: the physical identifier
        // still refuses without the content proof.
        let search_index_key = search_index_key().clone();
        assert!(matches!(
            PublishedSemanticVectorReadPortV1::new(
                vectors.clone(),
                search_index_key.clone(),
                &republished,
                None,
            ),
            Err(RetrievalPortError::GenerationMismatch)
        ));

        // Proven-content admission serves, rebound to the publication that
        // queries actually pin.
        let port = PublishedSemanticVectorReadPortV1::new_source_coherent(
            vectors.clone(),
            search_index_key.clone(),
            &republished,
            None,
        )
        .expect("content-proven vectors serve the republication");
        assert_eq!(
            port.source_coherence,
            SemanticSourceCoherenceV1::ProvenSourceContent
        );
        assert_eq!(
            port.source_generation,
            republished.manifest().generation_id,
            "the port binds the serving publication identity"
        );
        assert_eq!(port.rows.len(), republished.chunks().chunks().len());
        assert!(
            port.rows.iter().all(|row| row.source_generation
                == republished.manifest().generation_id
                && row.vector_generation == *vectors.generation_id()),
            "rows carry the serving source identity and the exact physical vector identity"
        );

        // The exact source keeps serving exactly.
        let exact = PublishedSemanticVectorReadPortV1::new_source_coherent(
            vectors,
            search_index_key,
            &first,
            None,
        )
        .expect("the exact source generation still serves");
        assert_eq!(
            exact.source_coherence,
            SemanticSourceCoherenceV1::ExactGeneration
        );
    }

    /// #753 success test 2: a serving generation sealing different source
    /// content is a typed mismatch that names both identities; nothing
    /// attaches silently.
    #[test]
    fn a_different_source_identity_is_a_typed_mismatch_naming_both_identities() {
        let (first, _, edited) = sealed_publications();
        assert_ne!(
            first.snapshot().content_identity,
            edited.snapshot().content_identity,
            "the edited publication must seal different source truth"
        );
        let vectors = published_vectors_for(&first);

        let outcome = semantic_source_coherence(&vectors, edited.manifest());
        let SemanticSourceCoherenceOutcomeV1::Mismatch(mismatch) = outcome else {
            panic!("different source content must be a typed mismatch: {outcome:?}");
        };
        assert_eq!(
            mismatch.vector_source_generation,
            first.manifest().generation_id,
            "the mismatch names the identity the vectors were evaluated from"
        );
        assert_eq!(
            &mismatch.vector_source_manifest_digest,
            vectors.source_manifest_digest(),
            "the mismatch names the evaluated source manifest digest"
        );
        assert_eq!(
            mismatch.serving_generation,
            edited.manifest().generation_id,
            "the mismatch names the serving publication"
        );
        assert_eq!(
            mismatch.vector_source_full_replay_digest,
            first
                .manifest()
                .source_commitments
                .as_ref()
                .expect("first source commitments")
                .full_replay_digest,
            "the mismatch names the vector source content identity"
        );
        assert_eq!(
            mismatch.serving_source_full_replay_digest,
            edited
                .manifest()
                .source_commitments
                .as_ref()
                .expect("edited source commitments")
                .full_replay_digest,
            "the mismatch names the serving source content identity"
        );

        // No silent attach on either admission path.
        let search_index_key = search_index_key().clone();
        assert!(matches!(
            PublishedSemanticVectorReadPortV1::new(
                vectors.clone(),
                search_index_key.clone(),
                &edited,
                None,
            ),
            Err(RetrievalPortError::GenerationMismatch)
        ));
        assert!(matches!(
            PublishedSemanticVectorReadPortV1::new_source_coherent(
                vectors,
                search_index_key,
                &edited,
                None,
            ),
            Err(RetrievalPortError::GenerationMismatch)
        ));
    }

    #[test]
    fn source_coherence_reports_missing_sealed_commitments_as_unavailable() {
        let (first, _, _) = sealed_publications();
        let vectors = published_vectors_for(&first);
        let mut historical = first.manifest().clone();
        historical.source_commitments = None;

        assert_eq!(
            semantic_source_coherence(&vectors, &historical),
            SemanticSourceCoherenceOutcomeV1::Unavailable(
                SemanticSourceUnavailableV1::ServingCommitmentsMissing
            )
        );
    }
}
