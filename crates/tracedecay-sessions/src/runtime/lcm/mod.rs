pub mod compression;
pub mod compression_decision;
pub mod dag;
pub mod doctor;
pub mod extraction;
pub mod gc;
pub mod hermes;
pub mod payload;
pub mod query;
pub mod raw;
mod replay_transactions;
pub mod schema;
pub mod security;
mod summarizer;
pub mod types;
pub mod util;

pub const LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT: &str = "You answer questions using expanded LCM retrieval context. Be concise, factual, and grounded in the provided context. If the context is insufficient, say so plainly.";

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
