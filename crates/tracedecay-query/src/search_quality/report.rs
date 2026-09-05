//! Retained sanitized evidence for one direct quality evaluation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracedecay_domain::canonical_sha256;

use super::candidate_output::{
    CandidateWorkloadV1, EvaluationExecutionContractV1, GenerateCandidateOutputsResultV1,
    OptionalStageMeasurementsV1, ProductionCandidateOutputV1, compute_corpus_digest,
    compute_workload_digest,
};
use super::evaluate::{
    DirectEvaluationStatusV1, SearchEvalError, evaluate_generated_outputs_against_corpus,
};
use super::semantic_native::{SemanticNativeStageResultV1, native_profile_requirements};

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
    let mut pairs = candidate
        .iter()
        .filter(|query| {
            query
                .strata
                .iter()
                .any(|stratum| stratum == "natural_language")
        })
        .filter_map(|query| {
            baseline
                .iter()
                .find(|baseline_query| baseline_query.query_id == query.query_id)
                .map(|baseline_query| (query, baseline_query))
        })
        .collect::<Vec<_>>();
    pairs.sort_by_key(|(_, baseline_query)| baseline_query.first_useful_rank == Some(1));
    pairs
}

pub(crate) fn semantic_distance_summary(distances: impl IntoIterator<Item = i64>) -> String {
    let mut distances = distances.into_iter().collect::<Vec<_>>();
    distances.sort_unstable();
    let top_distance = distances.first().copied();
    let second_distance = distances.get(1).copied();
    let top_margin = top_distance
        .zip(second_distance)
        .map(|(top, second)| u64::try_from(i128::from(second) - i128::from(top)).unwrap_or(0));
    let display =
        |value: Option<i64>| value.map_or_else(|| "none".to_owned(), |value| value.to_string());
    let display_margin =
        |value: Option<u64>| value.map_or_else(|| "none".to_owned(), |value| value.to_string());
    format!(
        "semantic_candidates={},top_distance={},second_distance={},top_margin={}",
        distances.len(),
        display(top_distance),
        display(second_distance),
        display_margin(top_margin),
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

    /// Revalidate genuine native activation evidence against a corpus digest
    /// derived by the caller's canonical embedded-corpus authority.
    pub fn validate_for_activation_against_authoritative_corpus(
        &self,
        workload: &CandidateWorkloadV1,
        corpus_digest: &str,
    ) -> Result<(), SearchEvalError> {
        self.validate_against_authoritative_corpus(workload, corpus_digest)?;
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
                if sample.provenance.workload_digest != self.workload_digest
                    || sample.provenance.corpus_digest != self.corpus_digest
                    || sample.eligible_chunks != expected_eligible_chunks
                    || sample.measured_queries != output.queries.len() as u64
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
                if !vector_generation_evidence
                    .accepts(sample.provenance.vector_generation_id.as_deref())
                {
                    return Err(SearchEvalError::Contract(format!(
                        "{}:{} native {scale} vector-generation provenance has the wrong retention state",
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
            if !profile.fallback_matches_expected {
                let observed = self
                    .raw_outputs
                    .iter()
                    .find(|output| {
                        output.profile_id == profile.profile_id
                            && output.partition == profile.partition
                    })
                    .map_or(("<unavailable>", "<unavailable>"), |output| {
                        (
                            output.expected_query_fallback_digest.as_str(),
                            output.query_fallback_digest.as_str(),
                        )
                    });
                return format!(
                    "{}:{} query fallback digest does not match the checked-in pin \
                     `expected_query_fallback_digests.{}`: expected {}, observed {}. \
                     Production retrieval changed; re-pin the workload only after \
                     confirming the query results are the intended ones.",
                    profile.profile_id,
                    profile.partition,
                    profile.partition,
                    observed.0,
                    observed.1,
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
                return format!(
                    "{}:{} resource budget failed",
                    profile.profile_id, profile.partition
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
        let diagnostic = super::evaluate::pairwise_candidate_failure_diagnostic(&self.profiles)
            .unwrap_or_else(|| "pairwise candidate quality failed".to_owned());
        self.pairwise_query_diagnostic()
            .map_or(diagnostic.clone(), |queries| {
                format!("{diagnostic} queries=[{queries}]")
            })
    }

    fn pairwise_query_diagnostic(&self) -> Option<String> {
        for candidate in self.profiles.iter().filter(|profile| {
            profile.profile_id == super::evaluate::SEMANTIC_PROFILE
                || profile.profile_id == super::evaluate::RERANK_PROFILE
        }) {
            let baseline = self.profiles.iter().find(|profile| {
                profile.profile_id == super::evaluate::QUERY_BASELINE_PROFILE
                    && profile.partition == candidate.partition
            })?;
            let baseline_natural = baseline
                .quality
                .strata
                .iter()
                .find(|stratum| stratum.stratum == "natural_language")?;
            let candidate_natural = candidate
                .quality
                .strata
                .iter()
                .find(|stratum| stratum.stratum == "natural_language")?;
            if candidate_natural
                .ndcg_at_10_ppm
                .saturating_sub(baseline_natural.ndcg_at_10_ppm)
                >= super::evaluate::REQUIRED_NATURAL_LANGUAGE_NDCG_GAIN_PPM
            {
                continue;
            }
            let output = self.raw_outputs.iter().find(|output| {
                output.profile_id == candidate.profile_id && output.partition == candidate.partition
            })?;
            let details = pairwise_query_pairs(&candidate.queries, &baseline.queries)
                .into_iter()
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
                    let (oracle_hits, top_distance, relevant_distance) =
                        match &native.exact_flat_oracle {
                            SemanticNativeStageResultV1::Complete(oracle) => (
                                oracle.hits.len().to_string(),
                                oracle
                                    .hits
                                    .first()
                                    .map(|hit| hit.evidence.distance.micros().to_string())
                                    .unwrap_or_else(|| "none".to_owned()),
                                relevant_anchor
                                    .and_then(|anchor| {
                                        oracle.hits.iter().find(|hit| {
                                            hit.candidate.anchor_id.as_str() == anchor
                                        })
                                    })
                                    .map(|hit| hit.evidence.distance.micros().to_string())
                                    .unwrap_or_else(|| "none".to_owned()),
                            ),
                        SemanticNativeStageResultV1::NotRequested => {
                            (
                                "not_requested".to_owned(),
                                "none".to_owned(),
                                "none".to_owned(),
                            )
                        }
                        SemanticNativeStageResultV1::Pending { .. } => {
                            (
                                "pending".to_owned(),
                                "none".to_owned(),
                                "none".to_owned(),
                            )
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
                .collect::<Vec<_>>();
            if !details.is_empty() {
                return Some(details.join(";"));
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
    ) -> Result<super::semantic_native::SemanticActivationResourcePinsV1, SearchEvalError> {
        use super::semantic_native::{
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
                // VmHWM remains useful whole-process diagnostic evidence, but
                // it includes every daemon service plus evaluator-only 10x
                // projection scratch. Activation owns only the warmed model
                // sessions and the retained vector/index read authority.
                // Binding the accepted semantic profile to process-lifetime
                // VmHWM makes unrelated prior work permanently inflate its
                // requirement and can reject an otherwise admissible runtime.
                let sample_resident_bytes = semantic_activation_resident_bytes(
                    sample.model_bytes,
                    sample.tokenizer_bytes,
                    sample.vector_bytes,
                    sample.index_bytes,
                    sample.cache_bytes,
                )
                .ok_or_else(|| {
                    SearchEvalError::Contract(
                        "activation semantic resident evidence overflowed".to_owned(),
                    )
                })?;
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

fn semantic_activation_resident_bytes(
    model_bytes: Option<u64>,
    tokenizer_bytes: Option<u64>,
    vector_bytes: Option<u64>,
    index_bytes: Option<u64>,
    cache_bytes: Option<u64>,
) -> Option<u64> {
    let model_bytes = model_bytes?;
    let tokenizer_bytes = tokenizer_bytes?;
    let vector_bytes = vector_bytes?;
    let index_bytes = index_bytes?;
    let cache_bytes = cache_bytes?;
    cache_bytes
        .max(model_bytes)
        .max(tokenizer_bytes)
        .checked_add(vector_bytes)?
        .checked_add(index_bytes)
}

#[cfg(test)]
mod activation_resource_tests {
    use super::semantic_activation_resident_bytes;

    #[test]
    fn semantic_activation_resident_bytes_exclude_process_lifetime_peak() {
        assert_eq!(
            semantic_activation_resident_bytes(Some(600), Some(20), Some(100), Some(10), Some(750),),
            Some(860)
        );
        assert_eq!(
            semantic_activation_resident_bytes(Some(600), Some(20), Some(100), Some(0), Some(0),),
            Some(700)
        );
        assert_eq!(
            semantic_activation_resident_bytes(Some(u64::MAX), Some(1), Some(1), Some(0), Some(0),),
            None
        );
    }
}

#[cfg(test)]
mod pairwise_and_native_method_tests {
    use super::{
        DirectEvaluationStatusV1, DirectQueryEvaluationV1, DirectQueryQualityV1,
        DirectRatioMetricV1, pairwise_query_pairs, semantic_distance_summary,
        validate_native_measurement_method,
    };

    fn diagnostic_query(query_id: &str, first_useful_rank: u32) -> DirectQueryEvaluationV1 {
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
    fn pairwise_diagnostic_prioritizes_queries_with_improvement_headroom() {
        let candidate = vec![
            diagnostic_query("already-perfect", 1),
            diagnostic_query("can-improve", 2),
        ];
        let baseline = candidate.clone();

        let ordered = pairwise_query_pairs(&candidate, &baseline);

        assert_eq!(ordered.len(), 2);
        assert_eq!(ordered[0].0.query_id, "can-improve");
        assert_eq!(ordered[1].0.query_id, "already-perfect");
    }

    #[test]
    fn semantic_distance_summary_exposes_absolute_confidence_and_ambiguity() {
        assert_eq!(
            semantic_distance_summary([325_542_266, 325_542_266, 400_000_000]),
            "semantic_candidates=3,top_distance=325542266,second_distance=325542266,top_margin=0"
        );
        assert_eq!(
            semantic_distance_summary(std::iter::empty()),
            "semantic_candidates=0,top_distance=none,second_distance=none,top_margin=none"
        );
    }

    #[test]
    fn native_activation_rejects_sqlite_vector_measurement_provenance() {
        let stale = "linux-procfs-v1;projection=DatabaseVectorEvaluationStoreV1(SQLite-CAS)";
        assert!(validate_native_measurement_method(stale).is_err());
        assert!(
            validate_native_measurement_method(
                "linux-procfs-v1;projection=canonical-graph-prepared-generation-v1"
            )
            .is_ok()
        );
        assert!(
            validate_native_measurement_method(
                "linux-procfs-v1;projection-cases=prepare_semantic_evaluation_projection\
                 +GraphVectorGenerationStoreV1(isolated-in-memory-graph,watermark-CAS)"
            )
            .is_ok()
        );
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

impl NativeVectorGenerationEvidence {
    fn accepts(self, value: Option<&str>) -> bool {
        match self {
            Self::Recorded => value.is_some_and(|value| !value.is_empty()),
            Self::Redacted => value.is_none(),
        }
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
