//! Direct-evaluation scoring and packaged-profile activation inputs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::candidate_output;
use super::candidate_output::{
    CandidateOutputError, CandidateWorkloadV1, GenerateCandidateOutputsResultV1,
    ProductionCandidateOutputV1, ResourceMeasurementStatusV1, ResourceSampleV1, WorkloadQueryV1,
    compute_corpus_digest, compute_profile_material_digest, compute_workload_digest,
    validate_workload_for_tuning,
};
use super::report;
use super::report::{
    DirectEvaluationReportV1, DirectProfileEvaluationV1, DirectQualityMetricsV1,
    DirectQueryEvaluationV1, DirectQueryQualityV1, DirectRatioMetricV1, DirectStratumQualityV1,
    DirectWorstStratumV1,
};

/// The checked-in exact/lexical/graph profile every packaged evaluation runs.
pub const QUERY_BASELINE_PROFILE: &str = "query-fallback";
const METRIC_SCALE_PPM: u64 = 1_000_000;
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

pub fn evaluate_generated_outputs(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
    generated: &GenerateCandidateOutputsResultV1,
) -> Result<DirectEvaluationReportV1, SearchEvalError> {
    let corpus_digest = compute_corpus_digest(repo_root, workload)?;
    evaluate_generated_outputs_against_corpus(workload, generated, &corpus_digest)
}

/// Rebuild a report from retained outputs against an already-authoritative
/// corpus digest.
pub(super) fn evaluate_generated_outputs_against_corpus(
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
    Ok(DirectEvaluationReportV1 {
        command: "compare".to_owned(),
        status: aggregate_profile_status(&profiles),
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
    } else if resource_status == DirectEvaluationStatusV1::Pending {
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
        quality,
        status,
        queries: results,
    })
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
        DirectEvaluationStatusV1::Pass
    }
}

#[cfg(test)]
mod tests {
    use super::{aggregate_quality, evaluate_query};
    use crate::search_quality::candidate_output::{
        HistoricalQueryExecutionV1, QueryCandidateRowV1, RankedCandidateRowV1,
        ResourceMeasurementStatusV1, ResourceSampleV1, WorkloadQueryV1,
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
            lexical_aliases: Vec::new(),
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
        }
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
            Some("resource measurement pending"),
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
