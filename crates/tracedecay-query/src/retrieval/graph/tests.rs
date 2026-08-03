//! Focused graph-lane adapter tests. The read port is an in-memory fake over
//! frozen Plan 25 generation evidence; no graph rows are copied into a search
//! corpus and no graph storage behavior is exercised here.

use std::collections::BTreeMap;
use std::fmt;

use tempfile::TempDir;
use tracedecay_application::CancellationSignal;
use tracedecay_code_index::graph_projection::{
    CodeGraphEvidenceAdapterV1, CodeGraphProjectionPublisher, CodeGraphProjectionStore,
};
use tracedecay_application::retrieval::MAX_CALLABLE_CODE_DEPTH;
use tracedecay_domain::{
    BoundedSanitizedText, CanonicalRelationEdgeV1, ChunkerRevision, CodeSearchChunkAnchorV1,
    CodeSearchChunkGrainV1, CodeSearchChunkV1, CompactCandidate, ContentDigest, EdgeAuthorityV1,
    EvidenceRole, FixedPointScore, FreshnessCompatibilityV1, LanguageDescriptorRevision,
    PolicyRevisionId, PrincipalId, RelationEdgeKindV1, RetrievalBudget, RetrievalFailure,
    RetrievalRequest, RetrievalScope, RetrievalSnapshot, RetrieverBatch, RetrieverCoverage,
    RetrieverKind, RetrieverOutcome, SanitizerRevision, SensitivityDecision, SensitivityLevelV1,
    SingleRootScopeV1, SourceFreshness, SourceSpan, SymbolOccurrenceId, TemporalModeV1, UtcMicros,
    VectorWatermark,
};

use super::{
    GraphLane, GraphLaneEvidence, GraphLaneRequest, GraphLaneRetriever, GraphPathSegmentV1,
};
use crate::retrieval::ports::{
    CodeCandidateBindingV1, CodeOccurrenceRefV1, GraphEvidenceReadPort, RetrievalPortError,
};

mod measurement;
mod scale;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

fn digest_id<T>(byte: char) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).expect("valid fixture digest")
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
            freshness_digest: digest_id('f'),
            authorization_revision: id("authorization.v1"),
            captured_at: UtcMicros(7),
        },
        profile_id: id("profile.fixture.v1"),
        budget: budget(max_candidates_per_lane),
    }
}

fn freshness(compatibility: FreshnessCompatibilityV1) -> SourceFreshness {
    SourceFreshness {
        source_namespace: id("ns.code.fixture"),
        source_instance: id("instance.fixture"),
        source_watermark: Some(7),
        projection_watermark: Some(7),
        observed_at: UtcMicros(7),
        source_generation: Some(1),
        generation_lag: Some(0),
        compatibility,
        policy_revision: id("policy.fixture.v1"),
    }
}

fn binding(request: &GraphLaneRequest, occurrence: &str, symbol: &str) -> CodeCandidateBindingV1 {
    CodeCandidateBindingV1 {
        candidate_anchor: id(&format!("anchor.{occurrence}")),
        occurrence: CodeOccurrenceRefV1 {
            generation: request.generation.clone(),
            file: id(&format!("file.{occurrence}")),
            symbol: Some(id(symbol)),
            chunk: Some(id(&format!("chunk.{occurrence}"))),
        },
        language_descriptor_revision: id("language.rust.v1"),
        matched_term_kinds: Vec::new(),
        source_occurrence: id(occurrence),
    }
}

fn graph_request(max_candidates: u32, max_depth: u32) -> GraphLaneRequest {
    let mut request = GraphLaneRequest {
        base: base_request(max_candidates),
        generation: id("generation.1"),
        seed_anchors: Vec::new(),
        edge_kinds: vec![RelationEdgeKindV1::Calls, RelationEdgeKindV1::Uses],
        max_depth,
        budget: budget(max_candidates),
    };
    request.seed_anchors = vec![binding(&request, "occ.seed", "symbol.seed")];
    request
}

fn graph_pair(
    request: &GraphLaneRequest,
    occurrence: &str,
    path_ids: &[&str],
    authorities: &[EdgeAuthorityV1],
    score_micros: u64,
) -> (CompactCandidate, GraphLaneEvidence) {
    assert_eq!(path_ids.len(), authorities.len() + 1);
    let candidate = CompactCandidate {
        anchor_id: id(&format!("anchor.{occurrence}")),
        logical_evidence_id: id(&format!("logical.{occurrence}")),
        source_occurrence_id: id(occurrence),
        file_occurrence_id: None,
        source_namespace: id("ns.code.fixture"),
        repository_id: None,
        session_or_thread_id: None,
        logical_copy_cluster_id: None,
        logical_copy_evidence_anchor: None,
        evidence_role: EvidenceRole::Primary,
        retriever: RetrieverKind::Graph,
        retriever_revision: id("retriever.graph.v1"),
        score_domain: id("score.graph.v1"),
        raw_score: FixedPointScore(score_micros),
        ordinal_rank: 0,
        exact_admission_proof: None,
        retriever_evidence_anchor: id(&format!("evidence-anchor.{occurrence}")),
        freshness: freshness(FreshnessCompatibilityV1::Current),
    };
    let path: Vec<GraphPathSegmentV1> = authorities
        .iter()
        .enumerate()
        .map(|(index, authority)| GraphPathSegmentV1 {
            from: id(path_ids[index]),
            to: id(path_ids[index + 1]),
            edge_kind: RelationEdgeKindV1::Calls,
            authority: *authority,
            evidence_span: SourceSpan {
                start_byte: index as u64,
                end_byte: index as u64 + 1,
            },
        })
        .collect();
    let weakest_authority = authorities
        .iter()
        .copied()
        .reduce(EdgeAuthorityV1::weakest)
        .expect("fixture path has an edge");
    let evidence = GraphLaneEvidence {
        binding: binding(
            request,
            occurrence,
            path_ids.last().expect("fixture path has a target"),
        ),
        path,
        weakest_authority,
    };
    (candidate, evidence)
}

fn projection_chunk(request: &GraphLaneRequest, id_value: &str, symbol: &str) -> CodeSearchChunkV1 {
    CodeSearchChunkV1 {
        id: id(id_value),
        anchor: CodeSearchChunkAnchorV1 {
            generation_id: request.generation.clone(),
            file_occurrence_id: id(&format!("file.{symbol}")),
            symbol_occurrence_id: Some(id(symbol)),
            parent_chunk_id: None,
            source_span: SourceSpan {
                start_byte: 0,
                end_byte: 4,
            },
            grain: CodeSearchChunkGrainV1::SymbolBody,
            ordinal: 0,
        },
        content_digest: digest_id::<ContentDigest>('c'),
        language_descriptor_revision: id::<LanguageDescriptorRevision>("language.rust.v1"),
        chunker_revision: id::<ChunkerRevision>("chunker.v1"),
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
        sensitivity: SensitivityDecision {
            level: SensitivityLevelV1::Public,
            policy_revision: id::<PolicyRevisionId>("policy.v1"),
        },
        exact_terms: Vec::new(),
        subtokens: Vec::new(),
        sanitized_text: BoundedSanitizedText::new("code").expect("bounded fixture text"),
    }
}

fn projection_batch(
    request: &GraphLaneRequest,
    edges: &[CanonicalRelationEdgeV1],
    symbols: &[&str],
) -> RetrieverBatch<GraphLaneEvidence> {
    let chunks: Vec<_> = symbols
        .iter()
        .map(|symbol| projection_chunk(request, &format!("chunk.{symbol}"), symbol))
        .collect();
    let adapter = CodeGraphEvidenceAdapterV1::new(
        request.generation.clone(),
        None,
        freshness(FreshnessCompatibilityV1::Current),
        edges,
        &chunks,
    )
    .expect("projection is valid");
    complete_batch(
        adapter
            .read_graph_evidence(request)
            .expect("graph read succeeds"),
    )
}

fn publish_projection(
    store: &CodeGraphProjectionStore,
    request: &GraphLaneRequest,
    edges: &[CanonicalRelationEdgeV1],
    symbols: &[&str],
) {
    let chunks: Vec<_> = symbols
        .iter()
        .map(|symbol| projection_chunk(request, &format!("chunk.{symbol}"), symbol))
        .collect();
    let cancellation =
        CancellationSignal::active("cancellation.code-graph.fixture").expect("valid token");
    store
        .publish_code_graph(&request.generation, edges, &chunks, &cancellation)
        .expect("projection publication succeeds");
}

fn batch(
    pairs: Vec<(CompactCandidate, GraphLaneEvidence)>,
    coverage: RetrieverCoverage,
) -> RetrieverBatch<GraphLaneEvidence> {
    let mut candidates = Vec::new();
    let mut evidence_by_occurrence = BTreeMap::new();
    for (ordinal, (mut candidate, evidence)) in pairs.into_iter().enumerate() {
        candidate.ordinal_rank = ordinal as u32;
        evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
        candidates.push(candidate);
    }
    RetrieverBatch {
        candidates,
        evidence_by_occurrence,
        coverage,
        continuation: None,
    }
}

fn candidate_coverage(count: u64) -> RetrieverCoverage {
    RetrieverCoverage {
        examined: count,
        eligible: count,
        excluded: 0,
        capped: 0,
        unknown: 0,
    }
}

#[derive(Clone)]
enum PortReply {
    Outcome(RetrieverOutcome<RetrieverBatch<GraphLaneEvidence>>),
    Error(RetrievalPortError),
}

#[derive(Clone)]
struct FakeGraphPort {
    reply: PortReply,
}

impl FakeGraphPort {
    fn outcome(outcome: RetrieverOutcome<RetrieverBatch<GraphLaneEvidence>>) -> Self {
        Self {
            reply: PortReply::Outcome(outcome),
        }
    }

    fn error(error: RetrievalPortError) -> Self {
        Self {
            reply: PortReply::Error(error),
        }
    }
}

impl GraphEvidenceReadPort for FakeGraphPort {
    fn read_graph_evidence(
        &self,
        _request: &GraphLaneRequest,
    ) -> Result<RetrieverOutcome<RetrieverBatch<GraphLaneEvidence>>, RetrievalPortError> {
        match &self.reply {
            PortReply::Outcome(outcome) => Ok(outcome.clone()),
            PortReply::Error(error) => Err(error.clone()),
        }
    }
}

fn complete_batch(
    outcome: RetrieverOutcome<RetrieverBatch<GraphLaneEvidence>>,
) -> RetrieverBatch<GraphLaneEvidence> {
    match outcome {
        RetrieverOutcome::Complete(value) => value,
        other => panic!("expected a complete graph batch, got {other:?}"),
    }
}

fn result_order(batch: &RetrieverBatch<GraphLaneEvidence>, expected: &[&str]) {
    let actual: Vec<&str> = batch
        .candidates
        .iter()
        .map(|candidate| candidate.source_occurrence_id.as_str())
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn graph_projection_bounds_self_loop_and_cycle_at_depth_with_exact_coverage() {
    let request = graph_request(8, 2);
    let edge = |from: &str, to: &str, start_byte| CanonicalRelationEdgeV1 {
        from_occurrence: id(from),
        to_occurrence: id(to),
        kind: RelationEdgeKindV1::Calls,
        authority: EdgeAuthorityV1::SyntaxExact,
        evidence_span: SourceSpan {
            start_byte,
            end_byte: start_byte + 1,
        },
    };
    let edges = vec![
        edge("symbol.seed", "symbol.seed", 0),
        edge("symbol.seed", "symbol.middle", 1),
        edge("symbol.middle", "symbol.seed", 2),
        edge("symbol.middle", "symbol.target", 3),
        edge("symbol.target", "symbol.sink", 4),
    ];

    let result = projection_batch(
        &request,
        &edges,
        &[
            "symbol.seed",
            "symbol.middle",
            "symbol.target",
            "symbol.sink",
        ],
    );

    result_order(
        &result,
        &["code-graph:symbol.middle", "code-graph:symbol.target"],
    );
    assert_eq!(
        result.coverage,
        RetrieverCoverage {
            examined: 4,
            eligible: 2,
            excluded: 0,
            capped: 0,
            unknown: 0,
        }
    );
    assert_eq!(
        result.evidence_by_occurrence[&id("code-graph:symbol.middle")]
            .path
            .len(),
        1
    );
    assert_eq!(
        result.evidence_by_occurrence[&id("code-graph:symbol.target")]
            .path
            .len(),
        2
    );
    assert!(
        !result
            .evidence_by_occurrence
            .contains_key(&id("code-graph:symbol.sink"))
    );
}

#[test]
fn graph_projection_deduplicates_identical_edges_byte_for_byte_including_coverage() {
    let request = graph_request(8, 1);
    let edge = CanonicalRelationEdgeV1 {
        from_occurrence: id("symbol.seed"),
        to_occurrence: id("symbol.target"),
        kind: RelationEdgeKindV1::Calls,
        authority: EdgeAuthorityV1::SyntaxExact,
        evidence_span: SourceSpan {
            start_byte: 0,
            end_byte: 1,
        },
    };
    let single = projection_batch(
        &request,
        std::slice::from_ref(&edge),
        &["symbol.seed", "symbol.target"],
    );
    let duplicate = projection_batch(
        &request,
        &[edge.clone(), edge],
        &["symbol.seed", "symbol.target"],
    );

    assert_eq!(single, duplicate);
    assert_eq!(
        serde_json::to_vec(&single).expect("serialize single projection"),
        serde_json::to_vec(&duplicate).expect("serialize duplicate projection")
    );
    assert_eq!(
        single.coverage,
        RetrieverCoverage {
            examined: 1,
            eligible: 1,
            excluded: 0,
            capped: 0,
            unknown: 0,
        }
    );
}

#[test]
fn graph_projection_rejects_foreign_generation_chunks() {
    let request = graph_request(8, 1);
    let mut foreign = projection_chunk(&request, "chunk.foreign", "symbol.foreign");
    foreign.anchor.generation_id = id("generation.foreign");

    let result = CodeGraphEvidenceAdapterV1::new(
        request.generation,
        None,
        freshness(FreshnessCompatibilityV1::Current),
        &[],
        &[foreign],
    );

    assert!(matches!(
        result,
        Err(RetrievalPortError::GenerationMismatch)
    ));
}

#[test]
fn graph_projection_rejects_conflicting_symbol_bindings() {
    let request = graph_request(8, 1);
    let primary = projection_chunk(&request, "chunk.target.a", "symbol.target");
    let mut conflicting = projection_chunk(&request, "chunk.target.b", "symbol.target");
    conflicting.anchor.file_occurrence_id = id("file.symbol.other");

    let result = CodeGraphEvidenceAdapterV1::new(
        request.generation,
        None,
        freshness(FreshnessCompatibilityV1::Current),
        &[],
        &[primary, conflicting],
    );

    assert!(matches!(
        result,
        Err(RetrievalPortError::Contract(message))
            if message == "one symbol occurrence has conflicting graph candidate bindings"
    ));
}

#[test]
fn graph_lane_rejects_depth_above_the_callable_code_contract() {
    let request = graph_request(8, MAX_CALLABLE_CODE_DEPTH + 1);

    assert!(matches!(
        request.validate(),
        Err(RetrievalPortError::Contract(message))
            if message == "graph traversal depth exceeds the callable code bound"
    ));
}

#[test]
fn graph_lane_emits_generic_candidates_with_ordered_path_ids_and_weakest_authority() {
    let request = graph_request(8, 3);
    let pair = graph_pair(
        &request,
        "occ.target",
        &["symbol.seed", "symbol.middle", "symbol.target"],
        &[
            EdgeAuthorityV1::CompilerOrLspResolved,
            EdgeAuthorityV1::HeuristicCandidate,
        ],
        800_000,
    );
    let lane = GraphLane::new(FakeGraphPort::outcome(RetrieverOutcome::Complete(batch(
        vec![pair],
        candidate_coverage(1),
    ))));

    let result = complete_batch(
        lane.retrieve_graph(&request)
            .expect("graph retrieval succeeds"),
    );

    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.candidates[0].retriever, RetrieverKind::Graph);
    assert!(result.candidates[0].exact_admission_proof.is_none());
    let evidence = &result.evidence_by_occurrence[&id("occ.target")];
    let ordered_ids: Vec<&str> = evidence
        .ordered_path_ids()
        .expect("path is ordered")
        .iter()
        .map(|path_id: &&SymbolOccurrenceId| path_id.as_str())
        .collect();
    assert_eq!(
        ordered_ids,
        ["symbol.seed", "symbol.middle", "symbol.target"]
    );
    assert_eq!(
        evidence.weakest_authority,
        EdgeAuthorityV1::HeuristicCandidate
    );
    result.validate().expect("rebuilt batch is valid");
    let continuation = result.continuation.expect("checkpoint emitted");
    assert_eq!(continuation.lane, RetrieverKind::Graph);
    assert!(continuation.exhausted);
}

#[test]
fn graph_projection_emits_one_canonical_occurrence_when_seeds_converge() {
    let mut request = graph_request(8, 1);
    request.seed_anchors = vec![
        binding(&request, "occ.seed-weak", "symbol.seed-weak"),
        binding(&request, "occ.seed-strong", "symbol.seed-strong"),
    ];
    let target = id::<SymbolOccurrenceId>("symbol.target");
    let edges = vec![
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.seed-weak"),
            to_occurrence: target.clone(),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::HeuristicCandidate,
            evidence_span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
        },
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.seed-strong"),
            to_occurrence: target,
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 1,
                end_byte: 2,
            },
        },
    ];
    let symbols = ["symbol.seed-weak", "symbol.seed-strong", "symbol.target"];
    let result = projection_batch(&request, &edges, &symbols);

    result.validate().expect("batch remains valid");
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(
        result.candidates[0].source_occurrence_id.as_str(),
        "code-graph:symbol.target"
    );
    assert_eq!(
        result.candidates[0].anchor_id.as_str(),
        "code-symbol:symbol.target"
    );
    assert_eq!(
        result.candidates[0].logical_evidence_id.as_str(),
        "code-symbol:symbol.target"
    );
    let evidence = &result.evidence_by_occurrence[&id("code-graph:symbol.target")];
    assert_eq!(evidence.path[0].from.as_str(), "symbol.seed-strong");
    assert_eq!(evidence.weakest_authority, EdgeAuthorityV1::SyntaxExact);

    request.seed_anchors.reverse();
    let mut reversed_edges = edges;
    reversed_edges.reverse();
    let reversed = projection_batch(&request, &reversed_edges, &symbols);
    assert_eq!(
        reversed.evidence_by_occurrence[&id("code-graph:symbol.target")].path,
        evidence.path
    );
}

#[test]
fn graph_projection_reports_reachable_unbound_targets_as_unknown() {
    let request = graph_request(8, 1);
    let edge = CanonicalRelationEdgeV1 {
        from_occurrence: id("symbol.seed"),
        to_occurrence: id("symbol.unbound"),
        kind: RelationEdgeKindV1::Calls,
        authority: EdgeAuthorityV1::UnknownUnsupported,
        evidence_span: SourceSpan {
            start_byte: 0,
            end_byte: 1,
        },
    };

    let result = projection_batch(&request, &[edge], &["symbol.seed"]);

    assert!(result.candidates.is_empty());
    assert_eq!(result.coverage.examined, 1);
    assert_eq!(result.coverage.eligible, 0);
    assert_eq!(result.coverage.unknown, 1);
}

#[test]
fn graph_projection_accounts_for_filtered_edge_kinds() {
    let mut request = graph_request(8, 1);
    request.edge_kinds = vec![RelationEdgeKindV1::Uses];
    let edge = CanonicalRelationEdgeV1 {
        from_occurrence: id("symbol.seed"),
        to_occurrence: id("symbol.target"),
        kind: RelationEdgeKindV1::Calls,
        authority: EdgeAuthorityV1::SyntaxExact,
        evidence_span: SourceSpan {
            start_byte: 0,
            end_byte: 1,
        },
    };

    let result = projection_batch(&request, &[edge], &["symbol.seed", "symbol.target"]);

    assert!(result.candidates.is_empty());
    assert_eq!(result.coverage.examined, 1);
    assert_eq!(result.coverage.eligible, 0);
    assert_eq!(result.coverage.excluded, 1);
}

#[test]
fn graph_projection_selects_symbol_chunk_binding_canonically() {
    let request = graph_request(8, 1);
    let edge = CanonicalRelationEdgeV1 {
        from_occurrence: id("symbol.seed"),
        to_occurrence: id("symbol.target"),
        kind: RelationEdgeKindV1::Calls,
        authority: EdgeAuthorityV1::SyntaxExact,
        evidence_span: SourceSpan {
            start_byte: 0,
            end_byte: 1,
        },
    };
    let mut chunks = vec![
        projection_chunk(&request, "chunk.seed", "symbol.seed"),
        projection_chunk(&request, "chunk.target.z", "symbol.target"),
        projection_chunk(&request, "chunk.target.a", "symbol.target"),
    ];
    let read = |chunks: &[CodeSearchChunkV1]| {
        let adapter = CodeGraphEvidenceAdapterV1::new(
            request.generation.clone(),
            None,
            freshness(FreshnessCompatibilityV1::Current),
            std::slice::from_ref(&edge),
            chunks,
        )
        .expect("projection is valid");
        complete_batch(
            adapter
                .read_graph_evidence(&request)
                .expect("graph read succeeds"),
        )
    };

    let first = read(&chunks);
    chunks.reverse();
    let reversed = read(&chunks);

    assert_eq!(first, reversed);
    assert_eq!(
        first.evidence_by_occurrence[&id("code-graph:symbol.target")]
            .binding
            .occurrence
            .chunk
            .as_ref()
            .expect("chunk binding")
            .as_str(),
        "chunk.target.a"
    );
}

#[test]
fn graph_projection_breaks_equal_score_ties_by_canonical_full_path() {
    let mut request = graph_request(8, 1);
    request.seed_anchors = vec![
        binding(&request, "occ.seed-z", "symbol.seed-z"),
        binding(&request, "occ.seed-a", "symbol.seed-a"),
    ];
    let target = id::<SymbolOccurrenceId>("symbol.target");
    let edges = vec![
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.seed-z"),
            to_occurrence: target.clone(),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
        },
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.seed-a"),
            to_occurrence: target,
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 1,
                end_byte: 2,
            },
        },
    ];

    let result = projection_batch(
        &request,
        &edges,
        &["symbol.seed-z", "symbol.seed-a", "symbol.target"],
    );

    result.validate().expect("batch remains valid");
    let evidence = &result.evidence_by_occurrence[&id("code-graph:symbol.target")];
    assert_eq!(evidence.path[0].from.as_str(), "symbol.seed-a");
}

#[test]
fn graph_projection_relaxes_a_same_seed_diamond_to_the_stronger_path() {
    let request = graph_request(8, 2);
    let edges = vec![
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.seed"),
            to_occurrence: id("symbol.a-weak"),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
        },
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.seed"),
            to_occurrence: id("symbol.z-strong"),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 1,
                end_byte: 2,
            },
        },
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.a-weak"),
            to_occurrence: id("symbol.target"),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::HeuristicCandidate,
            evidence_span: SourceSpan {
                start_byte: 2,
                end_byte: 3,
            },
        },
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.z-strong"),
            to_occurrence: id("symbol.target"),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 3,
                end_byte: 4,
            },
        },
    ];

    let result = projection_batch(
        &request,
        &edges,
        &[
            "symbol.seed",
            "symbol.a-weak",
            "symbol.z-strong",
            "symbol.target",
        ],
    );

    result.validate().expect("batch remains valid");
    assert_eq!(
        result
            .candidates
            .iter()
            .filter(
                |candidate| candidate.source_occurrence_id.as_str() == "code-graph:symbol.target"
            )
            .count(),
        1
    );
    let evidence = &result.evidence_by_occurrence[&id("code-graph:symbol.target")];
    assert_eq!(evidence.path[0].to.as_str(), "symbol.z-strong");
    assert_eq!(evidence.weakest_authority, EdgeAuthorityV1::SyntaxExact);
}

#[test]
fn graph_lane_rejects_non_contiguous_ordered_path_ids() {
    let request = graph_request(8, 3);
    let (candidate, mut evidence) = graph_pair(
        &request,
        "occ.target",
        &["symbol.seed", "symbol.middle", "symbol.target"],
        &[EdgeAuthorityV1::SyntaxExact, EdgeAuthorityV1::NameResolved],
        800_000,
    );
    evidence.path[1].from = id("symbol.not-middle");
    let lane = GraphLane::new(FakeGraphPort::outcome(RetrieverOutcome::Complete(batch(
        vec![(candidate, evidence)],
        candidate_coverage(1),
    ))));

    let result = lane.retrieve_graph(&request);

    assert!(matches!(result, Err(RetrievalPortError::Contract(_))));
}

#[test]
fn graph_lane_rejects_paths_beyond_the_profile_depth_bound() {
    let request = graph_request(8, 1);
    let pair = graph_pair(
        &request,
        "occ.target",
        &["symbol.seed", "symbol.middle", "symbol.target"],
        &[EdgeAuthorityV1::SyntaxExact, EdgeAuthorityV1::NameResolved],
        800_000,
    );
    let lane = GraphLane::new(FakeGraphPort::outcome(RetrieverOutcome::Complete(batch(
        vec![pair],
        candidate_coverage(1),
    ))));

    let result = lane.retrieve_graph(&request);

    assert!(matches!(result, Err(RetrievalPortError::Contract(_))));
}

#[test]
fn graph_lane_recomputes_and_rejects_a_forged_weakest_authority() {
    let request = graph_request(8, 3);
    let (candidate, mut evidence) = graph_pair(
        &request,
        "occ.target",
        &["symbol.seed", "symbol.middle", "symbol.target"],
        &[
            EdgeAuthorityV1::SyntaxExact,
            EdgeAuthorityV1::HeuristicCandidate,
        ],
        800_000,
    );
    evidence.weakest_authority = EdgeAuthorityV1::SyntaxExact;
    let lane = GraphLane::new(FakeGraphPort::outcome(RetrieverOutcome::Complete(batch(
        vec![(candidate, evidence)],
        candidate_coverage(1),
    ))));

    let result = lane.retrieve_graph(&request);

    assert!(matches!(result, Err(RetrievalPortError::Contract(_))));
}

#[test]
fn graph_lane_rejects_cross_generation_evidence() {
    let request = graph_request(8, 2);
    let (candidate, mut evidence) = graph_pair(
        &request,
        "occ.target",
        &["symbol.seed", "symbol.target"],
        &[EdgeAuthorityV1::NameResolved],
        800_000,
    );
    evidence.binding.occurrence.generation = id("generation.2");
    let lane = GraphLane::new(FakeGraphPort::outcome(RetrieverOutcome::Complete(batch(
        vec![(candidate, evidence)],
        candidate_coverage(1),
    ))));

    let result = lane.retrieve_graph(&request);

    assert_eq!(result, Err(RetrievalPortError::GenerationMismatch));
}

#[test]
fn graph_lane_canonicalizes_and_caps_the_prefix_without_losing_coverage() {
    let request = graph_request(2, 2);
    let pairs = vec![
        graph_pair(
            &request,
            "occ.a",
            &["symbol.seed", "symbol.a"],
            &[EdgeAuthorityV1::SyntaxExact],
            100_000,
        ),
        graph_pair(
            &request,
            "occ.b",
            &["symbol.seed", "symbol.b"],
            &[EdgeAuthorityV1::NameResolved],
            300_000,
        ),
        graph_pair(
            &request,
            "occ.c",
            &["symbol.seed", "symbol.c"],
            &[EdgeAuthorityV1::CompilerOrLspResolved],
            200_000,
        ),
    ];
    let source_coverage = RetrieverCoverage {
        examined: 12,
        eligible: 3,
        excluded: 4,
        capped: 1,
        unknown: 2,
    };
    let first = complete_batch(
        GraphLane::new(FakeGraphPort::outcome(RetrieverOutcome::Complete(batch(
            pairs.clone(),
            source_coverage,
        ))))
        .retrieve_graph(&request)
        .expect("first run"),
    );
    let mut reversed = pairs;
    reversed.reverse();
    let second = complete_batch(
        GraphLane::new(FakeGraphPort::outcome(RetrieverOutcome::Complete(batch(
            reversed,
            source_coverage,
        ))))
        .retrieve_graph(&request)
        .expect("restarted run"),
    );

    assert_eq!(first, second, "committed graph prefix is deterministic");
    result_order(&first, &["occ.b", "occ.c"]);
    assert_eq!(first.coverage.examined, 12);
    assert_eq!(first.coverage.eligible, 3);
    assert_eq!(first.coverage.excluded, 4);
    assert_eq!(first.coverage.capped, 2);
    assert_eq!(first.coverage.unknown, 2);
    assert!(!first.continuation.expect("checkpoint emitted").exhausted);
}

#[test]
fn graph_lane_preserves_partial_stale_and_cancelled_outcomes() {
    let request = graph_request(8, 2);
    let pair = graph_pair(
        &request,
        "occ.target",
        &["symbol.seed", "symbol.target"],
        &[EdgeAuthorityV1::NameResolved],
        800_000,
    );
    let partial = GraphLane::new(FakeGraphPort::outcome(RetrieverOutcome::Partial {
        value: batch(vec![pair], candidate_coverage(1)),
        reason: RetrievalFailure::StaleSource,
    }))
    .retrieve_graph(&request)
    .expect("partial outcome");
    assert!(matches!(
        partial,
        RetrieverOutcome::Partial {
            reason: RetrievalFailure::StaleSource,
            ..
        }
    ));

    let stale_freshness = freshness(FreshnessCompatibilityV1::Stale);
    let stale = GraphLane::new(FakeGraphPort::outcome(RetrieverOutcome::Stale(
        stale_freshness.clone(),
    )))
    .retrieve_graph(&request)
    .expect("stale outcome");
    assert_eq!(stale, RetrieverOutcome::Stale(stale_freshness));

    let cancelled = GraphLane::new(FakeGraphPort::error(RetrievalPortError::Cancelled))
        .retrieve_graph(&request)
        .expect("cancelled outcome");
    assert_eq!(cancelled, RetrieverOutcome::Cancelled);
}

#[test]
fn graph_lane_reports_missing_authority_without_substitution() {
    let request = graph_request(8, 2);
    let lane = GraphLane::new(FakeGraphPort::error(
        RetrievalPortError::AuthorityUnavailable("graph generation is not published".to_owned()),
    ));

    let outcome = lane.retrieve_graph(&request).expect("typed outcome");

    assert!(matches!(
        outcome,
        RetrieverOutcome::Unavailable(RetrievalFailure::AuthorityUnavailable { .. })
    ));
}
