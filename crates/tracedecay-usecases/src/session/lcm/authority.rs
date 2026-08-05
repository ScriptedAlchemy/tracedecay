//! Typed application boundary for daemon-owned lossless context memory.
//!
//! Surface adapters submit typed commands and queries here. They never receive
//! a database, snapshot, global-store handle, filesystem root, or mutable
//! session authority. The daemon retains those resources and returns only
//! payloads plus exact authorization/execution receipts.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use tracedecay_application::{
    CancellationTokenId, CapabilityGrantId, OperationReceipt, RequestContext, RequestId,
};
use tracedecay_domain::{ManifestDigest, RetrievalAnchorId};

use crate::context::CancellationToken;
use crate::session::{SessionRequestBinding, SessionRetrievalOutcome, SessionTemporalQuery};
use tracedecay_sessions::runtime::lcm::{
    LcmCompressionResponse, LcmPreflightRequest, LcmPreflightResponse, LcmSessionBoundaryRequest,
    LcmSessionBoundaryResponse, LcmStatus,
};
use tracedecay_temporal_query::TemporalKernelResult;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

pub const LCM_DAEMON_COMMAND_CAPABILITY: &str = "capability.application.lcm-daemon-command";
pub const LCM_DAEMON_COMMAND_USE_CASE: &str = "use-case.application.lcm-daemon-command";
pub const LCM_DAEMON_QUERY_CAPABILITY: &str = "capability.application.lcm-daemon-query";
pub const LCM_DAEMON_QUERY_USE_CASE: &str = "use-case.application.lcm-daemon-query";

/// One operation admitted by the daemon LCM owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LcmAuthorityOperation {
    Preflight,
    Compact,
    SessionBoundary,
    Status,
    TemporalRead,
}

/// Authenticated host protocol evidence carried to compaction admission.
///
/// This is evidence presented to the daemon's host-protocol authority, not an
/// authorization token. Only that authority can classify a Claude payload as
/// an admitted native summary. Cursor and Codex events carry pressure/boundary
/// evidence only and can never supply summary text through this contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "host", rename_all = "snake_case")]
pub enum LcmHostProtocol {
    ClaudeCodePreCompact {
        protocol_revision: String,
        event_digest: ManifestDigest,
    },
    CursorPreCompact {
        protocol_revision: String,
        event_digest: ManifestDigest,
    },
    CodexContextCompacted {
        protocol_revision: String,
        event_digest: ManifestDigest,
    },
    Other {
        provider: String,
        protocol_revision: String,
        event_digest: ManifestDigest,
    },
}

impl LcmHostProtocol {
    pub fn provider(&self) -> &str {
        match self {
            Self::ClaudeCodePreCompact { .. } => "claude",
            Self::CursorPreCompact { .. } => "cursor",
            Self::CodexContextCompacted { .. } => "codex",
            Self::Other { provider, .. } => provider,
        }
    }

    pub fn event_digest(&self) -> &ManifestDigest {
        match self {
            Self::ClaudeCodePreCompact { event_digest, .. }
            | Self::CursorPreCompact { event_digest, .. }
            | Self::CodexContextCompacted { event_digest, .. }
            | Self::Other { event_digest, .. } => event_digest,
        }
    }
}

/// Compression content presented for host-protocol admission.
///
/// There is intentionally no generic `Provided`, `Fake`, native fallback, or
/// model-summary variant. Claude's proven PreCompact protocol is the only
/// host-supplied summary route; every source anchor is re-resolved and
/// hydrated from canonical session content before the summary can commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LcmCompressionEvidence {
    PressureOnly {
        protocol: LcmHostProtocol,
    },
    ClaudeNativeSummary {
        protocol: LcmHostProtocol,
        summary_text: String,
        source_anchors: Vec<RetrievalAnchorId>,
        source_content_digest: ManifestDigest,
    },
}

impl LcmCompressionEvidence {
    pub fn protocol(&self) -> &LcmHostProtocol {
        match self {
            Self::PressureOnly { protocol } | Self::ClaudeNativeSummary { protocol, .. } => {
                protocol
            }
        }
    }
}

/// Compaction inputs shared with preflight plus daemon-only compression state.
///
/// Reusing [`LcmPreflightRequest`] keeps message/budget configuration on one
/// maintained contract while keeping the storage summarizer mode unexposed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LcmCompactionCommand {
    pub preflight: LcmPreflightRequest,
    pub focus_topic: Option<String>,
    pub expected_current_frontier_store_id: Option<i64>,
    pub evidence: LcmCompressionEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LcmStatusQuery {
    pub provider: String,
    pub session_id: Option<String>,
    pub deep: bool,
}

/// Canonical temporal query with the exact pre-resolved session binding.
///
/// Summary pages are rendered only after the temporal kernel selects anchors;
/// the daemon hydrates those selected anchors through canonical content and
/// redaction authority on the same frozen snapshot/relation generation.
#[derive(Clone, Debug)]
pub struct LcmTemporalReadRequest {
    pub binding: SessionRequestBinding,
    pub query: SessionTemporalQuery,
}

#[derive(Clone, Debug)]
pub enum LcmAuthorityRequest {
    Preflight(LcmPreflightRequest),
    Compact(LcmCompactionCommand),
    SessionBoundary(LcmSessionBoundaryRequest),
    Status(LcmStatusQuery),
    TemporalRead(LcmTemporalReadRequest),
}

impl LcmAuthorityRequest {
    pub const fn operation(&self) -> LcmAuthorityOperation {
        match self {
            Self::Preflight(_) => LcmAuthorityOperation::Preflight,
            Self::Compact(_) => LcmAuthorityOperation::Compact,
            Self::SessionBoundary(_) => LcmAuthorityOperation::SessionBoundary,
            Self::Status(_) => LcmAuthorityOperation::Status,
            Self::TemporalRead(_) => LcmAuthorityOperation::TemporalRead,
        }
    }
}

pub fn lcm_authority_operation_identity(
    operation: LcmAuthorityOperation,
) -> Result<(CapabilityId, UseCaseId), tracedecay_application::ApplicationContractError> {
    let (capability, use_case) = match operation {
        LcmAuthorityOperation::Preflight
        | LcmAuthorityOperation::Compact
        | LcmAuthorityOperation::SessionBoundary => {
            (LCM_DAEMON_COMMAND_CAPABILITY, LCM_DAEMON_COMMAND_USE_CASE)
        }
        LcmAuthorityOperation::Status | LcmAuthorityOperation::TemporalRead => {
            (LCM_DAEMON_QUERY_CAPABILITY, LCM_DAEMON_QUERY_USE_CASE)
        }
    };
    Ok((CapabilityId::new(capability)?, UseCaseId::new(use_case)?))
}

#[derive(Clone, Debug)]
pub struct LcmAuthorityInvocation {
    pub context: RequestContext,
    pub cancellation: CancellationToken,
    pub request: LcmAuthorityRequest,
}

#[derive(Clone, Debug)]
pub enum LcmAuthorityPayload {
    Preflight(LcmPreflightResponse),
    Compaction(LcmCompressionResponse),
    SessionBoundary(LcmSessionBoundaryResponse),
    Status(LcmStatus),
    TemporalRead(SessionRetrievalOutcome<TemporalKernelResult>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LcmAuthorityUnavailableReason {
    StoreAuthorityUnavailable,
    TemporalAuthorityUnavailable,
    SummaryRelationAuthorityUnavailable,
    HostProtocolUnavailable,
    HostPayloadUnavailable,
    CanonicalSourceUnavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LcmAuthorityOutcome {
    Ready,
    Denied,
    Cancelled,
    TimedOut,
    Unavailable {
        reason: LcmAuthorityUnavailableReason,
    },
    Failed {
        diagnostic: String,
    },
}

/// Exact admission and execution receipt returned for every terminal result.
#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LcmAuthorityReceipt {
    pub request_id: RequestId,
    pub operation: LcmAuthorityOperation,
    pub grant_id: CapabilityGrantId,
    pub grant_revision: u64,
    pub grant_digest: ManifestDigest,
    pub authorized_scope_digest: ManifestDigest,
    pub cancellation_token_id: CancellationTokenId,
    pub committed_state: Option<ManifestDigest>,
    pub execution: OperationReceipt,
}

#[derive(Clone, Debug)]
pub struct LcmAuthorityResponse {
    pub outcome: LcmAuthorityOutcome,
    pub receipt: LcmAuthorityReceipt,
    pub payload: Option<LcmAuthorityPayload>,
}

pub type LcmAuthorityFuture<'a> = Pin<Box<dyn Future<Output = LcmAuthorityResponse> + Send + 'a>>;

/// Sole application-facing LCM command/query port.
pub trait LcmAuthorityPort: Send + Sync {
    fn execute(&self, invocation: LcmAuthorityInvocation) -> LcmAuthorityFuture<'_>;
}
