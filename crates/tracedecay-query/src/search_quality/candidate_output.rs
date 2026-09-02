//! Production candidate types, packaged-profile material, and digest authority.
//!
//! Candidate generation that publishes a fixture corpus stays in
//! `tracedecay-search-eval`. This module is the production kernel those
//! generators and evaluators share.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_application::historical_query::HistoricalGitReadUnavailableReasonV1;
use tracedecay_application::is_canonical_repository_relative_path;
use tracedecay_code_index::chunks::content_digest;
use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::canonical_text::encode_tagged_lowercase_hex;
use tracedecay_domain::git::GitOidV1;
use tracedecay_domain::{
    CalibrationProfileId, CodeGenerationId, CodeSearchChunkId, DiversityPolicy, DiversityPolicyId,
    EphemeralSanitizedQueryViewV1, FusionProfile, FusionProfileId, ManifestDigest, RerankPolicy,
    RetrievalAnchorId, RetrievalBudget, RetrievalRequest, RetrieverKind, ScoreDomainCalibrationV1,
    ScoreDomainId, VectorGenerationIdV1,
};

use super::semantic_native::{
    SemanticNativeQueryOutputV1, SemanticNativeRerankInputV1, SemanticNativeResourceEvidenceV1,
    SemanticNativeResourceSampleV1, SemanticNativeSemanticInputV1, SemanticNativeStageResultV1,
    SemanticProjectionCaseSampleV1, SemanticProjectionCaseV1,
};
pub const WORKLOAD_RELATIVE: &str =
    "tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json";
pub const PRODUCTION_BOUNDARY: &str = "CompositionKernel::compose";
pub const EVALUATION_MODEL_REVISION: &str =
    "JinaEmbeddingsV2BaseCode@516f4baf13dec4ddddda8631e019b5737c8bc250";
pub const EVALUATION_PROJECTION_REVISION: &str = "retriever.semantic-flat.evaluation.v1";
pub const EVALUATION_RUNTIME_REVISION: &str = "semantic.fastembed.production.v1";
pub const REQUIRED_CANCELLATION: &str = "bounded_typed_cancelled";
pub const REQUIRED_OFFLINE: &str = "no_network_and_query_fallback_available";
pub const EVALUATION_SEED: &str = "not_applicable_deterministic_no_rng";
pub const EVALUATION_CACHE_STATE: &str = "cold_empty_in_memory_publication";
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
    /// Per-profile semantic acceptance cut-off, expressed as a minimum
    /// nonnegative cosine similarity in parts per million. This is not the
    /// former shifted `[-1, 1]` calibration domain.
    ///
    /// # This value is in force
    ///
    /// It becomes `FusionProfile::minimum_calibrated_feature_micros[Semantic]`
    /// (see `fusion_profile` below), it is carried through
    /// `DirectEvaluatedProfileMaterialV1` into the daemon candidate builder,
    /// and `crate::retrieval::fusion` drops every semantic
    /// contribution whose calibrated feature falls under it. Editing this
    /// number changes production retrieval, not just the evaluation fixture.
    ///
    /// An earlier revision of this comment claimed the opposite — that the
    /// field was documentation only — after the wiring had already landed. The
    /// claim survived long enough for the value to be re-tuned five times in
    /// one day (`700000` → `690000` → `700000` → `400000` → `635000`) as a way
    /// to move a failing activation gate. Keep `calibration_threshold_ppm_is_in_force`
    /// green so the claim cannot come back.
    ///
    /// # It is a second cut on an already-measured quantity
    ///
    /// The semantic lane *also* abstains on
    /// `SemanticCalibrationProfileV1::maximum_distance_micros`, which
    /// `tracedecay_usecases::semantic_runtime::measure_acceptance_calibration`
    /// measures from the committed generation's own vectors — deliberately,
    /// because a cosine cut-off is a property of the model and corpus rather
    /// than of a checked-in profile. This field therefore imposes a *second*,
    /// tighter, unmeasured cut on the same cosine score after the measured one
    /// has already run.
    ///
    /// That gap is real and is documented in `acceptance_calibration`: the
    /// measured bound is a code↔code background distribution, while this gate
    /// decides natural-language↔code queries, which sit in a different score
    /// regime. A hand-picked constant is not the fix. The workload already
    /// carries the labelled data the honest fix needs — relevant anchors and
    /// `no_answer` negatives, split into `train` and `validation` — so the cut
    /// must be derived from the train partition's measured positive/negative
    /// separation and then hold on validation. Until that derivation exists,
    /// do not move this number to make a gate pass.
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
    pub tokenizer_bytes: u64,
    pub threads: u32,
    pub max_concurrent_sessions: u32,
    pub batch_size: u32,
    pub sequence_length: u32,
    pub load_deadline_ms: u64,
    pub cold_model_load_micros: u64,
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
    compute_corpus_digest_from_document_bytes(workload, |document| {
        let absolute = repo_root.join(&document.path);
        fs::read(&absolute)
            .map(Cow::Owned)
            .map_err(|source| CandidateOutputError::Read {
                path: absolute,
                source,
            })
    })
}

/// Compute the corpus identity from the package's embedded authoritative
/// bytes. Unlike [`compute_corpus_digest`], this validates no filesystem or
/// Git state and therefore cannot materialize the evaluator fixture.
pub fn compute_corpus_digest_from_embedded_bytes(
    workload: &CandidateWorkloadV1,
    files: &[(&str, &[u8])],
) -> Result<String, CandidateOutputError> {
    compute_corpus_digest_from_document_bytes(workload, |document| {
        files
            .iter()
            .find_map(|(path, bytes)| (*path == document.path).then_some(*bytes))
            .map(Cow::Borrowed)
            .ok_or_else(|| {
                CandidateOutputError::Contract(format!(
                    "packaged evaluator corpus is missing {}",
                    document.path
                ))
            })
    })
}

fn compute_corpus_digest_from_document_bytes<'a>(
    workload: &CandidateWorkloadV1,
    mut document_bytes: impl FnMut(&CorpusDocumentV1) -> Result<Cow<'a, [u8]>, CandidateOutputError>,
) -> Result<String, CandidateOutputError> {
    let mut bindings = Vec::with_capacity(workload.corpus.len());
    let mut corpus_bytes = 0_u64;
    for document in &workload.corpus {
        let bytes = document_bytes(document)?;
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
        if profile.calibration_threshold_ppm > 1_000_000 {
            return Err(CandidateOutputError::Contract(format!(
                "profile {} calibration threshold exceeds one million ppm",
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
        profile: fusion_profile(profile, true)?,
        diversity: evaluated_diversity_policy()?,
        rerank: evaluated_rerank_policy(profile)?,
    })
}

pub fn fusion_profile(
    profile: &ProfileSpecV1,
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
                typed_id::<CalibrationProfileId>(&format!(
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
            crate::retrieval::QUERY_EXACT_SCORE_DOMAIN_V1,
        ),
        (
            RetrieverKind::Lexical,
            crate::retrieval::QUERY_LEXICAL_SCORE_DOMAIN_V1,
        ),
        (
            RetrieverKind::Graph,
            crate::retrieval::QUERY_GRAPH_SCORE_DOMAIN_V1,
        ),
        (
            RetrieverKind::Semantic,
            crate::retrieval::QUERY_SEMANTIC_EVALUATION_SCORE_DOMAIN_V1,
        ),
    ]
    .into_iter()
    .filter(|(lane, _)| weights.contains_key(lane))
    .map(|(lane, domain)| {
        let score_domain = typed_id::<ScoreDomainId>(domain)?;
        let (raw_min_micros, raw_max_micros) = if lane == RetrieverKind::Semantic {
            (
                crate::retrieval::QUERY_SEMANTIC_EVALUATION_SCORE_RAW_MIN_MICROS_V1,
                crate::retrieval::QUERY_SEMANTIC_EVALUATION_SCORE_RAW_MAX_MICROS_V1,
            )
        } else {
            (0, 1_000_000)
        };
        Ok((
            score_domain.clone(),
            ScoreDomainCalibrationV1 {
                calibration_profile_id: typed_id(&format!(
                    "calibration.{}.{}",
                    lane.as_str(),
                    profile.profile_id
                ))?,
                score_domain,
                raw_min_micros,
                raw_max_micros,
            },
        ))
    })
    .collect::<Result<BTreeMap<_, _>, CandidateOutputError>>()?;
    Ok(FusionProfile {
        profile_id: typed_id::<FusionProfileId>(&format!("profile.{}", profile.profile_id))?,
        evaluation_result_anchor: typed_id::<RetrievalAnchorId>(&format!(
            "evaluation.{}",
            profile.profile_id
        ))?,
        calibrations,
        score_domain_calibrations,
        minimum_calibrated_feature_micros: (include_semantic && profile.semantic_weight_ppm > 0)
            .then_some((RetrieverKind::Semantic, profile.calibration_threshold_ppm))
            .into_iter()
            .collect(),
        weights_micros: weights,
        diversity_policy_id: typed_id::<DiversityPolicyId>("diversity.candidate.v1")?,
        rerank_policy_id: profile
            .rerank_policy
            .as_ref()
            .map(|policy| typed_id(&policy.policy_id))
            .transpose()?,
        retrieval_budget: retrieval_budget(),
    })
}

pub fn retrieval_budget() -> RetrievalBudget {
    RetrievalBudget {
        max_candidates_per_lane: 32,
        max_fused_candidates: 32,
        max_hydrated_results: 16,
        max_hydration_bytes: 65_536,
        deadline_micros: None,
    }
}

pub fn evaluated_diversity_policy() -> Result<DiversityPolicy, CandidateOutputError> {
    Ok(DiversityPolicy {
        policy_id: typed_id("diversity.candidate.v1")?,
        evaluation_result_anchor: Some(typed_id("evaluation.candidate.v1")?),
        per_source_namespace: None,
        per_source_instance: None,
        per_repository: None,
        per_file: Some(2),
        per_session_or_thread: None,
        per_copy_cluster: None,
        per_evidence_role: None,
    })
}

pub fn evaluated_rerank_policy(
    profile: &ProfileSpecV1,
) -> Result<Option<RerankPolicy>, CandidateOutputError> {
    let evaluation_result_anchor =
        typed_id::<RetrievalAnchorId>(&format!("evaluation.{}", profile.profile_id))?;
    profile
        .rerank_policy
        .as_ref()
        .map(|policy| {
            Ok(RerankPolicy {
                policy_id: typed_id(&policy.policy_id)?,
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

pub fn typed_id<T>(value: &str) -> Result<T, CandidateOutputError>
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Display,
{
    T::try_from(value.to_owned()).map_err(|error| CandidateOutputError::Contract(error.to_string()))
}

pub fn canonical_sha256<T: Serialize>(value: &T) -> Result<String, CandidateOutputError> {
    let bytes = canonical_json_bytes(value)?;
    Ok(encode_tagged_lowercase_hex(
        "sha256:",
        &Sha256::digest(bytes),
    ))
}

pub fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, CandidateOutputError> {
    let mut bytes = serde_json::to_vec(value)
        .map_err(|error| CandidateOutputError::Contract(format!("serialize: {error}")))?;
    // Stable formatting: re-parse and dump sorted keys via serde_json Value.
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| CandidateOutputError::Contract(format!("reparse: {error}")))?;
    bytes = serde_json::to_vec(&sort_value(value))
        .map_err(|error| CandidateOutputError::Contract(format!("reserialize: {error}")))?;
    Ok(bytes)
}

pub fn sort_value(value: serde_json::Value) -> serde_json::Value {
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
