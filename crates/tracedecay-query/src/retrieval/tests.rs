mod authority;
mod composition;
mod composition_order;
mod cursor;
mod diversity_caps;
mod evidence_lanes;
mod request;
mod rerank;

use std::collections::BTreeMap;
use std::fmt;

use tracedecay_domain::{
    CalibrationProfileId, CompactCandidate, DiversityPolicy, EvidenceRole, ExactAdmissionProof,
    ExactAdmissionRuleRevision, ExactFieldV1, FixedPointScore, FreshnessCompatibilityV1,
    FusionProfile, PrincipalId, RetrievalAnchorId, RetrievalBudget, RetrievalRequest,
    RetrievalScope, RetrievalSnapshot, RetrieverBatch, RetrieverCoverage, RetrieverKind,
    RetrieverOutcome, ScoreDomainCalibrationV1, SingleRootScopeV1, SourceFreshness, TemporalModeV1,
    UtcMicros, VectorWatermark,
};

use super::fusion::CompositionLaneInput;

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

fn budget() -> RetrievalBudget {
    RetrievalBudget {
        max_candidates_per_lane: 32,
        max_fused_candidates: 16,
        max_hydrated_results: 8,
        max_hydration_bytes: 65_536,
        deadline_micros: None,
    }
}

fn request() -> RetrievalRequest {
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
        budget: budget(),
    }
}

fn profile() -> FusionProfile {
    let lanes = RetrieverKind::QUERY_FALLBACK_LANES;
    FusionProfile {
        profile_id: id("profile.fixture.v1"),
        evaluation_result_anchor: RetrievalAnchorId::new("evaluation.fixture").unwrap(),
        calibrations: lanes
            .into_iter()
            .map(|lane| {
                (
                    lane,
                    id::<CalibrationProfileId>(&format!("calibration.{}.v1", lane.as_str())),
                )
            })
            .collect(),
        score_domain_calibrations: RetrieverKind::QUERY_FALLBACK_LANES
            .into_iter()
            .map(|lane| {
                let score_domain: tracedecay_domain::ScoreDomainId =
                    id(&format!("score.{}.v1", lane.as_str()));
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
        minimum_calibrated_feature_micros: BTreeMap::new(),
        weights_micros: [
            (RetrieverKind::ExactLiteral, 1_000_000),
            (RetrieverKind::Lexical, 500_000),
            (RetrieverKind::Graph, 250_000),
        ]
        .into_iter()
        .collect(),
        diversity_policy_id: id("diversity.fixture.v1"),
        rerank_policy_id: None,
        retrieval_budget: budget(),
    }
}

fn no_caps() -> DiversityPolicy {
    DiversityPolicy {
        policy_id: id("diversity.fixture.v1"),
        evaluation_result_anchor: Some(RetrievalAnchorId::new("evaluation.fixture").unwrap()),
        per_source_namespace: None,
        per_source_instance: None,
        per_repository: None,
        per_file: None,
        per_session_or_thread: None,
        per_copy_cluster: None,
        per_evidence_role: None,
    }
}

fn freshness(namespace: &str, instance: &str) -> SourceFreshness {
    SourceFreshness {
        source_namespace: id(namespace),
        source_instance: id(instance),
        source_watermark: Some(7),
        projection_watermark: Some(7),
        observed_at: UtcMicros(7),
        source_generation: Some(1),
        generation_lag: Some(0),
        compatibility: FreshnessCompatibilityV1::Current,
        policy_revision: id("policy.fixture.v1"),
    }
}

fn candidate(
    lane: RetrieverKind,
    name: &str,
    raw_score_micros: u64,
    ordinal_rank: u32,
) -> CompactCandidate {
    CompactCandidate {
        anchor_id: RetrievalAnchorId::new(format!("anchor.{name}")).unwrap(),
        logical_evidence_id: id(&format!("logical.{name}")),
        source_occurrence_id: id(&format!("occurrence.{lane:?}.{name}")),
        file_occurrence_id: None,
        source_namespace: id("namespace.code"),
        repository_id: Some(id("repository.fixture")),
        session_or_thread_id: None,
        logical_copy_cluster_id: None,
        logical_copy_evidence_anchor: None,
        evidence_role: EvidenceRole::Primary,
        retriever: lane,
        retriever_revision: id(&format!("retriever.{}.v1", lane.as_str())),
        score_domain: id(&format!("score.{}.v1", lane.as_str())),
        raw_score: FixedPointScore(raw_score_micros),
        ordinal_rank,
        exact_admission_proof: None,
        retriever_evidence_anchor: RetrievalAnchorId::new(format!("evidence.{lane:?}.{name}"))
            .unwrap(),
        freshness: freshness("namespace.code", &format!("file.{name}")),
    }
}

fn exact_candidate(name: &str, raw_score_micros: u64) -> CompactCandidate {
    let mut candidate = candidate(RetrieverKind::ExactLiteral, name, raw_score_micros, 0);
    candidate.exact_admission_proof = Some(ExactAdmissionProof {
        rule_revision: id::<ExactAdmissionRuleRevision>("exact-rules.v1"),
        field: ExactFieldV1::Identifier,
        original_bytes: name.as_bytes().to_vec(),
        canonical_bytes: name.as_bytes().to_vec(),
        normalization_steps: Vec::new(),
        scope_digest: digest_id('1'),
        authorization_revision: id("authorization.v1"),
        snapshot_digest: digest_id('2'),
    });
    candidate
}

fn batch<E: Clone>(candidates: Vec<CompactCandidate>, evidence: E) -> RetrieverBatch<E> {
    let evidence_by_occurrence = candidates
        .iter()
        .map(|candidate| (candidate.source_occurrence_id.clone(), evidence.clone()))
        .collect::<BTreeMap<_, _>>();
    RetrieverBatch {
        candidates,
        evidence_by_occurrence,
        coverage: RetrieverCoverage::default(),
        continuation: None,
    }
}

fn lane<E>(
    lane: RetrieverKind,
    outcome: RetrieverOutcome<RetrieverBatch<E>>,
) -> CompositionLaneInput {
    CompositionLaneInput::new(lane, outcome).expect("lane input is valid")
}

fn composition_lanes<E>(
    lanes: Vec<(RetrieverKind, RetrieverOutcome<RetrieverBatch<E>>)>,
) -> Vec<CompositionLaneInput> {
    lanes
        .into_iter()
        .map(|(kind, outcome)| lane(kind, outcome))
        .collect()
}

/// A bounded corpus that exercises every ordering and collapse rule at once:
/// score ties broken by source validity and anchor, same-source duplicate
/// rows, multi-occurrence fusions, evidence-backed logical-copy clusters
/// (one holding a contradiction and a corroboration), stale evidence, shared
/// files and sessions for diversity caps, and protected exact hits.
fn corpus_lanes() -> Vec<(
    RetrieverKind,
    RetrieverOutcome<RetrieverBatch<&'static str>>,
)> {
    let mut lexical = Vec::new();
    let mut graph = Vec::new();
    for index in 0..24_u32 {
        let name = format!("c{index:02}");
        // Groups of three share a raw score so the later comparator fields
        // decide the order.
        let raw_score = 900_000 - u64::from(index / 3) * 10_000;
        let mut hit = candidate(RetrieverKind::Lexical, &name, raw_score, index);
        hit.file_occurrence_id = Some(id(&format!("file.{}", index % 4)));
        hit.session_or_thread_id = Some(id(&format!("session.{}", index % 3)));
        if index % 7 == 0 {
            hit.freshness.compatibility = FreshnessCompatibilityV1::Stale;
        }
        // Three of every five candidates share an evidence-backed copy cluster.
        if index % 5 < 3 {
            let cluster = index / 5;
            hit.logical_copy_cluster_id = Some(id(&format!("copy.{cluster}")));
            hit.logical_copy_evidence_anchor =
                Some(RetrievalAnchorId::new(format!("copy-evidence.{cluster}")).unwrap());
        }
        if index == 11 {
            hit.evidence_role = EvidenceRole::Contradiction;
        }
        if index == 12 {
            hit.evidence_role = EvidenceRole::Corroboration;
        }
        // The graph lane returns every even candidate: multiples of four as
        // the identical evidence pair (same-source duplicate rows), the rest
        // as a second occurrence of the same logical evidence.
        if index % 2 == 0 {
            let mut graph_hit = candidate(RetrieverKind::Graph, &name, 500_000, index / 2);
            graph_hit.anchor_id = hit.anchor_id.clone();
            graph_hit.logical_evidence_id = hit.logical_evidence_id.clone();
            graph_hit.source_occurrence_id = hit.source_occurrence_id.clone();
            graph_hit.file_occurrence_id = hit.file_occurrence_id.clone();
            graph_hit.session_or_thread_id = hit.session_or_thread_id.clone();
            graph_hit.logical_copy_cluster_id = hit.logical_copy_cluster_id.clone();
            graph_hit.logical_copy_evidence_anchor = hit.logical_copy_evidence_anchor.clone();
            graph_hit.evidence_role = hit.evidence_role;
            graph_hit.freshness = hit.freshness.clone();
            if index % 4 == 0 {
                graph_hit.retriever_evidence_anchor = hit.retriever_evidence_anchor.clone();
            }
            graph.push(graph_hit);
        }
        lexical.push(hit);
    }
    let mut second_exact = exact_candidate("exact-b", 1);
    second_exact.ordinal_rank = 1;
    let exact = vec![exact_candidate("exact-a", 1), second_exact];
    vec![
        (
            RetrieverKind::ExactLiteral,
            RetrieverOutcome::Complete(batch(exact, "exact")),
        ),
        (
            RetrieverKind::Lexical,
            RetrieverOutcome::Complete(batch(lexical, "lexical")),
        ),
        (
            RetrieverKind::Graph,
            RetrieverOutcome::Complete(batch(graph, "graph")),
        ),
    ]
}

/// Caps that bite on the corpus along several dimensions at once, including
/// a boundary cap of one and one cap the corpus never reaches.
fn mixed_caps() -> DiversityPolicy {
    DiversityPolicy {
        per_file: Some(3),
        per_session_or_thread: Some(5),
        per_copy_cluster: Some(1),
        per_evidence_role: Some(40),
        ..no_caps()
    }
}
