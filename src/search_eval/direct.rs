use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

#[path = "candidate_output.rs"]
pub mod candidate_output;

pub use candidate_output::{
    CandidateOutputError, CandidateWorkloadV1, GenerateCandidateOutputsOptions,
    GenerateCandidateOutputsResultV1, ProductionCandidateOutputV1, WorkloadQueryV1,
    compute_workload_digest, generate_candidate_outputs, load_candidate_workload,
    retrieve_partition_query_bytes, validate_workload_for_tuning, write_generate_outputs,
};

const DEFAULT_WORKLOAD: &str = "tests/fixtures/search_quality/pr9-pr10-candidate-workload-v1.json";

#[derive(Debug, Error)]
pub enum SearchEvalError {
    #[error(transparent)]
    Candidate(#[from] CandidateOutputError),
    #[error("{0}")]
    Contract(String),
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DirectEvaluationStatusV1 {
    Pass,
    Fail,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DirectWorkloadSummaryV1 {
    pub command: &'static str,
    pub status: DirectEvaluationStatusV1,
    pub workload_digest: String,
    pub query_count: usize,
    pub partition_counts: BTreeMap<String, usize>,
    pub profile_count: usize,
    pub source_repository_commit: String,
    pub source_repository_tree: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DirectQueryEvaluationV1 {
    pub query_id: String,
    pub first_useful_rank: Option<u32>,
    pub returned_candidates: usize,
    pub wrong_scope_hits: usize,
    pub forbidden_hits: usize,
    pub expected_no_result: bool,
    pub status: DirectEvaluationStatusV1,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DirectProfileEvaluationV1 {
    pub profile_id: String,
    pub partition: String,
    pub query_count: usize,
    pub failed_queries: usize,
    pub fallback_stable: bool,
    pub cancellation_bounded: bool,
    pub offline: bool,
    pub resources_within_budget: bool,
    pub status: DirectEvaluationStatusV1,
    pub queries: Vec<DirectQueryEvaluationV1>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DirectEvaluationReportV1 {
    pub command: &'static str,
    pub status: DirectEvaluationStatusV1,
    pub workload_digest: String,
    pub source_repository_commit: String,
    pub source_repository_tree: String,
    pub profiles: Vec<DirectProfileEvaluationV1>,
}

pub fn default_workload_path(repo_root: &Path) -> PathBuf {
    repo_root.join(DEFAULT_WORKLOAD)
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
        query_count: workload.queries.len(),
        partition_counts,
        profile_count: workload.profile_matrix.len(),
        source_repository_commit: workload.source_repository_commit,
        source_repository_tree: workload.source_repository_tree,
    })
}

pub fn compare_direct(
    repo_root: &Path,
    workload_path: Option<&Path>,
    profile_ids: Option<&[String]>,
) -> Result<DirectEvaluationReportV1, SearchEvalError> {
    let path = workload_path.map_or_else(|| default_workload_path(repo_root), Path::to_path_buf);
    let workload = load_candidate_workload(&path)?;
    let generated = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
        repo_root,
        workload_path: Some(&path),
        profile_ids,
    })?;
    evaluate_generated_outputs(&workload, &generated)
}

pub fn evaluate_generated_outputs(
    workload: &CandidateWorkloadV1,
    generated: &GenerateCandidateOutputsResultV1,
) -> Result<DirectEvaluationReportV1, SearchEvalError> {
    let digest = compute_workload_digest(workload)?;
    if generated.workload_digest != digest {
        return Err(SearchEvalError::Contract(
            "generated outputs do not bind the checked-in workload".to_owned(),
        ));
    }
    let queries: BTreeMap<_, _> = workload
        .queries
        .iter()
        .map(|query| (query.query_id.as_str(), query))
        .collect();
    let mut profiles = generated
        .outputs
        .iter()
        .map(|output| evaluate_profile(workload, &queries, output))
        .collect::<Result<Vec<_>, _>>()?;
    profiles.sort_by(|left, right| {
        (&left.profile_id, &left.partition).cmp(&(&right.profile_id, &right.partition))
    });
    Ok(DirectEvaluationReportV1 {
        command: "compare",
        status: pass_if(
            profiles
                .iter()
                .all(|profile| profile.status == DirectEvaluationStatusV1::Pass),
        ),
        workload_digest: digest,
        source_repository_commit: workload.source_repository_commit.clone(),
        source_repository_tree: workload.source_repository_tree.clone(),
        profiles,
    })
}

fn evaluate_profile(
    workload: &CandidateWorkloadV1,
    queries: &BTreeMap<&str, &WorkloadQueryV1>,
    output: &ProductionCandidateOutputV1,
) -> Result<DirectProfileEvaluationV1, SearchEvalError> {
    if output.workload_digest != compute_workload_digest(workload)? {
        return Err(SearchEvalError::Contract(format!(
            "{} does not bind the workload",
            output.profile_id
        )));
    }
    let mut results = Vec::new();
    for row in &output.queries {
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
    let expected = workload
        .queries
        .iter()
        .filter(|query| query.partition == output.partition)
        .count();
    if results.len() != expected {
        return Err(SearchEvalError::Contract(format!(
            "{}:{} covers {} of {expected} queries",
            output.profile_id,
            output.partition,
            results.len()
        )));
    }
    results.sort_by(|left, right| left.query_id.cmp(&right.query_id));
    let fallback_stable = output.fallback_digest == output.pr9_fallback_digest;
    let cancellation_bounded =
        output.cancellation == workload.decision_policy.required_cancellation;
    let offline = output.offline == workload.decision_policy.required_offline;
    let resources_within_budget = resources_within_budget(workload, output);
    let failed_queries = results
        .iter()
        .filter(|result| result.status == DirectEvaluationStatusV1::Fail)
        .count();
    Ok(DirectProfileEvaluationV1 {
        profile_id: output.profile_id.clone(),
        partition: output.partition.clone(),
        query_count: results.len(),
        failed_queries,
        fallback_stable,
        cancellation_bounded,
        offline,
        resources_within_budget,
        status: pass_if(
            failed_queries == 0
                && fallback_stable
                && cancellation_bounded
                && offline
                && resources_within_budget,
        ),
        queries: results,
    })
}

fn evaluate_query(
    query: &WorkloadQueryV1,
    row: &candidate_output::QueryCandidateRowV1,
) -> Result<DirectQueryEvaluationV1, SearchEvalError> {
    let label = query.label.as_ref().ok_or_else(|| {
        SearchEvalError::Contract(format!("{} has no checked-in label", query.query_id))
    })?;
    let anchors = label_strings(label, "anchors")?;
    let forbidden_anchors = label_strings(label, "forbidden_anchors")?;
    let forbidden_documents = label_strings(label, "forbidden_documents")?;
    let first_useful_rank = row
        .ranked
        .iter()
        .position(|candidate| anchors.contains(&candidate.anchor))
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
                || forbidden_documents.contains(&candidate.document_id)
        })
        .count();
    let expected_no_result = anchors.is_empty();
    let expected_behavior = if expected_no_result {
        row.ranked.is_empty() || row.abstained
    } else {
        first_useful_rank.is_some()
    };
    Ok(DirectQueryEvaluationV1 {
        query_id: query.query_id.clone(),
        first_useful_rank,
        returned_candidates: row.ranked.len(),
        wrong_scope_hits,
        forbidden_hits,
        expected_no_result,
        status: pass_if(expected_behavior && wrong_scope_hits == 0 && forbidden_hits == 0),
    })
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

fn resources_within_budget(
    workload: &CandidateWorkloadV1,
    output: &ProductionCandidateOutputV1,
) -> bool {
    [
        ("current", &workload.resource_budgets.current),
        ("10x", &workload.resource_budgets.ten_x),
    ]
    .into_iter()
    .all(|(name, budget)| {
        output.resources.get(name).is_some_and(|sample| {
            sample.peak_rss_bytes <= budget.maximum_peak_rss_bytes
                && sample.p99_latency_us <= budget.maximum_p99_latency_us
                && sample.measured_queries > 0
        })
    })
}

const fn pass_if(condition: bool) -> DirectEvaluationStatusV1 {
    if condition {
        DirectEvaluationStatusV1::Pass
    } else {
        DirectEvaluationStatusV1::Fail
    }
}
