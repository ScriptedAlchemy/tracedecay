//! Canonical CLI/MCP wire contract for `tracedecay_retrieve`: one bounded
//! page of a truncated tool response cached under the project's store.
//!
//! Presentation-only transport keys such as `format` are removed before the
//! request body is decoded.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RetrieveSurfaceRequestV1 {
    /// The required `handle` argument copied exactly from a truncated MCP
    /// response envelope.
    pub handle: String,
    /// Character offset into the immutable stored response (default: 0). Use
    /// the prior page's next_offset.
    #[schemars(extend("default" = 0))]
    pub offset: Option<u64>,
    /// Maximum characters requested for this page. Values above the safe
    /// response-frame budget are clamped.
    #[schemars(range(min = 1))]
    pub max_chars: Option<u64>,
}

/// What a retrieve found under the handle.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum RetrieveResultV1 {
    Page(RetrievedPageV1),
    Missing(RetrieveHandleMissingV1),
    Expired(RetrieveHandleExpiredV1),
}

/// One page of the stored response, sized so its response frame fits the
/// response budget.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievedPageV1 {
    pub handle: String,
    /// Always `false`: a page is only served from a live record.
    pub expired: bool,
    pub original_chars: u64,
    pub total_chars: u64,
    pub offset: u64,
    /// Where the next page starts; absent on the last page.
    pub next_offset: Option<u64>,
    pub has_more: bool,
    pub created_at: i64,
    pub expires_at: i64,
    pub content: String,
}

/// No record exists under the handle in this project's store.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RetrieveHandleMissingV1 {
    pub handle: String,
    /// Always null: an absent record has no expiry to report.
    pub expired: Option<bool>,
    /// Always null: nothing is stored under the handle.
    pub content: Option<String>,
    pub reason_code: String,
    pub message: String,
    pub retryable: bool,
    pub retry_instruction: String,
}

/// The record expired and was removed.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RetrieveHandleExpiredV1 {
    pub handle: String,
    /// Always `true`.
    pub expired: bool,
    /// Always null: the expired content was removed.
    pub content: Option<String>,
    pub reason_code: String,
    pub message: String,
    pub retryable: bool,
    pub retry_instruction: String,
    pub created_at: i64,
    pub expires_at: i64,
}
