//! Search-quality evaluator for the production exact/lexical/graph retrieval
//! kernel.
//!
//! Candidate generation and live comparison over the packaged authoritative
//! workload and corpus. Production candidate types, packaged workload inputs,
//! and direct-report scoring live in `tracedecay_query::search_quality`;
//! callers import that kernel directly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tracedecay_query::search_quality::candidate_output::WORKLOAD_RELATIVE;
use tracedecay_query::search_quality::{
    DirectEvaluationReportV1, DirectEvaluationStatusV1, SearchEvalError, compute_corpus_digest,
    compute_workload_digest, evaluate_generated_outputs, load_candidate_workload,
};

mod admitted_corpus;
pub mod candidate_output;
mod controlled_workloads;
mod packaged_assets;

#[cfg(test)]
mod report_tests;

pub use admitted_corpus::root_admitted_corpus_scope;
pub use candidate_output::{
    AdmittedCorpusScopeFn, GenerateCandidateOutputsOptions, generate_candidate_outputs,
    no_admitted_corpus_scope, retrieve_partition_query_bytes, write_generate_outputs,
};
pub use controlled_workloads::{
    CURSOR_PARSE_REPORT_FILE, CURSOR_PARSE_WORKLOAD, ControlledOperationDeltaV1,
    ControlledOperationV1, ControlledWorkloadComparisonV1, ControlledWorkloadErrorV1,
    ControlledWorkloadReportV1, FRAMED_LOG_REPORT_FILE, FRAMED_LOG_WORKLOAD,
    compare_controlled_workloads, run_cursor_parse_batch_workload,
    run_framed_log_durability_workload, write_controlled_workload_reports,
};

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

pub fn default_workload_path(repo_root: &Path) -> PathBuf {
    repo_root.join(WORKLOAD_RELATIVE)
}

/// Validate the byte-pinned packaged workload.
///
/// Ordinary developer comparisons may use an explicit workload; this default
/// fixture is the one whose digest the package pins.
pub fn validate_default_workload() -> Result<DirectWorkloadSummaryV1, SearchEvalError> {
    let assets = packaged_assets::materialize()?;
    validate_direct_workload(assets.root(), Some(&assets.workload_path()))
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
    let generated = hotpath::measure_block!("search_eval.compare.generate", {
        generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root,
            workload_path: Some(&path),
            profile_ids,
            admitted_scope,
        })
    })?;
    hotpath::measure_block!("search_eval.compare", {
        evaluate_generated_outputs(repo_root, &workload, &generated)
    })
}

/// Run the packaged workload and corpus through production retrieval and
/// evaluate the checked-in labels, independent of the caller's checkout.
pub fn compare_default_direct(
    profile_ids: Option<&[String]>,
) -> Result<DirectEvaluationReportV1, SearchEvalError> {
    let assets = packaged_assets::materialize()?;
    let generated = hotpath::measure_block!("search_eval.compare.generate", {
        generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: assets.root(),
            workload_path: Some(&assets.workload_path()),
            profile_ids,
            admitted_scope: packaged_assets::admitted_scope,
        })
    })?;
    hotpath::measure_block!("search_eval.compare", {
        evaluate_generated_outputs(assets.root(), assets.workload(), &generated)
    })
}
