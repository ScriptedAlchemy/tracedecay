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

use std::fmt;

use serde::{Deserialize, Serialize};
use tracedecay_application::{
    AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand, ApplicationContractError,
    ApplicationOutcome, ApplicationProblem, AttachRuntimeEvidenceCommand, AuthorityReceipt,
    AuthorizedScopeSet, CancellationContext, CreateWorkCommand, Deadline, EffectId, EffectReceipt,
    EffectResult, EvidenceAuthority, EvidenceCoverage, EvidencePacket, EvidenceScore,
    IdempotencyKey, MultiRootExecuteRequestV1, MultiRootScopeSetCasRequestV1,
    MultiRootScopeSetCasResultV1, MultiRootScopeSetReadRequestV1, Omission, OperationReceipt,
    PageRequest, PageState, PreviewId, PreviewResult, ReconciliationState,
    ReplanDependenciesCommand, RequestId, ResolvedScope, RetrieverContribution,
    ReviewProposalRequestV1, TaskHandoffGrantV1, TaskHandoffIssueRequestV1,
    TaskHandoffRedeemRequestV1, TaskHandoffRedeemedV1, TemporalState,
    WorkAttemptAcquireLeaseRequestV1, WorkAttemptCancelRequestV1, WorkAttemptFinishRequestV1,
    WorkAttemptPublishArtifactRequestV1, WorkAttemptPublishProgressRequestV1,
    WorkAttemptRecoverRequestV1, WorkAttemptRenewLeaseRequestV1, WorkAttemptResponseV1,
    WorkAttemptStartRequestV1, WorkAttemptTerminalizeRequestV1, WorkProjectionDeltaRequestV1,
    WorkProjectionSnapshotRequestV1, WorkflowActivationV1, WorkflowDefinitionActivateRequestV1,
    WorkflowDefinitionRegisterRequestV1, WorkflowExecutionTruthV1, WorkflowFanOutRequestV1,
};
use tracedecay_domain::{
    ActorId, GitIndexPreviewV1, GitIndexTransactionReceiptV1, ManifestDigest, RetrievalAnchorId,
    ScopeSetId, UtcMicros, WorkProjection, WorkProjectionDeltaV1, WorkProjectionSnapshotV1,
};
use tracedecay_lsp::{
    LspSessionAccess, LspSessionCredential, LspSessionId, MAX_LSP_FRAME_BYTES,
    MAX_LSP_WORKSPACE_ROOTS,
};
use tracedecay_tool_catalog::{EffectClass, UseCaseId};

use crate::application::feedback::observations::{
    Plan26DeliveryRouteV1, Plan26FeedbackSourceEventV1,
};
use crate::application::primitives::Pr12PrimitiveRequest;
use crate::application_surface::{
    ConfigurationSurfaceRequest, ContextScoutSurfaceRequest, GitApplySurfaceRequest,
    GitPreviewSurfaceRequest, GitReadSurfaceRequest,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "attempt_operation",
    content = "request",
    rename_all = "snake_case"
)]
pub(crate) enum WorkAttemptInvocationV1 {
    AcquireLease(Box<WorkAttemptAcquireLeaseRequestV1>),
    RenewLease(WorkAttemptRenewLeaseRequestV1),
    Start(WorkAttemptStartRequestV1),
    PublishProgress(WorkAttemptPublishProgressRequestV1),
    PublishArtifact(WorkAttemptPublishArtifactRequestV1),
    Cancel(WorkAttemptCancelRequestV1),
    Recover(WorkAttemptRecoverRequestV1),
    Finish(WorkAttemptFinishRequestV1),
    Terminalize(WorkAttemptTerminalizeRequestV1),
}

impl WorkAttemptInvocationV1 {
    pub(crate) const fn operation_key(&self) -> &'static str {
        match self {
            Self::AcquireLease(_) => "attempt_acquire_lease",
            Self::RenewLease(_) => "attempt_renew_lease",
            Self::Start(_) => "attempt_start",
            Self::PublishProgress(_) => "attempt_publish_progress",
            Self::PublishArtifact(_) => "attempt_publish_artifact",
            Self::Cancel(_) => "attempt_cancel",
            Self::Recover(_) => "attempt_recover",
            Self::Finish(_) => "attempt_finish",
            Self::Terminalize(_) => "attempt_terminalize",
        }
    }
}

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

/// Stable discriminator for the closed post-handshake invocation protocol.
pub(crate) const DAEMON_INVOCATION_PROTOCOL: &str = "tracedecay.daemon.invocation";
/// Initial revision of the daemon-owned invocation wire shape.
pub(crate) const DAEMON_INVOCATION_REVISION: u16 = 1;

const MAX_INVOCATION_REQUEST_ID_BYTES: usize = 128;
const MAX_CLIENT_REVISION_BYTES: usize = 128;
const MAX_ROOT_HINT_BYTES: usize = 4_096;
const MAX_OPAQUE_HANDLE_BYTES: usize = 256;

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
    MultiRootScopeSetRead,
    MultiRootScopeSetCompareAndSwap,
    MultiRootExecute,
    WorkApplication,
    WorkflowApplication,
    WorkAttempt,
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
            Self::MultiRootScopeSetRead => "multi_root_scope_set_read",
            Self::MultiRootScopeSetCompareAndSwap => "multi_root_scope_set_compare_and_swap",
            Self::MultiRootExecute => "multi_root_execute",
            Self::WorkApplication => "work_application",
            Self::WorkflowApplication => "workflow_application",
            Self::WorkAttempt => "work_attempt",
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub(crate) enum WorkApplicationInvocationV1 {
    Snapshot(WorkProjectionSnapshotRequestV1),
    Delta(WorkProjectionDeltaRequestV1),
    Create(CreateWorkCommand),
    ReplanDependencies(ReplanDependenciesCommand),
    ReviewProposal(ReviewProposalRequestV1),
    AcceptProposal(AcceptProposalCommand),
    AdmitExecution(AdmitExecutionCommand),
    AttachRuntimeEvidence(AttachRuntimeEvidenceCommand),
    AcceptTask(AcceptTaskCommand),
}

impl WorkApplicationInvocationV1 {
    pub(crate) const fn operation_key(&self) -> &'static str {
        match self {
            Self::Snapshot(_) => "snapshot",
            Self::Delta(_) => "delta",
            Self::Create(_) => "create",
            Self::ReplanDependencies(_) => "replan_dependencies",
            Self::ReviewProposal(_) => "review_proposal",
            Self::AcceptProposal(_) => "accept_proposal",
            Self::AdmitExecution(_) => "admit_execution",
            Self::AttachRuntimeEvidence(_) => "attach_runtime_evidence",
            Self::AcceptTask(_) => "accept_task",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub(crate) enum WorkflowApplicationInvocationV1 {
    RegisterDefinition(WorkflowDefinitionRegisterRequestV1),
    ActivateDefinition(WorkflowDefinitionActivateRequestV1),
    ExecuteFanOut(Box<WorkflowFanOutRequestV1>),
    HandoffIssue(TaskHandoffIssueRequestV1),
    HandoffRedeem(TaskHandoffRedeemRequestV1),
}

impl WorkflowApplicationInvocationV1 {
    pub(crate) const fn operation_key(&self) -> &'static str {
        match self {
            Self::RegisterDefinition(_) => "register_definition",
            Self::ActivateDefinition(_) => "activate_definition",
            Self::ExecuteFanOut(_) => "execute_fan_out",
            Self::HandoffIssue(_) => "handoff_issue",
            Self::HandoffRedeem(_) => "handoff_redeem",
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
    pub(crate) delivery_route: Option<Plan26DeliveryRouteV1>,
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
        event: Plan26FeedbackSourceEventV1,
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
        request: Pr12PrimitiveRequest,
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
        request: WorkflowApplicationInvocationV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    WorkAttempt {
        request: WorkAttemptInvocationV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    SemanticEvaluateAndPublish {
        candidate: Box<crate::application::semantic_runtime::SemanticEvaluationProfileCandidateV1>,
    },
    LspOpen {
        client_revision: String,
        requested_root_uri: Option<String>,
        workspace_folders: Vec<String>,
    },
    LspFrame {
        session: DaemonLspSessionAccess,
        frame: String,
    },
    LspPoll {
        session: DaemonLspSessionAccess,
    },
    LspAcknowledge {
        session: DaemonLspSessionAccess,
    },
    LspReconnect {
        session: DaemonLspSessionAccess,
    },
    LspDetach {
        session: DaemonLspSessionAccess,
    },
}

impl DaemonInvocationRequest {
    pub(crate) fn git_read(
        request_id: impl Into<String>,
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
        request: GitReadSurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::GitRead {
                surface_operation,
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub(crate) fn git_preview(
        request_id: impl Into<String>,
        request: GitPreviewSurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::GitPreview {
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub(crate) fn git_apply(
        request_id: impl Into<String>,
        request: GitApplySurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::GitApply {
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
            | crate::application_surface::ApplicationSurfaceOperation::ConfigurationAudit => {
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
        event: Plan26FeedbackSourceEventV1,
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
        request: Pr12PrimitiveRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        let payload = match (operation, request) {
            (
                crate::application_surface::ApplicationSurfaceOperation::FeedbackImpact,
                Pr12PrimitiveRequest::Impact(request),
            ) => DaemonInvocationPayload::PrimitiveImpact {
                request,
                observed_at,
                deadline,
                cancellation,
            },
            (
                crate::application_surface::ApplicationSurfaceOperation::AffectedTests,
                Pr12PrimitiveRequest::AffectedFileTests(request),
            ) => DaemonInvocationPayload::PrimitiveAffectedTests {
                request,
                observed_at,
                deadline,
                cancellation,
            },
            (
                crate::application_surface::ApplicationSurfaceOperation::TestResults,
                Pr12PrimitiveRequest::RecentTestResults(page),
            ) => DaemonInvocationPayload::PrimitiveTestResults {
                page,
                observed_at,
                deadline,
                cancellation,
            },
            (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::SessionLookup,
                request @ Pr12PrimitiveRequest::SessionLookup(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::QualifiedName,
                request @ Pr12PrimitiveRequest::QualifiedName(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::CallChain,
                request @ Pr12PrimitiveRequest::CallChain(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::FileDependents,
                request @ Pr12PrimitiveRequest::FileDependents(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::SourceLines,
                request @ Pr12PrimitiveRequest::SourceLines(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::SourceBody,
                request @ Pr12PrimitiveRequest::SourceBody(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::SourceOutline,
                request @ Pr12PrimitiveRequest::SourceOutline(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::ModuleApi,
                request @ Pr12PrimitiveRequest::ModuleApi(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::FileMetadata,
                request @ Pr12PrimitiveRequest::FileMetadata(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::HealthRead,
                request @ Pr12PrimitiveRequest::HealthRead(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::HealthDelta,
                request @ Pr12PrimitiveRequest::HealthDelta(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::StorageStatus,
                request @ Pr12PrimitiveRequest::StorageStatus(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::DiagnosticsRead,
                request @ Pr12PrimitiveRequest::DiagnosticsRead(_),
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

    pub(crate) fn work_attempt(
        request_id: impl Into<String>,
        request: WorkAttemptInvocationV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::WorkAttempt {
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    #[cfg(test)]
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

    #[cfg(test)]
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

    #[cfg(test)]
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
        request: WorkflowApplicationInvocationV1,
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

    pub(crate) fn semantic_evaluate_and_publish(
        request_id: impl Into<String>,
        candidate: crate::application::semantic_runtime::SemanticEvaluationProfileCandidateV1,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::SemanticEvaluateAndPublish {
                candidate: Box::new(candidate),
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
            },
        }
    }

    pub(crate) fn lsp_frame(
        request_id: impl Into<String>,
        session: DaemonLspSessionAccess,
        frame: impl Into<String>,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::LspFrame {
                session,
                frame: frame.into(),
            },
        }
    }

    pub(crate) fn lsp_poll(request_id: impl Into<String>, session: DaemonLspSessionAccess) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::LspPoll { session },
        }
    }

    pub(crate) fn lsp_acknowledge(
        request_id: impl Into<String>,
        session: DaemonLspSessionAccess,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::LspAcknowledge { session },
        }
    }

    pub(crate) fn lsp_detach(
        request_id: impl Into<String>,
        session: DaemonLspSessionAccess,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::LspDetach { session },
        }
    }

    pub(crate) fn lsp_reconnect(
        request_id: impl Into<String>,
        session: DaemonLspSessionAccess,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::LspReconnect { session },
        }
    }

    pub(crate) fn with_delivery_route(mut self, route: Plan26DeliveryRouteV1) -> Self {
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
            DaemonInvocationPayload::GitApply { .. } => DaemonInvocationOperation::GitApply,
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
            DaemonInvocationPayload::WorkAttempt { .. } => DaemonInvocationOperation::WorkAttempt,
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
                | DaemonInvocationOperation::GitHunks
                | DaemonInvocationOperation::GitPreview
                | DaemonInvocationOperation::GitApply
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
                | DaemonInvocationOperation::Configuration
                | DaemonInvocationOperation::ContextScout
                | DaemonInvocationOperation::MultiRootScopeSetRead
                | DaemonInvocationOperation::MultiRootScopeSetCompareAndSwap
                | DaemonInvocationOperation::MultiRootExecute
                | DaemonInvocationOperation::WorkApplication
                | DaemonInvocationOperation::WorkflowApplication
                | DaemonInvocationOperation::WorkAttempt
                | DaemonInvocationOperation::SemanticEvaluateAndPublish
                | DaemonInvocationOperation::LspOpen
        )
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
            | DaemonInvocationPayload::GitApply {
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
            | DaemonInvocationPayload::Configuration {
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
            | DaemonInvocationPayload::WorkAttempt {
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
            DaemonInvocationPayload::SemanticEvaluateAndPublish { candidate } => {
                if candidate.evaluated_profile_id.trim() != candidate.evaluated_profile_id
                    || candidate.evaluated_profile_id.is_empty()
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
                    || !request.matches(*surface_operation)
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
            } => {
                if !valid_printable(client_revision, MAX_CLIENT_REVISION_BYTES)
                    || requested_root_uri
                        .as_deref()
                        .is_some_and(|uri| !valid_printable(uri, MAX_ROOT_HINT_BYTES))
                    || workspace_folders.len() > MAX_LSP_WORKSPACE_ROOTS
                    || workspace_folders
                        .iter()
                        .any(|folder| !valid_printable(folder, MAX_ROOT_HINT_BYTES))
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::LspFrame { session, frame } => {
                let _ = session.clone().into_access()?;
                if frame.len() > MAX_LSP_FRAME_BYTES {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::LspPoll { session }
            | DaemonInvocationPayload::LspAcknowledge { session }
            | DaemonInvocationPayload::LspReconnect { session }
            | DaemonInvocationPayload::LspDetach { session } => {
                let _ = session.clone().into_access()?;
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
    Unavailable,
}

/// Response envelope paired with one invocation request id.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
        outcome: WorkflowApplicationOutcomeV1,
    },
    WorkAttempt {
        scope: ResolvedScope,
        outcome: Box<ApplicationOutcome<WorkAttemptResponseV1>>,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "outcome", rename_all = "snake_case")]
pub(crate) enum WorkApplicationOutcomeV1 {
    Snapshot(ApplicationOutcome<WorkProjectionSnapshotV1>),
    Delta(ApplicationOutcome<WorkProjectionDeltaV1>),
    Create(ApplicationOutcome<WorkProjection>),
    ReplanDependencies(ApplicationOutcome<WorkProjection>),
    ReviewProposal(ApplicationOutcome<WorkProjection>),
    AcceptProposal(ApplicationOutcome<WorkProjection>),
    AdmitExecution(ApplicationOutcome<WorkProjection>),
    AttachRuntimeEvidence(ApplicationOutcome<WorkProjection>),
    AcceptTask(ApplicationOutcome<WorkProjection>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "outcome", rename_all = "snake_case")]
pub(crate) enum WorkflowApplicationOutcomeV1 {
    RegisterDefinition(ApplicationOutcome<tracedecay_domain::WorkflowDefinitionV1>),
    ActivateDefinition(ApplicationOutcome<WorkflowActivationV1>),
    ExecuteFanOut(ApplicationOutcome<WorkflowExecutionTruthV1>),
    HandoffIssue(ApplicationOutcome<TaskHandoffGrantV1>),
    HandoffRedeem(ApplicationOutcome<TaskHandoffRedeemedV1>),
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
