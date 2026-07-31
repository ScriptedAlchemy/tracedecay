//! Exact-lane behavior tests: admission-proof enforcement, exact/lexical
//! lane independence, budget cutoffs, typed coverage, and deterministic
//! continuation. Ports are in-memory fakes (the `src/query` test pattern):
//! the store-side posting adapters that implement `ExactTermPostingReadPort`
//! against the lexical projection land with the query/i3 composition packet.

use std::collections::BTreeMap;
use std::fmt;

use tracedecay_domain::{
    CompactCandidate, EphemeralSanitizedQueryViewV1, EvidenceRole, ExactAdmissionProof,
    ExactAdmissionRuleRevision, ExactAdmissionValidator, ExactFieldV1, FixedPointScore,
    FreshnessCompatibilityV1, PrincipalId, QueryNormalizationRevision, RetrievalBudget,
    RetrievalError, RetrievalRequest, RetrievalScope, RetrievalSnapshot, Retriever, RetrieverBatch,
    RetrieverKind, RetrieverOutcome, SanitizerRevision, SingleRootScopeV1, SourceFreshness,
    TemporalModeV1, UtcMicros, VectorWatermark,
};

use super::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLane, ExactLaneEvidence,
    ExactLaneRequest, ExactLaneRetriever, ExactLiteralV1,
};
use crate::retrieval::ports::{
    CodeCandidateBindingV1, CodeOccurrenceRefV1, ExactTermPostingReadPort, RetrievalPortError,
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

/// Deterministic test grammar for the central admission authority: quoted
/// tokens become phrases, `--` tokens become CLI flags, everything else is an
/// identifier. Canonical bytes are ASCII-lowercased.
fn parse_query_literals(query: &str) -> Vec<ExactLiteralV1> {
    query
        .split_whitespace()
        .map(|token| {
            let (field, text) = if token.len() > 2 && token.starts_with('"') && token.ends_with('"')
            {
                (ExactFieldV1::QuotedPhrase, &token[1..token.len() - 1])
            } else if let Some(flag) = token.strip_prefix("--") {
                (ExactFieldV1::CliFlag, flag)
            } else {
                (ExactFieldV1::Identifier, token)
            };
            ExactLiteralV1 {
                field,
                original_bytes: text.as_bytes().to_vec(),
                canonical_bytes: text.bytes().map(|byte| byte.to_ascii_lowercase()).collect(),
            }
        })
        .collect()
}

/// The test double for the central exact-admission authority. It is the only
/// proof minter in these fixtures; the lane can never construct a proof.
struct FixtureAuthority {
    rule_revision: ExactAdmissionRuleRevision,
}

impl FixtureAuthority {
    fn new() -> Self {
        Self {
            rule_revision: id("exact-rules.v1"),
        }
    }
}

impl ExactAdmissionValidator for FixtureAuthority {
    fn admit(
        &self,
        field: ExactFieldV1,
        candidate_bytes: &[u8],
        request: &RetrievalRequest,
    ) -> Result<Option<ExactAdmissionProof>, RetrievalError> {
        if candidate_bytes == b"authority-rejected" {
            return Ok(None);
        }
        let canonical_bytes: Vec<u8> = candidate_bytes.iter().map(u8::to_ascii_lowercase).collect();
        let normalization_steps = if canonical_bytes == candidate_bytes {
            Vec::new()
        } else {
            vec!["ascii_lowercase".to_owned()]
        };
        Ok(Some(ExactAdmissionProof {
            rule_revision: self.rule_revision.clone(),
            field,
            original_bytes: candidate_bytes.to_vec(),
            canonical_bytes,
            normalization_steps,
            scope_digest: request.scope.compute_digest()?,
            authorization_revision: request.snapshot.authorization_revision.clone(),
            snapshot_digest: request.snapshot.compute_digest()?,
        }))
    }
}

impl ExactAdmissionAuthority for FixtureAuthority {
    fn parse_literals(
        &self,
        query_view: &EphemeralSanitizedQueryViewV1,
        _request: &RetrievalRequest,
    ) -> Vec<ExactLiteralV1> {
        parse_query_literals(query_view.as_str())
    }
}

fn exact_request(
    authority: &FixtureAuthority,
    query: &str,
    max_candidates: u32,
) -> ExactLaneRequest<'static> {
    let base = base_request(max_candidates);
    let query_view = Box::leak(Box::new(
        EphemeralSanitizedQueryViewV1::sanitize(
            query,
            id::<SanitizerRevision>("query-sanitizer.v1"),
            id::<QueryNormalizationRevision>("query-normalization.v1"),
        )
        .expect("query sanitizes"),
    ));
    ExactLaneRequest {
        literals: authority.parse_literals(query_view, &base),
        base,
        query_view,
        generation: id("generation.1"),
        budget: budget(max_candidates),
    }
}

#[test]
fn central_authority_admits_unprefixed_contextual_error_text() {
    let authority = CentralExactAdmissionAuthorityV1::new(id("exact-rules.v1"));
    let query_view = EphemeralSanitizedQueryViewV1::sanitize(
        "time interval start must not be after its end",
        id::<SanitizerRevision>("query-sanitizer.v1"),
        id::<QueryNormalizationRevision>("query-normalization.v1"),
    )
    .expect("query sanitizes");

    let literals = authority.parse_literals(&query_view, &base_request(16));

    assert!(literals.iter().any(|literal| {
        literal.field == ExactFieldV1::CompilerOrRuntimeError
            && literal.original_bytes == b"time interval start must not be after its end"
    }));
}

#[test]
fn central_authority_does_not_promote_unprefixed_natural_language() {
    let authority = CentralExactAdmissionAuthorityV1::new(id("exact-rules.v1"));
    let query_view = EphemeralSanitizedQueryViewV1::sanitize(
        "who writes to the config file",
        id::<SanitizerRevision>("query-sanitizer.v1"),
        id::<QueryNormalizationRevision>("query-normalization.v1"),
    )
    .expect("query sanitizes");

    assert!(
        authority
            .parse_literals(&query_view, &base_request(16))
            .iter()
            .all(|literal| literal.field != ExactFieldV1::CompilerOrRuntimeError)
    );
}

/// Build one exact candidate/evidence pair whose proof is minted by the
/// central authority for `request.literals[literal_index]`.
fn exact_pair(
    authority: &FixtureAuthority,
    request: &ExactLaneRequest<'_>,
    occurrence: &str,
    literal_index: usize,
) -> (CompactCandidate, ExactLaneEvidence) {
    let literal = request.literals[literal_index].clone();
    let proof = authority
        .admit(literal.field, &literal.original_bytes, &request.base)
        .expect("admission succeeds")
        .expect("authority mints a proof");
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
        retriever: RetrieverKind::ExactLiteral,
        retriever_revision: id("retriever.exact.v1"),
        score_domain: id("score.exact.v1"),
        raw_score: FixedPointScore(0),
        ordinal_rank: 0,
        exact_admission_proof: Some(proof.clone()),
        retriever_evidence_anchor: id(&format!("evidence-anchor.{occurrence}")),
        freshness: freshness(),
    };
    let evidence = ExactLaneEvidence {
        binding: CodeCandidateBindingV1 {
            candidate_anchor: candidate.anchor_id.clone(),
            occurrence: CodeOccurrenceRefV1 {
                generation: request.generation.clone(),
                file: id(&format!("file.{occurrence}")),
                symbol: Some(id(&format!("symbol.{occurrence}"))),
                chunk: Some(id(&format!("chunk.{occurrence}"))),
            },
            language_descriptor_revision: id("language.rust.v1"),
            matched_term_kinds: vec![tracedecay_domain::ExactTechnicalTermKindV1::WholeSymbol],
            source_occurrence: candidate.source_occurrence_id.clone(),
        },
        matched_literals: vec![literal],
        admission_proof: proof,
    };
    (candidate, evidence)
}

fn batch(pairs: Vec<(CompactCandidate, ExactLaneEvidence)>) -> RetrieverBatch<ExactLaneEvidence> {
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
    Complete(RetrieverBatch<ExactLaneEvidence>),
    Error(RetrievalPortError),
}

struct FakeExactPort {
    reply: PortReply,
}

impl FakeExactPort {
    fn complete(pairs: Vec<(CompactCandidate, ExactLaneEvidence)>) -> Self {
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

impl ExactTermPostingReadPort for FakeExactPort {
    fn read_exact_postings(
        &self,
        _request: &ExactLaneRequest<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>, RetrievalPortError> {
        match &self.reply {
            PortReply::Complete(value) => Ok(RetrieverOutcome::Complete(value.clone())),
            PortReply::Error(error) => Err(error.clone()),
        }
    }
}

fn complete_batch(
    outcome: RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>,
) -> RetrieverBatch<ExactLaneEvidence> {
    match outcome {
        RetrieverOutcome::Complete(value) => value,
        other => panic!("expected a complete exact batch, got {other:?}"),
    }
}

#[test]
fn exact_lane_admits_only_authority_minted_proofs() {
    let authority = FixtureAuthority::new();
    let request = exact_request(&authority, "reserve_stock --force", 8);
    let port = FakeExactPort::complete(vec![
        exact_pair(&authority, &request, "occ.a", 0),
        exact_pair(&authority, &request, "occ.b", 1),
    ]);
    let lane = ExactLane::new(authority, port);

    let result = complete_batch(
        lane.retrieve_exact(&request)
            .expect("exact retrieval succeeds"),
    );

    assert_eq!(result.candidates.len(), 2);
    for (ordinal, candidate) in result.candidates.iter().enumerate() {
        assert_eq!(candidate.ordinal_rank, ordinal as u32);
        assert_eq!(candidate.retriever, RetrieverKind::ExactLiteral);
        let proof = candidate
            .exact_admission_proof
            .as_ref()
            .expect("exact candidates carry proofs");
        proof
            .validate_for_request(&request.base)
            .expect("proof binds the request");
        assert_eq!(candidate.raw_score, FixedPointScore(1_000_000));
    }
    assert_eq!(result.coverage.eligible, 2);
    assert_eq!(result.coverage.examined, 2);
    assert_eq!(result.coverage.capped, 0);
    result.validate().expect("rebuilt batch is valid");
    let continuation = result.continuation.expect("checkpoint emitted");
    assert_eq!(continuation.lane, RetrieverKind::ExactLiteral);
    assert!(continuation.exhausted);
}

#[test]
fn exact_lane_rejects_a_candidate_without_proof() {
    let authority = FixtureAuthority::new();
    let request = exact_request(&authority, "reserve_stock", 8);
    let (mut candidate, evidence) = exact_pair(&authority, &request, "occ.a", 0);
    candidate.exact_admission_proof = None;
    let port = FakeExactPort::complete(vec![(candidate, evidence)]);
    let lane = ExactLane::new(authority, port);

    let result = lane.retrieve_exact(&request);

    assert!(matches!(result, Err(RetrievalPortError::Contract(_))));
}

#[test]
fn exact_lane_rejects_a_forged_admission_proof() {
    let authority = FixtureAuthority::new();
    let request = exact_request(&authority, "reserve_stock", 8);
    let (mut candidate, mut evidence) = exact_pair(&authority, &request, "occ.a", 0);
    let mut forged = evidence.admission_proof.clone();
    forged.normalization_steps = vec!["forged_step".to_owned()];
    candidate.exact_admission_proof = Some(forged.clone());
    evidence.admission_proof = forged;
    let port = FakeExactPort::complete(vec![(candidate, evidence)]);
    let lane = ExactLane::new(authority, port);

    let result = lane.retrieve_exact(&request);

    assert!(matches!(result, Err(RetrievalPortError::Contract(_))));
}

#[test]
fn exact_lane_rejects_a_literal_the_authority_refuses() {
    let authority = FixtureAuthority::new();
    let request = exact_request(&authority, "authority-rejected", 8);
    // Mint a proof for a different query, then rebind its bytes to the
    // refused literal so every shape check passes until re-admission.
    let minting_request = exact_request(&authority, "reserve_stock", 8);
    let (_, mut evidence) = exact_pair(&authority, &minting_request, "occ.a", 0);
    let literal = request.literals[0].clone();
    evidence.admission_proof.original_bytes = literal.original_bytes.clone();
    evidence.admission_proof.canonical_bytes = literal.canonical_bytes.clone();
    evidence.matched_literals = vec![literal];
    let mut candidate = exact_pair(&authority, &minting_request, "occ.a", 0).0;
    candidate.exact_admission_proof = Some(evidence.admission_proof.clone());
    let port = FakeExactPort::complete(vec![(candidate, evidence)]);
    let lane = ExactLane::new(authority, port);

    let result = lane.retrieve_exact(&request);

    assert!(matches!(result, Err(RetrievalPortError::Contract(_))));
}

#[test]
fn exact_lane_never_admits_a_lexical_only_candidate() {
    let authority = FixtureAuthority::new();
    let request = exact_request(&authority, "reserve_stock", 8);
    // A BM25-only match has no exact admission proof and a lexical lane
    // identity; it can never enter the exact lane.
    let (mut candidate, evidence) = exact_pair(&authority, &request, "occ.bm25", 0);
    candidate.retriever = RetrieverKind::Lexical;
    candidate.exact_admission_proof = None;
    let port = FakeExactPort::complete(vec![(candidate, evidence)]);
    let lane = ExactLane::new(authority, port);

    let result = lane.retrieve_exact(&request);

    assert!(matches!(result, Err(RetrievalPortError::Contract(_))));
}

#[test]
fn exact_lane_rejects_literals_not_parsed_by_the_central_authority() {
    let authority = FixtureAuthority::new();
    let mut request = exact_request(&authority, "reserve_stock", 8);
    request.literals.push(ExactLiteralV1 {
        field: ExactFieldV1::Identifier,
        original_bytes: b"smuggled".to_vec(),
        canonical_bytes: b"smuggled".to_vec(),
    });
    let port = FakeExactPort::complete(Vec::new());
    let lane = ExactLane::new(authority, port);

    let result = lane.retrieve_exact(&request);

    assert!(matches!(result, Err(RetrievalPortError::Contract(_))));
}

#[test]
fn exact_lane_rejects_generation_mismatched_evidence() {
    let authority = FixtureAuthority::new();
    let request = exact_request(&authority, "reserve_stock", 8);
    let (candidate, mut evidence) = exact_pair(&authority, &request, "occ.a", 0);
    evidence.binding.occurrence.generation = id("generation.2");
    let port = FakeExactPort::complete(vec![(candidate, evidence)]);
    let lane = ExactLane::new(authority, port);

    let result = lane.retrieve_exact(&request);

    assert_eq!(result, Err(RetrievalPortError::GenerationMismatch));
}

#[test]
fn exact_lane_rejects_matches_outside_the_request_literals() {
    let authority = FixtureAuthority::new();
    let request = exact_request(&authority, "reserve_stock", 8);
    let (candidate, mut evidence) = exact_pair(&authority, &request, "occ.a", 0);
    let outside = ExactLiteralV1 {
        field: ExactFieldV1::Identifier,
        original_bytes: b"outside".to_vec(),
        canonical_bytes: b"outside".to_vec(),
    };
    evidence.matched_literals = vec![evidence.matched_literals[0].clone(), outside];
    let port = FakeExactPort::complete(vec![(candidate, evidence)]);
    let lane = ExactLane::new(authority, port);

    let result = lane.retrieve_exact(&request);

    assert!(matches!(result, Err(RetrievalPortError::Contract(_))));
}

/// Extend a pair with an additional matched literal so matched-literal count
/// ordering is observable.
fn with_extra_literal(
    pair: (CompactCandidate, ExactLaneEvidence),
    request: &ExactLaneRequest<'_>,
    literal_index: usize,
) -> (CompactCandidate, ExactLaneEvidence) {
    let (candidate, mut evidence) = pair;
    evidence
        .matched_literals
        .push(request.literals[literal_index].clone());
    (candidate, evidence)
}

#[test]
fn exact_lane_enforces_budget_cutoff_with_typed_coverage_and_deterministic_continuation() {
    let authority = FixtureAuthority::new();
    let request = exact_request(&authority, "reserve_stock --force", 2);
    let pairs = vec![
        exact_pair(&authority, &request, "occ.a", 0),
        with_extra_literal(exact_pair(&authority, &request, "occ.b", 1), &request, 0),
        exact_pair(&authority, &request, "occ.c", 0),
    ];
    let port = FakeExactPort::complete(pairs.clone());
    let lane = ExactLane::new(FixtureAuthority::new(), port);

    let first = complete_batch(lane.retrieve_exact(&request).expect("first run"));
    let restarted = ExactLane::new(FixtureAuthority::new(), FakeExactPort::complete(pairs));
    let second = complete_batch(restarted.retrieve_exact(&request).expect("restart run"));

    assert_eq!(
        first, second,
        "cursor bytes and prefix are restart-deterministic"
    );
    assert_eq!(first.candidates.len(), 2);
    assert_eq!(first.coverage.examined, 3);
    assert_eq!(first.coverage.eligible, 3);
    assert_eq!(first.coverage.capped, 1);
    // occ.b matched both literals (2.0 fixed-point) and outranks the
    // single-literal occurrences; ties break on stable occurrence identity.
    result_order(&first, &["occ.b", "occ.a"]);
    assert_eq!(first.candidates[0].raw_score, FixedPointScore(2_000_000));
    let continuation = first.continuation.expect("checkpoint emitted");
    assert!(!continuation.exhausted);

    // A differently completed prefix commits a different checkpoint digest.
    let wider_request = exact_request(&FixtureAuthority::new(), "reserve_stock --force", 3);
    let wide = complete_batch(
        ExactLane::new(
            FixtureAuthority::new(),
            FakeExactPort::complete(vec![
                exact_pair(&FixtureAuthority::new(), &wider_request, "occ.a", 0),
                with_extra_literal(
                    exact_pair(&FixtureAuthority::new(), &wider_request, "occ.b", 1),
                    &wider_request,
                    0,
                ),
                exact_pair(&FixtureAuthority::new(), &wider_request, "occ.c", 0),
            ]),
        )
        .retrieve_exact(&wider_request)
        .expect("wide run"),
    );
    assert_ne!(
        continuation.checkpoint_digest,
        wide.continuation
            .expect("wide checkpoint")
            .checkpoint_digest
    );
}

fn result_order(batch: &RetrieverBatch<ExactLaneEvidence>, expected: &[&str]) {
    let actual: Vec<&str> = batch
        .candidates
        .iter()
        .map(|candidate| candidate.source_occurrence_id.as_str())
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn exact_lane_prefix_is_canonical_regardless_of_port_order() {
    let authority = FixtureAuthority::new();
    let request = exact_request(&authority, "reserve_stock --force", 8);
    let forward = vec![
        exact_pair(&authority, &request, "occ.a", 0),
        with_extra_literal(exact_pair(&authority, &request, "occ.b", 1), &request, 0),
        exact_pair(&authority, &request, "occ.c", 0),
    ];
    let mut reversed = forward.clone();
    reversed.reverse();

    let first = complete_batch(
        ExactLane::new(FixtureAuthority::new(), FakeExactPort::complete(forward))
            .retrieve_exact(&request)
            .expect("forward run"),
    );
    let second = complete_batch(
        ExactLane::new(FixtureAuthority::new(), FakeExactPort::complete(reversed))
            .retrieve_exact(&request)
            .expect("reversed run"),
    );

    assert_eq!(
        first, second,
        "port emission order cannot select a different prefix"
    );
    // occ.b matched both literals and ranks first; the single-literal
    // occurrences order by stable occurrence identity.
    result_order(&first, &["occ.b", "occ.a", "occ.c"]);
}

#[test]
fn exact_lane_reports_unavailable_when_authority_is_missing() {
    let authority = FixtureAuthority::new();
    let request = exact_request(&authority, "reserve_stock", 8);
    let lane = ExactLane::new(
        authority,
        FakeExactPort::error(RetrievalPortError::AuthorityUnavailable(
            "exact postings are not published".to_owned(),
        )),
    );

    let outcome = lane.retrieve_exact(&request).expect("typed outcome");

    assert!(matches!(
        outcome,
        RetrieverOutcome::Unavailable(
            tracedecay_domain::RetrievalFailure::AuthorityUnavailable { .. }
        )
    ));
}

#[test]
fn exact_lane_satisfies_the_generic_retriever_contract() {
    let authority = FixtureAuthority::new();
    let request = exact_request(&authority, "reserve_stock", 8);
    let port = FakeExactPort::complete(vec![exact_pair(&authority, &request, "occ.a", 0)]);
    let lane = ExactLane::new(authority, port);

    let outcome = Retriever::retrieve(&lane, &request).expect("generic retrieval succeeds");
    assert_eq!(complete_batch(outcome).candidates.len(), 1);

    let mut invalid = exact_request(&FixtureAuthority::new(), "reserve_stock", 8);
    invalid.budget.max_candidates_per_lane = 0;
    let error = Retriever::retrieve(&lane, &invalid).expect_err("invalid budget is rejected");
    assert!(matches!(error, RetrievalError::InvalidRequest(_)));
}

#[test]
fn exact_lane_request_rejects_duplicate_literals() {
    let authority = FixtureAuthority::new();
    let mut request = exact_request(&authority, "reserve_stock", 8);
    let duplicate = request.literals[0].clone();
    request.literals.push(duplicate);

    assert!(matches!(
        request.validate(),
        Err(RetrievalPortError::Contract(_))
    ));
}
