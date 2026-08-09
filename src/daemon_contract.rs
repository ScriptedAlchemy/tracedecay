//! The daemon invocation wire contract.
//!
//! These are the request, response, outcome, and problem shapes exchanged over
//! the daemon's closed post-handshake invocation protocol. They are data: no
//! socket, no admission decision, no runtime registry, no database.
//!
//! The contract lives outside `crate::daemon` on purpose. Its callers are the
//! application surface and the invocation client, which have no business
//! reaching into the daemon's service internals just to name a payload. Keeping
//! the shapes here means a caller depends on the protocol, not on the server
//! that happens to implement it.
//!
//! Behavior stays with the daemon. Anything that interprets a request —
//! authority minting, scope resolution, dispatch — remains in
//! `crate::daemon::service::invocation`; only construction, validation, and the
//! application-DTO conversions travel with the types they belong to.

mod git_surface;
use std::fmt;

use serde::{Deserialize, Serialize};
use tracedecay_application::{
    AcceptProposalCommand, AcceptTaskCommand, AdjudicateWorkLeakCommandV1, AdmitExecutionCommand,
    AdmitWorkPlacementCommand, AdmitWorkSynthesisCommand, ApplicationContractError,
    ApplicationOutcome, ApplicationProblem, AttachRuntimeEvidenceCommand, AuthorityReceipt,
    AuthorizedScopeSet, CancelWorkAttemptCommand, CancellationContext, CreateWorkCommand, Deadline,
    EffectId, EffectReceipt, EffectResult, EvidenceAuthority, EvidenceCoverage, EvidencePacket,
    EvidenceScore, ExecutionTopologyMetricsRequestV1, ExecutionTopologyMetricsV1,
    ExecutionTopologyViewV1, GenerateProposalRequest, GeneratedWorkProposal, IdempotencyKey,
    IssueTaskHandoffRequestV1, IssueTaskHandoffResultV1, MultiRootExecuteRequestV1,
    MultiRootScopeSetCasRequestV1, MultiRootScopeSetCasResultV1, MultiRootScopeSetReadRequestV1,
    Omission, OpenInvestigationHandoffRequestV1, OpenInvestigationHandoffResultV1,
    OpenTaskHandoffRequestV1, OpenTaskHandoffResultV1, OperationReceipt, PageRequest, PageState,
    PauseWorkRunCommand, PreviewId, PreviewResult, ReconciliationState,
    ReleaseWorkPlacementCommand, ReplanDependenciesCommand, RequestId, ResolvedScope,
    ResumeWorkAttemptsCommand, ResumeWorkRunCommand, RetrieverContribution,
    RetryWorkAttemptCommandV1, ReviewProposalRequestV1, StartWorkAttemptCommand, TaskHandoffGrant,
    TaskHandoffIssueRequest, TaskHandoffRedeemRequest, TaskHandoffRedeemed, TemporalState,
    WorkArtifactHydrationRequestV1, WorkArtifactHydrationV1, WorkAttemptListRequestV1,
    WorkAttemptListV1, WorkAttemptRecoveryReportV1, WorkAttemptStatusRequestV1,
    WorkDuplicateAdjudicationAppendOutcomeV1, WorkGraphReadRequestV1, WorkGraphReadV1,
    WorkLeakAdjudicationOutcomeV1, WorkPlacementPreflightRequestV1, WorkPlacementReadingV1,
    WorkPlacementStatusRequestV1, WorkProductMutationReceiptV1, WorkProductMutationRequestV1,
    WorkProjectionDeltaRequestV1, WorkProjectionSnapshotRequestV1,
    WorkRetryTestBindingTokenOutcomeV1, WorkRetryTestBindingTokenRequestV1,
    WorkRunControlReadingV1, WorkRunControlRequestV1, WorkSynthesisAttemptV1,
    WorkTopologyViewRequestV1, WorkflowDefinitionActivateRequest, WorkflowDefinitionDiff,
    WorkflowDefinitionDiffRequest, WorkflowDefinitionDisposition, WorkflowDefinitionGetRequest,
    WorkflowDefinitionHistoryRequest, WorkflowDefinitionListRequest,
    WorkflowDefinitionRegisterRequest, WorkflowDefinitionRejectRequest,
    WorkflowDefinitionRetireRequest, WorkflowDefinitionValidateRequest,
    WorkflowDefinitionValidation,
};
use tracedecay_domain::{
    ActorId, GitIndexPreviewV1, GitIndexTransactionReceiptV1, ManifestDigest, RetrievalAnchorId,
    ScopeSetId, UtcMicros, WorkAttemptV1, WorkDuplicateAdjudicationCommandV1,
    WorkPlacementPreflightV1, WorkPlacementV1, WorkProjection, WorkProjectionDeltaV1,
    WorkProjectionSnapshotV1, WorkRunControlV1,
};
use tracedecay_lsp::{
    LspSessionAccess, LspSessionCredential, LspSessionId, MAX_LSP_FRAME_BYTES,
    MAX_LSP_WORKSPACE_ROOTS,
};
use tracedecay_tool_catalog::{EffectClass, UseCaseId};

use crate::application_surface::{ConfigurationSurfaceRequest, ContextScoutSurfaceRequest};
use tracedecay_usecases::feedback::observations::{FeedbackDeliveryRouteV1, FeedbackSourceEventV1};
use tracedecay_usecases::primitives::PrimitiveRequest;

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
pub(crate) const DAEMON_INVOCATION_PROTOCOL: &str = "tracedecay.daemon.invocation";
/// Initial revision of the daemon-owned invocation wire shape.
pub(crate) const DAEMON_INVOCATION_REVISION: u16 = 1;
const DAEMON_INVOCATION_CANCEL_OPERATION: &str = "invocation_cancel";
const DAEMON_INVOCATION_DELIVERY_ACK_OPERATION: &str = "invocation_delivery_ack";

const MAX_INVOCATION_REQUEST_ID_BYTES: usize = 128;
const MAX_CLIENT_REVISION_BYTES: usize = 128;
const MAX_ROOT_HINT_BYTES: usize = 4_096;
const MAX_OPAQUE_HANDLE_BYTES: usize = 256;

/// A separate authenticated control frame that can interrupt an in-flight
/// read without contending on that invocation's response connection.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct DaemonInvocationCancellationRequest {
    protocol: String,
    revision: u16,
    request_id: String,
    operation: String,
    target_request_id: String,
}

impl DaemonInvocationCancellationRequest {
    pub(crate) fn new(target_request_id: impl Into<String>) -> Self {
        let target_request_id = target_request_id.into();
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: target_request_id.clone(),
            operation: DAEMON_INVOCATION_CANCEL_OPERATION.to_owned(),
            target_request_id,
        }
    }

    pub(crate) fn target_request_id(&self) -> &str {
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

pub(crate) fn parse_daemon_invocation_cancellation_request(
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
pub(crate) struct DaemonInvocationDeliveryAckRequest {
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
pub(crate) enum DaemonInvocationDeliveryAckRejectReason {
    RecorderUnavailable,
    RecorderAtCapacity,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct DaemonInvocationDeliveryAckResponse {
    protocol: String,
    revision: u16,
    request_id: String,
    operation: String,
    #[serde(flatten)]
    outcome: DaemonInvocationDeliveryAckResponseOutcome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum DaemonInvocationDeliveryAckResponseOutcome {
    Accepted,
    Rejected {
        reason: DaemonInvocationDeliveryAckRejectReason,
    },
}

impl DaemonInvocationDeliveryAckResponse {
    pub(crate) fn accepted(request_id: impl Into<String>) -> Self {
        Self::with_outcome(
            request_id,
            DaemonInvocationDeliveryAckResponseOutcome::Accepted,
        )
    }

    pub(crate) fn rejected(
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

    pub(crate) fn matches_request(&self, request_id: &str) -> bool {
        self.protocol == DAEMON_INVOCATION_PROTOCOL
            && self.revision == DAEMON_INVOCATION_REVISION
            && self.operation == DAEMON_INVOCATION_DELIVERY_ACK_OPERATION
            && self.request_id == request_id
    }

    pub(crate) fn rejection_reason(&self) -> Option<DaemonInvocationDeliveryAckRejectReason> {
        match self.outcome {
            DaemonInvocationDeliveryAckResponseOutcome::Accepted => None,
            DaemonInvocationDeliveryAckResponseOutcome::Rejected { reason } => Some(reason),
        }
    }
}

impl DaemonInvocationDeliveryAckRequest {
    pub(crate) fn delivered(target_request_id: impl Into<String>) -> Self {
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

    pub(crate) fn dropped(
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

    pub(crate) fn target_request_id(&self) -> &str {
        &self.target_request_id
    }

    pub(crate) fn outcome(
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
            && match (self.outcome, self.drop_reason) {
                (tracedecay_domain::DeliverySettlementOutcomeV1::Delivered, None) => true,
                (tracedecay_domain::DeliverySettlementOutcomeV1::Dropped, Some(_)) => true,
                _ => false,
            }
    }
}

pub(crate) fn parse_daemon_invocation_delivery_ack_request(
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
pub(crate) enum DaemonInvocationOperation {
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
    CodeDefinition,
    CodeTypeDefinition,
    CodeReferences,
    Configuration,
    ContextScout,
    RetainedApplication,
    MultiRootScopeSetRead,
    MultiRootScopeSetCompareAndSwap,
    MultiRootExecute,
    WorkApplication,
    WorkflowApplication,
    HandoffApplication,
    SemanticEvaluateAndPublish,
    LspOpen,
    LspFrame,
    LspPoll,
    LspAcknowledge,
    LspReconnect,
    LspDetach,
}

impl DaemonInvocationOperation {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::GitStatus => "git_status",
            Self::GitDiff => "git_diff",
            Self::GitHistory => "git_history",
            Self::GitBlame => "git_blame",
            Self::GitHunks => "git_hunks",
            Self::GitPreview => "git_preview",
            Self::GitApply => "git_apply",
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
            Self::PrimitiveImpact => "feedback_impact",
            Self::PrimitiveAffectedTests => "affected_tests",
            Self::PrimitiveTestResults => "test_results",
            Self::PrimitiveRead => "primitive_read",
            Self::CodeExactOccurrence => "code_exact_occurrence",
            Self::CodePhraseSearch => "code_phrase_search",
            Self::CodeCallees => "code_callees",
            Self::CodeFacets => "code_facets",
            Self::CodeTimeline => "code_timeline",
            Self::CodeDeclaration => "code_declaration",
            Self::CodeDefinition => "code_definition",
            Self::CodeTypeDefinition => "code_type_definition",
            Self::CodeReferences => "code_references",
            Self::Configuration => "configuration",
            Self::ContextScout => "context_scout",
            Self::RetainedApplication => "retained_application",
            Self::MultiRootScopeSetRead => "multi_root_scope_set_read",
            Self::MultiRootScopeSetCompareAndSwap => "multi_root_scope_set_compare_and_swap",
            Self::MultiRootExecute => "multi_root_execute",
            Self::WorkApplication => "work_application",
            Self::WorkflowApplication => "workflow_application",
            Self::HandoffApplication => "handoff_application",
            Self::SemanticEvaluateAndPublish => "semantic_evaluate_and_publish",
            Self::LspOpen => "lsp_open",
            Self::LspFrame => "lsp_frame",
            Self::LspPoll => "lsp_poll",
            Self::LspAcknowledge => "lsp_acknowledge",
            Self::LspReconnect => "lsp_reconnect",
            Self::LspDetach => "lsp_detach",
        }
    }
}

/// Credential-bearing access data exchanged only between a bridge and the
/// authenticated daemon. Its debug representation never prints the secret.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct DaemonLspSessionAccess {
    pub(crate) session_id: String,
    credential: String,
}

impl fmt::Debug for DaemonLspSessionAccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DaemonLspSessionAccess")
            .field("session_id", &self.session_id)
            .field("credential", &"[redacted]")
            .finish()
    }
}

impl DaemonLspSessionAccess {
    pub(crate) fn from_access(access: &LspSessionAccess) -> Self {
        Self {
            session_id: access.session_id().as_str().to_owned(),
            credential: hex::encode(access.credential().as_bytes()),
        }
    }

    pub(crate) fn into_access(self) -> Result<LspSessionAccess, DaemonInvocationProblem> {
        let session_id = LspSessionId::new(self.session_id)
            .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
        let credential = hex::decode(self.credential)
            .ok()
            .and_then(|credential| LspSessionCredential::new(credential).ok())
            .ok_or(DaemonInvocationProblem::InvalidRequest)?;
        Ok(LspSessionAccess::new(session_id, credential))
    }
}

// `StartAttempt` is matched and constructed across several call sites
// (work_cli, service::invocation::work); boxing it would ripple through all
// of them for a request/response contract type, not a hot allocation path.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub(crate) enum WorkApplicationInvocationV1 {
    Snapshot(WorkProjectionSnapshotRequestV1),
    Delta(WorkProjectionDeltaRequestV1),
    GenerateProposal(GenerateProposalRequest),
    Create(CreateWorkCommand),
    ReplanDependencies(ReplanDependenciesCommand),
    ReviewProposal(ReviewProposalRequestV1),
    AcceptProposal(AcceptProposalCommand),
    AdmitExecution(AdmitExecutionCommand),
    AttachRuntimeEvidence(AttachRuntimeEvidenceCommand),
    AcceptTask(AcceptTaskCommand),
    StartAttempt(StartWorkAttemptCommand),
    Synthesize(AdmitWorkSynthesisCommand),
    AttemptStatus(WorkAttemptStatusRequestV1),
    CancelAttempt(CancelWorkAttemptCommand),
    ResumeAttempts(ResumeWorkAttemptsCommand),
    RetryAttempt(RetryWorkAttemptCommandV1),
    MintRetryTestBinding(WorkRetryTestBindingTokenRequestV1),
    ListAttempts(WorkAttemptListRequestV1),
    HydrateArtifacts(WorkArtifactHydrationRequestV1),
    Views(WorkGraphReadRequestV1),
    MutateGraph(WorkProductMutationRequestV1),
    Topology(WorkTopologyViewRequestV1),
    TopologyMetrics(ExecutionTopologyMetricsRequestV1),
    AdjudicateDuplicate(WorkDuplicateAdjudicationCommandV1),
    AdjudicateLeak(AdjudicateWorkLeakCommandV1),
    PauseRun(PauseWorkRunCommand),
    ResumeRun(ResumeWorkRunCommand),
    RunControl(WorkRunControlRequestV1),
    PlacementPreflight(WorkPlacementPreflightRequestV1),
    AdmitPlacement(AdmitWorkPlacementCommand),
    PlacementStatus(WorkPlacementStatusRequestV1),
    ReleasePlacement(ReleaseWorkPlacementCommand),
}

impl WorkApplicationInvocationV1 {
    pub(crate) const fn operation_key(&self) -> &'static str {
        match self {
            Self::Snapshot(_) => "snapshot",
            Self::Delta(_) => "delta",
            Self::GenerateProposal(_) => "generate_proposal",
            Self::Create(_) => "create",
            Self::ReplanDependencies(_) => "replan_dependencies",
            Self::ReviewProposal(_) => "review_proposal",
            Self::AcceptProposal(_) => "accept_proposal",
            Self::AdmitExecution(_) => "admit_execution",
            Self::AttachRuntimeEvidence(_) => "attach_runtime_evidence",
            Self::AcceptTask(_) => "accept_task",
            Self::StartAttempt(_) => "start_attempt",
            Self::Synthesize(_) => "synthesize",
            Self::AttemptStatus(_) => "attempt_status",
            Self::CancelAttempt(_) => "cancel_attempt",
            Self::ResumeAttempts(_) => "resume_attempts",
            Self::RetryAttempt(_) => "retry_attempt",
            Self::MintRetryTestBinding(_) => "mint_retry_test_binding",
            Self::ListAttempts(_) => "list_attempts",
            Self::HydrateArtifacts(_) => "hydrate_artifacts",
            Self::Views(_) => "views",
            Self::MutateGraph(_) => "mutate_graph",
            Self::Topology(_) => "topology",
            Self::TopologyMetrics(_) => "topology_metrics",
            Self::AdjudicateDuplicate(_) => "adjudicate_duplicate",
            Self::AdjudicateLeak(_) => "adjudicate_leak",
            Self::PauseRun(_) => "pause_run",
            Self::ResumeRun(_) => "resume_run",
            Self::RunControl(_) => "run_control",
            Self::PlacementPreflight(_) => "placement_preflight",
            Self::AdmitPlacement(_) => "admit_placement",
            Self::PlacementStatus(_) => "placement_status",
            Self::ReleasePlacement(_) => "release_placement",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub(crate) enum WorkflowApplicationInvocation {
    RegisterDefinition(WorkflowDefinitionRegisterRequest),
    ActivateDefinition(WorkflowDefinitionActivateRequest),
    RetireDefinition(WorkflowDefinitionRetireRequest),
    RejectDefinition(WorkflowDefinitionRejectRequest),
    ValidateDefinition(WorkflowDefinitionValidateRequest),
    GetDefinition(WorkflowDefinitionGetRequest),
    ListDefinitions(WorkflowDefinitionListRequest),
    DefinitionHistory(WorkflowDefinitionHistoryRequest),
    DiffDefinition(WorkflowDefinitionDiffRequest),
    HandoffIssue(TaskHandoffIssueRequest),
    HandoffRedeem(TaskHandoffRedeemRequest),
    StartRun(tracedecay_application::WorkflowRunStartRequest),
    PauseRun(tracedecay_application::WorkflowRunPauseRequest),
    ResumeRun(tracedecay_application::WorkflowRunResumeRequest),
    CancelRun(tracedecay_application::WorkflowRunCancelRequest),
    GetRun(tracedecay_application::WorkflowRunGetRequest),
}

impl WorkflowApplicationInvocation {
    pub(crate) const fn operation_key(&self) -> &'static str {
        match self {
            Self::RegisterDefinition(_) => "register_definition",
            Self::ActivateDefinition(_) => "activate_definition",
            Self::RetireDefinition(_) => "retire_definition",
            Self::RejectDefinition(_) => "reject_definition",
            Self::ValidateDefinition(_) => "validate_definition",
            Self::GetDefinition(_) => "get_definition",
            Self::ListDefinitions(_) => "list_definitions",
            Self::DefinitionHistory(_) => "definition_history",
            Self::DiffDefinition(_) => "diff_definition",
            Self::HandoffIssue(_) => "handoff_issue",
            Self::HandoffRedeem(_) => "handoff_redeem",
            Self::StartRun(_) => "start_run",
            Self::PauseRun(_) => "pause_run",
            Self::ResumeRun(_) => "resume_run",
            Self::CancelRun(_) => "cancel_run",
            Self::GetRun(_) => "get_run",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub(crate) enum HandoffApplicationInvocationV1 {
    IssueTaskHandoff(IssueTaskHandoffRequestV1),
    OpenInvestigationHandoff(OpenInvestigationHandoffRequestV1),
    OpenTaskHandoff(OpenTaskHandoffRequestV1),
}

impl HandoffApplicationInvocationV1 {
    pub(crate) const fn operation_key(&self) -> &'static str {
        match self {
            Self::IssueTaskHandoff(_) => "issue_task_handoff",
            Self::OpenInvestigationHandoff(_) => "open_investigation_handoff",
            Self::OpenTaskHandoff(_) => "open_task_handoff",
        }
    }
}

/// One versioned, request-correlated daemon operation.
#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonInvocationRequest {
    pub(crate) protocol: String,
    pub(crate) revision: u16,
    pub(crate) request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) delivery_route: Option<FeedbackDeliveryRouteV1>,
    #[serde(flatten)]
    pub(crate) payload: DaemonInvocationPayload,
}

/// Operation-specific fields for the closed invocation set.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub(crate) enum DaemonInvocationPayload {
    GitRead {
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
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
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
        request: crate::application_surface::NativeIntegrationSurfaceRequest,
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
        request: tracedecay_application::retrieval::GraphImpactPrimitiveRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    PrimitiveAffectedTests {
        request: tracedecay_application::retrieval::AffectedFileTestsPrimitiveRequest,
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
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
        request: PrimitiveRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    PrimitiveCode {
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
        request: crate::application_surface::PrimitiveCodeSurfaceRequest,
        page: PageRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    CallableCode {
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
        request: crate::application_surface::CallableCodeSurfaceRequest,
        page: PageRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    Configuration {
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
        request: ConfigurationSurfaceRequest,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolved_scope: Option<ResolvedScope>,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    ContextScout {
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
        request: ContextScoutSurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    RetainedApplication {
        request: tracedecay_application::RetainedSurfaceRequestV1,
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
        request: WorkApplicationInvocationV1,
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
        candidate: Box<tracedecay_usecases::semantic_runtime::SemanticEvaluationProfileCandidateV1>,
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
}

impl DaemonInvocationRequest {
    /// One typed constructor for the whole Plan 36 native-integration journey.
    ///
    /// The transport carries exact typed identity only; it contains no Git
    /// logic and no fallback mutation path.
    pub(crate) fn native_integration(
        request_id: impl Into<String>,
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
        request: crate::application_surface::NativeIntegrationSurfaceRequest,
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

    pub(crate) fn feedback(
        request_id: impl Into<String>,
        operation: crate::application_surface::ApplicationSurfaceOperation,
        request_handle: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        let payload = match operation {
            crate::application_surface::ApplicationSurfaceOperation::FeedbackDiagnostics => {
                DaemonInvocationPayload::FeedbackDiagnostics {
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                }
            }
            crate::application_surface::ApplicationSurfaceOperation::FeedbackGet => {
                DaemonInvocationPayload::FeedbackGet {
                    request_handle,
                    resolved_scope: None,
                    observed_at,
                    deadline,
                    cancellation,
                }
            }
            crate::application_surface::ApplicationSurfaceOperation::FeedbackExpand => {
                DaemonInvocationPayload::FeedbackExpand {
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                }
            }
            crate::application_surface::ApplicationSurfaceOperation::FeedbackList => {
                DaemonInvocationPayload::FeedbackList {
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                }
            }
            crate::application_surface::ApplicationSurfaceOperation::FeedbackImpact => {
                DaemonInvocationPayload::FeedbackImpact {
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                }
            }
            crate::application_surface::ApplicationSurfaceOperation::AffectedTests => {
                DaemonInvocationPayload::AffectedTests {
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                }
            }
            crate::application_surface::ApplicationSurfaceOperation::TestResults
            | crate::application_surface::ApplicationSurfaceOperation::FeedbackAdvisoryCycle
            | crate::application_surface::ApplicationSurfaceOperation::SessionLookup
            | crate::application_surface::ApplicationSurfaceOperation::QualifiedName
            | crate::application_surface::ApplicationSurfaceOperation::CallChain
            | crate::application_surface::ApplicationSurfaceOperation::FileDependents
            | crate::application_surface::ApplicationSurfaceOperation::SourceLines
            | crate::application_surface::ApplicationSurfaceOperation::SourceBody
            | crate::application_surface::ApplicationSurfaceOperation::SourceOutline
            | crate::application_surface::ApplicationSurfaceOperation::ModuleApi
            | crate::application_surface::ApplicationSurfaceOperation::FileMetadata
            | crate::application_surface::ApplicationSurfaceOperation::HealthRead
            | crate::application_surface::ApplicationSurfaceOperation::HealthDelta
            | crate::application_surface::ApplicationSurfaceOperation::StorageStatus
            | crate::application_surface::ApplicationSurfaceOperation::DiagnosticsRead
            | crate::application_surface::ApplicationSurfaceOperation::CodeSymbolSearch
            | crate::application_surface::ApplicationSurfaceOperation::CodeSignatureSearch
            | crate::application_surface::ApplicationSurfaceOperation::CodeImplementations
            | crate::application_surface::ApplicationSurfaceOperation::CodeTypeHierarchy
            | crate::application_surface::ApplicationSurfaceOperation::CodeCallers => {
                unreachable!("primitive operations use their typed constructor")
            }
            crate::application_surface::ApplicationSurfaceOperation::CodeExactOccurrence
            | crate::application_surface::ApplicationSurfaceOperation::CodePhraseSearch
            | crate::application_surface::ApplicationSurfaceOperation::CodeCallees
            | crate::application_surface::ApplicationSurfaceOperation::CodeFacets
            | crate::application_surface::ApplicationSurfaceOperation::CodeTimeline
            | crate::application_surface::ApplicationSurfaceOperation::CodeDeclaration
            | crate::application_surface::ApplicationSurfaceOperation::CodeDefinition
            | crate::application_surface::ApplicationSurfaceOperation::CodeTypeDefinition
            | crate::application_surface::ApplicationSurfaceOperation::CodeReferences => {
                unreachable!("callable code operations use their typed constructor")
            }
            crate::application_surface::ApplicationSurfaceOperation::GitStatus
            | crate::application_surface::ApplicationSurfaceOperation::GitDiff
            | crate::application_surface::ApplicationSurfaceOperation::GitHistory
            | crate::application_surface::ApplicationSurfaceOperation::GitBlame
            | crate::application_surface::ApplicationSurfaceOperation::GitHunks
            | crate::application_surface::ApplicationSurfaceOperation::GitPreview
            | crate::application_surface::ApplicationSurfaceOperation::GitApply => {
                unreachable!("Git operations use their typed constructors")
            }
            crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationStackSnapshot
            | crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationPreflight
            | crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationApprove
            | crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationApply
            | crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationStatus
            | crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationCancel => {
                unreachable!("native-integration operations use their typed constructor")
            }
            crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationWorktreeInventory
            | crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationWorktreeInspect
            | crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationWorktreeConfirm
            | crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove
            | crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationWorktreeReconcile => {
                unreachable!("native worktree operations use their typed constructor")
            }
            crate::application_surface::ApplicationSurfaceOperation::ConfigurationList
            | crate::application_surface::ApplicationSurfaceOperation::ConfigurationExplain
            | crate::application_surface::ApplicationSurfaceOperation::ConfigurationGet
            | crate::application_surface::ApplicationSurfaceOperation::ConfigurationSet
            | crate::application_surface::ApplicationSurfaceOperation::ConfigurationUnset
            | crate::application_surface::ApplicationSurfaceOperation::ConfigurationBatch
            | crate::application_surface::ApplicationSurfaceOperation::ConfigurationWriteCredential
            | crate::application_surface::ApplicationSurfaceOperation::ConfigurationObservedState
            | crate::application_surface::ApplicationSurfaceOperation::ConfigurationProtectedPreview
            | crate::application_surface::ApplicationSurfaceOperation::ConfigurationProtectedApply
            | crate::application_surface::ApplicationSurfaceOperation::ConfigurationRollbackPreview
            | crate::application_surface::ApplicationSurfaceOperation::ConfigurationRollbackApply
            | crate::application_surface::ApplicationSurfaceOperation::ConfigurationAudit
            | crate::application_surface::ApplicationSurfaceOperation::SemanticModelRetry
            | crate::application_surface::ApplicationSurfaceOperation::SemanticModelRemove
            | crate::application_surface::ApplicationSurfaceOperation::SemanticModelRollback
            | crate::application_surface::ApplicationSurfaceOperation::SemanticEmbeddingImportLocal
            | crate::application_surface::ApplicationSurfaceOperation::SemanticEmbeddingImportConfiguredHttps
            | crate::application_surface::ApplicationSurfaceOperation::SemanticRerankerImportLocal
            | crate::application_surface::ApplicationSurfaceOperation::SemanticRerankerImportConfiguredHttps
            | crate::application_surface::ApplicationSurfaceOperation::SemanticRerankerRollback => {
                unreachable!("configuration operations use their typed constructor")
            }
            crate::application_surface::ApplicationSurfaceOperation::ContextScoutStatus
            | crate::application_surface::ApplicationSurfaceOperation::ContextScoutRecent
            | crate::application_surface::ApplicationSurfaceOperation::ContextScoutExplain
            | crate::application_surface::ApplicationSurfaceOperation::ContextScoutCapability
            | crate::application_surface::ApplicationSurfaceOperation::ContextScoutBudget
            | crate::application_surface::ApplicationSurfaceOperation::ContextScoutPause
            | crate::application_surface::ApplicationSurfaceOperation::ContextScoutResume
            | crate::application_surface::ApplicationSurfaceOperation::ContextScoutCancel
            | crate::application_surface::ApplicationSurfaceOperation::ContextScoutClaim
            | crate::application_surface::ApplicationSurfaceOperation::ContextScoutDelivery
            | crate::application_surface::ApplicationSurfaceOperation::ContextScoutFeedback => {
                unreachable!("Context Scout operations use their typed constructor")
            }
            crate::application_surface::ApplicationSurfaceOperation::FactStoreAdd
            | crate::application_surface::ApplicationSurfaceOperation::FactStoreSearch
            | crate::application_surface::ApplicationSurfaceOperation::FactStoreProbe
            | crate::application_surface::ApplicationSurfaceOperation::FactStoreRelated
            | crate::application_surface::ApplicationSurfaceOperation::FactStoreReason
            | crate::application_surface::ApplicationSurfaceOperation::FactStoreContradict
            | crate::application_surface::ApplicationSurfaceOperation::FactStoreGet
            | crate::application_surface::ApplicationSurfaceOperation::FactStoreUpdate
            | crate::application_surface::ApplicationSurfaceOperation::FactStoreRemove
            | crate::application_surface::ApplicationSurfaceOperation::FactStoreList
            | crate::application_surface::ApplicationSurfaceOperation::FactFeedback
            | crate::application_surface::ApplicationSurfaceOperation::MemoryStatus
            | crate::application_surface::ApplicationSurfaceOperation::SessionRefreshStatus
            | crate::application_surface::ApplicationSurfaceOperation::SessionRefreshCancel
            | crate::application_surface::ApplicationSurfaceOperation::SessionRefreshBegin
            | crate::application_surface::ApplicationSurfaceOperation::MessageSearch
            | crate::application_surface::ApplicationSurfaceOperation::SessionsFor
            | crate::application_surface::ApplicationSurfaceOperation::Workflows
            | crate::application_surface::ApplicationSurfaceOperation::LcmStatus
            | crate::application_surface::ApplicationSurfaceOperation::LcmDoctor
            | crate::application_surface::ApplicationSurfaceOperation::LcmLoadSession
            | crate::application_surface::ApplicationSurfaceOperation::LcmGrep
            | crate::application_surface::ApplicationSurfaceOperation::LcmDescribe
            | crate::application_surface::ApplicationSurfaceOperation::LcmExpand
            | crate::application_surface::ApplicationSurfaceOperation::LcmExpandQuery => {
                unreachable!("retained operations use their typed constructor")
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

    pub(crate) fn feedback_advisory_cycle(
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

    pub(crate) fn feedback_observation(
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

    pub(crate) fn primitive(
        request_id: impl Into<String>,
        operation: crate::application_surface::ApplicationSurfaceOperation,
        request: PrimitiveRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        let payload = match (operation, request) {
            (
                crate::application_surface::ApplicationSurfaceOperation::FeedbackImpact,
                PrimitiveRequest::Impact(request),
            ) => DaemonInvocationPayload::PrimitiveImpact {
                request,
                observed_at,
                deadline,
                cancellation,
            },
            (
                crate::application_surface::ApplicationSurfaceOperation::AffectedTests,
                PrimitiveRequest::AffectedFileTests(request),
            ) => DaemonInvocationPayload::PrimitiveAffectedTests {
                request,
                observed_at,
                deadline,
                cancellation,
            },
            (
                crate::application_surface::ApplicationSurfaceOperation::TestResults,
                PrimitiveRequest::RecentTestResults(page),
            ) => DaemonInvocationPayload::PrimitiveTestResults {
                page,
                observed_at,
                deadline,
                cancellation,
            },
            (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::SessionLookup,
                request @ PrimitiveRequest::SessionLookup(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::QualifiedName,
                request @ PrimitiveRequest::QualifiedName(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::CallChain,
                request @ PrimitiveRequest::CallChain(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::FileDependents,
                request @ PrimitiveRequest::FileDependents(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::SourceLines,
                request @ PrimitiveRequest::SourceLines(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::SourceBody,
                request @ PrimitiveRequest::SourceBody(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::SourceOutline,
                request @ PrimitiveRequest::SourceOutline(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::ModuleApi,
                request @ PrimitiveRequest::ModuleApi(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::FileMetadata,
                request @ PrimitiveRequest::FileMetadata(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::HealthRead,
                request @ PrimitiveRequest::HealthRead(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::HealthDelta,
                request @ PrimitiveRequest::HealthDelta(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::StorageStatus,
                request @ PrimitiveRequest::StorageStatus(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::DiagnosticsRead,
                request @ PrimitiveRequest::DiagnosticsRead(_),
            ) => {
                DaemonInvocationPayload::PrimitiveRead {
                    surface_operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                }
            }
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

    pub(crate) fn configuration(
        request_id: impl Into<String>,
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
        request: ConfigurationSurfaceRequest,
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

    pub(crate) fn context_scout(
        request_id: impl Into<String>,
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
        request: ContextScoutSurfaceRequest,
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

    pub(crate) fn retained_application(
        request_id: impl Into<String>,
        request: tracedecay_application::RetainedSurfaceRequestV1,
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

    pub(crate) fn multi_root_scope_set_read(
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

    pub(crate) fn multi_root_scope_set_compare_and_swap(
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

    pub(crate) fn multi_root_execute(
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

    pub(crate) fn work_application(
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
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub(crate) fn workflow_application(
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

    pub(crate) fn handoff_application(
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

    pub(crate) fn semantic_evaluate_and_publish(
        request_id: impl Into<String>,
        candidate: tracedecay_usecases::semantic_runtime::SemanticEvaluationProfileCandidateV1,
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
                candidate: Box::new(candidate),
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub(crate) fn callable_code(
        request_id: impl Into<String>,
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
        request: crate::application_surface::CallableCodeSurfaceRequest,
        page: PageRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        debug_assert!(matches!(
            (&request, surface_operation),
            (
                crate::application_surface::CallableCodeSurfaceRequest::ExactOccurrence(_),
                crate::application_surface::ApplicationSurfaceOperation::CodeExactOccurrence,
            ) | (
                crate::application_surface::CallableCodeSurfaceRequest::PhraseSearch(_),
                crate::application_surface::ApplicationSurfaceOperation::CodePhraseSearch,
            ) | (
                crate::application_surface::CallableCodeSurfaceRequest::Callees(_),
                crate::application_surface::ApplicationSurfaceOperation::CodeCallees,
            ) | (
                crate::application_surface::CallableCodeSurfaceRequest::Facets(_),
                crate::application_surface::ApplicationSurfaceOperation::CodeFacets,
            ) | (
                crate::application_surface::CallableCodeSurfaceRequest::Timeline(_),
                crate::application_surface::ApplicationSurfaceOperation::CodeTimeline,
            ) | (
                crate::application_surface::CallableCodeSurfaceRequest::Declaration(_),
                crate::application_surface::ApplicationSurfaceOperation::CodeDeclaration,
            ) | (
                crate::application_surface::CallableCodeSurfaceRequest::Definition(_),
                crate::application_surface::ApplicationSurfaceOperation::CodeDefinition,
            ) | (
                crate::application_surface::CallableCodeSurfaceRequest::TypeDefinition(_),
                crate::application_surface::ApplicationSurfaceOperation::CodeTypeDefinition,
            ) | (
                crate::application_surface::CallableCodeSurfaceRequest::References(_),
                crate::application_surface::ApplicationSurfaceOperation::CodeReferences,
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

    pub(crate) fn primitive_code(
        request_id: impl Into<String>,
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
        request: crate::application_surface::PrimitiveCodeSurfaceRequest,
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

    pub(crate) fn lsp_open(
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

    pub(crate) fn lsp_frame(
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

    pub(crate) fn lsp_poll(
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

    pub(crate) fn lsp_acknowledge(
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

    pub(crate) fn lsp_detach(
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

    pub(crate) fn lsp_reconnect(
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

    pub(crate) fn with_delivery_route(mut self, route: FeedbackDeliveryRouteV1) -> Self {
        self.delivery_route = Some(route);
        self
    }

    pub(crate) fn with_resolved_scope(mut self, scope: Option<ResolvedScope>) -> Self {
        match &mut self.payload {
            DaemonInvocationPayload::FeedbackGet { resolved_scope, .. }
            | DaemonInvocationPayload::Configuration { resolved_scope, .. } => {
                *resolved_scope = scope;
            }
            _ => {}
        }
        self
    }

    pub(crate) fn lsp_workspace_folders(&self) -> Option<&[String]> {
        match &self.payload {
            DaemonInvocationPayload::LspOpen {
                workspace_folders, ..
            } => Some(workspace_folders),
            _ => None,
        }
    }

    pub(crate) fn lsp_open_control(&self) -> Option<(&Deadline, &CancellationContext)> {
        match &self.payload {
            DaemonInvocationPayload::LspOpen {
                deadline,
                cancellation,
                ..
            } => Some((deadline, cancellation)),
            _ => None,
        }
    }

    pub(crate) fn operation(&self) -> DaemonInvocationOperation {
        match self.payload {
            DaemonInvocationPayload::GitRead {
                surface_operation, ..
            } => match surface_operation {
                crate::application_surface::ApplicationSurfaceOperation::GitStatus => {
                    DaemonInvocationOperation::GitStatus
                }
                crate::application_surface::ApplicationSurfaceOperation::GitDiff => {
                    DaemonInvocationOperation::GitDiff
                }
                crate::application_surface::ApplicationSurfaceOperation::GitHistory => {
                    DaemonInvocationOperation::GitHistory
                }
                crate::application_surface::ApplicationSurfaceOperation::GitBlame => {
                    DaemonInvocationOperation::GitBlame
                }
                crate::application_surface::ApplicationSurfaceOperation::GitHunks => {
                    DaemonInvocationOperation::GitHunks
                }
                _ => unreachable!("Git read payloads use a Git read surface operation"),
            },
            DaemonInvocationPayload::GitPreview { .. } => DaemonInvocationOperation::GitPreview,
            DaemonInvocationPayload::GitHubStackSignalExpand { .. } => {
                DaemonInvocationOperation::GitHubStackSignalExpand
            },
            DaemonInvocationPayload::GitApply { .. } => DaemonInvocationOperation::GitApply,
            DaemonInvocationPayload::NativeIntegration {
                surface_operation, ..
            } => match surface_operation {
                crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationStackSnapshot => {
                    DaemonInvocationOperation::NativeIntegrationStackSnapshot
                }
                crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationPreflight => {
                    DaemonInvocationOperation::NativeIntegrationPreflight
                }
                crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationApprove => {
                    DaemonInvocationOperation::NativeIntegrationApprove
                }
                crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationApply => {
                    DaemonInvocationOperation::NativeIntegrationApply
                }
                crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationStatus => {
                    DaemonInvocationOperation::NativeIntegrationStatus
                }
                crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationCancel => {
                    DaemonInvocationOperation::NativeIntegrationCancel
                }
                crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationWorktreeInventory => {
                    DaemonInvocationOperation::NativeIntegrationWorktreeInventory
                }
                crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationWorktreeInspect => {
                    DaemonInvocationOperation::NativeIntegrationWorktreeInspect
                }
                crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationWorktreeConfirm => {
                    DaemonInvocationOperation::NativeIntegrationWorktreeConfirm
                }
                crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove => {
                    DaemonInvocationOperation::NativeIntegrationWorktreeRemove
                }
                crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationWorktreeReconcile => {
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
                request: crate::application_surface::CallableCodeSurfaceRequest::ExactOccurrence(_),
                ..
            } => DaemonInvocationOperation::CodeExactOccurrence,
            DaemonInvocationPayload::CallableCode {
                request: crate::application_surface::CallableCodeSurfaceRequest::PhraseSearch(_),
                ..
            } => DaemonInvocationOperation::CodePhraseSearch,
            DaemonInvocationPayload::CallableCode {
                request: crate::application_surface::CallableCodeSurfaceRequest::Callees(_),
                ..
            } => DaemonInvocationOperation::CodeCallees,
            DaemonInvocationPayload::CallableCode {
                request: crate::application_surface::CallableCodeSurfaceRequest::Facets(_),
                ..
            } => DaemonInvocationOperation::CodeFacets,
            DaemonInvocationPayload::CallableCode {
                request: crate::application_surface::CallableCodeSurfaceRequest::Timeline(_),
                ..
            } => DaemonInvocationOperation::CodeTimeline,
            DaemonInvocationPayload::CallableCode {
                request: crate::application_surface::CallableCodeSurfaceRequest::Declaration(_),
                ..
            } => DaemonInvocationOperation::CodeDeclaration,
            DaemonInvocationPayload::CallableCode {
                request: crate::application_surface::CallableCodeSurfaceRequest::Definition(_),
                ..
            } => DaemonInvocationOperation::CodeDefinition,
            DaemonInvocationPayload::CallableCode {
                request: crate::application_surface::CallableCodeSurfaceRequest::TypeDefinition(_),
                ..
            } => DaemonInvocationOperation::CodeTypeDefinition,
            DaemonInvocationPayload::CallableCode {
                request: crate::application_surface::CallableCodeSurfaceRequest::References(_),
                ..
            } => DaemonInvocationOperation::CodeReferences,
            DaemonInvocationPayload::Configuration { .. } => {
                DaemonInvocationOperation::Configuration
            }
            DaemonInvocationPayload::ContextScout { .. } => DaemonInvocationOperation::ContextScout,
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
            DaemonInvocationPayload::LspOpen { .. } => DaemonInvocationOperation::LspOpen,
            DaemonInvocationPayload::LspFrame { .. } => DaemonInvocationOperation::LspFrame,
            DaemonInvocationPayload::LspPoll { .. } => DaemonInvocationOperation::LspPoll,
            DaemonInvocationPayload::LspAcknowledge { .. } => {
                DaemonInvocationOperation::LspAcknowledge
            }
            DaemonInvocationPayload::LspReconnect { .. } => DaemonInvocationOperation::LspReconnect,
            DaemonInvocationPayload::LspDetach { .. } => DaemonInvocationOperation::LspDetach,
        }
    }

    pub(crate) fn requires_project(&self) -> bool {
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
                | DaemonInvocationOperation::CodeDefinition
                | DaemonInvocationOperation::CodeTypeDefinition
                | DaemonInvocationOperation::CodeReferences
                | DaemonInvocationOperation::Configuration
                | DaemonInvocationOperation::ContextScout
                | DaemonInvocationOperation::RetainedApplication
                | DaemonInvocationOperation::MultiRootScopeSetRead
                | DaemonInvocationOperation::MultiRootScopeSetCompareAndSwap
                | DaemonInvocationOperation::MultiRootExecute
                | DaemonInvocationOperation::WorkApplication
                | DaemonInvocationOperation::WorkflowApplication
                | DaemonInvocationOperation::HandoffApplication
                | DaemonInvocationOperation::SemanticEvaluateAndPublish
                | DaemonInvocationOperation::LspOpen
        )
    }

    pub(crate) fn is_workflow_application(&self) -> bool {
        matches!(
            &self.payload,
            DaemonInvocationPayload::WorkflowApplication { .. }
        )
    }

    /// The caller's immutable budget also bounds the terminal delivery ACK.
    /// Work output must not hold an authenticated connection past the
    /// invocation's own deadline when a surface disappears before ACKing.
    pub(crate) fn delivery_ack_deadline(&self) -> Option<&Deadline> {
        match &self.payload {
            DaemonInvocationPayload::WorkApplication { deadline, .. } => Some(deadline),
            _ => None,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), DaemonInvocationProblem> {
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
            } => {
                if observed_at.0 <= 0
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
                        crate::application_surface::ApplicationSurfaceOperation::CodeSymbolSearch,
                        crate::application_surface::PrimitiveCodeSurfaceRequest::SymbolSearch(_),
                    ) | (
                        crate::application_surface::ApplicationSurfaceOperation::CodeSignatureSearch,
                        crate::application_surface::PrimitiveCodeSurfaceRequest::SignatureSearch(_),
                    ) | (
                        crate::application_surface::ApplicationSurfaceOperation::CodeImplementations,
                        crate::application_surface::PrimitiveCodeSurfaceRequest::Implementations(_),
                    ) | (
                        crate::application_surface::ApplicationSurfaceOperation::CodeTypeHierarchy,
                        crate::application_surface::PrimitiveCodeSurfaceRequest::TypeHierarchy(_),
                    ) | (
                        crate::application_surface::ApplicationSurfaceOperation::CodeCallers,
                        crate::application_surface::PrimitiveCodeSurfaceRequest::Callers(_),
                    )
                );
                if !matches {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::SemanticEvaluateAndPublish {
                candidate,
                observed_at,
                deadline,
                cancellation,
            } => {
                if candidate.evaluated_profile_id.trim() != candidate.evaluated_profile_id
                    || candidate.evaluated_profile_id.is_empty()
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
                        crate::application_surface::ApplicationSurfaceOperation::CodeExactOccurrence,
                        crate::application_surface::CallableCodeSurfaceRequest::ExactOccurrence(_),
                    ) | (
                        crate::application_surface::ApplicationSurfaceOperation::CodePhraseSearch,
                        crate::application_surface::CallableCodeSurfaceRequest::PhraseSearch(_),
                    ) | (
                        crate::application_surface::ApplicationSurfaceOperation::CodeCallees,
                        crate::application_surface::CallableCodeSurfaceRequest::Callees(_),
                    ) | (
                        crate::application_surface::ApplicationSurfaceOperation::CodeFacets,
                        crate::application_surface::CallableCodeSurfaceRequest::Facets(_),
                    ) | (
                        crate::application_surface::ApplicationSurfaceOperation::CodeTimeline,
                        crate::application_surface::CallableCodeSurfaceRequest::Timeline(_),
                    ) | (
                        crate::application_surface::ApplicationSurfaceOperation::CodeDeclaration,
                        crate::application_surface::CallableCodeSurfaceRequest::Declaration(_),
                    ) | (
                        crate::application_surface::ApplicationSurfaceOperation::CodeDefinition,
                        crate::application_surface::CallableCodeSurfaceRequest::Definition(_),
                    ) | (
                        crate::application_surface::ApplicationSurfaceOperation::CodeTypeDefinition,
                        crate::application_surface::CallableCodeSurfaceRequest::TypeDefinition(_),
                    ) | (
                        crate::application_surface::ApplicationSurfaceOperation::CodeReferences,
                        crate::application_surface::CallableCodeSurfaceRequest::References(_),
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
                    || !request.matches_operation(surface_operation.as_str())
                    || matches!(
                        request,
                        ContextScoutSurfaceRequest::Recent(request)
                            | ContextScoutSurfaceRequest::Explain(request)
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
pub(crate) fn parse_daemon_invocation_request(
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
    Some(serde_json::from_value(value).map_err(|_| {
        DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::InvalidRequest)
    }))
}

/// A safe, deliberately non-diagnostic daemon invocation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DaemonInvocationProblem {
    InvalidRequest,
    UnsupportedRevision,
    NotFoundOrNotAuthorized,
    ResetRequired,
    Unavailable,
}

/// Response envelope paired with one invocation request id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DaemonInvocationResponse {
    pub(crate) protocol: String,
    pub(crate) revision: u16,
    pub(crate) request_id: String,
    #[serde(flatten)]
    pub(crate) outcome: DaemonInvocationOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)]
enum DaemonGitEffectClass {
    IndexStage,
    IndexUnstage,
    IndexCommit,
}

impl DaemonGitEffectClass {
    fn from_application(effect: EffectClass) -> Result<Self, ApplicationContractError> {
        match effect {
            EffectClass::GitIndexStage => Ok(Self::IndexStage),
            EffectClass::GitIndexUnstage => Ok(Self::IndexUnstage),
            EffectClass::GitIndexCommit => Ok(Self::IndexCommit),
            _ => Err(ApplicationContractError::Inconsistent {
                field: "daemon Git effect class",
            }),
        }
    }

    const fn into_application(self) -> EffectClass {
        match self {
            Self::IndexStage => EffectClass::GitIndexStage,
            Self::IndexUnstage => EffectClass::GitIndexUnstage,
            Self::IndexCommit => EffectClass::GitIndexCommit,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct DaemonGitPreviewResult {
    preview_id: PreviewId,
    preview_digest: ManifestDigest,
    effect_class: DaemonGitEffectClass,
    authority: AuthorityReceipt,
    expected_state: ManifestDigest,
    execution: OperationReceipt,
    payload: Option<GitIndexPreviewV1>,
}

impl DaemonGitPreviewResult {
    pub(crate) const fn execution(&self) -> &OperationReceipt {
        &self.execution
    }

    pub(crate) fn from_application(
        result: PreviewResult<GitIndexPreviewV1>,
    ) -> Result<Self, ApplicationContractError> {
        Ok(Self {
            preview_id: result.preview_id,
            preview_digest: result.preview_digest,
            effect_class: DaemonGitEffectClass::from_application(result.effect_class)?,
            authority: result.authority,
            expected_state: result.expected_state,
            execution: result.execution,
            payload: result.payload,
        })
    }

    pub(crate) fn into_application_result(
        self,
    ) -> Result<PreviewResult<serde_json::Value>, ApplicationContractError> {
        PreviewResult::new(
            self.preview_id,
            self.preview_digest,
            self.effect_class.into_application(),
            self.authority,
            self.expected_state,
            self.execution,
            self.payload
                .map(serde_json::to_value)
                .transpose()
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "git preview response payload",
                })?,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct DaemonEffectReceipt {
    operation: UseCaseId,
    request_id: RequestId,
    actor: ActorId,
    scope: ResolvedScope,
    effect_class: DaemonGitEffectClass,
    idempotency_key: IdempotencyKey,
    input_digest: ManifestDigest,
    expected_state: ManifestDigest,
    policy_digest: ManifestDigest,
    configuration_digest: ManifestDigest,
    catalog_digest: ManifestDigest,
    privacy_digest: ManifestDigest,
    outcome: tracedecay_application::EffectTermination,
    committed_state: Option<ManifestDigest>,
    external_proof: Option<RetrievalAnchorId>,
}

impl From<EffectReceipt> for DaemonEffectReceipt {
    fn from(receipt: EffectReceipt) -> Self {
        Self {
            operation: receipt.operation,
            request_id: receipt.request_id,
            actor: receipt.actor,
            scope: receipt.scope,
            effect_class: DaemonGitEffectClass::from_application(receipt.effect_class)
                .unwrap_or_else(|_| {
                    panic!("Git effect receipt class is validated by the application service")
                }),
            idempotency_key: receipt.idempotency_key,
            input_digest: receipt.input_digest,
            expected_state: receipt.expected_state,
            policy_digest: receipt.policy_digest,
            configuration_digest: receipt.configuration_digest,
            catalog_digest: receipt.catalog_digest,
            privacy_digest: receipt.privacy_digest,
            outcome: receipt.outcome,
            committed_state: receipt.committed_state,
            external_proof: receipt.external_proof,
        }
    }
}

impl DaemonEffectReceipt {
    fn into_application(self) -> EffectReceipt {
        EffectReceipt {
            operation: self.operation,
            request_id: self.request_id,
            actor: self.actor,
            scope: self.scope,
            effect_class: self.effect_class.into_application(),
            idempotency_key: self.idempotency_key,
            input_digest: self.input_digest,
            expected_state: self.expected_state,
            policy_digest: self.policy_digest,
            configuration_digest: self.configuration_digest,
            catalog_digest: self.catalog_digest,
            privacy_digest: self.privacy_digest,
            outcome: self.outcome,
            committed_state: self.committed_state,
            external_proof: self.external_proof,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct DaemonGitEffectResult {
    effect_id: EffectId,
    effect_class: DaemonGitEffectClass,
    idempotency_key: IdempotencyKey,
    authority: AuthorityReceipt,
    expected_state: ManifestDigest,
    execution: OperationReceipt,
    reconciliation: ReconciliationState,
    receipt: DaemonEffectReceipt,
    payload: Option<GitIndexTransactionReceiptV1>,
}

impl DaemonGitEffectResult {
    pub(crate) const fn execution(&self) -> &OperationReceipt {
        &self.execution
    }

    pub(crate) fn from_application(
        result: EffectResult<GitIndexTransactionReceiptV1>,
    ) -> Result<Self, ApplicationContractError> {
        Ok(Self {
            effect_id: result.effect_id,
            effect_class: DaemonGitEffectClass::from_application(result.effect_class)?,
            idempotency_key: result.idempotency_key,
            authority: result.authority,
            expected_state: result.expected_state,
            execution: result.execution,
            reconciliation: result.reconciliation,
            receipt: result.receipt.into(),
            payload: result.payload,
        })
    }

    pub(crate) fn into_application_result(
        self,
    ) -> Result<EffectResult<serde_json::Value>, ApplicationContractError> {
        EffectResult::new(
            self.effect_id,
            self.effect_class.into_application(),
            self.idempotency_key,
            self.authority,
            self.expected_state,
            self.execution,
            self.reconciliation,
            self.receipt.into_application(),
            self.payload
                .map(serde_json::to_value)
                .transpose()
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "git apply response payload",
                })?,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct DaemonFeedbackResult {
    temporal: TemporalState,
    authority: AuthorityReceipt,
    evidence_authorities: Vec<EvidenceAuthority>,
    coverage: EvidenceCoverage,
    omissions: Vec<Omission>,
    scores: Vec<EvidenceScore>,
    contributions: Vec<RetrieverContribution>,
    page: PageState,
    execution: OperationReceipt,
    payload: Option<serde_json::Value>,
}

impl DaemonFeedbackResult {
    /// Read-only views for the daemon's operation accounting. The fields stay
    /// private so the envelope can only be built from an application packet.
    pub(crate) const fn execution(&self) -> &OperationReceipt {
        &self.execution
    }

    pub(crate) const fn page(&self) -> &PageState {
        &self.page
    }

    pub(crate) fn from_application(packet: EvidencePacket<serde_json::Value>) -> Self {
        Self {
            temporal: packet.temporal,
            authority: packet.authority,
            evidence_authorities: packet.evidence_authorities,
            coverage: packet.coverage,
            omissions: packet.omissions,
            scores: packet.scores,
            contributions: packet.contributions,
            page: packet.page,
            execution: packet.execution,
            payload: packet.payload,
        }
    }

    pub(crate) fn into_application(self) -> EvidencePacket<serde_json::Value> {
        EvidencePacket {
            temporal: self.temporal,
            authority: self.authority,
            evidence_authorities: self.evidence_authorities,
            coverage: self.coverage,
            omissions: self.omissions,
            scores: self.scores,
            contributions: self.contributions,
            page: self.page,
            execution: self.execution,
            payload: self.payload,
        }
    }
}

/// Bounded operation outcomes. LSP payloads remain protocol frames, not an
/// unrestricted stream or arbitrary daemon-socket response.
// `WorkApplication` is matched and constructed across two dozen call sites
// (work_cli, application_surface, service::invocation::work and its tests);
// boxing it would ripple through all of them for a wire contract type.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum DaemonInvocationOutcome {
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
    RetainedApplication {
        scope: ResolvedScope,
        outcome: ApplicationOutcome<tracedecay_application::RetainedSurfaceResultV1>,
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
        outcome:
            ApplicationOutcome<tracedecay_application::MultiRootQueryPageV1<serde_json::Value>>,
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
        report: crate::search_eval::DirectEvaluationReportV1,
        source_generation: tracedecay_domain::CodeGenerationId,
        snapshot_digest: ManifestDigest,
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
    Problem {
        problem: DaemonInvocationProblem,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "outcome", rename_all = "snake_case")]
pub(crate) enum WorkApplicationOutcomeV1 {
    Snapshot(ApplicationOutcome<WorkProjectionSnapshotV1>),
    Delta(ApplicationOutcome<WorkProjectionDeltaV1>),
    GenerateProposal(ApplicationOutcome<GeneratedWorkProposal>),
    Create(ApplicationOutcome<WorkProjection>),
    ReplanDependencies(ApplicationOutcome<WorkProjection>),
    ReviewProposal(ApplicationOutcome<WorkProjection>),
    AcceptProposal(ApplicationOutcome<WorkProjection>),
    AdmitExecution(ApplicationOutcome<WorkProjection>),
    AttachRuntimeEvidence(ApplicationOutcome<WorkProjection>),
    AcceptTask(ApplicationOutcome<WorkProjection>),
    StartAttempt(ApplicationOutcome<WorkAttemptV1>),
    Synthesize(ApplicationOutcome<WorkSynthesisAttemptV1>),
    AttemptStatus(ApplicationOutcome<WorkAttemptV1>),
    CancelAttempt(ApplicationOutcome<WorkAttemptV1>),
    ResumeAttempts(ApplicationOutcome<WorkAttemptRecoveryReportV1>),
    RetryAttempt(ApplicationOutcome<tracedecay_application::WorkRetryAttemptOutcomeV1>),
    MintRetryTestBinding(ApplicationOutcome<WorkRetryTestBindingTokenOutcomeV1>),
    ListAttempts(ApplicationOutcome<WorkAttemptListV1>),
    HydrateArtifacts(ApplicationOutcome<WorkArtifactHydrationV1>),
    Views(ApplicationOutcome<WorkGraphReadV1>),
    MutateGraph(ApplicationOutcome<WorkProductMutationReceiptV1>),
    Topology(ApplicationOutcome<ExecutionTopologyViewV1>),
    TopologyMetrics(ApplicationOutcome<ExecutionTopologyMetricsV1>),
    AdjudicateDuplicate(ApplicationOutcome<WorkDuplicateAdjudicationAppendOutcomeV1>),
    AdjudicateLeak(ApplicationOutcome<WorkLeakAdjudicationOutcomeV1>),
    PauseRun(ApplicationOutcome<WorkRunControlV1>),
    ResumeRun(ApplicationOutcome<WorkRunControlV1>),
    RunControl(ApplicationOutcome<WorkRunControlReadingV1>),
    PlacementPreflight(ApplicationOutcome<WorkPlacementPreflightV1>),
    AdmitPlacement(ApplicationOutcome<WorkPlacementV1>),
    PlacementStatus(ApplicationOutcome<WorkPlacementReadingV1>),
    ReleasePlacement(ApplicationOutcome<WorkPlacementV1>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "outcome", rename_all = "snake_case")]
pub(crate) enum WorkflowApplicationOutcome {
    RegisterDefinition(ApplicationOutcome<tracedecay_domain::WorkflowDefinition>),
    ActivateDefinition(ApplicationOutcome<WorkflowDefinitionDisposition>),
    RetireDefinition(ApplicationOutcome<WorkflowDefinitionDisposition>),
    RejectDefinition(ApplicationOutcome<WorkflowDefinitionDisposition>),
    ValidateDefinition(ApplicationOutcome<WorkflowDefinitionValidation>),
    GetDefinition(ApplicationOutcome<tracedecay_domain::WorkflowDefinition>),
    ListDefinitions(ApplicationOutcome<Vec<tracedecay_domain::WorkflowDefinition>>),
    DefinitionHistory(ApplicationOutcome<Vec<tracedecay_domain::WorkflowDefinition>>),
    DiffDefinition(ApplicationOutcome<WorkflowDefinitionDiff>),
    HandoffIssue(ApplicationOutcome<TaskHandoffGrant>),
    HandoffRedeem(ApplicationOutcome<TaskHandoffRedeemed>),
    StartRun(ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>),
    PauseRun(ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>),
    ResumeRun(ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>),
    CancelRun(ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>),
    GetRun(ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "outcome", rename_all = "snake_case")]
pub(crate) enum HandoffApplicationOutcomeV1 {
    IssueTaskHandoff(ApplicationOutcome<IssueTaskHandoffResultV1>),
    OpenInvestigationHandoff(ApplicationOutcome<OpenInvestigationHandoffResultV1>),
    OpenTaskHandoff(ApplicationOutcome<OpenTaskHandoffResultV1>),
}

impl DaemonInvocationResponse {
    pub(crate) fn problem(request_id: impl Into<String>, problem: DaemonInvocationProblem) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            outcome: DaemonInvocationOutcome::Problem { problem },
        }
    }

    pub(crate) fn application_problem(
        request_id: impl Into<String>,
        problem: ApplicationProblem,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            outcome: DaemonInvocationOutcome::ApplicationProblem { problem },
        }
    }

    pub(crate) fn lsp_opened(
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

    pub(crate) fn with_outcome(request_id: String, outcome: DaemonInvocationOutcome) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id,
            outcome,
        }
    }
}
