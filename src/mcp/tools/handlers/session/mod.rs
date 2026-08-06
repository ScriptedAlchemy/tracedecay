use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value, json};

use super::super::render::{self, Md, truncated_json_envelope_with_handle};
use super::support::{argument_error, string_arg, tool_json, tool_json_with_md};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::{ParseOffset, RegisteredGlobalDb, TranscriptBatch};
use crate::mcp::response_handles::{
    RESPONSE_RETRIEVE_TOOL, observe_response_truncation, store_response_handle,
};
use crate::mcp::tools::{MAX_RESPONSE_CHARS, ToolResult};
use crate::sessions::git_correlation::{
    CommitRelationFilter, GitRefFilter, GitScopeFilter, SessionsForQuery,
};
use crate::sessions::lcm::compression_decision::{self, AssemblyCapInput};
use crate::sessions::lcm::{
    LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT, LcmCleanConfig, LcmCompressionRequest,
    LcmContentSlice, LcmDescribeTarget, LcmExpandQueryRequest, LcmExpandTarget, LcmGcConfig,
    LcmGrepSort, LcmPreflightRequest, LcmScope, LcmSessionBoundaryRequest, LcmSummarizerMode,
};
use crate::sessions::shared::{content_storage_text_and_tools, preview_title};
use crate::sessions::{
    ProviderScope, SessionMessageRecord, SessionMessageSearchResult, SessionMessageType,
    SessionRecord, SessionSearchScope, SessionSearchTimeRange,
};
use crate::timeutil::SearchTimeBound;
use crate::tracedecay::{TraceDecay, current_timestamp};

mod lcm_args;
mod lcm_compact;
mod lcm_handlers;
mod lcm_storage;
mod live_projection;
pub(crate) mod message_search;
mod session_refresh;
mod sessions_for;

pub(super) use lcm_handlers::{
    handle_lcm_compress, handle_lcm_describe, handle_lcm_doctor, handle_lcm_expand,
    handle_lcm_expand_query, handle_lcm_grep, handle_lcm_load_session, handle_lcm_preflight,
    handle_lcm_session_boundary, handle_lcm_status,
};
pub(super) use lcm_storage::LcmHandlerContext;
pub(crate) use session_refresh::{
    SessionRefreshAction, SessionRefreshCommand, SessionRefreshCoverageView,
    SessionRefreshFrontierView, SessionRefreshProgressView, SessionRefreshReceiptView,
    SessionRefreshServiceOutcome, SessionRefreshServicePort, SessionRefreshServices,
    handle_session_refresh, utc_micros_value,
};
pub(super) use sessions_for::handle_sessions_for;

#[cfg(test)]
use lcm_compact::{
    CompactTier, compact_lcm_expand_query_payload, lcm_expand_query_tool_json,
    lcm_preflight_tool_json,
};
#[cfg(test)]
use lcm_handlers::synthesize_expand_query_answer;
#[cfg(test)]
use message_search::parse_message_search_request;
#[cfg(test)]
use sessions_for::{message_text_snippet, render_message_search_md};

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;

#[cfg(test)]
#[path = "session_refresh/tests.rs"]
mod session_refresh_tests;
