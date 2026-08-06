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
use tracedecay_domain::ManifestDigest;

use crate::context::CancellationToken;
use crate::session::SessionRequestBinding;
use tracedecay_sessions::runtime::lcm::{LcmPreflightRequest, LcmPreflightResponse, LcmStatus};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

pub const LCM_DAEMON_COMMAND_CAPABILITY: &str = "capability.application.lcm-daemon-command";
pub const LCM_DAEMON_COMMAND_USE_CASE: &str = "use-case.application.lcm-daemon-command";
pub const LCM_DAEMON_QUERY_CAPABILITY: &str = "capability.application.lcm-daemon-query";
pub const LCM_DAEMON_QUERY_USE_CASE: &str = "use-case.application.lcm-daemon-query";

/// One operation admitted by the daemon LCM owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LcmAuthorityOperation {
    Ingest,
    Compact,
    Status,
    Doctor,
}

/// Authenticated host protocol evidence carried to compaction admission.
///
/// Cursor and Codex expose pressure/boundary signals without raw compacted
/// content. No generic provider escape hatch is accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "host", rename_all = "snake_case")]
pub enum LcmHostProtocol {
    CursorPreCompact {
        protocol_revision: String,
        event_digest: ManifestDigest,
    },
    CodexContextCompacted {
        protocol_revision: String,
        event_digest: ManifestDigest,
    },
}

impl LcmHostProtocol {
    pub fn provider(&self) -> &str {
        match self {
            Self::CursorPreCompact { .. } => "cursor",
            Self::CodexContextCompacted { .. } => "codex",
        }
    }

    pub fn event_digest(&self) -> &ManifestDigest {
        match self {
            Self::CursorPreCompact { event_digest, .. }
            | Self::CodexContextCompacted { event_digest, .. } => event_digest,
        }
    }
}

/// Host pressure evidence presented for daemon admission.
///
/// No supported hook currently carries machine-verifiable compacted content.
/// This contract therefore cannot represent caller-provided summaries or model
/// substitutes; hosts continue transcript ingest and receive typed unavailable
/// compaction until authenticated native provenance exists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LcmCompressionEvidence {
    PressureOnly { protocol: LcmHostProtocol },
}

impl LcmCompressionEvidence {
    pub fn protocol(&self) -> &LcmHostProtocol {
        match self {
            Self::PressureOnly { protocol } => protocol,
        }
    }
}

/// Authenticated pressure signal plus the transcript state admitted atomically.
///
/// Reusing [`LcmPreflightRequest`] keeps message/budget configuration on one
/// maintained contract while keeping the storage summarizer mode unexposed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LcmCompactionCommand {
    pub preflight: LcmPreflightRequest,
    pub evidence: LcmCompressionEvidence,
}

/// One authentic completed Hermes turn admitted by the host callback bridge.
///
/// The daemon recomputes `event_digest` from the exact provider/session/message
/// payload before touching storage. This is intentionally distinct from
/// compaction: a completed turn is durable transcript content, not pressure or
/// a caller-authored summary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LcmTranscriptIngestCommand {
    pub preflight: LcmPreflightRequest,
    pub protocol_revision: String,
    pub event_digest: ManifestDigest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LcmStatusQuery {
    pub provider: String,
    pub session_id: Option<String>,
    pub deep: bool,
}

#[derive(Clone, Debug, Default)]
pub struct LcmDoctorQuery;

#[derive(Clone, Debug)]
pub enum LcmAuthorityRequest {
    Ingest(LcmTranscriptIngestCommand),
    Compact(LcmCompactionCommand),
    Status(LcmStatusQuery),
    Doctor(LcmDoctorQuery),
}

/// Exact provider/session target bound into the daemon-minted capability grant.
///
/// Store-wide health reads are the sole target without a provider or session.
/// Callers cannot independently choose this value: the mounted daemon adapter
/// derives it from the typed request and the authority verifies the match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum LcmAuthorityTarget {
    Store,
    Provider {
        provider: String,
        session_id: Option<String>,
    },
}

impl LcmAuthorityRequest {
    pub const fn operation(&self) -> LcmAuthorityOperation {
        match self {
            Self::Ingest(_) => LcmAuthorityOperation::Ingest,
            Self::Compact(_) => LcmAuthorityOperation::Compact,
            Self::Status(_) => LcmAuthorityOperation::Status,
            Self::Doctor(_) => LcmAuthorityOperation::Doctor,
        }
    }

    pub fn authority_target(&self) -> LcmAuthorityTarget {
        match self {
            Self::Ingest(command) => LcmAuthorityTarget::Provider {
                provider: command.preflight.provider.clone(),
                session_id: Some(command.preflight.session_id.clone()),
            },
            Self::Compact(command) => LcmAuthorityTarget::Provider {
                provider: command.preflight.provider.clone(),
                session_id: Some(command.preflight.session_id.clone()),
            },
            Self::Status(query) => LcmAuthorityTarget::Provider {
                provider: query.provider.clone(),
                session_id: query.session_id.clone(),
            },
            Self::Doctor(_) => LcmAuthorityTarget::Store,
        }
    }
}

pub fn lcm_authority_operation_identity(
    operation: LcmAuthorityOperation,
) -> Result<(CapabilityId, UseCaseId), tracedecay_application::ApplicationContractError> {
    let (capability, use_case) = match operation {
        LcmAuthorityOperation::Ingest | LcmAuthorityOperation::Compact => {
            (LCM_DAEMON_COMMAND_CAPABILITY, LCM_DAEMON_COMMAND_USE_CASE)
        }
        LcmAuthorityOperation::Status | LcmAuthorityOperation::Doctor => {
            (LCM_DAEMON_QUERY_CAPABILITY, LCM_DAEMON_QUERY_USE_CASE)
        }
    };
    Ok((CapabilityId::new(capability)?, UseCaseId::new(use_case)?))
}

#[derive(Clone, Debug)]
pub struct LcmAuthorityInvocation {
    pub context: RequestContext,
    pub binding: SessionRequestBinding,
    pub target: LcmAuthorityTarget,
    pub cancellation: CancellationToken,
    pub request: LcmAuthorityRequest,
}

#[derive(Clone, Debug)]
pub enum LcmAuthorityPayload {
    Ingest(LcmPreflightResponse),
    Status(LcmStatus),
    Doctor(serde_json::Value),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LcmAuthorityUnavailableReason {
    StoreAuthorityUnavailable,
    HostProtocolUnavailable,
    HostPayloadUnavailable,
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
