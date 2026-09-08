use tracedecay_domain::{FusedCandidate, RankingDecision, RankingDecisionKind};

use super::{composition_lanes, corpus_lanes, id, mixed_caps, no_caps, profile};
use crate::retrieval::fusion::{
    CompositionKernel, CompositionOutputV1, DeterministicFixedPointFusion, FusionStageInput,
    digest_candidate_set,
};
use crate::retrieval::ordering::{compare_fused, decision_cmp};
use crate::retrieval::stage_counters;

fn compose_corpus(policy: &tracedecay_domain::DiversityPolicy) -> CompositionOutputV1 {
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

fn provenance_count(candidate: &FusedCandidate) -> usize {
    candidate
        .decisions
        .iter()
        .filter(|decision| decision.kind == RankingDecisionKind::ComparatorProvenance)
        .count()
}

fn is_sorted_by<T>(items: &[T], compare: impl Fn(&T, &T) -> std::cmp::Ordering) -> bool {
    items
        .windows(2)
        .all(|pair| compare(&pair[0], &pair[1]) != std::cmp::Ordering::Greater)
}

fn decisions_are_sorted(decisions: &[RankingDecision]) -> bool {
    is_sorted_by(decisions, decision_cmp)
}

#[test]
fn composition_sorts_once_and_keeps_one_provenance_per_fused_candidate() {
    stage_counters::reset();
    let output = compose_corpus(&no_caps());
    let work = stage_counters::snapshot();

    let collapsed = output
        .dedupe_decisions
        .iter()
        .flat_map(|decision| &decision.collapsed_candidates)
        .collect::<Vec<_>>();
    // The corpus must actually exercise ties, clusters, and collapse.
    assert_eq!(output.ranked_candidates.len(), 18);
    assert_eq!(collapsed.len(), 8);

    assert_eq!(
        work.fused_sorts, 1,
        "fused candidates are sorted exactly once"
    );
    assert_eq!(
        work.comparator_records,
        output.ranked_candidates.len() + collapsed.len(),
        "one comparator record per fused candidate, surviving or collapsed"
    );

    // Survivors are in comparator order without any later re-sort.
    let mut reference = output
        .ranked_candidates
        .iter()
        .map(|ranked| ranked.candidate.clone())
        .collect::<Vec<_>>();
    reference.sort_by(compare_fused);
    assert!(
        output
            .ranked_candidates
            .iter()
            .map(|ranked| &ranked.candidate)
            .eq(reference.iter())
    );
    assert!(
        output
            .ranked_candidates
            .iter()
            .enumerate()
            .all(|(ordinal, ranked)| ranked.final_ordinal as usize == ordinal)
    );

    // The reported comparator records are the ones built at ordering time
    // and match the ranked candidates field for field.
    let fusion = DeterministicFixedPointFusion::new(id("ranking.fixture.v1"));
    assert_eq!(
        output.comparator_records.len(),
        output.ranked_candidates.len()
    );
    for (record, ranked) in output
        .comparator_records
        .iter()
        .zip(&output.ranked_candidates)
    {
        assert_eq!(*record, fusion.comparator_record(&ranked.candidate));
    }

    for candidate in output
        .ranked_candidates
        .iter()
        .map(|ranked| &ranked.candidate)
        .chain(collapsed.iter().copied())
    {
        assert_eq!(
            provenance_count(candidate),
            1,
            "{} carries exactly one comparator provenance decision",
            candidate.anchor_id
        );
        assert!(
            decisions_are_sorted(&candidate.decisions),
            "{} keeps its decisions in canonical order",
            candidate.anchor_id
        );
    }
}

#[test]
fn representative_selection_preserves_order_and_moves_collapsed_copies() {
    let output = compose_corpus(&no_caps());
    let copy_decisions = output
        .dedupe_decisions
        .iter()
        .filter(|decision| {
            decision.decision.kind == RankingDecisionKind::LogicalCopyRepresentativeSelection
        })
        .collect::<Vec<_>>();

    // Four clusters collapse two copies each; the cluster holding the
    // contradiction and the corroboration keeps every member independent.
    assert_eq!(
        copy_decisions
            .iter()
            .map(|decision| decision
                .copy_cluster
                .as_ref()
                .map(|cluster| cluster.as_str()))
            .collect::<Vec<_>>(),
        vec![
            Some("copy.0"),
            Some("copy.1"),
            Some("copy.3"),
            Some("copy.4")
        ]
    );
    for decision in &copy_decisions {
        assert_eq!(decision.collapsed_candidates.len(), 2);
        let representative = output
            .ranked_candidates
            .iter()
            .map(|ranked| &ranked.candidate)
            .find(|candidate| {
                candidate.occurrences[0].source_occurrence_id == decision.kept_occurrence
            })
            .expect("representative survives");
        assert!(representative.decisions.contains(&decision.decision));
        assert!(is_sorted_by(&decision.collapsed_candidates, compare_fused));
        assert!(decision.collapsed_candidates.iter().all(|collapsed| {
            compare_fused(representative, collapsed) == std::cmp::Ordering::Less
        }));
        assert!(
            decision
                .collapsed_occurrences
                .iter()
                .all(|occurrence| *occurrence != decision.kept_occurrence)
        );
    }

    let survivor = |name: &str| {
        output
            .ranked_candidates
            .iter()
            .map(|ranked| &ranked.candidate)
            .find(|candidate| candidate.anchor_id.as_str() == name)
    };
    let contradiction = survivor("anchor.c11").expect("contradiction is preserved");
    assert!(
        contradiction
            .decisions
            .iter()
            .any(|decision| { decision.kind == RankingDecisionKind::ContradictionPreservation })
    );
    assert!(decisions_are_sorted(&contradiction.decisions));
    assert!(
        survivor("anchor.c12").is_some(),
        "corroboration stays independent"
    );
    assert!(
        survivor("anchor.c10").is_some(),
        "lone cluster member is its own representative"
    );
    assert!(
        survivor("anchor.c01").is_none(),
        "collapsed copy leaves the ranking"
    );
}

#[test]
fn capped_composition_is_lane_order_invariant_and_digest_stable() {
    let kernel = CompositionKernel::new(id("ranking.fixture.v1"));
    let expected = compose_corpus(&mixed_caps());
    assert!(
        !expected.diversity_decisions.is_empty(),
        "mixed caps must bite on the corpus"
    );
    let expected_digest = digest_candidate_set(&expected.ranked_candidates).unwrap();

    for iteration in 0..6 {
        let mut lanes = corpus_lanes();
        let offset = iteration % lanes.len();
        lanes.rotate_left(offset);
        if iteration % 2 == 1 {
            lanes.reverse();
        }
        let output = kernel
            .compose(
                &FusionStageInput {
                    profile: profile(),
                    lanes: composition_lanes(lanes),
                },
                &mixed_caps(),
            )
            .expect("permuted corpus composes");
        assert_eq!(
            output, expected,
            "lane permutation {iteration} changed composition"
        );
        assert_eq!(
            digest_candidate_set(&output.ranked_candidates).unwrap(),
            expected_digest
        );
    }
}
