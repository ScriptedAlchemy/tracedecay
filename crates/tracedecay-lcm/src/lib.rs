//! Lossless context memory (LCM) engine.
//!
//! Provider-neutral contracts and reducers plus the storage runtime that
//! implements them: raw transcript ingest, the summary DAG, external payload
//! authority, retrieval queries, compression, GC, and retention. This crate
//! sits below `tracedecay-sessions`; the session runtime adapts its store
//! handles onto these entry points and must never be a dependency here.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod compression;
pub mod compression_decision;
pub mod compression_policy;
pub mod contracts;
pub mod dag;
pub mod extraction;
pub mod gc;
pub mod hermes;
mod maintenance;
mod metrics;
pub mod payload;
pub mod query;
pub mod raw;
pub mod replay_transactions;
pub mod retention;
pub mod retrieval_content;
pub mod schema;
pub mod security;
mod summarizer;
pub mod summary_convergence;
#[cfg(test)]
mod summary_convergence_tests;
#[cfg(test)]
mod test_support;
pub mod types;
pub mod util;

pub const LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT: &str = "You answer questions using expanded LCM retrieval context. Be concise, factual, and grounded in the provided context. If the context is insufficient, say so plainly.";

/// Rows requested per keyset page of a whole-table LCM scan.
///
/// The `SQLite` runtime rejects any single query that materializes more than its
/// admission limit, so whole-table reads arrive as a sequence of pages that are
/// aggregated incrementally. The result stays a complete scan.
pub const LCM_SCAN_PAGE_ROWS: i64 = 512;

/// Byte budget for a keyset page that carries raw message text. Pages stop
/// short of the row budget when the text is large, so only an empty page
/// proves such a scan is complete.
pub const LCM_SCAN_PAGE_MAX_BYTES: i64 = 32 * 1024 * 1024;

pub use hermes::{LcmCompressionRequest, LcmSummarizerMode};
pub use raw::derived_text_for_index;
pub use schema::LCM_SCHEMA_VERSION;
pub use types::{
    DERIVED_TRUNCATION_MARKER, LCM_COMPRESSION_BOUNDARY_COOLDOWN_SECONDS,
    LCM_DEFAULT_FRESH_TAIL_COUNT, LCM_DEFAULT_SUMMARY_FAN_IN, LcmCompressionResponse,
    LcmConfigStatus, LcmContentRange, LcmContentSlice, LcmDagDepthStatus, LcmDagStatus,
    LcmDescribeExternalPayload, LcmDescribeRequest, LcmDescribeResponse, LcmDescribeSourceOverview,
    LcmDescribeSummaryNode, LcmDescribeTarget, LcmError, LcmExpandQueryBudget,
    LcmExpandQueryContextBlock, LcmExpandQueryMatch, LcmExpandQueryPagination,
    LcmExpandQueryRequest, LcmExpandQueryResponse, LcmExpandQuerySynthesisPrompt, LcmExpandRequest,
    LcmExpandResponse, LcmExpandSourcePagination, LcmExpandTarget, LcmExpandedSummarySource,
    LcmGcConfig, LcmGrepFilters, LcmGrepHit, LcmGrepOutcome, LcmGrepRequest, LcmGrepSort,
    LcmLifecycleState, LcmLifecycleUpdate, LcmLoadSessionMessage, LcmLoadSessionPage,
    LcmLoadSessionRequest, LcmMaintenanceDebt, LcmNoiseClassificationConfig, LcmPayloadExpansion,
    LcmPayloadGcStatus, LcmPayloadRef, LcmPreflightRequest, LcmPreflightResponse, LcmRawMessage,
    LcmRawMessageMetadata, LcmRawMessageOverview, LcmRecentSession, LcmRelationProjectionStatus,
    LcmReplayMessage, LcmReplaySummaryNode, LcmScope, LcmSessionBoundaryRequest,
    LcmSessionBoundaryResponse, LcmSessionReplayRequest, LcmSessionReplaySlice, LcmSourceRef,
    LcmStatus, LcmStorageKind, LcmStoreStatus, LcmSummaryConvergenceStatus, LcmSummaryExpansion,
    LcmSummaryNode, LcmSummaryNodeDraft, LcmSummaryNodeOverview, LcmSummaryRequest,
    LcmSummarySourceMessage, LcmSummarySourceRange, MAX_DERIVED_SNIPPET_CHARS,
    MAX_DERIVED_TEXT_CHARS,
};

pub use gc::LcmGcReport;
pub use retention::{
    LcmRetentionConfig, LcmRetentionPhaseReport, LcmRetentionReport, RetentionMode,
};

/// The LCM token-budget heuristic: whitespace-delimited words, never zero.
///
/// Every LCM budget decision is denominated in this unit — the compression
/// trigger, the replay accounting, the retrieval window, and the policy
/// reducer. It lives here, above both the contract reducers and the runtime,
/// because the four of them must agree: a heuristic that only some callers
/// adopt would let a session compress against one budget and be replayed
/// against another.
///
/// Named distinctly from the chars/4 `estimate_tokens` helpers in read-mode
/// and global-db surfaces so those cannot be imported into this budget path
/// by accident.
pub(crate) fn lcm_budget_tokens(text: &str) -> i64 {
    text.split_whitespace().count().max(1) as i64
}

/// Visible text of a JSON message: the text [`lcm_message_budget_tokens`]
/// counts, materialized for consumers that need the string itself.
///
/// The budget unit is words of user-visible content, not serialized JSON.
/// String bodies stay strings; `{ "text": ... }` objects and arrays of
/// `{ "text": ... }` parts contribute that text. Structured payloads with no
/// text parts fall through to `Value`'s compact Display so a count is still
/// produced — never a silent empty from a failed stringify.
pub(crate) fn lcm_message_visible_text(message: &Value) -> String {
    match visible_content(message) {
        VisibleContent::Empty => String::new(),
        VisibleContent::Text(text) => text.to_string(),
        VisibleContent::TextParts(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n"),
        VisibleContent::Structured(other) => other.to_string(),
    }
}

/// [`lcm_budget_tokens`] over the same visible text as
/// [`lcm_message_visible_text`], counted from the borrowed `Value` without
/// materializing that text. The parts of a text array are joined by
/// whitespace, so their word counts add; the global minimum of one still
/// applies to the whole message, not to each part.
pub(crate) fn lcm_message_budget_tokens(message: &Value) -> i64 {
    match visible_content(message) {
        VisibleContent::Empty => lcm_budget_tokens(""),
        VisibleContent::Text(text) => lcm_budget_tokens(text),
        VisibleContent::TextParts(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .map(|text| text.split_whitespace().count())
            .sum::<usize>()
            .max(1) as i64,
        VisibleContent::Structured(other) => lcm_budget_tokens(&other.to_string()),
    }
}

enum VisibleContent<'a> {
    Empty,
    Text(&'a str),
    /// A content array with at least one `{ "text": ... }` part.
    TextParts(&'a [Value]),
    Structured(&'a Value),
}

fn visible_content(message: &Value) -> VisibleContent<'_> {
    let Some(content) = message.get("content") else {
        return VisibleContent::Empty;
    };
    match content {
        Value::Null => VisibleContent::Empty,
        Value::String(text) => VisibleContent::Text(text),
        other => {
            if let Some(text) = other.get("text").and_then(Value::as_str) {
                return VisibleContent::Text(text);
            }
            if let Some(items) = other.as_array()
                && items
                    .iter()
                    .any(|item| item.get("text").and_then(Value::as_str).is_some())
            {
                return VisibleContent::TextParts(items);
            }
            VisibleContent::Structured(other)
        }
    }
}

/// Return the storage representation used by LCM raw ingest for provider
/// transcript content. This intentionally matches the active-message path:
/// strings stay strings, structured content is compact JSON.
pub fn message_storage_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    serde_json::to_string(content).unwrap_or_else(|_| content.to_string())
}

/// Semantic message filter shared by full-text and LCM retrieval. Providers
/// sometimes encode tool results with role `user`, so this is intentionally
/// stronger than the raw role filter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionMessageType {
    #[default]
    All,
    DirectUser,
    ToolResult,
}

impl SessionMessageType {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "all" => Some(Self::All),
            "direct_user" => Some(Self::DirectUser),
            "tool_result" => Some(Self::ToolResult),
            _ => None,
        }
    }

    #[hotpath::skip]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::DirectUser => "direct_user",
            Self::ToolResult => "tool_result",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionSearchScope {
    All,
    ParentsOnly,
    SubagentsOnly,
}

impl SessionSearchScope {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "all" => Some(Self::All),
            "parents_only" => Some(Self::ParentsOnly),
            "subagents_only" => Some(Self::SubagentsOnly),
            _ => None,
        }
    }

    #[hotpath::skip]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::ParentsOnly => "parents_only",
            Self::SubagentsOnly => "subagents_only",
        }
    }
}

/// Upper bound on sessions returned for one git-scope correlation query.
pub const MAX_SESSIONS_FOR_LIMIT: usize = 100;

/// Git scope narrowing for LCM retrieval, shared with the session
/// git-correlation engine. The correlation engine owns parsing and
/// normalization of raw arguments into this value type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitScopeFilter {
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub commit: Option<String>,
}

impl GitScopeFilter {
    #[hotpath::skip]
    pub const fn is_empty(&self) -> bool {
        self.branch.is_none() && self.worktree.is_none() && self.commit.is_none()
    }
}

#[cfg(test)]
mod budget_tests {
    use super::{lcm_budget_tokens, lcm_message_budget_tokens, lcm_message_visible_text};
    use serde_json::json;

    /// The borrowed counter must agree with counting the materialized visible
    /// text for every content shape, including the minimum-of-one floor.
    #[test]
    fn borrowed_budget_count_matches_materialized_visible_text() {
        let cases = [
            (json!({ "role": "user" }), 1),
            (json!({ "content": null }), 1),
            (json!({ "content": "" }), 1),
            (json!({ "content": "   \n\t " }), 1),
            (json!({ "content": "alpha beta\ngamma" }), 3),
            (json!({ "content": { "text": "" } }), 1),
            (json!({ "content": { "text": "one two" } }), 2),
            (json!({ "content": [] }), 1),
            (json!({ "content": [{ "text": "" }] }), 1),
            (json!({ "content": [{ "text": "" }, { "text": "   " }] }), 1),
            (
                json!({ "content": [{ "text": "" }, { "text": "one" }, { "kind": "image" }, { "text": "two three" }] }),
                3,
            ),
            (
                json!({ "content": [{ "text": "a b" }, { "text": "c" }] }),
                3,
            ),
            (
                json!({ "content": [{ "kind": "image", "url": "x y z" }] }),
                3,
            ),
            (
                json!({ "content": { "kind": "tool", "args": ["one two", 3] } }),
                2,
            ),
            (json!({ "content": 42 }), 1),
            (json!({ "content": true }), 1),
        ];
        for (message, expected) in cases {
            let materialized = lcm_budget_tokens(&lcm_message_visible_text(&message));
            assert_eq!(
                lcm_message_budget_tokens(&message),
                materialized,
                "borrowed count diverged for {message}"
            );
            assert_eq!(materialized, expected, "budget unit changed for {message}");
        }
    }
}
