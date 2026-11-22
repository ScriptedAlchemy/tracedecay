use std::collections::BTreeMap;

use tracedecay_domain::{
    CompactCandidate, DiversityPolicy, EvidenceRole, ExactClass, FreshnessCompatibilityV1,
    FusionProfile, RankingDecisionKind, RetrievalAnchorId, RetrievalSourceRoleV1, RetrieverKind,
    RetrieverOutcome, ScoreDomainId,
};

use super::{
    batch, candidate, composition_lanes, corpus_lanes, exact_candidate, id, mixed_caps, no_caps,
    profile,
};
use crate::retrieval::fusion::{CompositionKernel, CompositionOutputV1, FusionStageInput};
use crate::retrieval::stage_counters;

fn compose_corpus(policy: &DiversityPolicy) -> CompositionOutputV1 {
    CompositionKernel::new(id("ranking.fixture.v1"))
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(corpus_lanes()),
            },
            policy,
        )
        .expect("corpus composes")
}

fn ranked_anchors(output: &CompositionOutputV1) -> Vec<&str> {
    output
        .ranked_candidates
        .iter()
        .map(|ranked| ranked.candidate.anchor_id.as_str())
        .collect()
}

const UNCAPPED_ORDER: [&str; 18] = [
    "anchor.exact-a",
    "anchor.exact-b",
    "anchor.c02",
    "anchor.c04",
    "anchor.c06",
    "anchor.c08",
    "anchor.c10",
    "anchor.c12",
    "anchor.c14",
    "anchor.c16",
    "anchor.c18",
    "anchor.c20",
    "anchor.c03",
    "anchor.c09",
    "anchor.c11",
    "anchor.c13",
    "anchor.c19",
    "anchor.c23",
];

#[test]
fn absent_caps_are_a_no_op_that_derives_no_cap_keys() {
    stage_counters::reset();
    let output = compose_corpus(&no_caps());

    assert_eq!(stage_counters::snapshot().cap_key_derivations, 0);
    assert!(output.diversity_decisions.is_empty());
    assert_eq!(ranked_anchors(&output), UNCAPPED_ORDER);
    assert_eq!(
        output
            .ranked_candidates
            .iter()
            .map(|ranked| ranked.final_ordinal)
            .collect::<Vec<_>>(),
        (0..18).collect::<Vec<u32>>()
    );
    assert!(output.ranked_candidates.iter().all(|ranked| {
        ranked
            .candidate
            .decisions
            .iter()
            .all(|decision| decision.kind != RankingDecisionKind::DiversityCap)
    }));
}

#[test]
fn mixed_caps_derive_keys_once_per_unprotected_candidate_and_keep_reasons() {
    stage_counters::reset();
    let output = compose_corpus(&mixed_caps());

    // Files cycle through four values and copy clusters cap at one, so the
    // corpus caps four approximate candidates while the session and evidence
    // role caps stay below their limits. Both exact hits and the contradiction
    // are protected and never counted.
    let capped = output
        .diversity_decisions
        .iter()
        .map(|decision| {
            (
                decision.capped[0].as_str(),
                decision.decision.detail.as_str(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        capped,
        vec![
            ("anchor.c12", "capped by logical_copy_cluster"),
            ("anchor.c14", "capped by file"),
            ("anchor.c18", "capped by file"),
            ("anchor.c20", "capped by file"),
        ]
    );
    let capped_anchors = capped.iter().map(|(anchor, _)| *anchor).collect::<Vec<_>>();
    let expected_order = UNCAPPED_ORDER
        .iter()
        .copied()
        .filter(|anchor| !capped_anchors.contains(anchor))
        .collect::<Vec<_>>();
    assert_eq!(ranked_anchors(&output), expected_order);
    assert_eq!(
        output
            .ranked_candidates
            .iter()
            .map(|ranked| ranked.final_ordinal)
            .collect::<Vec<_>>(),
        (0..14).collect::<Vec<u32>>()
    );
    assert!(output.diversity_decisions.iter().all(|decision| {
        decision.decision.policy_anchor == mixed_caps().evaluation_result_anchor
            && decision.decision.evidence_anchor.is_some()
    }));

    let protected =
        output
            .ranked_candidates
            .iter()
            .filter(|ranked| {
                ranked.candidate.exact_class != ExactClass::Approximate
                    || ranked.candidate.decisions.iter().any(|decision| {
                        decision.kind == RankingDecisionKind::ContradictionPreservation
                    })
            })
            .count();
    assert_eq!(protected, 3);
    let unprotected_seen = output.ranked_candidates.len() - protected + capped.len();
    assert_eq!(
        stage_counters::snapshot().cap_key_derivations,
        unprotected_seen,
        "every unprotected candidate derives its cap keys exactly once"
    );
}

#[test]
fn a_boundary_cap_of_one_keeps_the_first_key_holder_and_refills_from_others() {
    let policy = DiversityPolicy {
        per_session_or_thread: Some(1),
        ..no_caps()
    };
    let output = compose_corpus(&policy);

    // Sessions cycle through three values; the first approximate holder of
    // each survives, protected evidence passes through, and everything else
    // is capped with the session reason.
    assert_eq!(
        ranked_anchors(&output),
        vec![
            "anchor.exact-a",
            "anchor.exact-b",
            "anchor.c02",
            "anchor.c04",
            "anchor.c06",
            "anchor.c11",
        ]
    );
    assert_eq!(output.diversity_decisions.len(), 12);
    assert!(
        output
            .diversity_decisions
            .iter()
            .all(|decision| decision.decision.detail == "capped by session_or_thread")
    );
}

fn file_candidate(
    lane: RetrieverKind,
    name: &str,
    raw_score_micros: u64,
    file: &str,
) -> CompactCandidate {
    let mut hit = candidate(lane, name, raw_score_micros, 0);
    hit.file_occurrence_id = Some(id(file));
    hit
}

fn compose_file_candidates(
    fusion_profile: FusionProfile,
    policy: &DiversityPolicy,
    candidates: Vec<CompactCandidate>,
    anchor_tiers: &BTreeMap<RetrievalAnchorId, u32>,
) -> CompositionOutputV1 {
    let lanes = RetrieverKind::QUERY_FALLBACK_LANES
        .into_iter()
        .map(|lane| {
            let mut candidates = candidates
                .iter()
                .filter(|candidate| candidate.retriever == lane)
                .cloned()
                .collect::<Vec<_>>();
            for (ordinal, candidate) in candidates.iter_mut().enumerate() {
                candidate.ordinal_rank = ordinal as u32;
            }
            (lane, RetrieverOutcome::Complete(batch(candidates, "file evidence")))
        })
        .collect();
    CompositionKernel::new(id("ranking.fixture.v1"))
        .compose_with_anchor_tiers(
            &FusionStageInput {
                profile: fusion_profile,
                lanes: composition_lanes(lanes),
            },
            policy,
            anchor_tiers,
        )
        .expect("file evidence composes")
}

#[test]
fn a_file_retains_direct_text_and_fills_its_other_slot_in_fused_order() {
    let candidates = vec![
        file_candidate(RetrieverKind::Lexical, "direct", 400_000, "file.shared"),
        file_candidate(RetrieverKind::Lexical, "context", 200_000, "file.shared"),
        file_candidate(RetrieverKind::Graph, "context", 1_000_000, "file.shared"),
        file_candidate(RetrieverKind::Graph, "graph-only", 900_000, "file.shared"),
    ];
    let uncapped = compose_file_candidates(
        profile(),
        &no_caps(),
        candidates.clone(),
        &BTreeMap::new(),
    );
    assert_eq!(
        ranked_anchors(&uncapped),
        ["anchor.context", "anchor.graph-only", "anchor.direct"]
    );

    let policy = DiversityPolicy {
        per_file: Some(2),
        ..no_caps()
    };
    let output = compose_file_candidates(profile(), &policy, candidates, &BTreeMap::new());
    assert_eq!(ranked_anchors(&output), ["anchor.context", "anchor.direct"]);
    assert_eq!(output.diversity_decisions.len(), 1);
    assert_eq!(output.diversity_decisions[0].capped, vec![id("anchor.graph-only")]);
    assert_eq!(output.diversity_decisions[0].decision.detail, "capped by file");
}

#[test]
fn text_retention_uses_saturation_ties_and_freshness_independently_of_lane_ordinals() {
    let policy = DiversityPolicy {
        per_file: Some(1),
        ..no_caps()
    };
    for (direct_raw, context_raw, context_freshness) in [
        (2_000_000, 1_500_000, FreshnessCompatibilityV1::Current),
        (1_500_000, 2_000_000, FreshnessCompatibilityV1::Stale),
    ] {
        let direct = file_candidate(RetrieverKind::Lexical, "direct", direct_raw, "file.shared");
        let mut context =
            file_candidate(RetrieverKind::Lexical, "context", context_raw, "file.shared");
        context.freshness.compatibility = context_freshness;
        let mut graph =
            file_candidate(RetrieverKind::Graph, "context", 1_000_000, "file.shared");
        graph.freshness.compatibility = context_freshness;
        let candidates = vec![direct, context, graph];
        for reversed in [false, true] {
            let mut candidates = candidates.clone();
            if reversed {
                candidates.reverse();
            }
            // The helper renumbers each valid batch after this permutation.
            let output =
                compose_file_candidates(profile(), &policy, candidates, &BTreeMap::new());
            assert_eq!(
                ranked_anchors(&output),
                ["anchor.direct"],
                "direct={direct_raw}, context={context_raw}, freshness={context_freshness:?}, \
                 reversed={reversed}"
            );
        }
    }
}

#[test]
fn text_reservations_do_not_override_anchor_or_source_priority() {
    let policy = DiversityPolicy {
        per_file: Some(1),
        ..no_caps()
    };
    for caller_anchor in [false, true] {
        let direct = file_candidate(RetrieverKind::Lexical, "direct", 1_000_000, "file.shared");
        let mut preferred =
            file_candidate(RetrieverKind::Graph, "preferred", 1_000_000, "file.shared");
        let mut anchor_tiers = BTreeMap::new();
        if caller_anchor {
            anchor_tiers.insert(preferred.anchor_id.clone(), 1);
        } else {
            preferred.source_role = RetrievalSourceRoleV1::ProductionDefinition;
        }
        let output =
            compose_file_candidates(profile(), &policy, vec![direct, preferred], &anchor_tiers);
        assert_eq!(ranked_anchors(&output), ["anchor.preferred"]);
    }
}

#[test]
fn an_infeasible_text_reservation_replays_original_priority_at_other_caps() {
    let mut first = file_candidate(RetrieverKind::Graph, "first", 1_000_000, "file.first");
    first.session_or_thread_id = Some(id("session.blocked"));
    let mut context = file_candidate(RetrieverKind::Graph, "context", 900_000, "file.shared");
    context.session_or_thread_id = Some(id("session.competing"));
    let mut competitor =
        file_candidate(RetrieverKind::Graph, "competitor", 800_000, "file.other");
    competitor.session_or_thread_id = Some(id("session.competing"));
    let mut direct = file_candidate(RetrieverKind::Lexical, "direct", 100_000, "file.shared");
    direct.session_or_thread_id = Some(id("session.blocked"));
    let policy = DiversityPolicy {
        per_file: Some(1),
        per_session_or_thread: Some(1),
        ..no_caps()
    };
    stage_counters::reset();
    let output = compose_file_candidates(
        profile(),
        &policy,
        vec![first, context, competitor, direct],
        &BTreeMap::new(),
    );
    assert_eq!(ranked_anchors(&output), ["anchor.first", "anchor.context"]);
    assert_eq!(stage_counters::snapshot().cap_key_derivations, 4);
    assert_eq!(
        output
            .diversity_decisions
            .iter()
            .map(|decision| {
                (
                    decision.capped[0].as_str(),
                    decision.decision.detail.as_str(),
                )
            })
            .collect::<Vec<_>>(),
        [
            ("anchor.competitor", "capped by session_or_thread"),
            ("anchor.direct", "capped by file,session_or_thread"),
        ]
    );
}

#[test]
fn multi_file_reservations_follow_text_provenance_and_release_failed_witnesses() {
    let policy = DiversityPolicy {
        per_file: Some(1),
        per_session_or_thread: Some(1),
        ..no_caps()
    };
    for blocked_second_file in [false, true] {
        let mut direct_b = file_candidate(RetrieverKind::Lexical, "direct-b", 400_000, "file.b");
        direct_b.session_or_thread_id = Some(id("session.shared"));
        let mut candidates = vec![
            file_candidate(RetrieverKind::Lexical, "multi", 1_000_000, "file.a"),
            file_candidate(RetrieverKind::Graph, "multi", 1_000_000, "file.b"),
            direct_b,
        ];
        if blocked_second_file {
            let mut blocker = file_candidate(RetrieverKind::Graph, "blocker", 900_000, "file.c");
            blocker.session_or_thread_id = Some(id("session.shared"));
            candidates.push(blocker);
        }
        let output = compose_file_candidates(profile(), &policy, candidates, &BTreeMap::new());
        if blocked_second_file {
            // Both initial reservations fail. Releasing a reservation must
            // leave its candidate eligible for ordinary admission on replay.
            assert_eq!(ranked_anchors(&output), ["anchor.multi", "anchor.blocker"]);
        } else {
            // The lexical occurrence in file.a cannot reserve a text slot
            // for the same candidate's graph-only occurrence in file.b.
            assert_eq!(ranked_anchors(&output), ["anchor.direct-b"]);
        }
    }
}

#[test]
fn zero_file_capacity_preserves_exact_contradictions_and_unfiled_evidence() {
    let mut exact = exact_candidate("exact", 1_000_000);
    exact.file_occurrence_id = Some(id("file.shared"));
    let mut contradiction =
        file_candidate(RetrieverKind::Graph, "contradiction", 900_000, "file.shared");
    contradiction.evidence_role = EvidenceRole::Contradiction;
    let unfiled = candidate(RetrieverKind::Lexical, "unfiled", 300_000, 0);
    let policy = DiversityPolicy {
        per_file: Some(0),
        ..no_caps()
    };
    let output = compose_file_candidates(
        profile(),
        &policy,
        vec![
            exact,
            contradiction,
            unfiled,
            file_candidate(RetrieverKind::Lexical, "direct", 1_000_000, "file.shared"),
            file_candidate(RetrieverKind::Graph, "context", 1_000_000, "file.shared"),
        ],
        &BTreeMap::new(),
    );
    assert_eq!(
        ranked_anchors(&output),
        ["anchor.exact", "anchor.contradiction", "anchor.unfiled"]
    );
    assert_eq!(output.diversity_decisions.len(), 2);
}

#[test]
fn disabled_or_zero_utility_text_does_not_reserve_a_file_slot() {
    let policy = DiversityPolicy {
        per_file: Some(1),
        ..no_caps()
    };
    let mut disabled = profile();
    disabled.weights_micros.insert(RetrieverKind::Lexical, 0);
    let mut zero_feature = profile();
    zero_feature
        .score_domain_calibrations
        .get_mut(&id::<ScoreDomainId>("score.lexical.v1"))
        .expect("lexical calibration exists")
        .raw_min_micros = 500_000;
    let mut rounded_to_zero = profile();
    rounded_to_zero.weights_micros.insert(RetrieverKind::Lexical, 1);
    for (name, fusion_profile, direct_raw) in [
        ("disabled weight", disabled, 1_000_000),
        ("zero calibrated feature", zero_feature, 400_000),
        ("zero weighted contribution", rounded_to_zero, 1),
    ] {
        let output = compose_file_candidates(
            fusion_profile,
            &policy,
            vec![
                file_candidate(RetrieverKind::Lexical, "direct", direct_raw, "file.shared"),
                file_candidate(RetrieverKind::Graph, "context", 1_000_000, "file.shared"),
            ],
            &BTreeMap::new(),
        );
        assert_eq!(ranked_anchors(&output), ["anchor.context"], "{name}");
    }
}
