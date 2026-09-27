//! Canonical CLI/MCP wire contract of `tracedecay_hook_runtime`, the internal
//! operation agent-host hooks call to record session evidence in the daemon.
//!
//! Each action is exactly what a shipped host sends. Host-native payloads
//! (hook envelopes, receipts, events, transcript messages) stay JSON here and
//! are decoded by the hook domain types that own them. Presentation-only
//! transport keys such as `format` are removed before these request bodies
//! are decoded.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::HookOrchestrationAdmissionV1;
use crate::context_scout::ContextScoutAddressV1;

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum HookRuntimeSurfaceRequestV1 {
    /// Reset the project's local token counter.
    ResetCounter {},
    /// Admit one Hook V2 envelope for the bound project.
    HookV2Admit(HookV2AdmitRequestV1),
    /// Commit a Context Scout delivery receipt for guidance the last
    /// admission handed out.
    HookV2DeliveryReceipt { receipt: Value },
    /// Acknowledge delivery of an advisory feedback notice.
    HookV2FeedbackNoticeDelivery {
        envelope: Value,
        feedback_notice: Value,
    },
    /// Record an `OpenCode` `lsp.updated` event.
    OpencodeLspUpdated { event: Value },
    /// Land a host transcript in the owning session store.
    IngestTranscript(HookIngestTranscriptRequestV1),
    /// Codex `PostCompact` pressure evidence.
    CodexCompact { event_json: String },
    /// Claude `PostCompact` pressure evidence; `user_scope` routes it to the
    /// profile.
    ClaudeCompact {
        event_json: String,
        user_scope: bool,
    },
    /// Cursor `preCompact` pressure evidence.
    CursorCompact { event_json: String },
    /// Review a finished user session.
    UserReview {
        provider: String,
        session_id: Option<String>,
    },
    /// Record a Hermes terminal receipt without a project route.
    HermesReceipt { event: Value },
    /// Admit a native event without a project route into the profile's
    /// Hook V2 ledger.
    HookV2ProfileAdmit { admission: Value },
}

/// Whether a hook call records session evidence, so it needs the session
/// stores only the project's full server mounts. Resetting the local counter
/// is the one action that does not; anything else, including a request that
/// does not decode, waits for them.
pub fn hook_runtime_needs_session_stores(arguments: &serde_json::Map<String, Value>) -> bool {
    arguments.get("action").and_then(Value::as_str) != Some("reset_counter")
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HookV2AdmitRequestV1 {
    pub envelope: Value,
    /// The host's native session id; it binds only when it hashes to the
    /// envelope's protected session.
    pub native_session_id: Option<String>,
    pub native_lifecycle: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HookIngestTranscriptRequestV1 {
    pub provider: String,
    /// Land the transcript in the profile's user store instead of the
    /// project's.
    pub user_scope: bool,
    pub session_id: Option<String>,
    /// The host's raw hook event, for providers that locate the transcript
    /// from it.
    pub event_json: Option<String>,
    /// One turn the host inlined instead of a source to scan.
    pub messages: Option<Vec<Value>>,
    /// Byte budget for this pass across every scanned source.
    pub max_new_bytes: Option<u64>,
}

/// How the hook transport should treat a Hook V2 envelope.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookRuntimeDispositionV1 {
    Accepted,
    /// The published binding no longer authorizes the envelope; the host
    /// must catch up before sending again.
    CatchupRequired,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum HookRuntimeResultV1 {
    ResetCounter { reset: bool },
    HookV2Admit(HookV2AdmissionResultV1),
    HookV2DeliveryReceipt { status: ContextScoutStoreStatusV1 },
    HookV2FeedbackNoticeDelivery(HookV2NoticeDeliveryResultV1),
    OpencodeLspUpdated { status: HookRuntimeAcceptedV1 },
    IngestTranscript(Box<HookIngestTranscriptResultV1>),
    CodexCompact(HookCompactionResultV1),
    ClaudeCompact(HookCompactionResultV1),
    CursorCompact(HookCompactionResultV1),
    HermesReceipt { status: HermesReceiptStatusV1 },
    HookV2ProfileAdmit(HookV2ProfileAdmissionResultV1),
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookRuntimeAcceptedV1 {
    Accepted,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum HookV2AdmissionResultV1 {
    Accepted {
        disposition: HookRuntimeDispositionV1,
        orchestration: HookOrchestrationAdmissionV1,
        context_scout_address: Option<ContextScoutAddressV1>,
        /// The hook's `HookReadyGuidanceV1`, when admission claimed one.
        ready_guidance: Option<Value>,
        /// The advisory `AdvisoryHookLookupNoticeV1` waiting for this
        /// worktree.
        feedback_notice: Option<Value>,
        github_stack_signal_available: bool,
    },
    /// This exact envelope was already admitted; a retry may still receive
    /// guidance retained before a lost host response.
    ExactDuplicate {
        disposition: HookRuntimeDispositionV1,
        context_scout_address: Option<ContextScoutAddressV1>,
        ready_guidance: Option<Value>,
    },
    Rejected {
        disposition: HookRuntimeDispositionV1,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<HookV2RejectionReasonV1>,
    },
    /// Idempotency could not be recorded, so nothing was admitted.
    Backpressured {},
    Unavailable {},
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookV2RejectionReasonV1 {
    /// The same event identity previously carried different bytes.
    AdmissionIdentityConflict,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutStoreStatusV1 {
    Stored,
    Duplicate,
    Superseded,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum HookV2NoticeDeliveryResultV1 {
    Stored {},
    Unavailable {},
    Rejected {
        disposition: HookRuntimeDispositionV1,
    },
}

/// The admission authority's verdict on one transcript ingest.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HookIngestAdmissionV1 {
    /// The session host-admission status, in its wire spelling.
    pub status: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HookIngestTranscriptResultV1 {
    pub provider: String,
    pub user_scope: bool,
    /// False when the byte budget left work for a later pass.
    pub completed: bool,
    pub status: String,
    pub admission: HookIngestAdmissionV1,
    pub messages_upserted: u64,
    /// How emitted hook hints settled against the new project activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint_outcomes: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observations_committed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_consumed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_by_byte_cap: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_duplicates: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_advances: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_duplicates: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub records_rejected: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub records_quarantined: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projections_completed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projections_skipped: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_duplicates: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_sources: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_bytes_scanned: Option<u64>,
}

/// A compaction hook's outcome. Variants are distinguished by the fields
/// they carry.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum HookCompactionResultV1 {
    /// The LCM authority settled the compaction.
    Settled {
        status: String,
        reason: String,
        summary_nodes_created: u64,
        summary_node_ids: Vec<String>,
        relation_projection_status: Value,
        retry_status: Option<String>,
        authority_outcome: Value,
        committed_state: Value,
        messages_upserted: u64,
    },
    /// The LCM authority answered without a compaction.
    Refused {
        status: String,
        reason: String,
        authority_outcome: Value,
        committed_state: Value,
        summary_nodes_created: u64,
        summary_node_ids: Vec<String>,
        messages_upserted: u64,
    },
    /// No compaction ran: the authority is absent, the host supplied nothing
    /// to compact, or its summaries have no provenance.
    NotRun {
        status: String,
        reason: String,
        summary_nodes_created: u64,
        summary_node_ids: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relation_projection_status: Option<Value>,
    },
    /// The host event named no session to compact.
    NoSession {
        status: String,
        reason: String,
        messages_upserted: u64,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HermesReceiptStatusV1 {
    /// A terminal receipt was recorded.
    Recorded,
    /// The turn was marked ingested and no receipt awaits review.
    Ingested,
    /// A receipt awaits a transcript that has not landed yet.
    AwaitingTranscript,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum HookV2ProfileAdmissionResultV1 {
    Accepted {
        disposition: HookRuntimeDispositionV1,
    },
    ExactDuplicate {
        disposition: HookRuntimeDispositionV1,
    },
    Rejected {
        disposition: HookRuntimeDispositionV1,
    },
    Unavailable {},
}

#[cfg(test)]
mod tests;
