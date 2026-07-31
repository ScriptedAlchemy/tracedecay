//! Search-quality evaluator for the production retrieval kernel.
//!
//! Candidate generation, direct (locked-quality) evaluation, and native
//! semantic measurement over a checked-in workload and corpus. The evaluator
//! mounts the production code-index owner and retrieval kernel directly; the
//! only capability it cannot own is the authoritative repository identity of
//! the checkout under evaluation, which the composing binary injects as an
//! [`AdmittedCorpusScopeFn`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub mod candidate_output;
pub mod semantic_native;

pub use candidate_output::{
    AdmittedCorpusScopeFn, CandidateOutputError, CandidateWorkloadV1,
    DirectEvaluatedProfileMaterialV1,
    GenerateCandidateOutputsOptions, GenerateCandidateOutputsResultV1, OptionalStageMeasurementV1,
    OptionalStageMeasurementsV1, ProductionCandidateNativeExecutionAuthorityV1,
    ProductionCandidateNativeGenerationResourcesV1, ProductionCandidateNativeQueryContextV1,
    ProductionCandidateNativeQueryInputsV1, ProductionCandidateNativeResourceContextV1,
    ProductionCandidateOutputV1, ResourceMeasurementStatusV1, WorkloadQueryV1,
    compute_corpus_digest, compute_profile_material_digest, compute_workload_digest,
    direct_evaluated_profile_material, generate_candidate_outputs,
    generate_candidate_outputs_with_native, load_candidate_workload,
    load_direct_evaluated_profile_material, no_admitted_corpus_scope,
    retrieve_partition_query_bytes, validate_workload_for_tuning, write_generate_outputs,
};

/// Returns the nearest-rank percentile from an ascending sample.
///
/// The caller owns sorting so repeated percentile reads can share one sort.
/// Empty samples and percentiles outside `1..=100` return `None`.
pub fn nearest_rank(sorted: &[u64], percentile: usize) -> Option<u64> {
    if sorted.is_empty() || !(1..=100).contains(&percentile) {
        return None;
    }
    let rank = percentile.saturating_mul(sorted.len()).div_ceil(100);
    sorted.get(rank.saturating_sub(1)).copied()
}

const DEFAULT_WORKLOAD: &str =
    "tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json";
const DEFAULT_WORKLOAD_SHA256: &str =
    "78782062ce57b3b0dcea3f82e103c1b6b9e50b362af49d3868b04994db54909b";
const QUERY_BASELINE_PROFILE: &str = "query-fallback";
const SEMANTIC_PROFILE: &str = "hybrid-conservative";
const RERANK_PROFILE: &str = "hybrid-reranked";
const METRIC_SCALE_PPM: u64 = 1_000_000;
const REQUIRED_NATURAL_LANGUAGE_NDCG_GAIN_PPM: u32 = 1;
const MAX_PROTECTED_QUALITY_REGRESSION_PPM: u32 = 0;
const PROTECTED_STRATA: &[&str] = &[
    "config_key",
    "exact_error",
    "exact_flag",
    "exact_path",
    "exact_symbol",
    "qualified_name",
    "quoted_phrase",
    "tool_name",
    "commit_identifier",
];

#[derive(Debug, Error)]
pub enum SearchEvalError {
    #[error(transparent)]
    Candidate(#[from] CandidateOutputError),
    #[error("{0}")]
    Contract(String),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DirectEvaluationStatusV1 {
    Pass,
    Fail,
    Pending,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DirectWorkloadSummaryV1 {
    pub command: &'static str,
    pub status: DirectEvaluationStatusV1,
    pub workload_digest: String,
    pub corpus_digest: String,
    pub query_count: usize,
    pub partition_counts: BTreeMap<String, usize>,
    pub profile_count: usize,
    pub fixture_source_repository_commit: String,
    pub fixture_source_repository_tree: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
pub struct DirectRatioMetricV1 {
    pub numerator: u64,
    pub denominator: u64,
    pub ppm: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectQueryQualityV1 {
    pub recall_at_10: DirectRatioMetricV1,
    pub precision_at_10: DirectRatioMetricV1,
    pub reciprocal_rank_ppm: u32,
    pub ndcg_at_10_ppm: u32,
    pub duplicate_rate: DirectRatioMetricV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
pub struct DirectWorstStratumV1 {
    pub stratum: String,
    pub protected: bool,
    pub relevant_query_count: u64,
    pub recall_at_10: DirectRatioMetricV1,
    pub mean_reciprocal_rank_ppm: u32,
    pub ndcg_at_10_ppm: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
pub struct DirectEvaluationReportV1 {
    pub command: String,
    pub status: DirectEvaluationStatusV1,
    pub workload_digest: String,
    pub corpus_digest: String,
    pub fixture_source_repository_commit: String,
    pub fixture_source_repository_tree: String,
    pub profiles: Vec<DirectProfileEvaluationV1>,
}

/// Genuine activation-eligible output coupling one immutable evaluator report
/// to the exact checked-in domain material it exercised.
#[derive(Clone, Debug)]
pub struct DirectActivationEvaluationV1 {
    report: DirectEvaluationReportV1,
    evaluated_material: DirectEvaluatedProfileMaterialV1,
}

impl DirectActivationEvaluationV1 {
    pub fn into_parts(self) -> (DirectEvaluationReportV1, DirectEvaluatedProfileMaterialV1) {
        (self.report, self.evaluated_material)
    }
}

pub fn default_workload_path(repo_root: &Path) -> PathBuf {
    repo_root.join(DEFAULT_WORKLOAD)
}

fn load_authoritative_default_workload(
    repo_root: &Path,
) -> Result<(PathBuf, CandidateWorkloadV1), SearchEvalError> {
    let canonical_root = fs::canonicalize(repo_root).map_err(|error| {
        SearchEvalError::Contract(format!(
            "canonicalize repository root {}: {error}",
            repo_root.display()
        ))
    })?;
    let expected_path = canonical_root.join(DEFAULT_WORKLOAD);
    let canonical_path = fs::canonicalize(&expected_path).map_err(|error| {
        SearchEvalError::Contract(format!(
            "canonicalize authoritative direct workload {}: {error}",
            expected_path.display()
        ))
    })?;
    if canonical_path != expected_path {
        return Err(SearchEvalError::Contract(
            "authoritative direct workload must be the checked-in non-symlink fixture".to_owned(),
        ));
    }
    let bytes = fs::read(&canonical_path).map_err(|error| {
        SearchEvalError::Contract(format!(
            "read authoritative direct workload {}: {error}",
            canonical_path.display()
        ))
    })?;
    let observed_digest = hex::encode(Sha256::digest(&bytes));
    if observed_digest != DEFAULT_WORKLOAD_SHA256 {
        return Err(SearchEvalError::Contract(format!(
            "authoritative direct workload digest mismatch: expected {DEFAULT_WORKLOAD_SHA256}, observed {observed_digest}"
        )));
    }
    let workload = load_candidate_workload(&canonical_path)?;
    validate_activation_profile_matrix(&workload)?;
    Ok((canonical_path, workload))
}

fn validate_activation_profile_matrix(
    workload: &CandidateWorkloadV1,
) -> Result<(), SearchEvalError> {
    let profile = |profile_id: &str| {
        workload
            .profile_matrix
            .iter()
            .find(|profile| profile.profile_id == profile_id)
            .ok_or_else(|| {
                SearchEvalError::Contract(format!(
                    "authoritative direct workload is missing required profile {profile_id}"
                ))
            })
    };
    let baseline = profile(QUERY_BASELINE_PROFILE)?;
    let semantic = profile(SEMANTIC_PROFILE)?;
    let rerank = profile(RERANK_PROFILE)?;
    if baseline.semantic_weight_ppm != 0 || baseline.rerank_weight_ppm != 0 {
        return Err(SearchEvalError::Contract(
            "query baseline must disable semantic and rerank lanes".to_owned(),
        ));
    }
    if semantic.semantic_weight_ppm == 0 || semantic.rerank_weight_ppm != 0 {
        return Err(SearchEvalError::Contract(
            "semantic comparison profile must enable semantic and disable rerank".to_owned(),
        ));
    }
    if rerank.semantic_weight_ppm != semantic.semantic_weight_ppm
        || rerank.rerank_weight_ppm == 0
        || rerank.lexical_weight_ppm != semantic.lexical_weight_ppm
        || rerank.graph_weight_ppm != semantic.graph_weight_ppm
        || rerank.calibration_threshold_ppm != semantic.calibration_threshold_ppm
    {
        return Err(SearchEvalError::Contract(
            "rerank comparison must differ from the semantic profile only by rerank material"
                .to_owned(),
        ));
    }
    Ok(())
}

fn activation_profile_chain(
    workload: &CandidateWorkloadV1,
    evaluated_profile_id: &str,
) -> Result<Vec<String>, SearchEvalError> {
    validate_activation_profile_matrix(workload)?;
    let profile_ids = match evaluated_profile_id {
        QUERY_BASELINE_PROFILE => vec![QUERY_BASELINE_PROFILE],
        SEMANTIC_PROFILE => vec![QUERY_BASELINE_PROFILE, SEMANTIC_PROFILE],
        RERANK_PROFILE => vec![QUERY_BASELINE_PROFILE, SEMANTIC_PROFILE, RERANK_PROFILE],
        _ => {
            return Err(SearchEvalError::Contract(format!(
                "{evaluated_profile_id} is not an activation-eligible checked-in profile"
            )));
        }
    };
    Ok(profile_ids.into_iter().map(str::to_owned).collect())
}

/// Validate the exact checked-in workload used by configuration activation.
///
/// Ordinary developer comparisons may use an explicit workload, but only this
/// byte-pinned default fixture can mint an activation-eligible evaluation.
pub fn validate_default_activation_workload(
    repo_root: &Path,
) -> Result<DirectWorkloadSummaryV1, SearchEvalError> {
    let (path, _) = load_authoritative_default_workload(repo_root)?;
    validate_direct_workload(repo_root, Some(&path))
}

pub fn validate_direct_workload(
    repo_root: &Path,
    workload_path: Option<&Path>,
) -> Result<DirectWorkloadSummaryV1, SearchEvalError> {
    let path = workload_path.map_or_else(|| default_workload_path(repo_root), Path::to_path_buf);
    let workload = load_candidate_workload(&path)?;
    let mut partition_counts = BTreeMap::new();
    for query in &workload.queries {
        *partition_counts
            .entry(query.partition.clone())
            .or_insert(0usize) += 1;
    }
    Ok(DirectWorkloadSummaryV1 {
        command: "validate",
        status: DirectEvaluationStatusV1::Pass,
        workload_digest: compute_workload_digest(&workload)?,
        corpus_digest: compute_corpus_digest(repo_root, &workload)?,
        query_count: workload.queries.len(),
        partition_counts,
        profile_count: workload.profile_matrix.len(),
        fixture_source_repository_commit: workload.source_repository_commit,
        fixture_source_repository_tree: workload.source_repository_tree,
    })
}

pub fn compare_direct(
    repo_root: &Path,
    workload_path: Option<&Path>,
    profile_ids: Option<&[String]>,
    admitted_scope: AdmittedCorpusScopeFn,
) -> Result<DirectEvaluationReportV1, SearchEvalError> {
    let path = workload_path.map_or_else(|| default_workload_path(repo_root), Path::to_path_buf);
    let workload = load_candidate_workload(&path)?;
    let generated = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
        repo_root,
        workload_path: Some(&path),
        profile_ids,
        admitted_scope,
    })?;
    evaluate_generated_outputs(repo_root, &workload, &generated)
}

/// Run the exact checked-in activation matrix through genuine native
/// semantic/rerank authorities.
///
/// The selected profile determines the required comparison chain:
/// query baseline; semantic with semantic-disabled query ablations; and, for the
/// reranked profile, the same semantic profile with rerank disabled.
pub fn evaluate_default_activation_candidate(
    repo_root: &Path,
    evaluated_profile_id: &str,
    authority: &dyn ProductionCandidateNativeExecutionAuthorityV1,
    admitted_scope: AdmittedCorpusScopeFn,
) -> Result<DirectActivationEvaluationV1, SearchEvalError> {
    let (workload_path, workload) = load_authoritative_default_workload(repo_root)?;
    let profile_ids = activation_profile_chain(&workload, evaluated_profile_id)?;
    let generated = generate_candidate_outputs_with_native(
        &GenerateCandidateOutputsOptions {
            repo_root,
            workload_path: Some(&workload_path),
            profile_ids: Some(&profile_ids),
            admitted_scope,
        },
        authority,
    )?;
    validate_activation_native_matrix(&profile_ids, &generated)?;
    let report = evaluate_generated_outputs(repo_root, &workload, &generated)?;
    if report.status != DirectEvaluationStatusV1::Pass {
        return Err(SearchEvalError::Contract(
            "activation candidate did not pass the required direct comparison matrix".to_owned(),
        ));
    }
    let evaluated_material = direct_evaluated_profile_material(&workload, evaluated_profile_id)?;
    Ok(DirectActivationEvaluationV1 {
        report,
        evaluated_material,
    })
}

fn validate_activation_native_matrix(
    required_profiles: &[String],
    generated: &GenerateCandidateOutputsResultV1,
) -> Result<(), SearchEvalError> {
    let observed_profiles = generated
        .outputs
        .iter()
        .map(|output| output.profile_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let expected_profiles = required_profiles
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    if observed_profiles != expected_profiles {
        return Err(SearchEvalError::Contract(
            "activation evaluation did not execute the required profile matrix".to_owned(),
        ));
    }
    for output in &generated.outputs {
        if output.native_resources.is_none()
            || output.queries.iter().any(|query| query.native.is_none())
        {
            return Err(SearchEvalError::Contract(format!(
                "{}:{} is missing genuine native evaluation evidence",
                output.profile_id, output.partition
            )));
        }
    }
    Ok(())
}

pub fn evaluate_generated_outputs(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
    generated: &GenerateCandidateOutputsResultV1,
) -> Result<DirectEvaluationReportV1, SearchEvalError> {
    validate_workload_for_tuning(workload)?;
    let digest = compute_workload_digest(workload)?;
    if generated.workload_digest != digest {
        return Err(SearchEvalError::Contract(
            "generated outputs do not bind the checked-in workload".to_owned(),
        ));
    }
    let corpus_digest = compute_corpus_digest(repo_root, workload)?;
    validate_output_matrix(workload, generated, &corpus_digest)?;
    let queries: BTreeMap<_, _> = workload
        .queries
        .iter()
        .map(|query| (query.query_id.as_str(), query))
        .collect();
    let mut profiles = generated
        .outputs
        .iter()
        .map(|output| evaluate_profile(workload, &queries, &corpus_digest, output))
        .collect::<Result<Vec<_>, _>>()?;
    profiles.sort_by(|left, right| {
        (&left.profile_id, &left.partition).cmp(&(&right.profile_id, &right.partition))
    });
    Ok(DirectEvaluationReportV1 {
        command: "compare".to_owned(),
        status: aggregate_profile_status(&profiles),
        workload_digest: digest,
        corpus_digest,
        fixture_source_repository_commit: workload.source_repository_commit.clone(),
        fixture_source_repository_tree: workload.source_repository_tree.clone(),
        profiles,
    })
}

fn evaluate_profile(
    workload: &CandidateWorkloadV1,
    queries: &BTreeMap<&str, &WorkloadQueryV1>,
    corpus_digest: &str,
    output: &ProductionCandidateOutputV1,
) -> Result<DirectProfileEvaluationV1, SearchEvalError> {
    if output.schema_version != 2 {
        return Err(SearchEvalError::Contract(format!(
            "{}:{} uses unsupported candidate output schema {}",
            output.profile_id, output.partition, output.schema_version
        )));
    }
    if output.production_boundary != candidate_output::PRODUCTION_BOUNDARY {
        return Err(SearchEvalError::Contract(format!(
            "{}:{} did not run the production boundary",
            output.profile_id, output.partition
        )));
    }
    if output.fixture_source_commit != workload.source_repository_commit
        || output.fixture_source_tree != workload.source_repository_tree
    {
        return Err(SearchEvalError::Contract(format!(
            "{}:{} does not bind the fixture source commit/tree",
            output.profile_id, output.partition
        )));
    }
    if output.seed != candidate_output::EVALUATION_SEED
        || output.cache_state != candidate_output::EVALUATION_CACHE_STATE
    {
        return Err(SearchEvalError::Contract(format!(
            "{}:{} does not report the deterministic seed/cold cache state",
            output.profile_id, output.partition
        )));
    }
    if output.corpus_digest != corpus_digest {
        return Err(SearchEvalError::Contract(format!(
            "{}:{} does not bind the byte-exact corpus",
            output.profile_id, output.partition
        )));
    }
    if output.toolchain.trim().is_empty() || output.hardware.trim().is_empty() {
        return Err(SearchEvalError::Contract(format!(
            "{}:{} is missing its environment summary",
            output.profile_id, output.partition
        )));
    }
    if output.workload_digest != compute_workload_digest(workload)? {
        return Err(SearchEvalError::Contract(format!(
            "{} does not bind the workload",
            output.profile_id
        )));
    }
    let profile = workload
        .profile_matrix
        .iter()
        .find(|profile| profile.profile_id == output.profile_id)
        .ok_or_else(|| {
            SearchEvalError::Contract(format!("unknown output profile_id {}", output.profile_id))
        })?;
    if output.profile_material_digest != compute_profile_material_digest(profile)? {
        return Err(SearchEvalError::Contract(format!(
            "{}:{} does not bind the exact checked-in profile material",
            output.profile_id, output.partition
        )));
    }
    validate_optional_stage_evidence(profile, output)?;
    let mut results = Vec::new();
    let mut seen_queries = BTreeMap::new();
    for row in &output.queries {
        if seen_queries.insert(row.query_id.as_str(), ()).is_some() {
            return Err(SearchEvalError::Contract(format!(
                "{}:{} has duplicate query row {}",
                output.profile_id, output.partition, row.query_id
            )));
        }
        let query = queries
            .get(row.query_id.as_str())
            .ok_or_else(|| SearchEvalError::Contract(format!("unknown query {}", row.query_id)))?;
        if query.partition != output.partition {
            return Err(SearchEvalError::Contract(format!(
                "{} is outside {}",
                row.query_id, output.partition
            )));
        }
        results.push(evaluate_query(query, row)?);
    }
    let expected: Vec<_> = workload
        .queries
        .iter()
        .filter(|query| query.partition == output.partition)
        .map(|query| query.query_id.as_str())
        .collect();
    let missing: Vec<_> = expected
        .iter()
        .copied()
        .filter(|query_id| !seen_queries.contains_key(query_id))
        .collect();
    if !missing.is_empty() {
        return Err(SearchEvalError::Contract(format!(
            "{}:{} is missing query rows: {}",
            output.profile_id,
            output.partition,
            missing.join(", ")
        )));
    }
    results.sort_by(|left, right| left.query_id.cmp(&right.query_id));
    let expected_fallback_digest = workload
        .expected_query_fallback_digests
        .get(&output.partition)
        .ok_or_else(|| {
            SearchEvalError::Contract(format!(
                "missing expected query fallback digest for {}",
                output.partition
            ))
        })?;
    if output.expected_query_fallback_digest != *expected_fallback_digest {
        return Err(SearchEvalError::Contract(format!(
            "{}:{} does not bind the checked-in expected query fallback digest",
            output.profile_id, output.partition
        )));
    }
    let fallback_matches_expected = output.query_fallback_matches_expected
        && output.query_fallback_digest == *expected_fallback_digest;
    let fallback_stable =
        output.fallback_digest == output.query_fallback_digest && fallback_matches_expected;
    let cancellation_bounded =
        output.cancellation == workload.decision_policy.required_cancellation;
    let offline = output.offline == workload.decision_policy.required_offline;
    let resource_status = evaluate_resources(workload, output);
    let failed_queries = results
        .iter()
        .filter(|result| result.status == DirectEvaluationStatusV1::Fail)
        .count();
    let quality = aggregate_quality(&results);
    let hard_invariants_pass = failed_queries == 0
        && fallback_stable
        && cancellation_bounded
        && offline
        && quality.protected_recall_at_10.denominator != 0
        && quality.protected_recall_at_10.numerator == quality.protected_recall_at_10.denominator
        && quality.duplicate_rate.numerator == 0
        && resource_status != DirectEvaluationStatusV1::Fail;
    let status = if !hard_invariants_pass {
        DirectEvaluationStatusV1::Fail
    } else if optional_stages_pending(output.optional_stages)
        || resource_status == DirectEvaluationStatusV1::Pending
    {
        DirectEvaluationStatusV1::Pending
    } else {
        DirectEvaluationStatusV1::Pass
    };
    Ok(DirectProfileEvaluationV1 {
        profile_id: output.profile_id.clone(),
        partition: output.partition.clone(),
        query_count: results.len(),
        failed_queries,
        fallback_stable,
        fallback_matches_expected,
        cancellation_bounded,
        offline,
        resource_status,
        optional_stages: output.optional_stages,
        quality,
        status,
        queries: results,
    })
}

fn validate_optional_stage_evidence(
    profile: &candidate_output::ProfileSpecV1,
    output: &ProductionCandidateOutputV1,
) -> Result<(), SearchEvalError> {
    validate_stage_request(
        profile.semantic_weight_ppm != 0,
        output.optional_stages.semantic,
        "semantic",
        &output
            .queries
            .iter()
            .filter_map(|row| row.native.as_ref())
            .map(|native| &native.exact_flat_oracle)
            .collect::<Vec<_>>(),
        output.queries.len(),
    )?;
    let rerank_results = output
        .queries
        .iter()
        .filter_map(|row| row.native.as_ref())
        .map(|native| (&native.rerank.on, &native.rerank.execution))
        .collect::<Vec<_>>();
    validate_rerank_stage_request(
        profile.rerank_weight_ppm != 0,
        output.optional_stages.rerank,
        &rerank_results,
        output.queries.len(),
    )?;
    for row in &output.queries {
        let Some(native) = &row.native else {
            continue;
        };
        if native.profile_id != output.profile_id || !native.fallback_bytes_unchanged {
            return Err(SearchEvalError::Contract(format!(
                "{}:{} query {} has invalid native profile/fallback binding",
                output.profile_id, output.partition, row.query_id
            )));
        }
    }
    if let Some(evidence) = &output.native_resources {
        evidence
            .validate()
            .map_err(|error| SearchEvalError::Contract(error.to_string()))?;
    }
    Ok(())
}

fn validate_stage_request<T>(
    requested: bool,
    status: OptionalStageMeasurementV1,
    stage: &str,
    results: &[&semantic_native::SemanticNativeStageResultV1<T>],
    query_count: usize,
) -> Result<(), SearchEvalError> {
    use semantic_native::SemanticNativeStageResultV1;

    match (requested, status) {
        (false, OptionalStageMeasurementV1::NotRequested) => {
            if results
                .iter()
                .any(|result| !matches!(result, SemanticNativeStageResultV1::NotRequested))
            {
                return Err(SearchEvalError::Contract(format!(
                    "unrequested {stage} stage reported native execution"
                )));
            }
        }
        (false, _) | (true, OptionalStageMeasurementV1::NotRequested) => {
            return Err(SearchEvalError::Contract(format!(
                "{stage} optional stage status disagrees with its checked-in profile"
            )));
        }
        (true, OptionalStageMeasurementV1::Complete) => {
            if results.len() != query_count
                || results
                    .iter()
                    .any(|result| !matches!(result, SemanticNativeStageResultV1::Complete(_)))
            {
                return Err(SearchEvalError::Contract(format!(
                    "complete {stage} status lacks complete native evidence for every query"
                )));
            }
        }
        (true, OptionalStageMeasurementV1::Pending) => {
            if results.len() == query_count
                && results
                    .iter()
                    .all(|result| matches!(result, SemanticNativeStageResultV1::Complete(_)))
            {
                return Err(SearchEvalError::Contract(format!(
                    "{stage} status is pending despite complete native evidence"
                )));
            }
        }
    }
    Ok(())
}

fn validate_rerank_stage_request<On, Execution>(
    requested: bool,
    status: OptionalStageMeasurementV1,
    results: &[(
        &semantic_native::SemanticNativeStageResultV1<On>,
        &semantic_native::SemanticNativeStageResultV1<Execution>,
    )],
    query_count: usize,
) -> Result<(), SearchEvalError> {
    use semantic_native::SemanticNativeStageResultV1;

    for (on, execution) in results {
        let matching_state = matches!(
            (on, execution),
            (
                SemanticNativeStageResultV1::NotRequested,
                SemanticNativeStageResultV1::NotRequested
            ) | (
                SemanticNativeStageResultV1::Complete(_),
                SemanticNativeStageResultV1::Complete(_)
            ) | (
                SemanticNativeStageResultV1::Pending { .. },
                SemanticNativeStageResultV1::Pending { .. }
            )
        );
        if !matching_state {
            return Err(SearchEvalError::Contract(
                "rerank output and execution evidence disagree".to_owned(),
            ));
        }
    }
    validate_stage_request(
        requested,
        status,
        "rerank",
        &results.iter().map(|(on, _)| *on).collect::<Vec<_>>(),
        query_count,
    )
}

fn validate_output_matrix(
    workload: &CandidateWorkloadV1,
    generated: &GenerateCandidateOutputsResultV1,
    corpus_digest: &str,
) -> Result<(), SearchEvalError> {
    if generated.outputs.is_empty() {
        return Err(SearchEvalError::Contract(
            "generated output matrix must not be empty".to_owned(),
        ));
    }
    let known_profiles: std::collections::BTreeSet<_> = workload
        .profile_matrix
        .iter()
        .map(|profile| profile.profile_id.as_str())
        .collect();
    let mut selected_profiles = std::collections::BTreeSet::new();
    let mut pairs = std::collections::BTreeSet::new();
    for output in &generated.outputs {
        if !known_profiles.contains(output.profile_id.as_str()) {
            return Err(SearchEvalError::Contract(format!(
                "unknown output profile_id {}",
                output.profile_id
            )));
        }
        if output.partition != "train" && output.partition != "validation" {
            return Err(SearchEvalError::Contract(format!(
                "unknown output partition {}",
                output.partition
            )));
        }
        selected_profiles.insert(output.profile_id.as_str());
        if output.corpus_digest != corpus_digest {
            return Err(SearchEvalError::Contract(format!(
                "{}:{} does not bind the byte-exact corpus",
                output.profile_id, output.partition
            )));
        }
        if !pairs.insert((output.profile_id.as_str(), output.partition.as_str())) {
            return Err(SearchEvalError::Contract(format!(
                "duplicate profile/partition {}:{}",
                output.profile_id, output.partition
            )));
        }
    }
    for profile_id in selected_profiles {
        for partition in ["train", "validation"] {
            if !pairs.contains(&(profile_id, partition)) {
                return Err(SearchEvalError::Contract(format!(
                    "missing profile/partition {profile_id}:{partition}"
                )));
            }
        }
    }
    Ok(())
}

fn evaluate_query(
    query: &WorkloadQueryV1,
    row: &candidate_output::QueryCandidateRowV1,
) -> Result<DirectQueryEvaluationV1, SearchEvalError> {
    if row.abstained != row.ranked.is_empty() {
        return Err(SearchEvalError::Contract(format!(
            "{} has inconsistent abstention state",
            row.query_id
        )));
    }
    let label = query.label.as_ref().ok_or_else(|| {
        SearchEvalError::Contract(format!("{} has no checked-in label", query.query_id))
    })?;
    let anchors = label_strings(label, "anchors")?;
    let forbidden_anchors = label_strings(label, "forbidden_anchors")?;
    let forbidden_documents = label_strings(label, "forbidden_documents")?;
    let protected = query
        .strata
        .iter()
        .any(|stratum| PROTECTED_STRATA.contains(&stratum.as_str()));
    let first_useful_rank = row
        .ranked
        .iter()
        .position(|candidate| candidate_matches_any_anchor(candidate, &anchors))
        .map(|rank| rank as u32 + 1);
    let wrong_scope_hits = row
        .ranked
        .iter()
        .filter(|candidate| !query.allowed_scopes.contains(&candidate.scope))
        .count();
    let forbidden_hits = row
        .ranked
        .iter()
        .filter(|candidate| {
            forbidden_anchors.contains(&candidate.anchor)
                || candidate
                    .anchors
                    .iter()
                    .any(|candidate_anchor| forbidden_anchors.contains(candidate_anchor))
                || forbidden_documents.contains(&candidate.document_id)
        })
        .count();
    let expected_no_result = anchors.is_empty();
    let top_10 = row.ranked.iter().take(10).collect::<Vec<_>>();
    let relevant_hits = anchors
        .iter()
        .filter(|anchor| {
            top_10
                .iter()
                .any(|candidate| candidate_matches_anchor(candidate, anchor))
        })
        .count() as u64;
    let precision_hits = top_10
        .iter()
        .filter(|candidate| candidate_matches_any_anchor(candidate, &anchors))
        .count() as u64;
    let duplicate_denominator = row.ranked.len() as u64;
    let unique_candidates = row
        .ranked
        .iter()
        .map(|candidate| candidate.anchor.as_str())
        .collect::<BTreeSet<_>>()
        .len() as u64;
    let duplicate_count = duplicate_denominator.saturating_sub(unique_candidates);
    let quality = DirectQueryQualityV1 {
        recall_at_10: ratio_metric(relevant_hits, anchors.len() as u64),
        precision_at_10: ratio_metric(precision_hits, top_10.len() as u64),
        reciprocal_rank_ppm: first_useful_rank
            .map_or(0, |rank| (METRIC_SCALE_PPM / u64::from(rank)) as u32),
        ndcg_at_10_ppm: ndcg_at_10_ppm(&top_10, &anchors),
        duplicate_rate: ratio_metric(duplicate_count, duplicate_denominator),
    };
    let expected_behavior = if expected_no_result {
        row.ranked.is_empty() || row.abstained
    } else {
        first_useful_rank.is_some()
    };
    let protected_behavior = !protected
        || (quality.recall_at_10.denominator != 0
            && quality.recall_at_10.numerator == quality.recall_at_10.denominator);
    Ok(DirectQueryEvaluationV1 {
        query_id: query.query_id.clone(),
        strata: query.strata.clone(),
        protected,
        first_useful_rank,
        returned_candidates: row.ranked.len(),
        wrong_scope_hits,
        forbidden_hits,
        expected_no_result,
        status: pass_if(
            expected_behavior
                && protected_behavior
                && duplicate_count == 0
                && wrong_scope_hits == 0
                && forbidden_hits == 0,
        ),
        quality,
    })
}

fn candidate_matches_anchor(
    candidate: &candidate_output::RankedCandidateRowV1,
    anchor: &str,
) -> bool {
    candidate.anchor == anchor
        || candidate
            .anchors
            .iter()
            .any(|candidate_anchor| candidate_anchor == anchor)
}

fn candidate_matches_any_anchor(
    candidate: &candidate_output::RankedCandidateRowV1,
    anchors: &[String],
) -> bool {
    anchors
        .iter()
        .any(|anchor| candidate_matches_anchor(candidate, anchor))
}

fn ratio_metric(numerator: u64, denominator: u64) -> DirectRatioMetricV1 {
    let ppm = if denominator == 0 {
        0
    } else {
        u32::try_from(
            u128::from(numerator)
                .saturating_mul(u128::from(METRIC_SCALE_PPM))
                .checked_div(u128::from(denominator))
                .unwrap_or(0)
                .min(u128::from(METRIC_SCALE_PPM)),
        )
        .unwrap_or(METRIC_SCALE_PPM as u32)
    };
    DirectRatioMetricV1 {
        numerator,
        denominator,
        ppm,
    }
}

fn ndcg_at_10_ppm(
    candidates: &[&candidate_output::RankedCandidateRowV1],
    anchors: &[String],
) -> u32 {
    if anchors.is_empty() {
        return 0;
    }
    let dcg = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate_matches_any_anchor(candidate, anchors))
        .map(|(index, _)| 1.0 / ((index + 2) as f64).log2())
        .sum::<f64>();
    let ideal = (0..anchors.len().min(10))
        .map(|index| 1.0 / ((index + 2) as f64).log2())
        .sum::<f64>();
    if ideal == 0.0 {
        0
    } else {
        ((dcg / ideal) * METRIC_SCALE_PPM as f64)
            .round()
            .clamp(0.0, METRIC_SCALE_PPM as f64) as u32
    }
}

fn label_strings(label: &serde_json::Value, field: &str) -> Result<Vec<String>, SearchEvalError> {
    let Some(value) = label.get(field) else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| SearchEvalError::Contract(format!("{field} must be an array")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| SearchEvalError::Contract(format!("{field} must contain strings")))
        })
        .collect()
}

fn aggregate_quality(results: &[DirectQueryEvaluationV1]) -> DirectQualityMetricsV1 {
    let rows = results.iter().collect::<Vec<_>>();
    let aggregate = aggregate_quality_rows(&rows);
    let protected_rows = rows
        .iter()
        .copied()
        .filter(|result| result.protected)
        .collect::<Vec<_>>();
    let protected_recall_at_10 = aggregate_ratio(
        protected_rows
            .iter()
            .map(|result| &result.quality.recall_at_10),
    );
    let strata_names = results
        .iter()
        .flat_map(|result| result.strata.iter().cloned())
        .collect::<BTreeSet<_>>();
    let strata = strata_names
        .into_iter()
        .map(|stratum| {
            let stratum_rows = rows
                .iter()
                .copied()
                .filter(|result| result.strata.contains(&stratum))
                .collect::<Vec<_>>();
            let quality = aggregate_quality_rows(&stratum_rows);
            DirectStratumQualityV1 {
                stratum: stratum.clone(),
                protected: PROTECTED_STRATA.contains(&stratum.as_str()),
                query_count: stratum_rows.len() as u64,
                relevant_query_count: quality.relevant_query_count,
                recall_at_10: quality.recall_at_10,
                precision_at_10: quality.precision_at_10,
                mean_reciprocal_rank_ppm: quality.mean_reciprocal_rank_ppm,
                ndcg_at_10_ppm: quality.ndcg_at_10_ppm,
                duplicate_rate: quality.duplicate_rate,
            }
        })
        .collect::<Vec<_>>();
    let worst_stratum = strata
        .iter()
        .filter(|stratum| stratum.relevant_query_count != 0)
        .min_by(|left, right| {
            (
                left.recall_at_10.ppm,
                left.mean_reciprocal_rank_ppm,
                left.ndcg_at_10_ppm,
                left.stratum.as_str(),
            )
                .cmp(&(
                    right.recall_at_10.ppm,
                    right.mean_reciprocal_rank_ppm,
                    right.ndcg_at_10_ppm,
                    right.stratum.as_str(),
                ))
        })
        .map(|stratum| DirectWorstStratumV1 {
            stratum: stratum.stratum.clone(),
            protected: stratum.protected,
            relevant_query_count: stratum.relevant_query_count,
            recall_at_10: stratum.recall_at_10.clone(),
            mean_reciprocal_rank_ppm: stratum.mean_reciprocal_rank_ppm,
            ndcg_at_10_ppm: stratum.ndcg_at_10_ppm,
        });
    DirectQualityMetricsV1 {
        relevant_query_count: aggregate.relevant_query_count,
        recall_at_10: aggregate.recall_at_10,
        precision_at_10: aggregate.precision_at_10,
        mean_reciprocal_rank_ppm: aggregate.mean_reciprocal_rank_ppm,
        ndcg_at_10_ppm: aggregate.ndcg_at_10_ppm,
        duplicate_rate: aggregate.duplicate_rate,
        protected_recall_at_10,
        strata,
        worst_stratum,
    }
}

struct AggregateQualityRowsV1 {
    relevant_query_count: u64,
    recall_at_10: DirectRatioMetricV1,
    precision_at_10: DirectRatioMetricV1,
    mean_reciprocal_rank_ppm: u32,
    ndcg_at_10_ppm: u32,
    duplicate_rate: DirectRatioMetricV1,
}

fn aggregate_quality_rows(rows: &[&DirectQueryEvaluationV1]) -> AggregateQualityRowsV1 {
    let relevant = rows
        .iter()
        .copied()
        .filter(|result| result.quality.recall_at_10.denominator != 0)
        .collect::<Vec<_>>();
    let relevant_query_count = relevant.len() as u64;
    let mean_reciprocal_rank_ppm = mean_ppm(
        relevant
            .iter()
            .map(|result| result.quality.reciprocal_rank_ppm),
        relevant_query_count,
    );
    let ndcg_at_10_ppm = mean_ppm(
        relevant.iter().map(|result| result.quality.ndcg_at_10_ppm),
        relevant_query_count,
    );
    AggregateQualityRowsV1 {
        relevant_query_count,
        recall_at_10: aggregate_ratio(rows.iter().map(|result| &result.quality.recall_at_10)),
        precision_at_10: aggregate_ratio(
            relevant
                .iter()
                .map(|result| &result.quality.precision_at_10),
        ),
        mean_reciprocal_rank_ppm,
        ndcg_at_10_ppm,
        duplicate_rate: aggregate_ratio(rows.iter().map(|result| &result.quality.duplicate_rate)),
    }
}

fn aggregate_ratio<'a>(
    metrics: impl Iterator<Item = &'a DirectRatioMetricV1>,
) -> DirectRatioMetricV1 {
    let (numerator, denominator) = metrics.fold((0_u64, 0_u64), |totals, metric| {
        (
            totals.0.saturating_add(metric.numerator),
            totals.1.saturating_add(metric.denominator),
        )
    });
    ratio_metric(numerator, denominator)
}

fn mean_ppm(values: impl Iterator<Item = u32>, support: u64) -> u32 {
    if support == 0 {
        return 0;
    }
    let total = values.fold(0_u128, |total, value| total + u128::from(value));
    u32::try_from(total / u128::from(support)).unwrap_or(METRIC_SCALE_PPM as u32)
}

fn evaluate_resources(
    workload: &CandidateWorkloadV1,
    output: &ProductionCandidateOutputV1,
) -> DirectEvaluationStatusV1 {
    if output.resources.len() != 2 {
        return DirectEvaluationStatusV1::Fail;
    }
    let Some(current) = output.resources.get("current") else {
        return DirectEvaluationStatusV1::Fail;
    };
    let Some(ten_x) = output.resources.get("10x") else {
        return DirectEvaluationStatusV1::Fail;
    };
    let Some(expected_ten_x_chunks) = current.eligible_chunks.checked_mul(10) else {
        return DirectEvaluationStatusV1::Fail;
    };
    if current.eligible_chunks == 0 || ten_x.eligible_chunks != expected_ten_x_chunks {
        return DirectEvaluationStatusV1::Fail;
    }
    let mut pending = false;
    for (name, budget) in [
        ("current", &workload.resource_budgets.current),
        ("10x", &workload.resource_budgets.ten_x),
    ] {
        let Some(sample) = output.resources.get(name) else {
            return DirectEvaluationStatusV1::Fail;
        };
        match sample.status {
            ResourceMeasurementStatusV1::Measured => {
                if sample.pending_reason.is_some()
                    || sample.peak_rss_bytes.is_none()
                    || sample.measured_queries != sample.latency_samples_us.len() as u64
                    || sample.measured_queries != output.queries.len() as u64
                    || sample.latency_samples_us.is_empty()
                    || sample
                        .peak_rss_bytes
                        .is_some_and(|peak| peak > budget.maximum_peak_rss_bytes)
                    || p99_latency_us(&sample.latency_samples_us)
                        .is_none_or(|p99| p99 > budget.maximum_p99_latency_us)
                {
                    return DirectEvaluationStatusV1::Fail;
                }
            }
            ResourceMeasurementStatusV1::Pending => {
                if sample.peak_rss_bytes.is_some()
                    || sample.measured_queries != 0
                    || !sample.latency_samples_us.is_empty()
                    || sample
                        .pending_reason
                        .as_deref()
                        .is_none_or(|reason| reason.trim().is_empty())
                {
                    return DirectEvaluationStatusV1::Fail;
                }
                pending = true;
            }
        }
    }
    if pending {
        DirectEvaluationStatusV1::Pending
    } else {
        DirectEvaluationStatusV1::Pass
    }
}

fn p99_latency_us(samples: &[u64]) -> Option<u64> {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    nearest_rank(&ordered, 99)
}

const fn pass_if(condition: bool) -> DirectEvaluationStatusV1 {
    if condition {
        DirectEvaluationStatusV1::Pass
    } else {
        DirectEvaluationStatusV1::Fail
    }
}

const fn optional_stages_pending(stages: OptionalStageMeasurementsV1) -> bool {
    matches!(stages.semantic, OptionalStageMeasurementV1::Pending)
        || matches!(stages.rerank, OptionalStageMeasurementV1::Pending)
}

fn aggregate_profile_status(profiles: &[DirectProfileEvaluationV1]) -> DirectEvaluationStatusV1 {
    if profiles
        .iter()
        .any(|profile| profile.status == DirectEvaluationStatusV1::Fail)
    {
        DirectEvaluationStatusV1::Fail
    } else if profiles
        .iter()
        .any(|profile| profile.status == DirectEvaluationStatusV1::Pending)
    {
        DirectEvaluationStatusV1::Pending
    } else {
        pairwise_candidate_status(profiles)
    }
}

fn pairwise_candidate_status(profiles: &[DirectProfileEvaluationV1]) -> DirectEvaluationStatusV1 {
    let mut unavailable = false;
    for candidate in profiles.iter().filter(|profile| {
        profile.profile_id == SEMANTIC_PROFILE || profile.profile_id == RERANK_PROFILE
    }) {
        let Some(baseline) = profiles.iter().find(|profile| {
            profile.profile_id == QUERY_BASELINE_PROFILE && profile.partition == candidate.partition
        }) else {
            unavailable = true;
            continue;
        };
        let (Some(baseline_natural), Some(candidate_natural)) = (
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
        ) else {
            unavailable = true;
            continue;
        };
        if candidate_natural
            .ndcg_at_10_ppm
            .saturating_sub(baseline_natural.ndcg_at_10_ppm)
            < REQUIRED_NATURAL_LANGUAGE_NDCG_GAIN_PPM
        {
            return DirectEvaluationStatusV1::Fail;
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
                unavailable = true;
                continue;
            };
            let regressions = [
                baseline_stratum
                    .recall_at_10
                    .ppm
                    .saturating_sub(candidate_stratum.recall_at_10.ppm),
                baseline_stratum
                    .mean_reciprocal_rank_ppm
                    .saturating_sub(candidate_stratum.mean_reciprocal_rank_ppm),
                baseline_stratum
                    .ndcg_at_10_ppm
                    .saturating_sub(candidate_stratum.ndcg_at_10_ppm),
            ];
            if regressions
                .into_iter()
                .any(|regression| regression > MAX_PROTECTED_QUALITY_REGRESSION_PPM)
            {
                return DirectEvaluationStatusV1::Fail;
            }
        }
    }
    if unavailable {
        DirectEvaluationStatusV1::Pending
    } else {
        DirectEvaluationStatusV1::Pass
    }
}

#[cfg(test)]
mod tests {
    use super::{
        QUERY_BASELINE_PROFILE, RERANK_PROFILE, SEMANTIC_PROFILE, activation_profile_chain,
        aggregate_profile_status, aggregate_quality, evaluate_query,
        load_authoritative_default_workload, p99_latency_us, ratio_metric,
    };
    use crate::candidate_output::{
        HistoricalQueryExecutionV1, OptionalStageMeasurementV1, OptionalStageMeasurementsV1,
        QueryCandidateRowV1, RankedCandidateRowV1, WorkloadQueryV1,
    };

    fn ranked(anchor: &str) -> RankedCandidateRowV1 {
        RankedCandidateRowV1 {
            anchor: anchor.to_owned(),
            anchors: vec![anchor.to_owned()],
            scope: "research".to_owned(),
            document_id: anchor.to_owned(),
            tier: "exact".to_owned(),
        }
    }

    fn query(id: &str, stratum: &str, anchors: &[&str]) -> WorkloadQueryV1 {
        WorkloadQueryV1 {
            query_id: id.to_owned(),
            partition: "validation".to_owned(),
            strata: vec![stratum.to_owned()],
            query: id.to_owned(),
            allowed_scopes: vec!["research".to_owned()],
            historical_commit: None,
            label: Some(serde_json::json!({ "anchors": anchors })),
        }
    }

    fn row(id: &str, ranked: Vec<RankedCandidateRowV1>) -> QueryCandidateRowV1 {
        QueryCandidateRowV1 {
            query_id: id.to_owned(),
            abstained: ranked.is_empty(),
            ranked,
            historical: HistoricalQueryExecutionV1::NotRequested,
            native: None,
        }
    }

    fn passing_profile(
        profile_id: &str,
        natural_language_ndcg_at_10_ppm: u32,
        protected_mrr_ppm: u32,
    ) -> super::DirectProfileEvaluationV1 {
        let perfect = ratio_metric(1, 1);
        let empty = ratio_metric(0, 0);
        super::DirectProfileEvaluationV1 {
            profile_id: profile_id.to_owned(),
            partition: "validation".to_owned(),
            query_count: 2,
            failed_queries: 0,
            fallback_stable: true,
            fallback_matches_expected: true,
            cancellation_bounded: true,
            offline: true,
            resource_status: super::DirectEvaluationStatusV1::Pass,
            optional_stages: OptionalStageMeasurementsV1 {
                semantic: OptionalStageMeasurementV1::NotRequested,
                rerank: OptionalStageMeasurementV1::NotRequested,
            },
            quality: super::DirectQualityMetricsV1 {
                relevant_query_count: 2,
                recall_at_10: perfect.clone(),
                precision_at_10: perfect.clone(),
                mean_reciprocal_rank_ppm: protected_mrr_ppm,
                ndcg_at_10_ppm: natural_language_ndcg_at_10_ppm,
                duplicate_rate: empty,
                protected_recall_at_10: perfect.clone(),
                strata: vec![
                    super::DirectStratumQualityV1 {
                        stratum: "exact_symbol".to_owned(),
                        protected: true,
                        query_count: 1,
                        relevant_query_count: 1,
                        recall_at_10: perfect.clone(),
                        precision_at_10: perfect.clone(),
                        mean_reciprocal_rank_ppm: protected_mrr_ppm,
                        ndcg_at_10_ppm: protected_mrr_ppm,
                        duplicate_rate: ratio_metric(0, 1),
                    },
                    super::DirectStratumQualityV1 {
                        stratum: "natural_language".to_owned(),
                        protected: false,
                        query_count: 1,
                        relevant_query_count: 1,
                        recall_at_10: perfect.clone(),
                        precision_at_10: perfect,
                        mean_reciprocal_rank_ppm: natural_language_ndcg_at_10_ppm,
                        ndcg_at_10_ppm: natural_language_ndcg_at_10_ppm,
                        duplicate_rate: ratio_metric(0, 1),
                    },
                ],
                worst_stratum: None,
            },
            status: super::DirectEvaluationStatusV1::Pass,
            queries: Vec::new(),
        }
    }

    #[test]
    fn p99_uses_nearest_rank_over_real_samples() {
        assert_eq!(p99_latency_us(&[]), None);
        assert_eq!(p99_latency_us(&[7]), Some(7));
        assert_eq!(
            p99_latency_us(&(1..=100).rev().collect::<Vec<_>>()),
            Some(99)
        );
        assert_eq!(p99_latency_us(&(1..=101).collect::<Vec<_>>()), Some(100));
    }

    #[test]
    fn activation_profile_chain_is_closed_and_ordered() {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let (_, workload) =
            load_authoritative_default_workload(repo_root).expect("authoritative workload");

        assert_eq!(
            activation_profile_chain(&workload, QUERY_BASELINE_PROFILE).expect("query chain"),
            vec![QUERY_BASELINE_PROFILE.to_owned()]
        );
        assert_eq!(
            activation_profile_chain(&workload, SEMANTIC_PROFILE).expect("semantic chain"),
            vec![
                QUERY_BASELINE_PROFILE.to_owned(),
                SEMANTIC_PROFILE.to_owned()
            ]
        );
        assert_eq!(
            activation_profile_chain(&workload, RERANK_PROFILE).expect("rerank chain"),
            vec![
                QUERY_BASELINE_PROFILE.to_owned(),
                SEMANTIC_PROFILE.to_owned(),
                RERANK_PROFILE.to_owned()
            ]
        );
        assert!(activation_profile_chain(&workload, "caller-authored").is_err());
    }

    #[test]
    fn quality_metrics_retain_exact_numerators_denominators_and_worst_stratum() {
        let exact = evaluate_query(
            &query("exact", "exact_symbol", &["a", "b"]),
            &row("exact", vec![ranked("a"), ranked("noise"), ranked("b")]),
        )
        .expect("exact query");
        let natural = evaluate_query(
            &query("natural", "natural_language", &["c"]),
            &row("natural", vec![ranked("noise-2"), ranked("c")]),
        )
        .expect("natural query");
        let quality = aggregate_quality(&[exact.clone(), natural]);

        assert!(exact.protected);
        assert_eq!(
            (
                exact.quality.recall_at_10.numerator,
                exact.quality.recall_at_10.denominator
            ),
            (2, 2)
        );
        assert_eq!(
            (
                exact.quality.precision_at_10.numerator,
                exact.quality.precision_at_10.denominator
            ),
            (2, 3)
        );
        assert_eq!(quality.protected_recall_at_10.ppm, 1_000_000);
        assert_eq!(
            (
                quality.recall_at_10.numerator,
                quality.recall_at_10.denominator
            ),
            (3, 3)
        );
        assert_eq!(
            (
                quality.precision_at_10.numerator,
                quality.precision_at_10.denominator
            ),
            (3, 5)
        );
        assert_eq!(quality.mean_reciprocal_rank_ppm, 750_000);
        assert_eq!(quality.duplicate_rate.numerator, 0);
        assert_eq!(
            quality
                .worst_stratum
                .as_ref()
                .map(|stratum| stratum.stratum.as_str()),
            Some("natural_language")
        );
    }

    #[test]
    fn protected_recall_at_10_and_duplicate_rate_are_hard_query_failures() {
        let mut candidates = (0..10)
            .map(|index| ranked(&format!("noise-{index}")))
            .collect::<Vec<_>>();
        candidates.push(ranked("wanted"));
        candidates.push(ranked("noise-0"));
        let result = evaluate_query(
            &query("late", "qualified_name", &["wanted"]),
            &row("late", candidates),
        )
        .expect("quality result");

        assert_eq!(result.status, super::DirectEvaluationStatusV1::Fail);
        assert_eq!(
            (
                result.quality.recall_at_10.numerator,
                result.quality.recall_at_10.denominator
            ),
            (0, 1)
        );
        assert_eq!(
            (
                result.quality.duplicate_rate.numerator,
                result.quality.duplicate_rate.denominator
            ),
            (1, 12)
        );
    }

    #[test]
    fn candidate_without_pairwise_natural_language_gain_fails() {
        for profile_id in [SEMANTIC_PROFILE, RERANK_PROFILE] {
            let baseline = passing_profile(QUERY_BASELINE_PROFILE, 500_000, 1_000_000);
            let candidate = passing_profile(profile_id, 500_000, 1_000_000);

            assert_eq!(
                aggregate_profile_status(&[baseline, candidate]),
                super::DirectEvaluationStatusV1::Fail
            );
        }
    }

    #[test]
    fn candidate_with_protected_quality_regression_fails() {
        let baseline = passing_profile(QUERY_BASELINE_PROFILE, 500_000, 1_000_000);
        let candidate = passing_profile(SEMANTIC_PROFILE, 600_000, 900_000);

        assert_eq!(
            aggregate_profile_status(&[baseline, candidate]),
            super::DirectEvaluationStatusV1::Fail
        );
    }

    #[test]
    fn candidate_without_pairwise_baseline_remains_pending() {
        let candidate = passing_profile(SEMANTIC_PROFILE, 600_000, 1_000_000);

        assert_eq!(
            aggregate_profile_status(&[candidate]),
            super::DirectEvaluationStatusV1::Pending
        );
    }

    #[test]
    fn candidate_with_pairwise_gain_and_no_regression_passes() {
        let baseline = passing_profile(QUERY_BASELINE_PROFILE, 500_000, 1_000_000);
        let candidate = passing_profile(SEMANTIC_PROFILE, 500_001, 1_000_000);

        assert_eq!(
            aggregate_profile_status(&[baseline, candidate]),
            super::DirectEvaluationStatusV1::Pass
        );
    }

    #[test]
    fn altered_or_relabelled_workload_cannot_mint_activation() {
        let temp = tempfile::tempdir().expect("temp repository");
        let path = temp.path().join(super::DEFAULT_WORKLOAD);
        std::fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture directory");
        let mut bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(super::DEFAULT_WORKLOAD),
        )
        .expect("checked-in workload");
        bytes.push(b'\n');
        std::fs::write(path, bytes).expect("altered workload");

        let error =
            load_authoritative_default_workload(temp.path()).expect_err("digest must reject copy");
        assert!(error.to_string().contains("digest mismatch"));
    }
}
