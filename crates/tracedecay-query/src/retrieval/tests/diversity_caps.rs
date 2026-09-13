use tracedecay_domain::{
    CalibrationProfileId, DiversityPolicy, ExactClass, RankingDecisionKind, RetrieverKind,
    RetrieverOutcome, ScoreDomainCalibrationV1, ScoreDomainId,
};

use super::{batch, candidate, composition_lanes, corpus_lanes, id, mixed_caps, no_caps, profile};
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

#[test]
fn semantic_recomposition_preserves_fallback_file_slots_and_uses_free_ones() {
    let mut augmented_profile = profile();
    let semantic_domain = id::<ScoreDomainId>("score.semantic.v1");
    augmented_profile.calibrations.insert(
        RetrieverKind::Semantic,
        id::<CalibrationProfileId>("calibration.semantic.v1"),
    );
    augmented_profile.score_domain_calibrations.insert(
        semantic_domain.clone(),
        ScoreDomainCalibrationV1 {
            calibration_profile_id: id("calibration.semantic.v1"),
            score_domain: semantic_domain,
            raw_min_micros: 0,
            raw_max_micros: 1_000_000,
        },
    );
    augmented_profile
        .weights_micros
        .insert(RetrieverKind::Semantic, 1_000_000);

    let mut first = candidate(RetrieverKind::Lexical, "first", 900_000, 0);
    let mut second = candidate(RetrieverKind::Lexical, "second", 800_000, 1);
    let mut challenger = candidate(RetrieverKind::Lexical, "challenger", 700_000, 2);
    for candidate in [&mut first, &mut second, &mut challenger] {
        candidate.file_occurrence_id = Some(id("file.shared"));
    }
    let lexical = vec![first, second, challenger.clone()];
    let policy = DiversityPolicy {
        per_file: Some(2),
        ..no_caps()
    };
    let kernel = CompositionKernel::new(id("ranking.fixture.v1"));
    let fallback = kernel
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(vec![
                    (
                        RetrieverKind::ExactLiteral,
                        RetrieverOutcome::Complete(batch(Vec::new(), "exact")),
                    ),
                    (
                        RetrieverKind::Lexical,
                        RetrieverOutcome::Complete(batch(lexical.clone(), "lexical")),
                    ),
                    (
                        RetrieverKind::Graph,
                        RetrieverOutcome::Complete(batch(Vec::new(), "graph")),
                    ),
                ]),
            },
            &policy,
        )
        .expect("fallback composes");
    assert_eq!(
        ranked_anchors(&fallback),
        vec!["anchor.first", "anchor.second"]
    );

    let mut semantic_challenger = candidate(RetrieverKind::Semantic, "challenger", 1_000_000, 0);
    semantic_challenger.file_occurrence_id = Some(id("file.shared"));
    semantic_challenger.anchor_id = challenger.anchor_id;
    semantic_challenger.logical_evidence_id = challenger.logical_evidence_id;
    let mut new_file = candidate(RetrieverKind::Semantic, "new-file", 600_000, 1);
    new_file.file_occurrence_id = Some(id("file.other"));
    let augmented_input = FusionStageInput {
        profile: augmented_profile,
        lanes: composition_lanes(vec![
            (
                RetrieverKind::ExactLiteral,
                RetrieverOutcome::Complete(batch(Vec::new(), "exact")),
            ),
            (
                RetrieverKind::Lexical,
                RetrieverOutcome::Complete(batch(lexical, "lexical")),
            ),
            (
                RetrieverKind::Graph,
                RetrieverOutcome::Complete(batch(Vec::new(), "graph")),
            ),
            (
                RetrieverKind::Semantic,
                RetrieverOutcome::Complete(batch(vec![semantic_challenger, new_file], "semantic")),
            ),
        ]),
    };

    let unreserved = kernel
        .compose(&augmented_input, &policy)
        .expect("ordinary augmented composition succeeds");
    assert!(!ranked_anchors(&unreserved).contains(&"anchor.second"));

    let recomposed = kernel
        .compose_preserving_cap_incumbents(&augmented_input, &policy, &fallback.ranked_candidates)
        .expect("semantic recomposition succeeds");
    assert_eq!(
        ranked_anchors(&recomposed),
        vec!["anchor.new-file", "anchor.first", "anchor.second"]
    );
    assert_eq!(
        recomposed.diversity_decisions[0].capped[0].as_str(),
        "anchor.challenger"
    );
    assert_eq!(
        recomposed.ranked_candidates[0].candidate.utility_micros, 600_000,
        "the new-file semantic candidate keeps its fused score and rank"
    );
}
