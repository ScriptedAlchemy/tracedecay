//! Generic retrieval port surface.
//!
//! Read-only ports are implemented by root store/projector adapters, while
//! each lane exposes its own typed retriever trait.
//!
//! No SQL, no transport, no policy imports. Ports are synchronous contracts;
//! scheduling and cancellation are application-layer concerns.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkId, CompactCandidate, CursorPayloadDigest,
    ExactTechnicalTermKindV1, FileOccurrenceId, LanguageDescriptorRevision, RetrievalAnchorId,
    RetrievalBudget, RetrievalError, RetrieverBatch, RetrieverKind, RetrieverOutcome,
    SourceOccurrenceId, SymbolOccurrenceId, canonical_sha256,
};

use super::exact::{ExactLaneEvidence, ExactLaneRequest};
use super::graph::{GraphLaneEvidence, GraphLaneRequest};
use super::lexical::{LexicalLaneEvidence, LexicalLaneRequest};

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

impl RetrievalPortError {
    /// True when this failure is a deterministic contract violation that the
    /// same input reproduces on every pass — a wrong filesystem mode, a
    /// symlinked or non-directory store path, a corrupt identity. Background
    /// workers park these visibly instead of masking them as warming, unlike
    /// transient capacity, availability, staleness, and cancellation failures
    /// that a later pass can clear on its own.
    pub fn is_deterministic_contract(&self) -> bool {
        matches!(self, Self::Contract(_))
    }
}

impl From<RetrievalPortError> for RetrievalError {
    fn from(error: RetrievalPortError) -> Self {
        match error {
            RetrievalPortError::CapabilityManifestRejected => Self::CapabilityManifestRejected,
            RetrievalPortError::GenerationMismatch => Self::GenerationMismatch,
            RetrievalPortError::AuthorityUnavailable(detail) => Self::AuthorityUnavailable(detail),
            RetrievalPortError::IncompatibleProjection => Self::IncompatibleProjection,
            RetrievalPortError::StaleEvidence => Self::StaleEvidence,
            RetrievalPortError::Cancelled => Self::Cancelled,
            RetrievalPortError::BudgetExceeded => Self::BudgetExceeded,
            RetrievalPortError::Contract(detail) => Self::LaneContract(detail),
        }
    }
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
/// the domain `canonical_sha256` stream (canonical JSON into the hasher,
/// then the `sha256:<hex>` spelling a cursor payload digest accepts). Only
/// the payload differs between lanes, so only the payload is a lane's
/// business. Cursor bytes are ephemeral; live cursors minted before this
/// encoding are rejected as a set mismatch.
pub(crate) fn checkpoint_digest<T>(payload: &T) -> Result<CursorPayloadDigest, RetrievalPortError>
where
    T: Serialize,
{
    let digest = canonical_sha256(payload).map_err(contract_error)?;
    CursorPayloadDigest::new(digest.as_str()).map_err(contract_error)
}

/// How many candidates one lane may commit: the tighter of its own budget and
/// the shared request budget.
///
/// A lane budget can only narrow the request budget, never widen it, so the
/// two are always read together.
pub(crate) fn lane_candidate_cap(lane: &RetrievalBudget, base: &RetrievalBudget) -> usize {
    lane.max_candidates_per_lane
        .min(base.max_candidates_per_lane) as usize
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
        control: std::sync::Arc<dyn super::graph::GraphExecutionControl>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<GraphLaneEvidence>>, RetrievalPortError>;
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

#[cfg(test)]
mod tests {
    use super::{RetrievalError, RetrievalPortError};

    #[test]
    fn retrieval_port_error_preserves_identity_in_retrieval_error() {
        let cases = [
            (
                RetrievalPortError::CapabilityManifestRejected,
                RetrievalError::CapabilityManifestRejected,
            ),
            (
                RetrievalPortError::GenerationMismatch,
                RetrievalError::GenerationMismatch,
            ),
            (
                RetrievalPortError::AuthorityUnavailable("index offline".to_owned()),
                RetrievalError::AuthorityUnavailable("index offline".to_owned()),
            ),
            (
                RetrievalPortError::IncompatibleProjection,
                RetrievalError::IncompatibleProjection,
            ),
            (
                RetrievalPortError::StaleEvidence,
                RetrievalError::StaleEvidence,
            ),
            (RetrievalPortError::Cancelled, RetrievalError::Cancelled),
            (
                RetrievalPortError::BudgetExceeded,
                RetrievalError::BudgetExceeded,
            ),
            (
                RetrievalPortError::Contract("row binding".to_owned()),
                RetrievalError::LaneContract("row binding".to_owned()),
            ),
        ];
        for (port, expected) in cases {
            assert_eq!(RetrievalError::from(port), expected);
        }
    }
}
