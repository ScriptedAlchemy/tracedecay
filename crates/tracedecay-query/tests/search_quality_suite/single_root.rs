use std::collections::BTreeMap;

use tracedecay_domain::{
    CalibrationProfileId, CodeGenerationId, CodeSearchChunkGrainV1, CompactCandidate,
    ComponentRevision, DiversityPolicy, EdgeAuthorityV1, EphemeralSanitizedQueryViewV1,
    ExactAdmissionRuleRevision, ExactClass, ExactTechnicalTermKindV1, FixedPointScore,
    FreshnessCompatibilityV1, FusionProfile, HydrationReceipt, QueryNormalizationRevision,
    RankedCandidate, RelationEdgeKindV1, RetrievalAnchorId, RetrievalCursorKeyId, RetrieverBatch,
    RetrieverCoverage, RetrieverKind, RetrieverOutcome, SanitizerRevision,
    ScoreDomainCalibrationV1, SourceSpan, SymbolOccurrenceId, UtcMicros,
};
use tracedecay_query::retrieval::exact::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLane, ExactLaneRequest,
    ExactLaneRetriever,
};
use tracedecay_query::retrieval::fusion::{
    CompositionKernel, CompositionLaneInput, CompositionOutputV1, FusionStageInput,
    RetrievalCursorKeyringV1,
};
use tracedecay_query::retrieval::graph::{
    GraphLane, GraphLaneEvidence, GraphLaneRequest, GraphLaneRetriever, GraphPathSegmentV1,
};
use tracedecay_query::retrieval::hydrate::{
    CanonicalLateHydration, HydrationAuthorizationV1, HydrationPreflightOutcomeV1,
    HydrationReadOutcomeV1, HydrationWorkPermitV1, LateHydrationSource,
};
use tracedecay_query::retrieval::lexical::{
    CodeLexicalProjectionAdapterV1, LexicalLane, LexicalLaneRetriever,
};
use tracedecay_query::retrieval::ports::{
    CodeCandidateBindingV1, CodeOccurrenceRefV1, GraphEvidenceReadPort, RetrievalPortError,
};

use crate::candidate_producers::{
    base_request, budget, chunk, complete, id, lexical_request, projection_metadata,
};

#[derive(Clone, Copy)]
enum GraphDisposition {
    Complete,
    Denied,
    Unavailable,
}

const CURSOR_NOW: UtcMicros = UtcMicros(10);

#[derive(Clone)]
enum GraphPortReply {
    Outcome(RetrieverOutcome<RetrieverBatch<GraphLaneEvidence>>),
    Error(RetrievalPortError),
}

#[derive(Clone)]
struct FixtureGraphPort {
    reply: GraphPortReply,
}

impl GraphEvidenceReadPort for FixtureGraphPort {
    fn read_graph_evidence(
        &self,
        _request: &GraphLaneRequest,
    ) -> Result<RetrieverOutcome<RetrieverBatch<GraphLaneEvidence>>, RetrievalPortError> {
        match &self.reply {
            GraphPortReply::Outcome(outcome) => Ok(outcome.clone()),
            GraphPortReply::Error(error) => Err(error.clone()),
        }
    }
}

struct SingleRootFixture {
    request: tracedecay_domain::RetrievalRequest,
    lanes: Vec<CompositionLaneInput>,
    kernel: CompositionKernel,
}

impl SingleRootFixture {
    fn compose(&self, lanes: Vec<CompositionLaneInput>) -> CompositionOutputV1 {
        self.kernel
            .compose(
                &FusionStageInput {
                    profile: profile(),
                    lanes,
                },
                &no_caps(),
            )
            .expect("single-root composition succeeds")
    }
}

fn profile() -> FusionProfile {
    FusionProfile {
        profile_id: id("profile.fixture.v1"),
        evaluation_result_anchor: id("evaluation.fixture"),
        calibrations: RetrieverKind::PR9_FALLBACK_LANES
            .into_iter()
            .map(|lane| {
                (
                    lane,
                    id::<CalibrationProfileId>(&format!("calibration.{}.v1", lane.as_str())),
                )
            })
            .collect(),
        score_domain_calibrations: [
            (RetrieverKind::ExactLiteral, "score.exact.v1"),
            (RetrieverKind::Lexical, "score.lexical.v1"),
            (RetrieverKind::Graph, "score.graph.v1"),
        ]
        .into_iter()
        .map(|(lane, score_domain)| {
            let score_domain: tracedecay_domain::ScoreDomainId = id(score_domain);
            (
                score_domain.clone(),
                ScoreDomainCalibrationV1 {
                    calibration_profile_id: id(&format!("calibration.{}.v1", lane.as_str())),
                    score_domain,
                    raw_min_micros: 0,
                    raw_max_micros: 1_000_000,
                },
            )
        })
        .collect(),
        weights_micros: BTreeMap::from([
            (RetrieverKind::ExactLiteral, 1_000_000),
            (RetrieverKind::Lexical, 1_000_000),
            (RetrieverKind::Graph, 1_000_000),
        ]),
        diversity_policy_id: id("diversity.fixture.v1"),
        rerank_policy_id: None,
        retrieval_budget: budget(16),
    }
}

fn no_caps() -> DiversityPolicy {
    DiversityPolicy {
        policy_id: id("diversity.fixture.v1"),
        evaluation_result_anchor: Some(id("evaluation.fixture")),
        per_source_namespace: None,
        per_source_instance: None,
        per_repository: None,
        per_file: None,
        per_session_or_thread: None,
        per_copy_cluster: None,
        per_evidence_role: None,
    }
}

fn query_view(query: &str) -> EphemeralSanitizedQueryViewV1 {
    EphemeralSanitizedQueryViewV1::sanitize(
        query,
        id::<SanitizerRevision>("query-sanitizer.v1"),
        id::<QueryNormalizationRevision>("query-normalization.v1"),
    )
    .expect("bounded sanitized query view")
}

fn cursor_keys(request: &tracedecay_domain::RetrievalRequest) -> RetrievalCursorKeyringV1 {
    RetrievalCursorKeyringV1::new(
        request.scope.privacy_domain.clone(),
        id::<RetrievalCursorKeyId>("retrieval-key.fixture"),
        1,
        vec![7_u8; 32],
        100,
    )
    .expect("retrieval cursor keyring")
}

fn graph_request(
    request: &tracedecay_domain::RetrievalRequest,
    generation: &CodeGenerationId,
) -> GraphLaneRequest {
    GraphLaneRequest {
        base: request.clone(),
        generation: generation.clone(),
        seed_anchors: vec![CodeCandidateBindingV1 {
            candidate_anchor: id("anchor.seed"),
            occurrence: CodeOccurrenceRefV1 {
                generation: generation.clone(),
                file: id("file.seed"),
                symbol: Some(id("symbol.seed")),
                chunk: Some(id("chunk.seed")),
            },
            language_descriptor_revision: id("language.rust.v1"),
            matched_term_kinds: Vec::new(),
            source_occurrence: id("occurrence.seed"),
        }],
        edge_kinds: vec![RelationEdgeKindV1::Calls],
        max_depth: 1,
        budget: budget(16),
    }
}

fn graph_pair(
    request: &GraphLaneRequest,
    template: &CompactCandidate,
    name: &str,
    score_micros: u64,
    retain_logical_identity: bool,
) -> (CompactCandidate, GraphLaneEvidence) {
    let mut candidate = template.clone();
    if !retain_logical_identity {
        candidate.anchor_id = id(&format!("anchor.graph.{name}"));
        candidate.logical_evidence_id = id(&format!("logical.graph.{name}"));
    }
    candidate.source_occurrence_id = id(&format!("occurrence.graph.{name}"));
    candidate.retriever = RetrieverKind::Graph;
    candidate.retriever_revision = id("retriever.graph.v1");
    candidate.score_domain = id("score.graph.v1");
    candidate.raw_score = FixedPointScore(score_micros);
    candidate.ordinal_rank = 0;
    candidate.exact_admission_proof = None;
    candidate.retriever_evidence_anchor = id(&format!("evidence.graph.{name}"));

    let target = id::<SymbolOccurrenceId>(&format!("symbol.graph.{name}"));
    let evidence = GraphLaneEvidence {
        binding: CodeCandidateBindingV1 {
            candidate_anchor: candidate.anchor_id.clone(),
            occurrence: CodeOccurrenceRefV1 {
                generation: request.generation.clone(),
                file: id(&format!("file.graph.{name}")),
                symbol: Some(target.clone()),
                chunk: Some(id(&format!("chunk.graph.{name}"))),
            },
            language_descriptor_revision: id("language.rust.v1"),
            matched_term_kinds: Vec::new(),
            source_occurrence: candidate.source_occurrence_id.clone(),
        },
        path: vec![GraphPathSegmentV1 {
            from: id("symbol.seed"),
            to: target,
            edge_kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
        }],
        weakest_authority: EdgeAuthorityV1::SyntaxExact,
    };
    (candidate, evidence)
}

fn graph_batch(
    request: &GraphLaneRequest,
    lexical_candidates: &[CompactCandidate],
) -> RetrieverBatch<GraphLaneEvidence> {
    let mut pairs = vec![
        graph_pair(request, &lexical_candidates[0], "overlap", 4_000_000, true),
        graph_pair(request, &lexical_candidates[1], "first", 700_000, false),
        graph_pair(request, &lexical_candidates[2], "second", 600_000, false),
    ];
    let mut candidates = Vec::with_capacity(pairs.len());
    let mut evidence_by_occurrence = BTreeMap::new();
    for (ordinal, (mut candidate, evidence)) in pairs.drain(..).enumerate() {
        candidate.ordinal_rank = ordinal as u32;
        evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
        candidates.push(candidate);
    }
    RetrieverBatch {
        candidates,
        evidence_by_occurrence,
        coverage: RetrieverCoverage {
            examined: 3,
            eligible: 3,
            excluded: 0,
            capped: 0,
            unknown: 0,
        },
        continuation: None,
    }
}

fn fixture(disposition: GraphDisposition) -> SingleRootFixture {
    let generation = id::<CodeGenerationId>("generation.1");
    let request = base_request("--release", 16);
    let chunks = vec![
        chunk(
            &generation,
            1,
            CodeSearchChunkGrainV1::SymbolBody,
            "build with --release",
            &[(ExactTechnicalTermKindV1::CliFlag, "--release")],
            &["build", "release"],
        ),
        chunk(
            &generation,
            2,
            CodeSearchChunkGrainV1::SymbolSignature,
            "fn target_alpha",
            &[],
            &["target", "alpha"],
        ),
        chunk(
            &generation,
            3,
            CodeSearchChunkGrainV1::SymbolSignature,
            "fn target_beta",
            &[],
            &["target", "beta"],
        ),
        chunk(
            &generation,
            4,
            CodeSearchChunkGrainV1::SymbolSignature,
            "fn target_gamma",
            &[],
            &["target", "gamma"],
        ),
    ];
    let projection = CodeLexicalProjectionAdapterV1::new(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        chunks,
    )
    .expect("single-root projection builds");

    let authority =
        CentralExactAdmissionAuthorityV1::new(id::<ExactAdmissionRuleRevision>("exact-rules.v1"));
    let exact_query_view = query_view("--release");
    let exact_request = ExactLaneRequest {
        literals: authority.parse_literals(&exact_query_view, &request),
        base: request.clone(),
        query_view: &exact_query_view,
        generation: generation.clone(),
        budget: budget(16),
    };
    let exact_outcome = ExactLane::new(authority.clone(), projection.exact_adapter(authority))
        .retrieve_exact(&exact_request)
        .expect("exact lane completes");

    let mut lexical_request = lexical_request("--release", &[], &["target"], &[], 0, 16);
    lexical_request.base = request.clone();
    let lexical_outcome = LexicalLane::new(projection)
        .retrieve_lexical(&lexical_request)
        .expect("lexical lane completes");
    let lexical_batch = complete(lexical_outcome.clone());
    assert_eq!(lexical_batch.candidates.len(), 3);

    let graph_request = graph_request(&request, &generation);
    let reply = match disposition {
        GraphDisposition::Complete => GraphPortReply::Outcome(RetrieverOutcome::Complete(
            graph_batch(&graph_request, &lexical_batch.candidates),
        )),
        GraphDisposition::Denied => GraphPortReply::Outcome(RetrieverOutcome::Denied),
        GraphDisposition::Unavailable => {
            GraphPortReply::Error(RetrievalPortError::AuthorityUnavailable(
                "fixture graph authority is unavailable".to_owned(),
            ))
        }
    };
    let graph_outcome = GraphLane::new(FixtureGraphPort { reply })
        .retrieve_graph(&graph_request)
        .expect("graph lane reports a typed outcome");

    SingleRootFixture {
        request,
        lanes: vec![
            CompositionLaneInput::new(RetrieverKind::ExactLiteral, exact_outcome)
                .expect("exact lane is admitted"),
            CompositionLaneInput::new(RetrieverKind::Lexical, lexical_outcome)
                .expect("lexical lane is admitted"),
            CompositionLaneInput::new(RetrieverKind::Graph, graph_outcome)
                .expect("graph lane is admitted"),
        ],
        kernel: CompositionKernel::new(id::<ComponentRevision>("ranking.fixture.v1")),
    }
}

#[test]
fn protected_exact_precedes_higher_scoring_lexical_and_graph_evidence() {
    let fixture = fixture(GraphDisposition::Complete);
    let output = fixture.compose(fixture.lanes.clone());
    let first = &output.ranked_candidates[0].candidate;
    let strongest_approximate = output
        .ranked_candidates
        .iter()
        .filter(|ranked| ranked.candidate.exact_class == ExactClass::Approximate)
        .map(|ranked| ranked.candidate.utility_micros)
        .max()
        .expect("approximate candidates exist");

    assert_ne!(first.exact_class, ExactClass::Approximate);
    assert!(
        strongest_approximate > first.utility_micros,
        "exact precedence must not depend on the approximate utility score"
    );
    assert!(
        output.ranked_candidates[1..]
            .iter()
            .all(|ranked| ranked.candidate.exact_class == ExactClass::Approximate)
    );
}

#[test]
fn shuffled_lane_completion_is_byte_stable() {
    let fixture = fixture(GraphDisposition::Complete);
    let expected = fixture.compose(fixture.lanes.clone());
    let query_view = query_view("--release");
    let cursor_keys = cursor_keys(&fixture.request);
    let expected_page = fixture
        .kernel
        .paginate_at(
            &fixture.request,
            &query_view,
            &cursor_keys,
            &expected,
            2,
            None,
            CURSOR_NOW,
        )
        .expect("first page is available");

    for iteration in 0..100 {
        let mut shuffled = fixture.lanes.clone();
        let offset = iteration % shuffled.len();
        shuffled.rotate_left(offset);
        if iteration % 2 == 1 {
            shuffled.reverse();
        }
        let output = fixture.compose(shuffled);
        let page = fixture
            .kernel
            .paginate_at(
                &fixture.request,
                &query_view,
                &cursor_keys,
                &output,
                2,
                None,
                CURSOR_NOW,
            )
            .expect("shuffled first page is available");
        assert_eq!(output, expected, "shuffle {iteration} changed composition");
        assert_eq!(
            page, expected_page,
            "shuffle {iteration} changed cursor bytes"
        );
    }
}

#[test]
fn denied_graph_lane_does_not_change_public_results_or_cursor_bytes() {
    let denied = fixture(GraphDisposition::Denied);
    let unavailable = fixture(GraphDisposition::Unavailable);
    let denied_output = denied.compose(denied.lanes.clone());
    let unavailable_output = unavailable.compose(unavailable.lanes.clone());

    assert_eq!(
        denied_output.ranked_candidates,
        unavailable_output.ranked_candidates
    );
    assert_eq!(
        denied_output.public_lane_statuses,
        unavailable_output.public_lane_statuses
    );
    assert_ne!(
        denied_output.internal_lane_outcomes,
        unavailable_output.internal_lane_outcomes
    );
    let denied_query_view = query_view("--release");
    let denied_cursor_keys = cursor_keys(&denied.request);
    let denied_cursor = denied
        .kernel
        .paginate_at(
            &denied.request,
            &denied_query_view,
            &denied_cursor_keys,
            &denied_output,
            2,
            None,
            CURSOR_NOW,
        )
        .expect("denied page")
        .cursor
        .expect("denied overflow cursor");
    let unavailable_query_view = query_view("--release");
    let unavailable_cursor_keys = cursor_keys(&unavailable.request);
    let unavailable_cursor = unavailable
        .kernel
        .paginate_at(
            &unavailable.request,
            &unavailable_query_view,
            &unavailable_cursor_keys,
            &unavailable_output,
            2,
            None,
            CURSOR_NOW,
        )
        .expect("unavailable page")
        .cursor
        .expect("unavailable overflow cursor");
    assert_eq!(denied_cursor, unavailable_cursor);
}

#[test]
fn cursor_spillback_covers_three_pages_without_reordering_or_duplication() {
    let fixture = fixture(GraphDisposition::Complete);
    let output = fixture.compose(fixture.lanes.clone());
    assert_eq!(output.ranked_candidates.len(), 6);

    let query_view = query_view("--release");
    let cursor_keys = cursor_keys(&fixture.request);
    let mut cursor = None;
    let mut paged = Vec::new();
    for page_number in 0..3 {
        let page = fixture
            .kernel
            .paginate_at(
                &fixture.request,
                &query_view,
                &cursor_keys,
                &output,
                2,
                cursor.as_ref(),
                CURSOR_NOW,
            )
            .expect("cursor page is valid");
        assert_eq!(page.ranked_candidates.len(), 2);
        paged.extend(page.ranked_candidates);
        cursor = page.cursor;
        assert_eq!(cursor.is_some(), page_number < 2);
    }

    assert_eq!(paged, output.ranked_candidates);
    assert_eq!(
        paged
            .iter()
            .map(|ranked| ranked.final_ordinal)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4, 5]
    );
}

#[derive(Default)]
struct RecordingHydrationSource {
    reads: Vec<RetrievalAnchorId>,
}

impl LateHydrationSource<String> for RecordingHydrationSource {
    fn authorize(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        _candidate: &RankedCandidate,
    ) -> HydrationAuthorizationV1 {
        HydrationAuthorizationV1::Authorized
    }

    fn preflight_authorized(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        _candidate: &RankedCandidate,
        _permit: &HydrationWorkPermitV1,
    ) -> HydrationPreflightOutcomeV1 {
        HydrationPreflightOutcomeV1::Ready { estimated_bytes: 1 }
    }

    fn hydrate_authorized(
        &mut self,
        _request: &tracedecay_domain::RetrievalRequest,
        candidate: &RankedCandidate,
        permit: &HydrationWorkPermitV1,
    ) -> HydrationReadOutcomeV1<String> {
        assert!(permit.remaining_bytes > 0);
        let occurrence = candidate
            .candidate
            .occurrences
            .first()
            .expect("ranked candidate has occurrence provenance");
        self.reads.push(candidate.candidate.anchor_id.clone());
        HydrationReadOutcomeV1::Complete {
            payload: candidate.candidate.anchor_id.as_str().to_owned(),
            receipt: HydrationReceipt {
                anchor_id: candidate.candidate.anchor_id.clone(),
                source_occurrence_id: occurrence.source_occurrence_id.clone(),
                hydration_revision: id("hydration.fixture.v1"),
                bytes_hydrated: 1,
                authorized: true,
                freshness: occurrence.freshness.clone(),
            },
        }
    }
}

#[test]
fn exact_lexical_and_graph_rank_before_hydrating_only_the_selected_prefix() {
    let fixture = fixture(GraphDisposition::Complete);
    let output = fixture.compose(fixture.lanes.clone());
    let selected = output.ranked_candidates[..3].to_vec();
    let expected_reads = selected
        .iter()
        .map(|ranked| ranked.candidate.anchor_id.clone())
        .collect::<Vec<_>>();
    assert_ne!(selected[0].candidate.exact_class, ExactClass::Approximate);
    assert!(
        selected[1]
            .candidate
            .contributions
            .iter()
            .any(|contribution| { contribution.retriever == RetrieverKind::Lexical })
    );
    assert!(
        selected[1]
            .candidate
            .contributions
            .iter()
            .any(|contribution| { contribution.retriever == RetrieverKind::Graph })
    );

    let mut hydration_budget = fixture.request.budget;
    hydration_budget.max_hydrated_results = 3;
    let mut source = RecordingHydrationSource::default();
    assert!(source.reads.is_empty());
    let page = CanonicalLateHydration::new(&mut source)
        .hydrate(
            &fixture.request,
            &output.ranked_candidates,
            &hydration_budget,
        )
        .expect("selected prefix hydrates");

    assert_eq!(source.reads, expected_reads);
    assert_eq!(page.results.len(), 3);
    assert_eq!(page.receipts.len(), 3);
    assert!(
        output.ranked_candidates[3..]
            .iter()
            .all(|ranked| !source.reads.contains(&ranked.candidate.anchor_id))
    );
}
