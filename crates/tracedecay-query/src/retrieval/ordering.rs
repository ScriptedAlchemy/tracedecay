use std::cmp::{Ordering, Reverse};

use tracedecay_domain::{
    ExactClass, FixedPointScore, FreshnessCompatibilityV1, FusedCandidate, RankingDecision,
    RetrievalAnchorId, RetrieverKind, ScoreDomainId, SourceOccurrenceId,
};

use super::stage_counters;

/// Fused candidates in `compare_fused` order.
///
/// The only constructor sorts, so a stage that takes this type never re-sorts
/// and never trusts an unchecked caller's claim of order. Mutation through
/// [`Self::iter_mut`] is limited by contract to `decisions`, which the
/// comparator does not read.
pub(super) struct OrderedFusedCandidates(Vec<FusedCandidate>);

impl OrderedFusedCandidates {
    pub(super) fn sort(mut candidates: Vec<FusedCandidate>) -> Self {
        stage_counters::record_fused_sort();
        candidates.sort_by(compare_fused);
        Self(candidates)
    }

    pub(super) fn iter_mut(&mut self) -> std::slice::IterMut<'_, FusedCandidate> {
        self.0.iter_mut()
    }

    pub(super) fn into_vec(self) -> Vec<FusedCandidate> {
        self.0
    }
}

pub(super) fn compare_fused(left: &FusedCandidate, right: &FusedCandidate) -> Ordering {
    exact_class_rank(left.exact_class)
        .cmp(&exact_class_rank(right.exact_class))
        .then_with(|| right.utility_micros.cmp(&left.utility_micros))
        .then_with(|| source_validity_rank(right).cmp(&source_validity_rank(left)))
        .then_with(|| ordered_domain_scores(left).cmp(&ordered_domain_scores(right)))
        .then_with(|| {
            ordered_retriever_evidence_anchors(left).cmp(&ordered_retriever_evidence_anchors(right))
        })
        .then_with(|| ordered_occurrence_id_refs(left).cmp(&ordered_occurrence_id_refs(right)))
}

/// Source-bound lexical/exact and graph evidence anchors are generation-free.
/// Generation-scoped occurrence IDs remain the final discriminator only when
/// the available source identity cannot distinguish otherwise equal evidence.
pub(super) fn ordered_retriever_evidence_anchors(
    candidate: &FusedCandidate,
) -> Vec<&RetrievalAnchorId> {
    let mut anchors = candidate
        .occurrences
        .iter()
        .map(|occurrence| &occurrence.retriever_evidence_anchor)
        .collect::<Vec<_>>();
    anchors.sort();
    anchors.dedup();
    anchors
}

pub(super) fn decision_cmp(left: &RankingDecision, right: &RankingDecision) -> Ordering {
    left.kind
        .cmp(&right.kind)
        .then_with(|| left.retriever.cmp(&right.retriever))
        .then_with(|| left.policy_anchor.cmp(&right.policy_anchor))
        .then_with(|| left.evidence_anchor.cmp(&right.evidence_anchor))
        .then_with(|| left.detail.cmp(&right.detail))
}

pub(super) fn exact_class_rank(class: ExactClass) -> u8 {
    match class {
        ExactClass::ExactMessage => 0,
        ExactClass::ExactLiteralPhrase => 1,
        ExactClass::Approximate => 2,
    }
}

pub(super) fn source_validity_rank(candidate: &FusedCandidate) -> u8 {
    candidate
        .freshness
        .iter()
        .map(|freshness| match freshness.compatibility {
            FreshnessCompatibilityV1::Current => 4,
            FreshnessCompatibilityV1::Unknown => 3,
            FreshnessCompatibilityV1::Stale => 2,
            FreshnessCompatibilityV1::Missing => 1,
            FreshnessCompatibilityV1::Incompatible => 0,
        })
        .max()
        .unwrap_or(0)
}

pub(super) fn ordered_occurrence_id_refs(candidate: &FusedCandidate) -> Vec<&SourceOccurrenceId> {
    let mut occurrences = candidate
        .occurrences
        .iter()
        .map(|occurrence| &occurrence.source_occurrence_id)
        .collect::<Vec<_>>();
    occurrences.sort();
    occurrences.dedup();
    occurrences
}

pub(super) fn ordered_occurrence_ids(candidate: &FusedCandidate) -> Vec<SourceOccurrenceId> {
    ordered_occurrence_id_refs(candidate)
        .into_iter()
        .cloned()
        .collect()
}

/// Preserve measured score differences when calibration rounds or saturates.
/// Compare unique entries lexicographically: retriever and score-domain tags
/// ascending, then raw score descending within matching tags. Different lane
/// or domain mixes therefore use tag order before evidence identity, never
/// compare unrelated numeric scales. A common-domains-only comparison would
/// not define a transitive total order across candidates with different lanes.
pub(super) fn ordered_domain_scores(
    candidate: &FusedCandidate,
) -> Vec<(RetrieverKind, &ScoreDomainId, Reverse<FixedPointScore>)> {
    let mut scores = candidate
        .contributions
        .iter()
        .filter(|contribution| contribution.weight_micros > 0)
        .map(|contribution| {
            (
                contribution.retriever,
                &contribution.score_domain,
                Reverse(contribution.raw_score),
            )
        })
        .collect::<Vec<_>>();
    scores.sort();
    scores.dedup();
    scores
}
