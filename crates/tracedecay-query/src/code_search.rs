//! Code-index search boundary contracts.
//!
//! These are the pure request/outcome value types exchanged across the
//! MCP/daemon code-index search boundary. They carry no transport, storage, or
//! policy behavior: the daemon-owned executor authenticates the admission
//! envelope and produces the terminal outcome, while the MCP tool layer only
//! renders it. Keeping the family in the query kernel lets both sides depend on
//! the retrieval crate instead of on each other.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Search policy crossing the MCP/daemon boundary. The daemon owns profile,
/// generation, query-MAC, and semantic calibration authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeIndexSearchModeV1 {
    FallbackAllowed,
    StrictSemantic,
}

/// Existing route admission required before MCP may invoke retrieval.
/// Neither value may be derived from paths, profile labels, or query bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeIndexSearchAuthorityV1 {
    pub principal: tracedecay_domain::PrincipalId,
    pub authorization_revision: tracedecay_domain::AuthorizationRevision,
}

#[derive(Clone, Debug)]
pub struct CodeIndexSearchRequestV1 {
    pub project_root: PathBuf,
    pub query: String,
    pub limit: usize,
    pub cursor: Option<tracedecay_domain::RetrievalCursor>,
    pub mode: CodeIndexSearchModeV1,
    /// MCP→executor admission envelope. The type-erased
    /// [`CodeIndexSearchExecutor`] authenticates this value; keep it on the
    /// request even when local analysis cannot see through `Arc<dyn Fn…>`.
    pub authority: Option<CodeIndexSearchAuthorityV1>,
    pub deadline: Option<tracedecay_application::Deadline>,
    pub cancellation: Option<tracedecay_application::CancellationSignal>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeIndexSemanticStatusV1 {
    Complete,
    Unavailable { reason: &'static str },
}

/// Internal scheduler probe used by the daemon search executor while semantic
/// calibration remains unavailable. This is status data, not a second MCP
/// callback surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeIndexSemanticAbstentionV1 {
    pub code_generation: Option<String>,
    pub reason: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeIndexSearchUnavailableReasonV1 {
    CapabilityUnavailable,
    AuthorityUnavailable,
    Cancelled,
    TimedOut,
    CapacityUnavailable,
    GenerationUnavailable,
    SemanticUnavailable,
    InvalidRequest,
    Internal,
}

impl CodeIndexSearchUnavailableReasonV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CapabilityUnavailable => "code_index_unavailable",
            Self::AuthorityUnavailable => "authority_unavailable",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::CapacityUnavailable => "search_capacity_unavailable",
            Self::GenerationUnavailable => "generation_unavailable",
            Self::SemanticUnavailable => "semantic_unavailable",
            Self::InvalidRequest => "invalid_request",
            Self::Internal => "search_failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeIndexSearchDisplayV1 {
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeIndexSearchCompletedV1 {
    pub code_generation: String,
    /// Visible result page: canonical bytes when semantic abstains, separately
    /// recomposed accepted-profile candidates when semantic augments.
    pub ordered_candidates: Vec<tracedecay_domain::RankedCandidate>,
    /// Exact canonical object produced under the mounted query authority.
    /// Optional semantic work may report status but cannot mutate these bytes.
    pub query_fallback: Arc<tracedecay_domain::QueryFallbackSubpayload>,
    /// Authorized generation-bound display metadata, kept outside the
    /// canonical bytes so presentation cannot mutate ranking identity.
    pub display_by_anchor: HashMap<tracedecay_domain::RetrievalAnchorId, CodeIndexSearchDisplayV1>,
    pub semantic: CodeIndexSemanticStatusV1,
    pub next_cursor: Option<tracedecay_domain::RetrievalCursor>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeIndexSearchUnavailableV1 {
    pub code_generation: Option<String>,
    pub reason: CodeIndexSearchUnavailableReasonV1,
    pub semantic: CodeIndexSemanticStatusV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)] // typed search terminal states; avoid heap on Unavailable
pub enum CodeIndexSearchOutcomeV1 {
    Complete(CodeIndexSearchCompletedV1),
    Unavailable(CodeIndexSearchUnavailableV1),
}

pub type CodeIndexSearchFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = CodeIndexSearchOutcomeV1> + Send + 'static>>;

/// Type-erased production search bridge. Direct servers leave it absent and
/// fail capability-closed instead of substituting the legacy graph search.
pub type CodeIndexSearchExecutor =
    Arc<dyn Fn(CodeIndexSearchRequestV1) -> CodeIndexSearchFuture + Send + Sync + 'static>;
