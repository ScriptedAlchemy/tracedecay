//! Binding checks between the public fallback payload and its native baseline.

use tracedecay_domain::{QueryFallbackSubpayload, RankedCandidate};

use super::{SemanticChannelAblationResultV1, SemanticNativeEvaluationErrorV1};

/// A digest proves that a fallback has not changed, but not that it was derived
/// from this query. Bind the supplied fallback to the exact/lexical/graph
/// baseline before optional semantic work can report it as preserved.
pub(super) fn validate_query_fallback_baseline(
    fallback: &QueryFallbackSubpayload,
    baseline: &SemanticChannelAblationResultV1,
) -> Result<(), SemanticNativeEvaluationErrorV1> {
    if fallback.public_fallback_lane_coverage != baseline.public_lane_statuses {
        return Err(SemanticNativeEvaluationErrorV1::Contract(
            "query fallback coverage does not match the exact/lexical/graph baseline".to_owned(),
        ));
    }
    if fallback.freshness != baseline.freshness {
        return Err(SemanticNativeEvaluationErrorV1::Contract(
            "query fallback freshness does not match the exact/lexical/graph baseline".to_owned(),
        ));
    }
    if !same_order_and_provenance(&fallback.ordered_candidates, &baseline.ranked_candidates) {
        return Err(SemanticNativeEvaluationErrorV1::Contract(
            "query fallback does not reproduce exact/lexical/graph baseline order and provenance"
                .to_owned(),
        ));
    }
    Ok(())
}

fn same_order_and_provenance(fallback: &[RankedCandidate], baseline: &[RankedCandidate]) -> bool {
    fallback.len() == baseline.len()
        && fallback.iter().zip(baseline).all(|(fallback, baseline)| {
            fallback.final_ordinal == baseline.final_ordinal
                && fallback.candidate.anchor_id == baseline.candidate.anchor_id
                && fallback.candidate.logical_evidence_id == baseline.candidate.logical_evidence_id
                && fallback.candidate.exact_class == baseline.candidate.exact_class
                && fallback.candidate.occurrences == baseline.candidate.occurrences
        })
}
