use std::path::Path;

use serde_json::{Map, Value, json};

use super::super::render::{self, Md, truncated_json_envelope_with_handle};
use super::support::{argument_error, string_arg, tool_json, tool_json_with_md};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::mcp::response_handles::{
    RESPONSE_RETRIEVE_TOOL, observe_response_truncation, store_response_handle,
};
use crate::mcp::tools::{MAX_RESPONSE_CHARS, ToolResult};
use crate::timeutil::SearchTimeBound;
use crate::tracedecay::{TraceDecay, current_timestamp};
use tracedecay_sessions::runtime::git_correlation::{
    CommitRelationFilter, GitRefFilter, GitScopeFilter, SessionsForQuery,
};
use tracedecay_sessions::runtime::lcm::{
    LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT, LcmContentSlice, LcmDescribeTarget,
    LcmExpandQueryRequest, LcmExpandTarget, LcmGrepSort, LcmScope,
};
use tracedecay_sessions::runtime::{
    ProviderScope, SessionMessageSearchResult, SessionMessageType, SessionSearchScope,
    SessionSearchTimeRange,
};

mod lcm_args;
mod lcm_compact;
mod lcm_handlers;
mod lcm_storage;
pub(crate) mod message_search;
mod session_refresh;
mod sessions_for;

pub(super) use lcm_handlers::{
    handle_lcm_describe, handle_lcm_doctor, handle_lcm_expand, handle_lcm_expand_query,
    handle_lcm_grep, handle_lcm_load_session, handle_lcm_status,
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
use lcm_compact::{CompactTier, compact_lcm_expand_query_payload, lcm_expand_query_tool_json};
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
