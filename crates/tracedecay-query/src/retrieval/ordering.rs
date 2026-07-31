use std::cmp::Ordering;

use tracedecay_domain::{ExactClass, FreshnessCompatibilityV1, FusedCandidate, SourceOccurrenceId};

pub(super) fn compare_fused(left: &FusedCandidate, right: &FusedCandidate) -> Ordering {
    exact_class_rank(left.exact_class)
        .cmp(&exact_class_rank(right.exact_class))
        .then_with(|| right.utility_micros.cmp(&left.utility_micros))
        .then_with(|| source_validity_rank(right).cmp(&source_validity_rank(left)))
        .then_with(|| left.anchor_id.cmp(&right.anchor_id))
        .then_with(|| left.logical_evidence_id.cmp(&right.logical_evidence_id))
        .then_with(|| ordered_occurrence_ids(left).cmp(&ordered_occurrence_ids(right)))
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

pub(super) fn ordered_occurrence_ids(candidate: &FusedCandidate) -> Vec<SourceOccurrenceId> {
    let mut occurrences = candidate
        .occurrences
        .iter()
        .map(|occurrence| occurrence.source_occurrence_id.clone())
        .collect::<Vec<_>>();
    occurrences.sort();
    occurrences.dedup();
    occurrences
}
