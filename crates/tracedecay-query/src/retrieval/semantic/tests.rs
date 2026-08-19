use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CalibrationProfileId, ChunkerRevision, CodeGenerationId,
    CodeSearchChunkId, CompactCandidate, ComponentRevision, DiversityPolicy,
    EmbeddingDeviceClassV1, EmbeddingMetricV1, EmbeddingNormalizationV1, EmbeddingPoolingV1,
    EmbeddingPrecisionV1, EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1, EvidenceRole,
    ExactClass, FreshnessCompatibilityV1, FusionProfile, LogicalEvidenceId, ManifestDigest,
    PrincipalId, ProjectionKeyV1, PublicRetrieverStatus, QueryDigest, QueryFallbackSubpayload,
    QueryMac, QueryNormalizationRevision, RetrievalAnchorId, RetrievalBudget, RetrievalCursorKeyId,
    RetrievalRequest, RetrievalScope, RetrievalSnapshot, Retriever, RetrieverBatch,
    RetrieverCoverage, RetrieverKind, RetrieverOutcome, SanitizerRevision,
    ScoreDomainCalibrationV1, ScoreDomainId, SemanticSearchIndexKeyV1,
    SemanticSearchIndexProfileV1, SingleRootScopeV1, SourceFreshness, SourceNamespace,
    SourceOccurrenceId, TemporalModeV1, UtcMicros, VectorGenerationIdV1, VectorWatermark,
};

use super::*;
use crate::retrieval::fusion::{
    CompositionKernel, CompositionLaneInput, FusionStageInput, RetrievalCursorKeyringV1,
};
use crate::retrieval::hydrate::{
    CanonicalLateHydration, HydrationAuthorizationV1, HydrationPreflightOutcomeV1,
    HydrationReadOutcomeV1, HydrationWorkPermitV1, LateHydrationSource,
};
use crate::retrieval::ports::{CodeCandidateBindingV1, CodeOccurrenceRefV1, RetrievalPortError};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

fn digest<T>(byte: char) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn projection() -> AdmittedEmbeddingProjectionKeyV1 {
    EmbeddingProjectionKeyV1 {
        model_artifact_digest: digest('a'),
        tokenizer_digest: digest('b'),
        config_digest: digest('c'),
        query_instruction_digest: Some(digest('d')),
        document_instruction_digest: Some(digest('e')),
        pooling: EmbeddingPoolingV1::Mean,
        truncation_side: EmbeddingTruncationSideV1::Right,
        truncation_length: 128,
        inference_batch_size: 8,
        inference_batch_bytes: 4 * 1024,
        runtime_backend: "onnx.cpu".to_owned(),
        runtime_build_revision: "runtime.v1".to_owned(),
        device_class: EmbeddingDeviceClassV1::Cpu,
        dimensions: 2,
        metric: EmbeddingMetricV1::Cosine,
        normalization: EmbeddingNormalizationV1::L2,
        precision: EmbeddingPrecisionV1::Fp32,
        chunk_schema_revision: "chunk.v1".to_owned(),
        chunker_revision: id::<ChunkerRevision>("chunker.v1"),
        privacy_domain: id("privacy.fixture"),
        privacy_key_epoch: 7,
    }
    .admit()
    .expect("valid admitted projection")
}

fn search_index_key() -> &'static SemanticSearchIndexKeyV1 {
    static KEY: OnceLock<SemanticSearchIndexKeyV1> = OnceLock::new();
    KEY.get_or_init(|| {
        SemanticSearchIndexProfileV1::exact_flat_v1()
            .and_then(|profile| profile.index_key())
            .expect("exact-flat search index key")
    })
}

fn budget(max_candidates_per_lane: u32) -> RetrievalBudget {
    RetrievalBudget {
        max_candidates_per_lane,
        max_fused_candidates: 16,
        max_hydrated_results: 8,
        max_hydration_bytes: 65_536,
        deadline_micros: None,
    }
}

fn base_request(max_candidates_per_lane: u32) -> RetrievalRequest {
    RetrievalRequest {
        principal: id::<PrincipalId>("principal.fixture"),
        scope: RetrievalScope {
            privacy_domain: id("privacy.fixture"),
            root: SingleRootScopeV1 {
                repository: id("repository.fixture"),
                worktree: None,
                reference: None,
            },
        },
        temporal_mode: TemporalModeV1::Current,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: digest('f'),
            authorization_revision: id("authorization.v1"),
            captured_at: UtcMicros(7),
        },
        profile_id: id("profile.semantic.v1"),
        budget: budget(max_candidates_per_lane),
    }
}

fn query_view() -> tracedecay_domain::EphemeralSanitizedQueryViewV1 {
    tracedecay_domain::EphemeralSanitizedQueryViewV1::sanitize(
        "semantic query",
        id::<SanitizerRevision>("sanitizer.v1"),
        id::<QueryNormalizationRevision>("normalizer.v1"),
    )
    .expect("valid ephemeral query")
}

fn query_digest(byte: char) -> QueryDigest {
    QueryDigest::new(
        id("privacy.fixture"),
        7,
        QueryMac::new(format!("hmac-sha256:{}", byte.to_string().repeat(64)))
            .expect("valid query MAC"),
    )
}

fn request<'a>(
    query_view: &'a tracedecay_domain::EphemeralSanitizedQueryViewV1,
    projection: &'a AdmittedEmbeddingProjectionKeyV1,
    max_candidates_per_lane: u32,
) -> SemanticRetrievalRequestV1<'a> {
    SemanticRetrievalRequestV1 {
        base: base_request(max_candidates_per_lane),
        query_digest: query_digest('1'),
        query_view,
        projection,
        search_index_key: search_index_key(),
        capability_manifest_digest: digest('9'),
        vector_generation: VectorGenerationIdV1::new(digest('8')),
        code_generation: id("generation.1"),
        budget: budget(max_candidates_per_lane),
    }
}

fn freshness() -> SourceFreshness {
    SourceFreshness {
        source_namespace: id("namespace.code"),
        source_instance: id("source.fixture"),
        source_watermark: Some(7),
        projection_watermark: Some(7),
        observed_at: UtcMicros(7),
        source_generation: Some(1),
        generation_lag: Some(0),
        compatibility: FreshnessCompatibilityV1::Current,
        policy_revision: id("policy.v1"),
    }
}

fn record(
    request: &SemanticRetrievalRequestV1<'_>,
    name: &str,
    values: Vec<f32>,
) -> SemanticVectorRecordV1 {
    let source_occurrence_id = id::<SourceOccurrenceId>(&format!("occurrence.{name}"));
    let anchor_id = RetrievalAnchorId::new(format!("anchor.{name}")).expect("valid anchor");
    let chunk_id = id::<CodeSearchChunkId>(&format!("chunk.{name}"));
    SemanticVectorRecordV1 {
        vector_generation: request.vector_generation.clone(),
        projection_key: request.projection.projection_key().clone(),
        source_generation: request.code_generation.clone(),
        chunk_id: chunk_id.clone(),
        candidate: CompactCandidate {
            anchor_id: anchor_id.clone(),
            logical_evidence_id: id::<LogicalEvidenceId>(&format!("logical.{name}")),
            source_occurrence_id: source_occurrence_id.clone(),
            file_occurrence_id: Some(id(&format!("file.{name}"))),
            source_namespace: id::<SourceNamespace>("namespace.code"),
            repository_id: Some(id("repository.fixture")),
            session_or_thread_id: None,
            logical_copy_cluster_id: None,
            logical_copy_evidence_anchor: None,
            evidence_role: EvidenceRole::Primary,
            retriever: RetrieverKind::Semantic,
            retriever_revision: id::<ComponentRevision>("retriever.semantic-flat.v1"),
            score_domain: id::<ScoreDomainId>("score.semantic-distance.v1"),
            raw_score: tracedecay_domain::FixedPointScore::ZERO,
            ordinal_rank: 0,
            exact_admission_proof: None,
            retriever_evidence_anchor: RetrievalAnchorId::new(format!("evidence.{name}"))
                .expect("valid evidence anchor"),
            freshness: freshness(),
        },
        binding: CodeCandidateBindingV1 {
            candidate_anchor: anchor_id,
            occurrence: CodeOccurrenceRefV1 {
                generation: request.code_generation.clone(),
                file: id(&format!("file.{name}")),
                symbol: None,
                chunk: Some(chunk_id),
            },
            language_descriptor_revision: id("language.rust.v1"),
            matched_term_kinds: Vec::new(),
            source_occurrence: source_occurrence_id,
        },
        values,
    }
}

#[derive(Default)]
struct FakeQueryEmbedder {
    calls: Cell<u32>,
}

impl SemanticQueryEmbeddingPort for FakeQueryEmbedder {
    fn embed_query(
        &self,
        request: SemanticQueryEmbeddingRequestV1<'_>,
    ) -> Result<EphemeralQueryEmbeddingV1, RetrievalPortError> {
        self.calls.set(self.calls.get() + 1);
        assert_eq!(request.query_digest, &query_digest('1'));
        assert_eq!(request.query_view.as_str(), "semantic query");
        EphemeralQueryEmbeddingV1::new(
            request.query_digest.clone(),
            request.projection.clone(),
            vec![1.0, 0.0],
        )
    }
}

struct FakeVectorReadPort {
    rows: Vec<SemanticVectorRecordV1>,
    scans: Cell<u32>,
    summary: Option<SemanticVectorScanSummaryV1>,
    after_scan_cancel: Option<Rc<Cell<bool>>>,
    after_scan_elapsed: Option<(Rc<Cell<u64>>, u64)>,
    vector_generation: VectorGenerationIdV1,
    projection_key: ProjectionKeyV1,
    search_index_key: SemanticSearchIndexKeyV1,
    source_generation: CodeGenerationId,
    capability_manifest_digest: ManifestDigest,
}

impl FakeVectorReadPort {
    fn new(request: &SemanticRetrievalRequestV1<'_>, rows: Vec<SemanticVectorRecordV1>) -> Self {
        Self {
            rows,
            scans: Cell::new(0),
            summary: None,
            after_scan_cancel: None,
            after_scan_elapsed: None,
            vector_generation: request.vector_generation.clone(),
            projection_key: request.projection.projection_key().clone(),
            search_index_key: request.search_index_key.clone(),
            source_generation: request.code_generation.clone(),
            capability_manifest_digest: request.capability_manifest_digest.clone(),
        }
    }
}

impl SemanticVectorReadPort for FakeVectorReadPort {
    fn scan_exact_flat(
        &self,
        request: SemanticVectorReadRequestV1<'_>,
        visit: &mut dyn FnMut(&SemanticVectorRecordV1) -> Result<(), RetrievalPortError>,
    ) -> Result<SemanticVectorScanSummaryV1, RetrievalPortError> {
        self.scans.set(self.scans.get() + 1);
        assert_eq!(request.vector_generation, &self.vector_generation);
        assert_eq!(request.projection_key, &self.projection_key);
        assert_eq!(request.search_index_key, &self.search_index_key);
        assert_eq!(request.source_generation, &self.source_generation);
        assert_eq!(
            request.capability_manifest_digest,
            &self.capability_manifest_digest
        );
        assert_eq!(request.search_kind, SemanticSearchKindV1::ExactFlat);
        for row in &self.rows {
            visit(row)?;
        }
        if let Some(cancelled) = &self.after_scan_cancel {
            cancelled.set(true);
        }
        if let Some((elapsed, value)) = &self.after_scan_elapsed {
            elapsed.set(*value);
        }
        Ok(self.summary.unwrap_or(SemanticVectorScanSummaryV1 {
            examined: self.rows.len() as u64,
            eligible: self.rows.len() as u64,
            excluded: 0,
            unknown: 0,
        }))
    }
}

#[derive(Clone, Default)]
struct FixedExecutionControl {
    cancelled: Rc<Cell<bool>>,
    elapsed_micros: Rc<Cell<u64>>,
    cancellation_checks: Rc<Cell<u32>>,
    cancel_after_checks: Option<u32>,
    elapsed_checks: Rc<Cell<u32>>,
    expire_after_elapsed_checks: Option<u32>,
}

impl SemanticExecutionControl for FixedExecutionControl {
    fn is_cancelled(&self) -> bool {
        let checks = self.cancellation_checks.get() + 1;
        self.cancellation_checks.set(checks);
        if self
            .cancel_after_checks
            .is_some_and(|allowed| checks > allowed)
        {
            return true;
        }
        self.cancelled.get()
    }

    fn elapsed_micros(&self) -> u64 {
        let checks = self.elapsed_checks.get() + 1;
        self.elapsed_checks.set(checks);
        if self
            .expire_after_elapsed_checks
            .is_some_and(|allowed| checks > allowed)
        {
            return u64::MAX;
        }
        self.elapsed_micros.get()
    }
}

struct FixedSemanticLane {
    calls: Cell<u32>,
    outcome: Result<RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>, RetrievalPortError>,
}

impl SemanticLaneRetriever for FixedSemanticLane {
    fn retrieve_semantic(
        &self,
        _request: &SemanticRetrievalRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>, RetrievalPortError> {
        self.calls.set(self.calls.get() + 1);
        self.outcome.clone()
    }
}

#[derive(Default)]
struct DenyingHydrationSource {
    authorization_checks: usize,
    payload_reads: usize,
}

impl LateHydrationSource<String> for DenyingHydrationSource {
    fn authorize(
        &mut self,
        _request: &RetrievalRequest,
        _candidate: &tracedecay_domain::RankedCandidate,
    ) -> HydrationAuthorizationV1 {
        self.authorization_checks += 1;
        HydrationAuthorizationV1::Denied
    }

    fn preflight_authorized(
        &mut self,
        _request: &RetrievalRequest,
        _candidate: &tracedecay_domain::RankedCandidate,
        _permit: &HydrationWorkPermitV1,
    ) -> HydrationPreflightOutcomeV1 {
        panic!("denied semantic candidate must not reach hydration preflight")
    }

    fn hydrate_authorized(
        &mut self,
        _request: &RetrievalRequest,
        _candidate: &tracedecay_domain::RankedCandidate,
        _permit: &HydrationWorkPermitV1,
    ) -> HydrationReadOutcomeV1<String> {
        self.payload_reads += 1;
        panic!("denied semantic candidate must not read payload")
    }
}

#[test]
fn exact_flat_scan_is_deterministic_and_emits_generic_semantic_evidence() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 2);
    let rows = vec![
        record(&request, "orthogonal", vec![0.0, 1.0]),
        record(&request, "opposite", vec![-1.0, 0.0]),
        record(&request, "identical", vec![1.0, 0.0]),
    ];
    let embedder = FakeQueryEmbedder::default();
    let vectors = FakeVectorReadPort::new(&request, rows);
    let control = FixedExecutionControl::default();
    let retriever = SemanticCodeRetriever::new(&embedder, &vectors, &control);

    let outcome = Retriever::<SemanticRetrievalRequestV1<'_>, CodeSemanticEvidenceV1>::retrieve(
        &retriever, &request,
    )
    .expect("semantic retrieval succeeds");
    let RetrieverOutcome::Complete(batch) = outcome else {
        panic!("expected a complete semantic batch");
    };

    assert_eq!(embedder.calls.get(), 1);
    assert_eq!(vectors.scans.get(), 1);
    assert_eq!(
        batch
            .candidates
            .iter()
            .map(|candidate| candidate.source_occurrence_id.as_str())
            .collect::<Vec<_>>(),
        vec!["occurrence.identical", "occurrence.orthogonal"]
    );
    assert_eq!(
        batch
            .candidates
            .iter()
            .map(|candidate| candidate.ordinal_rank)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert!(
        batch.candidates[0].raw_score > batch.candidates[1].raw_score,
        "smaller canonical distance must sort first"
    );
    assert_eq!(batch.coverage.examined, 3);
    assert_eq!(batch.coverage.eligible, 3);
    assert_eq!(batch.coverage.capped, 1);
    let continuation = batch.continuation.expect("bounded continuation");
    assert_eq!(continuation.lane, RetrieverKind::Semantic);
    assert!(!continuation.exhausted);

    let evidence = batch
        .evidence_by_occurrence
        .get(&id("occurrence.identical"))
        .expect("occurrence-keyed semantic evidence");
    assert_eq!(evidence.search_kind, SemanticSearchKindV1::ExactFlat);
    assert_eq!(evidence.distance.micros(), 0);
    assert_eq!(evidence.vector_generation, request.vector_generation);
    assert_eq!(evidence.projection_key, *request.projection.embedding_key());
}

#[test]
fn bounded_scan_retains_the_cap_smallest_rows_by_tie_break_order() {
    // Equivalence guard for the bounded ExactFlat scan (finding 15). Every row
    // shares an identical distance, so the retained set is decided purely by the
    // secondary tie-break key (`source_occurrence_id`). Rows are emitted in
    // reverse tie-break order to prove the max-heap evicts the worst retained
    // row regardless of emission order, exactly as a full sort followed by
    // truncation to the cap would.
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 2);
    let rows = vec![
        record(&request, "c", vec![0.0, 1.0]),
        record(&request, "b", vec![0.0, 1.0]),
        record(&request, "a", vec![0.0, 1.0]),
    ];
    let embedder = FakeQueryEmbedder::default();
    let vectors = FakeVectorReadPort::new(&request, rows);
    let control = FixedExecutionControl::default();
    let retriever = SemanticCodeRetriever::new(&embedder, &vectors, &control);

    let outcome = Retriever::<SemanticRetrievalRequestV1<'_>, CodeSemanticEvidenceV1>::retrieve(
        &retriever, &request,
    )
    .expect("semantic retrieval succeeds");
    let RetrieverOutcome::Complete(batch) = outcome else {
        panic!("expected a complete semantic batch");
    };

    // The two smallest by tie-break (a, b) are kept in ascending order; c is
    // dropped even though it was emitted first.
    assert_eq!(
        batch
            .candidates
            .iter()
            .map(|candidate| candidate.source_occurrence_id.as_str())
            .collect::<Vec<_>>(),
        vec!["occurrence.a", "occurrence.b"]
    );
    assert_eq!(
        batch
            .candidates
            .iter()
            .map(|candidate| candidate.ordinal_rank)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    // All three rows were still visited and accounted for; only retention is
    // bounded.
    assert_eq!(batch.coverage.examined, 3);
    assert_eq!(batch.coverage.eligible, 3);
    assert_eq!(batch.coverage.capped, 1);
    let continuation = batch.continuation.expect("bounded continuation");
    assert!(!continuation.exhausted);
}

#[test]
fn capped_exact_flat_scan_materializes_only_retained_rows() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 2);
    let rows = vec![
        record(&request, "a", vec![1.0, 0.0]),
        record(&request, "b", vec![1.0, 0.0]),
        record(&request, "c", vec![0.0, 1.0]),
        record(&request, "d", vec![0.0, 1.0]),
    ];
    let embedder = FakeQueryEmbedder::default();
    let vectors = FakeVectorReadPort::new(&request, rows);
    let control = FixedExecutionControl::default();
    let _ = take_semantic_retained_materializations();

    let outcome = SemanticCodeRetriever::new(&embedder, &vectors, &control)
        .retrieve_semantic(&request)
        .expect("semantic retrieval");
    let RetrieverOutcome::Complete(batch) = outcome else {
        panic!("expected a complete semantic batch");
    };

    assert_eq!(batch.coverage.examined, 4);
    assert_eq!(batch.coverage.eligible, 4);
    assert_eq!(batch.candidates.len(), 2);
    assert_eq!(take_semantic_retained_materializations(), 2);
}

#[test]
fn privacy_identity_mismatch_fails_before_embedding_or_vector_reads() {
    let query_view = query_view();
    let projection = projection();
    let mut request = request(&query_view, &projection, 4);
    request.query_digest = QueryDigest::new(
        id("privacy.foreign"),
        7,
        QueryMac::new(format!("hmac-sha256:{}", "2".repeat(64))).expect("valid query MAC"),
    );
    let embedder = FakeQueryEmbedder::default();
    let vectors = FakeVectorReadPort::new(&request, Vec::new());
    let control = FixedExecutionControl::default();
    let retriever = SemanticCodeRetriever::new(&embedder, &vectors, &control);

    let error = retriever
        .retrieve_semantic(&request)
        .expect_err("foreign privacy identity must fail closed");

    assert!(matches!(error, RetrievalPortError::Contract(_)));
    assert_eq!(embedder.calls.get(), 0);
    assert_eq!(vectors.scans.get(), 0);
}

#[test]
fn cancellation_invokes_no_embedding_or_vector_authority() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 4);
    let embedder = FakeQueryEmbedder::default();
    let vectors = FakeVectorReadPort::new(&request, Vec::new());
    let control = FixedExecutionControl {
        cancelled: Rc::new(Cell::new(true)),
        elapsed_micros: Rc::new(Cell::new(0)),
        ..FixedExecutionControl::default()
    };
    let retriever = SemanticCodeRetriever::new(&embedder, &vectors, &control);

    assert_eq!(
        retriever
            .retrieve_semantic(&request)
            .expect("cancellation is a typed lane outcome"),
        RetrieverOutcome::Cancelled
    );
    assert_eq!(embedder.calls.get(), 0);
    assert_eq!(vectors.scans.get(), 0);
}

#[test]
fn scan_rejects_foreign_generation_rows_instead_of_broadening_scope() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 4);
    let mut foreign = record(&request, "foreign", vec![1.0, 0.0]);
    foreign.source_generation = id::<CodeGenerationId>("generation.foreign");
    let embedder = FakeQueryEmbedder::default();
    let vectors = FakeVectorReadPort::new(&request, vec![foreign]);
    let control = FixedExecutionControl::default();
    let retriever = SemanticCodeRetriever::new(&embedder, &vectors, &control);

    let error = retriever
        .retrieve_semantic(&request)
        .expect_err("foreign vector generation must fail closed");

    assert_eq!(error, RetrievalPortError::GenerationMismatch);
}

struct MismatchedDigestEmbedder;

impl SemanticQueryEmbeddingPort for MismatchedDigestEmbedder {
    fn embed_query(
        &self,
        request: SemanticQueryEmbeddingRequestV1<'_>,
    ) -> Result<EphemeralQueryEmbeddingV1, RetrievalPortError> {
        EphemeralQueryEmbeddingV1::new(
            query_digest('2'),
            request.projection.clone(),
            vec![1.0, 0.0],
        )
    }
}

#[test]
fn embedding_result_must_echo_the_exact_query_digest() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 4);
    let embedder = MismatchedDigestEmbedder;
    let vectors = FakeVectorReadPort::new(&request, Vec::new());
    let control = FixedExecutionControl::default();
    let retriever = SemanticCodeRetriever::new(&embedder, &vectors, &control);

    assert_eq!(
        retriever
            .retrieve_semantic(&request)
            .expect_err("a lookalike query embedding must fail closed"),
        RetrievalPortError::IncompatibleProjection
    );
    assert_eq!(vectors.scans.get(), 0);
}

#[test]
fn ties_and_checkpoints_are_stable_across_vector_emission_order() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 4);
    let left_rows = vec![
        record(&request, "zeta", vec![0.0, 1.0]),
        record(&request, "alpha", vec![0.0, -1.0]),
    ];
    let mut right_rows = left_rows.clone();
    right_rows.reverse();
    let left_embedder = FakeQueryEmbedder::default();
    let right_embedder = FakeQueryEmbedder::default();
    let left_vectors = FakeVectorReadPort::new(&request, left_rows);
    let right_vectors = FakeVectorReadPort::new(&request, right_rows);
    let left_control = FixedExecutionControl::default();
    let right_control = FixedExecutionControl::default();
    let left = SemanticCodeRetriever::new(&left_embedder, &left_vectors, &left_control)
        .retrieve_semantic(&request)
        .expect("left scan");
    let right = SemanticCodeRetriever::new(&right_embedder, &right_vectors, &right_control)
        .retrieve_semantic(&request)
        .expect("right scan");
    let (RetrieverOutcome::Complete(left), RetrieverOutcome::Complete(right)) = (left, right)
    else {
        panic!("both scans must complete");
    };

    assert_eq!(left.candidates, right.candidates);
    assert_eq!(left.evidence_by_occurrence, right.evidence_by_occurrence);
    assert_eq!(
        left.continuation
            .expect("left continuation")
            .checkpoint_digest,
        right
            .continuation
            .expect("right continuation")
            .checkpoint_digest
    );
}

#[test]
fn canonical_distance_supports_every_admitted_metric() {
    assert_eq!(
        canonical_distance(EmbeddingMetricV1::Cosine, &[1.0, 0.0], &[0.0, 1.0])
            .expect("cosine distance")
            .micros(),
        1_000_000_000
    );
    assert_eq!(
        canonical_distance(EmbeddingMetricV1::DotProduct, &[2.0, 0.0], &[3.0, 0.0])
            .expect("dot-product distance")
            .micros(),
        -6_000_000_000
    );
    assert_eq!(
        canonical_distance(EmbeddingMetricV1::EuclideanL2, &[0.0, 0.0], &[3.0, 4.0])
            .expect("L2 distance")
            .micros(),
        5_000_000_000
    );
}

#[test]
fn non_finite_and_zero_norm_vectors_fail_closed() {
    assert!(matches!(
        canonical_distance(EmbeddingMetricV1::Cosine, &[f32::NAN, 0.0], &[1.0, 0.0]),
        Err(RetrievalPortError::Contract(_))
    ));
    assert!(matches!(
        canonical_distance(
            EmbeddingMetricV1::DotProduct,
            &[1.0, 0.0],
            &[f32::INFINITY, 0.0]
        ),
        Err(RetrievalPortError::Contract(_))
    ));
    assert!(matches!(
        canonical_distance(EmbeddingMetricV1::Cosine, &[0.0, 0.0], &[1.0, 0.0]),
        Err(RetrievalPortError::Contract(_))
    ));
}

#[test]
fn malformed_scan_coverage_is_rejected() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 4);
    let embedder = FakeQueryEmbedder::default();
    let mut vectors =
        FakeVectorReadPort::new(&request, vec![record(&request, "one", vec![1.0, 0.0])]);
    vectors.summary = Some(SemanticVectorScanSummaryV1 {
        examined: 1,
        eligible: 0,
        excluded: 1,
        unknown: 0,
    });
    let control = FixedExecutionControl::default();

    assert!(matches!(
        SemanticCodeRetriever::new(&embedder, &vectors, &control).retrieve_semantic(&request),
        Err(RetrievalPortError::Contract(_))
    ));
}

#[test]
fn unknown_vector_coverage_is_unavailable_and_never_returns_candidates() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 4);
    let embedder = FakeQueryEmbedder::default();
    let mut vectors =
        FakeVectorReadPort::new(&request, vec![record(&request, "one", vec![1.0, 0.0])]);
    vectors.summary = Some(SemanticVectorScanSummaryV1 {
        examined: 2,
        eligible: 1,
        excluded: 0,
        unknown: 1,
    });
    let control = FixedExecutionControl::default();

    assert!(matches!(
        SemanticCodeRetriever::new(&embedder, &vectors, &control)
            .retrieve_semantic(&request)
            .expect("unknown coverage is a typed lane outcome"),
        RetrieverOutcome::Unavailable(
            tracedecay_domain::RetrievalFailure::AuthorityUnavailable { .. }
        )
    ));
}

#[test]
fn complete_uncapped_scan_marks_continuation_exhausted() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 4);
    let embedder = FakeQueryEmbedder::default();
    let vectors = FakeVectorReadPort::new(&request, vec![record(&request, "one", vec![1.0, 0.0])]);
    let control = FixedExecutionControl::default();
    let outcome = SemanticCodeRetriever::new(&embedder, &vectors, &control)
        .retrieve_semantic(&request)
        .expect("semantic retrieval");
    let RetrieverOutcome::Complete(batch) = outcome else {
        panic!("scan must complete");
    };

    assert!(batch.continuation.expect("semantic continuation").exhausted);
}

#[test]
fn elapsed_deadline_before_scan_invokes_no_authority() {
    let query_view = query_view();
    let projection = projection();
    let mut request = request(&query_view, &projection, 4);
    request.budget.deadline_micros = Some(5);
    let embedder = FakeQueryEmbedder::default();
    let vectors = FakeVectorReadPort::new(&request, Vec::new());
    let control = FixedExecutionControl {
        elapsed_micros: Rc::new(Cell::new(5)),
        ..FixedExecutionControl::default()
    };

    assert!(matches!(
        SemanticCodeRetriever::new(&embedder, &vectors, &control)
            .retrieve_semantic(&request)
            .expect("typed budget outcome"),
        RetrieverOutcome::BudgetExceeded(_)
    ));
    assert_eq!(embedder.calls.get(), 0);
    assert_eq!(vectors.scans.get(), 0);
}

#[test]
fn omitted_request_deadline_uses_crate_exact_flat_default() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 4);
    assert!(request.budget.deadline_micros.is_none());
    assert!(request.base.budget.deadline_micros.is_none());
    let embedder = FakeQueryEmbedder::default();
    let vectors = FakeVectorReadPort::new(&request, Vec::new());
    let control = FixedExecutionControl {
        elapsed_micros: Rc::new(Cell::new(SEMANTIC_EXACT_FLAT_DEFAULT_DEADLINE_MICROS_V1)),
        ..FixedExecutionControl::default()
    };

    assert!(matches!(
        SemanticCodeRetriever::new(&embedder, &vectors, &control)
            .retrieve_semantic(&request)
            .expect("typed budget outcome"),
        RetrieverOutcome::BudgetExceeded(_)
    ));
    assert_eq!(embedder.calls.get(), 0);
    assert_eq!(vectors.scans.get(), 0);
}

#[test]
fn cancellation_and_deadline_are_checked_during_scan() {
    let query_view = query_view();
    let projection = projection();
    let mut request = request(&query_view, &projection, 4);
    request.budget.deadline_micros = Some(5);
    let row = record(&request, "one", vec![1.0, 0.0]);

    let cancelled_embedder = FakeQueryEmbedder::default();
    let cancelled_vectors = FakeVectorReadPort::new(&request, vec![row.clone()]);
    let cancelled_control = FixedExecutionControl {
        cancel_after_checks: Some(2),
        ..FixedExecutionControl::default()
    };
    assert_eq!(
        SemanticCodeRetriever::new(&cancelled_embedder, &cancelled_vectors, &cancelled_control,)
            .retrieve_semantic(&request)
            .expect("typed cancellation"),
        RetrieverOutcome::Cancelled
    );

    let expired_embedder = FakeQueryEmbedder::default();
    let expired_vectors = FakeVectorReadPort::new(&request, vec![row]);
    let expired_control = FixedExecutionControl {
        expire_after_elapsed_checks: Some(2),
        ..FixedExecutionControl::default()
    };
    assert!(matches!(
        SemanticCodeRetriever::new(&expired_embedder, &expired_vectors, &expired_control)
            .retrieve_semantic(&request)
            .expect("typed deadline"),
        RetrieverOutcome::BudgetExceeded(_)
    ));
}

#[test]
fn cancellation_and_deadline_are_checked_after_empty_excluded_scan() {
    let query_view = query_view();
    let projection = projection();
    let mut request = request(&query_view, &projection, 4);
    request.budget.deadline_micros = Some(5);

    let cancelled_embedder = FakeQueryEmbedder::default();
    let cancelled_control = FixedExecutionControl::default();
    let mut cancelled_vectors = FakeVectorReadPort::new(&request, Vec::new());
    cancelled_vectors.summary = Some(SemanticVectorScanSummaryV1 {
        examined: 1,
        eligible: 0,
        excluded: 1,
        unknown: 0,
    });
    cancelled_vectors.after_scan_cancel = Some(Rc::clone(&cancelled_control.cancelled));
    assert_eq!(
        SemanticCodeRetriever::new(&cancelled_embedder, &cancelled_vectors, &cancelled_control,)
            .retrieve_semantic(&request)
            .expect("typed cancellation"),
        RetrieverOutcome::Cancelled
    );

    let expired_embedder = FakeQueryEmbedder::default();
    let expired_control = FixedExecutionControl::default();
    let mut expired_vectors = FakeVectorReadPort::new(&request, Vec::new());
    expired_vectors.summary = Some(SemanticVectorScanSummaryV1 {
        examined: 1,
        eligible: 0,
        excluded: 1,
        unknown: 0,
    });
    expired_vectors.after_scan_elapsed = Some((Rc::clone(&expired_control.elapsed_micros), 5));
    assert!(matches!(
        SemanticCodeRetriever::new(&expired_embedder, &expired_vectors, &expired_control)
            .retrieve_semantic(&request)
            .expect("typed deadline"),
        RetrieverOutcome::BudgetExceeded(_)
    ));
}

#[test]
fn cancellation_and_deadline_are_rechecked_before_completion() {
    let query_view = query_view();
    let projection = projection();
    let mut request = request(&query_view, &projection, 4);
    request.budget.deadline_micros = Some(5);
    let row = record(&request, "one", vec![1.0, 0.0]);

    let cancelled_embedder = FakeQueryEmbedder::default();
    let cancelled_vectors = FakeVectorReadPort::new(&request, vec![row.clone()]);
    let cancelled_control = FixedExecutionControl {
        cancel_after_checks: Some(4),
        ..FixedExecutionControl::default()
    };
    assert_eq!(
        SemanticCodeRetriever::new(&cancelled_embedder, &cancelled_vectors, &cancelled_control,)
            .retrieve_semantic(&request)
            .expect("typed cancellation"),
        RetrieverOutcome::Cancelled
    );

    let expired_embedder = FakeQueryEmbedder::default();
    let expired_vectors = FakeVectorReadPort::new(&request, vec![row]);
    let expired_control = FixedExecutionControl {
        expire_after_elapsed_checks: Some(4),
        ..FixedExecutionControl::default()
    };
    assert!(matches!(
        SemanticCodeRetriever::new(&expired_embedder, &expired_vectors, &expired_control)
            .retrieve_semantic(&request)
            .expect("typed deadline"),
        RetrieverOutcome::BudgetExceeded(_)
    ));
}

fn fallback() -> Arc<QueryFallbackSubpayload> {
    let mut fallback = QueryFallbackSubpayload {
        profile_id: id("profile.query.fixture"),
        ordered_candidates: Vec::new(),
        public_fallback_lane_coverage: BTreeMap::from([
            (RetrieverKind::ExactLiteral, PublicRetrieverStatus::Complete),
            (RetrieverKind::Lexical, PublicRetrieverStatus::Complete),
            (RetrieverKind::Graph, PublicRetrieverStatus::Complete),
        ]),
        freshness: Vec::new(),
        cursor: None,
        digest: digest('0'),
    };
    fallback.digest = fallback.compute_digest().expect("fallback digest");
    Arc::new(fallback)
}

fn calibration(
    request: &SemanticRetrievalRequestV1<'_>,
    maximum_distance_micros: i64,
    minimum_margin_micros: u64,
) -> SemanticCalibrationProfileV1 {
    SemanticCalibrationProfileV1 {
        calibration_profile_id: id::<CalibrationProfileId>("calibration.semantic.fixture.v1"),
        cohort_digest: digest('7'),
        projection_key: request.projection.projection_key().clone(),
        vector_generation: request.vector_generation.clone(),
        capability_manifest_digest: request.capability_manifest_digest.clone(),
        maximum_distance_micros,
        minimum_margin_micros,
    }
}

fn complete_generation(request: &SemanticRetrievalRequestV1<'_>) -> CompleteSemanticGenerationV1 {
    CompleteSemanticGenerationV1::new(
        request.projection.projection_key().clone(),
        request.search_index_key.clone(),
        request.vector_generation.clone(),
        request.code_generation.clone(),
        request.capability_manifest_digest.clone(),
    )
    .expect("complete semantic generation")
}

#[test]
fn semantic_calibration_profile_is_canonical_and_threshold_bound() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 4);
    let calibration = calibration(&request, 2_000_000, 17);
    let encoded = serde_json::to_vec(&calibration).expect("serialize calibration");
    let decoded: SemanticCalibrationProfileV1 =
        serde_json::from_slice(&encoded).expect("deserialize calibration");

    assert_eq!(decoded, calibration);
    let accepted_digest = calibration
        .canonical_digest()
        .expect("canonical calibration digest");
    let mut changed_threshold = calibration.clone();
    changed_threshold.minimum_margin_micros += 1;
    assert_ne!(
        changed_threshold
            .canonical_digest()
            .expect("changed threshold digest"),
        accepted_digest
    );
    let mut changed_cohort = calibration;
    changed_cohort.cohort_digest = digest('8');
    assert_ne!(
        changed_cohort
            .canonical_digest()
            .expect("changed cohort digest"),
        accepted_digest
    );
}

fn shared_fusion_profile() -> FusionProfile {
    let lanes = [
        RetrieverKind::ExactLiteral,
        RetrieverKind::Lexical,
        RetrieverKind::Graph,
        RetrieverKind::Semantic,
    ];
    let calibrations = lanes
        .into_iter()
        .map(|lane| {
            (
                lane,
                id::<CalibrationProfileId>(&format!("calibration.{}.v1", lane.as_str())),
            )
        })
        .collect();
    let score_domain_calibrations = lanes
        .into_iter()
        .map(|lane| {
            let score_domain: ScoreDomainId = if lane == RetrieverKind::Semantic {
                id("score.semantic-distance.v1")
            } else {
                id(&format!("score.{}.v1", lane.as_str()))
            };
            (
                score_domain.clone(),
                ScoreDomainCalibrationV1 {
                    calibration_profile_id: id(&format!("calibration.{}.v1", lane.as_str())),
                    score_domain,
                    raw_min_micros: 0,
                    raw_max_micros: u64::MAX,
                },
            )
        })
        .collect();
    FusionProfile {
        profile_id: id("profile.semantic.v1"),
        evaluation_result_anchor: RetrievalAnchorId::new("evaluation.semantic.v1")
            .expect("evaluation anchor"),
        calibrations,
        score_domain_calibrations,
        weights_micros: lanes.into_iter().map(|lane| (lane, 1_000_000)).collect(),
        diversity_policy_id: id("diversity.semantic.v1"),
        rerank_policy_id: None,
        retrieval_budget: budget(16),
    }
}

fn no_diversity_caps() -> DiversityPolicy {
    DiversityPolicy {
        policy_id: id("diversity.semantic.v1"),
        evaluation_result_anchor: Some(
            RetrievalAnchorId::new("evaluation.semantic.v1").expect("evaluation anchor"),
        ),
        per_source_namespace: None,
        per_source_instance: None,
        per_repository: None,
        per_file: None,
        per_session_or_thread: None,
        per_copy_cluster: None,
        per_evidence_role: None,
    }
}

fn empty_shared_lane(lane: RetrieverKind) -> CompositionLaneInput {
    CompositionLaneInput::new(
        lane,
        RetrieverOutcome::Complete(RetrieverBatch::<()> {
            candidates: Vec::new(),
            evidence_by_occurrence: BTreeMap::new(),
            coverage: RetrieverCoverage::default(),
            continuation: None,
        }),
    )
    .expect("empty shared-kernel lane")
}

#[test]
fn calibrated_semantic_service_augments_without_mutating_fallback() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 4);
    let embedder = FakeQueryEmbedder::default();
    let vectors = FakeVectorReadPort::new(
        &request,
        vec![
            record(&request, "orthogonal", vec![0.0, 1.0]),
            record(&request, "identical", vec![1.0, 0.0]),
        ],
    );
    let control = FixedExecutionControl::default();
    let lane = SemanticCodeRetriever::new(&embedder, &vectors, &control);
    let fallback = fallback();
    let fallback_identity = Arc::as_ptr(&fallback);
    let generation = complete_generation(&request);
    let calibration = calibration(&request, 100_000_000, 100_000_000);

    let outcome = CalibratedSemanticQueryService::new(&lane)
        .execute(
            SemanticLaneReadinessV1::Ready {
                request: &request,
                generation: &generation,
                calibration: Some(&calibration),
            },
            SemanticQueryDecisionV1::EXECUTE_WITH_FALLBACK,
            Arc::clone(&fallback),
        )
        .expect("calibrated semantic query");

    let SemanticQueryServiceOutcomeV1::Augmented {
        semantic_lane,
        fallback,
        ..
    } = outcome
    else {
        panic!("a separated best match should be admitted");
    };
    let RetrieverOutcome::Complete(semantic) = semantic_lane.outcome else {
        panic!("semantic lane must enter the shared kernel as complete");
    };
    assert_eq!(semantic.candidates.len(), 2);
    assert_eq!(Arc::as_ptr(&fallback), fallback_identity);
    fallback.validate().expect("fallback remains byte-valid");
}

#[test]
fn admitted_semantic_lane_uses_shared_fusion_cursor_and_hydration_stages() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 4);
    let embedder = FakeQueryEmbedder::default();
    let vectors = FakeVectorReadPort::new(
        &request,
        vec![
            record(&request, "orthogonal", vec![0.0, 1.0]),
            record(&request, "identical", vec![1.0, 0.0]),
        ],
    );
    let control = FixedExecutionControl::default();
    let lane = SemanticCodeRetriever::new(&embedder, &vectors, &control);
    let generation = complete_generation(&request);
    let calibration = calibration(&request, 2_000_000_000, 0);
    let outcome = CalibratedSemanticQueryService::new(&lane)
        .execute(
            SemanticLaneReadinessV1::Ready {
                request: &request,
                generation: &generation,
                calibration: Some(&calibration),
            },
            SemanticQueryDecisionV1::EXECUTE_WITH_FALLBACK,
            fallback(),
        )
        .expect("semantic lane admission");
    let SemanticQueryServiceOutcomeV1::Augmented { semantic_lane, .. } = outcome else {
        panic!("complete calibrated semantic generation must be admitted");
    };

    let mut lanes = vec![
        semantic_lane,
        empty_shared_lane(RetrieverKind::Graph),
        empty_shared_lane(RetrieverKind::ExactLiteral),
        empty_shared_lane(RetrieverKind::Lexical),
    ];
    let kernel = CompositionKernel::new(id("ranking.semantic.v1"));
    let first = kernel
        .compose(
            &FusionStageInput {
                profile: shared_fusion_profile(),
                lanes: lanes.clone(),
            },
            &no_diversity_caps(),
        )
        .expect("shared kernel composes semantic candidates");
    lanes.reverse();
    let second = kernel
        .compose(
            &FusionStageInput {
                profile: shared_fusion_profile(),
                lanes,
            },
            &no_diversity_caps(),
        )
        .expect("shared kernel is independent of lane completion order");

    assert_eq!(first, second);
    assert_eq!(first.ranked_candidates.len(), 2);
    assert!(
        first
            .ranked_candidates
            .iter()
            .all(|candidate| candidate.candidate.exact_class == ExactClass::Approximate)
    );

    let keyring = RetrievalCursorKeyringV1::new(
        request.base.scope.privacy_domain.clone(),
        id::<RetrievalCursorKeyId>("semantic-cursor-key.v1"),
        7,
        vec![7_u8; 32],
        100,
    )
    .expect("cursor keyring");
    let left_page = kernel
        .paginate_at(
            &request.base,
            request.query_view,
            &keyring,
            &first,
            1,
            None,
            UtcMicros(10),
        )
        .expect("first deterministic page");
    let right_page = kernel
        .paginate_at(
            &request.base,
            request.query_view,
            &keyring,
            &second,
            1,
            None,
            UtcMicros(10),
        )
        .expect("second deterministic page");
    assert_eq!(left_page, right_page);
    assert!(left_page.cursor.is_some());

    let mut hydration_budget = request.budget;
    hydration_budget.max_hydrated_results = 1;
    let mut source = DenyingHydrationSource::default();
    let hydrated = CanonicalLateHydration::new(&mut source)
        .hydrate(&request.base, &first.ranked_candidates, &hydration_budget)
        .expect("denial is a positional hydration result");
    assert_eq!(hydrated.results.len(), 1);
    assert_eq!(source.authorization_checks, 1);
    assert_eq!(source.payload_reads, 0);
}

#[test]
fn missing_or_shifted_calibration_abstains_and_preserves_exact_fallback() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 4);
    let embedder = FakeQueryEmbedder::default();
    let vectors = FakeVectorReadPort::new(&request, vec![record(&request, "one", vec![1.0, 0.0])]);
    let control = FixedExecutionControl::default();
    let lane = SemanticCodeRetriever::new(&embedder, &vectors, &control);
    let fallback = fallback();
    let fallback_identity = Arc::as_ptr(&fallback);
    let generation = complete_generation(&request);

    let permissive = CalibratedSemanticQueryService::new(&lane)
        .execute(
            SemanticLaneReadinessV1::Ready {
                request: &request,
                generation: &generation,
                calibration: None,
            },
            SemanticQueryDecisionV1::EXECUTE_WITH_FALLBACK,
            Arc::clone(&fallback),
        )
        .expect("missing calibration uses the declared fallback");
    assert!(matches!(
        &permissive,
        SemanticQueryServiceOutcomeV1::Fallback {
            abstention: SemanticAbstentionV1::CalibrationUnavailable,
            ..
        }
    ));
    assert_eq!(
        Arc::as_ptr(permissive.fallback()),
        fallback_identity,
        "fallback is the exact same owned payload"
    );
    assert_eq!(embedder.calls.get(), 0);
    assert_eq!(vectors.scans.get(), 0);

    let mut shifted = calibration(&request, 100_000_000, 0);
    shifted.vector_generation = VectorGenerationIdV1::new(digest('6'));
    assert!(matches!(
        CalibratedSemanticQueryService::new(&lane).execute(
            SemanticLaneReadinessV1::Ready {
                request: &request,
                generation: &generation,
                calibration: Some(&shifted),
            },
            SemanticQueryDecisionV1::EXECUTE_STRICT,
            fallback,
        ),
        Err(SemanticQueryServiceError::StrictUnavailable(
            SemanticAbstentionV1::CalibrationShifted
        ))
    ));
}

#[test]
fn calibrated_distance_and_margin_thresholds_abstain_without_relabeling_scores() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 4);
    let embedder = FakeQueryEmbedder::default();
    let vectors = FakeVectorReadPort::new(
        &request,
        vec![
            record(&request, "left", vec![0.0, 1.0]),
            record(&request, "right", vec![0.0, -1.0]),
        ],
    );
    let control = FixedExecutionControl::default();
    let lane = SemanticCodeRetriever::new(&embedder, &vectors, &control);
    let generation = complete_generation(&request);
    let calibration = calibration(&request, 2_000_000_000, 1);

    let outcome = CalibratedSemanticQueryService::new(&lane)
        .execute(
            SemanticLaneReadinessV1::Ready {
                request: &request,
                generation: &generation,
                calibration: Some(&calibration),
            },
            SemanticQueryDecisionV1::EXECUTE_WITH_FALLBACK,
            fallback(),
        )
        .expect("ambiguous semantic result falls back");

    assert!(matches!(
        outcome,
        SemanticQueryServiceOutcomeV1::Fallback {
            abstention: SemanticAbstentionV1::AmbiguousTopCandidates,
            ..
        }
    ));
}

#[test]
fn every_non_ready_state_bypasses_semantic_authorities_and_preserves_query_bytes() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 4);
    let embedder = FakeQueryEmbedder::default();
    let vectors =
        FakeVectorReadPort::new(&request, vec![record(&request, "ignored", vec![1.0, 0.0])]);
    let control = FixedExecutionControl::default();
    let lane = SemanticCodeRetriever::new(&embedder, &vectors, &control);
    let fallback = fallback();
    let fallback_bytes = serde_json::to_vec(fallback.as_ref()).expect("serialize fallback");

    for (state, expected) in [
        (
            SemanticIndexStateV1::Unavailable,
            SemanticAbstentionV1::IndexUnavailable,
        ),
        (
            SemanticIndexStateV1::Indexing,
            SemanticAbstentionV1::Indexing,
        ),
        (
            SemanticIndexStateV1::Degraded,
            SemanticAbstentionV1::IndexDegraded,
        ),
        (
            SemanticIndexStateV1::Failed,
            SemanticAbstentionV1::IndexFailed,
        ),
        (
            SemanticIndexStateV1::Stale,
            SemanticAbstentionV1::IndexStale,
        ),
        (
            SemanticIndexStateV1::Incompatible,
            SemanticAbstentionV1::IndexIncompatible,
        ),
    ] {
        let outcome = CalibratedSemanticQueryService::new(&lane)
            .execute(
                SemanticLaneReadinessV1::Unavailable(state),
                SemanticQueryDecisionV1::UseFallback,
                Arc::clone(&fallback),
            )
            .expect("ordinary search bypasses a non-ready semantic lane");
        let SemanticQueryServiceOutcomeV1::Fallback { abstention, .. } = &outcome else {
            panic!("non-ready semantic state must preserve the query fallback");
        };
        assert_eq!(abstention, &expected);
        assert_eq!(
            serde_json::to_vec(outcome.fallback().as_ref()).expect("serialize returned fallback"),
            fallback_bytes
        );
        assert!(matches!(
            CalibratedSemanticQueryService::new(&lane).execute(
                SemanticLaneReadinessV1::Unavailable(state),
                SemanticQueryDecisionV1::RejectUnavailable,
                Arc::clone(&fallback),
            ),
            Err(SemanticQueryServiceError::StrictUnavailable(ref reason)) if reason == &expected
        ));
    }
    assert_eq!(embedder.calls.get(), 0);
    assert_eq!(vectors.scans.get(), 0);
}

#[test]
fn mismatched_complete_generation_bypasses_semantic_authorities() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 4);
    let embedder = FakeQueryEmbedder::default();
    let vectors =
        FakeVectorReadPort::new(&request, vec![record(&request, "ignored", vec![1.0, 0.0])]);
    let control = FixedExecutionControl::default();
    let lane = SemanticCodeRetriever::new(&embedder, &vectors, &control);
    let mut shifted_search_index = request.search_index_key.clone();
    shifted_search_index.profile_digest = digest('6');
    let generation = CompleteSemanticGenerationV1::new(
        request.projection.projection_key().clone(),
        shifted_search_index,
        request.vector_generation.clone(),
        request.code_generation.clone(),
        request.capability_manifest_digest.clone(),
    )
    .expect("well-formed but incompatible generation");
    let calibration = calibration(&request, 100_000_000, 0);

    let outcome = CalibratedSemanticQueryService::new(&lane)
        .execute(
            SemanticLaneReadinessV1::Ready {
                request: &request,
                generation: &generation,
                calibration: Some(&calibration),
            },
            SemanticQueryDecisionV1::EXECUTE_WITH_FALLBACK,
            fallback(),
        )
        .expect("ordinary search bypasses a shifted complete generation");

    assert!(matches!(
        outcome,
        SemanticQueryServiceOutcomeV1::Fallback {
            abstention: SemanticAbstentionV1::IndexIncompatible,
            ..
        }
    ));
    assert_eq!(embedder.calls.get(), 0);
    assert_eq!(vectors.scans.get(), 0);
}

#[test]
fn partial_cancelled_and_failed_semantic_attempts_preserve_query_bytes() {
    let query_view = query_view();
    let projection = projection();
    let request = request(&query_view, &projection, 4);
    let embedder = FakeQueryEmbedder::default();
    let vectors = FakeVectorReadPort::new(&request, vec![record(&request, "one", vec![1.0, 0.0])]);
    let control = FixedExecutionControl::default();
    let complete = SemanticCodeRetriever::new(&embedder, &vectors, &control)
        .retrieve_semantic(&request)
        .expect("fixture semantic retrieval");
    let RetrieverOutcome::Complete(batch) = complete else {
        panic!("fixture semantic retrieval must complete");
    };
    let generation = complete_generation(&request);
    let calibration = calibration(&request, 100_000_000, 0);
    let fallback = fallback();
    let fallback_bytes = serde_json::to_vec(fallback.as_ref()).expect("serialize fallback");

    let cases = [
        (
            Ok(RetrieverOutcome::Partial {
                value: batch,
                reason: tracedecay_domain::RetrievalFailure::AuthorityUnavailable {
                    detail: "partial vector publication".to_owned(),
                },
            }),
            SemanticAbstentionV1::PartialCoverage,
        ),
        (
            Ok(RetrieverOutcome::Cancelled),
            SemanticAbstentionV1::Cancelled,
        ),
        (
            Err(RetrievalPortError::AuthorityUnavailable(
                "fastembed out of memory".to_owned(),
            )),
            SemanticAbstentionV1::LaneFailure,
        ),
    ];

    for (lane_outcome, expected) in cases {
        let lane = FixedSemanticLane {
            calls: Cell::new(0),
            outcome: lane_outcome,
        };
        let outcome = CalibratedSemanticQueryService::new(&lane)
            .execute(
                SemanticLaneReadinessV1::Ready {
                    request: &request,
                    generation: &generation,
                    calibration: Some(&calibration),
                },
                SemanticQueryDecisionV1::EXECUTE_WITH_FALLBACK,
                Arc::clone(&fallback),
            )
            .expect("ordinary search falls back on any incomplete semantic attempt");
        let SemanticQueryServiceOutcomeV1::Fallback { abstention, .. } = &outcome else {
            panic!("incomplete semantic attempts must never enter ranking");
        };
        assert_eq!(abstention, &expected);
        assert_eq!(lane.calls.get(), 1);
        assert_eq!(
            serde_json::to_vec(outcome.fallback().as_ref()).expect("serialize returned fallback"),
            fallback_bytes
        );
    }
}
