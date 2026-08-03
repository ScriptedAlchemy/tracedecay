//! MCP message-search translation and rendering adapter.
//!
//! Canonical temporal retrieval, authorization, pagination, and payload
//! hydration are owned by `crate::mcp::server::session_retrieval`.

#[path = "message_search/adapter.rs"]
mod adapter;
#[path = "message_search/contract.rs"]
mod contract;

pub(crate) use adapter::handle_message_search_with_service;
#[cfg(test)]
pub(crate) use adapter::{parse_message_search_request, render_temporal_message_search_md};
pub(crate) use contract::{
    LcmDescribeServiceCommand, LcmDescribeServiceFuture, LcmDescribeServiceOutcome,
    LcmExpandServiceCommand, LcmExpandServiceFuture, LcmExpandServiceOutcome,
    SessionRetrievalCommand, SessionRetrievalExplanationView, SessionRetrievalFilters,
    SessionRetrievalOmissionView, SessionRetrievalPageView, SessionRetrievalServiceFuture,
    SessionRetrievalServiceOutcome, SessionRetrievalServicePort, SessionRetrievalStoreScope,
    SessionRetrievalUnavailable, SessionRetrievalUnavailableReason, SessionRetrievalWorkerBlocker,
    SessionRetrievalWorkerRetryClass, SessionRetrievalWorkerStatusView,
    SessionTemporalMetadataView, SessionTemporalWatermarksView,
};

#[cfg(test)]
#[path = "message_search/tests.rs"]
mod tests;
