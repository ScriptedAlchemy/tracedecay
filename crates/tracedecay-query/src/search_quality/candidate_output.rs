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
use tracedecay_code_index::chunks::content_digest;
use tracedecay_contracts::historical_query::HistoricalGitReadUnavailableReasonV1;
use tracedecay_contracts::is_canonical_repository_relative_path;
use tracedecay_domain::canonical_text::encode_tagged_lowercase_hex;
use tracedecay_domain::git::GitOidV1;
use tracedecay_domain::{
    CalibrationProfileId, DiversityPolicy, DiversityPolicyId, FusionProfile, FusionProfileId,
    RetrievalAnchorId, RetrievalBudget, RetrieverKind, ScoreDomainCalibrationV1, ScoreDomainId,
};

use crate::retrieval::lexical::LexicalAliasV1;

pub const WORKLOAD_RELATIVE: &str =
    "tests/fixtures/search_quality/query-lexical-graph-workload-v1.json";
pub const PRODUCTION_BOUNDARY: &str = "CompositionKernel::compose";
pub const REQUIRED_CANCELLATION: &str = "bounded_typed_cancelled";
pub const REQUIRED_OFFLINE: &str = "no_network_and_query_fallback_available";
pub const EVALUATION_SEED: &str = "not_applicable_deterministic_no_rng";
pub const EVALUATION_CACHE_STATE: &str = "cold_empty_in_memory_publication";
/// The stratum whose queries are conceptual needs rather than technical
/// lookups. Every query in it must document where the need came from and
/// which corpus symbols answer it, so a relevance judgment is never an
/// unsourced assertion.
pub const NEED_STRATUM: &str = "natural_language";
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
    pub corpus: Vec<CorpusDocumentV1>,
    pub profile_matrix: Vec<ProfileSpecV1>,
    pub decision_policy: DecisionPolicySliceV1,
    pub expected_query_fallback_digests: BTreeMap<String, String>,
    pub queries: Vec<WorkloadQueryV1>,
}

/// Where one natural-language need came from, and why its labelled targets
/// answer it.
///
/// The quote is verified verbatim against the cited corpus document, so a
/// fabricated citation fails workload validation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NeedProvenanceV1 {
    pub source_kind: NeedProvenanceKindV1,
    pub source_document_id: String,
    pub source_quote: String,
    pub judgment_rationale: String,
}

/// Artifact classes that may source a natural-language need. Each one is prose
/// written for the corpus itself, not for this evaluator.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NeedProvenanceKindV1 {
    /// A doc comment stating what a capability is for.
    CorpusDocumentation,
    /// A user-visible error string stating a rule.
    CorpusErrorContract,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvaluationExecutionContractV1 {
    pub exact_file_count: u64,
    pub exact_corpus_bytes: u64,
    pub exact_eligible_chunks_current: u64,
    pub exact_eligible_chunks_10x: u64,
    pub exact_query_count: u64,
    pub fusion_revision: String,
    pub cache_state: String,
    pub concurrency: EvaluationConcurrencyContractV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvaluationConcurrencyContractV1 {
    pub query_workers: u32,
    pub query_execution: String,
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
    /// Lexical lane weight in parts per million; the exact lane is always
    /// weighted at one million.
    pub lexical_weight_ppm: u32,
    pub graph_weight_ppm: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DecisionPolicySliceV1 {
    pub required_cancellation: String,
    pub required_offline: String,
    pub required_fallback_byte_stability: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkloadQueryV1 {
    pub query_id: String,
    pub partition: String,
    pub strata: Vec<String>,
    pub query: String,
    pub allowed_scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lexical_aliases: Vec<LexicalAliasV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub historical_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<serde_json::Value>,
    /// Required on every query in [`NEED_STRATUM`]: a measured quality is only
    /// as good as the needs it is measured on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub need_provenance: Option<NeedProvenanceV1>,
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
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
pub enum HistoricalQueryExecutionV1 {
    NotRequested,
    Complete,
    Unavailable(HistoricalGitReadUnavailableReasonV1),
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
    pub resources: BTreeMap<String, ResourceSampleV1>,
    pub queries: Vec<QueryCandidateRowV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenerateCandidateOutputsResultV1 {
    pub workload_digest: String,
    pub outputs: Vec<ProductionCandidateOutputV1>,
}

/// Load the checked-in exact/lexical/graph direct-evaluation workload.
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

/// Verify every documented need's provenance quote against the corpus bytes
/// this build carries.
///
/// Provenance that cannot be found in the document it cites is not provenance,
/// so this refuses a fabricated citation instead of trusting the workload's own
/// claim about itself. Comment markers and line wrapping are normalized away:
/// the quote is prose, not a byte-exact source line.
pub fn validate_need_provenance_against_embedded_corpus(
    workload: &CandidateWorkloadV1,
    files: &[(&str, &[u8])],
) -> Result<(), CandidateOutputError> {
    for query in &workload.queries {
        let Some(provenance) = &query.need_provenance else {
            continue;
        };
        let document = workload
            .corpus
            .iter()
            .find(|document| document.document_id == provenance.source_document_id)
            .ok_or_else(|| {
                CandidateOutputError::Contract(format!(
                    "need {} cites document {} which is outside the corpus",
                    query.query_id, provenance.source_document_id
                ))
            })?;
        let bytes = files
            .iter()
            .find_map(|(path, bytes)| (*path == document.path).then_some(*bytes))
            .ok_or_else(|| {
                CandidateOutputError::Contract(format!(
                    "packaged evaluator corpus is missing {}",
                    document.path
                ))
            })?;
        let prose = normalized_document_prose(bytes);
        if !prose.contains(&collapse_whitespace(&provenance.source_quote)) {
            return Err(CandidateOutputError::Contract(format!(
                "need {} quotes text that does not appear in {}",
                query.query_id, document.source_path
            )));
        }
    }
    Ok(())
}

/// One whitespace-collapsed line of prose per document, comment markers removed.
fn normalized_document_prose(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let joined = text
        .lines()
        .map(|line| {
            let line = line.trim();
            line.strip_prefix("///")
                .or_else(|| line.strip_prefix("//!"))
                .or_else(|| line.strip_prefix("//"))
                .or_else(|| line.strip_prefix('#'))
                .unwrap_or(line)
                .trim()
        })
        .collect::<Vec<_>>()
        .join(" ");
    collapse_whitespace(&joined)
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
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
        || contract.fusion_revision != PRODUCTION_BOUNDARY
        || contract.cache_state != EVALUATION_CACHE_STATE
        || contract.concurrency.query_workers != 1
        || contract.concurrency.query_execution != "serial_exact_workload_order"
    {
        return Err(CandidateOutputError::Contract(
            "evaluation execution contract does not match the production workload".to_owned(),
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
        .any(|digest| !is_canonical_sha256(digest))
    {
        return Err(CandidateOutputError::Contract(
            "expected query fallback digest is not canonical".to_owned(),
        ));
    }
    validate_need_provenance(workload)
}

fn is_canonical_sha256(digest: &str) -> bool {
    digest.len() == 71
        && digest.starts_with("sha256:")
        && digest[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Every [`NEED_STRATUM`] query must document its provenance, and every
/// documented need must say where it came from and what answers it.
///
/// A measured quality is only as good as the needs it is measured on, so a
/// need without a verifiable source or a relevance judgment is refused rather
/// than scored.
fn validate_need_provenance(workload: &CandidateWorkloadV1) -> Result<(), CandidateOutputError> {
    for need in &workload.queries {
        let in_need_stratum = need.strata.iter().any(|name| name == NEED_STRATUM);
        let Some(provenance) = &need.need_provenance else {
            if in_need_stratum {
                return Err(CandidateOutputError::Contract(format!(
                    "{NEED_STRATUM} need {} has no documented provenance",
                    need.query_id
                )));
            }
            continue;
        };
        if provenance.source_quote.trim().is_empty()
            || provenance.judgment_rationale.trim().is_empty()
        {
            return Err(CandidateOutputError::Contract(format!(
                "need {} has an empty provenance quote or rationale",
                need.query_id
            )));
        }
        if !workload
            .corpus
            .iter()
            .any(|document| document.document_id == provenance.source_document_id)
        {
            return Err(CandidateOutputError::Contract(format!(
                "need {} cites document {} which is outside the corpus",
                need.query_id, provenance.source_document_id
            )));
        }
        let labelled_targets = need
            .label
            .as_ref()
            .and_then(|label| label.get("anchors"))
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
        if labelled_targets == 0 {
            return Err(CandidateOutputError::Contract(format!(
                "need {} has no relevance judgment",
                need.query_id
            )));
        }
    }
    Ok(())
}

pub fn fusion_profile(profile: &ProfileSpecV1) -> Result<FusionProfile, CandidateOutputError> {
    let weights = BTreeMap::from([
        (RetrieverKind::ExactLiteral, 1_000_000),
        (RetrieverKind::Lexical, profile.lexical_weight_ppm),
        (RetrieverKind::Graph, profile.graph_weight_ppm),
    ]);
    let calibrations = weights
        .keys()
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
    ]
    .into_iter()
    .map(|(lane, domain)| {
        let score_domain = typed_id::<ScoreDomainId>(domain)?;
        Ok((
            score_domain.clone(),
            ScoreDomainCalibrationV1 {
                calibration_profile_id: typed_id(&format!(
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
        profile_id: typed_id::<FusionProfileId>(&format!("profile.{}", profile.profile_id))?,
        evaluation_result_anchor: typed_id::<RetrievalAnchorId>(&format!(
            "evaluation.{}",
            profile.profile_id
        ))?,
        calibrations,
        score_domain_calibrations,
        minimum_calibrated_feature_micros: BTreeMap::new(),
        weights_micros: weights,
        diversity_policy_id: typed_id::<DiversityPolicyId>("diversity.candidate.v1")?,
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

/// Exact-tier and contradiction evidence is cap-exempt in
/// `crate::retrieval::diversity`, so `per_file` bounds only the *approximate*
/// candidates one file may contribute. Raising it changes production ranking
/// for every query, so it needs its own measurement.
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

#[cfg(test)]
mod need_provenance_tests {
    use super::{
        CandidateWorkloadV1, NEED_STRATUM, validate_need_provenance_against_embedded_corpus,
        validate_workload_for_tuning,
    };
    use crate::search_quality::packaged;

    fn workload() -> CandidateWorkloadV1 {
        packaged::load_workload().expect("packaged workload")
    }

    fn refusal(workload: &CandidateWorkloadV1) -> String {
        validate_workload_for_tuning(workload)
            .expect_err("an unfit workload is refused")
            .to_string()
    }

    fn first_need(workload: &mut CandidateWorkloadV1) -> &mut super::WorkloadQueryV1 {
        workload
            .queries
            .iter_mut()
            .find(|query| query.strata.iter().any(|stratum| stratum == NEED_STRATUM))
            .expect("a natural-language need")
    }

    /// Positive control: the packaged workload passes, so every denial below
    /// is a real change and not a pre-existing failure.
    #[test]
    fn checked_in_workload_is_fit() {
        let workload = workload();

        assert_eq!(
            validate_workload_for_tuning(&workload).map_err(|error| error.to_string()),
            Ok(())
        );
        assert_eq!(workload.profile_matrix.len(), 1);
        assert_eq!(
            workload.profile_matrix[0].profile_id,
            crate::search_quality::evaluate::QUERY_BASELINE_PROFILE
        );
    }

    #[test]
    fn a_need_without_documented_provenance_is_refused() {
        let mut workload = workload();
        first_need(&mut workload).need_provenance = None;

        assert!(refusal(&workload).contains("has no documented provenance"));
    }

    #[test]
    fn a_need_without_a_relevance_judgment_is_refused() {
        let mut workload = workload();
        first_need(&mut workload).label = Some(serde_json::json!({ "anchors": [] }));

        assert!(refusal(&workload).contains("has no relevance judgment"));
    }

    #[test]
    fn a_need_citing_a_document_outside_the_corpus_is_refused() {
        let mut workload = workload();
        first_need(&mut workload)
            .need_provenance
            .as_mut()
            .expect("checked-in provenance")
            .source_document_id = "not-in-the-corpus".to_owned();

        assert!(refusal(&workload).contains("outside the corpus"));
    }

    /// A rationale alone is not provenance: the quote has to be findable in the
    /// document it claims to come from, or a need can cite anything.
    #[test]
    fn a_need_quoting_text_absent_from_its_document_is_refused() {
        let mut workload = workload();
        first_need(&mut workload)
            .need_provenance
            .as_mut()
            .expect("checked-in provenance")
            .source_quote = "text no corpus document contains".to_owned();

        let error = validate_need_provenance_against_embedded_corpus(
            &workload,
            packaged::packaged_evaluator_files(),
        )
        .expect_err("an unfounded quote is refused")
        .to_string();
        assert!(
            error.contains("quotes text that does not appear in"),
            "{error}"
        );
    }

    /// Every checked-in quote resolves in the bytes the package actually ships,
    /// so provenance is verified against the evaluated corpus rather than a
    /// working-tree copy.
    #[test]
    fn every_checked_in_need_quote_resolves_in_the_embedded_corpus() {
        assert_eq!(
            validate_need_provenance_against_embedded_corpus(
                &workload(),
                packaged::packaged_evaluator_files(),
            )
            .map_err(|error| error.to_string()),
            Ok(())
        );
    }
}
