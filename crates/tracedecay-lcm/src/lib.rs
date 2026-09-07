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

/// Visible text that feeds [`lcm_budget_tokens`] for a JSON message.
///
/// The budget unit is words of user-visible content, not serialized JSON.
/// String bodies stay strings; `{ "text": ... }` objects and arrays of
/// `{ "text": ... }` parts contribute that text. Structured payloads with no
/// text parts fall through to `Value`'s compact Display so a count is still
/// produced — never a silent empty from a failed stringify.
pub(crate) fn lcm_message_visible_text(message: &Value) -> String {
    let Some(content) = message.get("content") else {
        return String::new();
    };
    match content {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => {
            if let Some(text) = other.get("text").and_then(Value::as_str) {
                return text.to_string();
            }
            if let Some(items) = other.as_array() {
                let texts = items
                    .iter()
                    .filter_map(|item| item.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>();
                if !texts.is_empty() {
                    return texts.join("\n\n");
                }
            }
            other.to_string()
        }
    }
}

/// [`lcm_budget_tokens`] over [`lcm_message_visible_text`].
pub(crate) fn lcm_message_budget_tokens(message: &Value) -> i64 {
    lcm_budget_tokens(&lcm_message_visible_text(message))
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

    #[test]
    fn object_with_text_exposes_visible_words_not_json_keys() {
        let message = json!({
            "content": {
                "extra": "ignored key words",
                "text": "one",
            }
        });
        assert_eq!(lcm_message_visible_text(&message), "one");
        assert_eq!(
            lcm_message_budget_tokens(&message),
            lcm_budget_tokens("one")
        );
        assert_eq!(lcm_message_budget_tokens(&message), 1);
    }

    #[test]
    fn array_of_text_parts_joins_visible_words() {
        let message = json!({
            "content": [
                { "extra": "ignored key words", "text": "one" },
                { "text": "two three" },
            ]
        });
        assert_eq!(lcm_message_visible_text(&message), "one\n\ntwo three");
        assert_eq!(
            lcm_message_budget_tokens(&message),
            lcm_budget_tokens("one\n\ntwo three")
        );
        assert_eq!(lcm_message_budget_tokens(&message), 3);
    }
}
