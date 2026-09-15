//! Retained sanitized evidence for one direct quality evaluation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracedecay_domain::canonical_sha256;

use super::candidate_output::{
    CandidateWorkloadV1, EvaluationExecutionContractV1, GenerateCandidateOutputsResultV1,
    ProductionCandidateOutputV1, compute_corpus_digest, compute_workload_digest,
};
use super::evaluate::{
    DirectEvaluationStatusV1, SearchEvalError, evaluate_generated_outputs_against_corpus,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectQueryEvaluationV1 {
    pub query_id: String,
    pub strata: Vec<String>,
    pub protected: bool,
    pub first_useful_rank: Option<u32>,
    pub returned_candidates: usize,
    pub wrong_scope_hits: usize,
    pub forbidden_hits: usize,
    pub expected_no_result: bool,
    pub quality: DirectQueryQualityV1,
    pub status: DirectEvaluationStatusV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectRatioMetricV1 {
    pub numerator: u64,
    pub denominator: u64,
    pub ppm: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectQueryQualityV1 {
    pub recall_at_10: DirectRatioMetricV1,
    pub precision_at_10: DirectRatioMetricV1,
    pub reciprocal_rank_ppm: u32,
    pub ndcg_at_10_ppm: u32,
    pub duplicate_rate: DirectRatioMetricV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectStratumQualityV1 {
    pub stratum: String,
    pub protected: bool,
    pub query_count: u64,
    pub relevant_query_count: u64,
    pub recall_at_10: DirectRatioMetricV1,
    pub precision_at_10: DirectRatioMetricV1,
    pub mean_reciprocal_rank_ppm: u32,
    pub ndcg_at_10_ppm: u32,
    pub duplicate_rate: DirectRatioMetricV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectWorstStratumV1 {
    pub stratum: String,
    pub protected: bool,
    pub relevant_query_count: u64,
    pub recall_at_10: DirectRatioMetricV1,
    pub mean_reciprocal_rank_ppm: u32,
    pub ndcg_at_10_ppm: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectQualityMetricsV1 {
    pub relevant_query_count: u64,
    pub recall_at_10: DirectRatioMetricV1,
    pub precision_at_10: DirectRatioMetricV1,
    pub mean_reciprocal_rank_ppm: u32,
    pub ndcg_at_10_ppm: u32,
    pub duplicate_rate: DirectRatioMetricV1,
    pub protected_recall_at_10: DirectRatioMetricV1,
    pub strata: Vec<DirectStratumQualityV1>,
    pub worst_stratum: Option<DirectWorstStratumV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectProfileEvaluationV1 {
    pub profile_id: String,
    pub partition: String,
    pub query_count: usize,
    pub failed_queries: usize,
    pub fallback_stable: bool,
    pub fallback_matches_expected: bool,
    pub cancellation_bounded: bool,
    pub offline: bool,
    pub resource_status: DirectEvaluationStatusV1,
    pub quality: DirectQualityMetricsV1,
    pub status: DirectEvaluationStatusV1,
    pub queries: Vec<DirectQueryEvaluationV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectEvaluationReportV1 {
    pub command: String,
    pub status: DirectEvaluationStatusV1,
    pub workload_digest: String,
    pub corpus_digest: String,
    pub fixture_source_repository_commit: String,
    pub fixture_source_repository_tree: String,
    /// Exact execution contract and measured scale inventory that produced
    /// the retained raw candidate evidence.
    pub execution_contract: EvaluationExecutionContractV1,
    /// Each selected profile's immutable material, independently retained so
    /// a report cannot be rebound to a later profile definition.
    pub profile_material_digests: BTreeMap<String, String>,
    /// Canonical digest of `raw_outputs`, including every per-query and
    /// current/10x resource observation.
    pub raw_output_digest: String,
    /// Raw production outputs retained beside the aggregate judgment. These
    /// are sanitized fixture evidence, never a replacement data authority.
    pub raw_outputs: Vec<ProductionCandidateOutputV1>,
    pub profiles: Vec<DirectProfileEvaluationV1>,
}

impl DirectEvaluationReportV1 {
    /// Reconstruct every retained aggregate from the exact sanitized workload
    /// and raw candidate evidence. This is deliberately stricter than JSON
    /// deserialization: a report never becomes a new source of truth.
    pub fn validate_against(
        &self,
        repo_root: &std::path::Path,
        workload: &CandidateWorkloadV1,
    ) -> Result<(), SearchEvalError> {
        let corpus_digest = compute_corpus_digest(repo_root, workload)?;
        if self.command != "compare" {
            return Err(SearchEvalError::Contract(
                "direct evaluation report has an unsupported command".to_owned(),
            ));
        }
        let workload_digest = compute_workload_digest(workload)?;
        if self.workload_digest != workload_digest {
            return Err(SearchEvalError::Contract(
                "direct evaluation report does not bind the checked-in workload".to_owned(),
            ));
        }
        if self.corpus_digest != corpus_digest {
            return Err(SearchEvalError::Contract(
                "direct evaluation report does not bind the byte-exact corpus".to_owned(),
            ));
        }
        if self.fixture_source_repository_commit != workload.source_repository_commit
            || self.fixture_source_repository_tree != workload.source_repository_tree
        {
            return Err(SearchEvalError::Contract(
                "direct evaluation report does not bind the fixture source".to_owned(),
            ));
        }
        if self.execution_contract != workload.execution_contract {
            return Err(SearchEvalError::Contract(
                "direct evaluation report does not bind the execution contract".to_owned(),
            ));
        }
        let profile_digests = profile_material_digests(&self.raw_outputs)?;
        if self.profile_material_digests != profile_digests {
            return Err(SearchEvalError::Contract(
                "direct evaluation report profile material digests do not bind raw outputs"
                    .to_owned(),
            ));
        }
        let raw_digest = raw_output_digest(&self.raw_outputs)?;
        if self.raw_output_digest != raw_digest {
            return Err(SearchEvalError::Contract(
                "direct evaluation report raw output digest does not bind retained outputs"
                    .to_owned(),
            ));
        }
        let reconstructed = evaluate_generated_outputs_against_corpus(
            workload,
            &GenerateCandidateOutputsResultV1 {
                workload_digest,
                outputs: self.raw_outputs.clone(),
            },
            &corpus_digest,
        )?;
        if self != &reconstructed {
            return Err(SearchEvalError::Contract(
                "direct evaluation report aggregates do not match retained raw outputs".to_owned(),
            ));
        }
        Ok(())
    }
}

pub(super) fn profile_material_digests(
    outputs: &[ProductionCandidateOutputV1],
) -> Result<BTreeMap<String, String>, SearchEvalError> {
    let mut digests = BTreeMap::new();
    for output in outputs {
        match digests.insert(
            output.profile_id.clone(),
            output.profile_material_digest.clone(),
        ) {
            Some(previous) if previous != output.profile_material_digest => {
                return Err(SearchEvalError::Contract(format!(
                    "{} has inconsistent profile material digests across partitions",
                    output.profile_id
                )));
            }
            Some(_) | None => {}
        }
    }
    Ok(digests)
}

pub(super) fn raw_output_digest(
    outputs: &[ProductionCandidateOutputV1],
) -> Result<String, SearchEvalError> {
    canonical_sha256(&("tracedecay.search-eval.raw-output-evidence.v1", outputs))
        .map(|digest| digest.as_str().to_owned())
        .map_err(|error| {
            SearchEvalError::Contract(format!("hash raw evaluation evidence: {error}"))
        })
}
