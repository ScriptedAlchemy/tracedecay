use tracedecay_domain::{DiversityPolicy, FusionProfile};

use super::SemanticActivationCoordinationErrorV1;
use crate::config::retrieval::{
    AcceptedRetrievalProfileV1, PassingRetrievalEvaluationV1, RetrievalCompatibilityPinsV1,
    RetrievalRuntimeCompatibilityV1,
};
use crate::search_eval::{
    CandidateWorkloadV1, DirectEvaluationReportV1, direct_evaluated_profile_material,
};

const PR9_PROFILE_ID: &str = "pr9-fallback";
const PR9_WORKLOAD_JSON: &str =
    include_str!("../../../tests/fixtures/search_quality/pr9-pr10-candidate-workload-v1.json");
const PR9_REPORT_JSON: &str =
    include_str!("../../../benchmarks/search-quality/pr9-fallback-report-v1.json");

/// Reconstruct the shipped exact/lexical/graph profile from the byte-pinned
/// evaluator workload and its passing direct report. The report is rechecked
/// by `PassingRetrievalEvaluationV1`; no caller-supplied pass label or profile
/// material enters this path.
pub(crate) fn bundled_pr9_authority() -> Result<
    (
        DirectEvaluationReportV1,
        AcceptedRetrievalProfileV1,
        RetrievalRuntimeCompatibilityV1,
    ),
    SemanticActivationCoordinationErrorV1,
> {
    let workload: CandidateWorkloadV1 = serde_json::from_str(PR9_WORKLOAD_JSON)
        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
    let report: DirectEvaluationReportV1 = serde_json::from_str(PR9_REPORT_JSON)
        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
    let evaluation = PassingRetrievalEvaluationV1::from_report(&report, PR9_PROFILE_ID)
        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
    let material = direct_evaluated_profile_material(&workload, PR9_PROFILE_ID)
        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
    if material.rerank.is_some() {
        return Err(SemanticActivationCoordinationErrorV1::Rejected);
    }
    let evaluation_anchor = evaluation.evaluation_anchor().clone();
    let profile = FusionProfile {
        evaluation_result_anchor: evaluation_anchor.clone(),
        ..material.profile
    };
    let diversity = DiversityPolicy {
        evaluation_result_anchor: Some(evaluation_anchor),
        ..material.diversity
    };
    let retrieval_ceiling = profile.retrieval_budget;
    let accepted_profile = AcceptedRetrievalProfileV1::new(
        profile,
        diversity,
        None,
        RetrievalCompatibilityPinsV1::default(),
        evaluation,
    )
    .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
    let runtime = RetrievalRuntimeCompatibilityV1 {
        retrieval_ceiling,
        semantic: None,
        semantic_ceiling: None,
        rerank: None,
        rerank_ceiling: None,
    };
    accepted_profile
        .executable_under(&runtime)
        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
    Ok((report, accepted_profile, runtime))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search_eval::DirectEvaluationStatusV1;

    #[test]
    fn bundled_profile_is_the_passing_exact_pr9_fallback() {
        let (report, accepted_profile, _) = bundled_pr9_authority().expect("bundled PR9 authority");

        assert_eq!(report.status, DirectEvaluationStatusV1::Pass);
        assert!(accepted_profile.is_exact_pr9_fallback());
        assert_eq!(
            accepted_profile.evaluation().evaluated_profile_id(),
            PR9_PROFILE_ID
        );
    }
}
