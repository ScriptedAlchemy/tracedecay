//! Lexical-lane behavior tests: deterministic fixed-point scoring, field
//! filtering, exact/lexical lane independence, budget cutoffs, typed
//! coverage, and deterministic continuation. Ports are in-memory fakes (the
//! `src/query` test pattern): the store-side posting adapters that implement
//! `LexicalPostingReadPort` against the lexical projection land with the
//! query/i3 composition packet.

use std::collections::BTreeMap;
use std::fmt;

use tracedecay_domain::{
    CompactCandidate, EphemeralSanitizedQueryViewV1, EvidenceRole, ExactAdmissionProof,
    ExactAdmissionRuleRevision, ExactFieldV1, FixedPointScore, FreshnessCompatibilityV1,
    PrincipalId, QueryNormalizationRevision, RetrievalBudget, RetrievalError, RetrievalRequest,
    RetrievalScope, RetrievalSnapshot, Retriever, RetrieverBatch, RetrieverKind, RetrieverOutcome,
    SanitizerRevision, SingleRootScopeV1, SourceFreshness, TemporalModeV1, UtcMicros,
    VectorWatermark,
};

use super::{
    LexicalFieldFilterV1, LexicalFieldV1, LexicalLane, LexicalLaneEvidence, LexicalLaneRequest,
    LexicalLaneRetriever, lexical_query_parts,
};
use crate::retrieval::ports::{
    CodeCandidateBindingV1, CodeOccurrenceRefV1, LexicalPostingReadPort, RetrievalPortError,
};

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

#[test]
fn shared_query_parser_retains_phrase_and_identifier_subtokens() {
    let parts =
        lexical_query_parts("who calls VectorWatermark::merge_max").expect("lexical query parts");

    assert_eq!(
        parts.phrases,
        ["who calls VectorWatermark::merge_max".to_owned()]
    );
    assert!(parts.whole_terms.contains(&"VectorWatermark".to_owned()));
    assert!(parts.subtokens.contains(&"vector".to_owned()));
    assert!(parts.subtokens.contains(&"watermark".to_owned()));
    assert!(parts.subtokens.contains(&"merge".to_owned()));
    assert!(parts.subtokens.contains(&"max".to_owned()));
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

fn freshness() -> SourceFreshness {
    SourceFreshness {
        source_namespace: id("ns.code.fixture"),
        source_instance: id("instance.fixture"),
        source_watermark: Some(7),
        projection_watermark: Some(7),
        observed_at: UtcMicros(7),
        source_generation: Some(1),
        generation_lag: Some(0),
        compatibility: FreshnessCompatibilityV1::Current,
        policy_revision: id("policy.fixture.v1"),
    }
}

fn lexical_request(max_candidates: u32) -> LexicalLaneRequest<'static> {
    let query_view = Box::leak(Box::new(
        EphemeralSanitizedQueryViewV1::sanitize(
            "reserve stock",
            id::<SanitizerRevision>("query-sanitizer.v1"),
            id::<QueryNormalizationRevision>("query-normalization.v1"),
        )
        .expect("query sanitizes"),
    ));
    LexicalLaneRequest {
        base: base_request(max_candidates),
        query_view,
        generation: id("generation.1"),
        whole_terms: vec!["reserve".to_owned(), "stock".to_owned()],
        subtokens: vec!["res".to_owned()],
        phrases: Vec::new(),
        field_filters: Vec::new(),
        fuzzy_budget: 2,
        lexical_profile_revision: id("lexical-profile.v1"),
        score_domain: id("score.lexical.v1"),
        budget: budget(max_candidates),
    }
}

/// Build one lexical candidate/evidence pair with per-field fixed-point
/// micro scores supplied by the (fake) posting port.
fn lexical_pair(
    request: &LexicalLaneRequest<'_>,
    occurrence: &str,
    field_scores: &[(LexicalFieldV1, u64)],
    matched_whole_terms: &[&str],
    matched_subtokens: &[&str],
) -> (CompactCandidate, LexicalLaneEvidence) {
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
        retriever: RetrieverKind::Lexical,
        retriever_revision: id("retriever.lexical.v1"),
        score_domain: request.score_domain.clone(),
        raw_score: FixedPointScore(0),
        ordinal_rank: 0,
        exact_admission_proof: None,
        retriever_evidence_anchor: id(&format!("evidence-anchor.{occurrence}")),
        freshness: freshness(),
    };
    let evidence = LexicalLaneEvidence {
        binding: CodeCandidateBindingV1 {
            candidate_anchor: candidate.anchor_id.clone(),
            occurrence: CodeOccurrenceRefV1 {
                generation: request.generation.clone(),
                file: id(&format!("file.{occurrence}")),
                symbol: Some(id(&format!("symbol.{occurrence}"))),
                chunk: Some(id(&format!("chunk.{occurrence}"))),
            },
            language_descriptor_revision: id("language.rust.v1"),
            matched_term_kinds: Vec::new(),
            source_occurrence: candidate.source_occurrence_id.clone(),
        },
        field_scores_micros: field_scores.to_vec(),
        matched_whole_terms: matched_whole_terms
            .iter()
            .map(|term| (*term).to_owned())
            .collect(),
        matched_subtokens: matched_subtokens
            .iter()
            .map(|subtoken| (*subtoken).to_owned())
            .collect(),
        matched_phrases: Vec::new(),
        typo_recovery_applied: false,
        echo_penalty_applied: false,
    };
    (candidate, evidence)
}

fn batch(
    pairs: Vec<(CompactCandidate, LexicalLaneEvidence)>,
) -> RetrieverBatch<LexicalLaneEvidence> {
    let mut candidates = Vec::new();
    let mut evidence_by_occurrence = BTreeMap::new();
    for (ordinal, (mut candidate, evidence)) in pairs.into_iter().enumerate() {
        candidate.ordinal_rank = ordinal as u32;
        evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
        candidates.push(candidate);
    }
    let examined = candidates.len() as u64;
    RetrieverBatch {
        candidates,
        evidence_by_occurrence,
        coverage: tracedecay_domain::RetrieverCoverage {
            examined,
            eligible: examined,
            excluded: 0,
            capped: 0,
            unknown: 0,
        },
        continuation: None,
    }
}

enum PortReply {
    Complete(RetrieverBatch<LexicalLaneEvidence>),
    Error(RetrievalPortError),
}

struct FakeLexicalPort {
    reply: PortReply,
}

impl FakeLexicalPort {
    fn complete(pairs: Vec<(CompactCandidate, LexicalLaneEvidence)>) -> Self {
        Self {
            reply: PortReply::Complete(batch(pairs)),
        }
    }

    fn error(error: RetrievalPortError) -> Self {
        Self {
            reply: PortReply::Error(error),
        }
    }
}

impl LexicalPostingReadPort for FakeLexicalPort {
    fn read_lexical_postings(
        &self,
        _request: &LexicalLaneRequest<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>, RetrievalPortError> {
        match &self.reply {
            PortReply::Complete(value) => Ok(RetrieverOutcome::Complete(value.clone())),
            PortReply::Error(error) => Err(error.clone()),
        }
    }
}

fn complete_batch(
    outcome: RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>,
) -> RetrieverBatch<LexicalLaneEvidence> {
    match outcome {
        RetrieverOutcome::Complete(value) => value,
        other => panic!("expected a complete lexical batch, got {other:?}"),
    }
}

fn result_order(batch: &RetrieverBatch<LexicalLaneEvidence>, expected: &[&str]) {
    let actual: Vec<&str> = batch
        .candidates
        .iter()
        .map(|candidate| candidate.source_occurrence_id.as_str())
        .collect();
    assert_eq!(actual, expected);
}

fn three_pairs(request: &LexicalLaneRequest<'_>) -> Vec<(CompactCandidate, LexicalLaneEvidence)> {
    vec![
        lexical_pair(
            request,
            "occ.a",
            &[
                (LexicalFieldV1::SymbolName, 900_000),
                (LexicalFieldV1::BodyText, 100_000),
            ],
            &["reserve"],
            &[],
        ),
        lexical_pair(
            request,
            "occ.b",
            &[(LexicalFieldV1::QualifiedName, 800_000)],
            &["stock"],
            &["res"],
        ),
        lexical_pair(
            request,
            "occ.c",
            &[(LexicalFieldV1::Path, 300_000)],
            &[],
            &["res"],
        ),
    ]
}

#[test]
fn lexical_lane_scores_candidates_with_checked_fixed_point_sums() {
    let request = lexical_request(8);
    let lane = LexicalLane::new(FakeLexicalPort::complete(three_pairs(&request)));

    let result = complete_batch(lane.retrieve_lexical(&request).expect("lexical retrieval"));

    // Raw scores are the checked fixed-point sums of admitted field micros:
    // occ.a = 1_000_000, occ.b = 800_000, occ.c = 300_000. No float ever
    // crosses the candidate identity.
    result_order(&result, &["occ.a", "occ.b", "occ.c"]);
    assert_eq!(result.candidates[0].raw_score, FixedPointScore(1_000_000));
    assert_eq!(result.candidates[1].raw_score, FixedPointScore(800_000));
    assert_eq!(result.candidates[2].raw_score, FixedPointScore(300_000));
    for (ordinal, candidate) in result.candidates.iter().enumerate() {
        assert_eq!(candidate.ordinal_rank, ordinal as u32);
        assert_eq!(candidate.retriever, RetrieverKind::Lexical);
        assert!(candidate.exact_admission_proof.is_none());
    }
    assert_eq!(result.coverage.examined, 3);
    assert_eq!(result.coverage.eligible, 3);
    assert_eq!(result.coverage.excluded, 0);
    assert_eq!(result.coverage.capped, 0);
    result.validate().expect("rebuilt batch is valid");
    let continuation = result.continuation.expect("checkpoint emitted");
    assert_eq!(continuation.lane, RetrieverKind::Lexical);
    assert!(continuation.exhausted);
}

#[test]
fn lexical_lane_applies_include_field_filters() {
    let mut request = lexical_request(8);
    request.field_filters = vec![LexicalFieldFilterV1 {
        field: LexicalFieldV1::SymbolName,
        include: true,
    }];
    let lane = LexicalLane::new(FakeLexicalPort::complete(three_pairs(&request)));

    let result = complete_batch(lane.retrieve_lexical(&request).expect("filtered retrieval"));

    // Only occ.a has a SymbolName score; its BodyText micros are dropped
    // (900_000, not 1_000_000) and the other candidates are excluded with
    // typed coverage, never silently.
    result_order(&result, &["occ.a"]);
    assert_eq!(result.candidates[0].raw_score, FixedPointScore(900_000));
    assert_eq!(result.coverage.examined, 3);
    assert_eq!(result.coverage.eligible, 1);
    assert_eq!(result.coverage.excluded, 2);
    assert_eq!(
        result.evidence_by_occurrence[&id::<tracedecay_domain::SourceOccurrenceId>("occ.a")]
            .field_scores_micros,
        vec![(LexicalFieldV1::SymbolName, 900_000)]
    );
}

#[test]
fn lexical_lane_applies_exclude_field_filters() {
    let mut request = lexical_request(8);
    request.field_filters = vec![LexicalFieldFilterV1 {
        field: LexicalFieldV1::BodyText,
        include: false,
    }];
    let lane = LexicalLane::new(FakeLexicalPort::complete(three_pairs(&request)));

    let result = complete_batch(lane.retrieve_lexical(&request).expect("filtered retrieval"));

    // occ.a keeps only its SymbolName micros; every candidate survives
    // because each retains at least one admitted field.
    result_order(&result, &["occ.a", "occ.b", "occ.c"]);
    assert_eq!(result.candidates[0].raw_score, FixedPointScore(900_000));
    assert_eq!(result.coverage.excluded, 0);
}

#[test]
fn lexical_lane_enforces_budget_cutoff_with_typed_coverage_and_deterministic_continuation() {
    let request = lexical_request(2);
    let lane = LexicalLane::new(FakeLexicalPort::complete(three_pairs(&request)));

    let first = complete_batch(lane.retrieve_lexical(&request).expect("first run"));
    let restarted = LexicalLane::new(FakeLexicalPort::complete(three_pairs(&request)));
    let second = complete_batch(restarted.retrieve_lexical(&request).expect("restart run"));

    assert_eq!(
        first, second,
        "cursor bytes and prefix are restart-deterministic"
    );
    result_order(&first, &["occ.a", "occ.b"]);
    assert_eq!(first.coverage.examined, 3);
    assert_eq!(first.coverage.eligible, 3);
    assert_eq!(first.coverage.capped, 1);
    let continuation = first.continuation.expect("checkpoint emitted");
    assert!(!continuation.exhausted);

    // A differently completed prefix commits a different checkpoint digest.
    let wide_request = lexical_request(3);
    let wide = complete_batch(
        LexicalLane::new(FakeLexicalPort::complete(three_pairs(&wide_request)))
            .retrieve_lexical(&wide_request)
            .expect("wide run"),
    );
    assert_ne!(
        continuation.checkpoint_digest,
        wide.continuation
            .expect("wide checkpoint")
            .checkpoint_digest
    );
}

#[test]
fn lexical_lane_prefix_is_canonical_regardless_of_port_order() {
    let request = lexical_request(8);
    let mut reversed = three_pairs(&request);
    reversed.reverse();

    let first = complete_batch(
        LexicalLane::new(FakeLexicalPort::complete(three_pairs(&request)))
            .retrieve_lexical(&request)
            .expect("forward run"),
    );
    let second = complete_batch(
        LexicalLane::new(FakeLexicalPort::complete(reversed))
            .retrieve_lexical(&request)
            .expect("reversed run"),
    );

    assert_eq!(
        first, second,
        "port emission order cannot select a different prefix"
    );
    result_order(&first, &["occ.a", "occ.b", "occ.c"]);
}

#[test]
fn lexical_lane_rejects_exact_proof_smuggling() {
    let request = lexical_request(8);
    let (mut candidate, evidence) = lexical_pair(
        &request,
        "occ.a",
        &[(LexicalFieldV1::SymbolName, 900_000)],
        &["reserve"],
        &[],
    );
    candidate.exact_admission_proof = Some(ExactAdmissionProof {
        rule_revision: id::<ExactAdmissionRuleRevision>("exact-rules.v1"),
        field: ExactFieldV1::Identifier,
        original_bytes: b"reserve".to_vec(),
        canonical_bytes: b"reserve".to_vec(),
        normalization_steps: Vec::new(),
        scope_digest: request.base.scope.compute_digest().expect("scope digest"),
        authorization_revision: request.base.snapshot.authorization_revision.clone(),
        snapshot_digest: request
            .base
            .snapshot
            .compute_digest()
            .expect("snapshot digest"),
    });
    let lane = LexicalLane::new(FakeLexicalPort::complete(vec![(candidate, evidence)]));

    let result = lane.retrieve_lexical(&request);

    assert!(matches!(result, Err(RetrievalPortError::Contract(_))));
}

#[test]
fn lexical_lane_never_admits_an_exact_tier_candidate() {
    let request = lexical_request(8);
    let (mut candidate, evidence) = lexical_pair(
        &request,
        "occ.exact",
        &[(LexicalFieldV1::ExactTerm, 1_000_000)],
        &["reserve"],
        &[],
    );
    // An exact-lane candidate identity can never cross into the lexical
    // lane, even with a top field score.
    candidate.retriever = RetrieverKind::ExactLiteral;
    let lane = LexicalLane::new(FakeLexicalPort::complete(vec![(candidate, evidence)]));

    let result = lane.retrieve_lexical(&request);

    assert!(matches!(result, Err(RetrievalPortError::Contract(_))));
}

#[test]
fn lexical_lane_rejects_matches_outside_the_request_terms() {
    let request = lexical_request(8);
    let (candidate, mut evidence) = lexical_pair(
        &request,
        "occ.a",
        &[(LexicalFieldV1::SymbolName, 900_000)],
        &["reserve"],
        &[],
    );
    evidence.matched_whole_terms.push("smuggled".to_owned());
    let lane = LexicalLane::new(FakeLexicalPort::complete(vec![(candidate, evidence)]));

    let result = lane.retrieve_lexical(&request);

    assert!(matches!(result, Err(RetrievalPortError::Contract(_))));
}

#[test]
fn lexical_lane_rejects_duplicate_field_scores() {
    let request = lexical_request(8);
    let (candidate, evidence) = lexical_pair(
        &request,
        "occ.a",
        &[
            (LexicalFieldV1::SymbolName, 900_000),
            (LexicalFieldV1::SymbolName, 100_000),
        ],
        &["reserve"],
        &[],
    );
    let lane = LexicalLane::new(FakeLexicalPort::complete(vec![(candidate, evidence)]));

    let result = lane.retrieve_lexical(&request);

    assert!(matches!(result, Err(RetrievalPortError::Contract(_))));
}

#[test]
fn lexical_lane_rejects_generation_mismatched_evidence() {
    let request = lexical_request(8);
    let (candidate, mut evidence) = lexical_pair(
        &request,
        "occ.a",
        &[(LexicalFieldV1::SymbolName, 900_000)],
        &["reserve"],
        &[],
    );
    evidence.binding.occurrence.generation = id("generation.2");
    let lane = LexicalLane::new(FakeLexicalPort::complete(vec![(candidate, evidence)]));

    let result = lane.retrieve_lexical(&request);

    assert_eq!(result, Err(RetrievalPortError::GenerationMismatch));
}

#[test]
fn lexical_lane_reports_unavailable_when_authority_is_missing() {
    let request = lexical_request(8);
    let lane = LexicalLane::new(FakeLexicalPort::error(
        RetrievalPortError::AuthorityUnavailable("lexical postings are not published".to_owned()),
    ));

    let outcome = lane.retrieve_lexical(&request).expect("typed outcome");

    assert!(matches!(
        outcome,
        RetrieverOutcome::Unavailable(
            tracedecay_domain::RetrievalFailure::AuthorityUnavailable { .. }
        )
    ));
}

#[test]
fn lexical_lane_requires_at_least_one_term() {
    let mut request = lexical_request(8);
    request.whole_terms = Vec::new();
    request.subtokens = Vec::new();
    let lane = LexicalLane::new(FakeLexicalPort::complete(Vec::new()));

    let result = lane.retrieve_lexical(&request);

    assert!(matches!(result, Err(RetrievalPortError::Contract(_))));
}

#[test]
fn lexical_lane_satisfies_the_generic_retriever_contract() {
    let request = lexical_request(8);
    let lane = LexicalLane::new(FakeLexicalPort::complete(three_pairs(&request)));

    let outcome = Retriever::retrieve(&lane, &request).expect("generic retrieval succeeds");
    assert_eq!(complete_batch(outcome).candidates.len(), 3);

    let mut invalid = lexical_request(8);
    invalid.budget.max_candidates_per_lane = 0;
    let error = Retriever::retrieve(&lane, &invalid).expect_err("invalid budget is rejected");
    assert!(matches!(error, RetrievalError::InvalidRequest(_)));
}
