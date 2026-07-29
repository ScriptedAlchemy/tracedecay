use std::collections::BTreeMap;
use std::fmt;

use tracedecay::query::retrieval::exact::{ExactLaneEvidence, ExactLaneRequest, ExactLiteralV1};
use tracedecay::query::retrieval::ports::{
    CodeCandidateBindingV1, CodeOccurrenceRefV1, CompactCandidateEmission, EmitsCompactCandidates,
    RetrievalPortError,
};
use tracedecay_domain::{
    AuthorizationRevision, CodeGenerationId, CompactCandidate, EphemeralSanitizedQueryViewV1,
    ExactAdmissionProof, ExactAdmissionRuleRevision, ExactFieldV1, ExactTechnicalTermKindV1,
    FreshnessVectorDigest, PrincipalId, PrivacyDomainId, QueryNormalizationRevision, RepositoryId,
    RetrievalAnchorId, RetrievalBudget, RetrievalContractError, RetrievalRequest, RetrievalScope,
    RetrievalSnapshot, SanitizerRevision, SingleRootScopeV1, SourceOccurrenceId, TemporalModeV1,
    UtcMicros, VectorWatermark,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

fn request_and_proof() -> (ExactLaneRequest<'static>, ExactAdmissionProof) {
    let scope = RetrievalScope {
        privacy_domain: PrivacyDomainId::new("privacy.contract").unwrap(),
        root: SingleRootScopeV1 {
            repository: RepositoryId::new("repository.contract").unwrap(),
            worktree: None,
            reference: None,
        },
    };
    let authorization_revision = AuthorizationRevision::new("authorization.contract.v1").unwrap();
    let snapshot = RetrievalSnapshot {
        watermarks: VectorWatermark {
            components: BTreeMap::new(),
        },
        freshness_digest: FreshnessVectorDigest::new(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap(),
        authorization_revision: authorization_revision.clone(),
        captured_at: UtcMicros(1),
    };
    let scope_digest = scope.compute_digest().unwrap();
    let snapshot_digest = snapshot.compute_digest().unwrap();
    let budget = RetrievalBudget {
        max_candidates_per_lane: 10,
        max_fused_candidates: 10,
        max_hydrated_results: 5,
        max_hydration_bytes: 4096,
        deadline_micros: None,
    };
    let base = RetrievalRequest {
        principal: PrincipalId::new("principal.contract").unwrap(),
        scope,
        temporal_mode: TemporalModeV1::Current,
        snapshot,
        profile_id: id("profile.contract.v1"),
        budget,
    };
    let literal = ExactLiteralV1 {
        field: ExactFieldV1::Identifier,
        original_bytes: b"ExactAdmissionProof".to_vec(),
        canonical_bytes: b"ExactAdmissionProof".to_vec(),
    };
    let proof = ExactAdmissionProof {
        rule_revision: ExactAdmissionRuleRevision::new("exact.contract.v1").unwrap(),
        field: literal.field,
        original_bytes: literal.original_bytes.clone(),
        canonical_bytes: literal.canonical_bytes.clone(),
        normalization_steps: Vec::new(),
        scope_digest,
        authorization_revision,
        snapshot_digest,
    };
    let query_view = Box::leak(Box::new(
        EphemeralSanitizedQueryViewV1::sanitize(
            "ExactAdmissionProof",
            SanitizerRevision::new("query-sanitizer.contract.v1").unwrap(),
            QueryNormalizationRevision::new("query-normalization.contract.v1").unwrap(),
        )
        .expect("query sanitizes"),
    ));
    (
        ExactLaneRequest {
            base,
            query_view,
            generation: CodeGenerationId::new("generation.contract").unwrap(),
            literals: vec![literal],
            budget,
        },
        proof,
    )
}

#[test]
fn exact_proofs_bind_scope_authorization_and_snapshot() {
    let (mut request, proof) = request_and_proof();
    proof
        .validate_for_request(&request.base)
        .expect("proof matches the frozen request");

    request.base.snapshot.authorization_revision =
        AuthorizationRevision::new("authorization.other.v1").unwrap();
    assert_eq!(
        proof.validate_for_request(&request.base),
        Err(RetrievalContractError::InvalidExactAdmissionBinding {
            field: "authorization revision",
        })
    );
}

#[test]
fn exact_evidence_matches_the_frozen_generation_and_literal() {
    let (request, proof) = request_and_proof();
    let literal = request.literals[0].clone();
    let mut evidence = ExactLaneEvidence {
        binding: CodeCandidateBindingV1 {
            candidate_anchor: RetrievalAnchorId::new("anchor.contract").unwrap(),
            occurrence: CodeOccurrenceRefV1 {
                generation: request.generation.clone(),
                file: id("file.contract"),
                symbol: Some(id("symbol.contract")),
                chunk: Some(id("chunk.contract")),
            },
            language_descriptor_revision: id("descriptor.contract.v1"),
            matched_term_kinds: vec![ExactTechnicalTermKindV1::WholeSymbol],
            source_occurrence: SourceOccurrenceId::new("occurrence.contract").unwrap(),
        },
        matched_literals: vec![literal],
        admission_proof: proof,
    };
    evidence
        .validate(&request)
        .expect("matching exact evidence validates");

    evidence.binding.occurrence.generation = CodeGenerationId::new("generation.other").unwrap();
    assert_eq!(
        evidence.validate(&request),
        Err(RetrievalPortError::GenerationMismatch)
    );
}

#[test]
fn code_lanes_emit_only_the_shared_compact_candidate() {
    fn assert_shared_candidate<T>()
    where
        T: EmitsCompactCandidates<Candidate = CompactCandidate>,
    {
    }

    assert_shared_candidate::<CompactCandidateEmission>();
}
