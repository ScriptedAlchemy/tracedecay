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

/// Stable machine tokens for why one retrieval lane could not serve.
///
/// They are `&'static str` for the same reason [`CodeIndexSemanticStatusV1`]
/// is: a lane reason is a closed vocabulary the daemon emits and the MCP layer
/// renders, never free-form text derived from a query or a path.
pub mod lane_reason {
    /// A newer code-index generation is being built and the current one is not
    /// yet admitted. A previously published generation may still be servable.
    pub const GENERATION_REBUILDING: &str = "generation_rebuilding";
    /// No complete code-index generation exists at all for this scope.
    pub const GENERATION_UNAVAILABLE: &str = "generation_unavailable";
    /// The lane exists but this request was denied, cancelled, or timed out.
    pub const REQUEST_TERMINATED: &str = "request_terminated";
}

/// Per-lane serving status for one search response.
///
/// Serving never couples to indexing recency: a lane that can answer from an
/// older complete generation reports [`Self::Stale`] and still returns
/// results, rather than collapsing the whole query into a failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeIndexLaneStatusV1 {
    /// The lane served from the current complete code generation.
    Complete,
    /// The lane served from an older complete generation while a newer one is
    /// still being built. Recall is sound for that generation; freshness is
    /// not, and the caller is told which generation answered.
    Stale { generation: String },
    /// The lane could not run at all for this request.
    Unavailable { reason: &'static str },
}

impl CodeIndexLaneStatusV1 {
    /// Whether this lane contributed results to the response.
    pub const fn is_servable(&self) -> bool {
        matches!(self, Self::Complete | Self::Stale { .. })
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Stale { .. } => "stale",
            Self::Unavailable { .. } => "unavailable",
        }
    }
}

/// Explicit recall marker carried by every search outcome.
///
/// Agents cannot distinguish "no matches" from "the lane that would have
/// matched was not running" unless the response says so. This is that
/// statement, and it is populated on the success path as well as the failure
/// path so a degraded answer is never mistaken for a complete one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeIndexSearchCoverageV1 {
    pub exact: CodeIndexLaneStatusV1,
    pub lexical: CodeIndexLaneStatusV1,
    pub graph: CodeIndexLaneStatusV1,
    pub semantic: CodeIndexLaneStatusV1,
}

impl CodeIndexSearchCoverageV1 {
    /// Every lane served the current generation.
    pub const fn warm() -> Self {
        Self {
            exact: CodeIndexLaneStatusV1::Complete,
            lexical: CodeIndexLaneStatusV1::Complete,
            graph: CodeIndexLaneStatusV1::Complete,
            semantic: CodeIndexLaneStatusV1::Complete,
        }
    }

    /// The generation-bound fusion path: exact, lexical, and graph all ran
    /// against the admitted generation, and the semantic lane reports whatever
    /// the semantic runtime independently decided.
    pub fn fused(semantic: &CodeIndexSemanticStatusV1) -> Self {
        Self {
            semantic: match semantic {
                CodeIndexSemanticStatusV1::Complete => CodeIndexLaneStatusV1::Complete,
                CodeIndexSemanticStatusV1::Unavailable { reason } => {
                    CodeIndexLaneStatusV1::Unavailable { reason }
                }
            },
            ..Self::warm()
        }
    }

    /// The generation-bound lanes answered from an older complete generation.
    pub fn fused_stale(generation: &str, semantic: &CodeIndexSemanticStatusV1) -> Self {
        let stale = CodeIndexLaneStatusV1::Stale {
            generation: generation.to_owned(),
        };
        Self {
            exact: stale.clone(),
            lexical: stale.clone(),
            graph: stale,
            ..Self::fused(semantic)
        }
    }

    /// The generation-bound lanes are gone, but the retained graph store can
    /// still answer lexically. Recall is partial and says so.
    pub const fn retained_lexical_only(reason: &'static str) -> Self {
        Self {
            exact: CodeIndexLaneStatusV1::Unavailable { reason },
            lexical: CodeIndexLaneStatusV1::Complete,
            graph: CodeIndexLaneStatusV1::Complete,
            semantic: CodeIndexLaneStatusV1::Unavailable { reason },
        }
    }

    /// No lane can serve this request.
    pub const fn unavailable(reason: &'static str) -> Self {
        Self {
            exact: CodeIndexLaneStatusV1::Unavailable { reason },
            lexical: CodeIndexLaneStatusV1::Unavailable { reason },
            graph: CodeIndexLaneStatusV1::Unavailable { reason },
            semantic: CodeIndexLaneStatusV1::Unavailable { reason },
        }
    }

    pub const fn lanes(&self) -> [&CodeIndexLaneStatusV1; 4] {
        [&self.exact, &self.lexical, &self.graph, &self.semantic]
    }

    /// At least one lane produced results, so the response is worth returning.
    pub fn any_servable(&self) -> bool {
        self.lanes().iter().any(|lane| lane.is_servable())
    }

    /// Some lane is missing, so recall is partial and callers must be told.
    pub fn is_degraded(&self) -> bool {
        !self
            .lanes()
            .iter()
            .all(|lane| **lane == CodeIndexLaneStatusV1::Complete)
    }

    /// The progressive-degradation gate in one place: keep serving while any
    /// lane is ready, and surface the typed failure only when none is. It
    /// never blocks — the decision is made from already-resolved lane state.
    pub fn degraded_or_fail(
        self,
        reason: CodeIndexSearchUnavailableReasonV1,
    ) -> std::result::Result<Self, CodeIndexSearchUnavailableReasonV1> {
        if self.any_servable() {
            Ok(self)
        } else {
            Err(reason)
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
    /// Which lanes actually answered. Additive metadata only: it never
    /// participates in ranking identity, so a warm response carries the same
    /// candidates, fallback bytes, and cursor it always did.
    pub coverage: CodeIndexSearchCoverageV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeIndexSearchUnavailableV1 {
    pub code_generation: Option<String>,
    pub reason: CodeIndexSearchUnavailableReasonV1,
    pub semantic: CodeIndexSemanticStatusV1,
    /// Lane state at the point the request was abandoned. Every lane is
    /// unavailable here by construction; a response with any servable lane is
    /// a [`CodeIndexSearchOutcomeV1::Complete`] instead.
    pub coverage: CodeIndexSearchCoverageV1,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_fusion_reports_no_degradation() {
        let coverage = CodeIndexSearchCoverageV1::fused(&CodeIndexSemanticStatusV1::Complete);

        assert_eq!(coverage, CodeIndexSearchCoverageV1::warm());
        assert!(!coverage.is_degraded());
        assert!(coverage.any_servable());
        assert!(
            coverage
                .clone()
                .degraded_or_fail(CodeIndexSearchUnavailableReasonV1::Internal)
                .is_ok()
        );
    }

    #[test]
    fn a_ready_lane_keeps_serving_while_the_generation_rebuilds() {
        let stale = CodeIndexSearchCoverageV1::fused_stale(
            "generation.previous",
            &CodeIndexSemanticStatusV1::Unavailable {
                reason: "semantic_indexing",
            },
        );
        assert!(stale.any_servable());
        assert!(stale.is_degraded());
        assert_eq!(
            stale.exact,
            CodeIndexLaneStatusV1::Stale {
                generation: "generation.previous".to_owned(),
            }
        );
        assert!(stale.exact.is_servable());
        assert_eq!(
            stale.semantic,
            CodeIndexLaneStatusV1::Unavailable {
                reason: "semantic_indexing",
            }
        );

        let retained =
            CodeIndexSearchCoverageV1::retained_lexical_only(lane_reason::GENERATION_REBUILDING);
        assert!(retained.any_servable());
        assert!(retained.is_degraded());
        assert_eq!(retained.lexical, CodeIndexLaneStatusV1::Complete);
        assert_eq!(
            retained.exact,
            CodeIndexLaneStatusV1::Unavailable {
                reason: lane_reason::GENERATION_REBUILDING,
            }
        );
        assert!(
            retained
                .degraded_or_fail(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)
                .is_ok(),
            "a servable lexical lane must never collapse into a query failure"
        );
    }

    #[test]
    fn every_lane_down_fails_fast_with_the_typed_reason() {
        let coverage = CodeIndexSearchCoverageV1::unavailable(lane_reason::GENERATION_UNAVAILABLE);

        assert!(!coverage.any_servable());
        assert!(coverage.is_degraded());
        assert_eq!(
            coverage.degraded_or_fail(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable),
            Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)
        );
    }
}
