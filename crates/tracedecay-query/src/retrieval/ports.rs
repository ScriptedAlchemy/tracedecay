//! Generic retrieval port surface.
//!
//! Read-only ports are implemented by root store/projector adapters, while
//! lanes compose the shared `Retriever<R, E>` domain port.
//!
//! No SQL, no transport, no policy imports. Ports are synchronous contracts;
//! scheduling and cancellation are application-layer concerns.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkId, ExactTechnicalTermKindV1, FileOccurrenceId,
    LanguageDescriptorRevision, RetrievalAnchorId, RetrieverBatch, RetrieverOutcome,
    SourceOccurrenceId, SymbolOccurrenceId,
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
