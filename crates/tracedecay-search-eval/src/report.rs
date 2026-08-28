//! Retained sanitized evidence for one direct quality evaluation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracedecay_domain::canonical_sha256;

use crate::candidate_output::{
    CandidateWorkloadV1, EvaluationExecutionContractV1, GenerateCandidateOutputsResultV1,
    OptionalStageMeasurementsV1, ProductionCandidateOutputV1, ResourceSampleV1,
    compute_corpus_digest, compute_workload_digest,
};
use crate::semantic_native::{SemanticNativeStageResultV1, native_profile_requirements};
use crate::{DirectEvaluationStatusV1, SearchEvalError, evaluate_generated_outputs_against_corpus};

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

pub(crate) fn pairwise_query_pairs<'a>(
    candidate: &'a [DirectQueryEvaluationV1],
    baseline: &'a [DirectQueryEvaluationV1],
) -> Vec<(&'a DirectQueryEvaluationV1, &'a DirectQueryEvaluationV1)> {
    let mut pairs: Vec<_> = candidate
        .iter()
        .filter_map(|query| {
            baseline
                .iter()
                .find(|baseline_query| baseline_query.query_id == query.query_id)
                .map(|baseline_query| (query, baseline_query))
        })
        .collect();
    pairs.sort_by_key(|(query, baseline_query)| {
        (
            query.first_useful_rank == Some(1),
            baseline_query.first_useful_rank == Some(1),
        )
    });
    pairs
}

pub(crate) fn semantic_distance_summary(distances: impl IntoIterator<Item = i64>) -> String {
    let mut distances = distances.into_iter().collect::<Vec<_>>();
    distances.sort_unstable();
    let top_distance = distances.first().copied();
    let second_distance = distances.get(1).copied();
    let display =
        |value: Option<i64>| value.map_or_else(|| "absent".to_owned(), |value| value.to_string());
    let top_margin = match (top_distance, second_distance) {
        (Some(top), Some(second)) => u64::try_from(i128::from(second) - i128::from(top))
            .map(|margin| margin.to_string())
            .unwrap_or_else(|_| "overflow".to_owned()),
        _ => "absent".to_owned(),
    };
    format!(
        "semantic_candidates={},top_distance={},second_distance={},top_margin={}",
        distances.len(),
        display(top_distance),
        display(second_distance),
        top_margin,
    )
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
        let corpus_digest = compute_corpus_digest(repo_root, workload)?;
        self.validate_against_authoritative_corpus(workload, &corpus_digest)
    }

    /// Reconstruct this report against the independently loaded workload and
    /// its already-verified corpus binding. This retains every normal report
    /// validation while allowing packaged qualification to avoid materializing
    /// a temporary evaluator fixture.
    pub(crate) fn validate_against_authoritative_corpus(
        &self,
        workload: &CandidateWorkloadV1,
        corpus_digest: &str,
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
            corpus_digest,
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
        self.validate_native_evidence(workload, NativeVectorGenerationEvidence::Recorded)
    }

    /// Validate a redacted portable-qualification report. This is crate-local
    /// because an ordinary evaluator report must retain the actual local
    /// vector-generation provenance it observed.
    pub(crate) fn validate_portable_qualification_against_authoritative_corpus(
        &self,
        workload: &CandidateWorkloadV1,
        corpus_digest: &str,
    ) -> Result<(), PortableNativeQualificationValidationErrorV1> {
        self.validate_against_authoritative_corpus(workload, corpus_digest)
            .map_err(|_| PortableNativeQualificationValidationErrorV1::Report)?;
        self.validate_native_evidence(workload, NativeVectorGenerationEvidence::Redacted)
            .map_err(|_| PortableNativeQualificationValidationErrorV1::NativeEvidence)
    }

    fn validate_native_evidence(
        &self,
        workload: &CandidateWorkloadV1,
        vector_generation_evidence: NativeVectorGenerationEvidence,
    ) -> Result<(), SearchEvalError> {
        if self.status == DirectEvaluationStatusV1::Fail {
            return Err(SearchEvalError::Contract(format!(
                "native activation direct evaluation report failed: {}",
                self.failure_diagnostic()
            )));
        }
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
                if let Some(reason) = native_resource_report_bind_mismatch(
                    &sample.provenance.workload_digest,
                    &self.workload_digest,
                    &sample.provenance.corpus_digest,
                    &self.corpus_digest,
                    sample.eligible_chunks,
                    expected_eligible_chunks,
                    sample.measured_queries,
                    output.queries.len() as u64,
                    sample.provenance.artifact_digest.as_deref(),
                ) {
                    return Err(SearchEvalError::Contract(format!(
                        "{}:{} native {scale} resource provenance is unbound: {reason}",
                        output.profile_id, output.partition
                    )));
                }
                if let Some(reason) = native_vector_generation_retention_mismatch(
                    vector_generation_evidence,
                    sample.provenance.vector_generation_id.as_deref(),
                ) {
                    return Err(SearchEvalError::Contract(format!(
                        "{}:{} native {scale} vector-generation provenance has the wrong retention state: {reason}",
                        output.profile_id, output.partition
                    )));
                }
                validate_native_measurement_method(&sample.provenance.measurement_method)?;
                if let Some(reason) = native_evaluated_resource_mismatch(
                    output.resources.get(scale),
                    sample.as_existing_evaluator_sample(),
                ) {
                    return Err(SearchEvalError::Contract(format!(
                        "{}:{} native {scale} evidence does not match its evaluated resource sample: {reason}",
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
                if let Some(reason) = native_query_provenance_mismatch(
                    &native.profile_id,
                    &output.profile_id,
                    native.fallback_bytes_unchanged,
                ) {
                    return Err(SearchEvalError::Contract(format!(
                        "{}:{} query {} has invalid native provenance: {reason}",
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

    fn failure_diagnostic(&self) -> String {
        if let Some(profile) = self
            .profiles
            .iter()
            .find(|profile| profile.status == DirectEvaluationStatusV1::Fail)
        {
            if let Some(query) = profile
                .queries
                .iter()
                .find(|query| query.status == DirectEvaluationStatusV1::Fail)
            {
                let semantic_confidence = self
                    .raw_outputs
                    .iter()
                    .find(|output| {
                        output.profile_id == profile.profile_id
                            && output.partition == profile.partition
                    })
                    .and_then(|output| {
                        output
                            .queries
                            .iter()
                            .find(|raw| raw.query_id == query.query_id)
                    })
                    .and_then(|raw| raw.native.as_ref())
                    .map(|native| match &native.exact_flat_oracle {
                        SemanticNativeStageResultV1::Complete(oracle) => semantic_distance_summary(
                            oracle.hits.iter().map(|hit| hit.evidence.distance.micros()),
                        ),
                        SemanticNativeStageResultV1::NotRequested => {
                            "semantic_candidates=not_requested".to_owned()
                        }
                        SemanticNativeStageResultV1::Pending { .. } => {
                            "semantic_candidates=pending".to_owned()
                        }
                    })
                    .unwrap_or_else(|| "semantic_candidates=unavailable".to_owned());
                return format!(
                    "{}:{} query {} failed: first_useful_rank={:?} returned_candidates={} wrong_scope_hits={} forbidden_hits={} expected_no_result={} protected={} recall={}/{} duplicates={}/{} {}",
                    profile.profile_id,
                    profile.partition,
                    query.query_id,
                    query.first_useful_rank,
                    query.returned_candidates,
                    query.wrong_scope_hits,
                    query.forbidden_hits,
                    query.expected_no_result,
                    query.protected,
                    query.quality.recall_at_10.numerator,
                    query.quality.recall_at_10.denominator,
                    query.quality.duplicate_rate.numerator,
                    query.quality.duplicate_rate.denominator,
                    semantic_confidence,
                );
            }
            if !profile.fallback_stable {
                return format!(
                    "{}:{} fallback bytes changed",
                    profile.profile_id, profile.partition
                );
            }
            if !profile.cancellation_bounded {
                return format!(
                    "{}:{} cancellation contract failed",
                    profile.profile_id, profile.partition
                );
            }
            if !profile.offline {
                return format!(
                    "{}:{} offline contract failed",
                    profile.profile_id, profile.partition
                );
            }
            if profile.resource_status == DirectEvaluationStatusV1::Fail {
                let reason = self
                    .raw_outputs
                    .iter()
                    .find(|output| {
                        output.profile_id == profile.profile_id
                            && output.partition == profile.partition
                    })
                    .and_then(|output| {
                        crate::evaluate_resource_catalog_failure_reason(
                            &output.resources,
                            output.queries.len(),
                        )
                    })
                    .unwrap_or("resource_catalog");
                return resource_catalog_failure_diagnostic(
                    &profile.profile_id,
                    &profile.partition,
                    Some(reason),
                );
            }
            return format!(
                "{}:{} aggregate quality failed: protected_recall={}/{} duplicates={}/{}",
                profile.profile_id,
                profile.partition,
                profile.quality.protected_recall_at_10.numerator,
                profile.quality.protected_recall_at_10.denominator,
                profile.quality.duplicate_rate.numerator,
                profile.quality.duplicate_rate.denominator,
            );
        }
        let diagnostic = crate::pairwise_candidate_failure_diagnostic(&self.profiles)
            .unwrap_or_else(|| "pairwise candidate quality failed".to_owned());
        self.pairwise_query_diagnostic()
            .map_or(diagnostic.clone(), |queries| {
                format!("{diagnostic} queries=[{queries}]")
            })
    }

    fn pairwise_query_diagnostic(&self) -> Option<String> {
        for candidate in self.profiles.iter().filter(|profile| {
            profile.profile_id == crate::SEMANTIC_PROFILE
                || profile.profile_id == crate::RERANK_PROFILE
        }) {
            let Some(baseline) = self.profiles.iter().find(|profile| {
                profile.profile_id == crate::QUERY_BASELINE_PROFILE
                    && profile.partition == candidate.partition
            }) else {
                continue;
            };
            let Some(output) = self.raw_outputs.iter().find(|output| {
                output.profile_id == candidate.profile_id && output.partition == candidate.partition
            }) else {
                continue;
            };
            if let (Some(baseline_natural), Some(candidate_natural)) = (
                baseline
                    .quality
                    .strata
                    .iter()
                    .find(|stratum| stratum.stratum == "natural_language"),
                candidate
                    .quality
                    .strata
                    .iter()
                    .find(|stratum| stratum.stratum == "natural_language"),
            ) {
                if candidate_natural
                    .ndcg_at_10_ppm
                    .saturating_sub(baseline_natural.ndcg_at_10_ppm)
                    < crate::REQUIRED_NATURAL_LANGUAGE_NDCG_GAIN_PPM
                {
                    let details = pairwise_query_evidence_lines(
                        candidate,
                        baseline,
                        output,
                        "natural_language",
                    );
                    if !details.is_empty() {
                        return Some(details.join(";"));
                    }
                }
            }
            for baseline_stratum in baseline
                .quality
                .strata
                .iter()
                .filter(|stratum| stratum.protected)
            {
                let Some(candidate_stratum) = candidate
                    .quality
                    .strata
                    .iter()
                    .find(|stratum| stratum.stratum == baseline_stratum.stratum)
                else {
                    continue;
                };
                if !protected_stratum_regressed(baseline_stratum, candidate_stratum) {
                    continue;
                }
                let details = pairwise_query_evidence_lines(
                    candidate,
                    baseline,
                    output,
                    &baseline_stratum.stratum,
                );
                if !details.is_empty() {
                    return Some(details.join(";"));
                }
            }
        }
        None
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
                // VmHWM remains whole-process diagnostic evidence. It includes
                // every daemon service plus evaluator-only 10x scratch, so
                // binding it to the accepted semantic profile permanently
                // inflates the requirement and can reject an admissible runtime.
                let sample_resident_bytes = match semantic_activation_resident_bytes(
                    sample.model_bytes,
                    sample.tokenizer_bytes,
                    sample.vector_bytes,
                    sample.index_bytes,
                    sample.cache_bytes,
                ) {
                    SemanticActivationResidentEvidence::Bound(bytes) => bytes,
                    SemanticActivationResidentEvidence::Incomplete(field) => {
                        return Err(SearchEvalError::Contract(format!(
                            "activation semantic resident evidence is incomplete: {field}"
                        )));
                    }
                    SemanticActivationResidentEvidence::Overflowed => {
                        return Err(SearchEvalError::Contract(
                            "activation semantic resident evidence overflowed".to_owned(),
                        ));
                    }
                };
                resident_bytes = resident_bytes.max(sample_resident_bytes);
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
        if let Some(reason) = activation_resource_consistency_mismatch(
            output_count,
            sample_count,
            resident_bytes,
            model_bytes,
            tokenizer_bytes,
        ) {
            return Err(SearchEvalError::Contract(format!(
                "activation resource evidence is internally inconsistent: {reason}"
            )));
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SemanticActivationResidentEvidence {
    Bound(u64),
    Incomplete(&'static str),
    Overflowed,
}

fn semantic_activation_resident_bytes(
    model_bytes: Option<u64>,
    tokenizer_bytes: Option<u64>,
    vector_bytes: Option<u64>,
    index_bytes: Option<u64>,
    cache_bytes: Option<u64>,
) -> SemanticActivationResidentEvidence {
    let Some(model_bytes) = model_bytes else {
        return SemanticActivationResidentEvidence::Incomplete("model_bytes");
    };
    let Some(tokenizer_bytes) = tokenizer_bytes else {
        return SemanticActivationResidentEvidence::Incomplete("tokenizer_bytes");
    };
    let Some(vector_bytes) = vector_bytes else {
        return SemanticActivationResidentEvidence::Incomplete("vector_bytes");
    };
    let Some(index_bytes) = index_bytes else {
        return SemanticActivationResidentEvidence::Incomplete("index_bytes");
    };
    let Some(cache_bytes) = cache_bytes else {
        return SemanticActivationResidentEvidence::Incomplete("cache_bytes");
    };
    match cache_bytes
        .max(model_bytes)
        .max(tokenizer_bytes)
        .checked_add(vector_bytes)
        .and_then(|bytes| bytes.checked_add(index_bytes))
    {
        Some(bytes) => SemanticActivationResidentEvidence::Bound(bytes),
        None => SemanticActivationResidentEvidence::Overflowed,
    }
}

/// Fail-closed classification for the single reconstruction performed while
/// accepting portable native qualification evidence. Aggregate mismatches and
/// missing native evidence intentionally retain distinct package denials.
pub(crate) enum PortableNativeQualificationValidationErrorV1 {
    Report,
    NativeEvidence,
}

#[derive(Clone, Copy)]
enum NativeVectorGenerationEvidence {
    Recorded,
    Redacted,
}

fn native_vector_generation_retention_mismatch(
    evidence: NativeVectorGenerationEvidence,
    value: Option<&str>,
) -> Option<&'static str> {
    match evidence {
        NativeVectorGenerationEvidence::Recorded => match value {
            Some(value) if !value.is_empty() => None,
            Some(_) => Some("empty"),
            None => Some("missing"),
        },
        NativeVectorGenerationEvidence::Redacted => value.map(|_| "present"),
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

fn required_native_stage_mismatch<T>(
    requested: bool,
    stage: &SemanticNativeStageResultV1<T>,
) -> Option<&'static str> {
    match (requested, stage) {
        (true, SemanticNativeStageResultV1::Complete(_))
        | (false, SemanticNativeStageResultV1::NotRequested) => None,
        (true, SemanticNativeStageResultV1::NotRequested) => Some("not_requested"),
        (true, SemanticNativeStageResultV1::Pending { .. })
        | (false, SemanticNativeStageResultV1::Pending { .. }) => Some("pending"),
        (false, SemanticNativeStageResultV1::Complete(_)) => Some("complete"),
    }
}

fn validate_required_stage<T>(
    requested: bool,
    stage: &SemanticNativeStageResultV1<T>,
    stage_name: &str,
    profile_id: &str,
    partition: &str,
    query_id: &str,
) -> Result<(), SearchEvalError> {
    match required_native_stage_mismatch(requested, stage) {
        None => Ok(()),
        Some(reason) => Err(SearchEvalError::Contract(format!(
            "{profile_id}:{partition} query {query_id} has incomplete {stage_name} evidence: {reason}"
        ))),
    }
}

fn native_evaluated_resource_mismatch(
    evaluated: Option<&ResourceSampleV1>,
    native: Option<ResourceSampleV1>,
) -> Option<&'static str> {
    match (evaluated, native.as_ref()) {
        (None, None) => None,
        (None, Some(_)) => Some("evaluated_missing"),
        (Some(_), None) => Some("native_projection"),
        (Some(evaluated), Some(native)) if evaluated == native => None,
        (Some(evaluated), Some(native)) => {
            if evaluated.status != native.status {
                Some("status")
            } else if evaluated.eligible_chunks != native.eligible_chunks {
                Some("eligible_chunks")
            } else if evaluated.peak_rss_bytes != native.peak_rss_bytes {
                Some("peak_rss_bytes")
            } else if evaluated.latency_samples_us != native.latency_samples_us {
                Some("latency_samples_us")
            } else if evaluated.measured_queries != native.measured_queries {
                Some("measured_queries")
            } else if evaluated.pending_reason != native.pending_reason {
                Some("pending_reason")
            } else {
                Some("resource_sample")
            }
        }
    }
}

fn activation_resource_consistency_mismatch(
    output_count: u8,
    sample_count: u8,
    resident_bytes: u64,
    model_bytes: u64,
    tokenizer_bytes: u64,
) -> Option<&'static str> {
    if output_count != 2 {
        Some("output_count")
    } else if sample_count != 4 {
        Some("sample_count")
    } else if resident_bytes < model_bytes {
        Some("resident_bytes_below_model")
    } else if resident_bytes < tokenizer_bytes {
        Some("resident_bytes_below_tokenizer")
    } else {
        None
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

fn protected_stratum_regressed(
    baseline: &DirectStratumQualityV1,
    candidate: &DirectStratumQualityV1,
) -> bool {
    [
        baseline
            .recall_at_10
            .ppm
            .saturating_sub(candidate.recall_at_10.ppm),
        baseline
            .mean_reciprocal_rank_ppm
            .saturating_sub(candidate.mean_reciprocal_rank_ppm),
        baseline
            .ndcg_at_10_ppm
            .saturating_sub(candidate.ndcg_at_10_ppm),
    ]
    .into_iter()
    .any(|regression| regression > crate::MAX_PROTECTED_QUALITY_REGRESSION_PPM)
}

fn pairwise_relevant_distance_label(
    relevant_anchor: Option<&str>,
    oracle_distance_micros: Option<i64>,
) -> String {
    match (relevant_anchor, oracle_distance_micros) {
        (None, _) => "missing_anchor".to_owned(),
        (Some(_), None) => "absent".to_owned(),
        (Some(_), Some(micros)) => micros.to_string(),
    }
}

fn pairwise_query_evidence_lines(
    candidate: &DirectProfileEvaluationV1,
    baseline: &DirectProfileEvaluationV1,
    output: &ProductionCandidateOutputV1,
    stratum: &str,
) -> Vec<String> {
    pairwise_query_pairs(&candidate.queries, &baseline.queries)
        .into_iter()
        .filter(|(query, _)| query.strata.iter().any(|query_stratum| query_stratum == stratum))
        .filter_map(|(query, baseline_query)| {
            let raw = output
                .queries
                .iter()
                .find(|raw| raw.query_id == query.query_id)?;
            let native = raw.native.as_ref()?;
            let semantic_candidates = match &native.measurements.semantic {
                SemanticNativeStageResultV1::Complete(measurement) => {
                    measurement.output_candidates.to_string()
                }
                SemanticNativeStageResultV1::NotRequested => "not_requested".to_owned(),
                SemanticNativeStageResultV1::Pending { .. } => "pending".to_owned(),
            };
            let relevant_anchor = query
                .first_useful_rank
                .and_then(|rank| usize::try_from(rank.saturating_sub(1)).ok())
                .and_then(|index| raw.ranked.get(index))
                .map(|ranked| ranked.anchor.as_str());
            let (oracle_hits, top_distance, relevant_distance) = match &native.exact_flat_oracle {
                SemanticNativeStageResultV1::Complete(oracle) => (
                    oracle.hits.len().to_string(),
                    oracle
                        .hits
                        .first()
                        .map(|hit| hit.evidence.distance.micros().to_string())
                        .unwrap_or_else(|| "none".to_owned()),
                    pairwise_relevant_distance_label(
                        relevant_anchor,
                        relevant_anchor.and_then(|anchor| {
                            oracle.hits.iter().find_map(|hit| {
                                (hit.candidate.anchor_id.as_str() == anchor)
                                    .then(|| hit.evidence.distance.micros())
                            })
                        }),
                    ),
                ),
                SemanticNativeStageResultV1::NotRequested => (
                    "not_requested".to_owned(),
                    "none".to_owned(),
                    "none".to_owned(),
                ),
                SemanticNativeStageResultV1::Pending { .. } => {
                    ("pending".to_owned(), "none".to_owned(), "none".to_owned())
                }
            };
            Some(format!(
                "{}:baseline_rank={:?},candidate_rank={:?},semantic_candidates={},oracle_hits={},top_distance={},relevant_distance={}",
                query.query_id,
                baseline_query.first_useful_rank,
                query.first_useful_rank,
                semantic_candidates,
                oracle_hits,
                top_distance,
                relevant_distance,
            ))
        })
        .collect()
}

fn native_query_provenance_mismatch(
    native_profile_id: &str,
    output_profile_id: &str,
    fallback_bytes_unchanged: bool,
) -> Option<&'static str> {
    if native_profile_id != output_profile_id {
        Some("profile_id")
    } else if !fallback_bytes_unchanged {
        Some("fallback_bytes_unchanged")
    } else {
        None
    }
}

fn native_resource_report_bind_mismatch(
    sample_workload_digest: &str,
    report_workload_digest: &str,
    sample_corpus_digest: &str,
    report_corpus_digest: &str,
    sample_eligible_chunks: u64,
    expected_eligible_chunks: u64,
    sample_measured_queries: u64,
    expected_measured_queries: u64,
    artifact_digest: Option<&str>,
) -> Option<&'static str> {
    if sample_workload_digest != report_workload_digest {
        Some("workload_digest")
    } else if sample_corpus_digest != report_corpus_digest {
        Some("corpus_digest")
    } else if sample_eligible_chunks != expected_eligible_chunks {
        Some("eligible_chunks")
    } else if sample_measured_queries != expected_measured_queries {
        Some("measured_queries")
    } else if artifact_digest.is_none_or(str::is_empty) {
        Some("artifact_digest")
    } else {
        None
    }
}

fn resource_catalog_failure_diagnostic(
    profile_id: &str,
    partition: &str,
    reason: Option<&str>,
) -> String {
    format!(
        "{profile_id}:{partition} resource catalog failed: {}",
        reason.unwrap_or("resource_catalog")
    )
}

#[cfg(test)]
mod tests {
    use super::{
        DirectQueryEvaluationV1, DirectQueryQualityV1, DirectRatioMetricV1, DirectStratumQualityV1,
        NativeVectorGenerationEvidence, SemanticActivationResidentEvidence,
        activation_resource_consistency_mismatch, native_evaluated_resource_mismatch,
        native_query_provenance_mismatch, native_resource_report_bind_mismatch,
        native_vector_generation_retention_mismatch, pairwise_query_pairs,
        pairwise_relevant_distance_label, protected_stratum_regressed,
        required_native_stage_mismatch, resource_catalog_failure_diagnostic,
        semantic_activation_resident_bytes,
    };
    use crate::DirectEvaluationStatusV1;
    use crate::candidate_output::{ResourceMeasurementStatusV1, ResourceSampleV1};
    use crate::semantic_native::SemanticNativeStageResultV1;

    #[test]
    fn resource_catalog_diagnostic_names_the_failed_field() {
        assert_eq!(
            resource_catalog_failure_diagnostic(
                "semantic",
                "validation",
                Some("ten_x_eligible_chunks")
            ),
            "semantic:validation resource catalog failed: ten_x_eligible_chunks"
        );
    }

    #[test]
    fn resource_catalog_diagnostic_does_not_call_a_removed_size_budget() {
        let diagnostic = resource_catalog_failure_diagnostic("semantic", "validation", None);
        assert!(
            !diagnostic.contains("budget"),
            "size-cap budgets were removed; the diagnostic must name the catalog: {diagnostic}"
        );
        assert_eq!(
            diagnostic,
            "semantic:validation resource catalog failed: resource_catalog"
        );
    }

    #[test]
    fn native_report_bind_accepts_matching_digests_and_artifact() {
        assert_eq!(
            native_resource_report_bind_mismatch(
                "wl",
                "wl",
                "co",
                "co",
                2,
                2,
                3,
                3,
                Some("sha256:artifact"),
            ),
            None
        );
    }

    #[test]
    fn native_report_bind_names_workload_before_missing_artifact() {
        assert_eq!(
            native_resource_report_bind_mismatch(
                "sample-wl",
                "report-wl",
                "co",
                "co",
                2,
                2,
                3,
                3,
                None,
            ),
            Some("workload_digest")
        );
    }

    #[test]
    fn native_report_bind_names_a_missing_artifact() {
        assert_eq!(
            native_resource_report_bind_mismatch("wl", "wl", "co", "co", 2, 2, 3, 3, None),
            Some("artifact_digest")
        );
    }

    fn stratum(mrr_ppm: u32) -> DirectStratumQualityV1 {
        let perfect = DirectRatioMetricV1 {
            numerator: 1,
            denominator: 1,
            ppm: 1_000_000,
        };
        DirectStratumQualityV1 {
            stratum: "exact_symbol".to_owned(),
            protected: true,
            query_count: 1,
            relevant_query_count: 1,
            recall_at_10: perfect.clone(),
            precision_at_10: perfect,
            mean_reciprocal_rank_ppm: mrr_ppm,
            ndcg_at_10_ppm: mrr_ppm,
            duplicate_rate: DirectRatioMetricV1 {
                numerator: 0,
                denominator: 1,
                ppm: 0,
            },
        }
    }

    #[test]
    fn protected_stratum_regression_is_detected_for_query_evidence() {
        assert!(!protected_stratum_regressed(
            &stratum(1_000_000),
            &stratum(1_000_000)
        ));
        assert!(protected_stratum_regressed(
            &stratum(1_000_000),
            &stratum(900_000)
        ));
    }

    #[test]
    fn native_query_provenance_accepts_matching_profile_and_unchanged_fallback() {
        assert_eq!(
            native_query_provenance_mismatch("hybrid-conservative", "hybrid-conservative", true),
            None
        );
    }

    #[test]
    fn native_query_provenance_names_profile_before_changed_fallback() {
        assert_eq!(
            native_query_provenance_mismatch("hybrid-reranked", "hybrid-conservative", false),
            Some("profile_id")
        );
    }

    #[test]
    fn native_query_provenance_names_changed_fallback_bytes() {
        assert_eq!(
            native_query_provenance_mismatch("hybrid-conservative", "hybrid-conservative", false),
            Some("fallback_bytes_unchanged")
        );
    }

    #[test]
    fn relevant_distance_names_a_missing_ranked_anchor() {
        assert_eq!(
            pairwise_relevant_distance_label(None, None),
            "missing_anchor"
        );
    }

    #[test]
    fn relevant_distance_names_an_absent_oracle_hit() {
        assert_eq!(
            pairwise_relevant_distance_label(Some("wanted"), None),
            "absent"
        );
    }

    #[test]
    fn relevant_distance_prints_the_oracle_micros() {
        assert_eq!(
            pairwise_relevant_distance_label(Some("wanted"), Some(42)),
            "42"
        );
    }

    #[test]
    fn required_native_stage_accepts_complete_when_requested() {
        assert_eq!(
            required_native_stage_mismatch(true, &SemanticNativeStageResultV1::Complete(())),
            None
        );
        assert_eq!(
            required_native_stage_mismatch(false, &SemanticNativeStageResultV1::<()>::NotRequested),
            None
        );
    }

    #[test]
    fn required_native_stage_names_not_requested_before_pending() {
        assert_eq!(
            required_native_stage_mismatch(true, &SemanticNativeStageResultV1::<()>::NotRequested),
            Some("not_requested")
        );
    }

    #[test]
    fn required_native_stage_names_complete_when_the_stage_was_not_requested() {
        assert_eq!(
            required_native_stage_mismatch(false, &SemanticNativeStageResultV1::Complete(())),
            Some("complete")
        );
    }

    #[test]
    fn recorded_vector_generation_names_missing_before_empty() {
        assert_eq!(
            native_vector_generation_retention_mismatch(
                NativeVectorGenerationEvidence::Recorded,
                None
            ),
            Some("missing")
        );
        assert_eq!(
            native_vector_generation_retention_mismatch(
                NativeVectorGenerationEvidence::Recorded,
                Some("")
            ),
            Some("empty")
        );
        assert_eq!(
            native_vector_generation_retention_mismatch(
                NativeVectorGenerationEvidence::Recorded,
                Some("gen-1")
            ),
            None
        );
    }

    #[test]
    fn redacted_vector_generation_names_a_present_id() {
        assert_eq!(
            native_vector_generation_retention_mismatch(
                NativeVectorGenerationEvidence::Redacted,
                Some("gen-1")
            ),
            Some("present")
        );
        assert_eq!(
            native_vector_generation_retention_mismatch(
                NativeVectorGenerationEvidence::Redacted,
                None
            ),
            None
        );
    }

    #[test]
    fn activation_resource_consistency_names_the_first_failed_pin() {
        assert_eq!(
            activation_resource_consistency_mismatch(1, 4, 8, 4, 2),
            Some("output_count")
        );
        assert_eq!(
            activation_resource_consistency_mismatch(2, 3, 8, 4, 2),
            Some("sample_count")
        );
        assert_eq!(
            activation_resource_consistency_mismatch(2, 4, 3, 4, 2),
            Some("resident_bytes_below_model")
        );
        assert_eq!(
            activation_resource_consistency_mismatch(2, 4, 3, 2, 4),
            Some("resident_bytes_below_tokenizer")
        );
        assert_eq!(
            activation_resource_consistency_mismatch(2, 4, 8, 4, 2),
            None
        );
    }

    #[test]
    fn semantic_activation_resident_bytes_exclude_process_lifetime_peak() {
        assert_eq!(
            semantic_activation_resident_bytes(Some(600), Some(20), Some(100), Some(10), Some(750)),
            SemanticActivationResidentEvidence::Bound(860)
        );
        assert_eq!(
            semantic_activation_resident_bytes(Some(600), Some(20), Some(100), Some(0), Some(0)),
            SemanticActivationResidentEvidence::Bound(700)
        );
        assert_eq!(
            semantic_activation_resident_bytes(Some(u64::MAX), Some(1), Some(1), Some(0), Some(0)),
            SemanticActivationResidentEvidence::Overflowed
        );
        assert_eq!(
            semantic_activation_resident_bytes(None, Some(20), Some(100), Some(10), Some(750)),
            SemanticActivationResidentEvidence::Incomplete("model_bytes")
        );
    }

    fn measured_sample(eligible_chunks: u64, measured_queries: u64) -> ResourceSampleV1 {
        ResourceSampleV1 {
            status: ResourceMeasurementStatusV1::Measured,
            eligible_chunks,
            peak_rss_bytes: Some(8),
            latency_samples_us: vec![1],
            measured_queries,
            pending_reason: None,
        }
    }

    #[test]
    fn native_evaluated_resource_accepts_matching_samples() {
        let sample = measured_sample(2, 3);
        assert_eq!(
            native_evaluated_resource_mismatch(Some(&sample), Some(sample.clone())),
            None
        );
    }

    #[test]
    fn native_evaluated_resource_names_eligible_chunks_before_query_count() {
        let evaluated = measured_sample(2, 3);
        let native = measured_sample(9, 1);
        assert_eq!(
            native_evaluated_resource_mismatch(Some(&evaluated), Some(native)),
            Some("eligible_chunks")
        );
    }

    fn pairwise_query_pair(query_id: &str, first_useful_rank: u32) -> DirectQueryEvaluationV1 {
        let zero = DirectRatioMetricV1 {
            numerator: 0,
            denominator: 0,
            ppm: 0,
        };
        DirectQueryEvaluationV1 {
            query_id: query_id.to_owned(),
            strata: vec!["natural_language".to_owned()],
            protected: false,
            first_useful_rank: Some(first_useful_rank),
            returned_candidates: 2,
            wrong_scope_hits: 0,
            forbidden_hits: 0,
            expected_no_result: false,
            quality: DirectQueryQualityV1 {
                recall_at_10: zero.clone(),
                precision_at_10: zero.clone(),
                reciprocal_rank_ppm: 0,
                ndcg_at_10_ppm: 0,
                duplicate_rate: zero,
            },
            status: DirectEvaluationStatusV1::Pass,
        }
    }

    #[test]
    fn pairwise_query_pairs_prioritize_queries_with_improvement_headroom() {
        let candidate = vec![
            pairwise_query_pair("already-perfect", 1),
            pairwise_query_pair("can-improve", 2),
        ];
        let baseline = candidate.clone();
        let ordered = pairwise_query_pairs(&candidate, &baseline);
        assert_eq!(ordered.len(), 2);
        assert_eq!(ordered[0].0.query_id, "can-improve");
        assert_eq!(ordered[1].0.query_id, "already-perfect");
    }

    #[test]
    fn pairwise_query_pairs_keep_candidate_headroom_ahead_of_a_perfect_baseline() {
        let candidate = vec![
            pairwise_query_pair("regressed", 2),
            pairwise_query_pair("both-perfect", 1),
        ];
        let mut baseline = candidate.clone();
        baseline[0].first_useful_rank = Some(1);
        let ordered = pairwise_query_pairs(&candidate, &baseline);
        assert_eq!(ordered[0].0.query_id, "regressed");
        assert_eq!(ordered[1].0.query_id, "both-perfect");
    }

    #[test]
    fn native_evaluated_resource_names_a_missing_projection() {
        let evaluated = measured_sample(2, 3);
        assert_eq!(
            native_evaluated_resource_mismatch(Some(&evaluated), None),
            Some("native_projection")
        );
        assert_eq!(
            native_evaluated_resource_mismatch(None, Some(evaluated)),
            Some("evaluated_missing")
        );
    }
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
