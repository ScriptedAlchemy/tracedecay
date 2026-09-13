//! The daemon invocation wire contract.
//!
//! These are the request, response, outcome, and problem shapes exchanged over
//! the daemon's closed post-handshake invocation protocol. They are data: no
//! socket, no admission decision, no runtime registry, no database.
//!
//! The contract lives here in `tracedecay-daemon-protocol`, outside the
//! daemon's service crate, on purpose. Its callers are the application surface
//! and the invocation client, which have no business reaching into the
//! daemon's service internals just to name a payload. Keeping the shapes here
//! means a caller depends on the protocol, not on the server that happens to
//! implement it.
//!
//! Behavior stays with the daemon. Anything that interprets a request —
//! authority minting, scope resolution, dispatch — remains in
//! `tracedecay-daemon-service`; only construction, validation, and the
//! application-DTO conversions travel with the types they belong to.

mod feedback;
mod git;
mod git_surface;
mod handoff;
mod lsp;
mod problem_response;
mod semantic;
mod work;
mod workflow;

pub use feedback::DaemonFeedbackResult;
pub use git::{DaemonGitEffectResult, DaemonGitPreviewResult};
pub use handoff::{HandoffApplicationInvocationV1, HandoffApplicationOutcomeV1};
pub use lsp::DaemonLspSessionAccess;
pub use semantic::{CanonicalQualificationBlob, CanonicalQualificationBlobError};
use serde::{Deserialize, Serialize};
use tracedecay_contracts::{
    ApplicationOutcome, ApplicationProblem, AuthorizedScopeSet, CancellationContext, Deadline,
    MultiRootExecuteRequestV1, MultiRootScopeSetCasRequestV1, MultiRootScopeSetCasResultV1,
    MultiRootScopeSetReadRequestV1, ObservatoryReadRequestV1, PageRequest, ResolvedScope,
    SourceEditInvocationV1, SourceEditReconciliationInvocationV1, SourceEditRollbackInvocationV1,
};
use tracedecay_domain::{ManifestDigest, ScopeSetId, UtcMicros};
use tracedecay_tool_catalog::ApplicationSurfaceOperation;
pub use work::{WorkApplicationInvocationV1, WorkApplicationOutcomeV1};
pub use workflow::{WorkflowApplicationInvocation, WorkflowApplicationOutcome};

use crate::lsp_wire::{MAX_LSP_FRAME_BYTES, MAX_LSP_WORKSPACE_ROOTS};
use crate::surface::GitReadSurfaceRequest;
use tracedecay_contracts::ConfigurationWireRequestV1;
use tracedecay_contracts::context_scout::ContextScoutSurfaceRequestV1;
use tracedecay_contracts::feedback::observations::{
    FeedbackDeliveryRouteV1, FeedbackSourceEventV1,
};
use tracedecay_contracts::git::GitHubStackSignalExpandSurfaceRequest;
use tracedecay_contracts::git::{GitApplySurfaceRequest, GitPreviewSurfaceRequest};
use tracedecay_contracts::retrieval::PrimitiveRequest;

/// Request-field character rules. The contract accepts opaque handles and ids
/// only in a shape it can echo back safely, so validation travels with the
/// wire types rather than with the server that reads them.
fn valid_token(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn valid_printable(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
}

fn valid_lsp_control(deadline: &Deadline, cancellation: &CancellationContext) -> bool {
    deadline.expires_at.0 > 0 && cancellation.token_id.as_str().len() <= MAX_OPAQUE_HANDLE_BYTES
}

/// Stable discriminator for the closed post-handshake invocation protocol.
pub const DAEMON_INVOCATION_PROTOCOL: &str = "tracedecay.daemon.invocation";
/// Initial revision of the daemon-owned invocation wire shape.
pub const DAEMON_INVOCATION_REVISION: u16 = 1;
/// Request method a control client sends over the closed protocol to ask the
/// daemon to shut down.
pub const DAEMON_SHUTDOWN_METHOD: &str = "tracedecay/daemon/shutdown";
const DAEMON_INVOCATION_CANCEL_OPERATION: &str = "invocation_cancel";
const DAEMON_INVOCATION_DELIVERY_ACK_OPERATION: &str = "invocation_delivery_ack";

const MAX_INVOCATION_REQUEST_ID_BYTES: usize = 128;
const MAX_CLIENT_REVISION_BYTES: usize = 128;
const MAX_ROOT_HINT_BYTES: usize = 4_096;
const MAX_OPAQUE_HANDLE_BYTES: usize = 256;

/// A separate authenticated control frame that can interrupt an in-flight
/// read without contending on that invocation's response connection.
#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonInvocationCancellationRequest {
    protocol: String,
    revision: u16,
    request_id: String,
    operation: String,
    target_request_id: String,
}

impl DaemonInvocationCancellationRequest {
    pub fn new(target_request_id: impl Into<String>) -> Self {
        let target_request_id = target_request_id.into();
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: target_request_id.clone(),
            operation: DAEMON_INVOCATION_CANCEL_OPERATION.to_owned(),
            target_request_id,
        }
    }

    pub fn target_request_id(&self) -> &str {
        &self.target_request_id
    }

    fn validate(&self) -> bool {
        self.protocol == DAEMON_INVOCATION_PROTOCOL
            && self.revision == DAEMON_INVOCATION_REVISION
            && self.operation == DAEMON_INVOCATION_CANCEL_OPERATION
            && valid_token(&self.request_id, MAX_INVOCATION_REQUEST_ID_BYTES)
            && valid_token(&self.target_request_id, MAX_INVOCATION_REQUEST_ID_BYTES)
    }
}

pub fn parse_daemon_invocation_cancellation_request(
    line: &str,
) -> Option<DaemonInvocationCancellationRequest> {
    let request = serde_json::from_str::<DaemonInvocationCancellationRequest>(line.trim()).ok()?;
    request.validate().then_some(request)
}

/// Terminal acknowledgement emitted by a surface adapter only after its own
/// response boundary has completed.  The daemon socket write is deliberately
/// not a delivery receipt: a CLI must first write and flush stdout, then send
/// this frame on the authenticated connection that carried the invocation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DaemonInvocationDeliveryAckRequest {
    protocol: String,
    revision: u16,
    request_id: String,
    operation: String,
    target_request_id: String,
    outcome: tracedecay_domain::DeliverySettlementOutcomeV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    drop_reason: Option<tracedecay_domain::DeliveryDropReasonV1>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonInvocationDeliveryAckRejectReason {
    RecorderUnavailable,
    RecorderAtCapacity,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DaemonInvocationDeliveryAckResponse {
    protocol: String,
    revision: u16,
    request_id: String,
    operation: String,
    #[serde(flatten)]
    outcome: DaemonInvocationDeliveryAckResponseOutcome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DaemonInvocationDeliveryAckResponseOutcome {
    Accepted,
    Rejected {
        reason: DaemonInvocationDeliveryAckRejectReason,
    },
}

impl DaemonInvocationDeliveryAckResponse {
    pub fn accepted(request_id: impl Into<String>) -> Self {
        Self::with_outcome(
            request_id,
            DaemonInvocationDeliveryAckResponseOutcome::Accepted,
        )
    }

    pub fn rejected(
        request_id: impl Into<String>,
        reason: DaemonInvocationDeliveryAckRejectReason,
    ) -> Self {
        Self::with_outcome(
            request_id,
            DaemonInvocationDeliveryAckResponseOutcome::Rejected { reason },
        )
    }

    fn with_outcome(
        request_id: impl Into<String>,
        outcome: DaemonInvocationDeliveryAckResponseOutcome,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            operation: DAEMON_INVOCATION_DELIVERY_ACK_OPERATION.to_owned(),
            outcome,
        }
    }

    pub fn matches_request(&self, request_id: &str) -> bool {
        self.protocol == DAEMON_INVOCATION_PROTOCOL
            && self.revision == DAEMON_INVOCATION_REVISION
            && self.operation == DAEMON_INVOCATION_DELIVERY_ACK_OPERATION
            && self.request_id == request_id
    }

    pub fn rejection_reason(&self) -> Option<DaemonInvocationDeliveryAckRejectReason> {
        match self.outcome {
            DaemonInvocationDeliveryAckResponseOutcome::Accepted => None,
            DaemonInvocationDeliveryAckResponseOutcome::Rejected { reason } => Some(reason),
        }
    }
}

impl DaemonInvocationDeliveryAckRequest {
    pub fn delivered(target_request_id: impl Into<String>) -> Self {
        let target_request_id = target_request_id.into();
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: target_request_id.clone(),
            operation: DAEMON_INVOCATION_DELIVERY_ACK_OPERATION.to_owned(),
            target_request_id,
            outcome: tracedecay_domain::DeliverySettlementOutcomeV1::Delivered,
            drop_reason: None,
        }
    }

    pub fn dropped(
        target_request_id: impl Into<String>,
        drop_reason: tracedecay_domain::DeliveryDropReasonV1,
    ) -> Self {
        let target_request_id = target_request_id.into();
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: target_request_id.clone(),
            operation: DAEMON_INVOCATION_DELIVERY_ACK_OPERATION.to_owned(),
            target_request_id,
            outcome: tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
            drop_reason: Some(drop_reason),
        }
    }

    pub fn target_request_id(&self) -> &str {
        &self.target_request_id
    }

    pub fn outcome(
        &self,
    ) -> (
        tracedecay_domain::DeliverySettlementOutcomeV1,
        Option<tracedecay_domain::DeliveryDropReasonV1>,
    ) {
        (self.outcome, self.drop_reason)
    }

    fn validate(&self) -> bool {
        self.protocol == DAEMON_INVOCATION_PROTOCOL
            && self.revision == DAEMON_INVOCATION_REVISION
            && self.operation == DAEMON_INVOCATION_DELIVERY_ACK_OPERATION
            && valid_token(&self.request_id, MAX_INVOCATION_REQUEST_ID_BYTES)
            && valid_token(&self.target_request_id, MAX_INVOCATION_REQUEST_ID_BYTES)
            && self.request_id == self.target_request_id
            && matches!(
                (self.outcome, self.drop_reason),
                (
                    tracedecay_domain::DeliverySettlementOutcomeV1::Delivered,
                    None
                ) | (
                    tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                    Some(_)
                )
            )
    }
}

pub fn parse_daemon_invocation_delivery_ack_request(
    line: &str,
) -> Option<DaemonInvocationDeliveryAckRequest> {
    let value = serde_json::from_str::<serde_json::Value>(line.trim()).ok()?;
    (value.get("protocol").and_then(serde_json::Value::as_str) == Some(DAEMON_INVOCATION_PROTOCOL)
        && value.get("operation").and_then(serde_json::Value::as_str)
            == Some(DAEMON_INVOCATION_DELIVERY_ACK_OPERATION))
    .then_some(())?;
    let request = serde_json::from_value::<DaemonInvocationDeliveryAckRequest>(value).ok()?;
    request.validate().then_some(request)
}

#[cfg(test)]
mod delivery_ack_tests {
    use super::{
        DaemonInvocationDeliveryAckRequest, DaemonInvocationDeliveryAckResponse,
        DaemonInvocationDeliveryAckResponseOutcome, parse_daemon_invocation_delivery_ack_request,
    };
    use tracedecay_domain::DeliveryDropReasonV1;

    #[test]
    fn delivered_ack_round_trips_and_rejects_a_drop_reason() {
        let ack = DaemonInvocationDeliveryAckRequest::delivered("request.cli.delivery.1");
        let wire = serde_json::to_string(&ack).expect("delivery ACK wire");
        let parsed = parse_daemon_invocation_delivery_ack_request(&wire)
            .expect("delivered ACK should parse");
        assert_eq!(parsed.target_request_id(), "request.cli.delivery.1");
        assert_eq!(
            parsed.outcome().0,
            tracedecay_domain::DeliverySettlementOutcomeV1::Delivered
        );

        let invalid = wire.replace(
            "\"outcome\":\"delivered\"",
            "\"outcome\":\"delivered\",\"drop_reason\":\"disconnected\"",
        );
        assert!(parse_daemon_invocation_delivery_ack_request(&invalid).is_none());
    }

    #[test]
    fn dropped_ack_requires_a_reason_and_response_is_typed() {
        let ack = DaemonInvocationDeliveryAckRequest::dropped(
            "request.cli.delivery.2",
            DeliveryDropReasonV1::Disconnected,
        );
        let wire = serde_json::to_string(&ack).expect("dropped ACK wire");
        assert!(parse_daemon_invocation_delivery_ack_request(&wire).is_some());

        let response = DaemonInvocationDeliveryAckResponse::rejected(
            "request.cli.delivery.2",
            super::DaemonInvocationDeliveryAckRejectReason::RecorderAtCapacity,
        );
        let value = serde_json::to_value(response).expect("ACK response wire");
        assert_eq!(value["status"], "rejected");
        assert_eq!(value["reason"], "recorder_at_capacity");
        assert!(matches!(
            serde_json::from_value::<DaemonInvocationDeliveryAckResponse>(value)
                .expect("ACK response parse")
                .outcome,
            DaemonInvocationDeliveryAckResponseOutcome::Rejected {
                reason: super::DaemonInvocationDeliveryAckRejectReason::RecorderAtCapacity
            }
        ));
    }
}

/// Closed operations accepted by the daemon invocation connection.
///
/// Git operations carry only their reviewed typed surface DTOs. Authority,
/// policy proof, actor, and scope are minted by the daemon after project
/// admission and never accepted from a client.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonInvocationOperation {
    GitStatus,
    GitDiff,
    GitHistory,
    GitBlame,
    GitHunks,
    GitPreview,
    GitApply,
    GitHubStackSignalExpand,
    NativeIntegrationStackSnapshot,
    NativeIntegrationPreflight,
    NativeIntegrationApprove,
    NativeIntegrationApply,
    NativeIntegrationStatus,
    NativeIntegrationCancel,
    NativeIntegrationWorktreeInventory,
    NativeIntegrationWorktreeInspect,
    NativeIntegrationWorktreeConfirm,
    NativeIntegrationWorktreeRemove,
    NativeIntegrationWorktreeReconcile,
    FeedbackDiagnostics,
    FeedbackGet,
    FeedbackExpand,
    FeedbackList,
    FeedbackAdvisoryCycle,
    FeedbackImpact,
    AffectedTests,
    FeedbackObserve,
    PrimitiveImpact,
    PrimitiveAffectedTests,
    PrimitiveTestResults,
    PrimitiveRead,
    CodeExactOccurrence,
    CodePhraseSearch,
    CodeCallees,
    CodeFacets,
    CodeTimeline,
    CodeDeclaration,
    CodeTypeDefinition,
    CodeReferences,
    Configuration,
    ContextScout,
    ObservatoryRead,
    RetainedApplication,
    MultiRootScopeSetRead,
    MultiRootScopeSetCompareAndSwap,
    MultiRootExecute,
    WorkApplication,
    WorkflowApplication,
    HandoffApplication,
    SemanticEvaluateAndPublish,
    SemanticActivate,
    SemanticQualify,
    LspOpen,
    LspFrame,
    LspPoll,
    LspAcknowledge,
    LspReconnect,
    LspDetach,
    SourceEdit,
    SourceEditReconcile,
    SourceEditRollback,
}

impl DaemonInvocationOperation {
    #[hotpath::skip]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GitStatus => "git_status",
            Self::GitDiff => "git_diff",
            Self::GitHistory => "git_history",
            Self::GitBlame => "git_blame",
            Self::GitHunks => "git_hunks",
            Self::GitPreview => "git_preview",
            Self::GitApply => "git_apply",
            Self::GitHubStackSignalExpand => "github_stack_signal_expand",
            Self::NativeIntegrationStackSnapshot => "stack_snapshot",
            Self::NativeIntegrationPreflight => "preflight_native_integration",
            Self::NativeIntegrationApprove => "approve_native_integration",
            Self::NativeIntegrationApply => "apply_native_integration",
            Self::NativeIntegrationStatus => "native_integration_status",
            Self::NativeIntegrationCancel => "cancel_native_integration",
            Self::NativeIntegrationWorktreeInventory => "worktree_inventory",
            Self::NativeIntegrationWorktreeInspect => "worktree_cleanup_inspect",
            Self::NativeIntegrationWorktreeConfirm => "worktree_cleanup_confirm",
            Self::NativeIntegrationWorktreeRemove => "worktree_cleanup_remove",
            Self::NativeIntegrationWorktreeReconcile => "worktree_cleanup_reconcile",
            Self::FeedbackDiagnostics => "feedback_diagnostics",
            Self::FeedbackGet => "feedback_get",
            Self::FeedbackExpand => "feedback_expand",
            Self::FeedbackList => "feedback_list",
            Self::FeedbackAdvisoryCycle => "feedback_advisory_cycle",
            Self::FeedbackImpact => "feedback_impact",
            Self::AffectedTests => "affected_tests",
            Self::FeedbackObserve => "feedback_observe",
            Self::PrimitiveImpact => "primitive_impact",
            Self::PrimitiveAffectedTests => "primitive_affected_tests",
            Self::PrimitiveTestResults => "test_results",
            Self::PrimitiveRead => "primitive_read",
            Self::CodeExactOccurrence => "code_exact_occurrence",
            Self::CodePhraseSearch => "code_phrase_search",
            Self::CodeCallees => "code_callees",
            Self::CodeFacets => "code_facets",
            Self::CodeTimeline => "code_timeline",
            Self::CodeDeclaration => "code_declaration",
            Self::CodeTypeDefinition => "code_type_definition",
            Self::CodeReferences => "code_references",
            Self::Configuration => "configuration",
            Self::ContextScout => "context_scout",
            Self::ObservatoryRead => "observatory_read",
            Self::RetainedApplication => "retained_application",
            Self::MultiRootScopeSetRead => "multi_root_scope_set_read",
            Self::MultiRootScopeSetCompareAndSwap => "multi_root_scope_set_compare_and_swap",
            Self::MultiRootExecute => "multi_root_execute",
            Self::WorkApplication => "work_application",
            Self::WorkflowApplication => "workflow_application",
            Self::HandoffApplication => "handoff_application",
            Self::SemanticEvaluateAndPublish => "semantic_evaluate_and_publish",
            Self::SemanticActivate => "semantic_activate",
            Self::SemanticQualify => "semantic_qualify",
            Self::LspOpen => "lsp_open",
            Self::LspFrame => "lsp_frame",
            Self::LspPoll => "lsp_poll",
            Self::LspAcknowledge => "lsp_acknowledge",
            Self::LspReconnect => "lsp_reconnect",
            Self::LspDetach => "lsp_detach",
            Self::SourceEdit => "source_edit",
            Self::SourceEditReconcile => "source_edit_reconcile",
            Self::SourceEditRollback => "source_edit_rollback",
        }
    }
}

/// One versioned, request-correlated daemon operation.
#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonInvocationRequest {
    pub protocol: String,
    pub revision: u16,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_route: Option<FeedbackDeliveryRouteV1>,
    #[serde(flatten)]
    pub payload: DaemonInvocationPayload,
}

/// Operation-specific fields for the closed invocation set.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum DaemonInvocationPayload {
    GitRead {
        surface_operation: ApplicationSurfaceOperation,
        request: GitReadSurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    GitPreview {
        request: GitPreviewSurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    GitApply {
        request: GitApplySurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    GitHubStackSignalExpand {
        request: GitHubStackSignalExpandSurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    NativeIntegration {
        surface_operation: ApplicationSurfaceOperation,
        request: tracedecay_contracts::NativeIntegrationSurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    FeedbackDiagnostics {
        request_handle: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    FeedbackGet {
        request_handle: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolved_scope: Option<ResolvedScope>,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    FeedbackExpand {
        request_handle: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    FeedbackList {
        request_handle: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    FeedbackAdvisoryCycle {
        document_uri: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    FeedbackImpact {
        request_handle: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    AffectedTests {
        request_handle: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    FeedbackObserve {
        subject_digest: ManifestDigest,
        observed_at: UtcMicros,
        event: FeedbackSourceEventV1,
    },
    PrimitiveImpact {
        request: tracedecay_contracts::retrieval::GraphImpactPrimitiveRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    PrimitiveAffectedTests {
        request: tracedecay_contracts::retrieval::AffectedFileTestsPrimitiveRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    PrimitiveTestResults {
        page: PageRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    PrimitiveRead {
        surface_operation: ApplicationSurfaceOperation,
        request: PrimitiveRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    PrimitiveCode {
        surface_operation: ApplicationSurfaceOperation,
        request: tracedecay_contracts::PrimitiveCodeSurfaceRequest,
        page: PageRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    CallableCode {
        surface_operation: ApplicationSurfaceOperation,
        request: tracedecay_contracts::CallableCodeSurfaceRequest,
        page: PageRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    Configuration {
        surface_operation: ApplicationSurfaceOperation,
        request: ConfigurationWireRequestV1,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolved_scope: Option<ResolvedScope>,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    ContextScout {
        surface_operation: ApplicationSurfaceOperation,
        request: ContextScoutSurfaceRequestV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    ObservatoryRead {
        request: ObservatoryReadRequestV1,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolved_scope: Option<ResolvedScope>,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    RetainedApplication {
        request: tracedecay_contracts::retained_surfaces::RetainedSurfaceRequestV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    MultiRootScopeSetRead {
        request: MultiRootScopeSetReadRequestV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    MultiRootScopeSetCompareAndSwap {
        request: MultiRootScopeSetCasRequestV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    MultiRootExecute {
        request: MultiRootExecuteRequestV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    WorkApplication {
        request: Box<WorkApplicationInvocationV1>,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    WorkflowApplication {
        request: WorkflowApplicationInvocation,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    HandoffApplication {
        request: HandoffApplicationInvocationV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    SemanticEvaluateAndPublish {
        evaluated_profile_id: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    /// One operator journey: evaluate the named profile natively, publish the
    /// accepted evaluation, and compare-and-swap it into `active_profile` of
    /// the project's semantic runtime configuration. The daemon composes the
    /// installed-model material and the configuration revision itself; the
    /// caller authors only the profile selection.
    SemanticActivate {
        evaluated_profile_id: String,
        /// Record the previously active profile as `rollback_profile` when
        /// one exists and differs from the newly activated selection.
        set_rollback: bool,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    SemanticQualify {
        evaluated_profile_id: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    LspOpen {
        client_revision: String,
        requested_root_uri: Option<String>,
        workspace_folders: Vec<String>,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    LspFrame {
        session: DaemonLspSessionAccess,
        frame: String,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    LspPoll {
        session: DaemonLspSessionAccess,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    LspAcknowledge {
        session: DaemonLspSessionAccess,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    LspReconnect {
        session: DaemonLspSessionAccess,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    LspDetach {
        session: DaemonLspSessionAccess,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    SourceEdit {
        request: SourceEditInvocationV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    SourceEditReconcile {
        request: SourceEditReconciliationInvocationV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    SourceEditRollback {
        request: SourceEditRollbackInvocationV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
}

impl DaemonInvocationRequest {
    /// One typed constructor for the whole Plan 36 native-integration journey.
    ///
    /// The transport carries exact typed identity only; it contains no Git
    /// logic and no fallback mutation path.
    pub fn native_integration(
        request_id: impl Into<String>,
        surface_operation: ApplicationSurfaceOperation,
        request: tracedecay_contracts::NativeIntegrationSurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::NativeIntegration {
                surface_operation,
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn feedback(
        request_id: impl Into<String>,
        operation: ApplicationSurfaceOperation,
        request_handle: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        let payload = match operation {
            ApplicationSurfaceOperation::FeedbackDiagnostics => {
                DaemonInvocationPayload::FeedbackDiagnostics {
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                }
            }
            ApplicationSurfaceOperation::FeedbackGet => DaemonInvocationPayload::FeedbackGet {
                request_handle,
                resolved_scope: None,
                observed_at,
                deadline,
                cancellation,
            },
            ApplicationSurfaceOperation::FeedbackExpand => {
                DaemonInvocationPayload::FeedbackExpand {
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                }
            }
            ApplicationSurfaceOperation::FeedbackList => DaemonInvocationPayload::FeedbackList {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            },
            ApplicationSurfaceOperation::FeedbackImpact => {
                DaemonInvocationPayload::FeedbackImpact {
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                }
            }
            ApplicationSurfaceOperation::AffectedTests => DaemonInvocationPayload::AffectedTests {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            },
            ApplicationSurfaceOperation::TestResults
            | ApplicationSurfaceOperation::ObservatoryRead
            | ApplicationSurfaceOperation::FeedbackAdvisoryCycle
            | ApplicationSurfaceOperation::SessionLookup
            | ApplicationSurfaceOperation::QualifiedName
            | ApplicationSurfaceOperation::CallChain
            | ApplicationSurfaceOperation::FileDependents
            | ApplicationSurfaceOperation::SourceLines
            | ApplicationSurfaceOperation::SourceBody
            | ApplicationSurfaceOperation::SourceOutline
            | ApplicationSurfaceOperation::ModuleApi
            | ApplicationSurfaceOperation::HealthRead
            | ApplicationSurfaceOperation::HealthDelta
            | ApplicationSurfaceOperation::StorageStatus
            | ApplicationSurfaceOperation::DiagnosticsRead
            | ApplicationSurfaceOperation::CodeSymbolSearch
            | ApplicationSurfaceOperation::CodeSignatureSearch
            | ApplicationSurfaceOperation::CodeImplementations
            | ApplicationSurfaceOperation::CodeTypeHierarchy
            | ApplicationSurfaceOperation::CodeCallers => {
                unreachable!("primitive operations use their typed constructor")
            }
            ApplicationSurfaceOperation::CodeExactOccurrence
            | ApplicationSurfaceOperation::CodePhraseSearch
            | ApplicationSurfaceOperation::CodeCallees
            | ApplicationSurfaceOperation::CodeFacets
            | ApplicationSurfaceOperation::CodeTimeline
            | ApplicationSurfaceOperation::CodeDeclaration
            | ApplicationSurfaceOperation::CodeTypeDefinition
            | ApplicationSurfaceOperation::CodeReferences => {
                unreachable!("callable code operations use their typed constructor")
            }
            ApplicationSurfaceOperation::GitStatus
            | ApplicationSurfaceOperation::GitDiff
            | ApplicationSurfaceOperation::GitHistory
            | ApplicationSurfaceOperation::GitBlame
            | ApplicationSurfaceOperation::GitHunks
            | ApplicationSurfaceOperation::GitPreview
            | ApplicationSurfaceOperation::GitApply
            | ApplicationSurfaceOperation::GitHubStackSignalExpand => {
                unreachable!("Git operations use their typed constructors")
            }
            ApplicationSurfaceOperation::NativeIntegrationStackSnapshot
            | ApplicationSurfaceOperation::NativeIntegrationPreflight
            | ApplicationSurfaceOperation::NativeIntegrationApprove
            | ApplicationSurfaceOperation::NativeIntegrationApply
            | ApplicationSurfaceOperation::NativeIntegrationStatus
            | ApplicationSurfaceOperation::NativeIntegrationCancel => {
                unreachable!("native-integration operations use their typed constructor")
            }
            ApplicationSurfaceOperation::NativeIntegrationWorktreeInventory
            | ApplicationSurfaceOperation::NativeIntegrationWorktreeInspect
            | ApplicationSurfaceOperation::NativeIntegrationWorktreeConfirm
            | ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove
            | ApplicationSurfaceOperation::NativeIntegrationWorktreeReconcile => {
                unreachable!("native worktree operations use their typed constructor")
            }
            ApplicationSurfaceOperation::ConfigurationList
            | ApplicationSurfaceOperation::ConfigurationGet
            | ApplicationSurfaceOperation::ConfigurationSet
            | ApplicationSurfaceOperation::ConfigurationUnset
            | ApplicationSurfaceOperation::ConfigurationBatch
            | ApplicationSurfaceOperation::ConfigurationObservedState
            | ApplicationSurfaceOperation::ConfigurationProtectedPreview
            | ApplicationSurfaceOperation::ConfigurationProtectedApply
            | ApplicationSurfaceOperation::ConfigurationRollbackPreview
            | ApplicationSurfaceOperation::ConfigurationRollbackApply
            | ApplicationSurfaceOperation::ConfigurationAudit => {
                unreachable!("configuration operations use their typed constructor")
            }
            ApplicationSurfaceOperation::ContextScoutStatus
            | ApplicationSurfaceOperation::ContextScoutRecent
            | ApplicationSurfaceOperation::ContextScoutExplain
            | ApplicationSurfaceOperation::ContextScoutCapability
            | ApplicationSurfaceOperation::ContextScoutBudget
            | ApplicationSurfaceOperation::ContextScoutPause
            | ApplicationSurfaceOperation::ContextScoutResume
            | ApplicationSurfaceOperation::ContextScoutCancel
            | ApplicationSurfaceOperation::ContextScoutClaim
            | ApplicationSurfaceOperation::ContextScoutDelivery
            | ApplicationSurfaceOperation::ContextScoutFeedback => {
                unreachable!("Context Scout operations use their typed constructor")
            }
        };
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload,
        }
    }

    pub fn feedback_advisory_cycle(
        request_id: impl Into<String>,
        document_uri: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::FeedbackAdvisoryCycle {
                document_uri,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn feedback_observation(
        request_id: impl Into<String>,
        subject_digest: ManifestDigest,
        observed_at: UtcMicros,
        event: FeedbackSourceEventV1,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::FeedbackObserve {
                subject_digest,
                observed_at,
                event,
            },
        }
    }

    pub fn primitive(
        request_id: impl Into<String>,
        operation: ApplicationSurfaceOperation,
        request: PrimitiveRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        let payload = match (operation, request) {
            (ApplicationSurfaceOperation::FeedbackImpact, PrimitiveRequest::Impact(request)) => {
                DaemonInvocationPayload::PrimitiveImpact {
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                }
            }
            (
                ApplicationSurfaceOperation::AffectedTests,
                PrimitiveRequest::AffectedFileTests(request),
            ) => DaemonInvocationPayload::PrimitiveAffectedTests {
                request,
                observed_at,
                deadline,
                cancellation,
            },
            (
                ApplicationSurfaceOperation::TestResults,
                PrimitiveRequest::RecentTestResults(page),
            ) => DaemonInvocationPayload::PrimitiveTestResults {
                page,
                observed_at,
                deadline,
                cancellation,
            },
            (
                surface_operation @ ApplicationSurfaceOperation::SessionLookup,
                request @ PrimitiveRequest::SessionLookup(_),
            )
            | (
                surface_operation @ ApplicationSurfaceOperation::QualifiedName,
                request @ PrimitiveRequest::QualifiedName(_),
            )
            | (
                surface_operation @ ApplicationSurfaceOperation::CallChain,
                request @ PrimitiveRequest::CallChain(_),
            )
            | (
                surface_operation @ ApplicationSurfaceOperation::FileDependents,
                request @ PrimitiveRequest::FileDependents(_),
            )
            | (
                surface_operation @ ApplicationSurfaceOperation::SourceLines,
                request @ PrimitiveRequest::SourceLines(_),
            )
            | (
                surface_operation @ ApplicationSurfaceOperation::SourceBody,
                request @ PrimitiveRequest::SourceBody(_),
            )
            | (
                surface_operation @ ApplicationSurfaceOperation::SourceOutline,
                request @ PrimitiveRequest::SourceOutline(_),
            )
            | (
                surface_operation @ ApplicationSurfaceOperation::ModuleApi,
                request @ PrimitiveRequest::ModuleApi(_),
            )
            | (
                surface_operation @ ApplicationSurfaceOperation::HealthRead,
                request @ PrimitiveRequest::HealthRead(_),
            )
            | (
                surface_operation @ ApplicationSurfaceOperation::HealthDelta,
                request @ PrimitiveRequest::HealthDelta(_),
            )
            | (
                surface_operation @ ApplicationSurfaceOperation::StorageStatus,
                request @ PrimitiveRequest::StorageStatus(_),
            )
            | (
                surface_operation @ ApplicationSurfaceOperation::DiagnosticsRead,
                request @ PrimitiveRequest::DiagnosticsRead(_),
            ) => DaemonInvocationPayload::PrimitiveRead {
                surface_operation,
                request,
                observed_at,
                deadline,
                cancellation,
            },
            _ => unreachable!("surface operation and primitive request must match"),
        };
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload,
        }
    }

    pub fn configuration(
        request_id: impl Into<String>,
        surface_operation: ApplicationSurfaceOperation,
        request: ConfigurationWireRequestV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::Configuration {
                surface_operation,
                request,
                resolved_scope: None,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn context_scout(
        request_id: impl Into<String>,
        surface_operation: ApplicationSurfaceOperation,
        request: ContextScoutSurfaceRequestV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::ContextScout {
                surface_operation,
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn retained_application(
        request_id: impl Into<String>,
        request: tracedecay_contracts::retained_surfaces::RetainedSurfaceRequestV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::RetainedApplication {
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn observatory_read(
        request_id: impl Into<String>,
        request: ObservatoryReadRequestV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::ObservatoryRead {
                request,
                resolved_scope: None,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn multi_root_scope_set_read(
        request_id: impl Into<String>,
        request: MultiRootScopeSetReadRequestV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::MultiRootScopeSetRead {
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn multi_root_scope_set_compare_and_swap(
        request_id: impl Into<String>,
        request: MultiRootScopeSetCasRequestV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::MultiRootScopeSetCompareAndSwap {
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn multi_root_execute(
        request_id: impl Into<String>,
        request: MultiRootExecuteRequestV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::MultiRootExecute {
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn work_application(
        request_id: impl Into<String>,
        request: WorkApplicationInvocationV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::WorkApplication {
                request: Box::new(request),
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn workflow_application(
        request_id: impl Into<String>,
        request: WorkflowApplicationInvocation,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::WorkflowApplication {
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn handoff_application(
        request_id: impl Into<String>,
        request: HandoffApplicationInvocationV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::HandoffApplication {
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn semantic_evaluate_and_publish(
        request_id: impl Into<String>,
        evaluated_profile_id: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::SemanticEvaluateAndPublish {
                evaluated_profile_id,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn semantic_activate(
        request_id: impl Into<String>,
        evaluated_profile_id: String,
        set_rollback: bool,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::SemanticActivate {
                evaluated_profile_id,
                set_rollback,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn semantic_qualify(
        request_id: impl Into<String>,
        evaluated_profile_id: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::SemanticQualify {
                evaluated_profile_id,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn callable_code(
        request_id: impl Into<String>,
        surface_operation: ApplicationSurfaceOperation,
        request: tracedecay_contracts::CallableCodeSurfaceRequest,
        page: PageRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        debug_assert!(matches!(
            (&request, surface_operation),
            (
                tracedecay_contracts::CallableCodeSurfaceRequest::ExactOccurrence(_),
                ApplicationSurfaceOperation::CodeExactOccurrence,
            ) | (
                tracedecay_contracts::CallableCodeSurfaceRequest::PhraseSearch(_),
                ApplicationSurfaceOperation::CodePhraseSearch,
            ) | (
                tracedecay_contracts::CallableCodeSurfaceRequest::Callees(_),
                ApplicationSurfaceOperation::CodeCallees,
            ) | (
                tracedecay_contracts::CallableCodeSurfaceRequest::Facets(_),
                ApplicationSurfaceOperation::CodeFacets,
            ) | (
                tracedecay_contracts::CallableCodeSurfaceRequest::Timeline(_),
                ApplicationSurfaceOperation::CodeTimeline,
            ) | (
                tracedecay_contracts::CallableCodeSurfaceRequest::Declaration(_),
                ApplicationSurfaceOperation::CodeDeclaration,
            ) | (
                tracedecay_contracts::CallableCodeSurfaceRequest::TypeDefinition(_),
                ApplicationSurfaceOperation::CodeTypeDefinition,
            ) | (
                tracedecay_contracts::CallableCodeSurfaceRequest::References(_),
                ApplicationSurfaceOperation::CodeReferences,
            )
        ));
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::CallableCode {
                surface_operation,
                request,
                page,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn primitive_code(
        request_id: impl Into<String>,
        surface_operation: ApplicationSurfaceOperation,
        request: tracedecay_contracts::PrimitiveCodeSurfaceRequest,
        page: PageRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::PrimitiveCode {
                surface_operation,
                request,
                page,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub fn lsp_open(
        request_id: impl Into<String>,
        client_revision: impl Into<String>,
        requested_root_uri: Option<String>,
        workspace_folders: Vec<String>,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::LspOpen {
                client_revision: client_revision.into(),
                requested_root_uri,
                workspace_folders,
                deadline,
                cancellation,
            },
        }
    }

    pub fn lsp_frame(
        request_id: impl Into<String>,
        session: DaemonLspSessionAccess,
        frame: impl Into<String>,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::LspFrame {
                session,
                frame: frame.into(),
                deadline,
                cancellation,
            },
        }
    }

    pub fn lsp_poll(
        request_id: impl Into<String>,
        session: DaemonLspSessionAccess,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::LspPoll {
                session,
                deadline,
                cancellation,
            },
        }
    }

    pub fn lsp_acknowledge(
        request_id: impl Into<String>,
        session: DaemonLspSessionAccess,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::LspAcknowledge {
                session,
                deadline,
                cancellation,
            },
        }
    }

    pub fn lsp_detach(
        request_id: impl Into<String>,
        session: DaemonLspSessionAccess,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::LspDetach {
                session,
                deadline,
                cancellation,
            },
        }
    }

    pub fn lsp_reconnect(
        request_id: impl Into<String>,
        session: DaemonLspSessionAccess,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::LspReconnect {
                session,
                deadline,
                cancellation,
            },
        }
    }

    #[must_use]
    pub fn with_delivery_route(mut self, route: FeedbackDeliveryRouteV1) -> Self {
        self.delivery_route = Some(route);
        self
    }

    #[must_use]
    pub fn with_resolved_scope(mut self, scope: Option<ResolvedScope>) -> Self {
        match &mut self.payload {
            DaemonInvocationPayload::FeedbackGet { resolved_scope, .. }
            | DaemonInvocationPayload::Configuration { resolved_scope, .. }
            | DaemonInvocationPayload::ObservatoryRead { resolved_scope, .. } => {
                *resolved_scope = scope;
            }
            _ => {}
        }
        self
    }

    pub fn lsp_workspace_folders(&self) -> Option<&[String]> {
        match &self.payload {
            DaemonInvocationPayload::LspOpen {
                workspace_folders, ..
            } => Some(workspace_folders),
            _ => None,
        }
    }

    pub fn lsp_open_control(&self) -> Option<(&Deadline, &CancellationContext)> {
        match &self.payload {
            DaemonInvocationPayload::LspOpen {
                deadline,
                cancellation,
                ..
            } => Some((deadline, cancellation)),
            _ => None,
        }
    }

    pub fn operation(&self) -> DaemonInvocationOperation {
        match self.payload {
            DaemonInvocationPayload::GitRead {
                surface_operation, ..
            } => match surface_operation {
                ApplicationSurfaceOperation::GitStatus => DaemonInvocationOperation::GitStatus,
                ApplicationSurfaceOperation::GitDiff => DaemonInvocationOperation::GitDiff,
                ApplicationSurfaceOperation::GitHistory => DaemonInvocationOperation::GitHistory,
                ApplicationSurfaceOperation::GitBlame => DaemonInvocationOperation::GitBlame,
                ApplicationSurfaceOperation::GitHunks => DaemonInvocationOperation::GitHunks,
                _ => unreachable!("Git read payloads use a Git read surface operation"),
            },
            DaemonInvocationPayload::GitPreview { .. } => DaemonInvocationOperation::GitPreview,
            DaemonInvocationPayload::GitHubStackSignalExpand { .. } => {
                DaemonInvocationOperation::GitHubStackSignalExpand
            }
            DaemonInvocationPayload::GitApply { .. } => DaemonInvocationOperation::GitApply,
            DaemonInvocationPayload::NativeIntegration {
                surface_operation, ..
            } => match surface_operation {
                ApplicationSurfaceOperation::NativeIntegrationStackSnapshot => {
                    DaemonInvocationOperation::NativeIntegrationStackSnapshot
                }
                ApplicationSurfaceOperation::NativeIntegrationPreflight => {
                    DaemonInvocationOperation::NativeIntegrationPreflight
                }
                ApplicationSurfaceOperation::NativeIntegrationApprove => {
                    DaemonInvocationOperation::NativeIntegrationApprove
                }
                ApplicationSurfaceOperation::NativeIntegrationApply => {
                    DaemonInvocationOperation::NativeIntegrationApply
                }
                ApplicationSurfaceOperation::NativeIntegrationStatus => {
                    DaemonInvocationOperation::NativeIntegrationStatus
                }
                ApplicationSurfaceOperation::NativeIntegrationCancel => {
                    DaemonInvocationOperation::NativeIntegrationCancel
                }
                ApplicationSurfaceOperation::NativeIntegrationWorktreeInventory => {
                    DaemonInvocationOperation::NativeIntegrationWorktreeInventory
                }
                ApplicationSurfaceOperation::NativeIntegrationWorktreeInspect => {
                    DaemonInvocationOperation::NativeIntegrationWorktreeInspect
                }
                ApplicationSurfaceOperation::NativeIntegrationWorktreeConfirm => {
                    DaemonInvocationOperation::NativeIntegrationWorktreeConfirm
                }
                ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove => {
                    DaemonInvocationOperation::NativeIntegrationWorktreeRemove
                }
                ApplicationSurfaceOperation::NativeIntegrationWorktreeReconcile => {
                    DaemonInvocationOperation::NativeIntegrationWorktreeReconcile
                }
                _ => unreachable!(
                    "native integration payloads use a native integration surface operation"
                ),
            },
            DaemonInvocationPayload::FeedbackDiagnostics { .. } => {
                DaemonInvocationOperation::FeedbackDiagnostics
            }
            DaemonInvocationPayload::FeedbackGet { .. } => DaemonInvocationOperation::FeedbackGet,
            DaemonInvocationPayload::FeedbackExpand { .. } => {
                DaemonInvocationOperation::FeedbackExpand
            }
            DaemonInvocationPayload::FeedbackList { .. } => DaemonInvocationOperation::FeedbackList,
            DaemonInvocationPayload::FeedbackAdvisoryCycle { .. } => {
                DaemonInvocationOperation::FeedbackAdvisoryCycle
            }
            DaemonInvocationPayload::FeedbackImpact { .. } => {
                DaemonInvocationOperation::FeedbackImpact
            }
            DaemonInvocationPayload::AffectedTests { .. } => {
                DaemonInvocationOperation::AffectedTests
            }
            DaemonInvocationPayload::FeedbackObserve { .. } => {
                DaemonInvocationOperation::FeedbackObserve
            }
            DaemonInvocationPayload::PrimitiveImpact { .. } => {
                DaemonInvocationOperation::PrimitiveImpact
            }
            DaemonInvocationPayload::PrimitiveAffectedTests { .. } => {
                DaemonInvocationOperation::PrimitiveAffectedTests
            }
            DaemonInvocationPayload::PrimitiveTestResults { .. } => {
                DaemonInvocationOperation::PrimitiveTestResults
            }
            DaemonInvocationPayload::PrimitiveRead { .. } => {
                DaemonInvocationOperation::PrimitiveRead
            }
            DaemonInvocationPayload::PrimitiveCode { .. } => {
                DaemonInvocationOperation::PrimitiveRead
            }
            DaemonInvocationPayload::CallableCode {
                request: tracedecay_contracts::CallableCodeSurfaceRequest::ExactOccurrence(_),
                ..
            } => DaemonInvocationOperation::CodeExactOccurrence,
            DaemonInvocationPayload::CallableCode {
                request: tracedecay_contracts::CallableCodeSurfaceRequest::PhraseSearch(_),
                ..
            } => DaemonInvocationOperation::CodePhraseSearch,
            DaemonInvocationPayload::CallableCode {
                request: tracedecay_contracts::CallableCodeSurfaceRequest::Callees(_),
                ..
            } => DaemonInvocationOperation::CodeCallees,
            DaemonInvocationPayload::CallableCode {
                request: tracedecay_contracts::CallableCodeSurfaceRequest::Facets(_),
                ..
            } => DaemonInvocationOperation::CodeFacets,
            DaemonInvocationPayload::CallableCode {
                request: tracedecay_contracts::CallableCodeSurfaceRequest::Timeline(_),
                ..
            } => DaemonInvocationOperation::CodeTimeline,
            DaemonInvocationPayload::CallableCode {
                request: tracedecay_contracts::CallableCodeSurfaceRequest::Declaration(_),
                ..
            } => DaemonInvocationOperation::CodeDeclaration,
            DaemonInvocationPayload::CallableCode {
                request: tracedecay_contracts::CallableCodeSurfaceRequest::TypeDefinition(_),
                ..
            } => DaemonInvocationOperation::CodeTypeDefinition,
            DaemonInvocationPayload::CallableCode {
                request: tracedecay_contracts::CallableCodeSurfaceRequest::References(_),
                ..
            } => DaemonInvocationOperation::CodeReferences,
            DaemonInvocationPayload::Configuration { .. } => {
                DaemonInvocationOperation::Configuration
            }
            DaemonInvocationPayload::ContextScout { .. } => DaemonInvocationOperation::ContextScout,
            DaemonInvocationPayload::ObservatoryRead { .. } => {
                DaemonInvocationOperation::ObservatoryRead
            }
            DaemonInvocationPayload::RetainedApplication { .. } => {
                DaemonInvocationOperation::RetainedApplication
            }
            DaemonInvocationPayload::MultiRootScopeSetRead { .. } => {
                DaemonInvocationOperation::MultiRootScopeSetRead
            }
            DaemonInvocationPayload::MultiRootScopeSetCompareAndSwap { .. } => {
                DaemonInvocationOperation::MultiRootScopeSetCompareAndSwap
            }
            DaemonInvocationPayload::MultiRootExecute { .. } => {
                DaemonInvocationOperation::MultiRootExecute
            }
            DaemonInvocationPayload::WorkApplication { .. } => {
                DaemonInvocationOperation::WorkApplication
            }
            DaemonInvocationPayload::WorkflowApplication { .. } => {
                DaemonInvocationOperation::WorkflowApplication
            }
            DaemonInvocationPayload::HandoffApplication { .. } => {
                DaemonInvocationOperation::HandoffApplication
            }
            DaemonInvocationPayload::SemanticEvaluateAndPublish { .. } => {
                DaemonInvocationOperation::SemanticEvaluateAndPublish
            }
            DaemonInvocationPayload::SemanticActivate { .. } => {
                DaemonInvocationOperation::SemanticActivate
            }
            DaemonInvocationPayload::SemanticQualify { .. } => {
                DaemonInvocationOperation::SemanticQualify
            }
            DaemonInvocationPayload::LspOpen { .. } => DaemonInvocationOperation::LspOpen,
            DaemonInvocationPayload::LspFrame { .. } => DaemonInvocationOperation::LspFrame,
            DaemonInvocationPayload::LspPoll { .. } => DaemonInvocationOperation::LspPoll,
            DaemonInvocationPayload::LspAcknowledge { .. } => {
                DaemonInvocationOperation::LspAcknowledge
            }
            DaemonInvocationPayload::LspReconnect { .. } => DaemonInvocationOperation::LspReconnect,
            DaemonInvocationPayload::LspDetach { .. } => DaemonInvocationOperation::LspDetach,
            DaemonInvocationPayload::SourceEdit { .. } => DaemonInvocationOperation::SourceEdit,
            DaemonInvocationPayload::SourceEditReconcile { .. } => {
                DaemonInvocationOperation::SourceEditReconcile
            }
            DaemonInvocationPayload::SourceEditRollback { .. } => {
                DaemonInvocationOperation::SourceEditRollback
            }
        }
    }

    pub fn requires_project(&self) -> bool {
        matches!(
            self.operation(),
            DaemonInvocationOperation::GitStatus
                | DaemonInvocationOperation::GitDiff
                | DaemonInvocationOperation::GitHistory
                | DaemonInvocationOperation::GitBlame
                | DaemonInvocationOperation::GitHubStackSignalExpand
                | DaemonInvocationOperation::GitHunks
                | DaemonInvocationOperation::GitPreview
                | DaemonInvocationOperation::GitApply
                | DaemonInvocationOperation::NativeIntegrationStackSnapshot
                | DaemonInvocationOperation::NativeIntegrationPreflight
                | DaemonInvocationOperation::NativeIntegrationApprove
                | DaemonInvocationOperation::NativeIntegrationApply
                | DaemonInvocationOperation::NativeIntegrationStatus
                | DaemonInvocationOperation::NativeIntegrationCancel
                | DaemonInvocationOperation::NativeIntegrationWorktreeInventory
                | DaemonInvocationOperation::NativeIntegrationWorktreeInspect
                | DaemonInvocationOperation::NativeIntegrationWorktreeConfirm
                | DaemonInvocationOperation::NativeIntegrationWorktreeRemove
                | DaemonInvocationOperation::NativeIntegrationWorktreeReconcile
                | DaemonInvocationOperation::FeedbackDiagnostics
                | DaemonInvocationOperation::FeedbackGet
                | DaemonInvocationOperation::FeedbackExpand
                | DaemonInvocationOperation::FeedbackList
                | DaemonInvocationOperation::FeedbackAdvisoryCycle
                | DaemonInvocationOperation::FeedbackImpact
                | DaemonInvocationOperation::AffectedTests
                | DaemonInvocationOperation::FeedbackObserve
                | DaemonInvocationOperation::PrimitiveImpact
                | DaemonInvocationOperation::PrimitiveAffectedTests
                | DaemonInvocationOperation::PrimitiveTestResults
                | DaemonInvocationOperation::PrimitiveRead
                | DaemonInvocationOperation::CodeExactOccurrence
                | DaemonInvocationOperation::CodePhraseSearch
                | DaemonInvocationOperation::CodeCallees
                | DaemonInvocationOperation::CodeFacets
                | DaemonInvocationOperation::CodeTimeline
                | DaemonInvocationOperation::CodeDeclaration
                | DaemonInvocationOperation::CodeTypeDefinition
                | DaemonInvocationOperation::CodeReferences
                | DaemonInvocationOperation::Configuration
                | DaemonInvocationOperation::ContextScout
                | DaemonInvocationOperation::ObservatoryRead
                | DaemonInvocationOperation::RetainedApplication
                | DaemonInvocationOperation::MultiRootScopeSetRead
                | DaemonInvocationOperation::MultiRootScopeSetCompareAndSwap
                | DaemonInvocationOperation::MultiRootExecute
                | DaemonInvocationOperation::WorkApplication
                | DaemonInvocationOperation::WorkflowApplication
                | DaemonInvocationOperation::HandoffApplication
                | DaemonInvocationOperation::SemanticEvaluateAndPublish
                | DaemonInvocationOperation::SemanticActivate
                | DaemonInvocationOperation::SemanticQualify
                | DaemonInvocationOperation::LspOpen
                | DaemonInvocationOperation::SourceEdit
                | DaemonInvocationOperation::SourceEditReconcile
                | DaemonInvocationOperation::SourceEditRollback
        )
    }

    pub fn is_workflow_application(&self) -> bool {
        matches!(
            &self.payload,
            DaemonInvocationPayload::WorkflowApplication { .. }
        )
    }

    /// The caller's immutable budget also bounds the terminal delivery ACK.
    /// Work output must not hold an authenticated connection past the
    /// invocation's own deadline when a surface disappears before `ACKing`.
    pub fn delivery_ack_deadline(&self) -> Option<&Deadline> {
        match &self.payload {
            DaemonInvocationPayload::WorkApplication { deadline, .. } => Some(deadline),
            _ => None,
        }
    }

    pub fn validate(&self) -> Result<(), DaemonInvocationProblem> {
        if self.protocol != DAEMON_INVOCATION_PROTOCOL {
            return Err(DaemonInvocationProblem::InvalidRequest);
        }
        if self.revision != DAEMON_INVOCATION_REVISION {
            return Err(DaemonInvocationProblem::UnsupportedRevision);
        }
        if !valid_token(&self.request_id, MAX_INVOCATION_REQUEST_ID_BYTES) {
            return Err(DaemonInvocationProblem::InvalidRequest);
        }
        match &self.payload {
            DaemonInvocationPayload::MultiRootScopeSetRead {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                if observed_at.0 <= 0
                    || deadline.expires_at.0 <= 0
                    || cancellation.token_id.as_str().len() > MAX_OPAQUE_HANDLE_BYTES
                    || MultiRootScopeSetReadRequestV1::new(request.scope_set_id.clone()).is_err()
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::MultiRootScopeSetCompareAndSwap {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                if observed_at.0 <= 0
                    || deadline.expires_at.0 <= 0
                    || cancellation.token_id.as_str().len() > MAX_OPAQUE_HANDLE_BYTES
                    || request.validate().is_err()
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::MultiRootExecute {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                if observed_at.0 <= 0
                    || deadline.expires_at.0 <= 0
                    || cancellation.token_id.as_str().len() > MAX_OPAQUE_HANDLE_BYTES
                    || request.validate().is_err()
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::GitRead {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::GitPreview {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::GitHubStackSignalExpand {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::GitApply {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::NativeIntegration {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::PrimitiveImpact {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::PrimitiveAffectedTests {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::PrimitiveTestResults {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::PrimitiveRead {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::WorkApplication {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::WorkflowApplication {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::HandoffApplication {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::SourceEdit {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::SourceEditReconcile {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::SourceEditRollback {
                observed_at,
                deadline,
                cancellation,
                ..
            } => {
                if observed_at.0 <= 0
                    || deadline.expires_at.0 <= 0
                    || cancellation.token_id.as_str().len() > MAX_OPAQUE_HANDLE_BYTES
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::ObservatoryRead {
                request,
                observed_at,
                deadline,
                cancellation,
                ..
            } => {
                if !(1..=365).contains(&request.window_days)
                    || observed_at.0 <= 0
                    || deadline.expires_at.0 <= 0
                    || cancellation.token_id.as_str().len() > MAX_OPAQUE_HANDLE_BYTES
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::Configuration {
                observed_at,
                deadline,
                cancellation,
                ..
            } => {
                if observed_at.0 <= 0
                    || deadline.expires_at.0 <= 0
                    || cancellation.token_id.as_str().len() > MAX_OPAQUE_HANDLE_BYTES
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::PrimitiveCode {
                surface_operation,
                request,
                page,
                observed_at,
                deadline,
                cancellation,
            } => {
                if observed_at.0 <= 0
                    || deadline.expires_at.0 <= 0
                    || PageRequest::new(page.page_size, page.cursor.clone()).is_err()
                    || cancellation.token_id.as_str().len() > MAX_OPAQUE_HANDLE_BYTES
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
                let matches = matches!(
                    (surface_operation, request),
                    (
                        ApplicationSurfaceOperation::CodeSymbolSearch,
                        tracedecay_contracts::PrimitiveCodeSurfaceRequest::SymbolSearch(_),
                    ) | (
                        ApplicationSurfaceOperation::CodeSignatureSearch,
                        tracedecay_contracts::PrimitiveCodeSurfaceRequest::SignatureSearch(_),
                    ) | (
                        ApplicationSurfaceOperation::CodeImplementations,
                        tracedecay_contracts::PrimitiveCodeSurfaceRequest::Implementations(_),
                    ) | (
                        ApplicationSurfaceOperation::CodeTypeHierarchy,
                        tracedecay_contracts::PrimitiveCodeSurfaceRequest::TypeHierarchy(_),
                    ) | (
                        ApplicationSurfaceOperation::CodeCallers,
                        tracedecay_contracts::PrimitiveCodeSurfaceRequest::Callers(_),
                    )
                );
                if !matches {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::SemanticEvaluateAndPublish {
                evaluated_profile_id,
                observed_at,
                deadline,
                cancellation,
            }
            | DaemonInvocationPayload::SemanticActivate {
                evaluated_profile_id,
                observed_at,
                deadline,
                cancellation,
                ..
            } => {
                if evaluated_profile_id.trim() != evaluated_profile_id
                    || evaluated_profile_id.is_empty()
                    || evaluated_profile_id.len() > MAX_OPAQUE_HANDLE_BYTES
                    || observed_at.0 <= 0
                    || deadline.expires_at.0 <= 0
                    || cancellation.token_id.as_str().len() > MAX_OPAQUE_HANDLE_BYTES
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::SemanticQualify {
                evaluated_profile_id,
                observed_at,
                deadline,
                cancellation,
            } => {
                if evaluated_profile_id.trim() != evaluated_profile_id
                    || evaluated_profile_id.is_empty()
                    || evaluated_profile_id.len() > MAX_OPAQUE_HANDLE_BYTES
                    || observed_at.0 <= 0
                    || deadline.expires_at.0 <= 0
                    || cancellation.token_id.as_str().len() > MAX_OPAQUE_HANDLE_BYTES
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::CallableCode {
                surface_operation,
                request,
                page,
                observed_at,
                deadline,
                cancellation,
            } => {
                if observed_at.0 <= 0
                    || deadline.expires_at.0 <= 0
                    || PageRequest::new(page.page_size, page.cursor.clone()).is_err()
                    || cancellation.token_id.as_str().len() > MAX_OPAQUE_HANDLE_BYTES
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
                let matches = matches!(
                    (surface_operation, request),
                    (
                        ApplicationSurfaceOperation::CodeExactOccurrence,
                        tracedecay_contracts::CallableCodeSurfaceRequest::ExactOccurrence(_),
                    ) | (
                        ApplicationSurfaceOperation::CodePhraseSearch,
                        tracedecay_contracts::CallableCodeSurfaceRequest::PhraseSearch(_),
                    ) | (
                        ApplicationSurfaceOperation::CodeCallees,
                        tracedecay_contracts::CallableCodeSurfaceRequest::Callees(_),
                    ) | (
                        ApplicationSurfaceOperation::CodeFacets,
                        tracedecay_contracts::CallableCodeSurfaceRequest::Facets(_),
                    ) | (
                        ApplicationSurfaceOperation::CodeTimeline,
                        tracedecay_contracts::CallableCodeSurfaceRequest::Timeline(_),
                    ) | (
                        ApplicationSurfaceOperation::CodeDeclaration,
                        tracedecay_contracts::CallableCodeSurfaceRequest::Declaration(_),
                    ) | (
                        ApplicationSurfaceOperation::CodeTypeDefinition,
                        tracedecay_contracts::CallableCodeSurfaceRequest::TypeDefinition(_),
                    ) | (
                        ApplicationSurfaceOperation::CodeReferences,
                        tracedecay_contracts::CallableCodeSurfaceRequest::References(_),
                    )
                );
                if !matches {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::ContextScout {
                surface_operation,
                request,
                observed_at,
                deadline,
                cancellation,
                ..
            } => {
                if observed_at.0 <= 0
                    || deadline.expires_at.0 <= 0
                    || cancellation.token_id.as_str().len() > MAX_OPAQUE_HANDLE_BYTES
                    || !request.matches(*surface_operation)
                    || matches!(
                        request,
                        ContextScoutSurfaceRequestV1::Recent(request)
                            | ContextScoutSurfaceRequestV1::Explain(request)
                            if !(1..=32).contains(&request.limit)
                    )
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::RetainedApplication {
                observed_at,
                deadline,
                cancellation,
                ..
            } => {
                if observed_at.0 <= 0
                    || deadline.expires_at.0 <= 0
                    || cancellation.token_id.as_str().len() > MAX_OPAQUE_HANDLE_BYTES
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::FeedbackDiagnostics {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            }
            | DaemonInvocationPayload::FeedbackGet {
                request_handle,
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::FeedbackExpand {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            }
            | DaemonInvocationPayload::FeedbackList {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            }
            | DaemonInvocationPayload::FeedbackImpact {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            }
            | DaemonInvocationPayload::AffectedTests {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            } => {
                if !valid_token(request_handle, MAX_OPAQUE_HANDLE_BYTES)
                    || observed_at.0 <= 0
                    || deadline.expires_at.0 <= 0
                    || cancellation.token_id.as_str().len() > MAX_OPAQUE_HANDLE_BYTES
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::FeedbackAdvisoryCycle {
                document_uri,
                observed_at,
                deadline,
                cancellation,
            } => {
                if !valid_printable(document_uri, MAX_ROOT_HINT_BYTES)
                    || observed_at.0 <= 0
                    || deadline.expires_at.0 <= 0
                    || cancellation.token_id.as_str().len() > MAX_OPAQUE_HANDLE_BYTES
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::FeedbackObserve {
                subject_digest,
                observed_at,
                event,
            } => {
                if subject_digest.validate().is_err()
                    || observed_at.0 <= 0
                    || event.validate().is_none()
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::LspOpen {
                client_revision,
                requested_root_uri,
                workspace_folders,
                deadline,
                cancellation,
            } => {
                if !valid_printable(client_revision, MAX_CLIENT_REVISION_BYTES)
                    || requested_root_uri
                        .as_deref()
                        .is_some_and(|uri| !valid_printable(uri, MAX_ROOT_HINT_BYTES))
                    || workspace_folders.len() > MAX_LSP_WORKSPACE_ROOTS
                    || workspace_folders
                        .iter()
                        .any(|folder| !valid_printable(folder, MAX_ROOT_HINT_BYTES))
                    || !valid_lsp_control(deadline, cancellation)
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::LspFrame {
                session,
                frame,
                deadline,
                cancellation,
            } => {
                let _ = session.clone().into_access()?;
                if frame.len() > MAX_LSP_FRAME_BYTES || !valid_lsp_control(deadline, cancellation) {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::LspPoll {
                session,
                deadline,
                cancellation,
            }
            | DaemonInvocationPayload::LspAcknowledge {
                session,
                deadline,
                cancellation,
            }
            | DaemonInvocationPayload::LspReconnect {
                session,
                deadline,
                cancellation,
            }
            | DaemonInvocationPayload::LspDetach {
                session,
                deadline,
                cancellation,
            } => {
                let _ = session.clone().into_access()?;
                if !valid_lsp_control(deadline, cancellation) {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
        }
        Ok(())
    }
}

/// Parse an invocation only when it explicitly selects this protocol. Ordinary
/// MCP JSON-RPC frames continue through the established daemon route.
///
/// Measured as the wire decode phase: this full-line parse runs between
/// `daemon.wire.read_line` and the dispatch span, so without its own label a
/// slow request could not be attributed between payload decode and handling.
#[hotpath::measure(label = "daemon.wire.decode_invocation")]
pub fn parse_daemon_invocation_request(
    line: &str,
) -> Option<Result<DaemonInvocationRequest, DaemonInvocationResponse>> {
    let value = serde_json::from_str::<serde_json::Value>(line.trim()).ok()?;
    if value.get("protocol").and_then(serde_json::Value::as_str) != Some(DAEMON_INVOCATION_PROTOCOL)
    {
        return None;
    }
    let request_id = value
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    // A frame that names this protocol but no longer deserializes is either
    // a foreign revision (explicitly, or as same-revision wire drift between
    // build versions) or a malformed request. Answer the former as the typed
    // revision refusal so a skewed client never has to guess from
    // `invalid_request` whether its own request was wrong.
    let envelope_revision = value.get("revision").and_then(serde_json::Value::as_u64);
    Some(serde_json::from_value(value).map_err(|_| {
        let problem = match envelope_revision {
            Some(revision) if revision != u64::from(DAEMON_INVOCATION_REVISION) => {
                DaemonInvocationProblem::UnsupportedRevision
            }
            _ => DaemonInvocationProblem::InvalidRequest,
        };
        DaemonInvocationResponse::problem(request_id, problem)
    }))
}

#[cfg(test)]
mod operation_label_tests {
    use super::DaemonInvocationOperation;

    /// Handle-bearing feedback operations and their direct-request primitive
    /// twins are distinct dispatch paths; the client labels every invocation
    /// with `as_str`, so a shared label would merge them in observability.
    #[test]
    fn primitive_operations_carry_labels_distinct_from_their_feedback_twins() {
        for (feedback, primitive) in [
            (
                DaemonInvocationOperation::FeedbackImpact,
                DaemonInvocationOperation::PrimitiveImpact,
            ),
            (
                DaemonInvocationOperation::AffectedTests,
                DaemonInvocationOperation::PrimitiveAffectedTests,
            ),
        ] {
            assert_ne!(
                feedback.as_str(),
                primitive.as_str(),
                "{feedback:?} vs {primitive:?}"
            );
        }
    }
}

/// A safe, deliberately non-diagnostic daemon invocation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonInvocationProblem {
    InvalidRequest,
    UnsupportedRevision,
    NotFoundOrNotAuthorized,
    ResetRequired,
    ApplicationContractViolation,
    Unavailable,
}

impl DaemonInvocationProblem {
    /// Preserve the typed refusal as a reason-coded error. Never format the
    /// variant with `Debug` into a user-facing message.
    pub fn into_trace_decay_error(self) -> tracedecay_domain::errors::TraceDecayError {
        let (reason_code, retryable, detail) = match self {
            Self::InvalidRequest => (
                "daemon_invocation.invalid_request",
                false,
                "The daemon invocation request is invalid",
            ),
            Self::UnsupportedRevision => (
                "daemon_invocation.unsupported_revision",
                false,
                "The daemon invocation revision is unsupported",
            ),
            Self::NotFoundOrNotAuthorized => (
                "daemon_invocation.not_found_or_not_authorized",
                false,
                "The requested daemon invocation was not found or is not authorized",
            ),
            Self::ResetRequired => (
                "daemon_invocation.reset_required",
                false,
                "The daemon invocation store must be reset",
            ),
            Self::ApplicationContractViolation => (
                "daemon_invocation.application_contract_violation",
                false,
                "The daemon invocation violated its application contract",
            ),
            Self::Unavailable => (
                "daemon_invocation.unavailable",
                true,
                "The daemon invocation is unavailable",
            ),
        };
        tracedecay_domain::errors::TraceDecayError::project_route(reason_code, retryable, detail)
    }
}

#[cfg(test)]
mod invocation_wire_revision_tests {
    use super::{
        DAEMON_INVOCATION_PROTOCOL, DaemonInvocationOutcome, DaemonInvocationProblem,
        parse_daemon_invocation_request,
    };

    fn parse_problem(line: &str) -> DaemonInvocationProblem {
        let response = parse_daemon_invocation_request(line)
            .expect("frames naming the invocation protocol must be answered")
            .expect_err("undecodable frames must produce a typed response");
        match response.outcome {
            DaemonInvocationOutcome::Problem { problem } => problem,
            outcome => panic!("expected a typed problem response, got {outcome:?}"),
        }
    }

    #[test]
    fn foreign_revision_frames_refuse_as_unsupported_revision() {
        let line = format!(
            r#"{{"protocol":"{DAEMON_INVOCATION_PROTOCOL}","revision":2,"request_id":"request.future","operation":"semantic_activate_v3"}}"#
        );
        assert_eq!(
            parse_problem(&line),
            DaemonInvocationProblem::UnsupportedRevision,
            "a frame from a different wire revision is a revision refusal, not a caller mistake"
        );
    }

    #[test]
    fn same_revision_malformed_frames_stay_invalid_request() {
        let line = format!(
            r#"{{"protocol":"{DAEMON_INVOCATION_PROTOCOL}","revision":1,"request_id":"request.same","operation":"no_such_operation"}}"#
        );
        assert_eq!(
            parse_problem(&line),
            DaemonInvocationProblem::InvalidRequest,
        );
    }
}

#[cfg(test)]
mod invocation_problem_tests {
    use super::{DaemonInvocationProblem, DaemonInvocationResponse};

    #[test]
    fn application_contract_violation_round_trips_without_diagnostics() {
        let response = DaemonInvocationResponse::problem(
            "request.application-contract",
            DaemonInvocationProblem::ApplicationContractViolation,
        );
        let wire = serde_json::to_value(&response).expect("daemon invocation response wire");
        assert_eq!(wire["status"], "problem");
        assert_eq!(wire["problem"], "application_contract_violation");
        assert!(wire.get("diagnostic").is_none());
        assert!(wire.get("message").is_none());
        assert_eq!(
            serde_json::from_value::<DaemonInvocationResponse>(wire)
                .expect("daemon invocation response")
                .outcome,
            response.outcome
        );
    }
}

/// Response envelope paired with one invocation request id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DaemonInvocationResponse {
    pub protocol: String,
    pub revision: u16,
    pub request_id: String,
    #[serde(flatten)]
    pub outcome: DaemonInvocationOutcome,
}

/// Bounded operation outcomes. LSP payloads remain protocol frames, not an
/// unrestricted stream or arbitrary daemon-socket response.
// `WorkApplication` is matched and constructed across two dozen call sites
// (work_cli, application_surface, service::invocation::work and its tests);
// boxing it would ripple through all of them for a wire contract type.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DaemonInvocationOutcome {
    GitRead {
        scope: ResolvedScope,
        result: DaemonFeedbackResult,
    },
    GitPreview {
        scope: ResolvedScope,
        preview: DaemonGitPreviewResult,
    },
    GitApply {
        scope: ResolvedScope,
        effect: DaemonGitEffectResult,
    },
    GitHubStackSignalExpand {
        scope: ResolvedScope,
        outcome: ApplicationOutcome<serde_json::Value>,
    },
    NativeIntegration {
        scope: ResolvedScope,
        outcome: ApplicationOutcome<serde_json::Value>,
    },
    Feedback {
        scope: ResolvedScope,
        result: DaemonFeedbackResult,
    },
    Primitive {
        scope: ResolvedScope,
        result: DaemonFeedbackResult,
    },
    CallableCode {
        scope: ResolvedScope,
        result: DaemonFeedbackResult,
    },
    Configuration {
        scope: ResolvedScope,
        outcome: ApplicationOutcome<serde_json::Value>,
    },
    ContextScout {
        scope: ResolvedScope,
        outcome: ApplicationOutcome<serde_json::Value>,
    },
    ObservatoryRead {
        scope: ResolvedScope,
        result: DaemonFeedbackResult,
    },
    RetainedApplication {
        scope: ResolvedScope,
        outcome:
            ApplicationOutcome<tracedecay_contracts::retained_surfaces::RetainedSurfaceResultV1>,
    },
    RetainedApplicationProblem {
        scope: ResolvedScope,
        problem: ApplicationProblem,
    },
    MultiRootScopeSetRead {
        scope: ResolvedScope,
        outcome: ApplicationOutcome<Option<AuthorizedScopeSet>>,
    },
    MultiRootScopeSetCompareAndSwap {
        scope: ResolvedScope,
        outcome: ApplicationOutcome<MultiRootScopeSetCasResultV1>,
    },
    MultiRootQueryPage {
        scope: ResolvedScope,
        outcome: ApplicationOutcome<tracedecay_contracts::MultiRootQueryPageV1<serde_json::Value>>,
    },
    WorkApplication {
        scope: ResolvedScope,
        outcome: WorkApplicationOutcomeV1,
    },
    WorkflowApplication {
        scope: ResolvedScope,
        outcome: WorkflowApplicationOutcome,
    },
    HandoffApplication {
        scope: ResolvedScope,
        outcome: HandoffApplicationOutcomeV1,
    },
    SemanticEvaluatedProfilePublished {
        scope: ResolvedScope,
        profile_digest: ManifestDigest,
        report_digest: ManifestDigest,
        report: serde_json::Value,
        source_generation: tracedecay_domain::CodeGenerationId,
        snapshot_digest: ManifestDigest,
    },
    /// Terminal receipt of the composed evaluate → publish → activate journey.
    ///
    /// `configuration_revision` is the revision produced by the activation
    /// compare-and-swap; `runtime_state` is the serialized
    /// `SemanticRuntimeStateV1` observed immediately after that revision
    /// applied (the daemon serializes the typed state; like the evaluation
    /// `report` above, it crosses this wire as JSON), so a caller can
    /// distinguish "activation recorded, runtime converging" from "ready".
    SemanticProfileActivated {
        scope: ResolvedScope,
        profile_digest: ManifestDigest,
        report_digest: ManifestDigest,
        configuration_revision: tracedecay_domain::configuration::ConfigurationRevisionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rollback_profile_id: Option<String>,
        runtime_state: serde_json::Value,
    },
    SemanticEvaluatedProfileQualified {
        qualification: CanonicalQualificationBlob,
    },
    ObservationAccepted,
    ApplicationProblem {
        problem: ApplicationProblem,
    },
    LspOpened {
        session: DaemonLspSessionAccess,
        expires_at_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope_set_id: Option<ScopeSetId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope_set_digest: Option<ManifestDigest>,
    },
    LspFrameAccepted {
        backpressured: bool,
        closed: bool,
    },
    LspFrame {
        frame: Option<String>,
        closed: bool,
    },
    LspAcknowledged {
        acknowledged: bool,
    },
    LspReconnected {
        session: DaemonLspSessionAccess,
    },
    LspDetached,
    SourceEdit {
        scope: ResolvedScope,
        result: tracedecay_contracts::source_edit::SourceEditSurfaceResultV1,
    },
    Problem {
        problem: DaemonInvocationProblem,
    },
}

impl DaemonInvocationResponse {
    pub fn lsp_opened(
        request_id: String,
        session: DaemonLspSessionAccess,
        expires_at_ms: u64,
        scope_set_id: Option<ScopeSetId>,
        scope_set_digest: Option<ManifestDigest>,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id,
            outcome: DaemonInvocationOutcome::LspOpened {
                session,
                expires_at_ms,
                scope_set_id,
                scope_set_digest,
            },
        }
    }

    pub fn with_outcome(request_id: String, outcome: DaemonInvocationOutcome) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id,
            outcome,
        }
    }
}
