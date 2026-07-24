use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

#[path = "candidate_output.rs"]
pub mod candidate_output;

pub use candidate_output::{
    CandidateOutputError, CandidateWorkloadV1, GenerateCandidateOutputsOptions,
    GenerateCandidateOutputsResultV1, OptionalStageMeasurementV1, OptionalStageMeasurementsV1,
    ProductionCandidateOutputV1, WorkloadQueryV1, compute_corpus_digest, compute_workload_digest,
    generate_candidate_outputs, load_candidate_workload, retrieve_partition_query_bytes,
    validate_workload_for_tuning, write_generate_outputs,
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
    pub resource_status: DirectEvaluationStatusV1,
    pub optional_stages: OptionalStageMeasurementsV1,
    pub status: DirectEvaluationStatusV1,
    pub queries: Vec<DirectQueryEvaluationV1>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DirectEvaluationReportV1 {
    pub command: &'static str,
    pub status: DirectEvaluationStatusV1,
    pub workload_digest: String,
    pub corpus_digest: String,
    pub fixture_source_repository_commit: String,
    pub fixture_source_repository_tree: String,
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
) -> Result<DirectEvaluationReportV1, SearchEvalError> {
    let path = workload_path.map_or_else(|| default_workload_path(repo_root), Path::to_path_buf);
    let workload = load_candidate_workload(&path)?;
    let generated = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
        repo_root,
        workload_path: Some(&path),
        profile_ids,
    })?;
    evaluate_generated_outputs(repo_root, &workload, &generated)
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
        command: "compare",
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
    let fallback_stable = output.fallback_digest == output.pr9_fallback_digest;
    let cancellation_bounded =
        output.cancellation == workload.decision_policy.required_cancellation;
    let offline = output.offline == workload.decision_policy.required_offline;
    let resource_status = evaluate_resources(workload, output);
    let failed_queries = results
        .iter()
        .filter(|result| result.status == DirectEvaluationStatusV1::Fail)
        .count();
    let hard_invariants_pass = failed_queries == 0
        && fallback_stable
        && cancellation_bounded
        && offline
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
        cancellation_bounded,
        offline,
        resource_status,
        optional_stages: output.optional_stages,
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

fn evaluate_resources(
    workload: &CandidateWorkloadV1,
    output: &ProductionCandidateOutputV1,
) -> DirectEvaluationStatusV1 {
    let mut pending = false;
    for (name, budget) in [
        ("current", &workload.resource_budgets.current),
        ("10x", &workload.resource_budgets.ten_x),
    ] {
        let Some(sample) = output.resources.get(name) else {
            return DirectEvaluationStatusV1::Fail;
        };
        if sample.measured_queries != sample.latency_samples_us.len() as u64 {
            return DirectEvaluationStatusV1::Fail;
        }
        if sample
            .peak_rss_bytes
            .is_some_and(|peak| peak > budget.maximum_peak_rss_bytes)
        {
            return DirectEvaluationStatusV1::Fail;
        }
        match sample.status {
            OptionalStageMeasurementV1::Pending => pending = true,
            OptionalStageMeasurementV1::NotRequested => {
                return DirectEvaluationStatusV1::Fail;
            }
        }
    }
    if pending {
        DirectEvaluationStatusV1::Pending
    } else {
        DirectEvaluationStatusV1::Pass
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
