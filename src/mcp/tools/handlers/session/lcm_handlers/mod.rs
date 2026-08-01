use serde::Serialize;
use tracedecay_domain::{
    HydrationStateV1, RetrievalGrainV1, SessionId, TemporalCoverageCountsV1, TemporalModeV1,
};
use tracedecay_sessions::lcm::contracts::{LcmDataFreshness, LcmRetrievalOutcome};

use super::lcm_args::*;
use super::lcm_compact::{
    MAX_LCM_EXPAND_QUERY_PROMPT_CHARS, MAX_LCM_EXPAND_QUERY_QUERY_CHARS,
    lcm_expand_query_tool_json, lcm_preflight_tool_json, lcm_response_handle_root, truncate_chars,
};
use super::lcm_storage::{LcmHandlerContext, LcmOpenMode, LcmStorageResolution, open_lcm_storage};
use super::live_projection::upsert_live_transcript_projection;
use super::message_search::{
    LcmDescribeServiceCommand, LcmDescribeServiceOutcome, LcmExpandServiceCommand,
    LcmExpandServiceOutcome, SessionRetrievalCommand, SessionRetrievalFilters,
    SessionRetrievalPageView, SessionRetrievalServiceOutcome, SessionRetrievalUnavailable,
    SessionTemporalMetadataView,
};
use super::*;
use crate::application::session::{
    SessionDataFreshness, SessionRetrievalScope, SessionTemporalQuery,
};
use crate::sessions::lcm::{
    LcmContentRange, LcmExpandQueryBudget, LcmExpandQueryContextBlock, LcmExpandQueryMatch,
    LcmExpandQueryPagination, LcmExpandQueryResponse, LcmExpandQuerySynthesisPrompt, LcmSourceRef,
};
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ranking::DiversityLimits;

mod expand_query;
mod expansion;
mod lifecycle;
mod retrieval;
mod shared;
mod status;

#[cfg(test)]
mod test_support;

pub(in crate::mcp::tools::handlers) use expand_query::handle_lcm_expand_query;
pub(in crate::mcp::tools::handlers) use expansion::{handle_lcm_describe, handle_lcm_expand};
pub(in crate::mcp::tools::handlers) use lifecycle::{
    handle_lcm_compress, handle_lcm_preflight, handle_lcm_session_boundary,
};
pub(in crate::mcp::tools::handlers) use retrieval::{handle_lcm_grep, handle_lcm_load_session};
pub(in crate::mcp::tools::handlers) use status::{handle_lcm_doctor, handle_lcm_status};

#[cfg(test)]
pub(super) use expand_query::synthesize_expand_query_answer;
