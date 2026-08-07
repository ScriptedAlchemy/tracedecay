//! Code retrieval query port contracts.
//!
//! This module tree composes the generic retrieval kernel owned by
//! `tracedecay_domain::retrieval`. It contains typed port traits and
//! lane-local request/evidence contracts only: no storage, no transport, no
//! policy, no ranking implementation. Root store/projector adapters implement
//! the read ports; lane adapters implement the lane retrievers; the
//! composition stages implement fusion, dedupe, diversity, and late
//! hydration.
//!
//! Foreground retrieval is explicitly single-root. The exact lane is
//! independent of the fielded lexical/BM25 lane. Semantic is an optional,
//! independently admitted augmentation.

pub mod dedupe;
pub mod diversity;
pub mod evidence_lanes;
pub mod exact;
pub mod execution;
pub mod fusion;
pub mod graph;
pub mod hydrate;
pub mod lexical;
pub mod observation;
mod ordering;
pub mod ports;
pub mod prepared_query;
pub mod query_authority;
pub mod request;
pub mod rerank;
pub mod semantic;

pub use self::execution::{
    AdmittedGenerationContextV1, NativeCodeOccurrenceV1, NativeExactRecordV1, NativeGraphRecordV1,
    NativeLaneOutcomeV1, NativeLanePageV1, NativeLexicalRecordV1, NativeRecordReadPortV1,
    NativeSemanticRecordV1, NativeSymbolRecordV1, QueryExecutionContractErrorV1,
};
pub use self::observation::{
    ContextUseOutcomeV1, ObservedWithCoverageV1, RetrievalPipelineObservationV1,
    observe_composition, observe_context_outcome,
};
pub use self::ports::{
    ExactTermPostingReadPort, GraphEvidenceReadPort, LexicalPostingReadPort, RetrievalPortError,
};
pub use self::prepared_query::{
    PreparedQueryBindingsV1, PreparedQueryCursorRoutingV1, PreparedQueryErrorV1,
    PreparedQueryPageV1, PreparedQueryRoutingBindingsV1, PreparedQueryV1,
    authenticate_prepared_query_cursor_for_routing, route_authenticated_prepared_query_cursor,
};
pub use self::query_authority::{
    AuthorizedFederatedRetrievalV1, AuthorizedQueryFallbackV1, QUERY_CURSOR_TTL_MICROS_V1,
    QUERY_RANKING_REVISION_V1, QueryAuthorityErrorV1, QueryAuthorityV1,
};
pub use self::request::{RawRetrievalRequestV1, SanitizedRetrievalRequestV1};

pub const QUERY_EXACT_RETRIEVER_REVISION_V1: &str = "retriever.exact.daemon.v1";
pub const QUERY_LEXICAL_RETRIEVER_REVISION_V1: &str = "retriever.lexical.daemon.v1";
pub const QUERY_GRAPH_RETRIEVER_REVISION_V1: &str = "retriever.graph.daemon.v1";
pub const QUERY_SANITIZER_REVISION_V1: &str = "query-sanitizer.daemon.v1";
pub const QUERY_NORMALIZATION_REVISION_V1: &str = "query-normalization.daemon.v1";
pub const QUERY_EXACT_RULE_REVISION_V1: &str = "exact-rules.daemon.v1";
pub const QUERY_LEXICAL_PROFILE_REVISION_V1: &str = "lexical-profile.daemon.v1";
pub const QUERY_EXACT_SCORE_DOMAIN_V1: &str = "score.exact.daemon.v1";
pub const QUERY_LEXICAL_SCORE_DOMAIN_V1: &str = "score.lexical.daemon.v1";
pub const QUERY_GRAPH_SCORE_DOMAIN_V1: &str = "score.graph.daemon.v1";

#[cfg(test)]
mod tests;
