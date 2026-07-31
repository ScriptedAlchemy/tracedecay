use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use tracedecay_domain::{
    CalibrationProfileId, ChunkerRevision, CodeGenerationId, CodeSearchChunkId, CompactCandidate,
    ComponentRevision, DiversityPolicy, EdgeAuthorityV1, EmbeddingDeviceClassV1, EmbeddingMetricV1,
    EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingProjectionKeyV1,
    EmbeddingTruncationSideV1, EvidenceRole, ExactAdmissionProof, ExactAdmissionRuleRevision,
    ExactFieldV1, ExactTechnicalTermKindV1, FileOccurrenceId, FixedPointScore,
    FreshnessCompatibilityV1, FusionProfile, ManifestDigest, PrincipalId, RelationEdgeKindV1,
    RetrievalAnchorId, RetrievalBudget, RetrievalBudgetUsage, RetrievalCursorKeyId,
    RetrievalRequest, RetrievalScope, RetrievalSnapshot, RetrieverBatch, RetrieverCoverage,
    RetrieverKind, RetrieverOutcome, ScoreDomainCalibrationV1, ScoreDomainId,
    SemanticSearchIndexProfileV1, SingleRootScopeV1, SourceFreshness, SourceOccurrenceId,
    SourceSpan, SymbolOccurrenceId, TemporalModeV1, UtcMicros, VectorGenerationIdV1,
    VectorWatermark,
};
use tracedecay_query::retrieval::exact::{ExactLaneEvidence, ExactLiteralV1};
use tracedecay_query::retrieval::fusion::RetrievalCursorKeyringV1;
use tracedecay_query::retrieval::graph::{GraphLaneEvidence, GraphPathSegmentV1};
use tracedecay_query::retrieval::lexical::{LexicalFieldV1, LexicalLaneEvidence};
use tracedecay_query::retrieval::ports::{CodeCandidateBindingV1, CodeOccurrenceRefV1};
use tracedecay_query::retrieval::semantic::{
    CanonicalSemanticDistanceV1, CodeSemanticEvidenceV1, SemanticSearchKindV1,
};
use tracedecay_query::retrieval::{
    AdmittedGenerationContextV1, NativeCodeOccurrenceV1, NativeExactRecordV1, NativeGraphRecordV1,
    NativeLaneOutcomeV1, NativeLanePageV1, NativeLexicalRecordV1, NativeRecordReadPortV1,
    NativeSemanticRecordV1, NativeSymbolRecordV1, PR9_RANKING_REVISION_V1, Pr9QueryAuthorityV1,
    PreparedQueryBindingsV1, PreparedQueryV1, inspect_prepared_query_cursor,
};

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

fn generation() -> CodeGenerationId {
    id("generation.canonical-equivalence")
}

fn freshness() -> SourceFreshness {
    SourceFreshness {
        source_namespace: id("namespace.canonical-equivalence"),
        source_instance: id("instance.canonical-equivalence"),
        source_watermark: Some(41),
        projection_watermark: Some(40),
        observed_at: UtcMicros(1_000),
        source_generation: Some(9),
        generation_lag: Some(1),
        compatibility: FreshnessCompatibilityV1::Current,
        policy_revision: id("freshness-policy.canonical-equivalence.v1"),
    }
}

fn coverage() -> RetrieverCoverage {
    RetrieverCoverage {
        examined: 1,
        eligible: 1,
        excluded: 0,
        capped: 0,
        unknown: 0,
    }
}

fn binding() -> CodeCandidateBindingV1 {
    CodeCandidateBindingV1 {
        candidate_anchor: id("anchor.canonical-equivalence"),
        occurrence: CodeOccurrenceRefV1 {
            generation: generation(),
            file: id("file.canonical-equivalence"),
            symbol: Some(id("symbol.canonical-equivalence")),
            chunk: Some(id("chunk.canonical-equivalence")),
        },
        language_descriptor_revision: id("language.rust.canonical-equivalence.v1"),
        matched_term_kinds: vec![ExactTechnicalTermKindV1::WholeSymbol],
        source_occurrence: id("source.canonical-equivalence"),
    }
}

fn candidate(kind: RetrieverKind, raw_score: u64) -> CompactCandidate {
    CompactCandidate {
        anchor_id: id("anchor.canonical-equivalence"),
        logical_evidence_id: id("logical.canonical-equivalence"),
        source_occurrence_id: id("source.canonical-equivalence"),
        file_occurrence_id: Some(id("file.canonical-equivalence")),
        source_namespace: id("namespace.canonical-equivalence"),
        repository_id: Some(id("repository.canonical-equivalence")),
        session_or_thread_id: None,
        logical_copy_cluster_id: None,
        logical_copy_evidence_anchor: None,
        evidence_role: EvidenceRole::Primary,
        retriever: kind,
        retriever_revision: id(&format!(
            "retriever.{}.canonical-equivalence.v1",
            kind.as_str()
        )),
        score_domain: id(&format!("score.{}.canonical-equivalence.v1", kind.as_str())),
        raw_score: FixedPointScore(raw_score),
        ordinal_rank: 0,
        exact_admission_proof: None,
        retriever_evidence_anchor: id("evidence.canonical-equivalence"),
        freshness: freshness(),
    }
}

fn batch<E>(candidate: CompactCandidate, evidence: E) -> RetrieverBatch<E> {
    RetrieverBatch {
        candidates: vec![candidate],
        evidence_by_occurrence: BTreeMap::from([(
            id::<SourceOccurrenceId>("source.canonical-equivalence"),
            evidence,
        )]),
        coverage: coverage(),
        continuation: None,
    }
}

fn exact_proof() -> ExactAdmissionProof {
    ExactAdmissionProof {
        rule_revision: id::<ExactAdmissionRuleRevision>("exact-rules.canonical-equivalence.v1"),
        field: ExactFieldV1::Identifier,
        original_bytes: b"run_query".to_vec(),
        canonical_bytes: b"run_query".to_vec(),
        normalization_steps: Vec::new(),
        scope_digest: digest('a'),
        authorization_revision: id("authorization.canonical-equivalence.v1"),
        snapshot_digest: digest('b'),
    }
}

fn exact_evidence() -> ExactLaneEvidence {
    ExactLaneEvidence {
        binding: binding(),
        matched_literals: vec![ExactLiteralV1 {
            field: ExactFieldV1::Identifier,
            original_bytes: b"run_query".to_vec(),
            canonical_bytes: b"run_query".to_vec(),
        }],
        admission_proof: exact_proof(),
    }
}

fn lexical_evidence() -> LexicalLaneEvidence {
    LexicalLaneEvidence {
        binding: binding(),
        field_scores_micros: vec![(LexicalFieldV1::BodyText, 875_000)],
        matched_whole_terms: vec!["canonical".to_owned(), "execution".to_owned()],
        matched_subtokens: vec!["query".to_owned()],
        matched_phrases: vec!["canonical execution".to_owned()],
        typo_recovery_applied: false,
        echo_penalty_applied: false,
    }
}

fn graph_evidence() -> GraphLaneEvidence {
    GraphLaneEvidence {
        binding: binding(),
        path: vec![GraphPathSegmentV1 {
            from: id("symbol.caller"),
            to: id("symbol.canonical-equivalence"),
            edge_kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 3,
                end_byte: 17,
            },
        }],
        weakest_authority: EdgeAuthorityV1::SyntaxExact,
    }
}

fn semantic_evidence() -> CodeSemanticEvidenceV1 {
    let projection = EmbeddingProjectionKeyV1 {
        model_artifact_digest: digest('1'),
        tokenizer_digest: digest('2'),
        config_digest: digest('3'),
        query_instruction_digest: Some(digest('4')),
        document_instruction_digest: Some(digest('5')),
        pooling: EmbeddingPoolingV1::Mean,
        truncation_side: EmbeddingTruncationSideV1::Right,
        truncation_length: 128,
        runtime_backend: "onnx.cpu".to_owned(),
        runtime_build_revision: "runtime.canonical-equivalence.v1".to_owned(),
        device_class: EmbeddingDeviceClassV1::Cpu,
        dimensions: 2,
        metric: EmbeddingMetricV1::Cosine,
        normalization: EmbeddingNormalizationV1::L2,
        precision: EmbeddingPrecisionV1::Fp32,
        chunk_schema_revision: "chunk.canonical-equivalence.v1".to_owned(),
        chunker_revision: id::<ChunkerRevision>("chunker.canonical-equivalence.v1"),
        privacy_domain: id("privacy.canonical-equivalence"),
        privacy_key_epoch: 7,
    }
    .admit()
    .expect("valid semantic projection");
    let distance: CanonicalSemanticDistanceV1 =
        serde_json::from_str("125000000").expect("public canonical distance wire");

    CodeSemanticEvidenceV1 {
        projection_key: projection.embedding_key().clone(),
        search_index_key: SemanticSearchIndexProfileV1::exact_flat_v1()
            .and_then(|profile| profile.index_key())
            .expect("exact-flat semantic index"),
        vector_generation: VectorGenerationIdV1::new(digest('6')),
        chunk_id: id("chunk.canonical-equivalence"),
        distance,
        search_kind: SemanticSearchKindV1::ExactFlat,
    }
}

struct FixtureRecords {
    generation: CodeGenerationId,
}

impl FixtureRecords {
    fn occurrence() -> NativeCodeOccurrenceV1 {
        NativeCodeOccurrenceV1 {
            file: id("file.canonical-equivalence"),
            symbol: Some(id("symbol.canonical-equivalence")),
            chunk: Some(id("chunk.canonical-equivalence")),
            path: "src/query/canonical.rs".to_owned(),
            span: SourceSpan {
                start_byte: 21,
                end_byte: 89,
            },
        }
    }

    fn symbol(symbol: &SymbolOccurrenceId) -> NativeSymbolRecordV1 {
        NativeSymbolRecordV1 {
            occurrence: symbol.clone(),
            name: "run_query".to_owned(),
            qualified_name: "query::canonical::run_query".to_owned(),
            kind: "function".to_owned(),
            path: "src/query/canonical.rs".to_owned(),
            span: SourceSpan {
                start_byte: 21,
                end_byte: 89,
            },
            signature: Some("pub async fn run_query()".to_owned()),
            is_async: true,
        }
    }
}

impl NativeRecordReadPortV1 for FixtureRecords {
    fn generation(&self) -> &CodeGenerationId {
        &self.generation
    }

    fn occurrence(
        &self,
        _: &CodeCandidateBindingV1,
    ) -> Result<NativeCodeOccurrenceV1, tracedecay_query::retrieval::QueryExecutionContractErrorV1>
    {
        Ok(Self::occurrence())
    }

    fn occurrence_by_chunk(
        &self,
        _: &CodeSearchChunkId,
    ) -> Result<NativeCodeOccurrenceV1, tracedecay_query::retrieval::QueryExecutionContractErrorV1>
    {
        Ok(Self::occurrence())
    }

    fn symbol(
        &self,
        symbol: &SymbolOccurrenceId,
        _: &FileOccurrenceId,
    ) -> Result<NativeSymbolRecordV1, tracedecay_query::retrieval::QueryExecutionContractErrorV1>
    {
        Ok(Self::symbol(symbol))
    }
}

fn context(records: &FixtureRecords) -> AdmittedGenerationContextV1<'_, FixtureRecords> {
    AdmittedGenerationContextV1::admit(records.generation.clone(), records)
        .expect("matching admitted generation")
}

#[test]
fn public_execution_translates_each_canonical_lane_without_field_loss() {
    let records = FixtureRecords {
        generation: generation(),
    };
    let context = context(&records);

    let mut exact_candidate = candidate(RetrieverKind::ExactLiteral, 1_000_000);
    exact_candidate.exact_admission_proof = Some(exact_proof());
    let exact = context
        .exact(
            RetrieverOutcome::Complete(batch(exact_candidate, exact_evidence())),
            "run_query",
            Some(ExactTechnicalTermKindV1::WholeSymbol),
            |path| path.starts_with("src/query/"),
        )
        .expect("exact translation");
    assert_eq!(
        exact,
        NativeLaneOutcomeV1::Complete(NativeLanePageV1 {
            generation: generation(),
            items: vec![NativeExactRecordV1 {
                occurrence: FixtureRecords::occurrence(),
                matched_kind: ExactTechnicalTermKindV1::WholeSymbol,
                matched_literal: "run_query".to_owned(),
            }],
            total_eligible: 1,
            coverage: coverage(),
        })
    );

    let lexical = context
        .lexical(
            RetrieverOutcome::Complete(batch(
                candidate(RetrieverKind::Lexical, 875_000),
                lexical_evidence(),
            )),
            |path| path.starts_with("src/query/"),
        )
        .expect("lexical translation");
    assert_eq!(
        lexical,
        NativeLaneOutcomeV1::Complete(NativeLanePageV1 {
            generation: generation(),
            items: vec![NativeLexicalRecordV1 {
                occurrence: FixtureRecords::occurrence(),
                score_micros: 875_000,
                matched_phrases: vec!["canonical execution".to_owned()],
                matched_terms: vec![
                    "canonical".to_owned(),
                    "execution".to_owned(),
                    "query".to_owned(),
                ],
            }],
            total_eligible: 1,
            coverage: coverage(),
        })
    );

    let graph = context
        .graph(
            RetrieverOutcome::Complete(batch(
                candidate(RetrieverKind::Graph, 750_000),
                graph_evidence(),
            )),
            |path| path.starts_with("src/query/"),
        )
        .expect("graph translation");
    assert_eq!(
        graph,
        NativeLaneOutcomeV1::Complete(NativeLanePageV1 {
            generation: generation(),
            items: vec![NativeGraphRecordV1 {
                symbol: FixtureRecords::symbol(&id("symbol.canonical-equivalence")),
                edge_kind: Some(RelationEdgeKindV1::Calls),
                depth: 1,
            }],
            total_eligible: 1,
            coverage: coverage(),
        })
    );

    let semantic = context
        .semantic(
            RetrieverOutcome::Complete(batch(
                candidate(RetrieverKind::Semantic, 700_000),
                semantic_evidence(),
            )),
            |path| path.starts_with("src/query/"),
        )
        .expect("semantic translation");
    assert_eq!(
        semantic,
        NativeLaneOutcomeV1::Complete(NativeLanePageV1 {
            generation: generation(),
            items: vec![NativeSemanticRecordV1 {
                occurrence: FixtureRecords::occurrence(),
                distance_micros: 125_000_000,
                score: FixedPointScore(700_000),
            }],
            total_eligible: 1,
            coverage: coverage(),
        })
    );
}

#[test]
fn public_execution_preserves_denied_stale_cancelled_and_budget_outcomes() {
    let records = FixtureRecords {
        generation: generation(),
    };
    let context = context(&records);
    let stale = freshness();
    let budget = RetrievalBudgetUsage {
        candidates_examined: 13,
        candidates_returned: 5,
        hydrated_results: 2,
        hydration_bytes: 4_096,
        elapsed_micros: 900,
    };

    assert_eq!(
        context
            .lexical(RetrieverOutcome::Denied, |_| true)
            .expect("denied decision"),
        NativeLaneOutcomeV1::Denied
    );
    assert_eq!(
        context
            .exact(
                RetrieverOutcome::Stale(stale.clone()),
                "run_query",
                None,
                |_| true,
            )
            .expect("stale decision"),
        NativeLaneOutcomeV1::Stale(stale)
    );
    assert_eq!(
        context
            .graph(RetrieverOutcome::BudgetExceeded(budget), |_| true)
            .expect("budget decision"),
        NativeLaneOutcomeV1::BudgetExceeded(budget)
    );
    assert_eq!(
        context
            .semantic(RetrieverOutcome::Cancelled, |_| true)
            .expect("cancelled decision"),
        NativeLaneOutcomeV1::Cancelled
    );
}

#[test]
fn public_execution_rejects_cross_generation_evidence() {
    let records = FixtureRecords {
        generation: generation(),
    };
    let context = context(&records);
    let mut evidence = lexical_evidence();
    evidence.binding.occurrence.generation = id("generation.other");

    assert_eq!(
        context.lexical(
            RetrieverOutcome::Complete(
                batch(candidate(RetrieverKind::Lexical, 875_000), evidence,)
            ),
            |_| true,
        ),
        Err(tracedecay_query::retrieval::QueryExecutionContractErrorV1::GenerationMismatch)
    );
}

fn retrieval_budget() -> RetrievalBudget {
    RetrievalBudget {
        max_candidates_per_lane: 32,
        max_fused_candidates: 16,
        max_hydrated_results: 8,
        max_hydration_bytes: 65_536,
        deadline_micros: None,
    }
}

fn retrieval_request() -> RetrievalRequest {
    RetrievalRequest {
        principal: id::<PrincipalId>("principal.canonical-equivalence"),
        scope: RetrievalScope {
            privacy_domain: id("privacy.canonical-equivalence"),
            root: SingleRootScopeV1 {
                repository: id("repository.canonical-equivalence"),
                worktree: None,
                reference: None,
            },
        },
        temporal_mode: TemporalModeV1::Current,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: digest('7'),
            authorization_revision: id("authorization.canonical-equivalence.v1"),
            captured_at: UtcMicros(10),
        },
        profile_id: id("profile.canonical-equivalence.v1"),
        budget: retrieval_budget(),
    }
}

fn query_authority() -> Arc<Pr9QueryAuthorityV1> {
    let evaluation = RetrievalAnchorId::new("evaluation.canonical-equivalence")
        .expect("valid evaluation anchor");
    let calibrations = RetrieverKind::PR9_FALLBACK_LANES
        .into_iter()
        .map(|lane| {
            (
                lane,
                id::<CalibrationProfileId>(&format!(
                    "calibration.{}.canonical-equivalence.v1",
                    lane.as_str()
                )),
            )
        })
        .collect();
    let score_domain_calibrations = RetrieverKind::PR9_FALLBACK_LANES
        .into_iter()
        .map(|lane| {
            let score_domain: ScoreDomainId =
                id(&format!("score.{}.canonical-equivalence.v1", lane.as_str()));
            (
                score_domain.clone(),
                ScoreDomainCalibrationV1 {
                    calibration_profile_id: id(&format!(
                        "calibration.{}.canonical-equivalence.v1",
                        lane.as_str()
                    )),
                    score_domain,
                    raw_min_micros: 0,
                    raw_max_micros: 1_000_000,
                },
            )
        })
        .collect();
    let profile = FusionProfile {
        profile_id: id("profile.canonical-equivalence.v1"),
        evaluation_result_anchor: evaluation.clone(),
        calibrations,
        score_domain_calibrations,
        weights_micros: [
            (RetrieverKind::ExactLiteral, 1_000_000),
            (RetrieverKind::Lexical, 500_000),
            (RetrieverKind::Graph, 250_000),
        ]
        .into_iter()
        .collect(),
        diversity_policy_id: id("diversity.canonical-equivalence.v1"),
        rerank_policy_id: None,
        retrieval_budget: retrieval_budget(),
    };
    let diversity = DiversityPolicy {
        policy_id: id("diversity.canonical-equivalence.v1"),
        evaluation_result_anchor: Some(evaluation),
        per_source_namespace: None,
        per_source_instance: None,
        per_repository: None,
        per_file: None,
        per_session_or_thread: None,
        per_copy_cluster: None,
        per_evidence_role: None,
    };
    let keyring = RetrievalCursorKeyringV1::new(
        id("privacy.canonical-equivalence"),
        id::<RetrievalCursorKeyId>("cursor-key.canonical-equivalence.v1"),
        7,
        vec![0x5a; 32],
        15 * 60 * 1_000_000,
    )
    .expect("valid cursor keyring");

    Arc::new(
        Pr9QueryAuthorityV1::new(
            profile,
            diversity,
            id::<ComponentRevision>(PR9_RANKING_REVISION_V1),
            keyring,
        )
        .expect("valid PR9 query authority"),
    )
}

#[test]
fn equivalent_prepared_queries_emit_identical_stable_cursor_bytes() {
    let authority = query_authority();
    let request = retrieval_request();
    let bindings = PreparedQueryBindingsV1::new(
        "code_canonical_query",
        digest::<ManifestDigest>('8'),
        generation(),
        digest::<ManifestDigest>('9'),
    )
    .expect("valid prepared-query bindings");
    let items = vec!["first".to_owned(), "second".to_owned(), "third".to_owned()];
    let now = UtcMicros(10);

    assert_eq!(
        PreparedQueryV1::prepare(authority.clone(), request.clone(), None)
            .expect("zero-page prepared query")
            .paginate(&bindings, items.clone(), 0, now),
        Err(tracedecay_query::retrieval::PreparedQueryErrorV1::Invalid)
    );

    let first = PreparedQueryV1::prepare(authority.clone(), request.clone(), None)
        .expect("first equivalent prepared query")
        .paginate(&bindings, items.clone(), 1, now)
        .expect("first prepared-query page");
    let second = PreparedQueryV1::prepare(authority.clone(), request.clone(), None)
        .expect("second equivalent prepared query")
        .paginate(&bindings, items.clone(), 1, now)
        .expect("second prepared-query page");

    assert_eq!(first.items, ["first"]);
    assert_eq!(first.total, 3);
    assert_eq!(first.next_cursor, second.next_cursor);
    assert_eq!(first.expires_at, Some(UtcMicros(900_000_010)));
    let first_cursor = first.next_cursor.expect("first continuation cursor");
    let second_cursor = second.next_cursor.expect("second continuation cursor");
    let first_wire = first_cursor
        .strip_prefix("ccq1.")
        .expect("canonical prepared-query cursor");
    let second_wire = second_cursor
        .strip_prefix("ccq1.")
        .expect("canonical prepared-query cursor");
    assert_eq!(
        hex::decode(first_wire).expect("first cursor bytes"),
        hex::decode(second_wire).expect("second cursor bytes"),
    );

    let routing = inspect_prepared_query_cursor(&first_cursor).expect("public cursor routing");
    assert_eq!(routing.generation, generation());
    assert_eq!(routing.expires_at, UtcMicros(900_000_010));

    assert_eq!(
        PreparedQueryV1::prepare(authority.clone(), request.clone(), Some(&first_cursor))
            .expect("authenticated prepared-query continuation")
            .paginate(&bindings, items.clone(), 2, UtcMicros(100)),
        Err(tracedecay_query::retrieval::PreparedQueryErrorV1::Invalid)
    );
    let resumed = PreparedQueryV1::prepare(authority.clone(), request.clone(), Some(&first_cursor))
        .expect("authenticated prepared-query continuation")
        .paginate(&bindings, items.clone(), 1, UtcMicros(100))
        .expect("resumed prepared-query page");
    let replayed = PreparedQueryV1::prepare(authority, request, Some(&first_cursor))
        .expect("stateless continuation remains restart-stable")
        .paginate(&bindings, items, 1, UtcMicros(100))
        .expect("stateless continuation can be retried");
    assert_eq!(resumed.items, ["second"]);
    assert_eq!(replayed.items, resumed.items);
    assert_eq!(replayed.next_cursor, resumed.next_cursor);
    assert_eq!(resumed.total, 3);
    let resumed_cursor = resumed.next_cursor.expect("second continuation cursor");
    assert_eq!(
        inspect_prepared_query_cursor(&resumed_cursor)
            .expect("resumed cursor routing")
            .expires_at,
        UtcMicros(900_000_010)
    );
}
