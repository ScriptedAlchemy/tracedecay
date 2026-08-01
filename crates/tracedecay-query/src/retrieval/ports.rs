//! Generic retrieval port surface.
//!
//! Read-only ports are implemented by root store/projector adapters, while
//! lanes compose the shared `Retriever<R, E>` domain port.
//!
//! No SQL, no transport, no policy imports. Ports are synchronous contracts;
//! scheduling and cancellation are application-layer concerns.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkId, CompactCandidate, CursorPayloadDigest,
    ExactTechnicalTermKindV1, FileOccurrenceId, LanguageDescriptorRevision, RetrievalAnchorId,
    RetrieverBatch, RetrieverKind, RetrieverOutcome, SourceOccurrenceId, SymbolOccurrenceId,
};

use super::exact::{ExactLaneEvidence, ExactLaneRequest};
use super::graph::{GraphLaneEvidence, GraphLaneRequest};
use super::lexical::{LexicalLaneEvidence, LexicalLaneRequest};

/// Failures of a store/projector read port or a lane adapter.
///
/// Incompatible indexes or models never trigger silent fallback.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RetrievalPortError {
    #[error(
        "the required base capability manifest is missing, incompatible, mixed-generation, or unauthorized"
    )]
    CapabilityManifestRejected,
    #[error("lane evidence generation does not match the pinned snapshot generation")]
    GenerationMismatch,
    #[error("lane authority is unavailable: {0}")]
    AuthorityUnavailable(String),
    #[error("lane projection is incompatible with the request profile")]
    IncompatibleProjection,
    #[error("the read port observed stale evidence")]
    StaleEvidence,
    #[error("the read port was cancelled")]
    Cancelled,
    #[error("the read port exceeded its bounded work budget")]
    BudgetExceeded,
    #[error("contract violation: {0}")]
    Contract(String),
}

/// Lift any displayable validation failure into `RetrievalPortError::Contract`.
///
/// Lanes reject on the *rendered* detail of the underlying contract error, so
/// this is the single conversion every `map_err` in the retrieval lanes uses.
pub(crate) fn contract_error(error: impl std::fmt::Display) -> RetrievalPortError {
    RetrievalPortError::Contract(error.to_string())
}

/// Hash one lane's already-domain-separated checkpoint payload.
///
/// Every lane commits its admitted prefix under the same construction:
/// canonical JSON of the lane's own payload tuple, SHA-256, then the
/// `sha256:<hex>` spelling a cursor payload digest accepts. Only the payload
/// differs between lanes, so only the payload is a lane's business.
pub(crate) fn checkpoint_digest<T>(payload: &T) -> Result<CursorPayloadDigest, RetrievalPortError>
where
    T: Serialize + ?Sized,
{
    let bytes = serde_json::to_vec(payload).map_err(contract_error)?;
    CursorPayloadDigest::new(format!("sha256:{}", hex::encode(Sha256::digest(&bytes))))
        .map_err(contract_error)
}

/// The `(occurrence, anchor, score)` triple lanes commit for each candidate in
/// their checkpoint prefix.
pub(crate) fn candidate_checkpoint_prefix(
    candidates: &[CompactCandidate],
) -> Vec<(&str, &str, u64)> {
    candidates
        .iter()
        .map(|candidate| {
            (
                candidate.source_occurrence_id.as_str(),
                candidate.retriever_evidence_anchor.as_str(),
                candidate.raw_score.micros(),
            )
        })
        .collect()
}

/// Lane-specific wording for the three contract violations every lane rejects
/// when it re-verifies a port-emitted batch.
pub(crate) struct LaneEvidenceRejections {
    /// The batch carried a candidate belonging to another lane.
    pub foreign_candidate: &'static str,
    /// The batch returned a candidate with no evidence for its occurrence.
    pub missing_evidence: &'static str,
    /// The evidence binding addresses a different candidate.
    pub unaddressed_binding: &'static str,
}

/// Evidence a lane can bind back to the candidate it was emitted for.
pub(crate) trait LaneBoundEvidence {
    fn binding(&self) -> &CodeCandidateBindingV1;
}

/// Resolve the evidence a batch emitted for one candidate, rejecting a
/// foreign-lane candidate, missing evidence, and a binding that addresses
/// something other than this candidate.
///
/// A lane can only trust evidence that names the candidate it arrived with;
/// this is the check every lane repeats before it applies its own admission
/// rules, so it lives once and each lane supplies only its own wording.
pub(crate) fn lane_bound_evidence<'batch, E>(
    batch: &'batch RetrieverBatch<E>,
    candidate: &CompactCandidate,
    lane: RetrieverKind,
    rejections: &LaneEvidenceRejections,
) -> Result<&'batch E, RetrievalPortError>
where
    E: LaneBoundEvidence,
{
    if candidate.retriever != lane {
        return Err(RetrievalPortError::Contract(
            rejections.foreign_candidate.to_owned(),
        ));
    }
    let evidence = batch
        .evidence_by_occurrence
        .get(&candidate.source_occurrence_id)
        .ok_or_else(|| RetrievalPortError::Contract(rejections.missing_evidence.to_owned()))?;
    let binding = evidence.binding();
    if binding.candidate_anchor != candidate.anchor_id
        || binding.source_occurrence != candidate.source_occurrence_id
    {
        return Err(RetrievalPortError::Contract(
            rejections.unaddressed_binding.to_owned(),
        ));
    }
    Ok(evidence)
}

/// Read-only port over whole-exact-term postings for one frozen code
/// generation.
///
/// The exact lane consumes only whole exact technical terms plus the central
/// `ExactAdmissionProof`; exact/lexical authority failure returns unavailable,
/// never substitution.
///
/// Implemented by a root store adapter against the lexical projection rows;
/// never by the lane itself.
pub trait ExactTermPostingReadPort {
    /// Return the committed candidate prefix for `request` against the
    /// pinned generation, or the typed outcome explaining why none exists.
    fn read_exact_postings(
        &self,
        request: &ExactLaneRequest,
    ) -> Result<RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>, RetrievalPortError>;
}

/// Read-only port over fielded lexical postings for one frozen code
/// generation.
///
/// Whole-term and language-profiled subtoken postings remain independent.
pub trait LexicalPostingReadPort {
    /// Return the committed candidate prefix for `request` against the
    /// pinned generation.
    fn read_lexical_postings(
        &self,
        request: &LexicalLaneRequest,
    ) -> Result<RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>, RetrievalPortError>;
}

/// Read-only port over generation-bound graph evidence.
///
/// The graph lane emits stable code anchors and ordered path evidence without
/// copying graph rows into a search corpus. Graph adapters expose their own
/// candidate pool and oracle recall.
pub trait GraphEvidenceReadPort {
    /// Return the committed candidate prefix for `request` against the
    /// pinned generation.
    fn read_graph_evidence(
        &self,
        request: &GraphLaneRequest,
    ) -> Result<RetrieverOutcome<RetrieverBatch<GraphLaneEvidence>>, RetrievalPortError>;
}

/// Compact-candidate lane adapter surface.
///
/// Each lane adapts its typed request and read port into shared compact
/// candidates; it never defines a second candidate, contribution, fusion,
/// cursor, or hydration hierarchy.
pub trait CompactCandidateLane<R, E> {
    /// Produce compact candidates for `request` against the pinned
    /// generation, preserving `(source_occurrence_id,
    /// retriever_evidence_anchor)` pairs exactly.
    fn candidates(
        &self,
        request: &R,
    ) -> Result<RetrieverOutcome<RetrieverBatch<E>>, RetrievalPortError>;
}

/// Typed reference used by lanes to bind candidates to exact code occurrences.
///
/// Every eligible chunk names exactly one code generation and file occurrence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeOccurrenceRefV1 {
    pub generation: CodeGenerationId,
    pub file: FileOccurrenceId,
    pub symbol: Option<SymbolOccurrenceId>,
    pub chunk: Option<CodeSearchChunkId>,
}

/// Lane adapter binding between a compact candidate and its code occurrence
/// evidence anchor.
///
/// `retriever_evidence_anchor` addresses the same evidence in the owning
/// source.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeCandidateBindingV1 {
    pub candidate_anchor: RetrievalAnchorId,
    pub occurrence: CodeOccurrenceRefV1,
    pub language_descriptor_revision: LanguageDescriptorRevision,
    pub matched_term_kinds: Vec<ExactTechnicalTermKindV1>,
    pub source_occurrence: SourceOccurrenceId,
}
