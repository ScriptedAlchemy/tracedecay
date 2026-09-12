//! Search-quality evaluator for the production retrieval kernel.
//!
//! Candidate generation and live comparison over packaged authoritative
//! workload and corpus. Production candidate types, packaged-profile inputs,
//! native qualification, and direct-report scoring live in
//! `tracedecay_query::search_quality`; callers import that kernel directly.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use tracedecay_query::search_quality::{
    DirectActivationEvaluationV1, DirectEvaluationReportV1, DirectEvaluationStatusV1,
    GenerateCandidateOutputsResultV1, ProductionCandidateNativeExecutionAuthorityV1,
    SearchEvalError, activation_profile_chain, compute_corpus_digest, compute_workload_digest,
    direct_evaluated_profile_material, evaluate_generated_outputs, load_candidate_workload,
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
    generate_candidate_outputs_with_native, no_admitted_corpus_scope,
    retrieve_partition_query_bytes, write_generate_outputs,
};
pub use controlled_workloads::{
    CURSOR_PARSE_REPORT_FILE, CURSOR_PARSE_WORKLOAD, ControlledOperationDeltaV1,
    ControlledOperationV1, ControlledWorkloadComparisonV1, ControlledWorkloadErrorV1,
    ControlledWorkloadReportV1, FRAMED_LOG_REPORT_FILE, FRAMED_LOG_WORKLOAD,
    compare_controlled_workloads, run_cursor_parse_batch_workload,
    run_framed_log_durability_workload, write_controlled_workload_reports,
};

/// The workspace root that hosts the checked-in evaluator fixtures.
///
/// Fixture paths are workspace-relative because the evaluator measures the
/// product repository, not this crate's directory.
#[cfg(test)]
pub(crate) fn checked_in_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root above crates/<crate>")
        .to_path_buf()
}

const DEFAULT_WORKLOAD: &str =
    "tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json";

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
    repo_root.join(DEFAULT_WORKLOAD)
}

fn load_authoritative_default_workload()
-> Result<packaged_assets::PackagedEvaluatorAssets, SearchEvalError> {
    let assets = packaged_assets::materialize()?;
    hotpath::measure_block!("search_eval.package.activation_verify", {
        tracedecay_query::search_quality::evaluate::validate_activation_profile_matrix(
            assets.workload(),
        )
    })?;
    Ok(assets)
}

/// Validate the exact checked-in workload used by configuration activation.
///
/// Ordinary developer comparisons may use an explicit workload, but only this
/// byte-pinned default fixture can mint an activation-eligible evaluation.
pub fn validate_default_activation_workload(
    _repo_root: &Path,
) -> Result<DirectWorkloadSummaryV1, SearchEvalError> {
    let assets = load_authoritative_default_workload()?;
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

pub fn compare_default_direct(
    _project_root: &Path,
    profile_ids: Option<&[String]>,
) -> Result<DirectEvaluationReportV1, SearchEvalError> {
    let assets = load_authoritative_default_workload()?;
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

/// Run the exact checked-in activation matrix through genuine native
/// semantic/rerank authorities.
///
/// The selected profile determines the required comparison chain:
/// query baseline; semantic with semantic-disabled query ablations; and, for the
/// reranked profile, the same semantic profile with rerank disabled.
#[hotpath::measure(label = "search_eval.activation.evaluate")]
pub fn evaluate_default_activation_candidate(
    evaluated_profile_id: &str,
    authority: &dyn ProductionCandidateNativeExecutionAuthorityV1,
) -> Result<DirectActivationEvaluationV1, SearchEvalError> {
    let assets = load_authoritative_default_workload()?;
    let workload = assets.workload();
    let profile_ids = activation_profile_chain(workload, evaluated_profile_id)?;
    let generated = generate_candidate_outputs_with_native(
        &GenerateCandidateOutputsOptions {
            repo_root: assets.root(),
            workload_path: Some(&assets.workload_path()),
            profile_ids: Some(&profile_ids),
            admitted_scope: packaged_assets::admitted_scope,
        },
        authority,
    )?;
    validate_activation_native_matrix(&profile_ids, &generated)?;
    let report = evaluate_generated_outputs(assets.root(), workload, &generated)?;
    report.validate_for_activation(assets.root(), workload)?;
    let evaluated_material = direct_evaluated_profile_material(workload, evaluated_profile_id)?;
    Ok(DirectActivationEvaluationV1::from_parts(
        report,
        evaluated_material,
    ))
}

fn validate_activation_native_matrix(
    required_profiles: &[String],
    generated: &GenerateCandidateOutputsResultV1,
) -> Result<(), SearchEvalError> {
    let observed_profiles = generated
        .outputs
        .iter()
        .map(|output| output.profile_id.as_str())
        .collect::<BTreeSet<_>>();
    let expected_profiles = required_profiles
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
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
