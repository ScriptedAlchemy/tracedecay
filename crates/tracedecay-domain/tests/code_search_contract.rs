use std::collections::BTreeMap;
use std::fmt;

use tracedecay_domain::{
    CandidateContribution, CompactCandidate, EvidenceRole, ExactAdmissionProof,
    ExactAdmissionRuleRevision, ExactClass, ExactFieldV1, FixedPointScore,
    FreshnessCompatibilityV1, FusedCandidate, OccurrenceProvenance, RankingDecision,
    RankingDecisionKind, RetrievalAnchorId, RetrievalContractError, RetrieverBatch,
    RetrieverCoverage, RetrieverKind, SourceFreshness, UtcMicros,
};

const ZERO_DIGEST: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

fn freshness() -> SourceFreshness {
    SourceFreshness {
        source_namespace: id("ns.contract"),
        source_instance: id("instance.contract"),
        source_watermark: Some(1),
        projection_watermark: Some(1),
        observed_at: UtcMicros(1),
        source_generation: Some(1),
        generation_lag: Some(0),
        compatibility: FreshnessCompatibilityV1::Current,
        policy_revision: id("policy.contract.v1"),
    }
}

fn proof() -> ExactAdmissionProof {
    ExactAdmissionProof {
        rule_revision: ExactAdmissionRuleRevision::new("exact.contract.v1").unwrap(),
        field: ExactFieldV1::Identifier,
        original_bytes: b"ExactAdmissionProof".to_vec(),
        canonical_bytes: b"ExactAdmissionProof".to_vec(),
        normalization_steps: Vec::new(),
        scope_digest: id(ZERO_DIGEST),
        authorization_revision: id("authorization.contract.v1"),
        snapshot_digest: id(ZERO_DIGEST),
    }
}

fn candidate(
    retriever: RetrieverKind,
    exact_admission_proof: Option<ExactAdmissionProof>,
) -> CompactCandidate {
    CompactCandidate {
        anchor_id: RetrievalAnchorId::new("anchor.contract").unwrap(),
        logical_evidence_id: id("logical.contract"),
        source_occurrence_id: id("occurrence.contract"),
        file_occurrence_id: None,
        source_namespace: id("ns.contract"),
        repository_id: None,
        session_or_thread_id: None,
        logical_copy_cluster_id: None,
        logical_copy_evidence_anchor: None,
        evidence_role: EvidenceRole::Primary,
        retriever,
        retriever_revision: id("retriever.contract.v1"),
        score_domain: id("score.contract.v1"),
        raw_score: FixedPointScore(1),
        ordinal_rank: 0,
        exact_admission_proof,
        retriever_evidence_anchor: RetrievalAnchorId::new("evidence.contract").unwrap(),
        freshness: freshness(),
    }
}

fn provenance(candidate: &CompactCandidate) -> OccurrenceProvenance {
    OccurrenceProvenance {
        source_occurrence_id: candidate.source_occurrence_id.clone(),
        file_occurrence_id: candidate.file_occurrence_id.clone(),
        retriever_evidence_anchor: candidate.retriever_evidence_anchor.clone(),
        source_namespace: candidate.source_namespace.clone(),
        repository_id: candidate.repository_id.clone(),
        session_or_thread_id: candidate.session_or_thread_id.clone(),
        logical_copy_cluster_id: candidate.logical_copy_cluster_id.clone(),
        logical_copy_evidence_anchor: candidate.logical_copy_evidence_anchor.clone(),
        evidence_role: candidate.evidence_role,
        freshness: candidate.freshness.clone(),
    }
}

#[test]
fn only_the_exact_lane_can_attach_a_central_admission_proof() {
    let exact_without_proof = candidate(RetrieverKind::ExactLiteral, None);
    let mut exact_evidence = BTreeMap::new();
    exact_evidence.insert(
        exact_without_proof.source_occurrence_id.clone(),
        provenance(&exact_without_proof),
    );
    let exact_batch = RetrieverBatch {
        candidates: vec![exact_without_proof],
        evidence_by_occurrence: exact_evidence,
        coverage: RetrieverCoverage::default(),
        continuation: None,
    };
    assert_eq!(
        exact_batch.validate(),
        Err(RetrievalContractError::ExactClassWithoutProof)
    );

    let lexical_with_proof = candidate(RetrieverKind::Lexical, Some(proof()));
    let mut lexical_evidence = BTreeMap::new();
    lexical_evidence.insert(
        lexical_with_proof.source_occurrence_id.clone(),
        provenance(&lexical_with_proof),
    );
    let lexical_batch = RetrieverBatch {
        candidates: vec![lexical_with_proof],
        evidence_by_occurrence: lexical_evidence,
        coverage: RetrieverCoverage::default(),
        continuation: None,
    };
    assert_eq!(
        lexical_batch.validate(),
        Err(RetrievalContractError::ExactProofOutsideExactLane)
    );
}

#[test]
fn exact_fusion_requires_an_attributed_admission_decision() {
    let exact = candidate(RetrieverKind::ExactLiteral, Some(proof()));
    let contribution = CandidateContribution {
        retriever: RetrieverKind::ExactLiteral,
        retriever_revision: exact.retriever_revision.clone(),
        source_occurrence_id: exact.source_occurrence_id.clone(),
        ordinal_rank: 0,
        raw_score: exact.raw_score,
        score_domain: exact.score_domain.clone(),
        calibration_profile_id: id("calibration.contract.v1"),
        calibrated_feature_micros: 1,
        weight_micros: 1,
        weighted_contribution_micros: 1,
    };
    let mut fused = FusedCandidate {
        anchor_id: exact.anchor_id.clone(),
        logical_evidence_id: exact.logical_evidence_id.clone(),
        occurrences: vec![provenance(&exact)],
        exact_class: ExactClass::ExactMessage,
        utility_micros: 1,
        contributions: vec![contribution],
        freshness: vec![freshness()],
        decisions: vec![RankingDecision {
            kind: RankingDecisionKind::ExactTierAdmission,
            retriever: None,
            policy_anchor: None,
            evidence_anchor: None,
            detail: "unattributed exact promotion".to_owned(),
        }],
    };
    assert_eq!(
        fused.validate(),
        Err(RetrievalContractError::ExactClassWithoutProof)
    );

    fused.decisions[0].retriever = Some(RetrieverKind::ExactLiteral);
    fused.decisions[0].policy_anchor = Some(RetrievalAnchorId::new("policy.exact.v1").unwrap());
    fused.decisions[0].evidence_anchor = Some(exact.retriever_evidence_anchor.clone());
    fused
        .validate()
        .expect("attributed exact admission validates");

    fused.exact_class = ExactClass::Approximate;
    assert_eq!(
        fused.validate(),
        Err(RetrievalContractError::UnexpectedExactTierAdmission)
    );
}
