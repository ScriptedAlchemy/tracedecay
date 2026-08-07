//! Retained sanitized evidence for one direct quality evaluation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracedecay_domain::canonical_sha256;

use crate::candidate_output::{
    CandidateWorkloadV1, EvaluationExecutionContractV1, GenerateCandidateOutputsResultV1,
    OptionalStageMeasurementsV1, ProductionCandidateOutputV1, compute_corpus_digest,
    compute_workload_digest,
};
use crate::semantic_native::{SemanticNativeStageResultV1, native_profile_requirements};
use crate::{DirectEvaluationStatusV1, SearchEvalError, evaluate_generated_outputs};

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
    pub optional_stages: OptionalStageMeasurementsV1,
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
    /// Exact execution revisions and measured scale inventory that produced
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
        let corpus_digest = compute_corpus_digest(repo_root, workload)?;
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
        let reconstructed = evaluate_generated_outputs(
            repo_root,
            workload,
            &GenerateCandidateOutputsResultV1 {
                workload_digest,
                outputs: self.raw_outputs.clone(),
            },
        )?;
        if self != &reconstructed {
            return Err(SearchEvalError::Contract(
                "direct evaluation report aggregates do not match retained raw outputs".to_owned(),
            ));
        }
        Ok(())
    }

    /// Require complete genuine native evidence before this report can back a
    /// semantic activation. Baseline-only reports remain useful comparison
    /// evidence, but never authorize an optional-stage profile.
    pub fn validate_for_activation(
        &self,
        repo_root: &std::path::Path,
        workload: &CandidateWorkloadV1,
    ) -> Result<(), SearchEvalError> {
        self.validate_against(repo_root, workload)?;
        if self.status != DirectEvaluationStatusV1::Pass {
            return Err(SearchEvalError::Contract(
                "only a passing direct evaluation report can activate semantics".to_owned(),
            ));
        }
        for output in &self.raw_outputs {
            let requirements = native_profile_requirements(workload, &output.profile_id)
                .map_err(|error| SearchEvalError::Contract(error.to_string()))?;
            let native_resources = output.native_resources.as_ref().ok_or_else(|| {
                SearchEvalError::Contract(format!(
                    "{}:{} has no native current/10x resource evidence",
                    output.profile_id, output.partition
                ))
            })?;
            native_resources
                .validate()
                .map_err(|error| SearchEvalError::Contract(error.to_string()))?;
            for (scale, expected_eligible_chunks) in [
                (
                    "current",
                    workload.execution_contract.exact_eligible_chunks_current,
                ),
                ("10x", workload.execution_contract.exact_eligible_chunks_10x),
            ] {
                let sample = native_resources.samples.get(scale).ok_or_else(|| {
                    SearchEvalError::Contract(format!(
                        "{}:{} lacks native {scale} resource evidence",
                        output.profile_id, output.partition
                    ))
                })?;
                let SemanticNativeStageResultV1::Complete(sample) = sample else {
                    return Err(SearchEvalError::Contract(format!(
                        "{}:{} native {scale} resource evidence is not complete",
                        output.profile_id, output.partition
                    )));
                };
                if sample.provenance.workload_digest != self.workload_digest
                    || sample.provenance.corpus_digest != self.corpus_digest
                    || sample.eligible_chunks != expected_eligible_chunks
                    || sample.measured_queries != output.queries.len() as u64
                    || sample
                        .provenance
                        .vector_generation_id
                        .as_deref()
                        .is_none_or(str::is_empty)
                    || sample
                        .provenance
                        .artifact_digest
                        .as_deref()
                        .is_none_or(str::is_empty)
                {
                    return Err(SearchEvalError::Contract(format!(
                        "{}:{} native {scale} resource provenance is incomplete or unbound",
                        output.profile_id, output.partition
                    )));
                }
                validate_native_measurement_method(&sample.provenance.measurement_method)?;
                if output.resources.get(scale) != sample.as_existing_evaluator_sample().as_ref() {
                    return Err(SearchEvalError::Contract(format!(
                        "{}:{} native {scale} evidence does not match its evaluated resource sample",
                        output.profile_id, output.partition
                    )));
                }
            }
            for query in &output.queries {
                let native = query.native.as_ref().ok_or_else(|| {
                    SearchEvalError::Contract(format!(
                        "{}:{} query {} lacks native evaluation evidence",
                        output.profile_id, output.partition, query.query_id
                    ))
                })?;
                if native.profile_id != output.profile_id || !native.fallback_bytes_unchanged {
                    return Err(SearchEvalError::Contract(format!(
                        "{}:{} query {} has invalid native provenance",
                        output.profile_id, output.partition, query.query_id
                    )));
                }
                validate_required_stage(
                    requirements.semantic_requested,
                    &native.exact_flat_oracle,
                    "semantic",
                    &output.profile_id,
                    &output.partition,
                    &query.query_id,
                )?;
                validate_required_stage(
                    requirements.rerank_requested,
                    &native.rerank.on,
                    "rerank",
                    &output.profile_id,
                    &output.partition,
                    &query.query_id,
                )?;
                validate_required_stage(
                    requirements.rerank_requested,
                    &native.rerank.execution,
                    "rerank execution",
                    &output.profile_id,
                    &output.partition,
                    &query.query_id,
                )?;
            }
        }
        Ok(())
    }

    /// Derive the accepted semantic resource pins from the exact selected
    /// profile's retained train/validation, current/10x native observations.
    /// Configuration ceilings are deliberately not an input.
    pub fn semantic_activation_resource_pins(
        &self,
        evaluated_profile_id: &str,
    ) -> Result<crate::semantic_native::SemanticActivationResourcePinsV1, SearchEvalError> {
        use crate::semantic_native::{
            SemanticActivationResourcePinsV1, SemanticNativeStageResultV1,
        };

        let mut fixed = None;
        let mut resident_bytes = 0_u64;
        let mut output_count = 0_u8;
        let mut sample_count = 0_u8;
        for output in self
            .raw_outputs
            .iter()
            .filter(|output| output.profile_id == evaluated_profile_id)
        {
            output_count = output_count.checked_add(1).ok_or_else(|| {
                SearchEvalError::Contract(
                    "activation resource evidence has too many profile outputs".to_owned(),
                )
            })?;
            let resources = output.native_resources.as_ref().ok_or_else(|| {
                SearchEvalError::Contract(
                    "activation profile lacks native resource evidence".to_owned(),
                )
            })?;
            resources
                .validate()
                .map_err(|error| SearchEvalError::Contract(error.to_string()))?;
            for sample in resources.samples.values() {
                let SemanticNativeStageResultV1::Complete(sample) = sample else {
                    return Err(SearchEvalError::Contract(
                        "activation resource evidence is not complete".to_owned(),
                    ));
                };
                sample_count = sample_count.checked_add(1).ok_or_else(|| {
                    SearchEvalError::Contract(
                        "activation resource evidence has too many samples".to_owned(),
                    )
                })?;
                let observed = (
                    sample.model_bytes.filter(|bytes| *bytes != 0),
                    sample.tokenizer_bytes.filter(|bytes| *bytes != 0),
                    sample.provenance.threads,
                    sample.provenance.max_concurrent_sessions,
                    sample.provenance.batch_size,
                    sample.provenance.sequence_length,
                    sample.provenance.load_deadline_ms,
                );
                match fixed {
                    None => fixed = Some(observed),
                    Some(expected) if expected == observed => {}
                    Some(_) => {
                        return Err(SearchEvalError::Contract(
                            "activation resource samples disagree on artifact or execution pins"
                                .to_owned(),
                        ));
                    }
                }
                resident_bytes = resident_bytes.max(
                    sample
                        .peak_rss_bytes
                        .filter(|bytes| *bytes != 0)
                        .ok_or_else(|| {
                            SearchEvalError::Contract(
                                "activation resource sample lacks peak RSS".to_owned(),
                            )
                        })?,
                );
            }
        }
        let (
            Some(model_bytes),
            Some(tokenizer_bytes),
            threads,
            max_concurrent_sessions,
            batch_size,
            sequence_length,
            load_deadline_ms,
        ) = fixed.ok_or_else(|| {
            SearchEvalError::Contract(
                "activation resource evidence has no selected profile output".to_owned(),
            )
        })?
        else {
            return Err(SearchEvalError::Contract(
                "activation resource evidence lacks exact artifact bytes".to_owned(),
            ));
        };
        if output_count != 2
            || sample_count != 4
            || resident_bytes < model_bytes
            || resident_bytes < tokenizer_bytes
        {
            return Err(SearchEvalError::Contract(
                "activation resource evidence is incomplete or internally inconsistent".to_owned(),
            ));
        }
        Ok(SemanticActivationResourcePinsV1 {
            model_bytes,
            tokenizer_bytes,
            resident_bytes,
            threads,
            max_concurrent_sessions,
            batch_size,
            sequence_length,
            load_deadline_ms,
        })
    }
}

pub(super) fn validate_native_measurement_method(
    measurement_method: &str,
) -> Result<(), SearchEvalError> {
    if measurement_method.contains("DatabaseVectorEvaluationStoreV1")
        || measurement_method.contains("SQLite")
    {
        return Err(SearchEvalError::Contract(
            "native evaluation evidence names the retired SQLite vector authority".to_owned(),
        ));
    }
    Ok(())
}

fn validate_required_stage<T>(
    requested: bool,
    stage: &SemanticNativeStageResultV1<T>,
    stage_name: &str,
    profile_id: &str,
    partition: &str,
    query_id: &str,
) -> Result<(), SearchEvalError> {
    let valid = if requested {
        matches!(stage, SemanticNativeStageResultV1::Complete(_))
    } else {
        matches!(stage, SemanticNativeStageResultV1::NotRequested)
    };
    if valid {
        Ok(())
    } else {
        Err(SearchEvalError::Contract(format!(
            "{profile_id}:{partition} query {query_id} has incomplete {stage_name} evidence"
        )))
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
