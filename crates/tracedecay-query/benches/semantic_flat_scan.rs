//! Latency of the exact-flat semantic lane at serving-cap scale.
//!
//! The production semantic read port materializes every published vector row
//! in memory (bounded by the resident-row cap) and the lane scores each row
//! per query. This target prices that scan with the real lane code —
//! `SemanticCodeRetriever` over an in-memory port shaped like
//! `PublishedSemanticVectorReadPortV1` — so it can be compared against the
//! graph store's HNSW `vector_search` bench over the same row counts,
//! dimensions, metric, and result limit.
//!
//! This target intentionally owns its fixture because query's production
//! fixture helpers are test-private.

use std::fmt;
use std::sync::OnceLock;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, ChunkerRevision, CodeSearchChunkId, CompactCandidate,
    ComponentRevision, EmbeddingDeviceClassV1, EmbeddingDocumentCompositionV1, EmbeddingMetricV1,
    EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingProjectionKeyV1,
    EmbeddingTruncationSideV1, EvidenceRole, FreshnessCompatibilityV1, LogicalEvidenceId,
    PrincipalId, QueryDigest, QueryMac, QueryNormalizationRevision, RetrievalAnchorId,
    RetrievalBudget, RetrievalRequest, RetrievalScope, RetrievalSnapshot, RetrieverKind,
    RetrieverOutcome, SanitizerRevision, ScoreDomainId, SemanticSearchIndexKeyV1,
    SemanticSearchIndexProfileV1, SingleRootScopeV1, SourceFreshness, SourceNamespace,
    SourceOccurrenceId, TemporalModeV1, UtcMicros, VectorGenerationIdV1, VectorWatermark,
};
use tracedecay_query::retrieval::ports::{
    CodeCandidateBindingV1, CodeOccurrenceRefV1, RetrievalPortError,
};
use tracedecay_query::retrieval::semantic::{
    EphemeralQueryEmbeddingV1, SemanticCodeRetriever, SemanticExecutionControl,
    SemanticLaneRetriever, SemanticQueryEmbeddingPort, SemanticQueryEmbeddingRequestV1,
    SemanticRetrievalRequestV1, SemanticVectorReadPort, SemanticVectorReadRequestV1,
    SemanticVectorRecordV1, SemanticVectorScanSummaryV1,
};

// Row counts stop at the production resident-row cap
// (`MAX_RESIDENT_VECTOR_ROWS`): the lane never flat-scans more rows than the
// port may hold. Sixteen dimensions mirror the graph store's `vector_search`
// bench so the two targets price the same workload; 384 dimensions price a
// production-shaped embedding profile.
const ROW_COUNT: usize = 100_000;
const DIMENSION_CASES: [usize; 2] = [16, 384];
const RESULT_LIMIT: u32 = 20;

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

fn projection(dimensions: u32) -> AdmittedEmbeddingProjectionKeyV1 {
    EmbeddingProjectionKeyV1 {
        model_artifact_digest: digest('a'),
        tokenizer_digest: digest('b'),
        config_digest: digest('c'),
        query_instruction_digest: Some(digest('d')),
        document_instruction_digest: Some(digest('e')),
        document_composition: EmbeddingDocumentCompositionV1::SanitizedText,
        pooling: EmbeddingPoolingV1::Mean,
        truncation_side: EmbeddingTruncationSideV1::Right,
        truncation_length: 128,
        inference_batch_size: 8,
        inference_batch_bytes: 4 * 1024,
        runtime_backend: "onnx.cpu".to_owned(),
        runtime_build_revision: "runtime.v1".to_owned(),
        device_class: EmbeddingDeviceClassV1::Cpu,
        dimensions,
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

fn budget() -> RetrievalBudget {
    RetrievalBudget {
        max_candidates_per_lane: RESULT_LIMIT,
        max_fused_candidates: RESULT_LIMIT,
        max_hydrated_results: 8,
        max_hydration_bytes: 65_536,
        deadline_micros: None,
    }
}

fn query_digest() -> QueryDigest {
    QueryDigest::new(
        id("privacy.fixture"),
        7,
        QueryMac::new(format!("hmac-sha256:{}", "1".repeat(64))).expect("valid query MAC"),
    )
}

fn request<'a>(
    query_view: &'a tracedecay_domain::EphemeralSanitizedQueryViewV1,
    projection: &'a AdmittedEmbeddingProjectionKeyV1,
) -> SemanticRetrievalRequestV1<'a> {
    SemanticRetrievalRequestV1 {
        base: RetrievalRequest {
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
            budget: budget(),
        },
        query_digest: query_digest(),
        query_view,
        projection,
        search_index_key: search_index_key(),
        capability_manifest_digest: digest('9'),
        vector_generation: VectorGenerationIdV1::new(digest('8')),
        code_generation: id("generation.1"),
        budget: budget(),
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

/// SplitMix64 keeps the fixture reproducible without collapsing it into a
/// small set of repeated vectors; identical to the graph store's
/// `vector_search` bench generator so both targets score the same data.
fn deterministic_vector(seed: usize, dimension: usize) -> Vec<f32> {
    let mut state = (seed as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut values = (0..dimension)
        .map(|_| {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut mixed = state;
            mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            mixed ^= mixed >> 31;
            ((mixed >> 40) as f32 + 1.0) / 16_777_217.0
        })
        .collect::<Vec<_>>();
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    for value in &mut values {
        *value /= norm;
    }
    values
}

fn record(
    request: &SemanticRetrievalRequestV1<'_>,
    ordinal: usize,
    values: Vec<f32>,
) -> SemanticVectorRecordV1 {
    let name = format!("{ordinal:07}");
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

struct FixedQueryEmbedder {
    values: Vec<f32>,
}

impl SemanticQueryEmbeddingPort for FixedQueryEmbedder {
    fn embed_query(
        &self,
        request: SemanticQueryEmbeddingRequestV1<'_>,
    ) -> Result<EphemeralQueryEmbeddingV1, RetrievalPortError> {
        EphemeralQueryEmbeddingV1::new(
            request.query_digest.clone(),
            request.projection.clone(),
            self.values.clone(),
        )
    }
}

/// In-memory rows shaped like the production published read port: the flat
/// scan visits every resident row exactly once per query.
struct ResidentRowsPort {
    rows: Vec<SemanticVectorRecordV1>,
}

impl SemanticVectorReadPort for ResidentRowsPort {
    fn scan_exact_flat(
        &self,
        _request: SemanticVectorReadRequestV1<'_>,
        examine: &mut dyn FnMut() -> Result<(), RetrievalPortError>,
        visit: &mut dyn FnMut(&SemanticVectorRecordV1) -> Result<(), RetrievalPortError>,
    ) -> Result<SemanticVectorScanSummaryV1, RetrievalPortError> {
        for row in &self.rows {
            examine()?;
            visit(row)?;
        }
        Ok(SemanticVectorScanSummaryV1 {
            examined: self.rows.len() as u64,
            eligible: self.rows.len() as u64,
            excluded: 0,
            unknown: 0,
        })
    }
}

struct NeverInterrupted;

impl SemanticExecutionControl for NeverInterrupted {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn elapsed_micros(&self) -> u64 {
        0
    }
}

fn semantic_flat_scan(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("semantic_flat_scan/exact_flat_lane");
    for dimension in DIMENSION_CASES {
        let projection = projection(dimension as u32);
        let query_view = tracedecay_domain::EphemeralSanitizedQueryViewV1::sanitize(
            "semantic query",
            id::<SanitizerRevision>("sanitizer.v1"),
            id::<QueryNormalizationRevision>("normalizer.v1"),
        )
        .expect("valid ephemeral query");
        let request = request(&query_view, &projection);
        let rows = (0..ROW_COUNT)
            .map(|ordinal| record(&request, ordinal, deterministic_vector(ordinal, dimension)))
            .collect::<Vec<_>>();
        let embedder = FixedQueryEmbedder {
            values: deterministic_vector(ROW_COUNT - 1, dimension),
        };
        let vectors = ResidentRowsPort { rows };
        let control = NeverInterrupted;
        let retriever = SemanticCodeRetriever::new(&embedder, &vectors, &control);

        let expected = format!("occurrence.{:07}", ROW_COUNT - 1);
        let preflight = retriever
            .retrieve_semantic(&request)
            .expect("benchmark flat scan preflight succeeds");
        let RetrieverOutcome::Complete(batch) = preflight else {
            panic!("benchmark flat scan preflight must complete");
        };
        assert_eq!(
            batch
                .candidates
                .first()
                .expect("benchmark flat scan returns candidates")
                .source_occurrence_id
                .as_str(),
            expected.as_str(),
            "benchmark query must retrieve its exact source vector first",
        );

        group.bench_with_input(
            BenchmarkId::new(format!("cosine_{dimension}d_top_{RESULT_LIMIT}"), ROW_COUNT),
            &ROW_COUNT,
            |bencher, _| {
                bencher.iter(|| {
                    black_box(
                        retriever
                            .retrieve_semantic(&request)
                            .expect("benchmark flat scan succeeds"),
                    )
                });
            },
        );
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(Duration::from_secs(20));
    targets = semantic_flat_scan
}
criterion_main!(benches);
