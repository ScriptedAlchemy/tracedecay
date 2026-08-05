//! Checked-in profile requirements for optional native evaluation stages.

use serde::{Deserialize, Serialize};

use crate::candidate_output::{CandidateWorkloadV1, ProfileSpecV1};

use super::SemanticNativeEvaluationErrorV1;

/// Optional stages requested by one checked-in Plan 15 profile.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticNativeProfileRequirementsV1 {
    pub profile_id: String,
    pub semantic_requested: bool,
    pub rerank_requested: bool,
}

/// Derive execution requirements from the checked-in workload profile.
pub fn native_profile_requirements(
    workload: &CandidateWorkloadV1,
    profile_id: &str,
) -> Result<SemanticNativeProfileRequirementsV1, SemanticNativeEvaluationErrorV1> {
    let profile = workload
        .profile_matrix
        .iter()
        .find(|profile| profile.profile_id == profile_id)
        .ok_or_else(|| {
            SemanticNativeEvaluationErrorV1::Contract(format!("unknown profile {profile_id}"))
        })?;
    Ok(requirements_for_profile(profile))
}

pub(super) fn requirements_for_profile(
    profile: &ProfileSpecV1,
) -> SemanticNativeProfileRequirementsV1 {
    SemanticNativeProfileRequirementsV1 {
        profile_id: profile.profile_id.clone(),
        semantic_requested: profile.semantic_weight_ppm != 0,
        rerank_requested: profile.rerank_weight_ppm != 0,
    }
}
