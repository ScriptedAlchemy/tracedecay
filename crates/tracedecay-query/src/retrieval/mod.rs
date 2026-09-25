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
//! independent of the fielded lexical/BM25 lane; the graph lane expands from
//! their seeds.

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
mod stage_counters;
pub mod task_session;

pub use self::execution::{
    AdmittedGenerationContextV1, NativeCodeOccurrenceV1, NativeExactRecordV1, NativeGraphRecordV1,
    NativeLaneOutcomeV1, NativeLanePageV1, NativeLexicalRecordV1, NativeRecordReadPortV1,
    NativeSymbolRecordV1, QueryExecutionContractErrorV1,
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
pub const QUERY_LEXICAL_RETRIEVER_REVISION_V1: &str = "retriever.lexical.daemon.qualified-names.v1";
pub const QUERY_GRAPH_RETRIEVER_REVISION_V1: &str = "retriever.graph.daemon.v1";
pub const QUERY_SANITIZER_REVISION_V1: &str = "query-sanitizer.daemon.v1";
pub const QUERY_NORMALIZATION_REVISION_V1: &str = "query-normalization.daemon.v1";
pub const QUERY_EXACT_RULE_REVISION_V1: &str = "exact-rules.daemon.v1";
pub const QUERY_LEXICAL_PROFILE_REVISION_V1: &str = "lexical-profile.daemon.v1";
pub const QUERY_EXACT_SCORE_DOMAIN_V1: &str = "score.exact.daemon.v1";
pub const QUERY_LEXICAL_SCORE_DOMAIN_V1: &str = "score.lexical.daemon.v1";
pub const QUERY_GRAPH_SCORE_DOMAIN_V1: &str = "score.graph.daemon.v1";
/// Raw lexical score (BM25 micros) that calibrates to the full lexical
/// feature. Fusion counts one lexical contribution per candidate, so this
/// range is the only lexical strength utility carries: at a 1.0 ceiling every
/// hit saturated and the graph lane alone decided between lexical candidates.
/// Hits above it still order by raw score.
///
/// ponytail: one static ceiling for every corpus (about the evaluator
/// corpus's 25th-percentile top hit); BM25 grows with corpus idf, so
/// per-query normalization is the upgrade path if larger repositories
/// saturate.
pub const QUERY_LEXICAL_CALIBRATION_CEILING_MICROS_V1: u64 = 32_000_000;
/// Score domain used when the mounted core fallback policy ranks TaskSession.
///
/// It is not part of the exact/lexical/graph fusion profile, so search cursor
/// identity stays the checked-in fallback policy.
pub const QUERY_TASK_SESSION_SCORE_DOMAIN_V1: &str = "score.task_session.daemon.v1";
pub const QUERY_TASK_SESSION_CALIBRATION_V1: &str = "calibration.task_session.query-fallback";

#[cfg(test)]
mod tests;
