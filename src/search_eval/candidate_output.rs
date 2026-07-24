//! Production-bound PR9/PR10 candidate-output generator.
//!
//! Builds one published code generation from checked-in sanitized corpus
//! fixtures, then runs the shared `CompositionKernel` over the real exact,
//! lexical, and graph production lanes. Optional semantic and rerank stages
//! remain explicitly pending until their real runtimes and generations run.
//!
//! Outputs deterministic checked-in `train` / `validation` candidate records
//! plus current/10x resource samples, cancellation, offline, and fallback
//! digests. Labels are ordinary reviewable fixture data and never confer
//! activation authority.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::application::code_index::open_production_code_index_owner_v1;
use crate::code_index::chunks::content_digest;
use crate::code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use crate::code_index::production::{
    CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
    CodeIndexExecutionControlV1, CodeIndexProductionConfigV1, CodeIndexPublicationStoreErrorV1,
    CodeIndexPublishedGenerationV1,
};
use crate::code_index::projection::{
    ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionSinkErrorV1, build_batch_receipt,
};
use crate::query::retrieval::exact::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLane, ExactLaneRequest,
    ExactLaneRetriever,
};
use crate::query::retrieval::fusion::{
    CompositionKernel, CompositionLaneInput, CompositionOutputV1, FusionStageInput,
};
use crate::query::retrieval::graph::{
    CodeGraphEvidenceAdapterV1, GraphLane, GraphLaneRequest, GraphLaneRetriever,
    production_code_index_freshness,
};
use crate::query::retrieval::lexical::{
    CodeLexicalProjectionAdapterV1, CodeLexicalProjectionMetadataV1, LexicalLane,
    LexicalLaneRequest, LexicalLaneRetriever,
};
use crate::query::retrieval::ports::CodeCandidateBindingV1;
use tracedecay_domain::{
    CalibrationProfileId, ChunkerRevision, CodeGenerationId, CodeSearchChunkV1, ComponentRevision,
    DiversityPolicy, DiversityPolicyId, EphemeralSanitizedQueryViewV1, ExactAdmissionRuleRevision,
    ExactClass, FileOccurrenceId, FusionProfile, FusionProfileId, LanguageId, ManifestDigest,
    PolicyRevisionId, Pr9FallbackSubpayload, PrincipalId, PrivacyDomainId,
    ProjectionBatchReceiptV1, ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionKindV1,
    ProjectionOperationV1, ProjectionOutcomeV1, PublicRetrieverStatus, QueryNormalizationRevision,
    RelationEdgeKindV1, RepositoryId, RetrievalAnchorId, RetrievalBudget, RetrievalFailure,
    RetrievalRequest, RetrievalScope, RetrievalSnapshot, RetrieverKind, RetrieverOutcome,
    SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision,
    ScoreDomainCalibrationV1, ScoreDomainId, SingleRootScopeV1, SnapshotFileDispositionV1,
    TemporalModeV1, UtcMicros, VectorWatermark,
};

const WORKLOAD_RELATIVE: &str = "tests/fixtures/search_quality/pr9-pr10-candidate-workload-v1.json";
pub(super) const PRODUCTION_BOUNDARY: &str = "CompositionKernel::compose";
const REQUIRED_CANCELLATION: &str = "bounded_typed_cancelled";
const REQUIRED_OFFLINE: &str = "no_network_and_pr9_fallback_available";
pub(super) const EVALUATION_SEED: &str = "not_applicable_deterministic_no_rng";
pub(super) const EVALUATION_CACHE_STATE: &str = "cold_empty_in_memory_publication";
const CORPUS_DIGEST_DOMAIN: &str = "tracedecay.search-eval.corpus-content.v1";

#[derive(Debug, Error)]
pub enum CandidateOutputError {
    #[error("read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("{0}")]
    Contract(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CandidateWorkloadV1 {
    pub schema_version: u32,
    pub workload_id: String,
    pub source_repository_commit: String,
    pub source_repository_tree: String,
    pub corpus: Vec<CorpusDocumentV1>,
    pub profile_matrix: Vec<ProfileSpecV1>,
    pub resource_budgets: ResourceBudgetsV1,
    pub decision_policy: DecisionPolicySliceV1,
    pub queries: Vec<WorkloadQueryV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CorpusDocumentV1 {
    pub document_id: String,
    pub path: String,
    pub scope: String,
    pub language: String,
    pub eligibility: String,
}

#[derive(Serialize)]
struct CorpusContentBindingV1<'a> {
    document_id: &'a str,
    path: &'a str,
    scope: &'a str,
    language: &'a str,
    eligibility: &'a str,
    content_digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProfileSpecV1 {
    pub profile_id: String,
    pub lexical_weight_ppm: u32,
    pub graph_weight_ppm: u32,
    pub semantic_weight_ppm: u32,
    pub rerank_weight_ppm: u32,
    pub calibration_threshold_ppm: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourceBudgetsV1 {
    pub current: ResourceBudgetV1,
    #[serde(rename = "10x")]
    pub ten_x: ResourceBudgetV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourceBudgetV1 {
    pub maximum_peak_rss_bytes: u64,
    pub maximum_p99_latency_us: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DecisionPolicySliceV1 {
    pub required_cancellation: String,
    pub required_offline: String,
    pub required_fallback_byte_stability: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkloadQueryV1 {
    pub query_id: String,
    pub partition: String,
    pub strata: Vec<String>,
    pub query: String,
    pub allowed_scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RankedCandidateRowV1 {
    pub anchor: String,
    pub scope: String,
    pub document_id: String,
    pub tier: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueryCandidateRowV1 {
    pub query_id: String,
    pub ranked: Vec<RankedCandidateRowV1>,
    pub abstained: bool,
}

/// Truthful execution state for an optional evaluated retrieval stage.
///
/// Candidate generation records optional stages only when a real stage ran.
/// A configured profile without such a run remains `Pending`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OptionalStageMeasurementV1 {
    NotRequested,
    Pending,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OptionalStageMeasurementsV1 {
    pub semantic: OptionalStageMeasurementV1,
    pub rerank: OptionalStageMeasurementV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourceSampleV1 {
    pub status: OptionalStageMeasurementV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_rss_bytes: Option<u64>,
    pub latency_samples_us: Vec<u64>,
    pub measured_queries: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProductionCandidateOutputV1 {
    pub schema_version: u32,
    pub workload_digest: String,
    pub profile_id: String,
    pub partition: String,
    pub production_boundary: String,
    pub fixture_source_commit: String,
    pub fixture_source_tree: String,
    pub corpus_digest: String,
    pub seed: String,
    pub cache_state: String,
    pub toolchain: String,
    pub hardware: String,
    pub fallback_digest: String,
    pub pr9_fallback_digest: String,
    pub cancellation: String,
    pub offline: String,
    pub optional_stages: OptionalStageMeasurementsV1,
    pub resources: BTreeMap<String, ResourceSampleV1>,
    pub queries: Vec<QueryCandidateRowV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenerateCandidateOutputsResultV1 {
    pub workload_digest: String,
    pub outputs: Vec<ProductionCandidateOutputV1>,
}

#[derive(Clone, Debug)]
pub struct GenerateCandidateOutputsOptions<'a> {
    pub repo_root: &'a Path,
    pub workload_path: Option<&'a Path>,
    pub profile_ids: Option<&'a [String]>,
}

#[derive(Clone, Default)]
struct SharedPublicationStore {
    active: Arc<Mutex<Option<CodeIndexPublishedGenerationV1>>>,
}

impl CodeIndexAtomicPublicationPort for SharedPublicationStore {
    fn load_active(
        &self,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        let active = self.active.lock().map_err(|_| {
            CodeIndexPublicationStoreErrorV1::Unavailable(
                "candidate-output publication lock is poisoned".to_owned(),
            )
        })?;
        Ok(active.clone())
    }

    fn publish_atomically(
        &mut self,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: CodeIndexPublishedGenerationV1,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut active = self.active.lock().map_err(|_| {
            CodeIndexPublicationStoreErrorV1::Unavailable(
                "candidate-output publication lock is poisoned".to_owned(),
            )
        })?;
        if active
            .as_ref()
            .map(|current| current.manifest().generation_id.clone())
            .as_ref()
            != expected_active_generation
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        *active = Some(generation);
        Ok(())
    }
}

#[derive(Default)]
struct ApplyingProjectionSink;

impl CodeChunkProjectionSink for ApplyingProjectionSink {
    fn project_changed_chunks(
        &mut self,
        request: ProjectionBatchRequestV1,
    ) -> Result<ProjectionBatchReceiptV1, ProjectionSinkErrorV1> {
        let decisions: Vec<ChunkProjectionDecisionV1> = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| ChunkProjectionDecisionV1 {
                chunk_id: change.chunk_id.clone(),
                prior_chunk_digest: change.prior_digest.clone(),
                current_chunk_digest: change.current_digest.clone(),
                operation: if change.prior_digest.is_some() {
                    ProjectionOperationV1::Updated
                } else {
                    ProjectionOperationV1::Added
                },
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: change.current_digest.clone(),
            })
            .collect();
        build_batch_receipt(&request, &decisions)
            .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
    }
}

struct ActiveControl;

impl CodeIndexExecutionControlV1 for ActiveControl {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

struct CancelledControl;

impl CodeIndexExecutionControlV1 for CancelledControl {
    fn is_cancelled(&self) -> bool {
        true
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

#[derive(Clone)]
struct OccurrenceMapEntry {
    document_id: String,
    scope: String,
    display_anchor: String,
}

struct PublishedCorpus {
    generation: CodeIndexPublishedGenerationV1,
    occurrence_map: BTreeMap<String, OccurrenceMapEntry>,
    corpus_digest: String,
}

/// Load the checked-in PR9/PR10 direct-evaluation workload.
pub fn load_candidate_workload(path: &Path) -> Result<CandidateWorkloadV1, CandidateOutputError> {
    let bytes = fs::read(path).map_err(|source| CandidateOutputError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let workload: CandidateWorkloadV1 =
        serde_json::from_slice(&bytes).map_err(|source| CandidateOutputError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    validate_workload_for_tuning(&workload)?;
    Ok(workload)
}

pub fn compute_workload_digest(
    workload: &CandidateWorkloadV1,
) -> Result<String, CandidateOutputError> {
    canonical_sha256(workload)
}

/// Hash the declared corpus and every byte-exact checked-in document.
///
/// Including document metadata prevents ambiguous concatenation while each
/// content digest binds the bytes actually read from `repo_root`.
pub fn compute_corpus_digest(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
) -> Result<String, CandidateOutputError> {
    let mut bindings = Vec::with_capacity(workload.corpus.len());
    for document in &workload.corpus {
        let absolute = repo_root.join(&document.path);
        let bytes = fs::read(&absolute).map_err(|source| CandidateOutputError::Read {
            path: absolute,
            source,
        })?;
        bindings.push(CorpusContentBindingV1 {
            document_id: &document.document_id,
            path: &document.path,
            scope: &document.scope,
            language: &document.language,
            eligibility: &document.eligibility,
            content_digest: content_digest(&bytes).as_str().to_owned(),
        });
    }
    canonical_sha256(&(CORPUS_DIGEST_DOMAIN, bindings))
}

pub fn validate_workload_for_tuning(
    workload: &CandidateWorkloadV1,
) -> Result<(), CandidateOutputError> {
    if workload.schema_version != 1 {
        return Err(CandidateOutputError::Contract(
            "candidate workload schema_version must be 1".to_owned(),
        ));
    }
    if workload.source_repository_commit.trim().is_empty()
        || workload.source_repository_tree.trim().is_empty()
    {
        return Err(CandidateOutputError::Contract(
            "fixture source commit/tree must not be empty".to_owned(),
        ));
    }
    let mut document_ids = BTreeSet::new();
    let mut document_paths = BTreeSet::new();
    for document in &workload.corpus {
        if [
            document.document_id.as_str(),
            document.path.as_str(),
            document.scope.as_str(),
            document.language.as_str(),
            document.eligibility.as_str(),
        ]
        .into_iter()
        .any(str::is_empty)
        {
            return Err(CandidateOutputError::Contract(
                "corpus document fields must not be empty".to_owned(),
            ));
        }
        if !document_ids.insert(document.document_id.as_str()) {
            return Err(CandidateOutputError::Contract(format!(
                "duplicate corpus document_id {}",
                document.document_id
            )));
        }
        if !document_paths.insert(document.path.as_str()) {
            return Err(CandidateOutputError::Contract(format!(
                "duplicate corpus path {}",
                document.path
            )));
        }
    }
    if document_ids.is_empty() {
        return Err(CandidateOutputError::Contract(
            "corpus must not be empty".to_owned(),
        ));
    }
    let mut profile_ids = BTreeSet::new();
    for profile in &workload.profile_matrix {
        if profile.profile_id.trim().is_empty() {
            return Err(CandidateOutputError::Contract(
                "profile_id must not be empty".to_owned(),
            ));
        }
        if !profile_ids.insert(profile.profile_id.as_str()) {
            return Err(CandidateOutputError::Contract(format!(
                "duplicate profile_id {}",
                profile.profile_id
            )));
        }
    }
    if profile_ids.is_empty() {
        return Err(CandidateOutputError::Contract(
            "profile_matrix must not be empty".to_owned(),
        ));
    }
    let mut query_ids = BTreeSet::new();
    let mut partitions = BTreeSet::new();
    for query in &workload.queries {
        if query.query_id.trim().is_empty() {
            return Err(CandidateOutputError::Contract(
                "query_id must not be empty".to_owned(),
            ));
        }
        if !query_ids.insert(query.query_id.as_str()) {
            return Err(CandidateOutputError::Contract(format!(
                "duplicate query_id {}",
                query.query_id
            )));
        }
        if query.partition != "train" && query.partition != "validation" {
            return Err(CandidateOutputError::Contract(format!(
                "unknown partition {}",
                query.partition
            )));
        }
        partitions.insert(query.partition.as_str());
        if query.label.is_none() {
            return Err(CandidateOutputError::Contract(format!(
                "query {} is missing its checked-in label",
                query.query_id
            )));
        }
    }
    for partition in ["train", "validation"] {
        if !partitions.contains(partition) {
            return Err(CandidateOutputError::Contract(format!(
                "partition {partition} has no queries"
            )));
        }
    }
    Ok(())
}

/// Generate deterministic train/validation outputs using the production
/// retrieval kernel.
pub fn generate_candidate_outputs(
    options: &GenerateCandidateOutputsOptions<'_>,
) -> Result<GenerateCandidateOutputsResultV1, CandidateOutputError> {
    let workload_path = options.workload_path.map_or_else(
        || options.repo_root.join(WORKLOAD_RELATIVE),
        Path::to_path_buf,
    );
    let workload = load_candidate_workload(&workload_path)?;
    let workload_digest = compute_workload_digest(&workload)?;
    let profiles: Vec<&ProfileSpecV1> = match options.profile_ids {
        Some(ids) => {
            let known: BTreeSet<_> = workload
                .profile_matrix
                .iter()
                .map(|profile| profile.profile_id.as_str())
                .collect();
            let mut requested = BTreeSet::new();
            for id in ids {
                if !requested.insert(id.as_str()) {
                    return Err(CandidateOutputError::Contract(format!(
                        "duplicate requested profile_id {id}"
                    )));
                }
                if !known.contains(id.as_str()) {
                    return Err(CandidateOutputError::Contract(format!(
                        "unknown requested profile_id {id}"
                    )));
                }
            }
            workload
                .profile_matrix
                .iter()
                .filter(|profile| requested.contains(profile.profile_id.as_str()))
                .collect()
        }
        None => workload.profile_matrix.iter().collect(),
    };
    if profiles.is_empty() {
        return Err(CandidateOutputError::Contract(
            "no profiles selected for candidate generation".to_owned(),
        ));
    }
    let published = publish_corpus(options.repo_root, &workload)?;

    let mut outputs = Vec::new();
    for profile in profiles {
        for partition in ["train", "validation"] {
            let output = generate_partition_output(
                &workload,
                &workload_digest,
                &published,
                profile,
                partition,
            )?;
            outputs.push(output);
        }
    }

    // Prove cancellation against the production code-index control path once.
    prove_cancellation(options.repo_root, &workload)?;

    Ok(GenerateCandidateOutputsResultV1 {
        workload_digest,
        outputs,
    })
}

/// Direct production call for one query/profile — used by tests to prove the
/// generator emits identical candidate bytes.
pub fn retrieve_partition_query_bytes(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
    profile_id: &str,
    query_id: &str,
) -> Result<Vec<u8>, CandidateOutputError> {
    validate_workload_for_tuning(workload)?;
    let profile = workload
        .profile_matrix
        .iter()
        .find(|profile| profile.profile_id == profile_id)
        .ok_or_else(|| CandidateOutputError::Contract(format!("unknown profile {profile_id}")))?;
    let query = workload
        .queries
        .iter()
        .find(|query| query.query_id == query_id)
        .ok_or_else(|| CandidateOutputError::Contract(format!("unknown query {query_id}")))?;
    let published = publish_corpus(repo_root, workload)?;
    let row = retrieve_one_query(&published, profile, query)?;
    canonical_json_bytes(&row)
}

pub fn write_generate_outputs(
    output_root: &Path,
    result: &GenerateCandidateOutputsResultV1,
) -> Result<(), CandidateOutputError> {
    fs::create_dir_all(output_root).map_err(|source| CandidateOutputError::Write {
        path: output_root.to_path_buf(),
        source,
    })?;
    let jsonl_path = output_root.join("train-validation-candidate-outputs.jsonl");
    let mut jsonl = String::new();
    for output in &result.outputs {
        jsonl.push_str(&serde_json::to_string(output).map_err(|error| {
            CandidateOutputError::Contract(format!("serialize candidate output: {error}"))
        })?);
        jsonl.push('\n');
    }
    fs::write(&jsonl_path, jsonl).map_err(|source| CandidateOutputError::Write {
        path: jsonl_path,
        source,
    })?;
    let summary_path = output_root.join("generate-summary.json");
    write_pretty_json(
        &summary_path,
        &serde_json::json!({
            "workload_digest": result.workload_digest,
            "outputs": result.outputs.len(),
            "production_boundary": PRODUCTION_BOUNDARY,
        }),
    )?;
    Ok(())
}

fn generate_partition_output(
    workload: &CandidateWorkloadV1,
    workload_digest: &str,
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    partition: &str,
) -> Result<ProductionCandidateOutputV1, CandidateOutputError> {
    let queries: Vec<&WorkloadQueryV1> = workload
        .queries
        .iter()
        .filter(|query| query.partition == partition)
        .collect();
    if queries.is_empty() {
        return Err(CandidateOutputError::Contract(format!(
            "partition {partition} has no queries"
        )));
    }
    let mut rows = Vec::new();
    let mut latencies_us = Vec::new();
    let peak_before = peak_rss_bytes();
    for query in &queries {
        let started = Instant::now();
        let row = retrieve_one_query(published, profile, query)?;
        latencies_us.push(started.elapsed().as_micros() as u64);
        rows.push(row);
    }
    let peak_after = peak_rss_bytes().max(peak_before);
    let current = ResourceSampleV1 {
        status: OptionalStageMeasurementV1::Pending,
        peak_rss_bytes: peak_after,
        latency_samples_us: latencies_us,
        measured_queries: rows.len() as u64,
        pending_reason: Some(
            "raw current-corpus samples recorded; p99 requires the declared Linux evaluation"
                .to_owned(),
        ),
    };
    let ten_x = ResourceSampleV1 {
        status: OptionalStageMeasurementV1::Pending,
        peak_rss_bytes: None,
        latency_samples_us: Vec::new(),
        measured_queries: 0,
        pending_reason: Some(
            "requires a distinct corpus with exactly 10x the eligible chunks".to_owned(),
        ),
    };

    let probe = queries.first().copied().ok_or_else(|| {
        CandidateOutputError::Contract(format!("partition {partition} has no queries"))
    })?;
    let fallback_digest = fallback_digest_for_query(published, profile, probe)?;
    let pr9_digest = pr9_fallback_digest_for_query(published, profile, probe)?;

    let mut resources = BTreeMap::new();
    resources.insert("current".to_owned(), current);
    resources.insert("10x".to_owned(), ten_x);

    Ok(ProductionCandidateOutputV1 {
        schema_version: 2,
        workload_digest: workload_digest.to_owned(),
        profile_id: profile.profile_id.clone(),
        partition: partition.to_owned(),
        production_boundary: PRODUCTION_BOUNDARY.to_owned(),
        fixture_source_commit: workload.source_repository_commit.clone(),
        fixture_source_tree: workload.source_repository_tree.clone(),
        corpus_digest: published.corpus_digest.clone(),
        seed: EVALUATION_SEED.to_owned(),
        cache_state: EVALUATION_CACHE_STATE.to_owned(),
        toolchain: toolchain_fingerprint(),
        hardware: hardware_fingerprint(),
        fallback_digest,
        pr9_fallback_digest: pr9_digest,
        cancellation: REQUIRED_CANCELLATION.to_owned(),
        offline: REQUIRED_OFFLINE.to_owned(),
        optional_stages: optional_stage_measurements(profile),
        resources,
        queries: rows,
    })
}

fn retrieve_one_query(
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    query: &WorkloadQueryV1,
) -> Result<QueryCandidateRowV1, CandidateOutputError> {
    let composed = compose_production_query(published, profile, query)?;
    let ranked = map_ranked_candidates(published, &composed)?;
    let abstained = ranked.is_empty();
    Ok(QueryCandidateRowV1 {
        query_id: query.query_id.clone(),
        ranked,
        abstained,
    })
}

fn optional_stage_measurements(profile: &ProfileSpecV1) -> OptionalStageMeasurementsV1 {
    OptionalStageMeasurementsV1 {
        semantic: if profile.semantic_weight_ppm == 0 {
            OptionalStageMeasurementV1::NotRequested
        } else {
            OptionalStageMeasurementV1::Pending
        },
        rerank: if profile.rerank_weight_ppm == 0 {
            OptionalStageMeasurementV1::NotRequested
        } else {
            OptionalStageMeasurementV1::Pending
        },
    }
}

fn compose_production_query(
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    query: &WorkloadQueryV1,
) -> Result<CompositionOutputV1, CandidateOutputError> {
    let generation_id = published.generation.manifest().generation_id.clone();
    let request = retrieval_request(&profile.profile_id, published)?;
    let query_view = EphemeralSanitizedQueryViewV1::sanitize(
        &query.query,
        id::<SanitizerRevision>("query-sanitizer.candidate.v1")?,
        id::<QueryNormalizationRevision>("query-normalization.candidate.v1")?,
    )
    .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;

    let freshness = production_code_index_freshness(
        published.generation.manifest().seal.sealed_at,
        id::<ComponentRevision>("policy.candidate.v1")?,
    )
    .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let metadata = CodeLexicalProjectionMetadataV1 {
        generation: generation_id.clone(),
        repository_id: Some(published.generation.snapshot().repository.clone()),
        freshness: freshness.clone(),
        exact_retriever_revision: id("retriever.exact.candidate.v1")?,
        lexical_retriever_revision: id("retriever.lexical.candidate.v1")?,
        exact_score_domain: id("score.exact.candidate.v1")?,
    };
    let admitted = published
        .generation
        .admitted_chunks()
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let lexical_projection = CodeLexicalProjectionAdapterV1::new_admitted(metadata, admitted)
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let authority =
        CentralExactAdmissionAuthorityV1::new(id::<ExactAdmissionRuleRevision>("exact-rules.v1")?);
    let exact_lane = ExactLane::new(
        authority.clone(),
        lexical_projection.exact_adapter(authority.clone()),
    );
    let lexical_lane = LexicalLane::new(lexical_projection);
    let graph_lane = GraphLane::new(
        CodeGraphEvidenceAdapterV1::new(
            generation_id.clone(),
            Some(published.generation.snapshot().repository.clone()),
            freshness,
            published.generation.edges(),
            published.generation.chunks().chunks(),
        )
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
    );

    let budget = retrieval_budget();
    let exact_request = ExactLaneRequest {
        base: request.clone(),
        query_view: &query_view,
        generation: generation_id.clone(),
        literals: authority.parse_literals(&query_view, &request),
        budget,
    };
    let exact_outcome = exact_lane
        .retrieve_exact(&exact_request)
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;

    let (whole_terms, subtokens) = lexical_terms(&query.query);
    let lexical_request = LexicalLaneRequest {
        base: request.clone(),
        query_view: &query_view,
        generation: generation_id.clone(),
        whole_terms,
        subtokens,
        phrases: Vec::new(),
        field_filters: Vec::new(),
        fuzzy_budget: 8,
        lexical_profile_revision: id("lexical-profile.candidate.v1")?,
        score_domain: id("score.lexical.candidate.v1")?,
        budget,
    };
    let lexical_outcome = lexical_lane
        .retrieve_lexical(&lexical_request)
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;

    let seed_anchors = graph_seeds_from_outcomes(&exact_outcome, &lexical_outcome);
    let graph_outcome = if seed_anchors.is_empty() {
        RetrieverOutcome::Unavailable(RetrievalFailure::AuthorityUnavailable {
            detail: "no graph seeds from exact/lexical".to_owned(),
        })
    } else {
        let graph_request = GraphLaneRequest {
            base: request.clone(),
            generation: generation_id.clone(),
            seed_anchors,
            edge_kinds: vec![
                RelationEdgeKindV1::Calls,
                RelationEdgeKindV1::Uses,
                RelationEdgeKindV1::Contains,
            ],
            max_depth: 2,
            budget,
        };
        graph_lane
            .retrieve_graph(&graph_request)
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?
    };

    let kernel = CompositionKernel::new(id::<ComponentRevision>("ranking.candidate.v1")?);
    let fusion_profile = fusion_profile(profile, &budget, false)?;
    let pr9_lanes = vec![
        CompositionLaneInput::new(RetrieverKind::ExactLiteral, exact_outcome)
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        CompositionLaneInput::new(RetrieverKind::Lexical, lexical_outcome)
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        CompositionLaneInput::new(RetrieverKind::Graph, graph_outcome)
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
    ];
    kernel
        .compose(
            &FusionStageInput {
                profile: fusion_profile,
                lanes: pr9_lanes,
            },
            &no_caps()?,
        )
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))
}

fn pr9_fallback_digest_for_query(
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    query: &WorkloadQueryV1,
) -> Result<String, CandidateOutputError> {
    // PR9-only profile compose for digest stability measurement.
    let mut pr9_profile = profile.clone();
    pr9_profile.semantic_weight_ppm = 0;
    pr9_profile.rerank_weight_ppm = 0;
    fallback_digest_for_query(published, &pr9_profile, query)
}

fn fallback_digest_for_query(
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    query: &WorkloadQueryV1,
) -> Result<String, CandidateOutputError> {
    let composed = compose_production_query(published, profile, query)?;
    let fallback = pr9_fallback_from_composition(&composed)?;
    Ok(fallback.digest.as_str().to_owned())
}

fn pr9_fallback_from_composition(
    output: &CompositionOutputV1,
) -> Result<Pr9FallbackSubpayload, CandidateOutputError> {
    let mut coverage = BTreeMap::new();
    for lane in RetrieverKind::PR9_FALLBACK_LANES {
        coverage.insert(
            lane,
            output
                .public_lane_statuses
                .get(&lane)
                .copied()
                .unwrap_or(PublicRetrieverStatus::Unavailable),
        );
    }
    let bootstrap_digest = canonical_sha256(&(
        "tracedecay.search-eval.pr9-fallback-bootstrap.v1",
        &output.profile_id,
        &output.ranked_candidates,
        &coverage,
        &output.freshness,
    ))?;
    let mut fallback = Pr9FallbackSubpayload {
        profile_id: output.profile_id.clone(),
        ordered_candidates: output.ranked_candidates.clone(),
        public_pr9_lane_coverage: coverage,
        freshness: output.freshness.clone(),
        cursor: None,
        digest: tracedecay_domain::FallbackSubpayloadDigest::new(bootstrap_digest)
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
    };
    fallback.digest = fallback
        .compute_digest()
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    fallback
        .validate()
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    Ok(fallback)
}

fn map_ranked_candidates(
    published: &PublishedCorpus,
    output: &CompositionOutputV1,
) -> Result<Vec<RankedCandidateRowV1>, CandidateOutputError> {
    let mut rows = Vec::new();
    for ranked in &output.ranked_candidates {
        let occurrence = ranked.candidate.occurrences.first().map_or_else(
            || ranked.candidate.anchor_id.as_str().to_owned(),
            |occurrence| occurrence.source_occurrence_id.as_str().to_owned(),
        );
        let mapped = published
            .occurrence_map
            .get(&occurrence)
            .cloned()
            .or_else(|| {
                published
                    .occurrence_map
                    .values()
                    .find(|entry| {
                        ranked
                            .candidate
                            .anchor_id
                            .as_str()
                            .contains(&entry.display_anchor)
                    })
                    .cloned()
            });
        let entry = mapped.ok_or_else(|| {
            CandidateOutputError::Contract(format!(
                "ranked candidate {} has no corpus occurrence binding",
                ranked.candidate.anchor_id
            ))
        })?;
        let tier = if ranked.candidate.exact_class != ExactClass::Approximate {
            "exact"
        } else {
            "approximate"
        };
        rows.push(RankedCandidateRowV1 {
            anchor: entry.display_anchor,
            scope: entry.scope,
            document_id: entry.document_id,
            tier: tier.to_owned(),
        });
    }
    Ok(rows)
}

fn publish_corpus(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
) -> Result<PublishedCorpus, CandidateOutputError> {
    let corpus_digest = compute_corpus_digest(repo_root, workload)?;
    let language_registry = StaticLanguageRegistry::new();
    let mut files = Vec::new();
    let mut captured = Vec::new();
    let mut file_to_document = BTreeMap::new();
    for document in &workload.corpus {
        let absolute = repo_root.join(&document.path);
        let bytes = fs::read(&absolute).map_err(|source| CandidateOutputError::Read {
            path: absolute.clone(),
            source,
        })?;
        let file_occurrence_id = id::<FileOccurrenceId>(&format!("file.{}", document.document_id))?;
        file_to_document.insert(file_occurrence_id.as_str().to_owned(), document.clone());
        let language = id::<LanguageId>(&document.language)?;
        let indexable = language_registry.descriptor(&language).is_some();
        files.push(SanitizedCodeFileV1 {
            file_occurrence_id: file_occurrence_id.clone(),
            logical_path: document.path.clone(),
            language: Some(language),
            content_digest: content_digest(&bytes),
            disposition: if indexable {
                SnapshotFileDispositionV1::Present
            } else {
                SnapshotFileDispositionV1::UnsupportedLanguage
            },
        });
        if indexable {
            captured.push(CodeIndexCapturedFileV1 {
                file_occurrence_id,
                sanitized_bytes: bytes,
            });
        }
    }
    files.sort_by(|left, right| {
        (&left.logical_path, &left.file_occurrence_id)
            .cmp(&(&right.logical_path, &right.file_occurrence_id))
    });
    captured.sort_by(|left, right| left.file_occurrence_id.cmp(&right.file_occurrence_id));
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: id::<RepositoryId>("repository.candidate.fixture")?,
        worktree: None,
        reference: None,
        source_revision: None,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.candidate.v1")?,
        sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.candidate.v1")?],
        content_identity: id(&corpus_digest)?,
        captured_at: UtcMicros(1_000_000),
        files,
    };
    let request = CodeIndexBuildRequestV1 {
        snapshot,
        captured_files: captured,
        changed_files: BTreeSet::new(),
        invalidations: BTreeSet::new(),
        sealed_at: UtcMicros(1_100_000),
        target_projection_key: ProjectionKeyV1 {
            kind: ProjectionKindV1::Lexical,
            schema_revision: "lexical.candidate.v1".to_owned(),
            profile_digest: lexical_projection_profile_digest()?,
        },
    };
    let config = CodeIndexProductionConfigV1 {
        repository: id::<RepositoryId>("repository.candidate.fixture")?,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.candidate.v1")?,
        policy_revision: id::<PolicyRevisionId>("policy.candidate.v1")?,
        chunker_revision: id::<ChunkerRevision>("chunker.candidate.v1")?,
        privacy_domain: id::<PrivacyDomainId>("privacy.candidate.fixture")?,
        privacy_key_epoch: 1,
        max_snapshot_age_micros: None,
    };
    let store = SharedPublicationStore::default();
    let mut owner = open_production_code_index_owner_v1(config, store, ApplyingProjectionSink)
        .map_err(|error| {
            CandidateOutputError::Contract(format!("open production owner: {error}"))
        })?;
    let generation = owner
        .build_and_publish(request, &ActiveControl)
        .map_err(|error| CandidateOutputError::Contract(format!("publish generation: {error}")))?;

    let mut occurrence_map = BTreeMap::new();
    for chunk in generation.chunks().chunks() {
        let Some(document) = file_to_document.get(chunk.anchor.file_occurrence_id.as_str()) else {
            continue;
        };
        let display = display_anchor_for_chunk(chunk, document);
        if let Some(symbol) = &chunk.anchor.symbol_occurrence_id {
            occurrence_map.insert(
                symbol.as_str().to_owned(),
                OccurrenceMapEntry {
                    document_id: document.document_id.clone(),
                    scope: document.scope.clone(),
                    display_anchor: display.clone(),
                },
            );
        }
        occurrence_map.insert(
            chunk.id.as_str().to_owned(),
            OccurrenceMapEntry {
                document_id: document.document_id.clone(),
                scope: document.scope.clone(),
                display_anchor: display,
            },
        );
    }

    Ok(PublishedCorpus {
        generation,
        occurrence_map,
        corpus_digest,
    })
}

fn display_anchor_for_chunk(chunk: &CodeSearchChunkV1, document: &CorpusDocumentV1) -> String {
    if let Some(term) = chunk.exact_terms.iter().find(|term| {
        matches!(
            term.kind(),
            tracedecay_domain::ExactTechnicalTermKindV1::WholeSymbol
        )
    }) {
        let name = String::from_utf8_lossy(term.canonical_bytes()).into_owned();
        return format!("{}::{name}", document.document_id);
    }
    let text = chunk.sanitized_text.as_str();
    let first = text.lines().next().unwrap_or(text).trim();
    if first.is_empty() {
        document.document_id.clone()
    } else {
        format!("{}::{first}", document.document_id)
    }
}

fn prove_cancellation(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
) -> Result<(), CandidateOutputError> {
    // Cancellation is proven against the production code-index control surface
    // (typed Cancelled interruption, no publish). The generator records the
    // required receipt string after this check succeeds.
    let mut files = Vec::new();
    let mut captured = Vec::new();
    let document = workload
        .corpus
        .first()
        .ok_or_else(|| CandidateOutputError::Contract("corpus empty".to_owned()))?;
    let absolute = repo_root.join(&document.path);
    let bytes = fs::read(&absolute).map_err(|source| CandidateOutputError::Read {
        path: absolute,
        source,
    })?;
    let file_occurrence_id = id::<FileOccurrenceId>("file.cancel.probe")?;
    files.push(SanitizedCodeFileV1 {
        file_occurrence_id: file_occurrence_id.clone(),
        logical_path: document.path.clone(),
        language: Some(id::<LanguageId>(&document.language)?),
        content_digest: content_digest(&bytes),
        disposition: SnapshotFileDispositionV1::Present,
    });
    captured.push(CodeIndexCapturedFileV1 {
        file_occurrence_id,
        sanitized_bytes: bytes.clone(),
    });
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: id::<RepositoryId>("repository.candidate.cancel")?,
        worktree: None,
        reference: None,
        source_revision: None,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.candidate.v1")?,
        sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.cancel")?],
        content_identity: content_digest(&bytes),
        captured_at: UtcMicros(1_000_000),
        files,
    };
    let request = CodeIndexBuildRequestV1 {
        snapshot,
        captured_files: captured,
        changed_files: BTreeSet::new(),
        invalidations: BTreeSet::new(),
        sealed_at: UtcMicros(1_100_000),
        target_projection_key: ProjectionKeyV1 {
            kind: ProjectionKindV1::Lexical,
            schema_revision: "lexical.candidate.v1".to_owned(),
            profile_digest: lexical_projection_profile_digest()?,
        },
    };
    let config = CodeIndexProductionConfigV1 {
        repository: id::<RepositoryId>("repository.candidate.cancel")?,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.candidate.v1")?,
        policy_revision: id::<PolicyRevisionId>("policy.candidate.v1")?,
        chunker_revision: id::<ChunkerRevision>("chunker.candidate.v1")?,
        privacy_domain: id::<PrivacyDomainId>("privacy.candidate.cancel")?,
        privacy_key_epoch: 1,
        max_snapshot_age_micros: None,
    };
    let store = SharedPublicationStore::default();
    let mut owner =
        open_production_code_index_owner_v1(config, store.clone(), ApplyingProjectionSink)
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let error = match owner.build_and_publish(request, &CancelledControl) {
        Err(error) => error,
        Ok(_) => {
            return Err(CandidateOutputError::Contract(
                "cancelled publish must fail".to_owned(),
            ));
        }
    };
    if !format!("{error:?}").contains("Cancelled") {
        return Err(CandidateOutputError::Contract(format!(
            "expected cancelled interruption, got {error:?}"
        )));
    }
    if store
        .load_active()
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?
        .is_some()
    {
        return Err(CandidateOutputError::Contract(
            "cancelled publish must not activate a generation".to_owned(),
        ));
    }
    Ok(())
}

fn graph_seeds_from_outcomes(
    exact: &RetrieverOutcome<
        tracedecay_domain::RetrieverBatch<crate::query::retrieval::exact::ExactLaneEvidence>,
    >,
    lexical: &RetrieverOutcome<
        tracedecay_domain::RetrieverBatch<crate::query::retrieval::lexical::LexicalLaneEvidence>,
    >,
) -> Vec<CodeCandidateBindingV1> {
    let mut seeds = Vec::new();
    let mut seen_occurrences = BTreeSet::new();
    let mut seen_symbols = BTreeSet::new();
    let mut push_seed = |binding: &CodeCandidateBindingV1,
                         seeds: &mut Vec<CodeCandidateBindingV1>| {
        let Some(symbol) = binding.occurrence.symbol.as_ref() else {
            return;
        };
        if !seen_occurrences.insert(binding.source_occurrence.clone()) {
            return;
        }
        if !seen_symbols.insert(symbol.clone()) {
            return;
        }
        seeds.push(binding.clone());
    };
    if let RetrieverOutcome::Complete(batch) | RetrieverOutcome::Partial { value: batch, .. } =
        exact
    {
        for evidence in batch.evidence_by_occurrence.values() {
            push_seed(&evidence.binding, &mut seeds);
            if seeds.len() >= 8 {
                return seeds;
            }
        }
    }
    if let RetrieverOutcome::Complete(batch) | RetrieverOutcome::Partial { value: batch, .. } =
        lexical
    {
        for evidence in batch.evidence_by_occurrence.values() {
            push_seed(&evidence.binding, &mut seeds);
            if seeds.len() >= 8 {
                return seeds;
            }
        }
    }
    seeds
}

fn fusion_profile(
    profile: &ProfileSpecV1,
    __budget: &RetrievalBudget,
    include_semantic: bool,
) -> Result<FusionProfile, CandidateOutputError> {
    let mut weights = BTreeMap::new();
    weights.insert(RetrieverKind::ExactLiteral, 1_000_000);
    weights.insert(RetrieverKind::Lexical, profile.lexical_weight_ppm);
    weights.insert(RetrieverKind::Graph, profile.graph_weight_ppm);
    if include_semantic && profile.semantic_weight_ppm > 0 {
        weights.insert(RetrieverKind::Semantic, profile.semantic_weight_ppm);
    }
    let lanes: Vec<RetrieverKind> = weights.keys().copied().collect();
    let calibrations = lanes
        .iter()
        .copied()
        .map(|lane| {
            Ok((
                lane,
                id::<CalibrationProfileId>(&format!(
                    "calibration.{}.{}",
                    lane.as_str(),
                    profile.profile_id
                ))?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>, CandidateOutputError>>()?;
    let score_domain_calibrations = [
        (RetrieverKind::ExactLiteral, "score.exact.candidate.v1"),
        (RetrieverKind::Lexical, "score.lexical.candidate.v1"),
        (RetrieverKind::Graph, "score.graph.daemon.v1"),
        (RetrieverKind::Semantic, "score.semantic.candidate.v1"),
    ]
    .into_iter()
    .filter(|(lane, _)| weights.contains_key(lane))
    .map(|(lane, domain)| {
        let score_domain = id::<ScoreDomainId>(domain)?;
        Ok((
            score_domain.clone(),
            ScoreDomainCalibrationV1 {
                calibration_profile_id: id(&format!(
                    "calibration.{}.{}",
                    lane.as_str(),
                    profile.profile_id
                ))?,
                score_domain,
                raw_min_micros: 0,
                raw_max_micros: 1_000_000,
            },
        ))
    })
    .collect::<Result<BTreeMap<_, _>, CandidateOutputError>>()?;
    Ok(FusionProfile {
        profile_id: id::<FusionProfileId>(&format!("profile.{}", profile.profile_id))?,
        evaluation_result_anchor: id::<RetrievalAnchorId>(&format!(
            "evaluation.{}",
            profile.profile_id
        ))?,
        calibrations,
        score_domain_calibrations,
        weights_micros: weights,
        diversity_policy_id: id::<DiversityPolicyId>("diversity.candidate.v1")?,
        rerank_policy_id: None,
        retrieval_budget: retrieval_budget(),
    })
}

fn retrieval_request(
    profile_id: &str,
    published: &PublishedCorpus,
) -> Result<RetrievalRequest, CandidateOutputError> {
    let manifest = published.generation.manifest();
    let freshness_digest = canonical_sha256(&(
        "tracedecay.search-eval.freshness.v1",
        &manifest.generation_id,
        &manifest.seal.expected_digest,
        manifest.seal.sealed_at,
    ))?;
    Ok(RetrievalRequest {
        principal: id::<PrincipalId>("principal.candidate")?,
        scope: RetrievalScope {
            privacy_domain: id("privacy.candidate.fixture")?,
            root: SingleRootScopeV1 {
                repository: id("repository.candidate.fixture")?,
                worktree: None,
                reference: None,
            },
        },
        temporal_mode: TemporalModeV1::Current,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: id(&freshness_digest)?,
            authorization_revision: id("authorization.candidate.v1")?,
            captured_at: manifest.seal.sealed_at,
        },
        profile_id: id(&format!("profile.{profile_id}"))?,
        budget: retrieval_budget(),
    })
}

fn lexical_projection_profile_digest() -> Result<ManifestDigest, CandidateOutputError> {
    let digest = canonical_sha256(&(
        "tracedecay.search-eval.lexical-projection-profile.v1",
        "lexical.candidate.v1",
        "sanitizer.candidate.v1",
        "chunker.candidate.v1",
        "policy.candidate.v1",
    ))?;
    id(&digest)
}

fn retrieval_budget() -> RetrievalBudget {
    RetrievalBudget {
        max_candidates_per_lane: 32,
        max_fused_candidates: 32,
        max_hydrated_results: 16,
        max_hydration_bytes: 65_536,
        deadline_micros: None,
    }
}

fn no_caps() -> Result<DiversityPolicy, CandidateOutputError> {
    Ok(DiversityPolicy {
        policy_id: id("diversity.candidate.v1")?,
        evaluation_result_anchor: Some(id("evaluation.candidate.v1")?),
        per_source_namespace: None,
        per_source_instance: None,
        per_repository: None,
        per_session_or_thread: None,
        per_copy_cluster: None,
        per_evidence_role: None,
    })
}

fn lexical_terms(query: &str) -> (Vec<String>, Vec<String>) {
    let mut whole = Vec::new();
    let mut subtokens = Vec::new();
    for token in query.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_') {
        if token.is_empty() {
            continue;
        }
        whole.push(token.to_owned());
        for part in split_identifier(token) {
            if part != token {
                subtokens.push(part);
            }
        }
    }
    whole.sort();
    whole.dedup();
    subtokens.sort();
    subtokens.dedup();
    (whole, subtokens)
}

fn split_identifier(token: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for ch in token.chars() {
        if ch == '_' || ch.is_uppercase() && !current.is_empty() {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current).to_ascii_lowercase());
            }
            if ch != '_' {
                current.push(ch);
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        parts.push(current.to_ascii_lowercase());
    }
    parts
}

fn id<T>(value: &str) -> Result<T, CandidateOutputError>
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Display,
{
    T::try_from(value.to_owned()).map_err(|error| CandidateOutputError::Contract(error.to_string()))
}

fn canonical_sha256<T: Serialize>(value: &T) -> Result<String, CandidateOutputError> {
    let bytes = canonical_json_bytes(value)?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}

fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, CandidateOutputError> {
    let mut bytes = serde_json::to_vec(value)
        .map_err(|error| CandidateOutputError::Contract(format!("serialize: {error}")))?;
    // Stable formatting: re-parse and dump sorted keys via serde_json Value.
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| CandidateOutputError::Contract(format!("reparse: {error}")))?;
    bytes = serde_json::to_vec(&sort_value(value))
        .map_err(|error| CandidateOutputError::Contract(format!("reserialize: {error}")))?;
    Ok(bytes)
}

fn sort_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut ordered = serde_json::Map::new();
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            for key in keys {
                if let Some(child) = map.get(&key) {
                    ordered.insert(key, sort_value(child.clone()));
                }
            }
            serde_json::Value::Object(ordered)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(sort_value).collect())
        }
        other => other,
    }
}

fn write_pretty_json(path: &Path, value: &impl Serialize) -> Result<(), CandidateOutputError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| CandidateOutputError::Contract(format!("serialize: {error}")))?;
    fs::write(path, bytes).map_err(|source| CandidateOutputError::Write {
        path: path.to_path_buf(),
        source,
    })
}

fn peak_rss_bytes() -> Option<u64> {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        return None;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok())?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

fn toolchain_fingerprint() -> String {
    format!(
        "rustc:{}",
        option_env!("RUSTC_COMMIT_HASH").unwrap_or("unknown")
    )
}

fn hardware_fingerprint() -> String {
    std::env::consts::ARCH.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn workload() -> CandidateWorkloadV1 {
        load_candidate_workload(&repo_root().join(WORKLOAD_RELATIVE)).expect("workload loads")
    }

    #[test]
    fn direct_workload_requires_checked_in_labels() {
        let mut workload = workload();
        workload.queries[0].label = None;
        let error = validate_workload_for_tuning(&workload).expect_err("missing label");
        assert!(error.to_string().contains("missing its checked-in label"));
    }

    #[test]
    fn direct_workload_rejects_ambiguous_corpus_identity() {
        let mut workload = workload();
        workload.corpus[1].document_id = workload.corpus[0].document_id.clone();
        let error = validate_workload_for_tuning(&workload).expect_err("duplicate document id");
        assert!(error.to_string().contains("duplicate corpus document_id"));

        let mut workload = workload();
        workload.corpus[1].path = workload.corpus[0].path.clone();
        let error = validate_workload_for_tuning(&workload).expect_err("duplicate corpus path");
        assert!(error.to_string().contains("duplicate corpus path"));
    }

    #[test]
    fn direct_workload_rejects_empty_and_duplicate_query_ids() {
        let mut empty = workload();
        empty.queries[0].query_id.clear();
        let error = validate_workload_for_tuning(&empty).expect_err("empty query id");
        assert!(error.to_string().contains("query_id must not be empty"));

        let mut duplicate = workload();
        duplicate.queries[1].query_id = duplicate.queries[0].query_id.clone();
        let error = validate_workload_for_tuning(&duplicate).expect_err("duplicate query id");
        assert!(error.to_string().contains("duplicate query_id"));
    }

    #[test]
    fn direct_workload_rejects_duplicate_profile_ids_and_empty_partitions() {
        let mut duplicate = workload();
        duplicate.profile_matrix[1].profile_id = duplicate.profile_matrix[0].profile_id.clone();
        let error = validate_workload_for_tuning(&duplicate).expect_err("duplicate profile id");
        assert!(error.to_string().contains("duplicate profile_id"));

        let mut missing = workload();
        missing.queries.retain(|query| query.partition == "train");
        let error = validate_workload_for_tuning(&missing).expect_err("empty validation partition");
        assert!(
            error
                .to_string()
                .contains("partition validation has no queries")
        );
    }

    #[test]
    fn candidate_generation_rejects_partially_unknown_profile_selection() {
        let error = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: &repo_root(),
            workload_path: None,
            profile_ids: Some(&["pr9-fallback".to_owned(), "unknown-profile".to_owned()]),
        })
        .expect_err("unknown profile");
        assert!(error.to_string().contains("unknown requested profile_id"));
    }

    #[test]
    fn direct_outputs_cover_train_and_validation() {
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: &repo_root(),
            workload_path: None,
            profile_ids: Some(&["pr9-fallback".to_owned()]),
        })
        .expect("generate");
        assert_eq!(result.outputs.len(), 2);
        let expected_corpus_digest =
            compute_corpus_digest(&repo_root(), &workload()).expect("corpus digest");
        for output in &result.outputs {
            assert_eq!(output.schema_version, 2);
            assert!(output.partition == "train" || output.partition == "validation");
            assert_eq!(output.production_boundary, PRODUCTION_BOUNDARY);
            assert_eq!(output.cancellation, REQUIRED_CANCELLATION);
            assert_eq!(output.offline, REQUIRED_OFFLINE);
            assert_eq!(output.fallback_digest, output.pr9_fallback_digest);
            assert_eq!(output.corpus_digest, expected_corpus_digest);
            assert_eq!(output.seed, EVALUATION_SEED);
            assert_eq!(output.cache_state, EVALUATION_CACHE_STATE);
            let current = output.resources.get("current").expect("current samples");
            assert_eq!(current.status, OptionalStageMeasurementV1::Pending);
            assert_eq!(
                current.measured_queries,
                current.latency_samples_us.len() as u64
            );
            assert!(
                serde_json::to_value(current)
                    .expect("resource sample serializes")
                    .get("p99_latency_us")
                    .is_none(),
                "small raw samples must not manufacture p99"
            );
            let ten_x = output.resources.get("10x").expect("10x status");
            assert_eq!(ten_x.status, OptionalStageMeasurementV1::Pending);
            assert_eq!(ten_x.measured_queries, 0);
            assert!(ten_x.latency_samples_us.is_empty());
            assert!(ten_x.peak_rss_bytes.is_none());
            assert!(
                output.queries.iter().all(|query| {
                    serde_json::to_value(query)
                        .expect("query serializes")
                        .get("confidence_ppm")
                        .is_none()
                }),
                "candidate rows must not manufacture confidence"
            );
        }
    }

    #[test]
    fn unsupported_complete_optional_stage_claim_is_rejected() {
        let error = serde_json::from_value::<OptionalStageMeasurementsV1>(serde_json::json!({
            "semantic": "complete",
            "rerank": "not_requested"
        }))
        .expect_err("complete requires an evidence-bearing schema");
        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn repeated_queries_do_not_claim_a_ten_x_resource_measurement() {
        let workload = workload();
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: &repo_root(),
            workload_path: None,
            profile_ids: Some(&["pr9-fallback".to_owned()]),
        })
        .expect("generate");
        let report =
            crate::search_eval::evaluate_generated_outputs(&repo_root(), &workload, &result)
                .expect("evaluate");

        assert_eq!(
            report.status,
            crate::search_eval::DirectEvaluationStatusV1::Pending
        );
        assert!(report.profiles.iter().all(|profile| {
            profile.resource_status == crate::search_eval::DirectEvaluationStatusV1::Pending
        }));

        let mut missing_resource = result.clone();
        missing_resource.outputs[0].resources.remove("10x");
        let report = crate::search_eval::evaluate_generated_outputs(
            &repo_root(),
            &workload,
            &missing_resource,
        )
        .expect("evaluate");
        assert_eq!(
            report.status,
            crate::search_eval::DirectEvaluationStatusV1::Fail
        );
        assert_eq!(
            report.profiles[0].resource_status,
            crate::search_eval::DirectEvaluationStatusV1::Fail
        );

        let mut duplicate_query = result.clone();
        duplicate_query.outputs[0].queries[1] = duplicate_query.outputs[0].queries[0].clone();
        let error = crate::search_eval::evaluate_generated_outputs(
            &repo_root(),
            &workload,
            &duplicate_query,
        )
        .expect_err("duplicate query row");
        assert!(error.to_string().contains("duplicate query row"));

        let mut duplicate_profile_partition = result.clone();
        duplicate_profile_partition.outputs[1] = duplicate_profile_partition.outputs[0].clone();
        let error = crate::search_eval::evaluate_generated_outputs(
            &repo_root(),
            &workload,
            &duplicate_profile_partition,
        )
        .expect_err("duplicate profile partition");
        assert!(error.to_string().contains("duplicate profile/partition"));

        let mut forged = result;
        forged.outputs[0].production_boundary = "lookalike".to_owned();
        let error =
            crate::search_eval::evaluate_generated_outputs(&repo_root(), &workload, &forged)
                .expect_err("forged production boundary");
        assert!(error.to_string().contains("production boundary"));

        forged.outputs[0].production_boundary = PRODUCTION_BOUNDARY.to_owned();
        forged.outputs[0].fixture_source_commit = "forged".to_owned();
        let error =
            crate::search_eval::evaluate_generated_outputs(&repo_root(), &workload, &forged)
                .expect_err("forged source commit");
        assert!(error.to_string().contains("source commit"));

        forged.outputs[0].fixture_source_commit = workload.source_repository_commit.clone();
        forged.outputs[0].corpus_digest = canonical_sha256(&"forged corpus").expect("digest");
        let error =
            crate::search_eval::evaluate_generated_outputs(&repo_root(), &workload, &forged)
                .expect_err("forged corpus digest");
        assert!(error.to_string().contains("byte-exact corpus"));

        forged.outputs[0].corpus_digest =
            compute_corpus_digest(&repo_root(), &workload).expect("corpus digest");
        forged.outputs[0].toolchain.clear();
        let error =
            crate::search_eval::evaluate_generated_outputs(&repo_root(), &workload, &forged)
                .expect_err("missing environment");
        assert!(error.to_string().contains("environment summary"));

        forged.outputs[0].toolchain = "rustc:test".to_owned();
        forged.outputs[0].queries[0].abstained = !forged.outputs[0].queries[0].ranked.is_empty();
        let error =
            crate::search_eval::evaluate_generated_outputs(&repo_root(), &workload, &forged)
                .expect_err("inconsistent abstention");
        assert!(error.to_string().contains("inconsistent abstention"));
    }

    #[test]
    fn candidate_bytes_match_direct_production_calls() {
        let workload = workload();
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: &repo_root(),
            workload_path: None,
            profile_ids: Some(&["pr9-fallback".to_owned()]),
        })
        .expect("generate");
        let train = result
            .outputs
            .iter()
            .find(|output| output.partition == "train" && output.profile_id == "pr9-fallback")
            .expect("train output");
        let probe = train.queries.first().expect("at least one train query");
        let direct = retrieve_partition_query_bytes(
            &repo_root(),
            &workload,
            "pr9-fallback",
            &probe.query_id,
        )
        .expect("direct retrieve");
        let generated = canonical_json_bytes(probe).expect("generated bytes");
        assert_eq!(
            generated, direct,
            "generator row must match direct production call bytes"
        );
    }

    #[test]
    fn semantic_profiles_do_not_claim_a_comparison_when_only_fallback_ran() {
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: &repo_root(),
            workload_path: None,
            profile_ids: Some(&["hybrid-conservative".to_owned()]),
        })
        .expect("generate");

        for output in result.outputs {
            assert_eq!(
                output.optional_stages.semantic,
                OptionalStageMeasurementV1::Pending
            );
            assert_eq!(
                output.optional_stages.rerank,
                OptionalStageMeasurementV1::NotRequested
            );
        }
    }

    #[test]
    fn rerank_profiles_remain_pending_when_no_rerank_measurement_ran() {
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: &repo_root(),
            workload_path: None,
            profile_ids: Some(&["hybrid-reranked".to_owned()]),
        })
        .expect("generate");

        for output in result.outputs {
            assert_eq!(
                output.optional_stages.semantic,
                OptionalStageMeasurementV1::Pending
            );
            assert_eq!(
                output.optional_stages.rerank,
                OptionalStageMeasurementV1::Pending
            );
        }
    }
}
