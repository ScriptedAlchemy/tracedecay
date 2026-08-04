//! Production-bound query/semantic candidate-output generator.
//!
//! Builds one published code generation from checked-in sanitized corpus
//! fixtures, then runs the shared `CompositionKernel` over the real exact,
//! lexical, and graph production lanes. A separate native entry point accepts
//! admitted semantic/rerank authorities; missing optional stages stay pending.
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

use super::semantic_native::{
    SemanticChannelAblationV1, SemanticNativeHydrationMeasurementV1, SemanticNativeQueryInputV1,
    SemanticNativeQueryOutputV1, SemanticNativeQueryStageMeasurementsV1,
    SemanticNativeRerankInputV1, SemanticNativeResourceEvidenceV1, SemanticNativeResourceSampleV1,
    SemanticNativeSemanticInputV1, SemanticNativeStageMeasurementV1, SemanticNativeStageResultV1,
    SemanticProjectionCaseSampleV1, SemanticProjectionCaseV1, evaluate_native_query,
};
use tracedecay_application::historical_query::{
    HistoricalGitQueryAdapter, HistoricalGitReadOutcomeV1, HistoricalGitReadUnavailableReasonV1,
    HistoricalQueryRequestV1, HistoricalRenameModeV1, HistoricalSourceAuthorizationV1,
};
use tracedecay_application::{
    NativeHistoricalBlobReaderV1, ResolvedScope, is_canonical_repository_relative_path,
};
use tracedecay_code_index::chunks::content_digest;
use tracedecay_code_index::graph_projection::CodeGraphEvidenceReader;
use tracedecay_code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use tracedecay_code_index::production::{
    CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
    CodeIndexExecutionControlV1, CodeIndexGenerationScopeV1, CodeIndexProductionConfigV1,
    CodeIndexProductionOwnerV1, CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
};
use tracedecay_code_index::projection::{
    ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionSinkErrorV1, build_batch_receipt,
};
use tracedecay_domain::git::GitOidV1;
use tracedecay_domain::{
    CalibrationProfileId, ChunkerRevision, CodeGenerationId, CodeSearchChunkId, CodeSearchChunkV1,
    ComponentRevision, DiversityPolicy, DiversityPolicyId, EphemeralSanitizedQueryViewV1,
    ExactAdmissionRuleRevision, ExactClass, FileOccurrenceId, FusionProfile, FusionProfileId,
    HydrationReceipt, HydrationRevision, LanguageId, ManifestDigest, PolicyRevisionId, PrincipalId,
    PrivacyDomainId, ProjectId, ProjectionBatchReceiptV1, ProjectionBatchRequestV1,
    ProjectionKeyV1, ProjectionKindV1, ProjectionOperationV1, ProjectionOutcomeV1,
    PublicRetrieverStatus, QueryFallbackSubpayload, QueryNormalizationRevision, RelationEdgeKindV1,
    RepositoryId, RerankPolicy, RetrievalAnchorId, RetrievalBudget, RetrievalFailure,
    RetrievalRequest, RetrievalScope, RetrievalSnapshot, RetrieverKind, RetrieverOutcome,
    SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision,
    ScoreDomainCalibrationV1, ScoreDomainId, SingleRootScopeV1, SnapshotFileDispositionV1,
    TemporalModeV1, UtcMicros, VectorGenerationIdV1, VectorWatermark,
};
use tracedecay_query::retrieval::exact::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLane, ExactLaneRequest,
    ExactLaneRetriever,
};
use tracedecay_query::retrieval::fusion::{
    CompositionKernel, CompositionLaneInput, CompositionOutputV1, FusionStageInput,
};
use tracedecay_query::retrieval::graph::{
    GraphLane, GraphLaneRequest, GraphLaneRetriever, production_code_index_freshness,
};
use tracedecay_query::retrieval::hydrate::{
    CanonicalLateHydration, HydrationAuthorizationV1, HydrationPreflightOutcomeV1,
    HydrationReadOutcomeV1, HydrationUnavailableV1, HydrationWorkPermitV1, LateHydrationSource,
};
use tracedecay_query::retrieval::lexical::{
    CodeLexicalProjectionAdapterV1, CodeLexicalProjectionMetadataV1, LexicalLane,
    LexicalLaneRequest, LexicalLaneRetriever, lexical_query_parts,
};
use tracedecay_query::retrieval::ports::CodeCandidateBindingV1;

const WORKLOAD_RELATIVE: &str =
    "tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json";
pub(super) const PRODUCTION_BOUNDARY: &str = "CompositionKernel::compose";
pub(super) const EVALUATION_MODEL_REVISION: &str =
    "JinaEmbeddingsV2BaseCode@516f4baf13dec4ddddda8631e019b5737c8bc250";
pub(super) const EVALUATION_PROJECTION_REVISION: &str = "retriever.semantic-flat.evaluation.v1";
pub(super) const EVALUATION_RUNTIME_REVISION: &str = "semantic.fastembed.production.v1";
const REQUIRED_CANCELLATION: &str = "bounded_typed_cancelled";
const REQUIRED_OFFLINE: &str = "no_network_and_query_fallback_available";
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
    pub execution_contract: EvaluationExecutionContractV1,
    pub incremental_fixture: IncrementalFixtureV1,
    pub corpus: Vec<CorpusDocumentV1>,
    pub profile_matrix: Vec<ProfileSpecV1>,
    pub resource_budgets: ResourceBudgetsV1,
    pub decision_policy: DecisionPolicySliceV1,
    pub expected_query_fallback_digests: BTreeMap<String, String>,
    pub queries: Vec<WorkloadQueryV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvaluationExecutionContractV1 {
    pub exact_file_count: u64,
    pub exact_corpus_bytes: u64,
    pub exact_eligible_chunks_current: u64,
    pub exact_eligible_chunks_10x: u64,
    pub exact_query_count: u64,
    pub model_revision: String,
    pub projection_revision: String,
    pub fusion_revision: String,
    pub runtime_revision: String,
    pub cache_state: String,
    pub concurrency: EvaluationConcurrencyContractV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvaluationConcurrencyContractV1 {
    pub query_workers: u32,
    pub projection_workers: u32,
    pub query_execution: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IncrementalFixtureV1 {
    pub document_id: String,
    pub after_path: String,
    pub after_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CorpusDocumentV1 {
    pub document_id: String,
    /// Repository-relative identity used by production path/history lanes.
    pub source_path: String,
    /// Checked-in fixture path used only to read the byte-exact corpus copy.
    pub path: String,
    pub scope: String,
    pub language: String,
    pub eligibility: String,
}

#[derive(Serialize)]
struct CorpusContentBindingV1<'a> {
    document_id: &'a str,
    source_path: &'a str,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_policy: Option<EvaluationRerankPolicyV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvaluationRerankPolicyV1 {
    pub policy_id: String,
    pub max_candidates: u32,
    pub max_input_bytes: u64,
    pub max_input_tokens: u64,
    pub max_work_units: u64,
    pub max_model_invocations: u32,
    pub deadline_micros: Option<u64>,
}

/// Exact domain material exercised by the direct evaluator for one checked-in
/// profile. Evaluation anchors are placeholders until a passing report is
/// derived and must be replaced by the publishing operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectEvaluatedProfileMaterialV1 {
    pub profile: FusionProfile,
    pub diversity: DiversityPolicy,
    pub rerank: Option<RerankPolicy>,
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
    pub historical_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RankedCandidateRowV1 {
    pub anchor: String,
    pub anchors: Vec<String>,
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
    pub historical: HistoricalQueryExecutionV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<SemanticNativeQueryOutputV1>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
pub enum HistoricalQueryExecutionV1 {
    NotRequested,
    Complete,
    Unavailable(HistoricalGitReadUnavailableReasonV1),
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
    Complete,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OptionalStageMeasurementsV1 {
    pub semantic: OptionalStageMeasurementV1,
    pub rerank: OptionalStageMeasurementV1,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceMeasurementStatusV1 {
    Measured,
    Pending,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourceSampleV1 {
    pub status: ResourceMeasurementStatusV1,
    pub eligible_chunks: u64,
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
    pub profile_material_digest: String,
    pub fallback_digest: String,
    pub query_fallback_digest: String,
    pub expected_query_fallback_digest: String,
    pub query_fallback_matches_expected: bool,
    pub cancellation: String,
    pub offline: String,
    pub optional_stages: OptionalStageMeasurementsV1,
    pub resources: BTreeMap<String, ResourceSampleV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_resources: Option<SemanticNativeResourceEvidenceV1>,
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
    /// Authoritative identity for `repo_root`, injected by the composing
    /// binary. See [`AdmittedCorpusScopeFn`].
    pub admitted_scope: AdmittedCorpusScopeFn,
}

/// Immutable identity and request material prepared by the production QUERY
/// generator for one native semantic query.
pub struct ProductionCandidateNativeQueryContextV1<'a> {
    pub profile: &'a ProfileSpecV1,
    pub query: &'a WorkloadQueryV1,
    pub request: &'a RetrievalRequest,
    pub query_view: &'a EphemeralSanitizedQueryViewV1,
    pub code: &'a CodeIndexPublishedGenerationV1,
    pub code_generation: &'a CodeGenerationId,
    pub semantic_allowed_chunks: &'a BTreeSet<CodeSearchChunkId>,
    pub rerank_policy: Option<&'a RerankPolicy>,
}

/// Genuine optional runtime inputs borrowed only for the evaluator call.
pub struct ProductionCandidateNativeQueryInputsV1<'a> {
    pub semantic: Option<SemanticNativeSemanticInputV1<'a>>,
    pub rerank: Option<SemanticNativeRerankInputV1<'a>>,
}

/// Genuine code generations used as canonical inputs to the production
/// semantic projector/store case matrix. Replay, cancellation, and
/// incompatibility are store operations over these exact generations.
pub struct ProductionCandidateSemanticProjectionSourcesV1<'a> {
    pub one_symbol: &'a CodeIndexPublishedGenerationV1,
    pub deletion: &'a CodeIndexPublishedGenerationV1,
    pub no_op: &'a CodeIndexPublishedGenerationV1,
}

/// Exact immutable generation whose resource use must be measured.
pub struct ProductionCandidateNativeResourceContextV1<'a> {
    pub profile: &'a ProfileSpecV1,
    pub queries: &'a [&'a WorkloadQueryV1],
    pub code: &'a CodeIndexPublishedGenerationV1,
    pub incremental_code: &'a CodeIndexPublishedGenerationV1,
    pub incremental_before_content_digest: &'a str,
    pub incremental_after_content_digest: &'a str,
    pub code_generation: &'a CodeGenerationId,
    pub workload_digest: &'a str,
    pub corpus_digest: &'a str,
    pub scale: &'a str,
    pub eligible_chunks: u64,
    pub semantic_projection_sources: ProductionCandidateSemanticProjectionSourcesV1<'a>,
}

/// Production-observed generation resources. Candidate generation supplies
/// query latency/CPU/RSS and rejects any mismatched returned identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProductionCandidateNativeGenerationResourcesV1 {
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub incremental_source_generation: CodeGenerationId,
    pub incremental_source_manifest_digest: ManifestDigest,
    pub vector_generation: Option<VectorGenerationIdV1>,
    pub artifact_digest: Option<ManifestDigest>,
    pub model_bytes: u64,
    pub vector_bytes: u64,
    pub index_bytes: u64,
    pub cache_bytes: u64,
    pub clean_projection_build_micros: u64,
    pub incremental_rebuild_micros: u64,
    pub projection_cases: BTreeMap<SemanticProjectionCaseV1, SemanticProjectionCaseSampleV1>,
}

/// Production authority that supplies admitted semantic/rerank runtimes.
///
/// The callback is the only way to produce a query result: candidate
/// generation invokes `semantic_native::evaluate_native_query` inside it.
pub trait ProductionCandidateNativeExecutionAuthorityV1: Send + Sync {
    fn with_query_inputs(
        &self,
        context: ProductionCandidateNativeQueryContextV1<'_>,
        evaluate: &mut dyn for<'inputs> FnMut(
            ProductionCandidateNativeQueryInputsV1<'inputs>,
        ) -> Result<(), CandidateOutputError>,
    ) -> Result<(), CandidateOutputError>;

    fn measure_resources(
        &self,
        context: ProductionCandidateNativeResourceContextV1<'_>,
        execute_queries: &mut dyn FnMut() -> Result<Vec<u64>, CandidateOutputError>,
    ) -> Result<SemanticNativeStageResultV1<SemanticNativeResourceSampleV1>, CandidateOutputError>;
}

#[derive(Clone, Default)]
struct SharedPublicationStore {
    active: Arc<Mutex<BTreeMap<CodeIndexGenerationScopeV1, CodeIndexPublishedGenerationV1>>>,
}

impl CodeIndexAtomicPublicationPort for SharedPublicationStore {
    fn load_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        let active = self.active.lock().map_err(|_| {
            CodeIndexPublicationStoreErrorV1::Unavailable(
                "candidate-output publication lock is poisoned".to_owned(),
            )
        })?;
        Ok(active.get(scope).cloned())
    }

    fn publish_atomically(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: CodeIndexPublishedGenerationV1,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut active = self.active.lock().map_err(|_| {
            CodeIndexPublicationStoreErrorV1::Unavailable(
                "candidate-output publication lock is poisoned".to_owned(),
            )
        })?;
        if active
            .get(scope)
            .map(|current| current.manifest().generation_id.clone())
            .as_ref()
            != expected_active_generation
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        active.insert(scope.clone(), generation);
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
        let mut decisions: Vec<ChunkProjectionDecisionV1> = request
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
        decisions.extend(
            request
                .changes
                .deleted
                .iter()
                .map(|change| ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: None,
                    operation: ProjectionOperationV1::Deleted,
                    outcome: ProjectionOutcomeV1::Applied,
                    output_digest: None,
                }),
        );
        decisions.extend(
            request
                .changes
                .reused
                .iter()
                .map(|change| ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: change.current_digest.clone(),
                    operation: ProjectionOperationV1::Reused,
                    outcome: ProjectionOutcomeV1::Reused,
                    output_digest: None,
                }),
        );
        decisions.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
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
    fixture_path: String,
    display_anchors: Vec<String>,
}

/// Retrieval adapters keyed by canonical allowed-scope key (sorted, deduped),
/// as produced by [`canonical_scope_key`].
type ScopedLexicalProjections = BTreeMap<Vec<String>, CodeLexicalProjectionAdapterV1>;
type ScopedGraphEvidence = BTreeMap<Vec<String>, CodeGraphEvidenceReader>;

struct PublishedCorpus {
    generation: CodeIndexPublishedGenerationV1,
    lexical_projections: ScopedLexicalProjections,
    graph_projections: ScopedGraphEvidence,
    incremental_generation: CodeIndexPublishedGenerationV1,
    incremental_before_content_digest: String,
    incremental_after_content_digest: String,
    occurrence_map: BTreeMap<String, OccurrenceMapEntry>,
    file_scopes: BTreeMap<String, String>,
    repo_root: PathBuf,
    source_commit: GitOidV1,
    corpus: Vec<CorpusDocumentV1>,
    corpus_digest: String,
    eligible_chunks: u64,
    no_op_generation: CodeIndexPublishedGenerationV1,
    deletion_generation: CodeIndexPublishedGenerationV1,
    admitted_scope: AdmittedCorpusScopeFn,
}

/// Root-injected authoritative identity for the checkout under evaluation.
///
/// Resolving a checkout's project/repository/worktree identity reads the
/// repository identity marker and the provenance admission context, both owned
/// by the composing binary. The evaluator owns everything downstream of the
/// scope: corpus binding, source authorization, and evidence validation.
/// Returning `None` means the checkout carries no authoritative identity, which
/// the historical lane reports as a contract failure rather than guessing one.
pub type AdmittedCorpusScopeFn = fn(&Path) -> Option<ResolvedScope>;

/// Refuses every checkout. Used where historical evidence is out of scope.
pub fn no_admitted_corpus_scope(_repo_root: &Path) -> Option<ResolvedScope> {
    None
}

fn canonical_scope_key(scopes: &[String]) -> Vec<String> {
    let mut key = scopes.to_vec();
    key.sort();
    key.dedup();
    key
}

fn build_query_projections(
    generation: &CodeIndexPublishedGenerationV1,
    file_scopes: &BTreeMap<String, String>,
    queries: &[WorkloadQueryV1],
) -> Result<(ScopedLexicalProjections, ScopedGraphEvidence), CandidateOutputError> {
    let generation_id = generation.manifest().generation_id.clone();
    let freshness = production_code_index_freshness(
        generation.manifest().seal.sealed_at,
        id::<ComponentRevision>("policy.candidate.v1")?,
    )
    .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let metadata = CodeLexicalProjectionMetadataV1 {
        generation: generation_id.clone(),
        repository_id: Some(generation.snapshot().repository.clone()),
        logical_paths: generation
            .snapshot()
            .files
            .iter()
            .map(|file| (file.file_occurrence_id.clone(), file.logical_path.clone()))
            .collect(),
        freshness: freshness.clone(),
        exact_retriever_revision: id(
            tracedecay_query::retrieval::QUERY_EXACT_RETRIEVER_REVISION_V1,
        )?,
        lexical_retriever_revision: id(
            tracedecay_query::retrieval::QUERY_LEXICAL_RETRIEVER_REVISION_V1,
        )?,
        exact_score_domain: id(tracedecay_query::retrieval::QUERY_EXACT_SCORE_DOMAIN_V1)?,
    };
    let admitted = generation
        .admitted_chunks()
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let mut lexical = BTreeMap::new();
    let mut graph = BTreeMap::new();
    for scope_key in queries
        .iter()
        .map(|query| canonical_scope_key(&query.allowed_scopes))
        .collect::<BTreeSet<_>>()
    {
        let scope_contains = |file_occurrence_id: &str| {
            file_scopes
                .get(file_occurrence_id)
                .is_some_and(|scope| scope_key.binary_search(scope).is_ok())
        };
        let scoped_admitted = admitted
            .iter()
            .filter(|chunk| scope_contains(chunk.chunk().anchor.file_occurrence_id.as_str()))
            .cloned()
            .collect();
        lexical.insert(
            scope_key.clone(),
            CodeLexicalProjectionAdapterV1::new_admitted(metadata.clone(), scoped_admitted)
                .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        );
        let graph_chunks = generation
            .chunks()
            .chunks()
            .iter()
            .filter(|chunk| scope_contains(chunk.anchor.file_occurrence_id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        graph.insert(
            scope_key,
            CodeGraphEvidenceReader::new(
                generation_id.clone(),
                Some(generation.snapshot().repository.clone()),
                freshness.clone(),
                generation.edges(),
                &graph_chunks,
            )
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        );
    }
    Ok((lexical, graph))
}

/// Load the checked-in query/semantic direct-evaluation workload.
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

pub fn compute_profile_material_digest(
    profile: &ProfileSpecV1,
) -> Result<String, CandidateOutputError> {
    canonical_sha256(&("tracedecay.search-eval.profile-material.v1", profile))
}

/// Hash the declared corpus and every byte-exact checked-in document.
///
/// Including document metadata prevents ambiguous concatenation while each
/// content digest binds the bytes actually read from `repo_root`.
pub fn compute_corpus_digest(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
) -> Result<String, CandidateOutputError> {
    validate_source_bindings(repo_root, workload)?;
    let mut bindings = Vec::with_capacity(workload.corpus.len());
    let mut corpus_bytes = 0_u64;
    for document in &workload.corpus {
        let absolute = repo_root.join(&document.path);
        let bytes = fs::read(&absolute).map_err(|source| CandidateOutputError::Read {
            path: absolute,
            source,
        })?;
        corpus_bytes = corpus_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| {
                CandidateOutputError::Contract("corpus byte count overflows".to_owned())
            })?;
        bindings.push(CorpusContentBindingV1 {
            document_id: &document.document_id,
            source_path: &document.source_path,
            path: &document.path,
            scope: &document.scope,
            language: &document.language,
            eligibility: &document.eligibility,
            content_digest: content_digest(&bytes).as_str().to_owned(),
        });
    }
    if corpus_bytes != workload.execution_contract.exact_corpus_bytes {
        return Err(CandidateOutputError::Contract(format!(
            "corpus byte count mismatch: declared {}, observed {corpus_bytes}",
            workload.execution_contract.exact_corpus_bytes
        )));
    }
    canonical_sha256(&(CORPUS_DIGEST_DOMAIN, bindings))
}

fn validate_source_bindings(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
) -> Result<(), CandidateOutputError> {
    let repo = gix::open(repo_root).map_err(|error| {
        CandidateOutputError::Contract(format!(
            "open source repository {}: {error}",
            repo_root.display()
        ))
    })?;
    let oid = gix::hash::ObjectId::from_hex(workload.source_repository_commit.as_bytes()).map_err(
        |error| CandidateOutputError::Contract(format!("invalid fixture source commit: {error}")),
    )?;
    let commit = repo
        .find_object(oid)
        .map_err(|error| {
            CandidateOutputError::Contract(format!("resolve fixture source commit: {error}"))
        })?
        .try_into_commit()
        .map_err(|error| {
            CandidateOutputError::Contract(format!("fixture source is not a commit: {error}"))
        })?;
    let tree_id = commit.tree_id().map_err(|error| {
        CandidateOutputError::Contract(format!("resolve fixture source tree: {error}"))
    })?;
    if tree_id.to_string() != workload.source_repository_tree {
        return Err(CandidateOutputError::Contract(format!(
            "fixture source tree mismatch: declared {}, resolved {tree_id}",
            workload.source_repository_tree
        )));
    }
    let tree = commit.tree().map_err(|error| {
        CandidateOutputError::Contract(format!("open fixture source tree: {error}"))
    })?;
    for document in &workload.corpus {
        let entry = tree
            .lookup_entry_by_path(Path::new(&document.source_path))
            .map_err(|error| {
                CandidateOutputError::Contract(format!(
                    "resolve corpus source_path {}: {error}",
                    document.source_path
                ))
            })?
            .ok_or_else(|| {
                CandidateOutputError::Contract(format!(
                    "corpus source_path is absent from the pinned tree: {}",
                    document.source_path
                ))
            })?;
        if !entry.mode().is_blob_or_symlink() {
            return Err(CandidateOutputError::Contract(format!(
                "corpus source_path is not a blob: {}",
                document.source_path
            )));
        }
        let mut pinned_blob = entry
            .object()
            .map_err(|error| {
                CandidateOutputError::Contract(format!(
                    "open pinned corpus blob {}: {error}",
                    document.source_path
                ))
            })?
            .try_into_blob()
            .map_err(|error| {
                CandidateOutputError::Contract(format!(
                    "pinned corpus source is not a blob {}: {error}",
                    document.source_path
                ))
            })?;
        let pinned_bytes = pinned_blob.take_data();
        let fixture_path = repo_root.join(&document.path);
        let fixture_bytes =
            fs::read(&fixture_path).map_err(|source| CandidateOutputError::Read {
                path: fixture_path,
                source,
            })?;
        if fixture_bytes != pinned_bytes {
            return Err(CandidateOutputError::Contract(format!(
                "corpus fixture bytes differ from pinned source blob: {}",
                document.document_id
            )));
        }
    }
    Ok(())
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
    let contract = &workload.execution_contract;
    if contract.exact_file_count != workload.corpus.len() as u64
        || contract.exact_query_count != workload.queries.len() as u64
        || contract.exact_corpus_bytes == 0
        || contract.exact_eligible_chunks_current == 0
        || contract.exact_eligible_chunks_10x
            != contract
                .exact_eligible_chunks_current
                .checked_mul(10)
                .ok_or_else(|| {
                    CandidateOutputError::Contract(
                        "evaluation current chunk count overflows 10x".to_owned(),
                    )
                })?
        || contract.model_revision != EVALUATION_MODEL_REVISION
        || contract.projection_revision != EVALUATION_PROJECTION_REVISION
        || contract.fusion_revision != PRODUCTION_BOUNDARY
        || contract.runtime_revision != EVALUATION_RUNTIME_REVISION
        || contract.cache_state != EVALUATION_CACHE_STATE
        || contract.concurrency.query_workers != 1
        || contract.concurrency.projection_workers != 1
        || contract.concurrency.query_execution != "serial_exact_workload_order"
    {
        return Err(CandidateOutputError::Contract(
            "evaluation execution contract does not match the production workload".to_owned(),
        ));
    }
    if workload.incremental_fixture.document_id.trim().is_empty()
        || !is_canonical_repository_relative_path(&workload.incremental_fixture.after_path)
        || workload.incremental_fixture.after_sha256.len() != 64
        || !workload
            .incremental_fixture
            .after_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(CandidateOutputError::Contract(
            "incremental fixture identity/path/digest is invalid".to_owned(),
        ));
    }
    let mut document_ids = BTreeSet::new();
    let mut document_paths = BTreeSet::new();
    let mut source_paths = BTreeSet::new();
    for document in &workload.corpus {
        if [
            document.document_id.as_str(),
            document.source_path.as_str(),
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
        if !is_canonical_repository_relative_path(&document.source_path) {
            return Err(CandidateOutputError::Contract(format!(
                "corpus source_path must be a safe repository-relative path: {}",
                document.source_path
            )));
        }
        if !source_paths.insert(document.source_path.as_str()) {
            return Err(CandidateOutputError::Contract(format!(
                "duplicate corpus source_path {}",
                document.source_path
            )));
        }
    }
    if document_ids.is_empty() {
        return Err(CandidateOutputError::Contract(
            "corpus must not be empty".to_owned(),
        ));
    }
    if !document_ids.contains(workload.incremental_fixture.document_id.as_str()) {
        return Err(CandidateOutputError::Contract(
            "incremental fixture document_id is absent from the corpus".to_owned(),
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
        if (profile.rerank_weight_ppm == 0) != profile.rerank_policy.is_none() {
            return Err(CandidateOutputError::Contract(format!(
                "profile {} must bind rerank weight and policy together",
                profile.profile_id
            )));
        }
        if let Some(policy) = &profile.rerank_policy
            && (policy.policy_id.trim().is_empty()
                || policy.max_candidates == 0
                || policy.max_input_bytes == 0
                || policy.max_input_tokens == 0
                || policy.max_work_units == 0
                || policy.max_model_invocations == 0)
        {
            return Err(CandidateOutputError::Contract(format!(
                "profile {} has an invalid bounded rerank policy",
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
        if query
            .historical_commit
            .as_ref()
            .is_some_and(|commit| GitOidV1::new(commit.clone()).is_err())
        {
            return Err(CandidateOutputError::Contract(format!(
                "query {} has an invalid historical commit",
                query.query_id
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
    if workload.expected_query_fallback_digests.len() != 2
        || !["train", "validation"].into_iter().all(|partition| {
            workload
                .expected_query_fallback_digests
                .contains_key(partition)
        })
    {
        return Err(CandidateOutputError::Contract(
            "expected query fallback digests must bind train and validation".to_owned(),
        ));
    }
    if workload
        .expected_query_fallback_digests
        .values()
        .any(|digest| {
            digest.len() != 71
                || !digest.starts_with("sha256:")
                || !digest[7..]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(CandidateOutputError::Contract(
            "expected query fallback digest is not canonical".to_owned(),
        ));
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
    let published = publish_corpus(options.repo_root, &workload, options.admitted_scope)?;

    let mut outputs = Vec::new();
    for &profile in &profiles {
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
    let ten_x_published =
        publish_corpus_with_scale(options.repo_root, &workload, 10, options.admitted_scope)?;
    let expected_ten_x_chunks = published.eligible_chunks.checked_mul(10).ok_or_else(|| {
        CandidateOutputError::Contract("current eligible chunk count overflows 10x".to_owned())
    })?;
    if ten_x_published.eligible_chunks != expected_ten_x_chunks {
        return Err(CandidateOutputError::Contract(format!(
            "10x corpus produced {} eligible chunks; expected exactly {}",
            ten_x_published.eligible_chunks, expected_ten_x_chunks
        )));
    }
    for output in &mut outputs {
        let profile = profiles
            .iter()
            .copied()
            .find(|profile| profile.profile_id == output.profile_id)
            .ok_or_else(|| {
                CandidateOutputError::Contract(format!(
                    "missing selected profile {}",
                    output.profile_id
                ))
            })?;
        let queries: Vec<_> = workload
            .queries
            .iter()
            .filter(|query| query.partition == output.partition)
            .collect();
        output.resources.insert(
            "10x".to_owned(),
            measure_partition_resources(&ten_x_published, profile, &queries)?,
        );
    }

    // Prove cancellation against the production code-index control path once.
    prove_cancellation(options.repo_root, &workload)?;

    Ok(GenerateCandidateOutputsResultV1 {
        workload_digest,
        outputs,
    })
}

/// Generate the same byte-stable query fallback plus evidence-bearing native
/// semantic/rerank results. Missing optional authorities remain pending.
pub fn generate_candidate_outputs_with_native(
    options: &GenerateCandidateOutputsOptions<'_>,
    authority: &dyn ProductionCandidateNativeExecutionAuthorityV1,
) -> Result<GenerateCandidateOutputsResultV1, CandidateOutputError> {
    let mut generated = generate_candidate_outputs(options)?;
    let workload_path = options.workload_path.map_or_else(
        || options.repo_root.join(WORKLOAD_RELATIVE),
        Path::to_path_buf,
    );
    let workload = load_candidate_workload(&workload_path)?;
    let published = publish_corpus(options.repo_root, &workload, options.admitted_scope)?;
    let ten_x_published =
        publish_corpus_with_scale(options.repo_root, &workload, 10, options.admitted_scope)?;
    if ten_x_published.eligible_chunks
        != published.eligible_chunks.checked_mul(10).ok_or_else(|| {
            CandidateOutputError::Contract("current eligible chunk count overflows 10x".to_owned())
        })?
    {
        return Err(CandidateOutputError::Contract(
            "native 10x generation does not contain exactly ten times the eligible chunks"
                .to_owned(),
        ));
    }
    let corpus_digest = compute_corpus_digest(options.repo_root, &workload)?;

    for output in &mut generated.outputs {
        let profile = workload
            .profile_matrix
            .iter()
            .find(|profile| profile.profile_id == output.profile_id)
            .ok_or_else(|| {
                CandidateOutputError::Contract(format!(
                    "native output references unknown profile {}",
                    output.profile_id
                ))
            })?;
        let queries = workload
            .queries
            .iter()
            .filter(|query| query.partition == output.partition)
            .collect::<Vec<_>>();
        let (rows, current) = measure_native_partition(
            &published,
            profile,
            &queries,
            authority,
            &generated.workload_digest,
            &corpus_digest,
            "current",
        )?;
        let (_, ten_x) = measure_native_partition(
            &ten_x_published,
            profile,
            &queries,
            authority,
            &generated.workload_digest,
            &corpus_digest,
            "10x",
        )?;
        let evidence = SemanticNativeResourceEvidenceV1 {
            samples: BTreeMap::from([("current".to_owned(), current), ("10x".to_owned(), ten_x)]),
        };
        evidence
            .validate()
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
        output.optional_stages = native_optional_stage_measurements(profile, &rows)?;
        apply_native_resource_evidence(output, &evidence)?;
        output.queries = rows;
    }
    Ok(generated)
}

fn measure_native_partition(
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    queries: &[&WorkloadQueryV1],
    authority: &dyn ProductionCandidateNativeExecutionAuthorityV1,
    workload_digest: &str,
    corpus_digest: &str,
    scale: &str,
) -> Result<
    (
        Vec<QueryCandidateRowV1>,
        SemanticNativeStageResultV1<SemanticNativeResourceSampleV1>,
    ),
    CandidateOutputError,
> {
    let mut rows = None;
    let generation = &published.generation;
    let mut execute_queries = || {
        let mut measured_rows = Vec::with_capacity(queries.len());
        let mut latency_samples_us = Vec::with_capacity(queries.len());
        for query in queries {
            let started = Instant::now();
            measured_rows.push(retrieve_one_native_query(
                published, profile, query, authority,
            )?);
            latency_samples_us.push(elapsed_micros(started));
        }
        rows = Some(measured_rows);
        Ok(latency_samples_us)
    };
    let evidence = authority.measure_resources(
        ProductionCandidateNativeResourceContextV1 {
            profile,
            queries,
            code: generation,
            incremental_code: &published.incremental_generation,
            incremental_before_content_digest: &published.incremental_before_content_digest,
            incremental_after_content_digest: &published.incremental_after_content_digest,
            code_generation: &generation.manifest().generation_id,
            workload_digest,
            corpus_digest,
            scale,
            eligible_chunks: published.eligible_chunks,
            semantic_projection_sources: ProductionCandidateSemanticProjectionSourcesV1 {
                one_symbol: &published.incremental_generation,
                deletion: &published.deletion_generation,
                no_op: &published.no_op_generation,
            },
        },
        &mut execute_queries,
    )?;
    let rows = rows.ok_or_else(|| {
        CandidateOutputError::Contract(
            "native resource authority did not execute the exact query workload".to_owned(),
        )
    })?;
    if let SemanticNativeStageResultV1::Complete(sample) = &evidence {
        let source_manifest_digest = &generation.projection().request().changes.manifest_digest;
        if sample.provenance.workload_digest != workload_digest
            || sample.provenance.corpus_digest != corpus_digest
            || sample.provenance.scale != scale
            || sample.provenance.code_generation_id != generation.manifest().generation_id.as_str()
            || sample.provenance.code_source_manifest_digest != source_manifest_digest.as_str()
            || sample.provenance.incremental_code_generation_id
                != published
                    .incremental_generation
                    .manifest()
                    .generation_id
                    .as_str()
            || sample.provenance.incremental_code_source_manifest_digest
                != published
                    .incremental_generation
                    .projection()
                    .request()
                    .changes
                    .manifest_digest
                    .as_str()
            || sample.provenance.incremental_before_content_digest
                != published.incremental_before_content_digest
            || sample.provenance.incremental_after_content_digest
                != published.incremental_after_content_digest
            || sample.eligible_chunks != published.eligible_chunks
            || sample.measured_queries != queries.len() as u64
        {
            return Err(CandidateOutputError::Contract(
                "native resource evidence is not bound to the exact evaluator workload".to_owned(),
            ));
        }
    }
    Ok((rows, evidence))
}

fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn retriever_outcome_candidate_count<E>(
    outcome: &RetrieverOutcome<tracedecay_domain::RetrieverBatch<E>>,
) -> u64 {
    match outcome {
        RetrieverOutcome::Complete(batch) | RetrieverOutcome::Partial { value: batch, .. } => {
            batch.candidates.len() as u64
        }
        RetrieverOutcome::Unavailable(_)
        | RetrieverOutcome::Denied
        | RetrieverOutcome::Stale(_)
        | RetrieverOutcome::BudgetExceeded(_)
        | RetrieverOutcome::Cancelled => 0,
    }
}

fn retrieve_one_native_query(
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    query: &WorkloadQueryV1,
    authority: &dyn ProductionCandidateNativeExecutionAuthorityV1,
) -> Result<QueryCandidateRowV1, CandidateOutputError> {
    let prepared = prepare_production_query(published, profile, query)?;
    let mut fusion = fusion_profile(profile, &retrieval_budget(), true)?;
    let mut native = None;
    let semantic_allowed_chunks = published
        .generation
        .chunks()
        .chunks()
        .iter()
        .filter(|chunk| {
            published
                .file_scopes
                .get(chunk.anchor.file_occurrence_id.as_str())
                .is_some_and(|scope| query.allowed_scopes.contains(scope))
        })
        .map(|chunk| chunk.id.clone())
        .collect::<BTreeSet<_>>();
    let mut evaluate = |inputs: ProductionCandidateNativeQueryInputsV1<'_>| {
        if native.is_some() {
            return Err(CandidateOutputError::Contract(format!(
                "native authority evaluated query {} more than once",
                query.query_id
            )));
        }
        fusion.rerank_policy_id = inputs
            .rerank
            .as_ref()
            .map(|rerank| rerank.policy.policy_id.clone());
        native = Some(
            evaluate_native_query(SemanticNativeQueryInputV1 {
                profile_spec: profile,
                fusion_profile: &fusion,
                diversity_policy: &prepared.diversity,
                kernel: &prepared.kernel,
                fallback_lanes: &prepared.fallback_lanes,
                query_measurements: prepared.query_measurements,
                semantic: inputs.semantic,
                fallback: &prepared.fallback,
                rerank: inputs.rerank,
            })
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        );
        Ok(())
    };
    authority.with_query_inputs(
        ProductionCandidateNativeQueryContextV1 {
            profile,
            query,
            request: &prepared.request,
            query_view: &prepared.query_view,
            code: &published.generation,
            code_generation: &prepared.code_generation,
            semantic_allowed_chunks: &semantic_allowed_chunks,
            rerank_policy: prepared.rerank_policy.as_ref(),
        },
        &mut evaluate,
    )?;
    let mut native = native.ok_or_else(|| {
        CandidateOutputError::Contract(format!(
            "native authority did not evaluate query {}",
            query.query_id
        ))
    })?;
    let ranked = match &native.rerank.on {
        SemanticNativeStageResultV1::Complete(ranked) => ranked.clone(),
        SemanticNativeStageResultV1::NotRequested | SemanticNativeStageResultV1::Pending { .. } => {
            native.rerank.off.clone()
        }
    };
    native.measurements.hydration = Some(measure_late_hydration(
        published,
        &prepared.request,
        &ranked,
        &retrieval_budget(),
    )?);
    validate_native_query_output(profile, &prepared.fallback, &native)?;
    let ranked = map_ranked_candidate_list(published, &ranked)?;
    let (historical, historical_ranked) = historical_candidates(published, query)?;
    let ranked = merge_candidate_timelines(query, ranked, historical_ranked);
    Ok(QueryCandidateRowV1 {
        query_id: query.query_id.clone(),
        abstained: ranked.is_empty(),
        ranked,
        historical,
        native: Some(native),
    })
}

fn validate_native_query_output(
    profile: &ProfileSpecV1,
    fallback: &QueryFallbackSubpayload,
    native: &SemanticNativeQueryOutputV1,
) -> Result<(), CandidateOutputError> {
    if native.profile_id != profile.profile_id
        || native.fallback_digest != fallback.digest.as_str()
        || !native.fallback_bytes_unchanged
    {
        return Err(CandidateOutputError::Contract(format!(
            "native query output does not preserve the exact query fallback for {}",
            profile.profile_id
        )));
    }
    let observed = native
        .ablations
        .iter()
        .map(|result| result.ablation)
        .collect::<BTreeSet<_>>();
    if observed.len() != native.ablations.len()
        || !observed.contains(&SemanticChannelAblationV1::ExactLexical)
        || !observed.contains(&SemanticChannelAblationV1::QueryExactLexicalGraph)
    {
        return Err(CandidateOutputError::Contract(
            "native query output is missing required query baseline ablations".to_owned(),
        ));
    }
    for ablation in &native.ablations {
        if ablation.measurement.output_candidates != ablation.ranked_candidates.len() as u64 {
            return Err(CandidateOutputError::Contract(
                "native fusion measurement does not match its ranked output".to_owned(),
            ));
        }
    }
    let hydration = native.measurements.hydration.ok_or_else(|| {
        CandidateOutputError::Contract(
            "native query output is missing genuine late-hydration measurements".to_owned(),
        )
    })?;
    if hydration.source_fetches != hydration.receipts
        || hydration.receipts > hydration.selected_candidates
        || (hydration.receipts != 0 && hydration.bytes_hydrated == 0)
    {
        return Err(CandidateOutputError::Contract(
            "native late-hydration measurements do not match source receipts".to_owned(),
        ));
    }
    let semantic_ablations = [
        SemanticChannelAblationV1::ExactLexicalSemantic,
        SemanticChannelAblationV1::HybridExactLexicalGraphSemantic,
    ];
    match (&native.exact_flat_oracle, &native.measurements.semantic) {
        (
            SemanticNativeStageResultV1::Complete(oracle),
            SemanticNativeStageResultV1::Complete(measurement),
        ) => {
            if measurement.output_candidates != oracle.hits.len() as u64 {
                return Err(CandidateOutputError::Contract(
                    "native semantic measurement does not match the exact-flat oracle".to_owned(),
                ));
            }
            if semantic_ablations
                .iter()
                .any(|ablation| !observed.contains(ablation))
            {
                return Err(CandidateOutputError::Contract(
                    "complete semantic output is missing required channel ablations".to_owned(),
                ));
            }
        }
        (SemanticNativeStageResultV1::NotRequested, SemanticNativeStageResultV1::NotRequested)
        | (
            SemanticNativeStageResultV1::Pending { .. },
            SemanticNativeStageResultV1::Pending { .. },
        ) => {
            if semantic_ablations
                .iter()
                .any(|ablation| observed.contains(ablation))
            {
                return Err(CandidateOutputError::Contract(
                    "semantic ablations cannot exist without a complete semantic run".to_owned(),
                ));
            }
        }
        _ => {
            return Err(CandidateOutputError::Contract(
                "native semantic result and measurement states disagree".to_owned(),
            ));
        }
    }
    Ok(())
}

fn native_optional_stage_measurements(
    profile: &ProfileSpecV1,
    rows: &[QueryCandidateRowV1],
) -> Result<OptionalStageMeasurementsV1, CandidateOutputError> {
    let native = rows
        .iter()
        .map(|row| {
            row.native.as_ref().ok_or_else(|| {
                CandidateOutputError::Contract(format!(
                    "native generation omitted query evidence {}",
                    row.query_id
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(OptionalStageMeasurementsV1 {
        semantic: aggregate_native_stage(
            profile.semantic_weight_ppm != 0,
            native.iter().map(|native| &native.exact_flat_oracle),
        )?,
        rerank: aggregate_native_rerank_stage(profile.rerank_weight_ppm != 0, &native)?,
    })
}

fn aggregate_native_stage<'a, T: 'a>(
    requested: bool,
    results: impl Iterator<Item = &'a SemanticNativeStageResultV1<T>>,
) -> Result<OptionalStageMeasurementV1, CandidateOutputError> {
    let results = results.collect::<Vec<_>>();
    if !requested {
        if results
            .iter()
            .any(|result| !matches!(result, SemanticNativeStageResultV1::NotRequested))
        {
            return Err(CandidateOutputError::Contract(
                "unrequested native stage reported execution".to_owned(),
            ));
        }
        return Ok(OptionalStageMeasurementV1::NotRequested);
    }
    if results
        .iter()
        .any(|result| matches!(result, SemanticNativeStageResultV1::NotRequested))
    {
        return Err(CandidateOutputError::Contract(
            "requested native stage reported not_requested".to_owned(),
        ));
    }
    Ok(
        if results
            .iter()
            .all(|result| matches!(result, SemanticNativeStageResultV1::Complete(_)))
        {
            OptionalStageMeasurementV1::Complete
        } else {
            OptionalStageMeasurementV1::Pending
        },
    )
}

fn aggregate_native_rerank_stage(
    requested: bool,
    native: &[&SemanticNativeQueryOutputV1],
) -> Result<OptionalStageMeasurementV1, CandidateOutputError> {
    for output in native {
        let states_agree = match (&output.rerank.on, &output.rerank.execution) {
            (
                SemanticNativeStageResultV1::NotRequested,
                SemanticNativeStageResultV1::NotRequested,
            )
            | (
                SemanticNativeStageResultV1::Complete(_),
                SemanticNativeStageResultV1::Complete(_),
            ) => true,
            (
                SemanticNativeStageResultV1::Pending { reason: left },
                SemanticNativeStageResultV1::Pending { reason: right },
            ) => left == right,
            _ => false,
        };
        if !states_agree {
            return Err(CandidateOutputError::Contract(
                "rerank output and resource execution states disagree".to_owned(),
            ));
        }
    }
    aggregate_native_stage(requested, native.iter().map(|native| &native.rerank.on))
}

fn apply_native_resource_evidence(
    output: &mut ProductionCandidateOutputV1,
    evidence: &SemanticNativeResourceEvidenceV1,
) -> Result<(), CandidateOutputError> {
    let expected_chunks = output
        .resources
        .iter()
        .map(|(scale, sample)| (scale.clone(), sample.eligible_chunks))
        .collect::<BTreeMap<_, _>>();
    let mut projected = BTreeMap::new();
    for (scale, stage) in &evidence.samples {
        let eligible_chunks = expected_chunks.get(scale).copied().ok_or_else(|| {
            CandidateOutputError::Contract(format!("unknown native resource scale {scale}"))
        })?;
        let sample = match stage {
            SemanticNativeStageResultV1::Complete(sample) => {
                let projected = sample.as_existing_evaluator_sample().ok_or_else(|| {
                    CandidateOutputError::Contract(format!(
                        "complete native resource sample {scale} is incomplete"
                    ))
                })?;
                if projected.eligible_chunks != eligible_chunks {
                    return Err(CandidateOutputError::Contract(format!(
                        "native resource sample {scale} has the wrong eligible chunk count"
                    )));
                }
                projected
            }
            SemanticNativeStageResultV1::Pending { reason } => ResourceSampleV1 {
                status: ResourceMeasurementStatusV1::Pending,
                eligible_chunks,
                peak_rss_bytes: None,
                latency_samples_us: Vec::new(),
                measured_queries: 0,
                pending_reason: Some(format!(
                    "native semantic resource measurement pending: {reason:?}"
                )),
            },
            SemanticNativeStageResultV1::NotRequested => {
                return Err(CandidateOutputError::Contract(format!(
                    "native resource sample {scale} cannot be not_requested"
                )));
            }
        };
        projected.insert(scale.clone(), sample);
    }
    output.resources = projected;
    output.native_resources = Some(evidence.clone());
    Ok(())
}

pub fn load_direct_evaluated_profile_material(
    repo_root: &Path,
    workload_path: Option<&Path>,
    profile_id: &str,
) -> Result<DirectEvaluatedProfileMaterialV1, CandidateOutputError> {
    let workload_path =
        workload_path.map_or_else(|| repo_root.join(WORKLOAD_RELATIVE), Path::to_path_buf);
    let workload = load_candidate_workload(&workload_path)?;
    validate_workload_for_tuning(&workload)?;
    direct_evaluated_profile_material(&workload, profile_id)
}

pub fn direct_evaluated_profile_material(
    workload: &CandidateWorkloadV1,
    profile_id: &str,
) -> Result<DirectEvaluatedProfileMaterialV1, CandidateOutputError> {
    validate_workload_for_tuning(workload)?;
    let profile = workload
        .profile_matrix
        .iter()
        .find(|profile| profile.profile_id == profile_id)
        .ok_or_else(|| {
            CandidateOutputError::Contract(format!("unknown requested profile_id {profile_id}"))
        })?;
    Ok(DirectEvaluatedProfileMaterialV1 {
        profile: fusion_profile(profile, &retrieval_budget(), true)?,
        diversity: evaluated_diversity_policy()?,
        rerank: evaluated_rerank_policy(profile)?,
    })
}

/// Direct production call for one query/profile — used by tests to prove the
/// generator emits identical candidate bytes.
pub fn retrieve_partition_query_bytes(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
    profile_id: &str,
    query_id: &str,
    admitted_scope: AdmittedCorpusScopeFn,
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
    let published = publish_corpus(repo_root, workload, admitted_scope)?;
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
    let current = completed_resource_sample(
        published.eligible_chunks,
        peak_after,
        latencies_us,
        rows.len() as u64,
    );

    let mut fallback_digests = Vec::with_capacity(queries.len());
    let mut query_digests = Vec::with_capacity(queries.len());
    for query in &queries {
        fallback_digests.push((
            query.query_id.as_str(),
            fallback_digest_for_query(published, profile, query)?,
        ));
        query_digests.push((
            query.query_id.as_str(),
            query_fallback_digest_for_query(published, profile, query)?,
        ));
    }
    let fallback_digest = canonical_sha256(&(
        "tracedecay.search-eval.partition-fallbacks.v1",
        &fallback_digests,
    ))?;
    let query_digest = canonical_sha256(&(
        "tracedecay.search-eval.partition-fallbacks.v1",
        &query_digests,
    ))?;
    let expected_query_fallback_digest = workload
        .expected_query_fallback_digests
        .get(partition)
        .cloned()
        .ok_or_else(|| {
            CandidateOutputError::Contract(format!(
                "missing expected query fallback digest for {partition}"
            ))
        })?;
    let query_fallback_matches_expected = query_digest == expected_query_fallback_digest;

    let mut resources = BTreeMap::new();
    resources.insert("current".to_owned(), current);

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
        profile_material_digest: compute_profile_material_digest(profile)?,
        fallback_digest,
        query_fallback_digest: query_digest,
        expected_query_fallback_digest,
        query_fallback_matches_expected,
        cancellation: REQUIRED_CANCELLATION.to_owned(),
        offline: REQUIRED_OFFLINE.to_owned(),
        optional_stages: optional_stage_measurements(profile),
        resources,
        native_resources: None,
        queries: rows,
    })
}

fn measure_partition_resources(
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    queries: &[&WorkloadQueryV1],
) -> Result<ResourceSampleV1, CandidateOutputError> {
    let peak_before = peak_rss_bytes();
    let mut latencies_us = Vec::with_capacity(queries.len());
    for query in queries {
        let started = Instant::now();
        retrieve_one_query(published, profile, query)?;
        latencies_us.push(started.elapsed().as_micros() as u64);
    }
    Ok(completed_resource_sample(
        published.eligible_chunks,
        peak_rss_bytes().max(peak_before),
        latencies_us,
        queries.len() as u64,
    ))
}

fn completed_resource_sample(
    eligible_chunks: u64,
    peak_rss_bytes: Option<u64>,
    latency_samples_us: Vec<u64>,
    measured_queries: u64,
) -> ResourceSampleV1 {
    let (status, pending_reason) = if peak_rss_bytes.is_some() {
        (ResourceMeasurementStatusV1::Measured, None)
    } else {
        (
            ResourceMeasurementStatusV1::Pending,
            Some("Linux peak RSS measurement is unavailable".to_owned()),
        )
    };
    ResourceSampleV1 {
        status,
        eligible_chunks,
        peak_rss_bytes,
        latency_samples_us,
        measured_queries,
        pending_reason,
    }
}

fn retrieve_one_query(
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    query: &WorkloadQueryV1,
) -> Result<QueryCandidateRowV1, CandidateOutputError> {
    let composed = compose_production_query(published, profile, query)?;
    let ranked = map_ranked_candidates(published, &composed)?;
    let (historical, historical_ranked) = historical_candidates(published, query)?;
    let ranked = merge_candidate_timelines(query, ranked, historical_ranked);
    let abstained = ranked.is_empty();
    Ok(QueryCandidateRowV1 {
        query_id: query.query_id.clone(),
        ranked,
        abstained,
        historical,
        native: None,
    })
}

pub(super) fn optional_stage_measurements(profile: &ProfileSpecV1) -> OptionalStageMeasurementsV1 {
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
    Ok(prepare_production_query(published, profile, query)?.query_output)
}

struct PreparedProductionQueryV1 {
    code_generation: CodeGenerationId,
    request: RetrievalRequest,
    query_view: EphemeralSanitizedQueryViewV1,
    kernel: CompositionKernel,
    fallback_lanes: Vec<CompositionLaneInput>,
    query_measurements: SemanticNativeQueryStageMeasurementsV1,
    query_output: CompositionOutputV1,
    fallback: QueryFallbackSubpayload,
    diversity: DiversityPolicy,
    rerank_policy: Option<RerankPolicy>,
}

fn prepare_production_query(
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    query: &WorkloadQueryV1,
) -> Result<PreparedProductionQueryV1, CandidateOutputError> {
    let generation_id = published.generation.manifest().generation_id.clone();
    let request = retrieval_request(&profile.profile_id, published)?;
    let query_view = EphemeralSanitizedQueryViewV1::sanitize(
        &query.query,
        id::<SanitizerRevision>(tracedecay_query::retrieval::QUERY_SANITIZER_REVISION_V1)?,
        id::<QueryNormalizationRevision>(
            tracedecay_query::retrieval::QUERY_NORMALIZATION_REVISION_V1,
        )?,
    )
    .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;

    let scope_key = canonical_scope_key(&query.allowed_scopes);
    let lexical_projection = published
        .lexical_projections
        .get(&scope_key)
        .cloned()
        .ok_or_else(|| {
            CandidateOutputError::Contract(format!(
                "missing lexical projection for query {}",
                query.query_id
            ))
        })?;
    let authority = CentralExactAdmissionAuthorityV1::new(id::<ExactAdmissionRuleRevision>(
        tracedecay_query::retrieval::QUERY_EXACT_RULE_REVISION_V1,
    )?);
    let exact_lane = ExactLane::new(
        authority.clone(),
        lexical_projection.exact_adapter(authority.clone()),
    );
    let lexical_lane = LexicalLane::new(lexical_projection);
    let graph_lane = GraphLane::new(
        published
            .graph_projections
            .get(&scope_key)
            .cloned()
            .ok_or_else(|| {
                CandidateOutputError::Contract(format!(
                    "missing graph projection for query {}",
                    query.query_id
                ))
            })?,
    );

    let budget = retrieval_budget();
    let exact_request = ExactLaneRequest {
        base: request.clone(),
        query_view: &query_view,
        generation: generation_id.clone(),
        literals: authority.parse_literals(&query_view, &request),
        budget,
    };
    let exact_started = Instant::now();
    let exact_outcome = exact_lane
        .retrieve_exact(&exact_request)
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let exact_measurement = SemanticNativeStageMeasurementV1 {
        elapsed_micros: elapsed_micros(exact_started),
        input_candidates: exact_request.literals.len() as u64,
        output_candidates: retriever_outcome_candidate_count(&exact_outcome),
    };

    let lexical_parts = lexical_query_parts(query_view.as_str())
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let lexical_request = LexicalLaneRequest {
        base: request.clone(),
        query_view: &query_view,
        generation: generation_id.clone(),
        whole_terms: lexical_parts.whole_terms,
        subtokens: lexical_parts.subtokens,
        phrases: lexical_parts.phrases,
        field_filters: Vec::new(),
        fuzzy_budget: 8,
        lexical_profile_revision: id(
            tracedecay_query::retrieval::QUERY_LEXICAL_PROFILE_REVISION_V1,
        )?,
        score_domain: id(tracedecay_query::retrieval::QUERY_LEXICAL_SCORE_DOMAIN_V1)?,
        budget,
    };
    let lexical_input_candidates = lexical_request
        .whole_terms
        .len()
        .saturating_add(lexical_request.subtokens.len())
        .saturating_add(lexical_request.phrases.len())
        .saturating_add(lexical_request.field_filters.len())
        as u64;
    let lexical_started = Instant::now();
    let lexical_outcome = lexical_lane
        .retrieve_lexical(&lexical_request)
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let lexical_measurement = SemanticNativeStageMeasurementV1 {
        elapsed_micros: elapsed_micros(lexical_started),
        input_candidates: lexical_input_candidates,
        output_candidates: retriever_outcome_candidate_count(&lexical_outcome),
    };

    let seed_anchors = graph_seeds_from_outcomes(&exact_outcome, &lexical_outcome);
    let graph_input_candidates = seed_anchors.len() as u64;
    let graph_started = Instant::now();
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
    let graph_measurement = SemanticNativeStageMeasurementV1 {
        elapsed_micros: elapsed_micros(graph_started),
        input_candidates: graph_input_candidates,
        output_candidates: retriever_outcome_candidate_count(&graph_outcome),
    };

    let kernel = CompositionKernel::new(id::<ComponentRevision>(
        tracedecay_query::retrieval::QUERY_RANKING_REVISION_V1,
    )?);
    let fallback_profile = query_fallback_profile(profile);
    let fusion_profile = fusion_profile(&fallback_profile, &budget, false)?;
    let fallback_lanes = vec![
        CompositionLaneInput::new(RetrieverKind::ExactLiteral, exact_outcome)
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        CompositionLaneInput::new(RetrieverKind::Lexical, lexical_outcome)
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        CompositionLaneInput::new(RetrieverKind::Graph, graph_outcome)
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
    ];
    let diversity = evaluated_diversity_policy()?;
    let query_output = kernel
        .compose(
            &FusionStageInput {
                profile: fusion_profile,
                lanes: fallback_lanes.clone(),
            },
            &diversity,
        )
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let fallback = query_fallback_from_composition(&query_output)?;
    let rerank_policy = evaluated_rerank_policy(profile)?;
    Ok(PreparedProductionQueryV1 {
        code_generation: generation_id,
        request,
        query_view,
        kernel,
        fallback_lanes,
        query_measurements: SemanticNativeQueryStageMeasurementsV1 {
            exact: exact_measurement,
            lexical: lexical_measurement,
            graph: graph_measurement,
        },
        query_output,
        fallback,
        diversity,
        rerank_policy,
    })
}

fn query_fallback_digest_for_query(
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    query: &WorkloadQueryV1,
) -> Result<String, CandidateOutputError> {
    // query-only profile compose for digest stability measurement.
    let query_profile = query_fallback_profile(profile);
    fallback_digest_for_query(published, &query_profile, query)
}

fn query_fallback_profile(profile: &ProfileSpecV1) -> ProfileSpecV1 {
    let mut fallback = profile.clone();
    "query-fallback".clone_into(&mut fallback.profile_id);
    fallback.semantic_weight_ppm = 0;
    fallback.rerank_weight_ppm = 0;
    fallback
}

fn fallback_digest_for_query(
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    query: &WorkloadQueryV1,
) -> Result<String, CandidateOutputError> {
    let composed = compose_production_query(published, profile, query)?;
    let fallback = query_fallback_from_composition(&composed)?;
    Ok(fallback.digest.as_str().to_owned())
}

fn query_fallback_from_composition(
    output: &CompositionOutputV1,
) -> Result<QueryFallbackSubpayload, CandidateOutputError> {
    let mut coverage = BTreeMap::new();
    for lane in RetrieverKind::QUERY_FALLBACK_LANES {
        coverage.insert(
            lane,
            output
                .public_lane_statuses
                .get(&lane)
                .copied()
                .unwrap_or(PublicRetrieverStatus::Unavailable),
        );
    }
    let fallback = QueryFallbackSubpayload::new(
        output.profile_id.clone(),
        output.ranked_candidates.clone(),
        coverage,
        output.freshness.clone(),
        None,
    )
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
    map_ranked_candidate_list(published, &output.ranked_candidates)
}

struct CandidateCorpusHydrationSourceV1<'a> {
    published: &'a PublishedCorpus,
    source_fetches: u64,
}

impl CandidateCorpusHydrationSourceV1<'_> {
    fn binding<'a>(
        &'a self,
        candidate: &'a tracedecay_domain::RankedCandidate,
    ) -> Option<(
        &'a OccurrenceMapEntry,
        &'a tracedecay_domain::OccurrenceProvenance,
    )> {
        candidate
            .candidate
            .occurrences
            .iter()
            .find_map(|occurrence| {
                self.published
                    .occurrence_map
                    .get(occurrence.source_occurrence_id.as_str())
                    .map(|entry| (entry, occurrence))
            })
            .or_else(|| {
                self.published
                    .occurrence_map
                    .get(candidate.candidate.anchor_id.as_str())
                    .zip(candidate.candidate.occurrences.first())
            })
    }
}

impl LateHydrationSource<Vec<u8>> for CandidateCorpusHydrationSourceV1<'_> {
    fn authorize(
        &mut self,
        _request: &RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
    ) -> HydrationAuthorizationV1 {
        if self.binding(candidate).is_some() {
            HydrationAuthorizationV1::Authorized
        } else {
            HydrationAuthorizationV1::Unavailable(HydrationUnavailableV1::Invalid)
        }
    }

    fn preflight_authorized(
        &mut self,
        _request: &RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
        permit: &HydrationWorkPermitV1,
    ) -> HydrationPreflightOutcomeV1 {
        let Some((entry, occurrence)) = self.binding(candidate) else {
            return HydrationPreflightOutcomeV1::Unavailable(HydrationUnavailableV1::Invalid);
        };
        if !permit
            .source_occurrence_ids
            .contains(&occurrence.source_occurrence_id)
        {
            return HydrationPreflightOutcomeV1::Unavailable(HydrationUnavailableV1::Invalid);
        }
        match fs::metadata(self.published.repo_root.join(&entry.fixture_path)) {
            Ok(metadata) if metadata.len() <= permit.remaining_bytes => {
                HydrationPreflightOutcomeV1::Ready {
                    estimated_bytes: metadata.len(),
                }
            }
            Ok(_) => HydrationPreflightOutcomeV1::BudgetExceeded,
            Err(_) => HydrationPreflightOutcomeV1::Unavailable(HydrationUnavailableV1::Internal),
        }
    }

    fn hydrate_authorized(
        &mut self,
        _request: &RetrievalRequest,
        candidate: &tracedecay_domain::RankedCandidate,
        _permit: &HydrationWorkPermitV1,
    ) -> HydrationReadOutcomeV1<Vec<u8>> {
        let Some((entry, occurrence)) = self.binding(candidate) else {
            return HydrationReadOutcomeV1::Unavailable(HydrationUnavailableV1::Invalid);
        };
        let anchor_id = candidate.candidate.anchor_id.clone();
        let source_occurrence_id = occurrence.source_occurrence_id.clone();
        let freshness = occurrence.freshness.clone();
        let path = self.published.repo_root.join(&entry.fixture_path);
        let hydration_revision = match HydrationRevision::new("hydration.search-eval.corpus.v1") {
            Ok(revision) => revision,
            Err(_) => {
                return HydrationReadOutcomeV1::Unavailable(HydrationUnavailableV1::Internal);
            }
        };
        match fs::read(path) {
            Ok(payload) => {
                self.source_fetches = self.source_fetches.saturating_add(1);
                HydrationReadOutcomeV1::Complete {
                    receipt: HydrationReceipt {
                        anchor_id,
                        source_occurrence_id,
                        hydration_revision,
                        bytes_hydrated: payload.len() as u64,
                        authorized: true,
                        freshness,
                    },
                    payload,
                }
            }
            Err(_) => HydrationReadOutcomeV1::Unavailable(HydrationUnavailableV1::Internal),
        }
    }
}

fn measure_late_hydration(
    published: &PublishedCorpus,
    request: &RetrievalRequest,
    ranked: &[tracedecay_domain::RankedCandidate],
    budget: &RetrievalBudget,
) -> Result<SemanticNativeHydrationMeasurementV1, CandidateOutputError> {
    let mut source = CandidateCorpusHydrationSourceV1 {
        published,
        source_fetches: 0,
    };
    let started = Instant::now();
    let page = CanonicalLateHydration::new(&mut source)
        .hydrate(request, ranked, budget)
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let bytes_hydrated = page
        .receipts
        .iter()
        .map(|receipt| receipt.bytes_hydrated)
        .sum();
    Ok(SemanticNativeHydrationMeasurementV1 {
        elapsed_micros: elapsed_micros(started),
        selected_candidates: page.results.len() as u64,
        source_fetches: source.source_fetches,
        receipts: page.receipts.len() as u64,
        bytes_hydrated,
    })
}

fn map_ranked_candidate_list(
    published: &PublishedCorpus,
    ranked_candidates: &[tracedecay_domain::RankedCandidate],
) -> Result<Vec<RankedCandidateRowV1>, CandidateOutputError> {
    let mut rows = Vec::new();
    for ranked in ranked_candidates {
        let entry = published
            .occurrence_map
            .get(ranked.candidate.anchor_id.as_str())
            .or_else(|| {
                ranked.candidate.occurrences.iter().find_map(|occurrence| {
                    published
                        .occurrence_map
                        .get(occurrence.source_occurrence_id.as_str())
                })
            })
            .cloned()
            .ok_or_else(|| {
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
        let anchor = ranked.candidate.anchor_id.as_str().to_owned();
        let mut anchors = entry.display_anchors;
        if !anchors.contains(&anchor) {
            anchors.insert(0, anchor.clone());
        }
        rows.push(RankedCandidateRowV1 {
            anchor,
            anchors,
            scope: entry.scope,
            document_id: entry.document_id,
            tier: tier.to_owned(),
        });
    }
    Ok(rows)
}

/// Mount the historical code-index join on one already-admitted checkout.
///
/// The scope arrives from the composing binary's identity authority; this
/// function only refuses provider drift and projects adapter errors onto the
/// typed unavailable reasons.
fn read_historical_evidence(
    repo_root: &Path,
    scope: &ResolvedScope,
    authorization: Option<&HistoricalSourceAuthorizationV1>,
    request: &HistoricalQueryRequestV1,
) -> HistoricalGitReadOutcomeV1 {
    let reader = NativeHistoricalBlobReaderV1::new(
        repo_root,
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
    );
    match HistoricalGitQueryAdapter::new(&reader, scope.clone()).query(authorization, request) {
        Ok(result) => HistoricalGitReadOutcomeV1::Complete {
            scope: scope.clone(),
            result,
        },
        Err(error) => HistoricalGitReadOutcomeV1::Unavailable {
            reason: HistoricalGitReadUnavailableReasonV1::from_query_error(&error),
        },
    }
}

fn historical_candidates(
    published: &PublishedCorpus,
    query: &WorkloadQueryV1,
) -> Result<(HistoricalQueryExecutionV1, Vec<RankedCandidateRowV1>), CandidateOutputError> {
    if !query.strata.iter().any(|stratum| {
        matches!(
            stratum.as_str(),
            "incremental_edit"
                | "incremental_delete"
                | "incremental_rename"
                | "renamed_moved_symbol"
        )
    }) {
        return Ok((HistoricalQueryExecutionV1::NotRequested, Vec::new()));
    }

    let scope = (published.admitted_scope)(&published.repo_root).ok_or_else(|| {
        CandidateOutputError::Contract(
            "historical evaluator requires the authoritative repository identity marker".to_owned(),
        )
    })?;
    let source_commit = query
        .historical_commit
        .as_deref()
        .map(GitOidV1::new)
        .transpose()
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?
        .unwrap_or_else(|| published.source_commit.clone());

    let paths: Vec<String> = published
        .corpus
        .iter()
        .filter(|document| query.allowed_scopes.contains(&document.scope))
        .map(|document| document.source_path.clone())
        .collect();
    let authorization = if paths.is_empty() {
        None
    } else {
        Some(
            HistoricalSourceAuthorizationV1::new(
                scope.clone(),
                [source_commit.clone()],
                paths.clone(),
            )
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        )
    };
    let terms = lexical_query_parts(&query.query)
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?
        .whole_terms;
    let request = HistoricalQueryRequestV1 {
        commits: vec![source_commit],
        paths,
        terms,
        rename_mode: HistoricalRenameModeV1::FollowExactObjectRenames,
        max_results: 32,
        max_blob_bytes: 8 * 1024 * 1024,
        max_total_bytes: 32 * 1024 * 1024,
    };
    match read_historical_evidence(
        &published.repo_root,
        &scope,
        authorization.as_ref(),
        &request,
    ) {
        HistoricalGitReadOutcomeV1::Unavailable { reason } => {
            Ok((HistoricalQueryExecutionV1::Unavailable(reason), Vec::new()))
        }
        HistoricalGitReadOutcomeV1::Complete {
            scope: returned_scope,
            result,
        } => {
            if returned_scope != scope || result.scope != scope {
                return Err(CandidateOutputError::Contract(
                    "historical evaluator received cross-scope evidence".to_owned(),
                ));
            }
            let mut rows = Vec::new();
            for evidence in result.evidence {
                let document = published
                    .corpus
                    .iter()
                    .find(|document| document.source_path == evidence.path)
                    .ok_or_else(|| {
                        CandidateOutputError::Contract(format!(
                            "historical evidence path {} is outside the corpus",
                            evidence.path
                        ))
                    })?;
                if !query.allowed_scopes.contains(&document.scope) {
                    return Err(CandidateOutputError::Contract(format!(
                        "historical evidence path {} escaped allowed scopes",
                        evidence.path
                    )));
                }
                let anchors: Vec<_> = evidence
                    .anchors
                    .iter()
                    .flat_map(|anchor| {
                        [
                            format!(
                                "git:{}:{}::{}",
                                evidence.commit.as_str(),
                                evidence.path,
                                anchor.term
                            ),
                            format!("{}::{}", evidence.path, anchor.term),
                        ]
                    })
                    .collect();
                let Some(anchor) = anchors.first().cloned() else {
                    continue;
                };
                rows.push(RankedCandidateRowV1 {
                    anchor,
                    anchors,
                    scope: document.scope.clone(),
                    document_id: document.document_id.clone(),
                    tier: "historical_exact".to_owned(),
                });
            }
            let mut seen = BTreeSet::new();
            rows.retain(|row| seen.insert(row.anchor.clone()));
            Ok((HistoricalQueryExecutionV1::Complete, rows))
        }
    }
}

fn merge_candidate_timelines(
    query: &WorkloadQueryV1,
    mut current: Vec<RankedCandidateRowV1>,
    mut historical: Vec<RankedCandidateRowV1>,
) -> Vec<RankedCandidateRowV1> {
    if query.historical_commit.is_some() {
        historical.append(&mut current);
        historical
    } else {
        current.append(&mut historical);
        current
    }
}

fn publish_corpus(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
    admitted_scope: AdmittedCorpusScopeFn,
) -> Result<PublishedCorpus, CandidateOutputError> {
    publish_corpus_with_scale(repo_root, workload, 1, admitted_scope)
}

fn publish_corpus_with_scale(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
    copies: usize,
    admitted_scope: AdmittedCorpusScopeFn,
) -> Result<PublishedCorpus, CandidateOutputError> {
    if copies == 0 {
        return Err(CandidateOutputError::Contract(
            "corpus scale must be positive".to_owned(),
        ));
    }
    let corpus_digest = compute_corpus_digest(repo_root, workload)?;
    let language_registry = StaticLanguageRegistry::new();
    let mut files = Vec::new();
    let mut captured = Vec::new();
    let mut file_to_document = BTreeMap::new();
    let mut file_scopes = BTreeMap::new();
    for copy in 0..copies {
        for document in &workload.corpus {
            let absolute = repo_root.join(&document.path);
            let bytes = fs::read(&absolute).map_err(|source| CandidateOutputError::Read {
                path: absolute.clone(),
                source,
            })?;
            let copy_suffix = if copy == 0 {
                String::new()
            } else {
                format!(".resource-copy-{copy}")
            };
            let file_occurrence_id =
                id::<FileOccurrenceId>(&format!("file.{}{}", document.document_id, copy_suffix))?;
            file_to_document.insert(file_occurrence_id.as_str().to_owned(), document.clone());
            file_scopes.insert(
                file_occurrence_id.as_str().to_owned(),
                document.scope.clone(),
            );
            let language = id::<LanguageId>(&document.language)?;
            let indexable = language_registry.descriptor(&language).is_some();
            files.push(SanitizedCodeFileV1 {
                file_occurrence_id: file_occurrence_id.clone(),
                logical_path: format!("{}{}", document.source_path, copy_suffix),
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
                    sensitivity_level: tracedecay_domain::SensitivityLevelV1::Public,
                });
            }
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
        content_identity: id(&canonical_sha256(&(
            "tracedecay.search-eval.scaled-corpus.v1",
            &corpus_digest,
            copies,
        ))?)?,
        captured_at: UtcMicros(1_000_000),
        files,
    };
    let incremental_snapshot_base = snapshot.clone();
    let incremental_captured_base = captured.clone();
    let target_projection_key = ProjectionKeyV1 {
        kind: ProjectionKindV1::Lexical,
        schema_revision: "lexical.candidate.v1".to_owned(),
        profile_digest: lexical_projection_profile_digest()?,
    };
    let request = CodeIndexBuildRequestV1 {
        snapshot,
        captured_files: captured,
        changed_files: BTreeSet::new(),
        invalidations: BTreeSet::new(),
        sealed_at: UtcMicros(1_100_000),
        target_projection_key: target_projection_key.clone(),
    };
    let config = CodeIndexProductionConfigV1 {
        project_id: id::<ProjectId>("project.candidate.fixture")?,
        repository: id::<RepositoryId>("repository.candidate.fixture")?,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.candidate.v1")?,
        policy_revision: id::<PolicyRevisionId>("policy.candidate.v1")?,
        chunker_revision: id::<ChunkerRevision>("chunker.candidate.v1")?,
        privacy_domain: id::<PrivacyDomainId>("privacy.candidate.fixture")?,
        privacy_key_epoch: 1,
        max_snapshot_age_micros: None,
    };
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config, store.clone(), ApplyingProjectionSink)
        .map_err(|error| {
            CandidateOutputError::Contract(format!("open production owner: {error}"))
        })?;
    let generation = owner
        .build_and_publish(request, &ActiveControl)
        .map_err(|error| CandidateOutputError::Contract(format!("publish generation: {error}")))?;
    let expected_chunks = match copies {
        1 => workload.execution_contract.exact_eligible_chunks_current,
        10 => workload.execution_contract.exact_eligible_chunks_10x,
        _ => {
            return Err(CandidateOutputError::Contract(
                "evaluation corpus scale must be current or exact 10x".to_owned(),
            ));
        }
    };
    let observed_chunks = generation.chunks().chunks().len() as u64;
    if observed_chunks != expected_chunks {
        return Err(CandidateOutputError::Contract(format!(
            "eligible chunk count mismatch for {copies}x corpus: declared {expected_chunks}, observed {observed_chunks}"
        )));
    }
    let mut incremental_snapshot = incremental_snapshot_base;
    incremental_snapshot.captured_at = UtcMicros(1_150_000);
    let mut incremental_captured = incremental_captured_base;
    let incremental_document = workload
        .corpus
        .iter()
        .find(|document| document.document_id == workload.incremental_fixture.document_id)
        .ok_or_else(|| {
            CandidateOutputError::Contract(
                "incremental fixture names an unknown corpus document".to_owned(),
            )
        })?;
    let changed_file =
        id::<FileOccurrenceId>(&format!("file.{}", incremental_document.document_id))?;
    let canonical_root =
        fs::canonicalize(repo_root).map_err(|source| CandidateOutputError::Read {
            path: repo_root.to_path_buf(),
            source,
        })?;
    let expected_after_path = canonical_root.join(&workload.incremental_fixture.after_path);
    let after_path =
        fs::canonicalize(&expected_after_path).map_err(|source| CandidateOutputError::Read {
            path: expected_after_path.clone(),
            source,
        })?;
    if after_path != expected_after_path {
        return Err(CandidateOutputError::Contract(
            "incremental fixture must be a checked-in non-symlink path".to_owned(),
        ));
    }
    let after_bytes = fs::read(&after_path).map_err(|source| CandidateOutputError::Read {
        path: after_path,
        source,
    })?;
    let observed_after_sha256 = hex::encode(Sha256::digest(&after_bytes));
    if observed_after_sha256 != workload.incremental_fixture.after_sha256 {
        return Err(CandidateOutputError::Contract(
            "incremental fixture bytes do not match the workload digest".to_owned(),
        ));
    }
    let changed = incremental_captured
        .iter_mut()
        .find(|file| file.file_occurrence_id == changed_file)
        .ok_or_else(|| {
            CandidateOutputError::Contract(
                "incremental fixture corpus document is not eligible".to_owned(),
            )
        })?;
    if changed.sanitized_bytes == after_bytes {
        return Err(CandidateOutputError::Contract(
            "incremental before/after fixture bytes are identical".to_owned(),
        ));
    }
    changed.sanitized_bytes = after_bytes;
    let changed_digest = content_digest(&changed.sanitized_bytes);
    let snapshot_file = incremental_snapshot
        .files
        .iter_mut()
        .find(|file| file.file_occurrence_id == changed_file)
        .ok_or_else(|| {
            CandidateOutputError::Contract(
                "incremental resource file is absent from the sanitized snapshot".to_owned(),
            )
        })?;
    let incremental_before_content_digest = snapshot_file.content_digest.as_str().to_owned();
    snapshot_file.content_digest = changed_digest.clone();
    let incremental_after_content_digest = changed_digest.as_str().to_owned();
    incremental_snapshot.content_identity = id(&canonical_sha256(&(
        "tracedecay.search-eval.incremental-corpus.v1",
        generation.manifest().generation_id.clone(),
        changed_file.clone(),
        changed_digest,
    ))?)?;
    let scenario_snapshot = incremental_snapshot.clone();
    let scenario_captured = incremental_captured.clone();
    let incremental_generation = owner
        .build_and_publish(
            CodeIndexBuildRequestV1 {
                snapshot: incremental_snapshot,
                captured_files: incremental_captured,
                changed_files: BTreeSet::from([incremental_document.source_path.clone()]),
                invalidations: BTreeSet::new(),
                sealed_at: UtcMicros(1_200_000),
                target_projection_key: target_projection_key.clone(),
            },
            &ActiveControl,
        )
        .map_err(|error| {
            CandidateOutputError::Contract(format!(
                "publish incremental resource generation: {error}"
            ))
        })?;
    let incremental_changes = &incremental_generation.projection().request().changes;
    let changed_chunks = incremental_changes
        .added_or_changed
        .iter()
        .filter_map(|change| {
            incremental_generation
                .chunks()
                .chunks()
                .iter()
                .find(|chunk| chunk.id == change.chunk_id)
        })
        .collect::<Vec<_>>();
    let deleted_chunks = incremental_changes
        .deleted
        .iter()
        .filter_map(|change| {
            generation
                .chunks()
                .chunks()
                .iter()
                .find(|chunk| chunk.id == change.chunk_id)
        })
        .collect::<Vec<_>>();
    let affected_count = incremental_changes
        .added_or_changed
        .len()
        .saturating_add(incremental_changes.deleted.len());
    if incremental_generation.manifest().parent_generation
        != Some(generation.manifest().generation_id.clone())
        || affected_count == 0
        || affected_count >= incremental_generation.chunks().chunks().len()
        || changed_chunks.len() != incremental_changes.added_or_changed.len()
        || deleted_chunks.len() != incremental_changes.deleted.len()
        || changed_chunks
            .iter()
            .chain(deleted_chunks.iter())
            .any(|chunk| chunk.anchor.file_occurrence_id != changed_file)
        || !changed_chunks
            .iter()
            .any(|chunk| chunk.anchor.symbol_occurrence_id.is_some())
    {
        return Err(CandidateOutputError::Contract(
            "resource corpus did not produce a genuine changed-chunk incremental generation"
                .to_owned(),
        ));
    }

    let no_op_generation = build_projection_source_generation(
        &mut owner,
        scenario_snapshot.clone(),
        scenario_captured.clone(),
        BTreeSet::new(),
        UtcMicros(1_250_000),
        target_projection_key.clone(),
        "no-op",
    )?;
    if !no_op_generation
        .projection()
        .request()
        .changes
        .added_or_changed
        .is_empty()
        || !no_op_generation
            .projection()
            .request()
            .changes
            .deleted
            .is_empty()
    {
        return Err(CandidateOutputError::Contract(
            "no-op projection case performed projection work".to_owned(),
        ));
    }

    let mut deletion_snapshot = scenario_snapshot.clone();
    deletion_snapshot.captured_at = UtcMicros(1_300_000);
    deletion_snapshot
        .files
        .retain(|file| file.file_occurrence_id != changed_file);
    deletion_snapshot.content_identity = id(&canonical_sha256(&(
        "tracedecay.search-eval.deletion-case.v1",
        &deletion_snapshot.files,
    ))?)?;
    let mut deletion_captured = scenario_captured.clone();
    deletion_captured.retain(|file| file.file_occurrence_id != changed_file);
    let deletion_generation = build_projection_source_generation(
        &mut owner,
        deletion_snapshot,
        deletion_captured,
        BTreeSet::from([incremental_document.source_path.clone()]),
        UtcMicros(1_350_000),
        target_projection_key,
        "deletion",
    )?;

    let qualified_names: BTreeMap<_, _> = generation
        .symbols()
        .symbols
        .iter()
        .map(|symbol| (symbol.occurrence.clone(), symbol.qualified_name.clone()))
        .collect();
    let mut occurrence_map = BTreeMap::new();
    for chunk in generation.chunks().chunks() {
        let Some(document) = file_to_document.get(chunk.anchor.file_occurrence_id.as_str()) else {
            continue;
        };
        let qualified_name = chunk
            .anchor
            .symbol_occurrence_id
            .as_ref()
            .and_then(|symbol| qualified_names.get(symbol));
        let display_anchors = display_anchors_for_chunk(chunk, document, qualified_name);
        if let Some(symbol) = &chunk.anchor.symbol_occurrence_id {
            occurrence_map.insert(
                format!("code-symbol:{}", symbol.as_str()),
                OccurrenceMapEntry {
                    document_id: document.document_id.clone(),
                    scope: document.scope.clone(),
                    fixture_path: document.path.clone(),
                    display_anchors: display_anchors.clone(),
                },
            );
            occurrence_map.insert(
                format!("code-graph:{}", symbol.as_str()),
                OccurrenceMapEntry {
                    document_id: document.document_id.clone(),
                    scope: document.scope.clone(),
                    fixture_path: document.path.clone(),
                    display_anchors: display_anchors.clone(),
                },
            );
        }
        occurrence_map.insert(
            format!("code-chunk:{}", chunk.id.as_str()),
            OccurrenceMapEntry {
                document_id: document.document_id.clone(),
                scope: document.scope.clone(),
                fixture_path: document.path.clone(),
                display_anchors,
            },
        );
    }

    let eligible_chunks = generation
        .admitted_chunks()
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?
        .len() as u64;
    let (lexical_projections, graph_projections) =
        build_query_projections(&generation, &file_scopes, &workload.queries)?;
    Ok(PublishedCorpus {
        generation,
        lexical_projections,
        graph_projections,
        incremental_generation,
        incremental_before_content_digest,
        incremental_after_content_digest,
        occurrence_map,
        file_scopes,
        repo_root: repo_root.to_path_buf(),
        source_commit: GitOidV1::new(workload.source_repository_commit.clone())
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        corpus: workload.corpus.clone(),
        corpus_digest,
        eligible_chunks,
        no_op_generation,
        deletion_generation,
        admitted_scope,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_projection_source_generation(
    owner: &mut CodeIndexProductionOwnerV1<SharedPublicationStore, ApplyingProjectionSink>,
    mut snapshot: SanitizedCodeSnapshotV1,
    captured_files: Vec<CodeIndexCapturedFileV1>,
    changed_files: BTreeSet<String>,
    sealed_at: UtcMicros,
    target_projection_key: ProjectionKeyV1,
    label: &str,
) -> Result<CodeIndexPublishedGenerationV1, CandidateOutputError> {
    snapshot.captured_at = UtcMicros(sealed_at.0.saturating_sub(10_000));
    owner
        .build_and_publish(
            CodeIndexBuildRequestV1 {
                snapshot,
                captured_files,
                changed_files,
                invalidations: BTreeSet::new(),
                sealed_at,
                target_projection_key,
            },
            &ActiveControl,
        )
        .map_err(|error| {
            CandidateOutputError::Contract(format!(
                "publish {label} semantic projection source: {error}"
            ))
        })
}

fn display_anchors_for_chunk(
    chunk: &CodeSearchChunkV1,
    document: &CorpusDocumentV1,
    qualified_name: Option<&String>,
) -> Vec<String> {
    let mut anchors = BTreeSet::from([document.document_id.clone()]);
    if let Some(qualified_name) = qualified_name {
        anchors.insert(qualified_name.clone());
        anchors.insert(display_qualified_anchor(document, qualified_name));
    }
    for term in &chunk.exact_terms {
        let term = String::from_utf8_lossy(term.canonical_bytes());
        if !term.is_empty() {
            anchors.insert(format!("{}::{term}", document.document_id));
        }
    }
    let text = chunk.sanitized_text.as_str();
    let first = text.lines().next().unwrap_or(text).trim();
    if !first.is_empty() {
        anchors.insert(format!("{}::{first}", document.document_id));
    }
    let primary = chunk
        .exact_terms
        .iter()
        .find(|term| {
            matches!(
                term.kind(),
                tracedecay_domain::ExactTechnicalTermKindV1::WholeSymbol
            )
        })
        .map(|term| {
            format!(
                "{}::{}",
                document.document_id,
                String::from_utf8_lossy(term.canonical_bytes())
            )
        })
        .or_else(|| {
            qualified_name.map(|qualified_name| display_qualified_anchor(document, qualified_name))
        })
        .unwrap_or_else(|| {
            format!(
                "{}:{}-{}",
                document.document_id,
                chunk.anchor.source_span.start_byte,
                chunk.anchor.source_span.end_byte
            )
        });
    anchors.insert(primary.clone());
    let mut ordered = vec![primary.clone()];
    ordered.extend(anchors.into_iter().filter(|anchor| anchor != &primary));
    ordered
}

fn display_qualified_anchor(document: &CorpusDocumentV1, qualified_name: &str) -> String {
    let segments: Vec<_> = document.source_path.split('/').collect();
    let module_prefix = segments
        .iter()
        .position(|segment| *segment == "src")
        .map(|src| {
            let mut modules = vec!["crate"];
            modules.extend(segments[src + 1..].iter().copied());
            if let Some(file) = modules.last_mut() {
                *file = file.strip_suffix(".rs").unwrap_or(file);
            }
            if modules
                .last()
                .is_some_and(|module| matches!(*module, "lib" | "main" | "mod"))
            {
                modules.pop();
            }
            modules.join("::")
        });
    let local_name = qualified_name
        .strip_prefix(&document.source_path)
        .and_then(|suffix| suffix.strip_prefix("::"))
        .or_else(|| {
            module_prefix
                .as_deref()
                .and_then(|prefix| qualified_name.strip_prefix(prefix))
                .and_then(|suffix| suffix.strip_prefix("::"))
        })
        .unwrap_or(qualified_name);
    if local_name.is_empty() {
        document.document_id.clone()
    } else {
        format!("{}::{local_name}", document.document_id)
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
        sensitivity_level: tracedecay_domain::SensitivityLevelV1::Public,
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
    let generation_scope = CodeIndexGenerationScopeV1::for_snapshot(&request.snapshot);
    let config = CodeIndexProductionConfigV1 {
        project_id: id::<ProjectId>("project.candidate.cancel")?,
        repository: id::<RepositoryId>("repository.candidate.cancel")?,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.candidate.v1")?,
        policy_revision: id::<PolicyRevisionId>("policy.candidate.v1")?,
        chunker_revision: id::<ChunkerRevision>("chunker.candidate.v1")?,
        privacy_domain: id::<PrivacyDomainId>("privacy.candidate.cancel")?,
        privacy_key_epoch: 1,
        max_snapshot_age_micros: None,
    };
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config, store.clone(), ApplyingProjectionSink)
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
        .load_active(&generation_scope)
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
        tracedecay_domain::RetrieverBatch<tracedecay_query::retrieval::exact::ExactLaneEvidence>,
    >,
    lexical: &RetrieverOutcome<
        tracedecay_domain::RetrieverBatch<
            tracedecay_query::retrieval::lexical::LexicalLaneEvidence,
        >,
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
        (
            RetrieverKind::ExactLiteral,
            tracedecay_query::retrieval::QUERY_EXACT_SCORE_DOMAIN_V1,
        ),
        (
            RetrieverKind::Lexical,
            tracedecay_query::retrieval::QUERY_LEXICAL_SCORE_DOMAIN_V1,
        ),
        (
            RetrieverKind::Graph,
            tracedecay_query::retrieval::QUERY_GRAPH_SCORE_DOMAIN_V1,
        ),
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
        rerank_policy_id: profile
            .rerank_policy
            .as_ref()
            .map(|policy| id(&policy.policy_id))
            .transpose()?,
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

fn evaluated_diversity_policy() -> Result<DiversityPolicy, CandidateOutputError> {
    Ok(DiversityPolicy {
        policy_id: id("diversity.candidate.v1")?,
        evaluation_result_anchor: Some(id("evaluation.candidate.v1")?),
        per_source_namespace: None,
        per_source_instance: None,
        per_repository: None,
        per_file: Some(2),
        per_session_or_thread: None,
        per_copy_cluster: None,
        per_evidence_role: None,
    })
}

fn evaluated_rerank_policy(
    profile: &ProfileSpecV1,
) -> Result<Option<RerankPolicy>, CandidateOutputError> {
    let evaluation_result_anchor =
        id::<RetrievalAnchorId>(&format!("evaluation.{}", profile.profile_id))?;
    profile
        .rerank_policy
        .as_ref()
        .map(|policy| {
            Ok(RerankPolicy {
                policy_id: id(&policy.policy_id)?,
                evaluation_result_anchor,
                max_candidates: policy.max_candidates,
                max_input_bytes: policy.max_input_bytes,
                max_input_tokens: policy.max_input_tokens,
                max_work_units: policy.max_work_units,
                max_model_invocations: policy.max_model_invocations,
                deadline_micros: policy.deadline_micros,
            })
        })
        .transpose()
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
    peak_rss_bytes_from_status(&status)
}

fn peak_rss_bytes_from_status(status: &str) -> Option<u64> {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
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
    use crate::semantic_native::SemanticNativePendingReasonV1;

    struct TestRepositoryFixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
    }

    impl TestRepositoryFixture {
        fn clone() -> Self {
            let temp = tempfile::tempdir().expect("temporary repository fixture");
            let root = temp.path().join("repo");
            let output = std::process::Command::new("git")
                .arg("clone")
                .arg("--quiet")
                .arg(repo_root())
                .arg(&root)
                .output()
                .expect("clone repository fixture");
            assert!(
                output.status.success(),
                "clone repository fixture: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            Self { _temp: temp, root }
        }
    }

    fn authenticated_repo_fixture() -> Arc<TestRepositoryFixture> {
        static FIXTURE: std::sync::OnceLock<Mutex<std::sync::Weak<TestRepositoryFixture>>> =
            std::sync::OnceLock::new();
        let mut fixture = FIXTURE
            .get_or_init(|| Mutex::new(std::sync::Weak::new()))
            .lock()
            .expect("repository fixture lock");
        if let Some(fixture) = fixture.upgrade() {
            return fixture;
        }
        let replacement = Arc::new(TestRepositoryFixture::clone());
        *fixture = Arc::downgrade(&replacement);
        replacement
    }

    /// Stands in for the composing binary's identity authority. The evaluator
    /// only requires a self-consistent admitted scope; resolving one from the
    /// on-disk repository identity marker is the root binary's contract and is
    /// covered where that authority lives.
    fn fixture_admitted_scope(_repo_root: &Path) -> Option<ResolvedScope> {
        ResolvedScope::new(
            ProjectId::new("project.search-eval-fixture").ok()?,
            RepositoryId::new("repository.search-eval-fixture").ok()?,
            tracedecay_domain::WorktreeId::new("worktree.search-eval-fixture").ok()?,
            None,
        )
        .ok()
    }

    fn repo_root() -> PathBuf {
        crate::checked_in_fixture_root()
    }

    fn workload() -> CandidateWorkloadV1 {
        load_candidate_workload(&repo_root().join(WORKLOAD_RELATIVE)).expect("workload loads")
    }

    #[test]
    fn direct_evaluated_material_matches_checked_in_profile() {
        let workload = workload();
        let spec = workload
            .profile_matrix
            .iter()
            .find(|profile| profile.profile_id == "query-fallback")
            .expect("checked-in fallback profile");
        let material = load_direct_evaluated_profile_material(&repo_root(), None, &spec.profile_id)
            .expect("evaluated profile material");

        assert_eq!(
            material.profile.profile_id.as_str(),
            format!("profile.{}", spec.profile_id)
        );
        assert_eq!(
            material.profile.weights_micros.get(&RetrieverKind::Lexical),
            Some(&spec.lexical_weight_ppm)
        );
        assert_eq!(
            material.profile.weights_micros.get(&RetrieverKind::Graph),
            Some(&spec.graph_weight_ppm)
        );
        assert!(
            !material
                .profile
                .weights_micros
                .contains_key(&RetrieverKind::Semantic)
        );
        assert!(material.rerank.is_none());

        let semantic =
            load_direct_evaluated_profile_material(&repo_root(), None, "hybrid-conservative")
                .expect("semantic material");
        assert!(
            semantic
                .profile
                .weights_micros
                .contains_key(&RetrieverKind::Semantic)
        );
        let reranked =
            load_direct_evaluated_profile_material(&repo_root(), None, "hybrid-reranked")
                .expect("rerank material");
        assert_eq!(
            reranked.profile.rerank_policy_id.as_ref(),
            reranked.rerank.as_ref().map(|policy| &policy.policy_id)
        );
        assert_eq!(reranked.diversity.per_file, Some(2));
        assert_eq!(
            reranked.rerank.as_ref().map(|policy| policy.max_candidates),
            Some(16)
        );
    }

    #[test]
    fn profile_material_digest_binds_every_checked_in_weight() {
        let workload = workload();
        let profile = workload.profile_matrix.first().expect("profile");
        let digest = compute_profile_material_digest(profile).expect("digest");
        let mut changed = profile.clone();
        changed.semantic_weight_ppm = changed.semantic_weight_ppm.saturating_add(1);

        assert_ne!(
            digest,
            compute_profile_material_digest(&changed).expect("changed digest")
        );
    }

    #[test]
    fn native_stage_completes_only_when_every_query_has_real_evidence() {
        let complete = SemanticNativeStageResultV1::Complete(());
        let pending = SemanticNativeStageResultV1::<()>::Pending {
            reason: SemanticNativePendingReasonV1::SemanticGenerationUnavailable,
        };

        assert_eq!(
            aggregate_native_stage(true, [&complete, &complete].into_iter()).expect("complete"),
            OptionalStageMeasurementV1::Complete
        );
        assert_eq!(
            aggregate_native_stage(true, [&complete, &pending].into_iter()).expect("pending"),
            OptionalStageMeasurementV1::Pending
        );
        assert!(
            aggregate_native_stage(false, [&complete].into_iter()).is_err(),
            "unrequested execution must fail closed"
        );
    }

    #[test]
    fn qualified_display_anchor_strips_exact_source_identity() {
        let document = CorpusDocumentV1 {
            document_id: "watermark".to_owned(),
            source_path: "crates/tracedecay-domain/src/research/watermark.rs".to_owned(),
            path: "tests/fixtures/search_quality/corpus/watermark.rs".to_owned(),
            scope: "research".to_owned(),
            language: "rust".to_owned(),
            eligibility: "eligible".to_owned(),
        };

        assert_eq!(
            display_qualified_anchor(
                &document,
                "crates/tracedecay-domain/src/research/watermark.rs::VectorWatermark::merge_max"
            ),
            "watermark::VectorWatermark::merge_max"
        );
    }

    #[test]
    fn historical_candidates_require_product_repository_identity() {
        let fixture = authenticated_repo_fixture();
        let workload = load_candidate_workload(&fixture.root.join(WORKLOAD_RELATIVE))
            .expect("markerless workload");
        let published = publish_corpus(&fixture.root, &workload, no_admitted_corpus_scope)
            .expect("markerless corpus");
        let query = workload
            .queries
            .iter()
            .find(|query| query.query_id == "train-012")
            .expect("historical query");

        let error = historical_candidates(&published, query).expect_err("identity is required");
        assert!(
            error
                .to_string()
                .contains("authoritative repository identity marker")
        );
    }

    #[test]
    fn direct_workload_requires_checked_in_labels() {
        let mut workload = workload();
        workload.queries[0].label = None;
        let error = validate_workload_for_tuning(&workload).expect_err("missing label");
        assert!(error.to_string().contains("missing its checked-in label"));
    }

    #[test]
    fn technical_query_tokens_do_not_leak_common_subtokens() {
        let absent = lexical_query_parts("qzxw_owner_validation_absent_551").expect("absent query");
        assert_eq!(
            absent.whole_terms,
            ["qzxw_owner_validation_absent_551".to_owned()]
        );
        assert!(absent.subtokens.is_empty());
        let private = lexical_query_parts("SessionEvidenceMetadataV1").expect("private query");
        assert_eq!(
            private.whole_terms,
            ["SessionEvidenceMetadataV1".to_owned()]
        );
        assert!(private.subtokens.is_empty());
    }

    #[test]
    fn direct_workload_rejects_ambiguous_corpus_identity() {
        let mut duplicate_id = workload();
        duplicate_id.corpus[1].document_id = duplicate_id.corpus[0].document_id.clone();
        let error = validate_workload_for_tuning(&duplicate_id).expect_err("duplicate document id");
        assert!(error.to_string().contains("duplicate corpus document_id"));

        let mut duplicate_path = workload();
        duplicate_path.corpus[1].path = duplicate_path.corpus[0].path.clone();
        let error =
            validate_workload_for_tuning(&duplicate_path).expect_err("duplicate corpus path");
        assert!(error.to_string().contains("duplicate corpus path"));

        let mut duplicate_source_path = workload();
        duplicate_source_path.corpus[1].source_path =
            duplicate_source_path.corpus[0].source_path.clone();
        let error = validate_workload_for_tuning(&duplicate_source_path)
            .expect_err("duplicate source path");
        assert!(error.to_string().contains("duplicate corpus source_path"));

        let mut unsafe_source_path = workload();
        unsafe_source_path.corpus[0].source_path = "../outside.rs".to_owned();
        let error =
            validate_workload_for_tuning(&unsafe_source_path).expect_err("unsafe source path");
        assert!(error.to_string().contains("safe repository-relative path"));
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

        let mut invalid_history = workload();
        invalid_history.queries[0].historical_commit = Some("not-a-commit".to_owned());
        let error =
            validate_workload_for_tuning(&invalid_history).expect_err("invalid historical commit");
        assert!(error.to_string().contains("invalid historical commit"));
    }

    #[test]
    fn direct_workload_rejects_duplicate_profile_ids_and_empty_partitions() {
        let mut duplicate = workload();
        duplicate.profile_matrix[1].profile_id = duplicate.profile_matrix[0].profile_id.clone();
        let error = validate_workload_for_tuning(&duplicate).expect_err("duplicate profile id");
        assert!(error.to_string().contains("duplicate profile_id"));

        let mut missing = workload();
        missing.queries.retain(|query| query.partition == "train");
        missing.execution_contract.exact_query_count = missing.queries.len() as u64;
        let error = validate_workload_for_tuning(&missing).expect_err("empty validation partition");
        assert!(
            error
                .to_string()
                .contains("partition validation has no queries")
        );
    }

    #[test]
    fn workload_requires_immutable_query_fallback_digests_for_both_partitions() {
        let mut missing = workload();
        missing.expected_query_fallback_digests.remove("validation");
        let error =
            validate_workload_for_tuning(&missing).expect_err("missing validation fallback digest");
        assert!(
            error
                .to_string()
                .contains("expected query fallback digests must bind train and validation")
        );

        let mut malformed = workload();
        malformed.expected_query_fallback_digests.insert(
            "train".to_owned(),
            "sha256:not-a-canonical-digest".to_owned(),
        );
        let error =
            validate_workload_for_tuning(&malformed).expect_err("malformed fallback digest");
        assert!(
            error
                .to_string()
                .contains("expected query fallback digest is not canonical")
        );
    }

    #[test]
    fn candidate_generation_rejects_partially_unknown_profile_selection() {
        let error = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: &repo_root(),
            admitted_scope: fixture_admitted_scope,
            workload_path: None,
            profile_ids: Some(&["query-fallback".to_owned(), "unknown-profile".to_owned()]),
        })
        .expect_err("unknown profile");
        assert!(error.to_string().contains("unknown requested profile_id"));
    }

    #[test]
    fn direct_outputs_cover_train_and_validation() {
        let fixture = authenticated_repo_fixture();
        let fixture_root = &fixture.root;
        let workload = workload();
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: fixture_root,
            admitted_scope: fixture_admitted_scope,
            workload_path: None,
            profile_ids: Some(&["query-fallback".to_owned()]),
        })
        .expect("generate");
        assert_eq!(result.outputs.len(), 2);
        let expected_corpus_digest =
            compute_corpus_digest(fixture_root, &workload).expect("corpus digest");
        for output in &result.outputs {
            assert_eq!(output.schema_version, 2);
            assert!(output.partition == "train" || output.partition == "validation");
            assert_eq!(output.production_boundary, PRODUCTION_BOUNDARY);
            assert_eq!(output.cancellation, REQUIRED_CANCELLATION);
            assert_eq!(output.offline, REQUIRED_OFFLINE);
            assert_eq!(output.fallback_digest, output.query_fallback_digest);
            assert_eq!(
                output.expected_query_fallback_digest,
                workload.expected_query_fallback_digests[&output.partition]
            );
            assert_eq!(
                output.query_fallback_matches_expected,
                output.query_fallback_digest == output.expected_query_fallback_digest
            );
            assert_eq!(output.corpus_digest, expected_corpus_digest);
            assert_eq!(output.seed, EVALUATION_SEED);
            assert_eq!(output.cache_state, EVALUATION_CACHE_STATE);
            let current = output.resources.get("current").expect("current samples");
            let expected_status = if peak_rss_bytes().is_some() {
                ResourceMeasurementStatusV1::Measured
            } else {
                ResourceMeasurementStatusV1::Pending
            };
            assert_eq!(current.status, expected_status);
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
            assert_eq!(ten_x.status, expected_status);
            assert_eq!(ten_x.measured_queries, output.queries.len() as u64);
            assert_eq!(
                ten_x.measured_queries,
                ten_x.latency_samples_us.len() as u64
            );
            assert_eq!(
                ten_x.eligible_chunks,
                current.eligible_chunks.saturating_mul(10)
            );
            assert_eq!(ten_x.peak_rss_bytes.is_some(), peak_rss_bytes().is_some());
            assert!(
                output.queries.iter().all(|query| {
                    serde_json::to_value(query)
                        .expect("query serializes")
                        .get("confidence_ppm")
                        .is_none()
                }),
                "candidate rows must not manufacture confidence"
            );
            for row in &output.queries {
                let query = workload
                    .queries
                    .iter()
                    .find(|query| query.query_id == row.query_id)
                    .expect("checked-in query");
                assert!(
                    row.ranked
                        .iter()
                        .all(|candidate| query.allowed_scopes.contains(&candidate.scope)),
                    "{} leaked a candidate outside its allowed scopes",
                    row.query_id
                );
                assert!(
                    row.ranked.iter().all(|candidate| {
                        !candidate.anchors.is_empty()
                            && candidate.anchors.contains(&candidate.anchor)
                    }),
                    "{} lost authoritative candidate anchors",
                    row.query_id
                );
            }
        }
    }

    #[test]
    fn published_corpus_maps_production_source_occurrences() {
        let fixture = authenticated_repo_fixture();
        let workload = workload();
        let published = publish_corpus(&fixture.root, &workload, fixture_admitted_scope)
            .expect("publish corpus");

        for chunk in published.generation.chunks().chunks() {
            let chunk_occurrence = format!("code-chunk:{}", chunk.id.as_str());
            assert!(
                published.occurrence_map.contains_key(&chunk_occurrence),
                "missing exact chunk occurrence {chunk_occurrence}"
            );
            if let Some(symbol) = &chunk.anchor.symbol_occurrence_id {
                let symbol_occurrence = format!("code-symbol:{}", symbol.as_str());
                assert!(
                    published.occurrence_map.contains_key(&symbol_occurrence),
                    "missing fused symbol occurrence {symbol_occurrence}"
                );
                let graph_occurrence = format!("code-graph:{}", symbol.as_str());
                assert!(
                    published.occurrence_map.contains_key(&graph_occurrence),
                    "missing exact graph occurrence {graph_occurrence}"
                );
            }
        }

        let authoritative_anchors: BTreeSet<_> = published
            .occurrence_map
            .values()
            .flat_map(|entry| entry.display_anchors.iter().map(String::as_str))
            .collect();
        for expected in [
            "watermark::VectorWatermark::merge_max",
            "error::DomainError::InvalidTimeInterval",
            "time::TimeInterval::validate",
            "config_store::ConfigStore::write_config",
            "coverage::RetentionClass::new",
        ] {
            assert!(
                authoritative_anchors.contains(expected),
                "missing extraction-qualified anchor {expected}"
            );
        }

        let path_query = workload
            .queries
            .iter()
            .find(|query| query.query_id == "validation-003")
            .expect("path query");
        let path_output =
            compose_production_query(&published, &workload.profile_matrix[0], path_query)
                .expect("path query composes");
        let path_rows =
            map_ranked_candidates(&published, &path_output).expect("path candidates map");
        assert!(
            path_rows
                .iter()
                .any(|candidate| candidate.document_id == "watermark"),
            "snapshot logical path must retrieve the bound source document"
        );

        let history_query = workload
            .queries
            .iter()
            .find(|query| query.query_id == "train-012")
            .expect("history query");
        let (history_status, history_rows) =
            historical_candidates(&published, history_query).expect("history query executes");
        assert_eq!(history_status, HistoricalQueryExecutionV1::Complete);
        assert!(history_rows.iter().any(|candidate| {
            candidate
                .anchors
                .iter()
                .any(|anchor| anchor.contains("crates/tracedecay-domain/src/session.rs"))
        }));

        for profile in &workload.profile_matrix {
            for query in &workload.queries {
                let output =
                    compose_production_query(&published, profile, query).expect("production query");
                for ranked in output.ranked_candidates {
                    assert!(
                        published
                            .occurrence_map
                            .contains_key(ranked.candidate.anchor_id.as_str())
                            || ranked.candidate.occurrences.iter().any(|occurrence| {
                                published
                                    .occurrence_map
                                    .contains_key(occurrence.source_occurrence_id.as_str())
                            }),
                        "ranked candidate {} has no corpus occurrence binding",
                        ranked.candidate.anchor_id
                    );
                }
            }
        }
    }

    #[test]
    fn complete_optional_stage_status_requires_native_evidence_at_validation() {
        let stages = serde_json::from_value::<OptionalStageMeasurementsV1>(serde_json::json!({
            "semantic": "complete",
            "rerank": "not_requested"
        }))
        .expect("complete is an evidence-bearing native state");
        assert_eq!(stages.semantic, OptionalStageMeasurementV1::Complete);
    }

    #[test]
    fn native_query_stages_and_late_hydration_emit_raw_measurements() {
        let fixture = authenticated_repo_fixture();
        let workload = workload();
        let published = publish_corpus(&fixture.root, &workload, fixture_admitted_scope)
            .expect("published corpus");
        let profile = workload
            .profile_matrix
            .iter()
            .find(|profile| profile.profile_id == "query-fallback")
            .expect("fallback profile");
        let query = workload.queries.first().expect("query");
        let prepared =
            prepare_production_query(&published, profile, query).expect("prepared query");
        let fusion = fusion_profile(profile, &retrieval_budget(), true).expect("fusion");
        let mut native = evaluate_native_query(SemanticNativeQueryInputV1 {
            profile_spec: profile,
            fusion_profile: &fusion,
            diversity_policy: &prepared.diversity,
            kernel: &prepared.kernel,
            fallback_lanes: &prepared.fallback_lanes,
            query_measurements: prepared.query_measurements,
            semantic: None,
            fallback: &prepared.fallback,
            rerank: None,
        })
        .expect("native query evaluation");
        let ranked = native.rerank.off.clone();
        native.measurements.hydration = Some(
            measure_late_hydration(&published, &prepared.request, &ranked, &retrieval_budget())
                .expect("late hydration"),
        );

        assert_eq!(
            native.measurements.query.lexical.output_candidates,
            prepared
                .fallback_lanes
                .iter()
                .find(|lane| lane.lane == RetrieverKind::Lexical)
                .map(|lane| retriever_outcome_candidate_count(&lane.outcome))
                .expect("lexical lane")
        );
        assert!(native.ablations.iter().all(|ablation| {
            ablation.measurement.output_candidates == ablation.ranked_candidates.len() as u64
        }));
        let hydration = native
            .measurements
            .hydration
            .expect("hydration measurement");
        assert_eq!(hydration.source_fetches, hydration.receipts);
        assert!(hydration.receipts <= hydration.selected_candidates);
    }

    #[test]
    fn distinct_ten_x_corpus_produces_measured_resource_evidence() {
        const CHILD_ENV: &str = "TRACEDECAY_RESOURCE_EVIDENCE_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            assert_distinct_ten_x_corpus_produces_measured_resource_evidence();
            return;
        }

        let output = std::process::Command::new(
            std::env::current_exe().expect("resource test binary has a current executable"),
        )
        .args([
            "--exact",
            "search_eval::candidate_output::tests::distinct_ten_x_corpus_produces_measured_resource_evidence",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .expect("run resource measurement in a dedicated process");
        assert!(
            output.status.success(),
            "dedicated resource measurement failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn assert_distinct_ten_x_corpus_produces_measured_resource_evidence() {
        let fixture = authenticated_repo_fixture();
        let fixture_root = &fixture.root;
        let workload = workload();
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: fixture_root,
            admitted_scope: fixture_admitted_scope,
            workload_path: None,
            profile_ids: Some(&["query-fallback".to_owned()]),
        })
        .expect("generate");
        let report =
            crate::evaluate_generated_outputs(fixture_root, &workload, &result).expect("evaluate");

        let expected_status = if peak_rss_bytes().is_some() {
            crate::DirectEvaluationStatusV1::Pass
        } else {
            crate::DirectEvaluationStatusV1::Pending
        };
        let resources = result
            .outputs
            .iter()
            .map(|output| (&output.partition, &output.resources))
            .collect::<Vec<_>>();
        assert_eq!(
            report.status, expected_status,
            "unexpected grouped resource evaluation: {resources:#?}"
        );
        assert!(
            report
                .profiles
                .iter()
                .all(|profile| { profile.resource_status == expected_status })
        );

        let current = publish_corpus(fixture_root, &workload, fixture_admitted_scope)
            .expect("current corpus");
        let ten_x = publish_corpus_with_scale(fixture_root, &workload, 10, fixture_admitted_scope)
            .expect("10x corpus");
        assert_ne!(
            current.generation.manifest().generation_id,
            ten_x.generation.manifest().generation_id
        );
        assert_eq!(
            ten_x.eligible_chunks,
            current.eligible_chunks.saturating_mul(10)
        );
        for published in [&current, &ten_x] {
            let no_op = &published.no_op_generation.projection().request().changes;
            assert!(no_op.added_or_changed.is_empty());
            assert!(no_op.deleted.is_empty());
            let deletion = &published.deletion_generation.projection().request().changes;
            assert!(!deletion.deleted.is_empty());
            assert_eq!(
                deletion.from_generation.as_ref(),
                Some(&published.no_op_generation.manifest().generation_id)
            );
        }

        let mut missing_resource = result.clone();
        missing_resource.outputs[0].resources.remove("10x");
        let report = crate::evaluate_generated_outputs(fixture_root, &workload, &missing_resource)
            .expect("evaluate");
        assert_eq!(report.status, crate::DirectEvaluationStatusV1::Fail);
        assert_eq!(
            report.profiles[0].resource_status,
            crate::DirectEvaluationStatusV1::Fail
        );

        let mut duplicate_query = result.clone();
        duplicate_query.outputs[0].queries[1] = duplicate_query.outputs[0].queries[0].clone();
        let error = crate::evaluate_generated_outputs(&repo_root(), &workload, &duplicate_query)
            .expect_err("duplicate query row");
        assert!(error.to_string().contains("duplicate query row"));

        let mut duplicate_profile_partition = result.clone();
        duplicate_profile_partition.outputs[1] = duplicate_profile_partition.outputs[0].clone();
        let error = crate::evaluate_generated_outputs(
            &repo_root(),
            &workload,
            &duplicate_profile_partition,
        )
        .expect_err("duplicate profile partition");
        assert!(error.to_string().contains("duplicate profile/partition"));

        let mut forged = result;
        forged.outputs[0].production_boundary = "lookalike".to_owned();
        let error = crate::evaluate_generated_outputs(&repo_root(), &workload, &forged)
            .expect_err("forged production boundary");
        assert!(error.to_string().contains("production boundary"));

        forged.outputs[0].production_boundary = PRODUCTION_BOUNDARY.to_owned();
        forged.outputs[0].fixture_source_commit = "forged".to_owned();
        let error = crate::evaluate_generated_outputs(&repo_root(), &workload, &forged)
            .expect_err("forged source commit");
        assert!(error.to_string().contains("source commit"));

        forged.outputs[0].fixture_source_commit = workload.source_repository_commit.clone();
        forged.outputs[0].corpus_digest = canonical_sha256(&"forged corpus").expect("digest");
        let error = crate::evaluate_generated_outputs(&repo_root(), &workload, &forged)
            .expect_err("forged corpus digest");
        assert!(error.to_string().contains("byte-exact corpus"));

        forged.outputs[0].corpus_digest =
            compute_corpus_digest(&repo_root(), &workload).expect("corpus digest");
        forged.outputs[0].toolchain.clear();
        let error = crate::evaluate_generated_outputs(&repo_root(), &workload, &forged)
            .expect_err("missing environment");
        assert!(error.to_string().contains("environment summary"));

        forged.outputs[0].toolchain = "rustc:test".to_owned();
        forged.outputs[0].queries[0].abstained = !forged.outputs[0].queries[0].ranked.is_empty();
        let error = crate::evaluate_generated_outputs(&repo_root(), &workload, &forged)
            .expect_err("inconsistent abstention");
        assert!(error.to_string().contains("inconsistent abstention"));
    }

    #[test]
    fn resource_sample_reads_linux_peak_rss() {
        let status = "VmRSS:\t1024 kB\nVmHWM:\t2048 kB\n";
        assert_eq!(peak_rss_bytes_from_status(status), Some(2 * 1024 * 1024));
    }

    #[test]
    fn evaluation_rejects_optional_stage_status_that_disagrees_with_profile() {
        let fixture = authenticated_repo_fixture();
        let workload = workload();
        let mut result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: &fixture.root,
            admitted_scope: fixture_admitted_scope,
            workload_path: None,
            profile_ids: Some(&["hybrid-reranked".to_owned()]),
        })
        .expect("generate");
        result.outputs[0].optional_stages.semantic = OptionalStageMeasurementV1::NotRequested;

        let error = crate::evaluate_generated_outputs(&fixture.root, &workload, &result)
            .expect_err("configured semantic stage cannot be reported as not requested");
        assert!(error.to_string().contains("optional stage status"));
    }

    #[test]
    fn resource_evidence_enforces_state_budgets_and_exact_catalog() {
        let fixture = authenticated_repo_fixture();
        let fixture_root = &fixture.root;
        let workload = workload();
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: fixture_root,
            admitted_scope: fixture_admitted_scope,
            workload_path: None,
            profile_ids: Some(&["query-fallback".to_owned()]),
        })
        .expect("generate");

        let mut invalid_pending = result.clone();
        let current = invalid_pending.outputs[0]
            .resources
            .get_mut("current")
            .expect("current resource");
        current.status = ResourceMeasurementStatusV1::Pending;
        current.pending_reason = None;
        let report = crate::evaluate_generated_outputs(fixture_root, &workload, &invalid_pending)
            .expect("evaluate");
        assert_eq!(
            report.profiles[0].resource_status,
            crate::DirectEvaluationStatusV1::Fail
        );

        let mut wrong_scale = result.clone();
        let current_chunks = wrong_scale.outputs[0]
            .resources
            .get("current")
            .expect("current resource")
            .eligible_chunks;
        wrong_scale.outputs[0]
            .resources
            .get_mut("10x")
            .expect("10x resource")
            .eligible_chunks = current_chunks;
        let report = crate::evaluate_generated_outputs(fixture_root, &workload, &wrong_scale)
            .expect("evaluate");
        assert_eq!(
            report.profiles[0].resource_status,
            crate::DirectEvaluationStatusV1::Fail
        );

        let mut over_budget = result.clone();
        let current = over_budget.outputs[0]
            .resources
            .get_mut("current")
            .expect("current resource");
        current.status = ResourceMeasurementStatusV1::Measured;
        current.peak_rss_bytes = Some(1);
        current.pending_reason = None;
        current.latency_samples_us.fill(
            workload
                .resource_budgets
                .current
                .maximum_p99_latency_us
                .saturating_add(1),
        );
        let report = crate::evaluate_generated_outputs(fixture_root, &workload, &over_budget)
            .expect("evaluate");
        assert_eq!(
            report.profiles[0].resource_status,
            crate::DirectEvaluationStatusV1::Fail
        );

        let mut extra_resource = result;
        let synthetic = extra_resource.outputs[0]
            .resources
            .get("current")
            .expect("current resource")
            .clone();
        extra_resource.outputs[0]
            .resources
            .insert("synthetic".to_owned(), synthetic);
        let report = crate::evaluate_generated_outputs(fixture_root, &workload, &extra_resource)
            .expect("evaluate");
        assert_eq!(
            report.profiles[0].resource_status,
            crate::DirectEvaluationStatusV1::Fail
        );
    }

    #[test]
    fn candidate_bytes_match_direct_production_calls() {
        let fixture = authenticated_repo_fixture();
        let workload = workload();
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: &fixture.root,
            admitted_scope: fixture_admitted_scope,
            workload_path: None,
            profile_ids: Some(&["query-fallback".to_owned()]),
        })
        .expect("generate");
        let train = result
            .outputs
            .iter()
            .find(|output| output.partition == "train" && output.profile_id == "query-fallback")
            .expect("train output");
        let probe = train.queries.first().expect("at least one train query");
        let direct = retrieve_partition_query_bytes(
            &fixture.root,
            &workload,
            "query-fallback",
            &probe.query_id,
            fixture_admitted_scope,
        )
        .expect("direct retrieve");
        let generated = canonical_json_bytes(probe).expect("generated bytes");
        assert_eq!(
            generated, direct,
            "generator row must match direct production call bytes"
        );
    }

    #[test]
    fn query_phrase_and_historical_queries_reach_their_checked_in_anchors() {
        let fixture = authenticated_repo_fixture();
        let workload = workload();
        let retrieve = |query_id: &str| {
            let bytes = retrieve_partition_query_bytes(
                &fixture.root,
                &workload,
                "query-fallback",
                query_id,
                fixture_admitted_scope,
            )
            .expect("direct retrieve");
            serde_json::from_slice::<QueryCandidateRowV1>(&bytes).expect("candidate row")
        };

        let diagnostic = retrieve("train-004");
        let diagnostic_top = diagnostic.ranked.iter().take(10).collect::<Vec<_>>();
        let diagnostic_target = diagnostic.ranked.iter().enumerate().find(|(_, candidate)| {
            candidate.anchor == "error::DomainError::InvalidTimeInterval"
                || candidate
                    .anchors
                    .iter()
                    .any(|anchor| anchor == "error::DomainError::InvalidTimeInterval")
        });
        assert!(
            diagnostic_top.iter().any(|candidate| {
                candidate.anchor == "error::DomainError::InvalidTimeInterval"
                    || candidate
                        .anchors
                        .iter()
                        .any(|anchor| anchor == "error::DomainError::InvalidTimeInterval")
            }),
            "diagnostic target: {diagnostic_target:#?}; top 10: {diagnostic_top:#?}"
        );

        let exact_symbol = retrieve("train-001");
        let unique_exact_symbol_anchors = exact_symbol
            .ranked
            .iter()
            .map(|candidate| candidate.anchor.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            unique_exact_symbol_anchors.len(),
            exact_symbol.ranked.len(),
            "duplicate visible anchors: {:#?}",
            exact_symbol.ranked
        );

        let historical = retrieve("train-012");
        assert!(historical.ranked.iter().take(10).any(|candidate| {
            candidate.anchor
                == "git:01b0a0afe34c3342d6b5b076383f86ed8a8d0c66:crates/tracedecay-domain/src/session.rs::ClosedUtcIntervalV1"
                || candidate.anchors.iter().any(|anchor| {
                    anchor
                        == "git:01b0a0afe34c3342d6b5b076383f86ed8a8d0c66:crates/tracedecay-domain/src/session.rs::ClosedUtcIntervalV1"
                })
        }));
    }

    #[test]
    fn semantic_profiles_do_not_claim_a_comparison_when_only_fallback_ran() {
        let fixture = authenticated_repo_fixture();
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: &fixture.root,
            admitted_scope: fixture_admitted_scope,
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
        let fixture = authenticated_repo_fixture();
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: &fixture.root,
            admitted_scope: fixture_admitted_scope,
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
