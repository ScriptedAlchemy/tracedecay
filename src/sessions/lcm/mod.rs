pub mod compression;
pub mod compression_decision;
pub mod dag;
pub mod doctor;
pub mod extraction;
pub mod gc;
pub mod hermes;
mod maintenance;
pub mod payload;
pub mod query;
pub mod raw;
pub(crate) mod render;
mod replay_transactions;
pub mod retention;
pub mod schema;
pub mod security;
mod summarizer;
pub mod types;
pub mod util;

pub const LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT: &str = "You answer questions using expanded LCM retrieval context. Be concise, factual, and grounded in the provided context. If the context is insufficient, say so plainly.";

/// Rows requested per keyset page of a whole-table LCM scan.
///
/// The SQLite runtime rejects any single query that materializes more than its
/// admission limit, so whole-table reads arrive as a sequence of pages that are
/// aggregated incrementally. The result stays a complete scan.
pub(crate) const LCM_SCAN_PAGE_ROWS: i64 = 512;

/// Byte budget for a keyset page that carries raw message text. Pages stop
/// short of the row budget when the text is large, so only an empty page
/// proves such a scan is complete.
pub(crate) const LCM_SCAN_PAGE_MAX_BYTES: i64 = 32 * 1024 * 1024;

pub use hermes::{LcmCompressionRequest, LcmSummarizerMode};
pub use raw::derived_text_for_index;
pub use schema::LCM_SCHEMA_VERSION;
pub use types::{
    DERIVED_TRUNCATION_MARKER, LCM_COMPRESSION_BOUNDARY_COOLDOWN_SECONDS,
    LCM_DEFAULT_FRESH_TAIL_COUNT, LCM_DEFAULT_SUMMARY_FAN_IN, LcmCleanConfig,
    LcmCompressionResponse, LcmConfigStatus, LcmContentRange, LcmContentSlice, LcmDagDepthStatus,
    LcmDagStatus, LcmDescribeExternalPayload, LcmDescribeRequest, LcmDescribeResponse,
    LcmDescribeSourceOverview, LcmDescribeSummaryNode, LcmDescribeTarget, LcmError,
    LcmExpandQueryBudget, LcmExpandQueryContextBlock, LcmExpandQueryMatch,
    LcmExpandQueryPagination, LcmExpandQueryRequest, LcmExpandQueryResponse,
    LcmExpandQuerySynthesisPrompt, LcmExpandRequest, LcmExpandResponse, LcmExpandSourcePagination,
    LcmExpandTarget, LcmExpandedSummarySource, LcmGcConfig, LcmGrepFilters, LcmGrepHit,
    LcmGrepOutcome, LcmGrepRequest, LcmGrepSort, LcmLifecycleState, LcmLifecycleUpdate,
    LcmLoadSessionMessage, LcmLoadSessionPage, LcmLoadSessionRequest, LcmMaintenanceDebt,
    LcmPayloadExpansion, LcmPayloadGcStatus, LcmPayloadRef, LcmPreflightRequest,
    LcmPreflightResponse, LcmRawMessage, LcmRawMessageOverview, LcmRecentSession, LcmReplayMessage,
    LcmReplaySummaryNode, LcmScope, LcmSessionBoundaryRequest, LcmSessionBoundaryResponse,
    LcmSessionReplayRequest, LcmSessionReplaySlice, LcmSourceRef, LcmStatus, LcmStorageKind,
    LcmStoreStatus, LcmSummaryExpansion, LcmSummaryNode, LcmSummaryNodeDraft,
    LcmSummaryNodeOverview, LcmSummaryRequest, LcmSummarySourceMessage, LcmSummarySourceRange,
    MAX_DERIVED_SNIPPET_CHARS, MAX_DERIVED_TEXT_CHARS,
};

pub use gc::LcmGcReport;
pub use retention::{
    LcmRetentionConfig, LcmRetentionPhaseReport, LcmRetentionReport, RetentionMode,
    run_session_retention,
};
