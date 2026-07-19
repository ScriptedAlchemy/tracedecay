//! Graph lane contracts (Plan 25: `src/query/retrieval/graph.rs` emits
//! generation-bound code anchors and ordered path evidence without copying
//! graph rows into a search corpus; Plan 15: the graph adapter exposes its
//! own candidate pool and oracle recall — it does not become a lexical
//! field).
//!
//! Every graph path preserves its weakest edge authority and coverage;
//! unresolved dispatch cannot become semantic fact.

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CodeGenerationId, EdgeAuthorityV1, RelationEdgeKindV1, RetrievalBudget, RetrievalRequest,
    RetrieverBatch, RetrieverOutcome, SourceSpan,
};

use super::ports::{CodeCandidateBindingV1, RetrievalPortError};

/// Typed graph-lane request: bounded traversal from generation-matched
/// anchors (Plan 05: relation and path requests preserve edge authority and
/// weakest coverage state).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphLaneRequest {
    pub base: RetrievalRequest,
    pub generation: CodeGenerationId,
    pub seed_anchors: Vec<CodeCandidateBindingV1>,
    pub edge_kinds: Vec<RelationEdgeKindV1>,
    /// Bounded traversal depth; the profile owns the bound (Plan 15: no
    /// graph-hop cutoff without locked evaluation).
    pub max_depth: u32,
    pub budget: RetrievalBudget,
}

/// One ordered path segment of graph evidence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphPathSegmentV1 {
    pub from: CodeCandidateBindingV1,
    pub to: CodeCandidateBindingV1,
    pub edge_kind: RelationEdgeKindV1,
    pub authority: EdgeAuthorityV1,
    pub evidence_span: SourceSpan,
}

/// Per-occurrence graph-lane evidence: ordered path segments plus the
/// path's weakest edge authority (Plan 25).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphLaneEvidence {
    pub binding: CodeCandidateBindingV1,
    pub path: Vec<GraphPathSegmentV1>,
    pub weakest_authority: EdgeAuthorityV1,
}

/// The graph-lane retriever contract. Graph consumes only generation-matched
/// Plan 25 evidence (Plan 25).
pub trait GraphLaneRetriever {
    /// Retrieve the committed graph candidate prefix for `request`.
    fn retrieve_graph(
        &self,
        request: &GraphLaneRequest,
    ) -> Result<RetrieverOutcome<RetrieverBatch<GraphLaneEvidence>>, RetrievalPortError>;
}
