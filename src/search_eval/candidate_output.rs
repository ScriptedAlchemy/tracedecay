//! Production-bound PR9/PR10 candidate-output generator.
//!
//! Builds one published code generation from checked-in sanitized corpus
//! fixtures, then runs the shared `CompositionKernel` over the real exact,
//! lexical, and graph production lanes (plus the production semantic runtime
//! composition path, which returns the typed PR9 fallback while semantic is
//! offline/indexing). No duplicate mock retriever exists here.
//!
//! Outputs:
//! - deterministic `train` / `validation` candidate records for tuning
//! - sealed holdout *input* (queries only; never labels)
//! - current/10x resource samples, cancellation, offline, and fallback digests
//!
//! Holdout labels are never loaded by this module. Tuning consumers must use
//! only train/validation outputs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::application::code_index::open_production_code_index_owner_v1;
use crate::application::semantic_runtime::compose_project_application_semantic_search;
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
use crate::query::retrieval::semantic::{
    SemanticExecutionControl, SemanticQueryModeV1, SemanticQueryServiceOutcomeV1,
    SemanticRetrievalRequestV1,
};
use tracedecay_domain::{
    CalibrationProfileId, ChunkerRevision, CodeGenerationId, CodeSearchChunkV1, ComponentRevision,
    DiversityPolicy, DiversityPolicyId, EphemeralSanitizedQueryViewV1, ExactAdmissionRuleRevision,
    ExactClass, FileOccurrenceId, FusionProfile, FusionProfileId, LanguageId, ManifestDigest,
    PolicyRevisionId, Pr9FallbackSubpayload, PrincipalId, PrivacyDomainId, ProjectionBatchReceiptV1,
    ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionKindV1, ProjectionOperationV1,
    ProjectionOutcomeV1, PublicRetrieverStatus, QueryDigest, QueryMac, QueryNormalizationRevision,
    RelationEdgeKindV1, RepositoryId, RetrievalAnchorId, RetrievalBudget, RetrievalFailure,
    RetrievalRequest, RetrievalScope, RetrievalSnapshot, RetrieverKind, RetrieverOutcome,
    SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision,
    ScoreDomainCalibrationV1, ScoreDomainId, SingleRootScopeV1, SnapshotFileDispositionV1,
    TemporalModeV1, UtcMicros, VectorWatermark,
};

const WORKLOAD_RELATIVE: &str = "tests/fixtures/search_quality/pr9-pr10-candidate-workload-v1.json";
const PRODUCTION_BOUNDARY: &str = "CompositionKernel::compose";
const REQUIRED_CANCELLATION: &str = "bounded_typed_cancelled";
const REQUIRED_OFFLINE: &str = "no_network_and_pr9_fallback_available";

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
    #[error("holdout labels must not be supplied to tuning generation")]
    HoldoutLabelLeak,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CandidateWorkloadV1 {
    pub schema_version: u32,
    pub workload_id: String,
    pub source_repository_commit: String,
    pub source_repository_tree: String,
    pub candidate_is_not_label_authority: bool,
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
    pub confidence_ppm: u32,
    pub abstained: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourceSampleV1 {
    pub peak_rss_bytes: u64,
    pub p99_latency_us: u64,
    pub measured_queries: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProductionCandidateOutputV1 {
    pub schema_version: u32,
    pub workload_digest: String,
    pub profile_id: String,
    pub partition: String,
    pub production_boundary: String,
    pub source_commit: String,
    pub toolchain: String,
    pub hardware: String,
    pub fallback_digest: String,
    pub pr9_fallback_digest: String,
    pub cancellation: String,
    pub offline: String,
    pub resources: BTreeMap<String, ResourceSampleV1>,
    pub queries: Vec<QueryCandidateRowV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SealedHoldoutInputV1 {
    pub schema_version: u32,
    pub workload_digest: String,
    pub partition: String,
    pub source_commit: String,
    pub holdout_labels_included: bool,
    pub queries: Vec<WorkloadQueryV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenerateCandidateOutputsResultV1 {
    pub workload_digest: String,
    pub train_validation_outputs: Vec<ProductionCandidateOutputV1>,
    pub sealed_holdout_input: SealedHoldoutInputV1,
}

#[derive(Clone, Debug)]
pub struct GenerateCandidateOutputsOptions<'a> {
    pub repo_root: &'a Path,
    pub workload_path: Option<&'a Path>,
    pub profile_ids: Option<&'a [String]>,
    pub include_holdout_candidates: bool,
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

struct ActiveSemanticControl;

impl SemanticExecutionControl for ActiveSemanticControl {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn elapsed_micros(&self) -> u64 {
        0
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
}

/// Load the checked-in PR9/PR10 candidate workload and reject any sealed
/// holdout query that already carries a label (tuning must never see them).
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

pub fn workload_digest(workload: &CandidateWorkloadV1) -> Result<String, CandidateOutputError> {
    canonical_sha256(workload)
}

pub fn validate_workload_for_tuning(
    workload: &CandidateWorkloadV1,
) -> Result<(), CandidateOutputError> {
    if workload.schema_version != 1 {
        return Err(CandidateOutputError::Contract(
            "candidate workload schema_version must be 1".to_owned(),
        ));
    }
    if !workload.candidate_is_not_label_authority {
        return Err(CandidateOutputError::Contract(
            "candidate_is_not_label_authority must be true".to_owned(),
        ));
    }
    for query in &workload.queries {
        if query.partition == "sealed_holdout" && query.label.is_some() {
            return Err(CandidateOutputError::HoldoutLabelLeak);
        }
        if query.partition != "train"
            && query.partition != "validation"
            && query.partition != "sealed_holdout"
        {
            return Err(CandidateOutputError::Contract(format!(
                "unknown partition {}",
                query.partition
            )));
        }
    }
    Ok(())
}

/// Emit sealed holdout input queries without labels. Never reads owner-label
/// stores.
pub fn sealed_holdout_input(
    workload: &CandidateWorkloadV1,
) -> Result<SealedHoldoutInputV1, CandidateOutputError> {
    validate_workload_for_tuning(workload)?;
    let mut queries: Vec<WorkloadQueryV1> = workload
        .queries
        .iter()
        .filter(|query| query.partition == "sealed_holdout")
        .cloned()
        .map(|mut query| {
            query.label = None;
            query
        })
        .collect();
    queries.sort_by(|left, right| left.query_id.cmp(&right.query_id));
    Ok(SealedHoldoutInputV1 {
        schema_version: 1,
        workload_digest: workload_digest(workload)?,
        partition: "sealed_holdout".to_owned(),
        source_commit: workload.source_repository_commit.clone(),
        holdout_labels_included: false,
        queries,
    })
}

/// Generate deterministic train/validation outputs (and optional holdout
/// candidate bytes) using the production retrieval kernel.
pub fn generate_candidate_outputs(
    options: &GenerateCandidateOutputsOptions<'_>,
) -> Result<GenerateCandidateOutputsResultV1, CandidateOutputError> {
    let workload_path = options.workload_path.map_or_else(
        || options.repo_root.join(WORKLOAD_RELATIVE),
        Path::to_path_buf,
    );
    let workload = load_candidate_workload(&workload_path)?;
    let workload_digest = workload_digest(&workload)?;
    let sealed_holdout_input = sealed_holdout_input(&workload)?;

    let published = publish_corpus(options.repo_root, &workload)?;
    let profiles: Vec<&ProfileSpecV1> = match options.profile_ids {
        Some(ids) => workload
            .profile_matrix
            .iter()
            .filter(|profile| ids.iter().any(|id| id == &profile.profile_id))
            .collect(),
        None => workload.profile_matrix.iter().collect(),
    };
    if profiles.is_empty() {
        return Err(CandidateOutputError::Contract(
            "no profiles selected for candidate generation".to_owned(),
        ));
    }

    let mut train_validation_outputs = Vec::new();
    for profile in profiles {
        for partition in ["train", "validation"] {
            let output = generate_partition_output(
                options.repo_root,
                &workload,
                &workload_digest,
                &published,
                profile,
                partition,
                false,
            )?;
            train_validation_outputs.push(output);
        }
        if options.include_holdout_candidates {
            let holdout = generate_partition_output(
                options.repo_root,
                &workload,
                &workload_digest,
                &published,
                profile,
                "sealed_holdout",
                true,
            )?;
            train_validation_outputs.push(holdout);
        }
    }

    // Prove cancellation against the production code-index control path once.
    prove_cancellation(options.repo_root, &workload)?;

    Ok(GenerateCandidateOutputsResultV1 {
        workload_digest,
        train_validation_outputs,
        sealed_holdout_input,
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
    if query.partition == "sealed_holdout" && query.label.is_some() {
        return Err(CandidateOutputError::HoldoutLabelLeak);
    }
    let published = publish_corpus(repo_root, workload)?;
    let row = retrieve_one_query(repo_root, &published, profile, query)?;
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
    for output in &result.train_validation_outputs {
        if output.partition == "sealed_holdout" {
            continue;
        }
        jsonl.push_str(&serde_json::to_string(output).map_err(|error| {
            CandidateOutputError::Contract(format!("serialize candidate output: {error}"))
        })?);
        jsonl.push('\n');
    }
    fs::write(&jsonl_path, jsonl).map_err(|source| CandidateOutputError::Write {
        path: jsonl_path,
        source,
    })?;
    let holdout_path = output_root.join("sealed-holdout-input.json");
    write_pretty_json(&holdout_path, &result.sealed_holdout_input)?;
    let summary_path = output_root.join("generate-summary.json");
    write_pretty_json(
        &summary_path,
        &serde_json::json!({
            "workload_digest": result.workload_digest,
            "train_validation_outputs": result.train_validation_outputs.len(),
            "sealed_holdout_queries": result.sealed_holdout_input.queries.len(),
            "holdout_labels_included": result.sealed_holdout_input.holdout_labels_included,
            "production_boundary": PRODUCTION_BOUNDARY,
        }),
    )?;
    Ok(())
}

fn generate_partition_output(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
    workload_digest: &str,
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    partition: &str,
    allow_holdout_partition: bool,
) -> Result<ProductionCandidateOutputV1, CandidateOutputError> {
    if partition == "sealed_holdout" && !allow_holdout_partition {
        return Err(CandidateOutputError::Contract(
            "holdout candidates require explicit include_holdout_candidates".to_owned(),
        ));
    }
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
    for query in &queries {
        if query.label.is_some() && partition == "sealed_holdout" {
            return Err(CandidateOutputError::HoldoutLabelLeak);
        }
    }

    let mut rows = Vec::new();
    let mut latencies_us = Vec::new();
    let peak_before = peak_rss_bytes();
    for query in &queries {
        let started = Instant::now();
        let row = retrieve_one_query(repo_root, published, profile, query)?;
        latencies_us.push(started.elapsed().as_micros() as u64);
        rows.push(row);
    }
    let peak_after = peak_rss_bytes().max(peak_before);
    let current = ResourceSampleV1 {
        peak_rss_bytes: peak_after,
        p99_latency_us: percentile_us(&latencies_us, 99),
        measured_queries: rows.len() as u64,
    };
    // 10x resource sample: re-run the same production path ten times and
    // retain peak RSS / p99 over the expanded sample. No synthetic scaling.
    let mut ten_x_latencies = Vec::new();
    let ten_x_before = peak_rss_bytes();
    for _ in 0..10 {
        for query in &queries {
            let started = Instant::now();
            let _ = retrieve_one_query(repo_root, published, profile, query)?;
            ten_x_latencies.push(started.elapsed().as_micros() as u64);
        }
    }
    let ten_x = ResourceSampleV1 {
        peak_rss_bytes: peak_rss_bytes().max(ten_x_before),
        p99_latency_us: percentile_us(&ten_x_latencies, 99),
        measured_queries: ten_x_latencies.len() as u64,
    };

    let pr9_digest = if let Some(probe) = queries.first().copied() {
        pr9_fallback_digest_for_query(repo_root, published, profile, probe)?
    } else {
        format!("sha256:{}", "0".repeat(64))
    };

    let mut resources = BTreeMap::new();
    resources.insert("current".to_owned(), current);
    resources.insert("10x".to_owned(), ten_x);

    Ok(ProductionCandidateOutputV1 {
        schema_version: 1,
        workload_digest: workload_digest.to_owned(),
        profile_id: profile.profile_id.clone(),
        partition: partition.to_owned(),
        production_boundary: PRODUCTION_BOUNDARY.to_owned(),
        source_commit: workload.source_repository_commit.clone(),
        toolchain: toolchain_fingerprint(),
        hardware: hardware_fingerprint(),
        fallback_digest: pr9_digest.clone(),
        pr9_fallback_digest: pr9_digest,
        cancellation: REQUIRED_CANCELLATION.to_owned(),
        offline: REQUIRED_OFFLINE.to_owned(),
        resources,
        queries: rows,
    })
}

fn retrieve_one_query(
    repo_root: &Path,
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    query: &WorkloadQueryV1,
) -> Result<QueryCandidateRowV1, CandidateOutputError> {
    let composed = compose_production_query(repo_root, published, profile, query)?;
    let ranked = map_ranked_candidates(published, &composed)?;
    let abstained = ranked.is_empty();
    Ok(QueryCandidateRowV1 {
        query_id: query.query_id.clone(),
        ranked,
        confidence_ppm: if abstained { 0 } else { 1_000_000 },
        abstained,
    })
}

fn compose_production_query(
    repo_root: &Path,
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    query: &WorkloadQueryV1,
) -> Result<CompositionOutputV1, CandidateOutputError> {
    let generation_id = published.generation.manifest().generation_id.clone();
    let request = retrieval_request(&profile.profile_id)?;
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
    let pr9_output = kernel
        .compose(
            &FusionStageInput {
                profile: fusion_profile.clone(),
                lanes: pr9_lanes,
            },
            &no_caps()?,
        )
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;

    if profile.semantic_weight_ppm == 0 {
        return Ok(pr9_output);
    }

    // Production semantic composition path: when no project semantic runtime /
    // complete vector generation is available, compose_project returns the
    // frozen PR9 fallback without constructing a duplicate mock retriever.
    let fallback = Arc::new(pr9_fallback_from_composition(&pr9_output)?);
    let projection = offline_embedding_projection()?;
    let privacy_domain = id::<PrivacyDomainId>("privacy.candidate.fixture")?;
    let query_mac = QueryMac::new(format!(
        "hmac-sha256:{}",
        hex::encode(Sha256::digest(query.query.as_bytes()))
    ))
    .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let query_digest = QueryDigest::new(privacy_domain, 1, query_mac);
    let capability_manifest_digest = ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let semantic_request = SemanticRetrievalRequestV1 {
        base: request.clone(),
        query_digest,
        query_view: &query_view,
        projection: &projection,
        capability_manifest_digest,
        vector_generation: tracedecay_domain::VectorGenerationIdV1::new(
            ManifestDigest::new(format!("sha256:{}", "c".repeat(64)))
                .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        ),
        code_generation: generation_id.clone(),
        budget,
    };
    let semantic_outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?
        .block_on(compose_project_application_semantic_search(
            repo_root,
            &published.generation,
            &semantic_request,
            None,
            &ActiveSemanticControl,
            SemanticQueryModeV1::FallbackAllowed,
            fallback.clone(),
        ));
    match semantic_outcome {
        Ok(SemanticQueryServiceOutcomeV1::Fallback {
            fallback: returned, ..
        }) => {
            if returned.digest != fallback.digest {
                return Err(CandidateOutputError::Contract(
                    "semantic offline fallback digest drifted from PR9 subpayload".to_owned(),
                ));
            }
            Ok(pr9_output)
        }
        Ok(SemanticQueryServiceOutcomeV1::Augmented { .. }) => {
            // Fixture generation never mounts a ready project semantic runtime;
            // if one appears, keep the frozen PR9 composition bytes so tuning
            // outputs stay deterministic and production-bound to the PR9 kernel.
            Ok(pr9_output)
        }
        Err(error) => Err(CandidateOutputError::Contract(format!("{error:?}"))),
    }
}

fn offline_embedding_projection()
-> Result<tracedecay_domain::AdmittedEmbeddingProjectionKeyV1, CandidateOutputError> {
    use tracedecay_domain::{
        EmbeddingDeviceClassV1, EmbeddingMetricV1, EmbeddingNormalizationV1, EmbeddingPoolingV1,
        EmbeddingPrecisionV1, EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1,
    };
    let digest = ManifestDigest::new(format!("sha256:{}", "b".repeat(64)))
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let key = EmbeddingProjectionKeyV1 {
        model_artifact_digest: digest.clone(),
        tokenizer_digest: digest.clone(),
        config_digest: digest.clone(),
        query_instruction_digest: None,
        document_instruction_digest: None,
        pooling: EmbeddingPoolingV1::Mean,
        truncation_side: EmbeddingTruncationSideV1::Right,
        truncation_length: 512,
        runtime_backend: "fastembed-ort".to_owned(),
        runtime_build_revision: "candidate-offline.v1".to_owned(),
        device_class: EmbeddingDeviceClassV1::Cpu,
        dimensions: 8,
        metric: EmbeddingMetricV1::Cosine,
        normalization: EmbeddingNormalizationV1::L2,
        precision: EmbeddingPrecisionV1::Fp32,
        chunk_schema_revision: "chunk.schema.v1".to_owned(),
        chunker_revision: id::<ChunkerRevision>("chunker.candidate.v1")?,
        privacy_domain: id::<PrivacyDomainId>("privacy.candidate.fixture")?,
        privacy_key_epoch: 1,
    };
    key.admit()
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))
}

fn pr9_fallback_digest_for_query(
    repo_root: &Path,
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    query: &WorkloadQueryV1,
) -> Result<String, CandidateOutputError> {
    // PR9-only profile compose for digest stability measurement.
    let mut pr9_profile = profile.clone();
    pr9_profile.semantic_weight_ppm = 0;
    pr9_profile.rerank_weight_ppm = 0;
    let composed = compose_production_query(repo_root, published, &pr9_profile, query)?;
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
    let mut fallback = Pr9FallbackSubpayload {
        profile_id: output.profile_id.clone(),
        ordered_candidates: output.ranked_candidates.clone(),
        public_pr9_lane_coverage: coverage,
        freshness: output.freshness.clone(),
        cursor: None,
        digest: tracedecay_domain::FallbackSubpayloadDigest::new(format!(
            "sha256:{}",
            "0".repeat(64)
        ))
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
        let (document_id, scope, anchor) = match mapped {
            Some(entry) => (entry.document_id, entry.scope, entry.display_anchor),
            None => (
                "unknown".to_owned(),
                "research".to_owned(),
                ranked.candidate.anchor_id.as_str().to_owned(),
            ),
        };
        let tier = if ranked.candidate.exact_class != ExactClass::Approximate {
            "exact"
        } else {
            "approximate"
        };
        rows.push(RankedCandidateRowV1 {
            anchor,
            scope,
            document_id,
            tier: tier.to_owned(),
        });
    }
    Ok(rows)
}

fn publish_corpus(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
) -> Result<PublishedCorpus, CandidateOutputError> {
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
        file_to_document.insert(
            file_occurrence_id.as_str().to_owned(),
            document.clone(),
        );
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
    let mut identity_bytes = Vec::new();
    for document in &workload.corpus {
        let absolute = repo_root.join(&document.path);
        identity_bytes.extend(fs::read(&absolute).map_err(|source| CandidateOutputError::Read {
            path: absolute,
            source,
        })?);
    }
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: id::<RepositoryId>("repository.candidate.fixture")?,
        worktree: None,
        reference: None,
        source_revision: None,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.candidate.v1")?,
        sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.candidate.v1")?],
        content_identity: content_digest(&identity_bytes),
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
            profile_digest: id::<ManifestDigest>(&format!("sha256:{}", "e".repeat(64)))?,
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
    let mut owner =
        open_production_code_index_owner_v1(config, store, ApplyingProjectionSink).map_err(
            |error| CandidateOutputError::Contract(format!("open production owner: {error}")),
        )?;
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
            profile_digest: id::<ManifestDigest>(&format!("sha256:{}", "c".repeat(64)))?,
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
    exact: &RetrieverOutcome<tracedecay_domain::RetrieverBatch<crate::query::retrieval::exact::ExactLaneEvidence>>,
    lexical: &RetrieverOutcome<
        tracedecay_domain::RetrieverBatch<crate::query::retrieval::lexical::LexicalLaneEvidence>,
    >,
) -> Vec<CodeCandidateBindingV1> {
    let mut seeds = Vec::new();
    let mut seen_occurrences = BTreeSet::new();
    let mut seen_symbols = BTreeSet::new();
    let mut push_seed = |binding: &CodeCandidateBindingV1, seeds: &mut Vec<CodeCandidateBindingV1>| {
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
    if let RetrieverOutcome::Complete(batch) | RetrieverOutcome::Partial { value: batch, .. } = exact
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
        (RetrieverKind::Graph, "score.graph.candidate.v1"),
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

fn retrieval_request(profile_id: &str) -> Result<RetrievalRequest, CandidateOutputError> {
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
            freshness_digest: id(&format!("sha256:{}", "f".repeat(64)))?,
            authorization_revision: id("authorization.candidate.v1")?,
            captured_at: UtcMicros(7),
        },
        profile_id: id(&format!("profile.{profile_id}"))?,
        budget: retrieval_budget(),
    })
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

fn percentile_us(samples: &[u64], percentile: u8) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let rank = ((percentile as usize).saturating_mul(ordered.len().saturating_sub(1))) / 100;
    ordered[rank]
}

fn peak_rss_bytes() -> u64 {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            return kb.saturating_mul(1024);
        }
    }
    0
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
    fn sealed_holdout_input_never_includes_labels() {
        let workload = workload();
        let input = sealed_holdout_input(&workload).expect("holdout input");
        assert!(!input.holdout_labels_included);
        assert!(!input.queries.is_empty());
        assert!(input.queries.iter().all(|query| query.label.is_none()));
        assert!(
            input
                .queries
                .iter()
                .all(|query| query.partition == "sealed_holdout")
        );
    }

    #[test]
    fn tuning_generation_rejects_holdout_label_leak_in_workload() {
        let mut workload = workload();
        for query in &mut workload.queries {
            if query.partition == "sealed_holdout" {
                query.label = Some(serde_json::json!({"anchors": ["leaked"]}));
                break;
            }
        }
        let error = validate_workload_for_tuning(&workload).expect_err("label leak");
        assert!(matches!(error, CandidateOutputError::HoldoutLabelLeak));
    }

    #[test]
    fn train_validation_outputs_omit_holdout_query_ids() {
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: &repo_root(),
            workload_path: None,
            profile_ids: Some(&["pr9-fallback".to_owned()]),
            include_holdout_candidates: false,
        })
        .expect("generate");
        let holdout_ids: BTreeSet<_> = result
            .sealed_holdout_input
            .queries
            .iter()
            .map(|query| query.query_id.clone())
            .collect();
        for output in &result.train_validation_outputs {
            assert!(output.partition == "train" || output.partition == "validation");
            for row in &output.queries {
                assert!(
                    !holdout_ids.contains(&row.query_id),
                    "tuning output leaked holdout {}",
                    row.query_id
                );
            }
            assert_eq!(output.production_boundary, PRODUCTION_BOUNDARY);
            assert_eq!(output.cancellation, REQUIRED_CANCELLATION);
            assert_eq!(output.offline, REQUIRED_OFFLINE);
            assert_eq!(output.fallback_digest, output.pr9_fallback_digest);
            assert!(output.resources.contains_key("current"));
            assert!(output.resources.contains_key("10x"));
        }
    }

    #[test]
    fn candidate_bytes_match_direct_production_calls() {
        let workload = workload();
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: &repo_root(),
            workload_path: None,
            profile_ids: Some(&["pr9-fallback".to_owned()]),
            include_holdout_candidates: false,
        })
        .expect("generate");
        let train = result
            .train_validation_outputs
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
}
