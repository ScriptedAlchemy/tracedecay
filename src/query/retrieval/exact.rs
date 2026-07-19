//! Independent exact-literal lane contracts (Plan 15: the exact tier is
//! non-demotable; Plan 25: `src/query/retrieval/exact.rs` consumes only whole
//! exact technical terms and a central `ExactAdmissionProof`).
//!
//! The exact lane is a true independent lane, separate from the fielded
//! lexical/BM25 lane. An approximate, graph-only, or later semantic candidate
//! cannot precede an eligible exact result.

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CodeGenerationId, ExactAdmissionProof, ExactAdmissionValidator, ExactFieldV1, RetrievalBudget,
    RetrievalRequest, RetrieverBatch, RetrieverOutcome,
};

use super::ports::{CodeCandidateBindingV1, RetrievalPortError};

/// Typed exact-lane request (Plan 15 pipeline step 2: exact technical
/// literals are parsed under a versioned exact-admission specification before
/// any lane executes).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactLaneRequest {
    pub base: RetrievalRequest,
    pub generation: CodeGenerationId,
    /// Candidate literals with their typed fields, pre-parsed by the central
    /// admission validator. The lane never re-derives exact status.
    pub literals: Vec<ExactLiteralV1>,
    pub budget: RetrievalBudget,
}

/// One pre-parsed exact literal candidate.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactLiteralV1 {
    pub field: ExactFieldV1,
    pub original_bytes: Vec<u8>,
    pub canonical_bytes: Vec<u8>,
}

/// Per-occurrence exact-lane evidence (Plan 15: exactly one typed evidence
/// value per returned `source_occurrence_id`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactLaneEvidence {
    pub binding: CodeCandidateBindingV1,
    pub matched_literals: Vec<ExactLiteralV1>,
    /// The validated admission proof minted centrally; the lane attaches it,
    /// it never constructs it.
    pub admission_proof: ExactAdmissionProof,
}

/// The exact-lane retriever contract. Implementations adapt the store-side
/// `ExactTermPostingReadPort` into `CompactCandidate` values for one frozen
/// generation (Plan 25).
pub trait ExactLaneRetriever {
    /// Retrieve the committed exact-tier candidate prefix for `request`.
    fn retrieve_exact(
        &self,
        request: &ExactLaneRequest,
    ) -> Result<RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>, RetrievalPortError>;
}

/// The sole exact-admission authority surface (Plan 15: only the central
/// exact-admission validator can mint `ExactAdmissionProof`; retrievers
/// cannot assign an exact tier).
pub trait ExactAdmissionAuthority: ExactAdmissionValidator {
    /// Parse `request.query` into typed literal candidates under the
    /// versioned admission specification, preserving original bytes and
    /// normalization provenance.
    fn parse_literals(&self, request: &RetrievalRequest) -> Vec<ExactLiteralV1>;
}
