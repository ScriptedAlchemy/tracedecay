//! Direct-evaluation scoring and packaged-profile activation inputs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::candidate_output;
use super::candidate_output::{
    CandidateOutputError, CandidateWorkloadV1, DirectEvaluatedProfileMaterialV1,
    GenerateCandidateOutputsResultV1, OptionalStageMeasurementV1, OptionalStageMeasurementsV1,
    ProductionCandidateOutputV1, ResourceMeasurementStatusV1, ResourceSampleV1, WorkloadQueryV1,
    compute_corpus_digest, compute_profile_material_digest, compute_workload_digest,
    direct_evaluated_profile_material, validate_workload_for_tuning,
};
use super::packaged;
use super::report;
use super::report::{
    DirectEvaluationReportV1, DirectProfileEvaluationV1, DirectQualityMetricsV1,
    DirectQueryEvaluationV1, DirectQueryQualityV1, DirectRatioMetricV1, DirectStratumQualityV1,
    DirectWorstStratumV1, PairedEffectMeasurementV1,
};
use super::semantic_native;

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

pub const QUERY_BASELINE_PROFILE: &str = "query-fallback";
pub const SEMANTIC_PROFILE: &str = "hybrid-conservative";
pub const RERANK_PROFILE: &str = "hybrid-reranked";
const METRIC_SCALE_PPM: u64 = 1_000_000;
const MAX_PROTECTED_QUALITY_REGRESSION_PPM: u32 = 0;
/// Two-sided 95% Student-t critical values by degrees of freedom.
///
/// A degree of freedom between two rows reads the lower row, which is the
/// larger critical value and therefore the wider, more conservative interval.
/// Beyond the last row the interval stays at the 120-degree value rather than
/// narrowing to the normal limit, for the same reason.
const T_CRITICAL_95_BY_DEGREES_OF_FREEDOM: &[(u64, f64)] = &[
    (1, 12.706),
    (2, 4.303),
    (3, 3.182),
    (4, 2.776),
    (5, 2.571),
    (6, 2.447),
    (7, 2.365),
    (8, 2.306),
    (9, 2.262),
    (10, 2.228),
    (12, 2.179),
    (15, 2.131),
    (20, 2.086),
    (25, 2.060),
    (30, 2.042),
    (40, 2.021),
    (60, 2.000),
    (120, 1.980),
];
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

/// Genuine activation-eligible output coupling one immutable evaluator report
/// to the exact checked-in domain material it exercised.
#[derive(Clone, Debug)]
pub struct DirectActivationEvaluationV1 {
    report: DirectEvaluationReportV1,
    evaluated_material: DirectEvaluatedProfileMaterialV1,
}

impl DirectActivationEvaluationV1 {
    /// Read the genuine evaluator report without granting construction or
    /// serialization authority for an activation candidate.
    pub fn report(&self) -> &DirectEvaluationReportV1 {
        &self.report
    }

    pub fn into_parts(self) -> (DirectEvaluationReportV1, DirectEvaluatedProfileMaterialV1) {
        (self.report, self.evaluated_material)
    }

    pub fn from_parts(
        report: DirectEvaluationReportV1,
        evaluated_material: DirectEvaluatedProfileMaterialV1,
    ) -> Self {
        Self {
            report,
            evaluated_material,
        }
    }
}

pub fn load_authoritative_default_workload_metadata() -> Result<CandidateWorkloadV1, SearchEvalError>
{
    let workload = packaged::load_workload()?;
    validate_activation_profile_matrix(&workload)?;
    Ok(workload)
}

pub fn validate_activation_profile_matrix(
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

pub fn activation_profile_chain(
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

pub fn load_default_evaluated_profile_material(
    profile_id: &str,
) -> Result<DirectEvaluatedProfileMaterialV1, SearchEvalError> {
    let workload = load_authoritative_default_workload_metadata()?;
    Ok(direct_evaluated_profile_material(&workload, profile_id)?)
}

pub fn evaluate_generated_outputs(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
    generated: &GenerateCandidateOutputsResultV1,
) -> Result<DirectEvaluationReportV1, SearchEvalError> {
    let corpus_digest = compute_corpus_digest(repo_root, workload)?;
    evaluate_generated_outputs_against_corpus(workload, generated, &corpus_digest)
}

/// Rebuild a report from retained outputs against an already-authoritative
/// corpus digest. Package qualification uses this to validate embedded bytes
/// without materializing the packaged fixture into a temporary directory.
pub fn evaluate_generated_outputs_against_corpus(
    workload: &CandidateWorkloadV1,
    generated: &GenerateCandidateOutputsResultV1,
    corpus_digest: &str,
) -> Result<DirectEvaluationReportV1, SearchEvalError> {
    validate_workload_for_tuning(workload)?;
    let digest = compute_workload_digest(workload)?;
    if generated.workload_digest != digest {
        return Err(SearchEvalError::Contract(
            "generated outputs do not bind the checked-in workload".to_owned(),
        ));
    }
    validate_output_matrix(workload, generated, corpus_digest)?;
    let queries: BTreeMap<_, _> = workload
        .queries
        .iter()
        .map(|query| (query.query_id.as_str(), query))
        .collect();
    let mut profiles = generated
        .outputs
        .iter()
        .map(|output| evaluate_profile(workload, &queries, corpus_digest, output))
        .collect::<Result<Vec<_>, _>>()?;
    profiles.sort_by(|left, right| {
        (&left.profile_id, &left.partition).cmp(&(&right.profile_id, &right.partition))
    });
    let effects = evaluate_paired_effects(workload, &profiles);
    Ok(DirectEvaluationReportV1 {
        command: "compare".to_owned(),
        status: aggregate_profile_status(&profiles, &effects),
        methodology_version: candidate_output::QUALIFICATION_METHODOLOGY_VERSION,
        paired_effects: effects.measurements,
        workload_digest: digest,
        corpus_digest: corpus_digest.to_owned(),
        fixture_source_repository_commit: workload.source_repository_commit.clone(),
        fixture_source_repository_tree: workload.source_repository_tree.clone(),
        execution_contract: workload.execution_contract.clone(),
        profile_material_digests: report::profile_material_digests(&generated.outputs)?,
        raw_output_digest: report::raw_output_digest(&generated.outputs)?,
        raw_outputs: generated.outputs.clone(),
        profiles,
    })
}

#[hotpath::measure(label = "search_eval.compare.evaluate_profile")]
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
    // Within-run byte stability only. Folding the checked-in expectation in
    // here reported every pin drift as "fallback bytes changed", which is a
    // different failure with a different operator remedy; `hard_invariants_pass`
    // below still requires both.
    let fallback_stable = output.fallback_digest == output.query_fallback_digest;
    let cancellation_bounded =
        output.cancellation == workload.decision_policy.required_cancellation;
    let offline = output.offline == workload.decision_policy.required_offline;
    let resource_status = evaluate_resources(output);
    let failed_queries = results
        .iter()
        .filter(|result| result.status == DirectEvaluationStatusV1::Fail)
        .count();
    let quality = aggregate_quality(&results);
    let hard_invariants_pass = failed_queries == 0
        && fallback_stable
        && fallback_matches_expected
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
    let known_profiles: BTreeSet<_> = workload
        .profile_matrix
        .iter()
        .map(|profile| profile.profile_id.as_str())
        .collect();
    let mut selected_profiles = BTreeSet::new();
    let mut pairs = BTreeSet::new();
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

#[hotpath::measure(label = "search_eval.compare.evaluate_query")]
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
    // Recall and ideal gain count labelled targets, not rows. Multiple
    // occurrences or aliases of one target must not earn repeated gain.
    let anchors = anchors.iter().collect::<BTreeSet<_>>();
    let dcg = anchors
        .iter()
        .filter_map(|anchor| {
            candidates
                .iter()
                .position(|candidate| candidate_matches_anchor(candidate, anchor))
        })
        .map(|index| 1.0 / ((index + 2) as f64).log2())
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

#[hotpath::measure(label = "search_eval.compare.evaluate_resources")]
fn evaluate_resources(output: &ProductionCandidateOutputV1) -> DirectEvaluationStatusV1 {
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
    for name in ["current", "10x"] {
        let Some(sample) = output.resources.get(name) else {
            return DirectEvaluationStatusV1::Fail;
        };
        match resource_sample_verdict(sample, output.queries.len() as u64) {
            Some(DirectEvaluationStatusV1::Pending) => pending = true,
            Some(DirectEvaluationStatusV1::Pass) => {}
            _ => return DirectEvaluationStatusV1::Fail,
        }
    }
    if pending {
        DirectEvaluationStatusV1::Pending
    } else {
        DirectEvaluationStatusV1::Pass
    }
}

/// Whether one resource sample is internally consistent, and if so whether
/// it is complete (`Pass`) or still `Pending`. `None` is an inconsistent
/// sample.
///
/// A pending sample has no peak RSS and names its reason. It is either not
/// run at all (no latency samples, zero measured queries) or a complete
/// latency run whose peak RSS the host could not report. The producer observes
/// the process-lifetime high-water mark through Linux `VmHWM`, macOS
/// `ru_maxrss`, or Windows `PeakWorkingSetSize`; these values are not isolated
/// per-stage allocations. Latency evidence without RSS is still evidence;
/// only the RSS half stays pending.
fn resource_sample_verdict(
    sample: &ResourceSampleV1,
    expected_queries: u64,
) -> Option<DirectEvaluationStatusV1> {
    let latency_samples = sample.latency_samples_us.len() as u64;
    if sample.measured_queries != latency_samples {
        return None;
    }
    match sample.status {
        ResourceMeasurementStatusV1::Measured => {
            // A zero high-water mark is not a measurement: every supported
            // sampler reports a positive peak for a process that ran queries,
            // so `Some(0)` is an unavailable observation wearing a measured
            // label. Reject it here rather than let it pass as evidence.
            if sample.pending_reason.is_some()
                || !sample.peak_rss_bytes.is_some_and(|bytes| bytes > 0)
                || sample.measured_queries != expected_queries
                || sample.latency_samples_us.is_empty()
            {
                return None;
            }
            Some(DirectEvaluationStatusV1::Pass)
        }
        ResourceMeasurementStatusV1::Pending => {
            if sample.peak_rss_bytes.is_some()
                || (sample.measured_queries != 0 && sample.measured_queries != expected_queries)
                || sample
                    .pending_reason
                    .as_deref()
                    .is_none_or(|reason| reason.trim().is_empty())
            {
                return None;
            }
            Some(DirectEvaluationStatusV1::Pending)
        }
    }
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

/// Complete verdict for the paired-effect gate, with the retained measurements
/// the report publishes and the first failure's operator diagnostic.
pub(super) struct PairedEffectEvaluationV1 {
    pub(super) status: DirectEvaluationStatusV1,
    pub(super) measurements: Vec<PairedEffectMeasurementV1>,
    pub(super) diagnostic: Option<String>,
}

fn aggregate_profile_status(
    profiles: &[DirectProfileEvaluationV1],
    effects: &PairedEffectEvaluationV1,
) -> DirectEvaluationStatusV1 {
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
        effects.status
    }
}

/// Score every candidate profile against its partition baseline under the
/// workload's predeclared methodology.
///
/// Qualification is decided on the held-out partition alone. The tuning
/// partition is measured with the same rule and reported as evidence, so a
/// profile that only works where it was tuned is visible rather than silent.
pub(super) fn evaluate_paired_effects(
    workload: &CandidateWorkloadV1,
    profiles: &[DirectProfileEvaluationV1],
) -> PairedEffectEvaluationV1 {
    let methodology = &workload.qualification_methodology;
    let mut measurements = Vec::new();
    let mut diagnostic = None;
    let mut unavailable = false;
    for candidate in profiles.iter().filter(|profile| {
        methodology
            .candidate_profile_ids
            .contains(&profile.profile_id)
    }) {
        let Some(baseline) = profiles.iter().find(|profile| {
            profile.profile_id == methodology.baseline_profile_id
                && profile.partition == candidate.partition
        }) else {
            unavailable = true;
            continue;
        };
        // Losing labelled evidence the baseline already retrieved is a
        // regression whatever the mean does, and a protected stratum may not
        // pay for the effect either. Both are floors on the measurement rather
        // than terms in it, so they refuse outright.
        let refusal = match compare_protected_strata(candidate, baseline) {
            ProtectedStratumComparisonV1::Regressed(refusal) => Some(refusal),
            ProtectedStratumComparisonV1::Unmeasured => {
                unavailable = true;
                None
            }
            ProtectedStratumComparisonV1::Held => None,
        };
        if let Some(refusal) = dropped_relevant_label(workload, candidate, baseline).or(refusal) {
            return PairedEffectEvaluationV1 {
                status: DirectEvaluationStatusV1::Fail,
                measurements,
                diagnostic: diagnostic.or(Some(refusal)),
            };
        }
        let measurement = measure_paired_effect(workload, candidate, baseline);
        if measurement.status != DirectEvaluationStatusV1::Pass && measurement.held_out {
            diagnostic = diagnostic.or_else(|| Some(paired_effect_diagnostic(&measurement)));
        }
        measurements.push(measurement);
    }
    let held_out = measurements
        .iter()
        .filter(|measurement| measurement.held_out)
        .collect::<Vec<_>>();
    let status = if unavailable
        || held_out.is_empty()
        || held_out
            .iter()
            .any(|measurement| measurement.status == DirectEvaluationStatusV1::Pending)
    {
        DirectEvaluationStatusV1::Pending
    } else if held_out
        .iter()
        .all(|measurement| measurement.status == DirectEvaluationStatusV1::Pass)
    {
        DirectEvaluationStatusV1::Pass
    } else {
        DirectEvaluationStatusV1::Fail
    };
    PairedEffectEvaluationV1 {
        status,
        measurements,
        diagnostic,
    }
}

/// Whether every protected stratum the baseline scored survived in the
/// candidate.
enum ProtectedStratumComparisonV1 {
    Held,
    Regressed(String),
    /// The candidate did not score a protected stratum the baseline did, so the
    /// comparison is unmeasured rather than passed or failed.
    Unmeasured,
}

/// Compare every protected stratum before the effect is measured at all.
///
/// A protected stratum is exact retrieval an agent already relies on. Semantic
/// gain elsewhere never buys the right to erode it.
fn compare_protected_strata(
    candidate: &DirectProfileEvaluationV1,
    baseline: &DirectProfileEvaluationV1,
) -> ProtectedStratumComparisonV1 {
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
            return ProtectedStratumComparisonV1::Unmeasured;
        };
        let regressed = [
            (
                "recall_at_10_ppm",
                baseline_stratum.recall_at_10.ppm,
                candidate_stratum.recall_at_10.ppm,
            ),
            (
                "mean_reciprocal_rank_ppm",
                baseline_stratum.mean_reciprocal_rank_ppm,
                candidate_stratum.mean_reciprocal_rank_ppm,
            ),
            (
                "ndcg_at_10_ppm",
                baseline_stratum.ndcg_at_10_ppm,
                candidate_stratum.ndcg_at_10_ppm,
            ),
        ]
        .into_iter()
        .find(|(_, baseline_value, candidate_value)| {
            baseline_value.saturating_sub(*candidate_value) > MAX_PROTECTED_QUALITY_REGRESSION_PPM
        });
        if let Some((metric, baseline_value, candidate_value)) = regressed {
            return ProtectedStratumComparisonV1::Regressed(format!(
                "protected stratum regressed: profile={} partition={} stratum={} metric={metric} baseline={baseline_value} candidate={candidate_value} maximum_regression={MAX_PROTECTED_QUALITY_REGRESSION_PPM}",
                candidate.profile_id, candidate.partition, baseline_stratum.stratum,
            ));
        }
    }
    ProtectedStratumComparisonV1::Held
}

/// Measure one candidate profile's paired effect against its partition baseline.
fn measure_paired_effect(
    workload: &CandidateWorkloadV1,
    candidate: &DirectProfileEvaluationV1,
    baseline: &DirectProfileEvaluationV1,
) -> PairedEffectMeasurementV1 {
    let methodology = &workload.qualification_methodology;
    let differences = paired_metric_differences(workload, candidate, baseline);
    let interval = paired_interval_95(&differences);
    let held_out = candidate.partition == methodology.held_out_partition;
    let paired_query_count = differences.len() as u64;
    let sufficient = paired_query_count >= methodology.minimum_effect_queries_per_partition;
    let status = match &interval {
        Some(interval)
            if sufficient
                && interval.mean_ppm
                    >= i64::from(methodology.practical_effect_threshold_ppm)
                && interval.lower_ppm > 0 =>
        {
            DirectEvaluationStatusV1::Pass
        }
        Some(_) => DirectEvaluationStatusV1::Fail,
        // Fewer than two paired needs cannot produce an interval at all. That
        // is an unmeasurable workload, not a measured tie.
        None => DirectEvaluationStatusV1::Pending,
    };
    PairedEffectMeasurementV1 {
        profile_id: candidate.profile_id.clone(),
        partition: candidate.partition.clone(),
        held_out,
        stratum: methodology.effect_stratum.clone(),
        metric: methodology.effect_metric.clone(),
        paired_query_count,
        minimum_paired_query_count: methodology.minimum_effect_queries_per_partition,
        mean_difference_ppm: interval.as_ref().map_or(0, |interval| interval.mean_ppm),
        practical_effect_threshold_ppm: methodology.practical_effect_threshold_ppm,
        confidence_level_ppm: methodology.confidence_level_ppm,
        confidence_interval_lower_ppm: interval.as_ref().map_or(0, |interval| interval.lower_ppm),
        confidence_interval_upper_ppm: interval.as_ref().map_or(0, |interval| interval.upper_ppm),
        status,
    }
}

fn paired_effect_diagnostic(measurement: &PairedEffectMeasurementV1) -> String {
    format!(
        "held-out effect failed: {}",
        paired_effect_summary(measurement)
    )
}

/// The exact numbers a qualification decision was made on, so a refusal can be
/// re-derived by hand from the operator log alone.
pub(super) fn paired_effect_summary(measurement: &PairedEffectMeasurementV1) -> String {
    format!(
        "profile={} partition={} stratum={} metric={} needs={}/{} mean_difference_ppm={} confidence_interval_ppm=[{},{}] confidence_level_ppm={} practical_effect_threshold_ppm={}",
        measurement.profile_id,
        measurement.partition,
        measurement.stratum,
        measurement.metric,
        measurement.paired_query_count,
        measurement.minimum_paired_query_count,
        measurement.mean_difference_ppm,
        measurement.confidence_interval_lower_ppm,
        measurement.confidence_interval_upper_ppm,
        measurement.confidence_level_ppm,
        measurement.practical_effect_threshold_ppm,
    )
}

/// `d(query)` for every effect-stratum need both profiles scored.
fn paired_metric_differences(
    workload: &CandidateWorkloadV1,
    candidate: &DirectProfileEvaluationV1,
    baseline: &DirectProfileEvaluationV1,
) -> Vec<i64> {
    let mut pairs = report::pairwise_query_pairs(
        &candidate.queries,
        &baseline.queries,
        &workload.qualification_methodology.effect_stratum,
    );
    pairs.sort_by(|left, right| left.0.query_id.cmp(&right.0.query_id));
    pairs
        .into_iter()
        .map(|(candidate_query, baseline_query)| {
            i64::from(candidate_query.quality.ndcg_at_10_ppm)
                - i64::from(baseline_query.quality.ndcg_at_10_ppm)
        })
        .collect()
}

struct PairedIntervalV1 {
    mean_ppm: i64,
    lower_ppm: i64,
    upper_ppm: i64,
}

/// Two-sided 95% Student-t interval on the mean paired difference.
///
/// The bounds round outward so a reported interval never claims precision the
/// sample does not support, and a zero-variance sample keeps a degenerate
/// interval at its own mean rather than a fabricated width.
fn paired_interval_95(differences: &[i64]) -> Option<PairedIntervalV1> {
    let count = differences.len();
    if count < 2 {
        return None;
    }
    let population = count as f64;
    let mean = differences.iter().map(|value| *value as f64).sum::<f64>() / population;
    let variance = differences
        .iter()
        .map(|value| (*value as f64 - mean).powi(2))
        .sum::<f64>()
        / (population - 1.0);
    let half_width =
        t_critical_95(count as u64 - 1) * (variance / population).sqrt();
    Some(PairedIntervalV1 {
        mean_ppm: clamp_ppm(mean.round()),
        lower_ppm: clamp_ppm((mean - half_width).floor()),
        upper_ppm: clamp_ppm((mean + half_width).ceil()),
    })
}

fn clamp_ppm(value: f64) -> i64 {
    let scale = METRIC_SCALE_PPM as f64;
    value.clamp(-scale, scale) as i64
}

fn t_critical_95(degrees_of_freedom: u64) -> f64 {
    T_CRITICAL_95_BY_DEGREES_OF_FREEDOM
        .iter()
        .rev()
        .find(|(tabulated, _)| *tabulated <= degrees_of_freedom)
        .map_or(T_CRITICAL_95_BY_DEGREES_OF_FREEDOM[0].1, |(_, value)| *value)
}

/// The first compared need whose candidate retrieves fewer labelled targets
/// than the baseline. Both sides score the same checked-in label set, so the
/// recall numerators compare directly.
fn dropped_relevant_label(
    workload: &CandidateWorkloadV1,
    candidate: &DirectProfileEvaluationV1,
    baseline: &DirectProfileEvaluationV1,
) -> Option<String> {
    let stratum = &workload.qualification_methodology.effect_stratum;
    report::pairwise_query_pairs(&candidate.queries, &baseline.queries, stratum)
        .into_iter()
        .find(|(candidate_query, baseline_query)| {
            candidate_query.quality.recall_at_10.numerator
                < baseline_query.quality.recall_at_10.numerator
        })
        .map(|(candidate_query, baseline_query)| {
            format!(
                "candidate dropped a labelled target the baseline retrieved: profile={} partition={} stratum={stratum} query={} metric=recall_at_10 baseline={}/{} candidate={}/{}",
                candidate.profile_id,
                candidate.partition,
                candidate_query.query_id,
                baseline_query.quality.recall_at_10.numerator,
                baseline_query.quality.recall_at_10.denominator,
                candidate_query.quality.recall_at_10.numerator,
                candidate_query.quality.recall_at_10.denominator,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{
        CandidateWorkloadV1, QUERY_BASELINE_PROFILE, RERANK_PROFILE, SEMANTIC_PROFILE,
        activation_profile_chain, aggregate_profile_status, aggregate_quality,
        evaluate_paired_effects, evaluate_query, load_authoritative_default_workload_metadata,
        ratio_metric,
    };
    use crate::search_quality::candidate_output::{
        HistoricalQueryExecutionV1, OptionalStageMeasurementV1, OptionalStageMeasurementsV1,
        QueryCandidateRowV1, RankedCandidateRowV1, ResourceMeasurementStatusV1, ResourceSampleV1,
        WorkloadQueryV1,
    };
    use crate::search_quality::report::{
        DirectProfileEvaluationV1, DirectQualityMetricsV1, DirectQueryEvaluationV1,
        DirectStratumQualityV1,
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
            need_provenance: None,
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
    ) -> DirectProfileEvaluationV1 {
        let perfect = ratio_metric(1, 1);
        let empty = ratio_metric(0, 0);
        DirectProfileEvaluationV1 {
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
            quality: DirectQualityMetricsV1 {
                relevant_query_count: 2,
                recall_at_10: perfect.clone(),
                precision_at_10: perfect.clone(),
                mean_reciprocal_rank_ppm: protected_mrr_ppm,
                ndcg_at_10_ppm: natural_language_ndcg_at_10_ppm,
                duplicate_rate: empty,
                protected_recall_at_10: perfect.clone(),
                strata: vec![
                    DirectStratumQualityV1 {
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
                    DirectStratumQualityV1 {
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
    fn activation_profile_chain_is_closed_and_ordered() {
        let workload =
            load_authoritative_default_workload_metadata().expect("authoritative workload");

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
    fn ndcg_credits_each_label_once_despite_distinct_candidate_aliases() {
        let query = query("aliases", "natural_language", &["a", "b", "c"]);
        let mut candidates = (0..5)
            .map(|index| {
                let mut candidate = ranked(&format!("symbol-{index}"));
                candidate.anchors.push("a".to_owned());
                candidate
            })
            .collect::<Vec<_>>();
        let repeated = evaluate_query(&query, &row("aliases", candidates.clone())).unwrap();
        assert_eq!(repeated.quality.recall_at_10.numerator, 1);
        assert_eq!(repeated.quality.duplicate_rate.numerator, 0);
        assert_eq!(repeated.quality.ndcg_at_10_ppm, 469_279);

        // Adding genuinely new labelled evidence earns gain at its real rank;
        // extra aliases of the first label did not move its first occurrence.
        candidates.push(ranked("b"));
        candidates.push(ranked("c"));
        let covered = evaluate_query(&query, &row("aliases", candidates)).unwrap();
        assert_eq!(covered.quality.recall_at_10.numerator, 3);
        assert!(covered.quality.ndcg_at_10_ppm > repeated.quality.ndcg_at_10_ppm);
        assert!(covered.quality.ndcg_at_10_ppm < 1_000_000);

        let ideal = evaluate_query(
            &query,
            &row("aliases", vec![ranked("a"), ranked("b"), ranked("c")]),
        )
        .unwrap();
        assert_eq!(ideal.quality.ndcg_at_10_ppm, 1_000_000);

        // One row can satisfy multiple labelled targets; each earns its first
        // gain at that row, rather than being lost to binary row relevance.
        let mut composite = ranked("composite");
        composite.anchors = vec!["a".to_owned(), "b".to_owned()];
        let multiple = evaluate_query(&query, &row("aliases", vec![composite])).unwrap();
        assert_eq!(multiple.quality.recall_at_10.numerator, 2);
        assert_eq!(multiple.quality.ndcg_at_10_ppm, 938_557);
    }

    /// One evaluated effect-stratum need at an exact per-need nDCG. Recall is
    /// held at full so these fixtures exercise the effect gate and not the
    /// dropped-label floor.
    fn effect_need(query_id: &str, ndcg_at_10_ppm: u32) -> DirectQueryEvaluationV1 {
        DirectQueryEvaluationV1 {
            query_id: query_id.to_owned(),
            strata: vec!["natural_language".to_owned()],
            protected: false,
            first_useful_rank: Some(1),
            returned_candidates: 1,
            wrong_scope_hits: 0,
            forbidden_hits: 0,
            expected_no_result: false,
            quality: super::DirectQueryQualityV1 {
                recall_at_10: ratio_metric(1, 1),
                precision_at_10: ratio_metric(1, 1),
                reciprocal_rank_ppm: 1_000_000,
                ndcg_at_10_ppm,
                duplicate_rate: ratio_metric(0, 1),
            },
            status: super::DirectEvaluationStatusV1::Pass,
        }
    }

    fn profile_with_effect_needs(
        profile_id: &str,
        partition: &str,
        per_need_ndcg_at_10_ppm: &[u32],
        protected_mrr_ppm: u32,
    ) -> DirectProfileEvaluationV1 {
        let mean = u32::try_from(
            per_need_ndcg_at_10_ppm
                .iter()
                .map(|value| u64::from(*value))
                .sum::<u64>()
                / per_need_ndcg_at_10_ppm.len() as u64,
        )
        .expect("stratum mean fits a ppm");
        let mut profile = passing_profile(profile_id, mean, protected_mrr_ppm);
        profile.partition = partition.to_owned();
        profile.queries = per_need_ndcg_at_10_ppm
            .iter()
            .enumerate()
            .map(|(index, ndcg)| effect_need(&format!("need-{index:03}"), *ndcg))
            .collect();
        profile
    }

    /// The methodology this build implements, with the need floor relaxed so a
    /// fixture can express a specific effect shape in a handful of needs. The
    /// floor itself is exercised by `effect_below_the_need_floor_is_pending`.
    fn small_sample_methodology_workload(minimum_needs: u64) -> CandidateWorkloadV1 {
        let mut workload =
            load_authoritative_default_workload_metadata().expect("authoritative workload");
        workload
            .qualification_methodology
            .minimum_effect_queries_per_partition = minimum_needs;
        workload
    }

    #[test]
    fn held_out_effect_below_the_practical_threshold_fails() {
        let workload = small_sample_methodology_workload(4);
        let held_out = workload.qualification_methodology.held_out_partition.clone();
        let threshold = workload
            .qualification_methodology
            .practical_effect_threshold_ppm;
        for profile_id in [SEMANTIC_PROFILE, RERANK_PROFILE] {
            // A consistent, tiny gain: the interval clears zero, so only the
            // predeclared practical-effect threshold refuses it.
            let baseline =
                profile_with_effect_needs(QUERY_BASELINE_PROFILE, &held_out, &[500_000; 4], 1_000_000);
            let candidate = profile_with_effect_needs(
                profile_id,
                &held_out,
                &[500_001, 500_002, 500_001, 500_002],
                1_000_000,
            );
            let effects = evaluate_paired_effects(&workload, &[baseline.clone(), candidate.clone()]);

            assert_eq!(effects.status, super::DirectEvaluationStatusV1::Fail);
            assert_eq!(
                aggregate_profile_status(&[baseline, candidate], &effects),
                super::DirectEvaluationStatusV1::Fail
            );
            let diagnostic = effects.diagnostic.expect("a refused effect names itself");
            assert!(diagnostic.contains("mean_difference_ppm=2"), "{diagnostic}");
            assert!(
                diagnostic.contains(&format!("practical_effect_threshold_ppm={threshold}")),
                "{diagnostic}"
            );
        }
    }

    /// A large mean built from a few needs that disagree is exactly the shape
    /// the old stratum-mean bar could not see: the interval covers zero, so the
    /// measurement does not support the claim.
    #[test]
    fn held_out_effect_whose_interval_covers_zero_fails() {
        let workload = small_sample_methodology_workload(4);
        let held_out = workload.qualification_methodology.held_out_partition.clone();
        let baseline =
            profile_with_effect_needs(QUERY_BASELINE_PROFILE, &held_out, &[500_000; 4], 1_000_000);
        let candidate = profile_with_effect_needs(
            SEMANTIC_PROFILE,
            &held_out,
            &[900_000, 100_000, 900_000, 700_000],
            1_000_000,
        );
        let effects = evaluate_paired_effects(&workload, &[baseline, candidate]);

        let measurement = effects
            .measurements
            .first()
            .expect("the held-out effect is measured");
        assert!(measurement.mean_difference_ppm > 0, "{measurement:?}");
        assert!(
            measurement.confidence_interval_lower_ppm < 0,
            "{measurement:?}"
        );
        assert_eq!(effects.status, super::DirectEvaluationStatusV1::Fail);
    }

    #[test]
    fn saturated_baseline_cannot_reach_the_practical_threshold() {
        let workload = small_sample_methodology_workload(4);
        let held_out = workload.qualification_methodology.held_out_partition.clone();
        let baseline =
            profile_with_effect_needs(QUERY_BASELINE_PROFILE, &held_out, &[1_000_000; 4], 1_000_000);
        let candidate =
            profile_with_effect_needs(SEMANTIC_PROFILE, &held_out, &[1_000_000; 4], 1_000_000);
        let effects = evaluate_paired_effects(&workload, &[baseline, candidate]);

        assert_eq!(effects.status, super::DirectEvaluationStatusV1::Fail);
        let measurement = &effects.measurements[0];
        assert_eq!(measurement.mean_difference_ppm, 0);
        assert_eq!(measurement.confidence_interval_lower_ppm, 0);
        assert_eq!(measurement.confidence_interval_upper_ppm, 0);
    }

    /// Fewer paired needs than the predeclared floor is an unmeasured
    /// workload, not a measured tie, so it can neither pass nor be reported as
    /// a genuine loss.
    #[test]
    fn effect_below_the_need_floor_cannot_qualify() {
        let workload = small_sample_methodology_workload(20);
        let held_out = workload.qualification_methodology.held_out_partition.clone();
        let baseline =
            profile_with_effect_needs(QUERY_BASELINE_PROFILE, &held_out, &[500_000; 4], 1_000_000);
        let candidate =
            profile_with_effect_needs(SEMANTIC_PROFILE, &held_out, &[900_000; 4], 1_000_000);
        let effects = evaluate_paired_effects(&workload, &[baseline, candidate]);

        let measurement = &effects.measurements[0];
        assert_eq!(measurement.paired_query_count, 4);
        assert_eq!(measurement.minimum_paired_query_count, 20);
        assert_eq!(measurement.status, super::DirectEvaluationStatusV1::Fail);
        assert_eq!(effects.status, super::DirectEvaluationStatusV1::Fail);
    }

    /// A single paired need cannot produce an interval at all. That is an
    /// unmeasurable workload rather than a decided one.
    #[test]
    fn a_single_paired_need_cannot_qualify_or_refuse() {
        let workload = small_sample_methodology_workload(1);
        let held_out = workload.qualification_methodology.held_out_partition.clone();
        let baseline =
            profile_with_effect_needs(QUERY_BASELINE_PROFILE, &held_out, &[500_000], 1_000_000);
        let candidate =
            profile_with_effect_needs(SEMANTIC_PROFILE, &held_out, &[1_000_000], 1_000_000);
        let effects = evaluate_paired_effects(&workload, &[baseline, candidate]);

        assert_eq!(
            effects.measurements[0].status,
            super::DirectEvaluationStatusV1::Pending
        );
        assert_eq!(effects.status, super::DirectEvaluationStatusV1::Pending);
    }

    /// Winning only where the profile was tuned is a reported measurement, not
    /// a qualification: the held-out partition alone decides.
    #[test]
    fn tuning_partition_gain_alone_does_not_qualify() {
        let workload = small_sample_methodology_workload(4);
        let methodology = &workload.qualification_methodology;
        let tuning = methodology.tuning_partition.clone();
        let held_out = methodology.held_out_partition.clone();
        let profiles = vec![
            profile_with_effect_needs(QUERY_BASELINE_PROFILE, &tuning, &[500_000; 4], 1_000_000),
            profile_with_effect_needs(SEMANTIC_PROFILE, &tuning, &[900_000; 4], 1_000_000),
            profile_with_effect_needs(QUERY_BASELINE_PROFILE, &held_out, &[500_000; 4], 1_000_000),
            profile_with_effect_needs(SEMANTIC_PROFILE, &held_out, &[500_000; 4], 1_000_000),
        ];
        let effects = evaluate_paired_effects(&workload, &profiles);

        let tuning_measurement = effects
            .measurements
            .iter()
            .find(|measurement| !measurement.held_out)
            .expect("the tuning partition is measured as evidence");
        assert_eq!(
            tuning_measurement.status,
            super::DirectEvaluationStatusV1::Pass
        );
        assert_eq!(effects.status, super::DirectEvaluationStatusV1::Fail);
    }

    /// A stratum mean must not buy a candidate the right to lose labelled
    /// evidence: this is the shape the last packaged qualification shipped,
    /// where semantic gain on one natural-language query paid for a labelled
    /// target dropped out of another query's ranking.
    #[test]
    fn candidate_that_drops_a_labelled_target_fails_despite_a_mean_gain() {
        let workload = small_sample_methodology_workload(2);
        let held_out = workload.qualification_methodology.held_out_partition.clone();
        let labels = query("nl-lost", "natural_language", &["a", "b", "c"]);
        let baseline_query = evaluate_query(
            &labels,
            &row("nl-lost", vec![ranked("a"), ranked("noise"), ranked("b")]),
        )
        .expect("baseline query");
        let candidate_query = evaluate_query(
            &labels,
            &row(
                "nl-lost",
                vec![ranked("a"), ranked("displaced"), ranked("noise")],
            ),
        )
        .expect("candidate query");
        assert_eq!(baseline_query.quality.recall_at_10.numerator, 2);
        assert_eq!(candidate_query.quality.recall_at_10.numerator, 1);

        let profile_with = |profile_id: &str, gain: u32, evaluated: DirectQueryEvaluationV1| {
            let mut profile =
                profile_with_effect_needs(profile_id, &held_out, &[gain, gain], 1_000_000);
            profile.queries.push(evaluated);
            profile
        };

        let baseline = profile_with(QUERY_BASELINE_PROFILE, 500_000, baseline_query.clone());
        let candidate = profile_with(SEMANTIC_PROFILE, 900_000, candidate_query);
        let effects = evaluate_paired_effects(&workload, &[baseline.clone(), candidate.clone()]);

        assert_eq!(
            aggregate_profile_status(&[baseline, candidate], &effects),
            super::DirectEvaluationStatusV1::Fail
        );
        let diagnostic = effects
            .diagnostic
            .expect("a dropped labelled target refuses activation");
        assert!(diagnostic.contains("query=nl-lost"), "{diagnostic}");
        assert!(
            diagnostic.contains("metric=recall_at_10 baseline=2/3 candidate=1/3"),
            "{diagnostic}"
        );

        // The same gain measured without losing a labelled target is
        // admissible, so the floor refuses the loss and not the gain.
        let retained = evaluate_paired_effects(
            &workload,
            &[
                profile_with_effect_needs(
                    QUERY_BASELINE_PROFILE,
                    &held_out,
                    &[500_000, 500_000],
                    1_000_000,
                ),
                profile_with_effect_needs(
                    SEMANTIC_PROFILE,
                    &held_out,
                    &[900_000, 900_000],
                    1_000_000,
                ),
            ],
        );
        assert_eq!(retained.status, super::DirectEvaluationStatusV1::Pass);
    }

    #[test]
    fn candidate_with_protected_quality_regression_fails() {
        let workload = small_sample_methodology_workload(4);
        let held_out = workload.qualification_methodology.held_out_partition.clone();
        let baseline =
            profile_with_effect_needs(QUERY_BASELINE_PROFILE, &held_out, &[500_000; 4], 1_000_000);
        let candidate =
            profile_with_effect_needs(SEMANTIC_PROFILE, &held_out, &[900_000; 4], 900_000);
        let effects = evaluate_paired_effects(&workload, &[baseline.clone(), candidate.clone()]);

        assert_eq!(
            aggregate_profile_status(&[baseline, candidate], &effects),
            super::DirectEvaluationStatusV1::Fail
        );
        assert!(
            effects
                .diagnostic
                .is_some_and(|diagnostic| diagnostic.contains("protected stratum regressed")),
        );
    }

    #[test]
    fn candidate_without_pairwise_baseline_remains_pending() {
        let workload = small_sample_methodology_workload(4);
        let held_out = workload.qualification_methodology.held_out_partition.clone();
        let candidate =
            profile_with_effect_needs(SEMANTIC_PROFILE, &held_out, &[900_000; 4], 1_000_000);
        let effects = evaluate_paired_effects(&workload, std::slice::from_ref(&candidate));

        assert_eq!(
            aggregate_profile_status(std::slice::from_ref(&candidate), &effects),
            super::DirectEvaluationStatusV1::Pending
        );
    }

    /// Positive control: a consistent effect above the predeclared threshold on
    /// the held-out partition still qualifies.
    #[test]
    fn consistent_held_out_effect_above_the_threshold_passes() {
        let workload = small_sample_methodology_workload(4);
        let methodology = &workload.qualification_methodology;
        let held_out = methodology.held_out_partition.clone();
        let candidate_needs = [700_000, 690_000, 710_000, 700_000];
        let baseline =
            profile_with_effect_needs(QUERY_BASELINE_PROFILE, &held_out, &[500_000; 4], 1_000_000);
        let candidate =
            profile_with_effect_needs(SEMANTIC_PROFILE, &held_out, &candidate_needs, 1_000_000);
        let effects = evaluate_paired_effects(&workload, &[baseline.clone(), candidate.clone()]);

        let measurement = &effects.measurements[0];
        assert!(measurement.held_out);
        assert_eq!(measurement.paired_query_count, 4);
        assert_eq!(measurement.mean_difference_ppm, 200_000);
        assert!(
            measurement.confidence_interval_lower_ppm > 0,
            "{measurement:?}"
        );
        assert_eq!(effects.status, super::DirectEvaluationStatusV1::Pass);
        assert_eq!(
            aggregate_profile_status(&[baseline, candidate], &effects),
            super::DirectEvaluationStatusV1::Pass
        );
    }

    fn resource_sample(
        status: ResourceMeasurementStatusV1,
        peak_rss_bytes: Option<u64>,
        latency_samples_us: Vec<u64>,
        pending_reason: Option<&str>,
    ) -> ResourceSampleV1 {
        ResourceSampleV1 {
            status,
            eligible_chunks: 64,
            peak_rss_bytes,
            measured_queries: latency_samples_us.len() as u64,
            latency_samples_us,
            pending_reason: pending_reason.map(str::to_owned),
        }
    }

    #[test]
    fn pending_resource_sample_with_a_complete_latency_run_stays_pending() {
        // A supported host API can still fail, so the producer retains every
        // query's latency under `Pending`; the latency half is evidence.
        let sample = resource_sample(
            ResourceMeasurementStatusV1::Pending,
            None,
            vec![10, 20, 30],
            Some("Linux peak RSS measurement is unavailable"),
        );
        assert_eq!(
            super::resource_sample_verdict(&sample, 3),
            Some(super::DirectEvaluationStatusV1::Pending)
        );
        let not_run = resource_sample(
            ResourceMeasurementStatusV1::Pending,
            None,
            Vec::new(),
            Some("native semantic resource measurement pending"),
        );
        assert_eq!(
            super::resource_sample_verdict(&not_run, 3),
            Some(super::DirectEvaluationStatusV1::Pending)
        );
    }

    #[test]
    fn inconsistent_resource_samples_are_rejected() {
        let partial_pending = resource_sample(
            ResourceMeasurementStatusV1::Pending,
            None,
            vec![10, 20],
            Some("Linux peak RSS measurement is unavailable"),
        );
        assert_eq!(
            super::resource_sample_verdict(&partial_pending, 3),
            None,
            "a partial latency run is neither not-run nor complete"
        );
        let pending_with_rss = resource_sample(
            ResourceMeasurementStatusV1::Pending,
            Some(4096),
            vec![10, 20, 30],
            Some("Linux peak RSS measurement is unavailable"),
        );
        assert_eq!(super::resource_sample_verdict(&pending_with_rss, 3), None);
        let unexplained = resource_sample(
            ResourceMeasurementStatusV1::Pending,
            None,
            vec![10, 20, 30],
            Some("  "),
        );
        assert_eq!(super::resource_sample_verdict(&unexplained, 3), None);
        let mut miscounted = resource_sample(
            ResourceMeasurementStatusV1::Measured,
            Some(4096),
            vec![10, 20, 30],
            None,
        );
        miscounted.measured_queries = 2;
        assert_eq!(super::resource_sample_verdict(&miscounted, 3), None);
        let measured = resource_sample(
            ResourceMeasurementStatusV1::Measured,
            Some(4096),
            vec![10, 20, 30],
            None,
        );
        assert_eq!(
            super::resource_sample_verdict(&measured, 3),
            Some(super::DirectEvaluationStatusV1::Pass)
        );
    }

    /// A host that cannot report peak RSS must stay `Pending`; it must never
    /// arrive as a measured zero. Every sampler filters its own non-positive
    /// reading, so a zero reaching the evaluator is an unavailable observation
    /// relabelled as evidence, and the verdict refuses it.
    #[test]
    fn a_zero_peak_rss_is_not_a_measurement() {
        let zero = resource_sample(
            ResourceMeasurementStatusV1::Measured,
            Some(0),
            vec![10, 20, 30],
            None,
        );
        assert_eq!(
            super::resource_sample_verdict(&zero, 3),
            None,
            "a zero peak RSS must not reach the report as passing resource evidence"
        );

        // The same unavailable reading, honestly typed, is still evidence.
        let honest = resource_sample(
            ResourceMeasurementStatusV1::Pending,
            None,
            vec![10, 20, 30],
            Some(
                "Windows peak_rss_bytes is unavailable because K32GetProcessMemoryInfo returned \
                 zero PeakWorkingSetSize",
            ),
        );
        assert_eq!(
            super::resource_sample_verdict(&honest, 3),
            Some(super::DirectEvaluationStatusV1::Pending)
        );
    }
}
