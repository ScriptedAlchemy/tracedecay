//! Closed, authenticated daemon invocation protocol.
//!
//! This module deliberately accepts a small typed operation set after the
//! daemon handshake. It is not a generic application invoke endpoint and it
//! never accepts a raw Git request, database selector, or LSP socket address.
//! LSP frames are handled by a daemon-owned protocol actor; the bridge only
//! receives the actor's bounded responses through explicit frame operations.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, RwLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, Semaphore};
use tracedecay_application::feedback::{
    FeedbackReadPort, FeedbackRouteAuthorizationPort, FeedbackRuntimeStatePort,
};
use tracedecay_application::{
    AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand, AffectedTestsRetrievalPort,
    AnalyzerAdmittedDiagnosticProviderV1, ApplicationContractError, ApplicationOperation,
    ApplicationOutcome, ApplicationProblem, ApplicationProblemKind, ApplicationResult,
    AttachRuntimeEvidenceCommand, AuthorityReceipt, CallableCodeAuthorizationPort,
    CallableCodeOperationKind, CallableCodeQueryService, CancellationContext, CapabilityGrantId,
    CapabilityGrantSnapshot, CoverageCompleteness, CoverageDomainState, CreateWorkCommand,
    Deadline, DiagnosticProviderIdentity, DisclosureClass, EffectId, EffectReceipt, EffectResult,
    EffectTermination, EvidenceAuthority, EvidenceCoverage, EvidenceDomain, EvidenceIdentity,
    EvidencePacket, EvidenceScore, GitIndexApplyPortResultV1, GitIndexApplyRequestV1,
    GitIndexEffectProofV1, GitIndexOperationBindingV1, GitIndexPreviewPortResultV1,
    GitIndexPreviewRequestV1, GitIndexRecoveryRequestV1, GitIndexTransactionApplicationError,
    GitIndexTransactionPort, GitIndexTransactionPortError, GitIndexTransactionService,
    IdempotencyKey, MultiRootExecuteRequestV1, MultiRootScopeSetCasRequestV1,
    MultiRootScopeSetCasResultV1, MultiRootScopeSetCasStatusV1, MultiRootScopeSetReadRequestV1,
    Omission, OmissionReason, OperationBudgetUsage, OperationReceipt, OperationTermination,
    PageRequest, PageState, PolicyDecisionRef, PolicyEvaluationContextV1,
    PolicyEvaluatorCompositionV1, PolicyEvidenceHorizonV1, PreviewId, PreviewResult,
    ReconciliationState, ReplanDependenciesCommand, RequestAdmission, RequestContext, RequestId,
    ResolvedScope, RetrieverContribution, RetryDirective, ReviewProposalRequestV1, SafeDiagnostic,
    TemporalState, WorkAttemptResponseV1, WorkExecutionError, WorkProjectionApplicationError,
    WorkProjectionDeltaRequestV1, WorkProjectionSnapshotRequestV1, callable_code_operations,
};
use tracedecay_domain::configuration::{
    CandidateDispositionV1, ConfigurationGrantId, ConfigurationGrantReceiptId,
    ConfigurationLayerIdV1, ConfigurationMutationEffectV1, ConfigurationMutationGrantReceiptV1,
    ConfigurationMutationOperationV1, ConfigurationMutationSinkV1, ConfigurationRevisionId,
    ConfigurationSnapshotV1, ProtectedApplyRequest,
};
use tracedecay_domain::feedback::{FeedbackCycleTerminationV1, ProviderEvaluationStateV1};
use tracedecay_domain::{
    AccessPolicyDigest, ActorId, ComponentVersion, GitHeadStateV1, GitIndexPreviewId,
    GitIndexPreviewV1, GitIndexTransactionOperationV1, GitIndexTransactionReceiptV1,
    ManifestDigest, ProjectId, RetrievalAnchorId, UserProfileId, UtcMicros, WorkAuthority,
    WorkProjection, WorkProjectionDeltaV1, WorkProjectionSnapshotV1, canonical_sha256,
};
use tracedecay_lsp::analyzer::broker::DiagnosticBroker;
use tracedecay_lsp::analyzer::client::LspRefreshTimeouts;
use tracedecay_lsp::{
    AdmittedRoot, AuthorizedLspSession, AuthorizedLspWorkspace,
    CanonicalContextProjectionAuthority, CanonicalDiagnosticRefreshRequest,
    CanonicalDiagnosticSnapshotAuthority, ContextProjectionOutcome, ContextProjectionRegistration,
    ContextProjectionRequest, DaemonLspRuntimeSession, DaemonLspSessionEndpoint, DiagnosticTrigger,
    FeedbackCycleRequest, FeedbackCycleRuntimePort, GatewayCapabilities, GenerationDiagnostics,
    LSP_SESSION_TTL_MS, LspAnalyzerCancellationAuthority, LspEndpointError, LspRequestId,
    LspRuntimeFailure, LspRuntimeFuture, LspSessionAccess, LspSessionAdmissionPort,
    LspSessionCredential, LspSessionId, LspSessionOpenRequest, LspSessionRegistry,
    MAX_LSP_FRAME_BYTES, SessionLifecycle, UnavailableSemanticProvider, UpstreamCapabilities,
};
use tracedecay_policy::configuration::{
    ConfigurationMutationGrantSnapshotV1, ConfigurationMutationGrantStateV1,
    ConfigurationMutationPermissionV1,
};
use tracedecay_policy::{
    AnalyzerAdmissionInputV1, CapabilityAvailabilityV1, CapabilityEffectClassV1, ScopeMatchV1,
    TruthFreshnessRequirementV1, TruthSourceStateV1,
};
use tracedecay_tool_catalog::{CapabilityId, EffectClass, SortContractId, UseCaseId};

use super::project_runtime::ProjectRuntimeRegistryV1;
use crate::agents::context_scout_ports::{
    AdmittedContextScoutHookV1, ContextScoutLifecycleAddressV1,
    ProjectContextScoutAddressRegistryV1,
};
use crate::application::ProjectSourceAccessSnapshot;
use crate::application::advisory::{
    CanonicalProximityEvidenceAuthorityV1, CiExactEvidenceAuthorityV1, CiReadOnlyProviderArchiveV1,
    GitHubCanonicalReviewAnchorAuthorityV1, GitHubCurrentBranchRemapper,
    Pr13AdvisoryDaemonStartupErrorV1, Pr13AdvisoryDaemonStartupRegistrationV1,
    Pr13AdvisoryHookLookupNoticeV1, Pr13AdvisoryProductionOpenErrorV1,
    Pr13AdvisoryProductionOpenV1, Pr13AdvisoryProductionStartupRegistrationV1,
    Pr13AdvisoryProviderAuthoritiesV1, Pr13AdvisoryRuntimeOpenV1,
    open_pr13_advisory_production_authorities, register_pr13_advisory_daemon_startup,
};
use crate::application::configuration::{
    AuthorizedActor, ConfigurationAuditQuery, ConfigurationControlStore, ConfigurationError,
    ConfigurationMutationAuthority, ConfigurationMutationGrantAuthority,
    ConfigurationMutationGrantAuthorityError, ConfigurationMutationGrantAuthorityFuture,
    ConfigurationRollbackRequest, CredentialWriteHandleV1, DirectConfigurationMutation,
    PolicyBackedConfigurationMutationAuthorization, ProjectConfigurationRuntime,
    ScopeResolutionPort, ScopeRevalidationEvidenceV1, WriteOnlyCredentialMutation,
    configuration_layer_scope_digest,
};
use crate::application::feedback::concrete::{
    Pr12FeedbackRuntime, Pr12FeedbackRuntimeError, ProjectFeedbackStore, open_pr12_feedback_runtime,
};
use crate::application::feedback::cycle_production::{
    ProductionFeedbackCycleProximityPortV1, production_proximity_feedback_cycle_input,
};
use crate::application::feedback::observations::{
    Plan26AnchorOperationV1, Plan26ArgumentRejectionClassV1, Plan26DeliveryRouteV1,
    Plan26FeedbackObservationEmitterV1, Plan26FeedbackOperationV1, Plan26FeedbackOutcomeV1,
    Plan26FeedbackSourceEventV1, Plan26RejectedArgumentV1,
};
use crate::application::feedback::owner::{
    DaemonFeedbackReadOwnerV1, FeedbackCanonicalProjectionKindV1, FeedbackReadInvocationResultV1,
    FeedbackReadOperationV1, FeedbackReadOwnerErrorV1, FeedbackReadRequestAuthority,
};
use crate::application::feedback::{
    Pr12FeedbackCycleLspInput, Pr12FeedbackCycleRuntime, Pr12FeedbackCycleRuntimeError,
    open_pr12_feedback_cycle_runtime,
};
use crate::application::lsp_runtime::{
    DaemonLspSessionFactory, LspCodeIndexProjectionIdentityPort, lsp_session_factory,
    production_semantic_authorities,
};
use crate::application::operation_stream::{
    OperationEmitter, OperationEventAuthority, OperationKind, operation_event_authority,
};
use crate::application::primitives::{
    Pr12PrimitiveDispatch, Pr12PrimitiveInvocation, Pr12PrimitiveProjectRuntime,
    Pr12PrimitiveRequest,
};
use crate::application::semantic_runtime::{
    ProductionSemanticConfigurationOperationV1, SemanticActivationCoordinationErrorV1,
    SemanticProtectedActivationOperationV1, SemanticProtectedRollbackOperationV1,
};
use crate::application_surface::{
    ConfigurationSurfaceRequest, ContextScoutSurfaceRequest, GitApplySurfaceRequest,
    GitPreviewSurfaceRequest, GitReadSurfaceRequest,
};
use crate::daemon::callable_code_authorization::DaemonCallableCodeAuthorizationSource;
use crate::daemon::git_transactions::{
    DaemonGitAuthorityStateV1, DaemonGitInvocationOwner, DaemonProjectGitIndexTransactionService,
};
use crate::daemon::work_runtime::{DaemonWorkRuntimeV1, WorkAttemptInvocationV1};
use crate::db::Database;
use crate::errors::TraceDecayError;
use crate::request_identity::{
    GlobalOpaqueIdentityKind, LogicalEffectIdempotencyDomain, derive_logical_effect_idempotency,
    mint_global_opaque_id,
};
use crate::tracedecay::TraceDecay;
use tracedecay_hooks::{
    HookBoundaryV1, HookEventEnvelopeV2, HookEventV2, HookFeedbackDeliveryPortV1,
    HookScopeBindingV1,
};

/// Stable discriminator for the closed post-handshake invocation protocol.
pub(crate) const DAEMON_INVOCATION_PROTOCOL: &str = "tracedecay.daemon.invocation";
/// Initial revision of the daemon-owned invocation wire shape.
pub(crate) const DAEMON_INVOCATION_REVISION: u16 = 1;

pub(crate) fn daemon_operation_event_authority() -> OperationEventAuthority {
    operation_event_authority()
}

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
    fn from_access(access: &LspSessionAccess) -> Self {
        Self {
            session_id: access.session_id().as_str().to_owned(),
            credential: hex::encode(access.credential().as_bytes()),
        }
    }

    fn into_access(self) -> Result<LspSessionAccess, DaemonInvocationProblem> {
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
    WorkAttempt {
        request: WorkAttemptInvocationV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    SemanticEvaluateAndPublish {
        candidate: crate::application::semantic_runtime::SemanticEvaluationProfileCandidateV1,
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

    pub(crate) fn semantic_evaluate_and_publish(
        request_id: impl Into<String>,
        candidate: crate::application::semantic_runtime::SemanticEvaluationProfileCandidateV1,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::SemanticEvaluateAndPublish { candidate },
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
                | DaemonInvocationOperation::WorkAttempt
                | DaemonInvocationOperation::SemanticEvaluateAndPublish
                | DaemonInvocationOperation::LspOpen
        )
    }

    fn validate(&self) -> Result<(), DaemonInvocationProblem> {
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
                    || workspace_folders.len() > 1
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
    fn from_application(
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
    fn from_application(
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

pub(crate) struct DaemonFeedbackInvocationRequest {
    #[allow(dead_code)] // in-flight feedback request field — staged
    pub(crate) request_id: RequestId,
    pub(crate) operation: DaemonInvocationOperation,
    pub(crate) request_handle: String,
    pub(crate) observed_at: UtcMicros,
    pub(crate) deadline: Deadline,
    pub(crate) cancellation: CancellationContext,
}

pub(crate) struct DaemonFeedbackInvocationResult {
    pub(crate) scope: ResolvedScope,
    pub(crate) evidence: EvidencePacket<serde_json::Value>,
}

pub(crate) type DaemonFeedbackInvocationFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<DaemonFeedbackInvocationResult, ApplicationProblem>> + Send + 'a,
    >,
>;

pub(crate) trait DaemonFeedbackInvocationPort: Send + Sync {
    fn invoke(
        &self,
        request: DaemonFeedbackInvocationRequest,
    ) -> DaemonFeedbackInvocationFuture<'_>;
}

impl<R, P, A> DaemonFeedbackInvocationPort for DaemonFeedbackReadOwnerV1<R, P, A>
where
    R: FeedbackReadRequestAuthority + Send + Sync,
    P: FeedbackReadPort + Send + Sync,
    A: FeedbackRouteAuthorizationPort + Send + Sync,
{
    fn invoke(
        &self,
        request: DaemonFeedbackInvocationRequest,
    ) -> DaemonFeedbackInvocationFuture<'_> {
        Box::pin(async move {
            let result = match request.operation {
                DaemonInvocationOperation::FeedbackImpact => {
                    DaemonFeedbackReadOwnerV1::invoke_projection_with_controls(
                        self,
                        FeedbackCanonicalProjectionKindV1::Impact,
                        &request.request_handle,
                        request.observed_at,
                        request.deadline,
                        request.cancellation,
                    )
                    .await
                }
                DaemonInvocationOperation::AffectedTests => {
                    DaemonFeedbackReadOwnerV1::invoke_projection_with_controls(
                        self,
                        FeedbackCanonicalProjectionKindV1::AffectedTests,
                        &request.request_handle,
                        request.observed_at,
                        request.deadline,
                        request.cancellation,
                    )
                    .await
                }
                operation @ (DaemonInvocationOperation::FeedbackDiagnostics
                | DaemonInvocationOperation::FeedbackGet
                | DaemonInvocationOperation::FeedbackExpand
                | DaemonInvocationOperation::FeedbackList) => {
                    let operation = match operation {
                        DaemonInvocationOperation::FeedbackDiagnostics => {
                            FeedbackReadOperationV1::Diagnostics
                        }
                        DaemonInvocationOperation::FeedbackGet => FeedbackReadOperationV1::Get,
                        DaemonInvocationOperation::FeedbackExpand => {
                            FeedbackReadOperationV1::Expand
                        }
                        DaemonInvocationOperation::FeedbackList => FeedbackReadOperationV1::List,
                        _ => unreachable!("feedback operation was exhaustively matched"),
                    };
                    DaemonFeedbackReadOwnerV1::invoke_with_controls(
                        self,
                        operation,
                        &request.request_handle,
                        request.observed_at,
                        request.deadline,
                        request.cancellation,
                    )
                    .await
                }
                _ => {
                    return Err(ApplicationProblem::InvalidRequest {
                        diagnostic: SafeDiagnostic {
                            code: "feedback.invalid_operation".to_owned(),
                            message: "The feedback read operation is invalid".to_owned(),
                        },
                        retry: RetryDirective::Never,
                        legal_actions: Vec::new(),
                    });
                }
            }
            .map_err(feedback_owner_problem)?;
            match result {
                FeedbackReadInvocationResultV1::Diagnostics(result) => {
                    feedback_invocation_result(result)
                }
                FeedbackReadInvocationResultV1::Get(result) => feedback_invocation_result(result),
                FeedbackReadInvocationResultV1::Expand(result) => {
                    feedback_invocation_result(result)
                }
                FeedbackReadInvocationResultV1::List(result) => feedback_invocation_result(result),
                FeedbackReadInvocationResultV1::Impact(result) => {
                    feedback_invocation_result(result)
                }
                FeedbackReadInvocationResultV1::AffectedTests(result) => {
                    feedback_invocation_result(result)
                }
            }
        })
    }
}

#[derive(Clone)]
pub(crate) struct DaemonFeedbackInvocationOwner {
    pub(crate) project_id: ProjectId,
    pub(crate) service: Arc<dyn DaemonFeedbackInvocationPort>,
}

impl DaemonFeedbackInvocationOwner {
    pub(crate) fn new(
        project_id: ProjectId,
        service: Arc<dyn DaemonFeedbackInvocationPort>,
    ) -> Self {
        Self {
            project_id,
            service,
        }
    }
}

fn feedback_invocation_result<T>(
    result: ApplicationResult<T>,
) -> Result<DaemonFeedbackInvocationResult, ApplicationProblem>
where
    T: Serialize,
{
    feedback_invocation_result_with(result, serde_json::to_value)
}

fn feedback_invocation_result_with<T>(
    result: ApplicationResult<T>,
    encode: impl FnOnce(T) -> Result<serde_json::Value, serde_json::Error>,
) -> Result<DaemonFeedbackInvocationResult, ApplicationProblem> {
    let application = result.map_err(|problem| problem.problem.into_source())?;
    let evidence = match application.outcome {
        ApplicationOutcome::Evidence(packet) => packet,
        ApplicationOutcome::Preview(_) | ApplicationOutcome::Effect(_) => {
            return Err(ApplicationProblem::unavailable(SafeDiagnostic {
                code: "feedback.invalid_owner_result".to_owned(),
                message: "The feedback read owner returned an invalid outcome".to_owned(),
            }));
        }
    };
    let payload = evidence.payload.map(encode).transpose().map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "feedback.result_encoding_failed".to_owned(),
            message: "The feedback read result could not be encoded".to_owned(),
        })
    })?;
    Ok(DaemonFeedbackInvocationResult {
        scope: application.scope,
        evidence: EvidencePacket {
            temporal: evidence.temporal,
            authority: evidence.authority,
            evidence_authorities: evidence.evidence_authorities,
            coverage: evidence.coverage,
            omissions: evidence.omissions,
            scores: evidence.scores,
            contributions: evidence.contributions,
            page: evidence.page,
            execution: evidence.execution,
            payload,
        },
    })
}

fn feedback_owner_problem(error: FeedbackReadOwnerErrorV1) -> ApplicationProblem {
    match error {
        FeedbackReadOwnerErrorV1::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        FeedbackReadOwnerErrorV1::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "feedback.owner_unavailable".to_owned(),
            message: "The feedback read owner is unavailable".to_owned(),
        }),
    }
}

impl DaemonFeedbackResult {
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

fn feedback_scope_matches(
    expected: Option<&ResolvedScope>,
    project_root: Option<&Path>,
    owner: Option<&DaemonFeedbackInvocationOwner>,
) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    let (Some(project_root), Some(owner)) = (project_root, owner) else {
        return false;
    };
    crate::daemon::project_open_owners::resolved_scope_for_project(project_root, &owner.project_id)
        .is_ok_and(|scope| &scope == expected)
}

async fn execute_feedback(
    wire_request_id: String,
    owner: Option<DaemonFeedbackInvocationOwner>,
    operation: DaemonInvocationOperation,
    request_handle: String,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(owner) = owner else {
        // No feedback owner registered means the feedback read service is
        // absent, not that a specific handle is hidden. Report the fail-closed
        // infrastructure truth (Unavailable); concealment semantics only apply
        // once the service exists and a caller names an unknown handle.
        return application_problem(
            wire_request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "feedback.owner_unavailable".to_owned(),
                message: "The feedback read owner is unavailable".to_owned(),
            }),
        );
    };
    let Ok(request_id) = RequestId::new(wire_request_id.clone()) else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::InvalidRequest,
        );
    };
    let result = owner
        .service
        .invoke(DaemonFeedbackInvocationRequest {
            request_id,
            operation,
            request_handle,
            observed_at,
            deadline,
            cancellation,
        })
        .await;
    match result {
        Ok(result) if result.scope.project_id == owner.project_id => {
            DaemonInvocationResponse::with_outcome(
                wire_request_id,
                DaemonInvocationOutcome::Feedback {
                    scope: result.scope,
                    result: DaemonFeedbackResult::from_application(result.evidence),
                },
            )
        }
        Ok(_) => concealed_application_problem(wire_request_id),
        Err(problem) => application_problem(wire_request_id, problem),
    }
}

async fn execute_feedback_advisory_cycle(
    wire_request_id: String,
    invoker: Option<Arc<dyn Pr13AdvisoryCycleInvocationPortV1>>,
    feedback_owner: Option<DaemonFeedbackInvocationOwner>,
    document_uri: String,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(invoker) = invoker else {
        return application_problem(
            wire_request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "feedback.advisory_cycle_unavailable".to_owned(),
                message: "The advisory feedback cycle is unavailable".to_owned(),
            }),
        );
    };
    let invocation = match invoker
        .invoke(Pr13AdvisoryCycleInvocationRequestV1 {
            request_id: wire_request_id.clone(),
            document_uri,
            observed_at,
            deadline: deadline.clone(),
            cancellation: cancellation.clone(),
        })
        .await
    {
        Ok(invocation) => invocation,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let Pr13AdvisoryCycleInvocationOutcomeV1 {
        request_handle,
        cycle,
    } = invocation;
    let mut response = execute_feedback(
        wire_request_id,
        feedback_owner,
        DaemonInvocationOperation::FeedbackDiagnostics,
        request_handle.clone(),
        observed_at,
        deadline,
        cancellation,
    )
    .await;
    if let DaemonInvocationOutcome::Feedback { result, .. } = &mut response.outcome {
        let diagnostics = result.payload.take();
        // `cycle` keeps the four-pillar terminal state visible even when the
        // publication-backed diagnostics read has nothing to return, so an
        // incomplete-coverage cycle never renders as a clean empty result.
        result.payload = Some(serde_json::json!({
            "request_handle": request_handle,
            "diagnostics": diagnostics,
            "cycle": cycle,
            "producer_contributions": [
                "github_review_ingest",
                "ci_failure_localize",
                "feedback_proximity"
            ]
        }));
    }
    response
}

#[allow(clippy::too_many_arguments)]
async fn execute_primitive(
    service: &DaemonInvocationService,
    project_root: Option<&Path>,
    wire_request_id: String,
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    request: Pr12PrimitiveRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(project_root) = project_root else {
        return concealed_application_problem(wire_request_id);
    };
    let dispatch = service
        .project_runtimes
        .read(project_root, Pr12PrimitiveProjectRuntime::dispatch)
        .await;
    let Some(dispatch) = dispatch else {
        return concealed_application_problem(wire_request_id);
    };
    let registered = service
        .project_runtimes
        .get::<RegisteredCallableCodeRuntime>(project_root)
        .await;
    let Some(registered) = registered else {
        return concealed_application_problem(wire_request_id);
    };
    let access = match registered.authorization.current(observed_at).await {
        Ok(access) if access.scope == registered.scope => access,
        Ok(_) | Err(_) => return concealed_application_problem(wire_request_id),
    };
    let Ok(Some(operation)) =
        tracedecay_application::feedback::feedback_surface_operation(surface_operation.as_str())
            .and_then(|operation| {
                operation.map_or_else(
                    || {
                        tracedecay_application::retrieval::catalog::primitive_read_operation(
                            surface_operation.as_str(),
                        )
                    },
                    |operation| Ok(Some(operation)),
                )
            })
    else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::InvalidRequest,
        );
    };
    let context = match callable_code_request_context(
        &registered.scope,
        &access,
        &wire_request_id,
        &operation,
        observed_at,
        deadline,
        cancellation,
    ) {
        Ok(context) => context,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let authorization = registered.authorization.authorize(access);
    let admission = match authorization.admit(&context, &operation, observed_at).await {
        Ok(admission) => admission,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let mut result = dispatch
        .dispatch(
            Pr12PrimitiveInvocation {
                operation: operation.clone(),
                request,
            },
            context.clone(),
            observed_at,
        )
        .await;
    if result.is_ok() {
        let finished_at = current_micros();
        let publication_authority = match authorization
            .recheck_publication(&context, &operation, &admission, finished_at)
            .await
        {
            Ok(authority) => authority,
            Err(problem) => return application_problem(wire_request_id, problem),
        };
        if !crate::application::primitives::runtime::reauthorize_primitive_evidence(
            &mut result,
            publication_authority,
        ) {
            return DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    }
    match feedback_invocation_result(result) {
        Ok(result) => DaemonInvocationResponse::with_outcome(
            wire_request_id,
            DaemonInvocationOutcome::Primitive {
                scope: result.scope,
                result: DaemonFeedbackResult::from_application(result.evidence),
            },
        ),
        Err(problem) => application_problem(wire_request_id, problem),
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_callable_code(
    service: &DaemonInvocationService,
    project_root: Option<&Path>,
    wire_request_id: String,
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    request: crate::application_surface::CallableCodeSurfaceRequest,
    page: PageRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(project_root) = project_root else {
        return concealed_application_problem(wire_request_id);
    };
    let registered = service
        .project_runtimes
        .get::<RegisteredCallableCodeRuntime>(project_root)
        .await;
    let Some(registered) = registered else {
        return concealed_application_problem(wire_request_id);
    };
    let access = match registered.authorization.current(observed_at).await {
        Ok(access) => access,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let kind = match (&request, surface_operation) {
        (
            crate::application_surface::CallableCodeSurfaceRequest::ExactOccurrence(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeExactOccurrence,
        ) => CallableCodeOperationKind::ExactOccurrence,
        (
            crate::application_surface::CallableCodeSurfaceRequest::PhraseSearch(_),
            crate::application_surface::ApplicationSurfaceOperation::CodePhraseSearch,
        ) => CallableCodeOperationKind::PhraseSearch,
        (
            crate::application_surface::CallableCodeSurfaceRequest::Callees(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeCallees,
        ) => CallableCodeOperationKind::Callees,
        (
            crate::application_surface::CallableCodeSurfaceRequest::Facets(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeFacets,
        ) => CallableCodeOperationKind::Facets,
        (
            crate::application_surface::CallableCodeSurfaceRequest::Timeline(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeTimeline,
        ) => CallableCodeOperationKind::Timeline,
        (
            crate::application_surface::CallableCodeSurfaceRequest::Declaration(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeDeclaration,
        ) => CallableCodeOperationKind::Declaration,
        (
            crate::application_surface::CallableCodeSurfaceRequest::Definition(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeDefinition,
        ) => CallableCodeOperationKind::Definition,
        (
            crate::application_surface::CallableCodeSurfaceRequest::TypeDefinition(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeTypeDefinition,
        ) => CallableCodeOperationKind::TypeDefinition,
        (
            crate::application_surface::CallableCodeSurfaceRequest::References(_),
            crate::application_surface::ApplicationSurfaceOperation::CodeReferences,
        ) => CallableCodeOperationKind::References,
        _ => {
            return DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        }
    };
    let Ok(operations) = callable_code_operations() else {
        return application_problem(
            wire_request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "callable_code.operation_unavailable".to_owned(),
                message: "The callable code operation is unavailable".to_owned(),
            }),
        );
    };
    let context = match callable_code_request_context(
        &registered.scope,
        &access,
        &wire_request_id,
        operations.get(kind),
        observed_at,
        deadline,
        cancellation,
    ) {
        Ok(context) => context,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let query = CallableCodeQueryService::new(
        service.code_index_schedulers.clone(),
        registered.authorization.authorize(access),
        operations,
    );
    match request {
        crate::application_surface::CallableCodeSurfaceRequest::ExactOccurrence(request) => {
            let Ok(request) = request.into_application_request(page) else {
                return invalid_callable_code_request(wire_request_id);
            };
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.exact_occurrence(&context, request, observed_at).await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::PhraseSearch(request) => {
            let Ok(request) = request.into_application_request(
                crate::daemon::code_index_scheduler::queries::callable_query_sanitizer_revision(),
                crate::daemon::code_index_scheduler::queries::callable_query_normalization_revision(
                ),
                page,
            ) else {
                return invalid_callable_code_request(wire_request_id);
            };
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.phrase_search(&context, request, observed_at).await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::Callees(request) => {
            let request = request.into_application_request(page);
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.callees(&context, request, observed_at).await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::Facets(request) => {
            let request = request.into_application_request(page);
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.facets(&context, request, observed_at).await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::Timeline(request) => {
            let request = request.into_application_request(page);
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.timeline(&context, request, observed_at).await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::Declaration(request) => {
            let request = request.into_application_request(page);
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.declaration(&context, request, observed_at).await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::Definition(request) => {
            let request = request.into_application_request(page);
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.definition(&context, request, observed_at).await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::TypeDefinition(request) => {
            let request = request.into_application_request(page);
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.type_definition(&context, request, observed_at).await,
            )
        }
        crate::application_surface::CallableCodeSurfaceRequest::References(request) => {
            let request = request.into_application_request(page);
            callable_code_response(
                wire_request_id,
                &registered.scope,
                query.references(&context, request, observed_at).await,
            )
        }
    }
}

fn invalid_callable_code_request(wire_request_id: String) -> DaemonInvocationResponse {
    application_problem(
        wire_request_id,
        ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "callable_code.invalid_query".to_owned(),
                message: "The callable code query is invalid".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        },
    )
}

fn callable_code_request_context(
    scope: &ResolvedScope,
    access: &ProjectSourceAccessSnapshot,
    wire_request_id: &str,
    operation: &ApplicationOperation,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<RequestContext, ApplicationProblem> {
    if scope != &access.scope {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    if cancellation.is_cancelled() {
        return Err(ApplicationProblem::cancelled_before_admission());
    }
    if deadline.is_elapsed_at(observed_at) || deadline.is_elapsed_at(current_micros()) {
        return Err(ApplicationProblem::timed_out_before_admission());
    }
    let expires_at = UtcMicros(deadline.expires_at.0.min(access.grant_expires_at.0));
    if expires_at.0 <= observed_at.0 {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    let request_id =
        RequestId::new(wire_request_id).map_err(|_| ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "callable_code.invalid_request_id".to_owned(),
                message: "The callable code request identifier is invalid".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        })?;
    // Correlation IDs stay on the RequestContext. The route authority is a
    // function of the access and the operation, so the same authorized call
    // resolves the same grant from any surface and across durable retries.
    let grant_digest = canonical_sha256(&(
        "tracedecay.daemon.callable-code-grant.v1",
        scope,
        &access.requester,
        &access.configuration_digest,
        operation.capability_id(),
        operation.use_case_id(),
    ))
    .map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "callable_code.grant_unavailable".to_owned(),
            message: "The callable code route grant is unavailable".to_owned(),
        })
    })?;
    let grant_id = CapabilityGrantId::new(format!(
        "grant.daemon.callable-code.{}",
        grant_digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "callable_code.grant_unavailable".to_owned(),
            message: "The callable code route grant is unavailable".to_owned(),
        })
    })?;
    let grant = CapabilityGrantSnapshot::new(
        grant_id,
        1,
        grant_digest.clone(),
        access.requester.clone(),
        observed_at,
        expires_at,
        scope.clone(),
        std::collections::BTreeSet::from([operation.capability_id().clone()]),
        std::collections::BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "callable_code.grant_unavailable".to_owned(),
            message: "The callable code route grant is unavailable".to_owned(),
        })
    })?;
    RequestContext::new(
        access.requester.clone(),
        scope.clone(),
        grant,
        request_id,
        Deadline::new(expires_at).map_err(|_| {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "callable_code.deadline_unavailable".to_owned(),
                message: "The callable code request deadline is unavailable".to_owned(),
            })
        })?,
        cancellation,
    )
    .map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "callable_code.context_unavailable".to_owned(),
            message: "The callable code request context is unavailable".to_owned(),
        })
    })
}

fn callable_code_response<T: Serialize>(
    wire_request_id: String,
    registered_scope: &ResolvedScope,
    result: ApplicationResult<T>,
) -> DaemonInvocationResponse {
    match feedback_invocation_result(result) {
        Ok(result) if &result.scope == registered_scope => DaemonInvocationResponse::with_outcome(
            wire_request_id,
            DaemonInvocationOutcome::CallableCode {
                scope: result.scope,
                result: DaemonFeedbackResult::from_application(result.evidence),
            },
        ),
        Ok(_) => concealed_application_problem(wire_request_id),
        Err(problem) => application_problem(wire_request_id, problem),
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_context_scout(
    service: &DaemonInvocationService,
    wire_request_id: String,
    registered: Option<RegisteredConfigurationRuntime>,
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    request: ContextScoutSurfaceRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(registered) = registered else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::NotFoundOrNotAuthorized,
        );
    };
    let current = match registered.runtime.client().current().await {
        Ok(current) => crate::application::configuration::ConfigurationCurrentStateV1 {
            revision_id: current.revision_id,
            snapshot: current.snapshot,
        },
        Err(error) => {
            return application_problem(wire_request_id, configuration_problem(error));
        }
    };
    let Some(configuration) =
        crate::agents::context_scout_ports::ContextScoutConfigurationPinV1::from_current(&current)
    else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::NotFoundOrNotAuthorized,
        );
    };
    let registry = service
        .context_scout_registries
        .lock()
        .await
        .get(&registered.scope.project_id)
        .cloned();
    let Some(registry) = registry else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::NotFoundOrNotAuthorized,
        );
    };
    let address = request.address();
    if !registry
        .authorize_current_exact_address(address, &configuration, &registered.scope)
        .await
    {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::NotFoundOrNotAuthorized,
        );
    }
    let mut owner = None;
    for candidate in crate::agents::context_scout_owner::lookup_registered_context_scout_owners(
        address.project_id,
    ) {
        if candidate.configured_status().await.is_ok_and(|status| {
            status.configuration_revision == configuration.control().configuration_revision
        }) {
            if owner.is_some() {
                return DaemonInvocationResponse::problem(
                    wire_request_id,
                    DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            }
            owner = Some(candidate);
        }
    }
    let Some(owner) = owner else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::NotFoundOrNotAuthorized,
        );
    };
    if let ContextScoutSurfaceRequest::Pause(control)
    | ContextScoutSurfaceRequest::Resume(control) = &request
    {
        let target = match &request {
            ContextScoutSurfaceRequest::Pause(_) => {
                tracedecay_domain::configuration::ContextScoutConfigurationStateV1::Paused
            }
            ContextScoutSurfaceRequest::Resume(_) => {
                tracedecay_domain::configuration::ContextScoutConfigurationStateV1::Active
            }
            _ => unreachable!("pause/resume matched above"),
        };
        return execute_context_scout_state_transition(
            wire_request_id,
            registered,
            owner,
            control,
            target,
            current,
            observed_at,
            deadline,
            cancellation,
        )
        .await;
    }
    let authority = match context_scout_request_authority(
        &registered,
        &wire_request_id,
        surface_operation,
        observed_at,
        deadline.clone(),
        cancellation,
    ) {
        Ok(authority) => authority,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let payload = match request {
        ContextScoutSurfaceRequest::Status(_) => owner
            .configured_status()
            .await
            .ok()
            .and_then(|status| serde_json::to_value(status).ok()),
        ContextScoutSurfaceRequest::Recent(request) => owner
            .recent_exact(request.address, request.limit)
            .await
            .ok()
            .and_then(|recent| serde_json::to_value(recent).ok()),
        ContextScoutSurfaceRequest::Explain(request) => owner
            .explain_exact(request.address, request.limit)
            .await
            .ok()
            .and_then(|explanation| serde_json::to_value(explanation).ok()),
        ContextScoutSurfaceRequest::Capability(_) => owner
            .capability()
            .await
            .ok()
            .and_then(|capability| serde_json::to_value(capability).ok()),
        ContextScoutSurfaceRequest::Budget(_) => owner
            .budget()
            .await
            .ok()
            .and_then(|budget| serde_json::to_value(budget).ok()),
        ContextScoutSurfaceRequest::Cancel(request) if request.work.address == request.address => {
            owner
                .cancel(request.work)
                .await
                .ok()
                .filter(|outcome| {
                    *outcome
                        != crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1::Unavailable
                })
                .map(|outcome| {
                    serde_json::json!({ "outcome": context_scout_store_outcome(outcome) })
                })
        }
        ContextScoutSurfaceRequest::Claim(request) => {
            let window = match request.window {
                crate::application_surface::ContextScoutClaimWindowSurfaceV1::IdleWindow => {
                    crate::agents::context_scout_v2::ContextScoutDeliveryWindowV1::IdleWindow
                }
                crate::application_surface::ContextScoutClaimWindowSurfaceV1::OnRequest => {
                    crate::agents::context_scout_v2::ContextScoutDeliveryWindowV1::OnRequest
                }
            };
            let digest = canonical_sha256(&(
                "tracedecay.context-scout.delivery-lease.v1",
                &wire_request_id,
                request.address,
                request.window,
                observed_at,
            ))
            .ok();
            let lease = digest.and_then(|digest| {
                let bytes = digest.as_str().as_bytes();
                (bytes.len() >= 16).then(|| {
                    let mut lease_id = [0; 16];
                    lease_id.copy_from_slice(&bytes[..16]);
                    crate::agents::context_scout_v2::ContextScoutLeaseV1 {
                        lease_id,
                        expires_at: UtcMicros(
                            deadline
                                .expires_at
                                .0
                                .min(observed_at.0.saturating_add(30_000_000)),
                        ),
                    }
                })
            });
            match lease {
                Some(lease) => match owner
                    .claim_delivery_exact(request.address, window, observed_at, lease)
                    .await
                {
                    crate::agents::context_scout_v2::ContextScoutDurableClaimOutcomeV1::Claimed(
                        claim,
                    ) => serde_json::to_value(claim).ok(),
                    crate::agents::context_scout_v2::ContextScoutDurableClaimOutcomeV1::Empty => {
                        Some(serde_json::json!({ "outcome": "empty" }))
                    }
                    crate::agents::context_scout_v2::ContextScoutDurableClaimOutcomeV1::Unavailable => {
                        None
                    }
                },
                None => None,
            }
        }
        ContextScoutSurfaceRequest::Delivery(request)
            if request.claim.entry.work.address == request.address =>
        {
            let outcome = owner
                .record_delivery(&request.claim, &request.receipt)
                .await;
            (outcome
                != crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1::Unavailable)
                .then(|| {
                    serde_json::json!({
                        "outcome": context_scout_store_outcome(outcome)
                    })
                })
        }
        ContextScoutSurfaceRequest::Feedback(request) => {
            let outcome = owner
                .record_feedback_exact(request.address, &request.receipt, request.feedback)
                .await;
            (outcome
                != crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1::Unavailable)
                .then(|| {
                    serde_json::json!({
                        "outcome": context_scout_store_outcome(outcome)
                    })
                })
        }
        ContextScoutSurfaceRequest::Pause(_)
        | ContextScoutSurfaceRequest::Resume(_)
        | ContextScoutSurfaceRequest::Cancel(_)
        | ContextScoutSurfaceRequest::Delivery(_) => None,
    };
    let Some(payload) = payload else {
        return application_problem(
            wire_request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "context_scout.unavailable".to_owned(),
                message: "The exact-address Context Scout operation is unavailable".to_owned(),
            }),
        );
    };
    match configuration_evidence(payload, authority, observed_at, deadline) {
        Ok(outcome) => DaemonInvocationResponse::with_outcome(
            wire_request_id,
            DaemonInvocationOutcome::ContextScout {
                scope: registered.scope,
                outcome,
            },
        ),
        Err(error) => application_problem(wire_request_id, configuration_problem(error)),
    }
}

async fn execute_context_scout_state_transition(
    wire_request_id: String,
    registered: RegisteredConfigurationRuntime,
    owner: Arc<crate::agents::context_scout_owner::ProjectContextScoutOwnerV1>,
    control: &crate::application_surface::ContextScoutControlSurfaceRequest,
    target: tracedecay_domain::configuration::ContextScoutConfigurationStateV1,
    current: crate::application::configuration::ConfigurationCurrentStateV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    if control.expected_revision != current.revision_id {
        return application_problem(
            wire_request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "context_scout.configuration_stale".to_owned(),
                message: "The Context Scout configuration revision is stale".to_owned(),
            }),
        );
    }
    let Some(key) = tracedecay_domain::configuration::SettingKey::new(
        tracedecay_domain::configuration::CONTEXT_SCOUT_SETTINGS_SETTING_KEY,
    )
    .ok() else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::Unavailable,
        );
    };
    let Some(tracedecay_domain::configuration::ConfigurationValueV1::ContextScoutSettings(
        mut settings,
    )) = current.snapshot.effective_values.get(&key).cloned()
    else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::NotFoundOrNotAuthorized,
        );
    };
    let valid_transition = matches!(
        (settings.state, target),
        (
            tracedecay_domain::configuration::ContextScoutConfigurationStateV1::Active,
            tracedecay_domain::configuration::ContextScoutConfigurationStateV1::Paused
        ) | (
            tracedecay_domain::configuration::ContextScoutConfigurationStateV1::Paused,
            tracedecay_domain::configuration::ContextScoutConfigurationStateV1::Active
        )
    );
    if !valid_transition {
        return application_problem(
            wire_request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "context_scout.invalid_state_transition".to_owned(),
                message: "The Context Scout state transition is unavailable".to_owned(),
            }),
        );
    }
    settings.state = target;
    let response = execute_configuration(
        wire_request_id,
        Some(registered.clone()),
        crate::application_surface::ApplicationSurfaceOperation::ConfigurationSet,
        ConfigurationSurfaceRequest::Set(
            crate::application_surface::ConfigurationSetSurfaceRequest {
                layer: tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
                    project_id: registered.scope.project_id.clone(),
                },
                key,
                value: tracedecay_domain::configuration::ConfigurationValueV1::ContextScoutSettings(
                    settings,
                ),
                expected_revision: current.revision_id,
            },
        ),
        observed_at,
        deadline,
        cancellation,
    )
    .await;
    let DaemonInvocationResponse {
        protocol,
        revision,
        request_id,
        outcome,
    } = response;
    let DaemonInvocationOutcome::Configuration { scope, outcome } = outcome else {
        return DaemonInvocationResponse {
            protocol,
            revision,
            request_id,
            outcome,
        };
    };
    let refreshed = registered
        .runtime
        .client()
        .current()
        .await
        .ok()
        .map(
            |current| crate::application::configuration::ConfigurationCurrentStateV1 {
                revision_id: current.revision_id,
                snapshot: current.snapshot,
            },
        )
        .and_then(|current| {
            crate::agents::context_scout_ports::ContextScoutConfigurationPinV1::from_current(
                &current,
            )
        });
    if let Some(refreshed) = refreshed {
        if owner.install_state_transition(refreshed).await.is_err() {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    } else {
        return DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable);
    }
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::ContextScout { scope, outcome },
    )
}

const fn context_scout_store_outcome(
    outcome: crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1,
) -> &'static str {
    match outcome {
        crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1::Stored => "stored",
        crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1::Duplicate => {
            "duplicate"
        }
        crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1::Superseded => {
            "superseded"
        }
        crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1::Unavailable => {
            "unavailable"
        }
    }
}

async fn execute_configuration(
    wire_request_id: String,
    registered: Option<RegisteredConfigurationRuntime>,
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    request: ConfigurationSurfaceRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(registered) = registered else {
        return concealed_application_problem(wire_request_id);
    };
    if cancellation.is_cancelled() {
        return application_problem(
            wire_request_id,
            ApplicationProblem::cancelled_before_admission(),
        );
    }
    if deadline.is_elapsed_at(observed_at) || deadline.is_elapsed_at(current_micros()) {
        return application_problem(
            wire_request_id,
            ApplicationProblem::timed_out_before_admission(),
        );
    }
    let authority = match configuration_request_authority(
        &registered,
        &wire_request_id,
        surface_operation,
        observed_at,
        deadline.clone(),
        cancellation,
    ) {
        Ok(authority) => authority,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let actor = AuthorizedActor {
        actor_id: registered.actor.clone(),
    };
    let client = registered.runtime.client();
    let result: Result<ApplicationOutcome<serde_json::Value>, ConfigurationError> = async {
        match (surface_operation, request) {
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationList,
                ConfigurationSurfaceRequest::List(_),
            ) => configuration_evidence(
                serde_json::to_value(client.list(actor).await?)
                    .map_err(|_| ConfigurationError::Unavailable)?,
                authority,
                observed_at,
                deadline,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationExplain,
                ConfigurationSurfaceRequest::Explain(request),
            ) => configuration_evidence(
                serde_json::to_value(client.explain(actor, request.key).await?)
                    .map_err(|_| ConfigurationError::Unavailable)?,
                authority,
                observed_at,
                deadline,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationGet,
                ConfigurationSurfaceRequest::Get(request),
            ) => configuration_evidence(
                serde_json::to_value(client.get(actor, request.key).await?)
                    .map_err(|_| ConfigurationError::Unavailable)?,
                authority,
                observed_at,
                deadline,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationObservedState,
                ConfigurationSurfaceRequest::ObservedState(_),
            ) => configuration_evidence(
                serde_json::to_value(client.observed_state(actor).await?)
                    .map_err(|_| ConfigurationError::Unavailable)?,
                authority,
                observed_at,
                deadline,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationAudit,
                ConfigurationSurfaceRequest::Audit(request),
            ) => configuration_evidence(
                serde_json::to_value(
                    client
                        .audit(
                            actor,
                            ConfigurationAuditQuery {
                                after_event_id: request.after_event_id,
                                limit: request.limit,
                            },
                        )
                        .await?,
                )
                .map_err(|_| ConfigurationError::Unavailable)?,
                authority,
                observed_at,
                deadline,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationSet,
                ConfigurationSurfaceRequest::Set(request),
            ) => {
                let mutation = DirectConfigurationMutation::Set {
                    layer: request.layer,
                    key: request.key,
                    value: request.value,
                };
                let mutation_authority = issue_direct_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    &mutation,
                    request.expected_revision.clone(),
                    observed_at,
                )?;
                let receipt = apply_configuration_or_semantic_transition(
                    &registered,
                    mutation_authority,
                    mutation,
                    request.expected_revision.clone(),
                    observed_at,
                )
                .await?;
                configuration_effect(
                    serde_json::to_value(&receipt).map_err(|_| ConfigurationError::Unavailable)?,
                    authority,
                    &registered.actor,
                    &registered.scope,
                    surface_operation,
                    &wire_request_id,
                    &request.expected_revision,
                    receipt.operation_digest,
                    observed_at,
                    deadline,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationUnset,
                ConfigurationSurfaceRequest::Unset(request),
            ) => {
                let mutation = DirectConfigurationMutation::Unset {
                    layer: request.layer,
                    key: request.key,
                };
                let mutation_authority = issue_direct_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    &mutation,
                    request.expected_revision.clone(),
                    observed_at,
                )?;
                let receipt = apply_configuration_or_semantic_transition(
                    &registered,
                    mutation_authority,
                    mutation,
                    request.expected_revision.clone(),
                    observed_at,
                )
                .await?;
                configuration_effect(
                    serde_json::to_value(&receipt).map_err(|_| ConfigurationError::Unavailable)?,
                    authority,
                    &registered.actor,
                    &registered.scope,
                    surface_operation,
                    &wire_request_id,
                    &request.expected_revision,
                    receipt.operation_digest,
                    observed_at,
                    deadline,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationBatch,
                ConfigurationSurfaceRequest::Batch(request),
            ) => {
                let mutations = request
                    .mutations
                    .into_iter()
                    .map(|mutation| match mutation {
                        crate::application_surface::ConfigurationDirectMutationSurfaceRequest::Set {
                            layer,
                            key,
                            value,
                        } => DirectConfigurationMutation::Set { layer, key, value },
                        crate::application_surface::ConfigurationDirectMutationSurfaceRequest::Unset {
                            layer,
                            key,
                        } => DirectConfigurationMutation::Unset { layer, key },
                    })
                    .collect();
                let mutation = DirectConfigurationMutation::Batch { mutations };
                let mutation_authority = issue_direct_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    &mutation,
                    request.expected_revision.clone(),
                    observed_at,
                )?;
                let receipt = apply_configuration_or_semantic_transition(
                    &registered,
                    mutation_authority,
                    mutation,
                    request.expected_revision.clone(),
                    observed_at,
                )
                .await?;
                configuration_effect(
                    serde_json::to_value(&receipt).map_err(|_| ConfigurationError::Unavailable)?,
                    authority,
                    &registered.actor,
                    &registered.scope,
                    surface_operation,
                    &wire_request_id,
                    &request.expected_revision,
                    receipt.operation_digest,
                    observed_at,
                    deadline,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationWriteCredential,
                ConfigurationSurfaceRequest::WriteCredential(request),
            ) => {
                let mutation_authority = issue_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    ConfigurationMutationOperationV1::CredentialWrite,
                    registered.scope.scope_digest.clone(),
                    request.expected_revision.clone(),
                    ConfigurationMutationSinkV1::CredentialStore,
                    ConfigurationMutationEffectV1::WriteCredentialReference,
                    observed_at,
                )?;
                let metadata = client
                    .write_credential(
                        mutation_authority,
                        WriteOnlyCredentialMutation {
                            expected_reference_id: request.expected_reference_id,
                            kind: request.kind,
                            write_handle: CredentialWriteHandleV1::new(request.write_handle)?,
                        },
                        request.expected_revision.clone(),
                    )
                    .await?;
                let payload =
                    serde_json::to_value(&metadata).map_err(|_| ConfigurationError::Unavailable)?;
                let digest = canonical_sha256(&(
                    "tracedecay.configuration.credential-surface.v1",
                    &payload,
                ))
                .map_err(ConfigurationError::validation)?;
                configuration_effect(
                    payload,
                    authority,
                    &registered.actor,
                    &registered.scope,
                    surface_operation,
                    &wire_request_id,
                    &request.expected_revision,
                    digest,
                    observed_at,
                    deadline,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationProtectedPreview,
                ConfigurationSurfaceRequest::ProtectedPreview(request),
            ) => {
                let mutation_authority = issue_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    ConfigurationMutationOperationV1::ProtectedDryRun,
                    registered.scope.scope_digest.clone(),
                    request.expected_revision.clone(),
                    ConfigurationMutationSinkV1::ConfigurationStore,
                    ConfigurationMutationEffectV1::CreateProtectedChangePlan,
                    observed_at,
                )?;
                let plan = client
                    .dry_run_protected_change(
                        mutation_authority,
                        request.change,
                        request.expected_revision.clone(),
                    )
                    .await?;
                configuration_preview(
                    serde_json::to_value(&plan).map_err(|_| ConfigurationError::Unavailable)?,
                    authority,
                    plan.plan_id.as_str(),
                    plan.operation_digest,
                    &request.expected_revision,
                    observed_at,
                    deadline,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationProtectedApply,
                ConfigurationSurfaceRequest::ProtectedApply(request),
            ) => {
                let mutation_authority = issue_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    ConfigurationMutationOperationV1::ProtectedApply,
                    registered.scope.scope_digest.clone(),
                    request.expected_base_revision_id.clone(),
                    ConfigurationMutationSinkV1::ConfigurationStore,
                    ConfigurationMutationEffectV1::CommitConfigurationRevision,
                    observed_at,
                )?;
                let receipt = client
                    .apply_protected_change(
                        mutation_authority,
                        ProtectedApplyRequest {
                            plan_id: request.plan_id,
                            actor_id: registered.actor.clone(),
                            expected_base_revision_id: request.expected_base_revision_id.clone(),
                            operation_digest: request.operation_digest,
                            idempotency_key: request.idempotency_key,
                        },
                    )
                    .await?;
                configuration_effect(
                    serde_json::to_value(&receipt).map_err(|_| ConfigurationError::Unavailable)?,
                    authority,
                    &registered.actor,
                    &registered.scope,
                    surface_operation,
                    &wire_request_id,
                    &request.expected_base_revision_id,
                    receipt.operation_digest,
                    observed_at,
                    deadline,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationRollbackPreview,
                ConfigurationSurfaceRequest::RollbackPreview(request),
            ) => {
                let current = client.current().await?;
                let mutation_authority = issue_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    ConfigurationMutationOperationV1::RollbackDryRun,
                    registered.scope.scope_digest.clone(),
                    current.revision_id.clone(),
                    ConfigurationMutationSinkV1::ConfigurationStore,
                    ConfigurationMutationEffectV1::CreateProtectedChangePlan,
                    observed_at,
                )?;
                let plan = client
                    .dry_run_rollback(
                        mutation_authority,
                        ConfigurationRollbackRequest {
                            target_revision_id: request.target_revision_id,
                            mode: request.mode,
                        },
                    )
                    .await?;
                configuration_preview(
                    serde_json::to_value(&plan).map_err(|_| ConfigurationError::Unavailable)?,
                    authority,
                    plan.plan_id.as_str(),
                    plan.operation_digest,
                    &current.revision_id,
                    observed_at,
                    deadline,
                )
            }
            (
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationRollbackApply,
                ConfigurationSurfaceRequest::RollbackApply(request),
            ) => {
                let mutation_authority = issue_configuration_mutation_authority(
                    &registered,
                    &wire_request_id,
                    ConfigurationMutationOperationV1::RollbackApply,
                    registered.scope.scope_digest.clone(),
                    request.expected_base_revision_id.clone(),
                    ConfigurationMutationSinkV1::ConfigurationStore,
                    ConfigurationMutationEffectV1::CommitConfigurationRevision,
                    observed_at,
                )?;
                let receipt = client
                    .apply_rollback(
                        mutation_authority,
                        ProtectedApplyRequest {
                            plan_id: request.plan_id,
                            actor_id: registered.actor.clone(),
                            expected_base_revision_id: request.expected_base_revision_id.clone(),
                            operation_digest: request.operation_digest,
                            idempotency_key: request.idempotency_key,
                        },
                    )
                    .await?;
                configuration_effect(
                    serde_json::to_value(&receipt).map_err(|_| ConfigurationError::Unavailable)?,
                    authority,
                    &registered.actor,
                    &registered.scope,
                    surface_operation,
                    &wire_request_id,
                    &request.expected_base_revision_id,
                    receipt.operation_digest,
                    observed_at,
                    deadline,
                )
            }
            _ => Err(ConfigurationError::validation_message(
                "configuration surface operation does not match its request",
            )),
        }
    }
    .await;

    match result {
        Ok(outcome) => DaemonInvocationResponse::with_outcome(
            wire_request_id,
            DaemonInvocationOutcome::Configuration {
                scope: registered.scope,
                outcome,
            },
        ),
        Err(error) => application_problem(wire_request_id, configuration_problem(error)),
    }
}

async fn apply_configuration_or_semantic_transition(
    registered: &RegisteredConfigurationRuntime,
    authority: ConfigurationMutationAuthority,
    mutation: DirectConfigurationMutation,
    expected_revision: ConfigurationRevisionId,
    now: UtcMicros,
) -> Result<crate::application::configuration::ConfigurationMutationReceipt, ConfigurationError> {
    let semantic_profile = semantic_profile_transition(&mutation)?;
    let receipt = if let Some(semantic_profile) = semantic_profile {
        let operation = registered
            .semantic_operation
            .get()
            .cloned()
            .ok_or(ConfigurationError::Unavailable)?;
        match semantic_profile {
            Some(selected_profile) => operation
                .activate(SemanticProtectedActivationOperationV1 {
                    authority,
                    selected_profile,
                    central_mutation: mutation,
                    now,
                })
                .await
                .map(|applied| applied.configuration_receipt)
                .map_err(map_semantic_configuration_error)?,
            None => operation
                .rollback(SemanticProtectedRollbackOperationV1 {
                    authority,
                    central_mutation: mutation,
                    trigger: "configuration_semantic_profile_disabled".to_owned(),
                    now,
                })
                .await
                .map(|applied| applied.configuration_receipt)
                .map_err(map_semantic_configuration_error)?,
        }
    } else {
        registered
            .runtime
            .client()
            .mutate_direct(authority, mutation, expected_revision)
            .await?
    };
    let current = registered.runtime.client().current().await?;
    crate::config::install_pinned_runtime_configuration(current.clone())
        .map_err(|_| ConfigurationError::Unavailable)?;
    registered
        .runtime
        .record_runtime_activation(Some(current.revision_id), None, now)
        .await?;
    Ok(receipt)
}

fn semantic_profile_transition(
    mutation: &DirectConfigurationMutation,
) -> Result<Option<Option<crate::config::SemanticProfileSelection>>, ConfigurationError> {
    match mutation {
        DirectConfigurationMutation::Set { key, value, .. }
            if key.as_str() == crate::config::SEMANTIC_RUNTIME_SETTING_KEY =>
        {
            let tracedecay_domain::configuration::ConfigurationValueV1::Text(value) = value else {
                return Err(ConfigurationError::validation_message(
                    "semantic runtime configuration must be canonical JSON text",
                ));
            };
            let semantic: crate::config::SemanticConfig =
                serde_json::from_str(value).map_err(|_| {
                    ConfigurationError::validation_message(
                        "semantic runtime configuration is invalid",
                    )
                })?;
            semantic.validate().map_err(|_| {
                ConfigurationError::validation_message("semantic runtime configuration is invalid")
            })?;
            Ok(Some(semantic.active_profile))
        }
        DirectConfigurationMutation::Unset { key, .. }
            if key.as_str() == crate::config::SEMANTIC_RUNTIME_SETTING_KEY =>
        {
            Ok(Some(None))
        }
        DirectConfigurationMutation::Batch { mutations } => {
            let mut semantic = None;
            for mutation in mutations {
                if let Some(next) = semantic_profile_transition(mutation)?
                    && semantic.replace(next).is_some()
                {
                    return Err(ConfigurationError::validation_message(
                        "semantic runtime configuration appears more than once",
                    ));
                }
            }
            Ok(semantic)
        }
        _ => Ok(None),
    }
}

fn map_semantic_configuration_error(
    error: SemanticActivationCoordinationErrorV1,
) -> ConfigurationError {
    match error {
        SemanticActivationCoordinationErrorV1::Unavailable => ConfigurationError::Unavailable,
        SemanticActivationCoordinationErrorV1::Conflict => ConfigurationError::RevisionConflict,
        SemanticActivationCoordinationErrorV1::Rejected
        | SemanticActivationCoordinationErrorV1::Runtime(_) => {
            ConfigurationError::validation_message("semantic configuration transition rejected")
        }
    }
}

fn issue_configuration_mutation_authority(
    registered: &RegisteredConfigurationRuntime,
    request_id: &str,
    operation: ConfigurationMutationOperationV1,
    scope_digest: ManifestDigest,
    expected_revision: ConfigurationRevisionId,
    sink: ConfigurationMutationSinkV1,
    effect: ConfigurationMutationEffectV1,
    observed_at: UtcMicros,
) -> Result<ConfigurationMutationAuthority, ConfigurationError> {
    registered
        .grants
        .issue(
            request_id,
            operation,
            scope_digest,
            expected_revision,
            sink,
            effect,
            observed_at,
        )
        .map_err(|_| ConfigurationError::Unavailable)
}

fn issue_direct_configuration_mutation_authority(
    registered: &RegisteredConfigurationRuntime,
    request_id: &str,
    mutation: &DirectConfigurationMutation,
    expected_revision: ConfigurationRevisionId,
    observed_at: UtcMicros,
) -> Result<ConfigurationMutationAuthority, ConfigurationError> {
    registered
        .grants
        .issue_direct(request_id, mutation, expected_revision, observed_at)
        .map_err(|problem| match problem {
            DaemonInvocationProblem::NotFoundOrNotAuthorized => {
                ConfigurationError::MutationAuthorityRejected
            }
            DaemonInvocationProblem::InvalidRequest => {
                ConfigurationError::validation_message("invalid configuration mutation target")
            }
            _ => ConfigurationError::Unavailable,
        })
}

fn configuration_request_authority(
    registered: &RegisteredConfigurationRuntime,
    request_id: &str,
    operation: crate::application_surface::ApplicationSurfaceOperation,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<AuthorityReceipt, ApplicationProblem> {
    if observed_at >= registered.grants.expires_at {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    let application_operation =
        tracedecay_application::configuration::configuration_surface_operation(operation.as_str())
            .map_err(|_| invalid_configuration_request())?
            .ok_or_else(invalid_configuration_request)?;
    let expires_at = UtcMicros(deadline.expires_at.0.min(registered.grants.expires_at.0));
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.daemon.configuration.{request_id}"))
            .map_err(|_| invalid_configuration_request())?,
        1,
        stable_digest(&(
            "tracedecay.daemon.configuration-route-grant.v1",
            request_id,
            &registered.scope,
            operation,
        ))?,
        ActorId::new("actor.tracedecay-daemon").map_err(|_| invalid_configuration_request())?,
        observed_at,
        expires_at,
        registered.scope.clone(),
        std::collections::BTreeSet::from([application_operation.capability_id().clone()]),
        std::collections::BTreeSet::from([application_operation.use_case_id().clone()]),
        DisclosureClass::Sensitive,
    )
    .map_err(|_| invalid_configuration_request())?;
    let context = RequestContext::new(
        registered.actor.clone(),
        registered.scope.clone(),
        grant,
        RequestId::new(request_id).map_err(|_| invalid_configuration_request())?,
        deadline,
        cancellation,
    )
    .map_err(|_| invalid_configuration_request())?;
    let policy_digest = ManifestDigest::new(registered.grants.policy_digest.as_str().to_owned())
        .map_err(|_| invalid_configuration_request())?;
    AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.daemon.configuration.v1",
            registered.grants.policy_epoch,
            policy_digest,
            ComponentVersion::new("tracedecay.daemon.configuration-policy.v1")
                .map_err(|_| invalid_configuration_request())?,
        )
        .map_err(|_| invalid_configuration_request())?,
        observed_at,
    )
    .map_err(|_| invalid_configuration_request())
}

fn context_scout_request_authority(
    registered: &RegisteredConfigurationRuntime,
    request_id: &str,
    operation: crate::application_surface::ApplicationSurfaceOperation,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<AuthorityReceipt, ApplicationProblem> {
    if observed_at >= registered.grants.expires_at {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    let application_operation =
        tracedecay_application::context_scout::context_scout_surface_operation(operation.as_str())
            .map_err(|_| invalid_configuration_request())?
            .ok_or_else(invalid_configuration_request)?;
    let expires_at = UtcMicros(deadline.expires_at.0.min(registered.grants.expires_at.0));
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.daemon.context-scout.{request_id}"))
            .map_err(|_| invalid_configuration_request())?,
        1,
        stable_digest(&(
            "tracedecay.daemon.context-scout-route-grant.v1",
            request_id,
            &registered.scope,
            operation,
        ))?,
        ActorId::new("actor.tracedecay-daemon").map_err(|_| invalid_configuration_request())?,
        observed_at,
        expires_at,
        registered.scope.clone(),
        std::collections::BTreeSet::from([application_operation.capability_id().clone()]),
        std::collections::BTreeSet::from([application_operation.use_case_id().clone()]),
        DisclosureClass::Sensitive,
    )
    .map_err(|_| invalid_configuration_request())?;
    let context = RequestContext::new(
        registered.actor.clone(),
        registered.scope.clone(),
        grant,
        RequestId::new(request_id).map_err(|_| invalid_configuration_request())?,
        deadline,
        cancellation,
    )
    .map_err(|_| invalid_configuration_request())?;
    let policy_digest = ManifestDigest::new(registered.grants.policy_digest.as_str().to_owned())
        .map_err(|_| invalid_configuration_request())?;
    AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.daemon.context-scout.v1",
            registered.grants.policy_epoch,
            policy_digest,
            ComponentVersion::new("tracedecay.daemon.context-scout-policy.v1")
                .map_err(|_| invalid_configuration_request())?,
        )
        .map_err(|_| invalid_configuration_request())?,
        observed_at,
    )
    .map_err(|_| invalid_configuration_request())
}

fn configuration_evidence(
    payload: serde_json::Value,
    authority: AuthorityReceipt,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<ApplicationOutcome<serde_json::Value>, ConfigurationError> {
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )
    .map_err(ConfigurationError::validation)?;
    let packet = EvidencePacket {
        temporal: TemporalState::current(execution.ended_at),
        authority,
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Operational], 1, 1, 1)
            .map_err(ConfigurationError::validation)?,
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new("sort.configuration.stable.v1")
                .map_err(ConfigurationError::validation)?,
            1,
            Some(1),
            1,
        )
        .map_err(ConfigurationError::validation)?,
        execution,
        payload: Some(payload),
    };
    Ok(ApplicationOutcome::Evidence(packet))
}

fn configuration_preview(
    payload: serde_json::Value,
    authority: AuthorityReceipt,
    preview_id: &str,
    preview_digest: ManifestDigest,
    expected_revision: &ConfigurationRevisionId,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<ApplicationOutcome<serde_json::Value>, ConfigurationError> {
    let expected_state = canonical_sha256(&(
        "tracedecay.configuration.expected-revision.v1",
        expected_revision,
    ))
    .map_err(ConfigurationError::validation)?;
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )
    .map_err(ConfigurationError::validation)?;
    Ok(ApplicationOutcome::Preview(
        PreviewResult::new(
            PreviewId::new(preview_id.to_owned()).map_err(ConfigurationError::validation)?,
            preview_digest,
            EffectClass::ConfigurationWrite,
            authority,
            expected_state,
            execution,
            Some(payload),
        )
        .map_err(ConfigurationError::validation)?,
    ))
}

#[allow(clippy::too_many_arguments)]
fn configuration_effect(
    payload: serde_json::Value,
    authority: AuthorityReceipt,
    actor: &ActorId,
    scope: &ResolvedScope,
    operation: crate::application_surface::ApplicationSurfaceOperation,
    request_id: &str,
    expected_revision: &ConfigurationRevisionId,
    operation_digest: ManifestDigest,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<ApplicationOutcome<serde_json::Value>, ConfigurationError> {
    let application_operation =
        tracedecay_application::configuration::configuration_surface_operation(operation.as_str())
            .map_err(ConfigurationError::validation)?
            .ok_or_else(|| {
                ConfigurationError::validation_message("unknown configuration operation")
            })?;
    let idempotency_digest = derive_logical_effect_idempotency(
        LogicalEffectIdempotencyDomain::ConfigurationEffect,
        &(
            actor,
            scope,
            operation.as_str(),
            expected_revision,
            &operation_digest,
        ),
    )
    .map_err(|error| ConfigurationError::validation_message(error.to_string()))?;
    let idempotency_suffix = idempotency_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| {
            ConfigurationError::validation_message(
                "configuration effect idempotency digest is malformed",
            )
        })?;
    let idempotency_key = IdempotencyKey::new(format!("configuration.effect.{idempotency_suffix}"))
        .map_err(ConfigurationError::validation)?;
    let expected_state = canonical_sha256(&(
        "tracedecay.configuration.expected-revision.v1",
        expected_revision,
    ))
    .map_err(ConfigurationError::validation)?;
    let committed_state = canonical_sha256(&(
        "tracedecay.configuration.committed-effect.v1",
        &operation_digest,
        &payload,
    ))
    .map_err(ConfigurationError::validation)?;
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )
    .map_err(ConfigurationError::validation)?;
    let receipt = EffectReceipt {
        operation: application_operation.use_case_id().clone(),
        request_id: RequestId::new(request_id).map_err(ConfigurationError::validation)?,
        actor: actor.clone(),
        scope: scope.clone(),
        effect_class: EffectClass::ConfigurationWrite,
        idempotency_key: idempotency_key.clone(),
        input_digest: operation_digest,
        expected_state: expected_state.clone(),
        policy_digest: authority.policy.digest.clone(),
        configuration_digest: committed_state.clone(),
        catalog_digest: stable_digest(&"tracedecay.application.catalog.v1")
            .map_err(|_| ConfigurationError::Unavailable)?,
        privacy_digest: stable_digest(&"tracedecay.application.privacy.v1")
            .map_err(|_| ConfigurationError::Unavailable)?,
        outcome: EffectTermination::Completed,
        committed_state: Some(committed_state),
        external_proof: None,
    };
    let effect = EffectResult::new(
        EffectId::new(format!("effect.configuration.{idempotency_suffix}"))
            .map_err(ConfigurationError::validation)?,
        EffectClass::ConfigurationWrite,
        idempotency_key,
        authority,
        expected_state,
        execution,
        ReconciliationState::Reconciled,
        receipt,
        Some(payload),
    )
    .map_err(ConfigurationError::validation)?;
    Ok(ApplicationOutcome::Effect(effect))
}

fn invalid_configuration_request() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "configuration.invalid_request".to_owned(),
            message: "The configuration request is invalid".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: Vec::new(),
    }
}

fn configuration_problem(error: ConfigurationError) -> ApplicationProblem {
    match error {
        ConfigurationError::TargetUnavailable
        | ConfigurationError::AuthorizedTargetAmbiguous
        | ConfigurationError::MutationAuthorityRejected
        | ConfigurationError::ProjectlessProfileRequired => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        ConfigurationError::RevisionConflict | ConfigurationError::IdempotencyConflict => {
            ApplicationProblem::Conflict {
                diagnostic: SafeDiagnostic {
                    code: "configuration.conflict".to_owned(),
                    message: "The configuration request conflicts with current state".to_owned(),
                },
                retry: RetryDirective::AfterRevalidate,
                legal_actions: vec![tracedecay_application::LegalAction::Refresh],
            }
        }
        ConfigurationError::PlanExpired | ConfigurationError::PlanStale => {
            ApplicationProblem::stale(SafeDiagnostic {
                code: "configuration.stale".to_owned(),
                message: "The configuration preview is stale".to_owned(),
            })
        }
        ConfigurationError::PolicyWideningForbidden | ConfigurationError::Validation(_) => {
            invalid_configuration_request()
        }
        ConfigurationError::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "configuration.unavailable".to_owned(),
            message: "The configuration authority is unavailable".to_owned(),
        }),
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
    WorkAttempt {
        scope: ResolvedScope,
        outcome: ApplicationOutcome<WorkAttemptResponseV1>,
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

    fn lsp_opened(request_id: String, session: DaemonLspSessionAccess, expires_at_ms: u64) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id,
            outcome: DaemonInvocationOutcome::LspOpened {
                session,
                expires_at_ms,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Pr13HookOrchestrationAdmissionV1 {
    Enqueued,
    Backpressured,
    UnsupportedTrigger,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pr13HookOrchestrationTriggerV1 {
    SavedEdit,
    Stop,
    Explicit,
}

#[derive(Clone)]
pub(crate) struct Pr13HookOrchestrationRequestV1 {
    pub hook: AdmittedContextScoutHookV1,
    pub lifecycle: Option<ContextScoutLifecycleAddressV1>,
    pub hook_configuration_revision: u64,
    pub trigger: Pr13HookOrchestrationTriggerV1,
    completion: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
}

impl Pr13HookOrchestrationRequestV1 {
    pub(in crate::daemon) fn from_envelope(
        envelope: HookEventEnvelopeV2,
        binding: &HookScopeBindingV1,
        lifecycle: Option<ContextScoutLifecycleAddressV1>,
        configuration_revision: u64,
        explicit: bool,
    ) -> Option<Self> {
        let hook = AdmittedContextScoutHookV1::new(envelope, binding)?;
        let trigger = if explicit {
            Pr13HookOrchestrationTriggerV1::Explicit
        } else {
            match &hook.envelope().event {
                HookEventV2::SavedEdit { .. } => Pr13HookOrchestrationTriggerV1::SavedEdit,
                HookEventV2::SessionBoundary {
                    boundary: HookBoundaryV1::End | HookBoundaryV1::TurnComplete,
                } => Pr13HookOrchestrationTriggerV1::Stop,
                _ => return None,
            }
        };
        Some(Self {
            hook,
            lifecycle,
            hook_configuration_revision: configuration_revision,
            trigger,
            completion: None,
        })
    }
}

/// Process-local bridge from an authenticated Hook V2 callback to the
/// project-open advisory owner. Implementations must return before provider,
/// retrieval, or model work begins.
pub(crate) trait Pr13HookOrchestrationPortV1: Send + Sync {
    fn admit(&self, request: Pr13HookOrchestrationRequestV1) -> Pr13HookOrchestrationAdmissionV1;
}

pub(crate) struct Pr13AdvisoryCycleInvocationRequestV1 {
    pub request_id: String,
    pub document_uri: String,
    pub observed_at: UtcMicros,
    pub deadline: Deadline,
    pub cancellation: CancellationContext,
}

/// Typed terminal state of the four-pillar cycle that minted a diagnostics
/// handle. `published` separates a complete-coverage cycle recorded in the
/// shared publication store from a cycle whose canonical result is truthfully
/// incomplete and therefore not publishable, so callers never read partial
/// coverage as complete.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Pr13AdvisoryCycleTerminalV1 {
    pub termination: FeedbackCycleTerminationV1,
    pub provider_states: Vec<ProviderEvaluationStateV1>,
    pub published: bool,
}

pub(crate) struct Pr13AdvisoryCycleInvocationOutcomeV1 {
    pub request_handle: String,
    pub cycle: Pr13AdvisoryCycleTerminalV1,
}

pub(crate) type Pr13AdvisoryCycleInvocationFutureV1<'a> = Pin<
    Box<
        dyn Future<Output = Result<Pr13AdvisoryCycleInvocationOutcomeV1, ApplicationProblem>>
            + Send
            + 'a,
    >,
>;

/// Authenticated explicit entry to the same project-open four-pillar owner
/// used by LSP and Hook V2. The returned diagnostics handle is minted by that
/// owner from the cycle it just ran; callers never provide one.
pub(crate) trait Pr13AdvisoryCycleInvocationPortV1: Send + Sync {
    fn invoke(
        &self,
        request: Pr13AdvisoryCycleInvocationRequestV1,
    ) -> Pr13AdvisoryCycleInvocationFutureV1<'_>;
}

type Pr13HookOrchestrationFutureV1 = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type Pr13HookOrchestrationWorkV1 =
    dyn Fn(Pr13HookOrchestrationRequestV1) -> Pr13HookOrchestrationFutureV1 + Send + Sync;

pub(crate) struct BoundedPr13HookOrchestratorV1 {
    permits: Arc<Semaphore>,
    work: Arc<Pr13HookOrchestrationWorkV1>,
}

impl BoundedPr13HookOrchestratorV1 {
    pub(crate) fn new<F, Fut>(max_concurrent: usize, work: F) -> Option<Arc<Self>>
    where
        F: Fn(Pr13HookOrchestrationRequestV1) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let work: Arc<Pr13HookOrchestrationWorkV1> =
            Arc::new(move |request| Box::pin(work(request)));
        (max_concurrent > 0).then(|| {
            Arc::new(Self {
                permits: Arc::new(Semaphore::new(max_concurrent)),
                work,
            })
        })
    }
}

impl Pr13HookOrchestrationPortV1 for BoundedPr13HookOrchestratorV1 {
    fn admit(&self, request: Pr13HookOrchestrationRequestV1) -> Pr13HookOrchestrationAdmissionV1 {
        let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() else {
            return Pr13HookOrchestrationAdmissionV1::Backpressured;
        };
        let work = Arc::clone(&self.work);
        let completion = request.completion.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return Pr13HookOrchestrationAdmissionV1::Unavailable;
        };
        handle.spawn(async move {
            (work)(request).await;
            if let Some(completion) = completion {
                completion();
            }
            drop(permit);
        });
        Pr13HookOrchestrationAdmissionV1::Enqueued
    }
}

fn pr13_hook_orchestration_registry()
-> &'static StdMutex<BTreeMap<([u8; 16], [u8; 16]), Weak<dyn Pr13HookOrchestrationPortV1>>> {
    static REGISTRY: OnceLock<
        StdMutex<BTreeMap<([u8; 16], [u8; 16]), Weak<dyn Pr13HookOrchestrationPortV1>>>,
    > = OnceLock::new();
    REGISTRY.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

pub(crate) fn admit_registered_pr13_hook_orchestration(
    envelope: HookEventEnvelopeV2,
    binding: HookScopeBindingV1,
    lifecycle: Option<ContextScoutLifecycleAddressV1>,
    configuration_revision: u64,
    explicit: bool,
    completion: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
) -> Pr13HookOrchestrationAdmissionV1 {
    let Some(mut request) = Pr13HookOrchestrationRequestV1::from_envelope(
        envelope,
        &binding,
        lifecycle,
        configuration_revision,
        explicit,
    ) else {
        return Pr13HookOrchestrationAdmissionV1::UnsupportedTrigger;
    };
    let Some(runtime) = pr13_hook_orchestration_registry()
        .lock()
        .ok()
        .and_then(|registry| {
            registry
                .get(&(
                    request.hook.envelope().project_id,
                    request.hook.envelope().worktree_id,
                ))
                .cloned()
        })
        .and_then(|runtime| runtime.upgrade())
    else {
        return Pr13HookOrchestrationAdmissionV1::Unavailable;
    };
    request.completion = completion;
    runtime.admit(request)
}

pub(super) struct SwitchableFeedbackCycleRuntimeV1 {
    current: RwLock<Arc<dyn FeedbackCycleRuntimePort>>,
}

pub(in crate::daemon) fn observe_accepted_feedback_cycle_terminal(
    observations: &Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>,
    project_id: &ProjectId,
    request: &FeedbackCycleRequest,
    outcome: Plan26FeedbackOutcomeV1,
) {
    let trigger = match request.trigger {
        DiagnosticTrigger::DocumentSave => "document_save",
        DiagnosticTrigger::ExplicitDocumentDiagnostics => "explicit_document_diagnostics",
    };
    let Ok(subject) = canonical_sha256(&(
        "tracedecay.feedback.accepted-cycle.v1",
        project_id,
        &request.root_uri,
        &request.document_uri,
        trigger,
    )) else {
        return;
    };
    observations.observe_source_event_for_subject(
        subject,
        now_micros(),
        Plan26FeedbackSourceEventV1::Delivery {
            operation: Plan26FeedbackOperationV1::FeedbackCycle,
            route: Plan26DeliveryRouteV1::Lsp,
            outcome,
            item_count: 0,
            duration_micros: None,
        },
    );
}

pub(super) struct UnavailableFeedbackCycleRuntimeV1 {
    project_id: ProjectId,
    observations: Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>,
}

impl UnavailableFeedbackCycleRuntimeV1 {
    pub(super) fn new(
        project_id: ProjectId,
        observations: Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>,
    ) -> Self {
        Self {
            project_id,
            observations,
        }
    }
}

impl FeedbackCycleRuntimePort for UnavailableFeedbackCycleRuntimeV1 {
    fn execute(
        &self,
        request: FeedbackCycleRequest,
    ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
        let project_id = self.project_id.clone();
        let observations = Arc::clone(&self.observations);
        Box::pin(async move {
            observe_accepted_feedback_cycle_terminal(
                &observations,
                &project_id,
                &request,
                Plan26FeedbackOutcomeV1::Unavailable,
            );
            Err(LspRuntimeFailure::new("feedback-cycle-unavailable"))
        })
    }
}

impl SwitchableFeedbackCycleRuntimeV1 {
    fn new(current: Arc<dyn FeedbackCycleRuntimePort>) -> Self {
        Self {
            current: RwLock::new(current),
        }
    }

    pub(super) fn replace(
        &self,
        current: Arc<dyn FeedbackCycleRuntimePort>,
    ) -> Result<(), LspRuntimeFailure> {
        *self
            .current
            .write()
            .map_err(|_| LspRuntimeFailure::new("feedback-cycle-router"))? = current;
        Ok(())
    }
}

impl FeedbackCycleRuntimePort for SwitchableFeedbackCycleRuntimeV1 {
    fn execute(
        &self,
        request: FeedbackCycleRequest,
    ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
        let current = self
            .current
            .read()
            .map(|current| Arc::clone(&current))
            .map_err(|_| LspRuntimeFailure::new("feedback-cycle-router"));
        Box::pin(async move { current?.execute(request).await })
    }
}

/// Retained daemon state for the typed LSP invocation operations.
#[derive(Clone)]
pub(super) struct RegisteredWorkRuntime {
    database: Arc<crate::global_db::RegisteredGlobalDb>,
    runtime: Arc<DaemonWorkRuntimeV1<tracedecay_rusqlite_runtime::work::WorkSqliteStorage>>,
    actor: ActorId,
    grant: CapabilityGrantSnapshot,
    authority_digest: ManifestDigest,
    policy_digest: ManifestDigest,
    configuration_digest: ManifestDigest,
}

impl RegisteredWorkRuntime {
    /// Takes the provider runtime out for shutdown, dropping the rest of the
    /// registration with it.
    pub(super) fn into_runtime(
        self,
    ) -> Arc<DaemonWorkRuntimeV1<tracedecay_rusqlite_runtime::work::WorkSqliteStorage>> {
        self.runtime
    }
}

#[derive(Clone)]
pub(crate) struct DaemonInvocationService {
    code_index_schedulers: crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    lsp_sessions: Arc<Mutex<BTreeMap<LspSessionId, RuntimeLspSession>>>,
    context_scout_registries:
        Arc<Mutex<BTreeMap<ProjectId, Arc<ProjectContextScoutAddressRegistryV1>>>>,
    /// Every per-project component, published together under one lock. See
    /// [`ProjectRuntimeRegistryV1`] for why these are not twelve maps.
    project_runtimes: ProjectRuntimeRegistryV1,
    operation_events: OperationEventAuthority,
}

impl Default for DaemonInvocationService {
    fn default() -> Self {
        Self::with_code_index_schedulers(
            crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1::new(1),
        )
    }
}

impl DaemonInvocationService {
    pub(crate) fn with_code_index_schedulers(
        code_index_schedulers: crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    ) -> Self {
        Self {
            code_index_schedulers,
            lsp_sessions: Arc::new(Mutex::new(BTreeMap::new())),
            context_scout_registries: Arc::new(Mutex::new(BTreeMap::new())),
            project_runtimes: ProjectRuntimeRegistryV1::default(),
            operation_events: daemon_operation_event_authority(),
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum DaemonContextScoutRuntimeRegistrationError {
    #[error("a Context Scout address registry is already mounted for this project")]
    AlreadyRegistered,
    #[error("the Context Scout address registry could not be opened")]
    InvalidProjectIdentity,
}

#[derive(Clone)]
pub(crate) struct DaemonContextScoutRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonContextScoutRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn open_and_register(
        &self,
        database: Database,
        project_id: ProjectId,
    ) -> Result<Arc<ProjectContextScoutAddressRegistryV1>, DaemonContextScoutRuntimeRegistrationError>
    {
        let Some(registry) =
            ProjectContextScoutAddressRegistryV1::new(database, project_id.clone())
        else {
            return Err(DaemonContextScoutRuntimeRegistrationError::InvalidProjectIdentity);
        };
        let mut registries = self.service.context_scout_registries.lock().await;
        if registries.contains_key(&project_id) {
            return Err(DaemonContextScoutRuntimeRegistrationError::AlreadyRegistered);
        }
        registries.insert(project_id, Arc::clone(&registry));
        Ok(registry)
    }

    pub(crate) async fn get(
        &self,
        project_id: &ProjectId,
    ) -> Option<Arc<ProjectContextScoutAddressRegistryV1>> {
        self.service
            .context_scout_registries
            .lock()
            .await
            .get(project_id)
            .cloned()
    }
}

pub(super) struct RegisteredFeedbackRuntime {
    project_id: ProjectId,
    runtime: Arc<Pr12FeedbackRuntime>,
}

impl RegisteredFeedbackRuntime {
    pub(super) fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub(super) fn runtime(&self) -> Arc<Pr12FeedbackRuntime> {
        Arc::clone(&self.runtime)
    }

    pub(super) fn invocation_owner(&self) -> DaemonFeedbackInvocationOwner {
        DaemonFeedbackInvocationOwner::new(self.project_id.clone(), self.runtime.owner())
    }

    pub(super) fn source_observation_port(
        &self,
    ) -> Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync> {
        self.runtime.source_observation_port()
    }
}

#[derive(Clone)]
pub(super) struct RegisteredCallableCodeRuntime {
    scope: ResolvedScope,
    authorization: DaemonCallableCodeAuthorizationSource,
}

#[derive(Clone)]
struct DaemonConfigurationGrantAuthority {
    actor: ActorId,
    policy_epoch: u64,
    policy_digest: AccessPolicyDigest,
    expires_at: UtcMicros,
    direct_layers: Arc<BTreeMap<ManifestDigest, ConfigurationLayerIdV1>>,
    grants: Arc<RwLock<BTreeMap<ConfigurationGrantId, ConfigurationMutationGrantSnapshotV1>>>,
}

impl DaemonConfigurationGrantAuthority {
    fn issue_direct(
        &self,
        request_id: &str,
        mutation: &DirectConfigurationMutation,
        expected_revision: ConfigurationRevisionId,
        issued_at: UtcMicros,
    ) -> Result<ConfigurationMutationAuthority, DaemonInvocationProblem> {
        let layer = mutation
            .target_layer()
            .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
        let scope_digest = mutation
            .target_scope_digest()
            .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
        if self.direct_layers.get(&scope_digest) != Some(layer) {
            return Err(DaemonInvocationProblem::NotFoundOrNotAuthorized);
        }
        self.issue(
            request_id,
            ConfigurationMutationOperationV1::DirectMutation,
            scope_digest,
            expected_revision,
            ConfigurationMutationSinkV1::ConfigurationStore,
            ConfigurationMutationEffectV1::CommitConfigurationRevision,
            issued_at,
        )
    }

    #[cfg(test)]
    fn for_test(
        layers: impl IntoIterator<Item = ConfigurationLayerIdV1>,
        expires_at: UtcMicros,
    ) -> Self {
        Self {
            actor: ActorId::new("actor.configuration.test").expect("actor"),
            policy_epoch: 1,
            policy_digest: AccessPolicyDigest::new(format!("sha256:{}", "a".repeat(64)))
                .expect("policy"),
            expires_at,
            direct_layers: Arc::new(
                layers
                    .into_iter()
                    .map(|layer| {
                        let digest =
                            configuration_layer_scope_digest(&layer).expect("layer digest");
                        (digest, layer)
                    })
                    .collect(),
            ),
            grants: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    fn issue(
        &self,
        request_id: &str,
        operation: ConfigurationMutationOperationV1,
        scope_digest: ManifestDigest,
        expected_revision: ConfigurationRevisionId,
        sink: ConfigurationMutationSinkV1,
        effect: ConfigurationMutationEffectV1,
        issued_at: UtcMicros,
    ) -> Result<ConfigurationMutationAuthority, DaemonInvocationProblem> {
        if issued_at >= self.expires_at {
            return Err(DaemonInvocationProblem::NotFoundOrNotAuthorized);
        }
        let grant_id = ConfigurationGrantId::new(format!("configuration.grant.{request_id}"))
            .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
        let receipt_id =
            ConfigurationGrantReceiptId::new(format!("configuration.grant-receipt.{request_id}"))
                .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
        let permission = ConfigurationMutationPermissionV1 {
            operation,
            sink,
            effect,
        };
        let grant_digest = canonical_sha256(&(
            "tracedecay.daemon.configuration-grant.v1",
            &grant_id,
            &self.actor,
            &scope_digest,
            &expected_revision,
            permission,
            self.policy_epoch,
            &self.policy_digest,
            issued_at,
            self.expires_at,
        ))
        .map_err(|_| DaemonInvocationProblem::Unavailable)?;
        let receipt = ConfigurationMutationGrantReceiptV1::issue(
            receipt_id,
            grant_id.clone(),
            self.actor.clone(),
            operation,
            scope_digest.clone(),
            expected_revision.clone(),
            self.policy_epoch,
            self.policy_digest.clone(),
            sink,
            effect,
            issued_at,
            self.expires_at,
        )
        .map_err(|_| DaemonInvocationProblem::Unavailable)?;
        let snapshot = ConfigurationMutationGrantSnapshotV1 {
            grant_id: grant_id.clone(),
            grant_revision: 1,
            grant_digest,
            authorized_receipt_digest: receipt.receipt_digest.clone(),
            actor_id: self.actor.clone(),
            scope_digest: scope_digest.clone(),
            expected_configuration_revision: expected_revision.clone(),
            permissions: std::collections::BTreeSet::from([permission]),
            policy_epoch: self.policy_epoch,
            policy_digest: self.policy_digest.clone(),
            issued_at,
            expires_at: self.expires_at,
            state: ConfigurationMutationGrantStateV1::Active,
        };
        if !snapshot.is_valid() {
            return Err(DaemonInvocationProblem::Unavailable);
        }
        self.grants
            .write()
            .map_err(|_| DaemonInvocationProblem::Unavailable)?
            .insert(grant_id, snapshot);
        Ok(ConfigurationMutationAuthority { receipt })
    }
}

fn mounted_configuration_layers(
    project_id: &ProjectId,
    profile_id: &UserProfileId,
    snapshot: &ConfigurationSnapshotV1,
) -> Result<BTreeMap<ManifestDigest, ConfigurationLayerIdV1>, DaemonInvocationProblem> {
    let mut layers = std::collections::BTreeSet::from([
        ConfigurationLayerIdV1::Project {
            project_id: project_id.clone(),
        },
        ConfigurationLayerIdV1::UserProfile {
            profile_id: profile_id.clone(),
        },
    ]);
    layers.extend(
        snapshot
            .provenance
            .values()
            .flatten()
            .filter_map(|candidate| match &candidate.layer {
                ConfigurationLayerIdV1::Collection { .. }
                    if matches!(
                        candidate.disposition,
                        CandidateDispositionV1::Winning | CandidateDispositionV1::Defaulted
                    ) =>
                {
                    Some(candidate.layer.clone())
                }
                _ => None,
            }),
    );
    layers
        .into_iter()
        .map(|layer| {
            configuration_layer_scope_digest(&layer)
                .map(|digest| (digest, layer))
                .map_err(|_| DaemonInvocationProblem::Unavailable)
        })
        .collect()
}

impl ConfigurationMutationGrantAuthority for DaemonConfigurationGrantAuthority {
    fn current_grant<'a>(
        &'a self,
        grant_id: &'a ConfigurationGrantId,
    ) -> ConfigurationMutationGrantAuthorityFuture<'a> {
        let result = self
            .grants
            .read()
            .map_err(|_| ConfigurationMutationGrantAuthorityError::Unavailable)
            .and_then(|grants| {
                grants
                    .get(grant_id)
                    .cloned()
                    .ok_or(ConfigurationMutationGrantAuthorityError::Rejected)
            });
        Box::pin(async move { result })
    }
}

#[derive(Clone)]
struct DaemonConfigurationScopeResolution {
    actor: ActorId,
    evidence: ScopeRevalidationEvidenceV1,
}

impl ScopeResolutionPort for DaemonConfigurationScopeResolution {
    fn resolve_protected_change<'a>(
        &'a self,
        actor: &'a AuthorizedActor,
        change: &'a tracedecay_domain::configuration::ProtectedChange,
    ) -> crate::application::configuration::ConfigurationOperationFuture<
        'a,
        ScopeRevalidationEvidenceV1,
    > {
        let allowed = actor.actor_id == self.actor && change.validate().is_ok();
        let evidence = self.evidence.clone();
        Box::pin(async move {
            allowed
                .then_some(evidence)
                .ok_or(crate::application::configuration::ConfigurationError::TargetUnavailable)
        })
    }

    fn revalidate_plan<'a>(
        &'a self,
        actor: &'a AuthorizedActor,
        plan: &'a tracedecay_domain::configuration::ProtectedChangePlan,
    ) -> crate::application::configuration::ConfigurationOperationFuture<
        'a,
        ScopeRevalidationEvidenceV1,
    > {
        let allowed = actor.actor_id == self.actor && plan.validate().is_ok();
        let evidence = self.evidence.clone();
        Box::pin(async move {
            allowed
                .then_some(evidence)
                .ok_or(crate::application::configuration::ConfigurationError::TargetUnavailable)
        })
    }
}

#[derive(Clone)]
pub(super) struct RegisteredConfigurationRuntime {
    runtime: Arc<ProjectConfigurationRuntime>,
    scope: ResolvedScope,
    actor: ActorId,
    grants: DaemonConfigurationGrantAuthority,
    semantic_operation: Arc<OnceLock<Arc<ProductionSemanticConfigurationOperationV1>>>,
}

#[derive(Debug, Error)]
pub(crate) enum DaemonFeedbackRuntimeRegistrationError {
    #[error("a PR12 feedback runtime is already mounted for this project database")]
    AlreadyRegistered,
    #[error("the PR12 feedback runtime must be mounted before its cycle")]
    MissingRuntime,
    #[error("the PR12 feedback runtime could not be opened")]
    Runtime(#[from] Pr12FeedbackRuntimeError),
    #[error("the PR12 feedback cycle runtime could not be opened")]
    Cycle(#[from] Pr12FeedbackCycleRuntimeError),
    #[error("the PR11 policy evaluator composition is invalid")]
    Policy(#[from] ApplicationContractError),
}

#[derive(Clone)]
pub(crate) struct DaemonFeedbackRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonFeedbackRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    /// Resolve the read store from the feedback runtime mounted for this exact
    /// project root. Doctor receives no provider runtime or write authority.
    pub(crate) async fn doctor_read_store(
        &self,
        project_root: &Path,
    ) -> Option<ProjectFeedbackStore> {
        self.service
            .feedback_runtime(Some(project_root))
            .await
            .map(|runtime| runtime.publication_store())
    }

    /// Registers feedback readers from the authoritative admission result.
    pub(crate) async fn open_and_register(
        &self,
        database: Database,
        project_root: PathBuf,
        scope: ResolvedScope,
        access: ProjectSourceAccessSnapshot,
        configuration: Arc<ProjectConfigurationRuntime>,
    ) -> Result<ProjectFeedbackStore, DaemonFeedbackRuntimeRegistrationError> {
        if self
            .service
            .project_runtimes
            .holds::<RegisteredFeedbackRuntime>(&project_root)
            .await
        {
            return Err(DaemonFeedbackRuntimeRegistrationError::AlreadyRegistered);
        }
        let project_id = scope.project_id.clone();
        let runtime = Arc::new(
            open_pr12_feedback_runtime(
                database,
                project_root.clone(),
                scope.clone(),
                access.clone(),
            )
            .await?,
        );
        self.service
            .project_runtimes
            .publish(
                project_root.clone(),
                RegisteredCallableCodeRuntime {
                    authorization: DaemonCallableCodeAuthorizationSource::production(
                        project_root.clone(),
                        scope.clone(),
                        configuration,
                    ),
                    scope,
                },
            )
            .await;
        let publications = runtime.publication_store();
        let unavailable_cycle = Arc::new(UnavailableFeedbackCycleRuntimeV1::new(
            project_id.clone(),
            runtime.source_observation_port(),
        ));
        self.service
            .project_runtimes
            .publish(
                project_root.clone(),
                RegisteredFeedbackRuntime {
                    project_id,
                    runtime,
                },
            )
            .await;
        let _ = self
            .service
            .project_runtimes
            .register(
                project_root,
                Arc::new(SwitchableFeedbackCycleRuntimeV1::new(unavailable_cycle)),
            )
            .await;
        Ok(publications)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn open_cycle_and_register(
        &self,
        project_root: PathBuf,
        database: Database,
        runtime_state: Arc<dyn FeedbackRuntimeStatePort + Send + Sync>,
        policy_context: PolicyEvaluationContextV1,
        evidence_horizon: PolicyEvidenceHorizonV1,
        evaluated_at: UtcMicros,
        provider_candidates: Vec<(DiagnosticProviderIdentity, AnalyzerAdmissionInputV1)>,
        graph: Arc<TraceDecay>,
        affected_tests: Arc<dyn AffectedTestsRetrievalPort + Send + Sync>,
        operation: ApplicationOperation,
        graph_operation: ApplicationOperation,
        tests_operation: ApplicationOperation,
        lsp_input: Pr12FeedbackCycleLspInput,
        proximity: Arc<dyn ProductionFeedbackCycleProximityPortV1>,
    ) -> Result<Arc<Pr12FeedbackCycleRuntime>, DaemonFeedbackRuntimeRegistrationError> {
        let policy = PolicyEvaluatorCompositionV1::from_application_catalog()?;
        let correlation_state = evidence_horizon.routing_state();
        let correlation_availability = match correlation_state {
            TruthSourceStateV1::Fresh | TruthSourceStateV1::Partial => {
                CapabilityAvailabilityV1::Available
            }
            TruthSourceStateV1::Stale => CapabilityAvailabilityV1::Stale,
            TruthSourceStateV1::Unavailable => CapabilityAvailabilityV1::Unavailable,
            TruthSourceStateV1::Unknown => CapabilityAvailabilityV1::Unknown,
        };
        // The request context is validated against its grant's scope before it
        // reaches here (`RequestContext::validate` rejects a scope that differs
        // from the grant's), so this route really is scope-matched. Live
        // correlation only reads, so it requires the Read effect class.
        let correlation_policy = operation.evaluate_local_live_policy(
            &policy,
            &policy_context,
            correlation_availability,
            ScopeMatchV1::Match,
            correlation_state,
            CapabilityEffectClassV1::Read,
            TruthFreshnessRequirementV1::FreshOrPartial,
            evidence_horizon,
            evaluated_at,
        )?;
        let provider_admissions = provider_candidates
            .into_iter()
            .map(|(identity, input)| {
                AnalyzerAdmittedDiagnosticProviderV1::evaluate_current_plan20_snapshot(
                    &policy,
                    &policy_context,
                    identity,
                    input,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let feedback = self
            .service
            .feedback_runtime(Some(&project_root))
            .await
            .ok_or(DaemonFeedbackRuntimeRegistrationError::MissingRuntime)?;
        let observations = feedback.observation_port();
        let production_lsp_input = Arc::clone(&lsp_input);
        let runtime = open_pr12_feedback_cycle_runtime(
            database,
            feedback,
            runtime_state,
            correlation_policy,
            provider_admissions,
            graph,
            affected_tests,
            observations,
            operation,
            graph_operation,
            tests_operation,
            lsp_input,
            Some(Arc::new(self.service.code_index_schedulers.clone())),
        )?;
        let production_input = production_proximity_feedback_cycle_input(
            runtime.clone(),
            production_lsp_input,
            proximity,
        );
        let cycle_input = self
            .service
            .project_runtimes
            .get::<Arc<SwitchableFeedbackCycleRuntimeV1>>(&project_root)
            .await;
        if let Some(cycle_input) = cycle_input {
            cycle_input
                .replace(production_input)
                .map_err(|_| DaemonFeedbackRuntimeRegistrationError::MissingRuntime)?;
        } else {
            self.service
                .project_runtimes
                .publish(
                    project_root.clone(),
                    Arc::new(SwitchableFeedbackCycleRuntimeV1::new(production_input)),
                )
                .await;
        }
        self.service
            .project_runtimes
            .publish(project_root, runtime.clone())
            .await;
        Ok(runtime)
    }

    pub(crate) async fn install_advisory_cycle_input(
        &self,
        project_root: &Path,
        input: Arc<dyn FeedbackCycleRuntimePort>,
    ) -> Result<(), DaemonFeedbackRuntimeRegistrationError> {
        let router = self
            .service
            .project_runtimes
            .get::<Arc<SwitchableFeedbackCycleRuntimeV1>>(project_root)
            .await
            .ok_or(DaemonFeedbackRuntimeRegistrationError::MissingRuntime)?;
        router
            .replace(input)
            .map_err(|_| DaemonFeedbackRuntimeRegistrationError::MissingRuntime)
    }
}

#[derive(Debug, Error)]
pub(crate) enum DaemonPrimitiveRuntimeRegistrationError {
    #[error("a PR12 primitive runtime is already mounted for this project")]
    AlreadyRegistered,
}

/// Central project-open registration for the owned primitive facade.
#[derive(Clone)]
pub(crate) struct DaemonPrimitiveRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonPrimitiveRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    /// Retains the already-opened project runtime as its teardown owner.
    /// Scope/access were bound by the concrete project-open factory.
    pub(crate) async fn register(
        &self,
        project_root: PathBuf,
        project_runtime: Pr12PrimitiveProjectRuntime,
    ) -> Result<Arc<dyn Pr12PrimitiveDispatch>, DaemonPrimitiveRuntimeRegistrationError> {
        let dispatch = project_runtime.dispatch();
        self.service
            .project_runtimes
            .register(project_root, project_runtime)
            .await
            .map_err(|_| DaemonPrimitiveRuntimeRegistrationError::AlreadyRegistered)?;
        Ok(dispatch)
    }

    #[allow(dead_code)] // in-flight route unregister — staged
    pub(crate) async fn unregister(&self, project_root: &Path) -> bool {
        let runtime = self
            .service
            .project_runtimes
            .withdraw::<Pr12PrimitiveProjectRuntime>(project_root)
            .await;
        runtime.is_some_and(|runtime| {
            runtime.teardown();
            true
        })
    }
}

#[derive(Clone)]
pub(crate) struct DaemonConfigurationRuntimeRegistrar {
    service: DaemonInvocationService,
}

pub(crate) enum DoctorConfigurationOutcomeV1 {
    Preview {
        preview_id: PreviewId,
        execution: OperationReceipt,
    },
    Effect {
        execution: OperationReceipt,
        receipt: EffectReceipt,
    },
    Denied,
    Unavailable,
}

impl DaemonConfigurationRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn doctor_owner_mounted(&self, project_root: &Path) -> bool {
        self.service
            .configuration_runtime(Some(project_root))
            .await
            .is_some()
    }

    pub(crate) async fn doctor_execute(
        &self,
        project_root: &Path,
        request_id: &RequestId,
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
        request: ConfigurationSurfaceRequest,
    ) -> DoctorConfigurationOutcomeV1 {
        let Some(registered) = self.service.configuration_runtime(Some(project_root)).await else {
            return DoctorConfigurationOutcomeV1::Unavailable;
        };
        let observed_at = current_micros();
        let deadline = match Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000))) {
            Ok(deadline) => deadline,
            Err(_) => return DoctorConfigurationOutcomeV1::Unavailable,
        };
        let cancellation = match CancellationContext::active(format!(
            "cancel.doctor-remediation.{}",
            request_id.as_str()
        )) {
            Ok(cancellation) => cancellation,
            Err(_) => return DoctorConfigurationOutcomeV1::Unavailable,
        };
        let response = execute_configuration(
            request_id.as_str().to_owned(),
            Some(registered),
            surface_operation,
            request,
            observed_at,
            deadline,
            cancellation,
        )
        .await;
        match response.outcome {
            DaemonInvocationOutcome::Configuration {
                outcome: ApplicationOutcome::Preview(preview),
                ..
            } => DoctorConfigurationOutcomeV1::Preview {
                preview_id: preview.preview_id,
                execution: preview.execution,
            },
            DaemonInvocationOutcome::Configuration {
                outcome: ApplicationOutcome::Effect(effect),
                ..
            } => DoctorConfigurationOutcomeV1::Effect {
                execution: effect.execution,
                receipt: effect.receipt,
            },
            DaemonInvocationOutcome::ApplicationProblem {
                problem: ApplicationProblem::NotFoundOrNotAuthorized { .. },
            } => DoctorConfigurationOutcomeV1::Denied,
            _ => DoctorConfigurationOutcomeV1::Unavailable,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn register(
        &self,
        project_root: PathBuf,
        runtime: Arc<ProjectConfigurationRuntime>,
        scope: ResolvedScope,
        profile_id: UserProfileId,
        actor: ActorId,
        expires_at: UtcMicros,
        membership_digest: Option<ManifestDigest>,
        policy_manifest_digest: ManifestDigest,
    ) -> Result<(), TraceDecayError> {
        if self
            .service
            .project_runtimes
            .holds::<RegisteredConfigurationRuntime>(&project_root)
            .await
        {
            return Ok(());
        }
        let policy_digest = AccessPolicyDigest::new(policy_manifest_digest.as_str().to_owned())
            .map_err(|error| TraceDecayError::Config {
                message: format!("configuration policy authority is invalid: {error}"),
            })?;
        let evidence = ScopeRevalidationEvidenceV1 {
            resolved_scope_digest: scope.scope_digest.clone(),
            membership_digest,
            authorization_policy_digest: policy_digest.clone(),
            policy_epoch: 1,
        };
        let current =
            runtime
                .client()
                .current()
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!("configuration layer authority unavailable: {error}"),
                })?;
        let direct_layers = mounted_configuration_layers(
            &runtime.configuration_target().project_id,
            &profile_id,
            &current.snapshot,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("configuration layer authority invalid: {error:?}"),
        })?;
        let grants = DaemonConfigurationGrantAuthority {
            actor: actor.clone(),
            policy_epoch: 1,
            policy_digest,
            expires_at,
            direct_layers: Arc::new(direct_layers),
            grants: Arc::new(RwLock::new(BTreeMap::new())),
        };
        let current = runtime.client().current().await.map_err(|error| {
            TraceDecayError::Config {
                message: format!(
                    "configuration runtime activation could not read the current revision: {error}"
                ),
            }
        })?;
        runtime
            .record_runtime_activation(Some(current.revision_id), None, current_micros())
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!("configuration runtime activation could not be recorded: {error}"),
            })?;
        runtime.install_authorities(
            Arc::new(DaemonConfigurationScopeResolution { actor, evidence }),
            Arc::new(PolicyBackedConfigurationMutationAuthorization::new(
                grants.clone(),
            )),
        )?;
        self.service
            .project_runtimes
            .publish(
                project_root,
                RegisteredConfigurationRuntime {
                    runtime,
                    scope,
                    actor: grants.actor.clone(),
                    grants,
                    semantic_operation: Arc::new(OnceLock::new()),
                },
            )
            .await;
        Ok(())
    }

    pub(crate) async fn install_semantic_operation(
        &self,
        project_root: &Path,
        operation: Arc<ProductionSemanticConfigurationOperationV1>,
    ) -> Result<(), TraceDecayError> {
        self.service
            .project_runtimes
            .read::<RegisteredConfigurationRuntime, _, _>(project_root, |registered| {
                registered
                    .semantic_operation
                    .set(operation)
                    .map_err(|_| TraceDecayError::Config {
                        message: "semantic configuration operation is already installed".to_owned(),
                    })
            })
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: "semantic configuration operation requires a registered Plan 20 runtime"
                    .to_owned(),
            })?
    }
}

#[derive(Clone)]
pub(crate) struct DaemonWorkRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonWorkRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn register(
        &self,
        project_root: PathBuf,
        database: Arc<crate::global_db::RegisteredGlobalDb>,
        authority: WorkAuthority,
        actor: ActorId,
        grant: CapabilityGrantSnapshot,
        policy_digest: ManifestDigest,
        configuration_digest: ManifestDigest,
        config: crate::sessions::codex_app_server::CodexAppServerSummaryConfig,
    ) -> Result<(), TraceDecayError> {
        if authority.project_id() != &grant.scope.project_id
            || authority.repository_id() != &grant.scope.repository_id
            || authority.worktree_id() != &grant.scope.worktree_id
            || authority.actor_id() != &actor
            || authority.policy_digest() != &grant.digest
        {
            return Err(TraceDecayError::Config {
                message: "Work runtime authority does not match its registered grant".to_owned(),
            });
        }
        let authority_digest =
            canonical_sha256(&authority).map_err(|error| TraceDecayError::Config {
                message: format!("Work runtime authority digest failed: {error}"),
            })?;
        self.service
            .project_runtimes
            .register_or_reconcile(
                project_root.clone(),
                |registered: &mut RegisteredWorkRuntime| {
                    if registered.actor == actor
                        && registered.grant.digest == grant.digest
                        && registered.grant.scope == grant.scope
                        && registered.authority_digest == authority_digest
                        && registered.policy_digest == policy_digest
                        && registered.configuration_digest == configuration_digest
                    {
                        // The same authority re-registering only renews its grant.
                        registered.grant = grant.clone();
                        return Ok(());
                    }
                    Err(TraceDecayError::Config {
                        message:
                            "a different Work authority is already registered for this project"
                                .to_owned(),
                    })
                },
                || {
                    // Opening the provider runtime is deferred until the slot is
                    // known to be free so a refused registration never starts one.
                    let runtime = database.work_runtime(authority, config, project_root.clone())?;
                    Ok(RegisteredWorkRuntime {
                        database,
                        runtime: Arc::new(runtime),
                        actor: actor.clone(),
                        grant: grant.clone(),
                        authority_digest: authority_digest.clone(),
                        policy_digest: policy_digest.clone(),
                        configuration_digest: configuration_digest.clone(),
                    })
                },
            )
            .await
    }

    pub(crate) async fn authority_matches(
        &self,
        project_root: &Path,
        authority: &WorkAuthority,
        actor: &ActorId,
        grant: &CapabilityGrantSnapshot,
        policy_digest: &ManifestDigest,
        configuration_digest: &ManifestDigest,
    ) -> bool {
        let Ok(authority_digest) = canonical_sha256(authority) else {
            return false;
        };
        self.service
            .project_runtimes
            .read::<RegisteredWorkRuntime, _, _>(project_root, |registered| {
                &registered.actor == actor
                    && registered.grant.digest == grant.digest
                    && registered.grant.scope == grant.scope
                    && registered.authority_digest == authority_digest
                    && &registered.policy_digest == policy_digest
                    && &registered.configuration_digest == configuration_digest
            })
            .await
            .unwrap_or(false)
    }
}

#[derive(Clone)]
pub(crate) struct DaemonLspOwnerRegistrar {
    service: DaemonInvocationService,
}

impl DaemonLspOwnerRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn register_lsp_owner(
        &self,
        project_root: PathBuf,
        owner: DaemonLspInvocationOwner,
    ) {
        self.service.install_lsp_owner(project_root, owner).await;
    }

    pub(crate) async fn register_factory(
        &self,
        project_root: PathBuf,
        factory: Arc<DaemonLspSessionFactory>,
    ) {
        self.register_lsp_owner(project_root, DaemonLspInvocationOwner::new(factory))
            .await;
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn build_and_register(
        &self,
        project_root: PathBuf,
        database: Database,
        code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
        runtime: tokio::runtime::Handle,
        diagnostic_broker: Arc<Mutex<DiagnosticBroker>>,
        languages: &[String],
        root_uri: String,
        timeouts: LspRefreshTimeouts,
        diagnostics_quiet_window: Duration,
        gateway_capabilities: GatewayCapabilities,
    ) -> Result<Arc<DaemonLspSessionFactory>, TraceDecayError> {
        let feedback_runtime = self
            .service
            .feedback_runtime(Some(&project_root))
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: "PR12 feedback runtime is not registered for the project".to_owned(),
            })?;
        let feedback_cycle_input = self
            .service
            .feedback_cycle_input(Some(&project_root))
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: "production feedback cycle input is not registered for the project"
                    .to_owned(),
            })?;
        let semantics = production_semantic_authorities(
            runtime.clone(),
            diagnostic_broker.clone(),
            database.clone(),
            languages,
            project_root.clone(),
            root_uri,
            timeouts,
        )
        .await?;
        let upstream_capabilities = UpstreamCapabilities {
            supports_diagnostics: semantics.analyzer_available,
            semantic: semantics.semantic_capabilities.clone(),
        };
        let factory = Arc::new(
            lsp_session_factory(
                runtime,
                feedback_runtime,
                database,
                code_index,
                move |_| Arc::clone(&feedback_cycle_input),
                semantics.semantics,
                diagnostic_broker,
                diagnostics_quiet_window,
                semantics.cancellation,
                gateway_capabilities,
                upstream_capabilities,
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!("could not construct LSP session factory: {error:?}"),
            })?,
        );
        self.register_factory(project_root, factory.clone()).await;
        Ok(factory)
    }
}

#[derive(Debug, Error)]
pub(crate) enum DaemonAdvisoryRuntimeRegistrationError {
    #[error("a PR13 advisory runtime is already mounted for this project")]
    AlreadyRegistered,
    #[error("the shared PR12 feedback readers must be registered before PR13")]
    MissingFeedbackRuntime,
    #[error("the PR13 Hook orchestration registry is unavailable")]
    HookOrchestrationUnavailable,
    #[error("the PR13 production authorities could not be opened")]
    Production(#[from] Pr13AdvisoryProductionOpenErrorV1),
    #[error(transparent)]
    Startup(#[from] Pr13AdvisoryDaemonStartupErrorV1),
}

#[derive(Clone)]
pub(crate) struct DaemonAdvisoryRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonAdvisoryRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn register<GR, GA, CS, CE, PE, PC>(
        &self,
        project_root: PathBuf,
        input: Pr13AdvisoryRuntimeOpenV1,
        providers: Pr13AdvisoryProviderAuthoritiesV1<GR, GA, CS, CE, PE, PC>,
        lsp_session_factory: Arc<DaemonLspSessionFactory>,
        hook_delivery_port: Arc<
            dyn HookFeedbackDeliveryPortV1<Pr13AdvisoryHookLookupNoticeV1> + Send + Sync,
        >,
    ) -> Result<
        Arc<Pr13AdvisoryDaemonStartupRegistrationV1<GR, GA, CS, CE, PE, PC>>,
        DaemonAdvisoryRuntimeRegistrationError,
    >
    where
        GR: GitHubCurrentBranchRemapper + Send + Sync + 'static,
        GA: GitHubCanonicalReviewAnchorAuthorityV1 + Clone + Send + Sync + 'static,
        CS: CiReadOnlyProviderArchiveV1 + Send + Sync + 'static,
        CE: CiExactEvidenceAuthorityV1<CS::Record> + Send + Sync + 'static,
        PE: CanonicalProximityEvidenceAuthorityV1 + Send + Sync + 'static,
        PC: ConfigurationControlStore + Clone + Send + Sync + 'static,
    {
        let project_id = input.resolved_scope.project_id.clone();
        let feedback_registered = self
            .service
            .project_runtimes
            .read::<RegisteredFeedbackRuntime, _, _>(&project_root, |runtime| {
                runtime.project_id == project_id
            })
            .await
            .unwrap_or(false);
        if !feedback_registered {
            return Err(DaemonAdvisoryRuntimeRegistrationError::HookOrchestrationUnavailable);
        }
        if self
            .service
            .project_runtimes
            .holds::<Arc<dyn Any + Send + Sync>>(&project_root)
            .await
        {
            return Err(DaemonAdvisoryRuntimeRegistrationError::AlreadyRegistered);
        }
        let registration = Arc::new(register_pr13_advisory_daemon_startup(
            input,
            providers,
            lsp_session_factory.clone(),
            hook_delivery_port,
        )?);
        let registered_root = project_root.clone();
        let published: Arc<dyn Any + Send + Sync> = registration.clone();
        self.service
            .project_runtimes
            .register(project_root, published)
            .await
            .map_err(|_| DaemonAdvisoryRuntimeRegistrationError::AlreadyRegistered)?;
        self.service
            .install_lsp_owner(
                registered_root,
                DaemonLspInvocationOwner::new(lsp_session_factory),
            )
            .await;
        Ok(registration)
    }

    pub(crate) async fn register_production(
        &self,
        project_root: PathBuf,
        input: Pr13AdvisoryRuntimeOpenV1,
        production: Pr13AdvisoryProductionOpenV1,
        lsp_session_factory: Arc<DaemonLspSessionFactory>,
    ) -> Result<
        Arc<Pr13AdvisoryProductionStartupRegistrationV1>,
        DaemonAdvisoryRuntimeRegistrationError,
    > {
        let authorities = open_pr13_advisory_production_authorities(production)?;
        let (providers, hook_delivery_port) = authorities.into_registrar_parts();
        self.register(
            project_root,
            input,
            providers,
            lsp_session_factory,
            hook_delivery_port,
        )
        .await
    }

    pub(crate) async fn register_hook_orchestrator(
        &self,
        project_root: PathBuf,
        project_id: [u8; 16],
        worktree_id: [u8; 16],
        runtime: Arc<dyn Pr13HookOrchestrationPortV1>,
    ) -> Result<(), DaemonAdvisoryRuntimeRegistrationError> {
        if project_id == [0; 16]
            || worktree_id == [0; 16]
            || !self
                .service
                .project_runtimes
                .holds::<Arc<dyn Any + Send + Sync>>(&project_root)
                .await
        {
            return Err(DaemonAdvisoryRuntimeRegistrationError::MissingFeedbackRuntime);
        }
        self.service
            .project_runtimes
            .register(project_root.clone(), Arc::clone(&runtime))
            .await
            .map_err(|_| DaemonAdvisoryRuntimeRegistrationError::AlreadyRegistered)?;
        let runtime_weak: Weak<dyn Pr13HookOrchestrationPortV1> = Arc::downgrade(&runtime);
        let registered = match pr13_hook_orchestration_registry().lock() {
            Ok(mut registry) => {
                registry.retain(|_, runtime| runtime.strong_count() > 0);
                let key = (project_id, worktree_id);
                if registry
                    .get(&key)
                    .and_then(Weak::upgrade)
                    .is_some_and(|existing| !Arc::ptr_eq(&existing, &runtime))
                {
                    false
                } else {
                    registry.insert(key, runtime_weak);
                    true
                }
            }
            Err(_) => false,
        };
        if registered {
            Ok(())
        } else {
            self.service
                .project_runtimes
                .withdraw::<Arc<dyn Pr13HookOrchestrationPortV1>>(&project_root)
                .await;
            Err(DaemonAdvisoryRuntimeRegistrationError::HookOrchestrationUnavailable)
        }
    }

    pub(crate) async fn register_cycle_invoker(
        &self,
        project_root: PathBuf,
        invoker: Arc<dyn Pr13AdvisoryCycleInvocationPortV1>,
    ) -> Result<(), DaemonAdvisoryRuntimeRegistrationError> {
        if !self
            .service
            .project_runtimes
            .holds::<Arc<dyn Any + Send + Sync>>(&project_root)
            .await
        {
            return Err(DaemonAdvisoryRuntimeRegistrationError::MissingFeedbackRuntime);
        }
        self.service
            .project_runtimes
            .register(project_root, invoker)
            .await
            .map_err(|_| DaemonAdvisoryRuntimeRegistrationError::AlreadyRegistered)
    }
}

/// Mounts one project's semantic-runtime scheduling handle as daemon-private
/// retained state. Semantic scheduling is never a wire operation: the daemon
/// consults the retained handle for status/coverage and to hand work to the
/// bounded background scheduler, and clients observe only the typed
/// freshness/coverage that ordinary operations already report.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DaemonSemanticRuntimeRegistrationError {
    #[error("a semantic runtime scheduler is already mounted for this project")]
    AlreadyRegistered,
}

pub(crate) struct DaemonSemanticRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonSemanticRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn register(
        &self,
        project_root: PathBuf,
        handle: crate::semantic_code::DaemonSemanticRuntimeHandleV1,
    ) -> Result<(), DaemonSemanticRuntimeRegistrationError> {
        self.service
            .project_runtimes
            .register_or_reconcile(
                project_root.clone(),
                |_: &mut crate::semantic_code::DaemonSemanticRuntimeHandleV1| {
                    Err(DaemonSemanticRuntimeRegistrationError::AlreadyRegistered)
                },
                || {
                    // The process-wide table is only joined once the project slot
                    // is known to be free, so a refused registration cannot
                    // replace a live handle there.
                    crate::application::semantic_runtime::register_project_semantic_runtime(
                        project_root.clone(),
                        handle.clone(),
                    );
                    Ok(handle)
                },
            )
            .await
    }
}

impl DaemonInvocationService {
    /// Returns the retained semantic scheduling handle for `project_root`,
    /// or the sole mounted handle when no root is given and exactly one
    /// project is registered.
    #[allow(dead_code)] // Plan 31 semantic runtime accessor — staged
    pub(crate) async fn semantic_runtime(
        &self,
        project_root: Option<&Path>,
    ) -> Option<crate::semantic_code::DaemonSemanticRuntimeHandleV1> {
        match project_root {
            Some(root) => self.project_runtimes.get(root).await,
            None => self.project_runtimes.sole().await,
        }
    }
}

struct RuntimeLspSession {
    expires_at_ms: u64,
    actor: RuntimeLspActor,
}

impl Drop for RuntimeLspSession {
    fn drop(&mut self) {
        // Every removal path (explicit detach, transport loss, TTL expiry, and
        // daemon shutdown) must cancel provider work and release overlays,
        // subscriptions, publications, and queued frames before the actor is
        // discarded.
        self.actor.expire();
    }
}

type RuntimeLspActor = DaemonLspRuntimeSession;

#[derive(Clone)]
pub(crate) struct DaemonLspInvocationOwner {
    factory: Arc<DaemonLspSessionFactory>,
}

impl DaemonLspInvocationOwner {
    pub(crate) fn new(factory: Arc<DaemonLspSessionFactory>) -> Self {
        Self { factory }
    }
}

/// Admission binds a session to the root independently resolved by the daemon
/// before this protocol is invoked. Client root hints are never consulted.
#[derive(Clone, Debug)]
struct AdmittedRootSessionAdmission {
    root: AdmittedRoot,
}

impl LspSessionAdmissionPort for AdmittedRootSessionAdmission {
    fn admit_lsp_session(
        &self,
        _request: &LspSessionOpenRequest,
        now_ms: u64,
    ) -> Result<AuthorizedLspSession, LspEndpointError> {
        let mut session_bytes = [0_u8; 16];
        let mut credential_bytes = [0_u8; 32];
        getrandom::getrandom(&mut session_bytes)
            .map_err(|_| LspEndpointError::AdmissionRejected)?;
        getrandom::getrandom(&mut credential_bytes)
            .map_err(|_| LspEndpointError::AdmissionRejected)?;
        let session_id = LspSessionId::new(format!("lsp-{}", hex::encode(session_bytes)))?;
        let credential = LspSessionCredential::new(credential_bytes.to_vec())?;
        Ok(AuthorizedLspSession {
            session_id,
            credential,
            workspace: AuthorizedLspWorkspace::single(self.root.clone()),
            expires_at_ms: now_ms.saturating_add(LSP_SESSION_TTL_MS),
        })
    }
}

#[derive(Clone)]
struct SharedGitTransactionPort {
    service: Arc<DaemonProjectGitIndexTransactionService>,
    cancellation: Option<OperationEmitter>,
}

impl GitIndexTransactionPort for SharedGitTransactionPort {
    fn preview(
        &self,
        request: &GitIndexPreviewRequestV1,
    ) -> Result<GitIndexPreviewPortResultV1, GitIndexTransactionPortError> {
        self.service.preview(request)
    }

    fn apply(
        &self,
        request: &GitIndexApplyRequestV1,
    ) -> Result<GitIndexApplyPortResultV1, GitIndexTransactionPortError> {
        self.cancellation.as_ref().map_or_else(
            || self.service.apply(request),
            |emitter| {
                self.service
                    .apply_cancellable(request, || emitter.cancellation_requested_at())
            },
        )
    }

    fn recover(
        &self,
        request: &GitIndexRecoveryRequestV1,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexTransactionPortError> {
        self.service.recover(request)
    }
}

#[allow(clippy::too_many_arguments)]
fn git_read_evidence_packet(
    request_id: &str,
    request: &crate::application::git_reads::GitReadRequestV1,
    current: &DaemonGitAuthorityStateV1,
    result: crate::application::git_reads::GitReadResultV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<EvidencePacket<serde_json::Value>, ApplicationProblem> {
    let capability_id =
        CapabilityId::new(request.capability_id()).map_err(|_| invalid_git_request())?;
    let use_case_id = UseCaseId::new(request.use_case_id()).map_err(|_| invalid_git_request())?;
    if !current.effective_capabilities.contains(&capability_id) {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    let grant_digest = stable_digest(&(
        &current.scope,
        &current.requester,
        &current.policy_digest,
        &current.configuration_digest,
        &current.catalog_digest,
        &current.privacy_digest,
        &capability_id,
        &use_case_id,
    ))?;
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!(
            "grant.daemon.git-read.{}",
            grant_digest.as_str().trim_start_matches("sha256:")
        ))
        .map_err(|_| invalid_git_request())?,
        current.policy_revision,
        grant_digest.clone(),
        current.requester.clone(),
        current.evaluated_at,
        current.grant_expires_at,
        current.scope.clone(),
        std::collections::BTreeSet::from([capability_id]),
        std::collections::BTreeSet::from([use_case_id]),
        DisclosureClass::Sensitive,
    )
    .map_err(|_| invalid_git_request())?;
    let context = RequestContext::new(
        current.requester.clone(),
        current.scope.clone(),
        grant,
        RequestId::new(request_id).map_err(|_| invalid_git_request())?,
        deadline.clone(),
        cancellation,
    )
    .map_err(|_| invalid_git_request())?;
    let authority = AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.daemon.git-read.v1",
            current.policy_revision,
            current.policy_digest.clone(),
            ComponentVersion::new("tracedecay.daemon.git-policy.v2")
                .map_err(|_| invalid_git_request())?,
        )
        .map_err(|_| invalid_git_request())?,
        current.evaluated_at,
    )
    .map_err(|_| invalid_git_request())?;
    let native_coverage = match &result {
        crate::application::git_reads::GitReadResultV1::Status(envelope) => &envelope.coverage,
        crate::application::git_reads::GitReadResultV1::Diff(envelope) => &envelope.coverage,
        crate::application::git_reads::GitReadResultV1::History(envelope) => &envelope.coverage,
        crate::application::git_reads::GitReadResultV1::Blame(envelope) => &envelope.coverage,
        crate::application::git_reads::GitReadResultV1::Hunks(envelope) => &envelope.coverage,
    };
    let coverage = if native_coverage.is_complete() {
        EvidenceCoverage::complete(vec![EvidenceDomain::Source], 1, 1, 1)
            .map_err(|_| invalid_git_request())?
    } else {
        let coverage = EvidenceCoverage {
            requested_domains: vec![EvidenceDomain::Source],
            visited: Some(1),
            eligible: Some(1),
            returned: 1,
            completeness: CoverageCompleteness::Partial,
            domains: vec![CoverageDomainState {
                domain: EvidenceDomain::Source,
                completeness: CoverageCompleteness::Partial,
            }],
        };
        coverage.validate().map_err(|_| invalid_git_request())?;
        coverage
    };
    let mut omission_counts = BTreeMap::<OmissionReason, u64>::new();
    for degradation in &native_coverage.degradations {
        use tracedecay_domain::git::GitDegradationV1;
        let reason = match degradation {
            GitDegradationV1::TruncatedOutput => OmissionReason::Budget,
            GitDegradationV1::ConflictedState | GitDegradationV1::InProgressOperation => {
                OmissionReason::Conflict
            }
            GitDegradationV1::UnreadableState => OmissionReason::Failed,
            GitDegradationV1::IgnoredCollision
            | GitDegradationV1::DetachedHead
            | GitDegradationV1::UnbornBranch
            | GitDegradationV1::SparseCheckout
            | GitDegradationV1::SplitIndex
            | GitDegradationV1::SubmoduleState
            | GitDegradationV1::UnsupportedObjectFormat
            | GitDegradationV1::ShallowBoundary => OmissionReason::Unsupported,
        };
        omission_counts
            .entry(reason)
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
    }
    let omissions = omission_counts
        .into_iter()
        .map(|(reason, count)| Omission {
            domain: EvidenceDomain::Source,
            count,
            reason,
        })
        .collect();
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )
    .map_err(|_| invalid_git_request())?;
    let payload = serde_json::to_value(result).map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "git_read.result_encoding_failed".to_owned(),
            message: "The Git read result could not be encoded".to_owned(),
        })
    })?;
    let evidence_digest = stable_digest(&(
        "tracedecay.native-git-read-evidence.v1",
        request,
        &current.scope,
        &current.configuration_digest,
        &current.catalog_digest,
        &payload,
    ))?;
    Ok(EvidencePacket {
        temporal: TemporalState::current(execution.ended_at),
        authority,
        evidence_authorities: vec![EvidenceAuthority {
            evidence_id: EvidenceIdentity::new(format!(
                "evidence.git-read.{}",
                evidence_digest.as_str().trim_start_matches("sha256:")
            ))
            .map_err(|_| invalid_git_request())?,
            source_kind: "native_git".to_owned(),
            producer: "git_query".to_owned(),
            scope: current.scope.clone(),
            revision: ComponentVersion::new("tracedecay.git-read.v1")
                .map_err(|_| invalid_git_request())?,
            horizon: Some(current.evaluated_at),
        }],
        coverage,
        omissions,
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new("sort.git-read.stable.v1").map_err(|_| invalid_git_request())?,
            1,
            Some(1),
            1,
        )
        .map_err(|_| invalid_git_request())?,
        execution,
        payload: Some(payload),
    })
}

#[allow(clippy::too_many_arguments)]
async fn execute_git_read(
    wire_request_id: String,
    project_root: Option<&Path>,
    owner: Option<DaemonGitInvocationOwner>,
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    request: GitReadSurfaceRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(owner) = owner else {
        return concealed_application_problem(wire_request_id);
    };
    let Some(project_root) = project_root.map(Path::to_path_buf) else {
        return concealed_application_problem(wire_request_id);
    };
    let expected_operation = match &request.request {
        crate::application::git_reads::GitReadRequestV1::Status => {
            crate::application_surface::ApplicationSurfaceOperation::GitStatus
        }
        crate::application::git_reads::GitReadRequestV1::Diff { .. } => {
            crate::application_surface::ApplicationSurfaceOperation::GitDiff
        }
        crate::application::git_reads::GitReadRequestV1::History { .. } => {
            crate::application_surface::ApplicationSurfaceOperation::GitHistory
        }
        crate::application::git_reads::GitReadRequestV1::Blame { .. } => {
            crate::application_surface::ApplicationSurfaceOperation::GitBlame
        }
        crate::application::git_reads::GitReadRequestV1::Hunks { .. } => {
            crate::application_surface::ApplicationSurfaceOperation::GitHunks
        }
    };
    if surface_operation != expected_operation {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::InvalidRequest,
        );
    }
    if cancellation.is_cancelled() {
        return application_problem(
            wire_request_id,
            ApplicationProblem::cancelled_before_admission(),
        );
    }
    if deadline.is_elapsed_at(observed_at) || deadline.is_elapsed_at(current_micros()) {
        return application_problem(
            wire_request_id,
            ApplicationProblem::timed_out_before_admission(),
        );
    }
    let initial = match owner.current_read_authority(&request.request) {
        Ok(authority) => authority,
        Err(_) => return concealed_application_problem(wire_request_id),
    };
    let remaining_micros = deadline
        .expires_at
        .0
        .saturating_sub(current_micros().0)
        .max(0) as u64;
    let bounds = crate::git_query::GitQueryBounds {
        max_entries: request.max_entries,
        max_bytes: request.max_bytes,
        deadline: Some(std::time::Instant::now() + Duration::from_micros(remaining_micros)),
        cancel: Some(Arc::new(AtomicBool::new(false))),
    };
    let selected_scope = initial.scope.clone();
    let read_request = request.request.clone();
    let authority = crate::application::git_reads::GitReadAuthorityV1::new(
        project_root,
        selected_scope.clone(),
    );
    let outcome = tokio::task::spawn_blocking(move || {
        crate::application::git_reads::execute_git_read(
            Some(&authority),
            &selected_scope,
            &read_request,
            &bounds,
        )
    })
    .await
    .unwrap_or(
        crate::application::git_reads::GitReadOutcomeV1::Unavailable {
            reason: crate::application::git_reads::GitReadUnavailableReasonV1::ReadFailed,
        },
    );
    let terminal = match owner.current_read_authority(&request.request) {
        Ok(authority) => authority,
        Err(_) => return concealed_application_problem(wire_request_id),
    };
    if initial.scope != terminal.scope
        || initial.requester != terminal.requester
        || initial.effective_capabilities != terminal.effective_capabilities
        || initial.grant_expires_at != terminal.grant_expires_at
        || initial.policy_revision != terminal.policy_revision
        || initial.policy_digest != terminal.policy_digest
        || initial.configuration_digest != terminal.configuration_digest
        || initial.catalog_digest != terminal.catalog_digest
        || initial.privacy_digest != terminal.privacy_digest
        || current_micros() >= terminal.grant_expires_at
    {
        return concealed_application_problem(wire_request_id);
    }
    match outcome {
        crate::application::git_reads::GitReadOutcomeV1::Complete { scope, result }
            if scope == terminal.scope =>
        {
            let packet = match git_read_evidence_packet(
                &wire_request_id,
                &request.request,
                &terminal,
                result,
                observed_at,
                deadline,
                cancellation,
            ) {
                Ok(packet) => packet,
                Err(problem) => return application_problem(wire_request_id, problem),
            };
            DaemonInvocationResponse::with_outcome(
                wire_request_id,
                DaemonInvocationOutcome::GitRead {
                    scope,
                    result: DaemonFeedbackResult::from_application(packet),
                },
            )
        }
        crate::application::git_reads::GitReadOutcomeV1::Unavailable {
            reason: crate::application::git_reads::GitReadUnavailableReasonV1::Cancelled,
        } => application_problem(
            wire_request_id,
            ApplicationProblem::cancelled_before_admission(),
        ),
        crate::application::git_reads::GitReadOutcomeV1::Unavailable {
            reason: crate::application::git_reads::GitReadUnavailableReasonV1::TimedOut,
        } => application_problem(
            wire_request_id,
            ApplicationProblem::timed_out_before_admission(),
        ),
        crate::application::git_reads::GitReadOutcomeV1::Complete { .. }
        | crate::application::git_reads::GitReadOutcomeV1::Unavailable { .. } => {
            DaemonInvocationResponse::problem(wire_request_id, DaemonInvocationProblem::Unavailable)
        }
    }
}

async fn execute_git_preview(
    operation_events: &OperationEventAuthority,
    wire_request_id: String,
    owner: Option<DaemonGitInvocationOwner>,
    request: GitPreviewSurfaceRequest,
    _observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(owner) = owner else {
        return concealed_application_problem(wire_request_id);
    };
    if request.repository_snapshot.project_id != owner.project_id {
        return concealed_application_problem(wire_request_id);
    }
    let service = Arc::clone(&owner.service);
    let operation = request.operation;
    let authority =
        match tokio::task::spawn_blocking(move || owner.current_authority(operation)).await {
            Ok(Ok(authority)) => authority,
            Ok(Err(error)) => {
                return application_problem(wire_request_id, map_git_port_problem(error));
            }
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    wire_request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            }
        };
    let request = match build_git_preview_request(
        &wire_request_id,
        request,
        &authority,
        deadline,
        cancellation,
    ) {
        Ok(request) => request,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let scope = request.context.scope().clone();
    let Ok(emitter) = operation_events
        .begin(
            &request.context,
            OperationKind::GitPreview,
            request.observed_at,
        )
        .await
    else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::Unavailable,
        );
    };
    let _ = emitter.progress(0, Some(1)).await;
    let started_at = request.observed_at;
    let effective_deadline = request.context.deadline().clone();
    let result = tokio::task::spawn_blocking(move || {
        GitIndexTransactionService::new(SharedGitTransactionPort {
            service,
            cancellation: None,
        })
        .preview(request)
    })
    .await;
    let response = match result {
        Ok(Ok(preview)) => match DaemonGitPreviewResult::from_application(preview) {
            Ok(preview) => DaemonInvocationResponse::with_outcome(
                wire_request_id,
                DaemonInvocationOutcome::GitPreview { scope, preview },
            ),
            Err(_) => DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::Unavailable,
            ),
        },
        Ok(Err(error)) => application_problem(wire_request_id, map_git_error(error)),
        Err(_) => {
            DaemonInvocationResponse::problem(wire_request_id, DaemonInvocationProblem::Unavailable)
        }
    };
    publish_invocation_terminal(&emitter, &response, started_at, effective_deadline).await;
    response
}

async fn execute_git_apply(
    operation_events: &OperationEventAuthority,
    wire_request_id: String,
    owner: Option<DaemonGitInvocationOwner>,
    request: GitApplySurfaceRequest,
    _observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(owner) = owner else {
        return concealed_application_problem(wire_request_id);
    };
    if request.preview.repository_snapshot.project_id != owner.project_id {
        return concealed_application_problem(wire_request_id);
    }
    let service = Arc::clone(&owner.service);
    let operation = request.preview.operation;
    let authority =
        match tokio::task::spawn_blocking(move || owner.current_authority(operation)).await {
            Ok(Ok(authority)) => authority,
            Ok(Err(error)) => {
                return application_problem(wire_request_id, map_git_port_problem(error));
            }
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    wire_request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            }
        };
    let request = match build_git_apply_request(
        &wire_request_id,
        request,
        &authority,
        deadline,
        cancellation,
    ) {
        Ok(request) => request,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let scope = request.context.scope().clone();
    let Ok(emitter) = operation_events
        .begin(
            &request.context,
            OperationKind::GitApply,
            request.observed_at,
        )
        .await
    else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::Unavailable,
        );
    };
    let _ = emitter.progress(0, Some(1)).await;
    let started_at = request.observed_at;
    let effective_deadline = request.context.deadline().clone();
    let cancellation = emitter.clone();
    let result = tokio::task::spawn_blocking(move || {
        GitIndexTransactionService::new(SharedGitTransactionPort {
            service,
            cancellation: Some(cancellation),
        })
        .apply(request)
    })
    .await;
    let response = match result {
        Ok(Ok(effect)) => match DaemonGitEffectResult::from_application(effect) {
            Ok(effect) => DaemonInvocationResponse::with_outcome(
                wire_request_id,
                DaemonInvocationOutcome::GitApply { scope, effect },
            ),
            Err(_) => DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::Unavailable,
            ),
        },
        Ok(Err(error)) => application_problem(wire_request_id, map_git_error(error)),
        Err(_) => {
            DaemonInvocationResponse::problem(wire_request_id, DaemonInvocationProblem::Unavailable)
        }
    };
    publish_invocation_terminal(&emitter, &response, started_at, effective_deadline).await;
    response
}

async fn publish_invocation_terminal(
    emitter: &OperationEmitter,
    response: &DaemonInvocationResponse,
    started_at: UtcMicros,
    effective_deadline: Deadline,
) {
    let ended_at = current_micros();
    let ended_at = if ended_at < started_at {
        started_at
    } else {
        ended_at
    };
    let receipt = invocation_operation_receipt(response).unwrap_or_else(|| OperationReceipt {
        started_at,
        ended_at,
        effective_deadline,
        cancellation: None,
        budget: OperationBudgetUsage::default(),
        termination: OperationTermination::Failed,
    });
    if receipt.termination == OperationTermination::Completed {
        let _ = emitter.progress(1, Some(1)).await;
    }
    let _ = emitter.terminal(receipt).await;
}

fn invocation_operation_receipt(response: &DaemonInvocationResponse) -> Option<OperationReceipt> {
    match &response.outcome {
        DaemonInvocationOutcome::GitRead { result, .. } => Some(result.execution.clone()),
        DaemonInvocationOutcome::GitPreview { preview, .. } => Some(preview.execution.clone()),
        DaemonInvocationOutcome::GitApply { effect, .. } => Some(effect.execution.clone()),
        DaemonInvocationOutcome::Feedback { result, .. } => Some(result.execution.clone()),
        _ => None,
    }
}

fn build_git_preview_request(
    request_id: &str,
    request: GitPreviewSurfaceRequest,
    current: &DaemonGitAuthorityStateV1,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<GitIndexPreviewRequestV1, ApplicationProblem> {
    let observed_at = current.evaluated_at;
    let preview_id = mint_git_preview_id()?;
    let mut selected_hunks = request.selected_hunks;
    for hunk in &mut selected_hunks {
        preview_id.as_str().clone_into(&mut hunk.preview_id);
    }
    let (context, authority, binding) = git_request_authority(
        request_id,
        &request.repository_snapshot,
        request.operation,
        current,
        deadline,
        cancellation,
        observed_at,
    )?;
    Ok(GitIndexPreviewRequestV1 {
        context,
        authority,
        binding,
        preview_id,
        repository_snapshot: request.repository_snapshot,
        selected_hunks,
        commit_intent: request.commit_intent,
        observed_at,
    })
}

fn mint_git_preview_id() -> Result<GitIndexPreviewId, ApplicationProblem> {
    let identity =
        mint_global_opaque_id(GlobalOpaqueIdentityKind::GitIndexPreview).map_err(|_| {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "git_index.preview_identity_unavailable".to_owned(),
                message: "The daemon could not mint a Git preview identity".to_owned(),
            })
        })?;
    GitIndexPreviewId::new(identity).map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "git_index.preview_identity_unavailable".to_owned(),
            message: "The daemon could not mint a Git preview identity".to_owned(),
        })
    })
}

fn build_git_apply_request(
    request_id: &str,
    request: GitApplySurfaceRequest,
    current: &DaemonGitAuthorityStateV1,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<GitIndexApplyRequestV1, ApplicationProblem> {
    let observed_at = current.evaluated_at;
    let (context, authority, binding) = git_request_authority(
        request_id,
        &request.preview.repository_snapshot,
        request.preview.operation,
        current,
        deadline,
        cancellation,
        observed_at,
    )?;
    Ok(GitIndexApplyRequestV1 {
        context,
        authority: authority.clone(),
        binding,
        preview_id: request.preview.preview_id,
        preview_digest: request.preview.preview_digest,
        idempotency_key: request.idempotency_key,
        proof: GitIndexEffectProofV1 {
            policy_digest: authority.policy.digest,
            configuration_digest: current.configuration_digest.clone(),
            catalog_digest: current.catalog_digest.clone(),
            privacy_digest: current.privacy_digest.clone(),
            external_proof: None,
        },
        observed_at,
    })
}

fn git_request_authority(
    request_id: &str,
    snapshot: &tracedecay_domain::RepositoryStateSnapshotV1,
    operation: GitIndexTransactionOperationV1,
    current: &DaemonGitAuthorityStateV1,
    deadline: Deadline,
    cancellation: CancellationContext,
    observed_at: UtcMicros,
) -> Result<(RequestContext, AuthorityReceipt, GitIndexOperationBindingV1), ApplicationProblem> {
    if cancellation.is_cancelled() {
        return Err(ApplicationProblem::cancelled_before_admission());
    }
    if deadline.is_elapsed_at(now_micros()) || deadline.is_elapsed_at(observed_at) {
        return Err(ApplicationProblem::timed_out_before_admission());
    }
    snapshot.validate().map_err(|_| invalid_git_request())?;
    if observed_at >= current.grant_expires_at
        || current.evaluated_at >= current.grant_expires_at
        || snapshot.project_id != current.scope.project_id
        || snapshot.repository_id != current.scope.repository_id
        || snapshot.worktree_id.as_ref() != Some(&current.scope.worktree_id)
        || !match (&current.scope.reference, &snapshot.head) {
            (
                Some(reference),
                GitHeadStateV1::Attached { branch, .. } | GitHeadStateV1::Unborn { branch },
            ) => reference.as_str() == branch,
            (None, GitHeadStateV1::Detached { .. }) => true,
            (None, _) | (Some(_), GitHeadStateV1::Detached { .. }) => false,
        }
    {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    let binding =
        GitIndexOperationBindingV1::for_operation(operation).map_err(|_| invalid_git_request())?;
    let capability_id = binding.capability_id.clone();
    let use_case_id = binding.use_case_id.clone();
    if !current.effective_capabilities.contains(&capability_id) {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    let grant_digest = stable_digest(&(
        &current.scope,
        &current.requester,
        &current.policy_digest,
        &current.configuration_digest,
        &current.catalog_digest,
        &current.privacy_digest,
        &capability_id,
        &use_case_id,
    ))?;
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!(
            "grant.daemon.git.{}",
            grant_digest.as_str().trim_start_matches("sha256:")
        ))
        .map_err(|_| invalid_git_request())?,
        current.policy_revision,
        grant_digest,
        current.requester.clone(),
        observed_at,
        current.grant_expires_at,
        current.scope.clone(),
        std::collections::BTreeSet::from([capability_id.clone()]),
        std::collections::BTreeSet::from([use_case_id.clone()]),
        DisclosureClass::Sensitive,
    )
    .map_err(|_| invalid_git_request())?;
    let context = RequestContext::new(
        current.requester.clone(),
        current.scope.clone(),
        grant,
        RequestId::new(request_id).map_err(|_| invalid_git_request())?,
        deadline,
        cancellation,
    )
    .map_err(|_| invalid_git_request())?;
    let authority = AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.daemon.git-index.v2",
            current.policy_revision,
            current.policy_digest.clone(),
            ComponentVersion::new("tracedecay.daemon.git-policy.v2")
                .map_err(|_| invalid_git_request())?,
        )
        .map_err(|_| invalid_git_request())?,
        current.evaluated_at,
    )
    .map_err(|_| invalid_git_request())?;
    Ok((context, authority, binding))
}

fn stable_digest(material: &impl Serialize) -> Result<ManifestDigest, ApplicationProblem> {
    canonical_sha256(material).map_err(|_| invalid_git_request())
}

fn now_micros() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_micros()),
        )
        .unwrap_or(i64::MAX),
    )
}

fn invalid_git_request() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "git_index.invalid_request".to_owned(),
            message: "The Git index request is invalid".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: Vec::new(),
    }
}

fn map_git_error(error: GitIndexTransactionApplicationError) -> ApplicationProblem {
    match error {
        GitIndexTransactionApplicationError::Contract(_) => invalid_git_request(),
        GitIndexTransactionApplicationError::Port(error) => map_git_port_problem(error),
    }
}

fn map_git_port_problem(error: GitIndexTransactionPortError) -> ApplicationProblem {
    match error {
        GitIndexTransactionPortError::StalePreview => ApplicationProblem::stale(SafeDiagnostic {
            code: "git_index.stale_preview".to_owned(),
            message: "The Git index preview is stale or absent".to_owned(),
        }),
        GitIndexTransactionPortError::PolicyDenied => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        GitIndexTransactionPortError::IdempotencyConflict => ApplicationProblem::Conflict {
            diagnostic: SafeDiagnostic {
                code: "git_index.idempotency_conflict".to_owned(),
                message: "The idempotency key is already bound to another input".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        },
        GitIndexTransactionPortError::Unsupported => ApplicationProblem::Unsupported {
            diagnostic: SafeDiagnostic {
                code: "git_index.unsupported".to_owned(),
                message: "The repository state does not support this Git index operation"
                    .to_owned(),
            },
            retry: RetryDirective::AfterRevalidate,
            legal_actions: Vec::new(),
        },
        GitIndexTransactionPortError::DaemonUnavailable
        | GitIndexTransactionPortError::RecoveryRequired
        | GitIndexTransactionPortError::NeedsInspection
        | GitIndexTransactionPortError::NativeFailure => {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: match error {
                    GitIndexTransactionPortError::RecoveryRequired => "git_index.recovery_required",
                    GitIndexTransactionPortError::NeedsInspection => "git_index.needs_inspection",
                    GitIndexTransactionPortError::NativeFailure => "git_index.native_failure",
                    _ => "git_index.unavailable",
                }
                .to_owned(),
                message: "The Git index transaction owner is not ready".to_owned(),
            })
        }
    }
}

fn application_problem(
    request_id: String,
    problem: ApplicationProblem,
) -> DaemonInvocationResponse {
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::ApplicationProblem { problem },
    )
}

fn concealed_application_problem(request_id: String) -> DaemonInvocationResponse {
    application_problem(
        request_id,
        ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
    )
}

fn execute_work_application(
    registered: RegisteredWorkRuntime,
    request_id: String,
    request: WorkApplicationInvocationV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let operation_key = request.operation_key();
    let Some((_, capability, use_case)) = tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1
        .iter()
        .find(|(operation, _, _)| *operation == operation_key)
    else {
        return DaemonInvocationResponse::problem(
            request_id,
            DaemonInvocationProblem::InvalidRequest,
        );
    };
    let (context, canonical_request_id, use_case) = match work_request_context(
        &registered,
        &request_id,
        capability,
        use_case,
        observed_at,
        deadline.clone(),
        cancellation,
    ) {
        Ok(context) => context,
        Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
    };
    let input_digest = match canonical_sha256(&request) {
        Ok(digest) => digest,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        }
    };
    let services = match registered.database.work_application_services() {
        Ok(services) => services,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    match request {
        WorkApplicationInvocationV1::Snapshot(request) => complete_work_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services
                .projections()
                .snapshot(&context, request.page_size)
                .map_err(work_projection_problem),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::Snapshot,
        ),
        WorkApplicationInvocationV1::Delta(request) => complete_work_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services
                .projections()
                .delta(&context, &request.cursor, request.page_size)
                .map_err(work_projection_problem),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::Delta,
        ),
        WorkApplicationInvocationV1::Create(command) => complete_work_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.commands().create(&context, command),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::Create,
        ),
        WorkApplicationInvocationV1::ReplanDependencies(command) => complete_work_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.commands().replan_dependencies(&context, command),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::ReplanDependencies,
        ),
        WorkApplicationInvocationV1::ReviewProposal(request) => complete_work_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.commands().review_proposal(&context, request),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::ReviewProposal,
        ),
        WorkApplicationInvocationV1::AcceptProposal(command) => complete_work_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.commands().accept_proposal(&context, command),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::AcceptProposal,
        ),
        WorkApplicationInvocationV1::AdmitExecution(command) => complete_work_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.commands().admit_execution(&context, command),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::AdmitExecution,
        ),
        WorkApplicationInvocationV1::AttachRuntimeEvidence(command) => complete_work_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services
                .commands()
                .attach_runtime_evidence(&context, command),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::AttachRuntimeEvidence,
        ),
        WorkApplicationInvocationV1::AcceptTask(command) => complete_work_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.commands().accept_task(&context, command),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::AcceptTask,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn work_request_context(
    registered: &RegisteredWorkRuntime,
    request_id: &str,
    capability: &str,
    use_case: &str,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<(RequestContext, RequestId, UseCaseId), DaemonInvocationProblem> {
    let capability =
        CapabilityId::new(capability).map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let use_case = UseCaseId::new(use_case).map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let canonical_request_id =
        RequestId::new(request_id).map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
    let context = RequestContext::new(
        registered.actor.clone(),
        registered.grant.scope.clone(),
        registered.grant.clone(),
        canonical_request_id.clone(),
        deadline,
        cancellation,
    )
    .map_err(|_| DaemonInvocationProblem::NotFoundOrNotAuthorized)?;
    if context.admission_at(observed_at) != RequestAdmission::Admitted
        || !context.allows(&capability, &use_case)
    {
        return Err(DaemonInvocationProblem::NotFoundOrNotAuthorized);
    }
    Ok((context, canonical_request_id, use_case))
}

fn work_projection_problem(error: WorkProjectionApplicationError) -> ApplicationProblem {
    match error {
        WorkProjectionApplicationError::Admission(problem) => problem,
        WorkProjectionApplicationError::InvalidPageSize => ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "work.invalid_page_size".to_owned(),
                message: "The Work projection page size is invalid".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![tracedecay_application::LegalAction::CorrectRequest],
        },
        WorkProjectionApplicationError::Port(
            tracedecay_application::WorkProjectionPortError::StaleCursor,
        ) => ApplicationProblem::stale(SafeDiagnostic {
            code: "work.stale_cursor".to_owned(),
            message: "The Work projection cursor is stale".to_owned(),
        }),
        WorkProjectionApplicationError::Port(
            tracedecay_application::WorkProjectionPortError::Unavailable,
        ) => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "work.projection_unavailable".to_owned(),
            message: "The Work projection authority is unavailable".to_owned(),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn complete_work_read<T>(
    registered: &RegisteredWorkRuntime,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: Result<T, ApplicationProblem>,
    observed_at: UtcMicros,
    deadline: Deadline,
    wrap: fn(ApplicationOutcome<T>) -> WorkApplicationOutcomeV1,
) -> DaemonInvocationResponse
where
    T: Serialize,
{
    let result = match result {
        Ok(result) => result,
        Err(problem) => return application_problem(request_id, problem),
    };
    let outcome = match work_evidence_packet(
        registered,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        result,
        observed_at,
        deadline,
    ) {
        Ok(evidence) => wrap(ApplicationOutcome::Evidence(evidence)),
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::WorkApplication {
            scope: context.scope().clone(),
            outcome,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn complete_work_effect<T>(
    registered: &RegisteredWorkRuntime,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: Result<T, ApplicationProblem>,
    observed_at: UtcMicros,
    deadline: Deadline,
    wrap: fn(ApplicationOutcome<T>) -> WorkApplicationOutcomeV1,
) -> DaemonInvocationResponse
where
    T: Serialize,
{
    let result = match result {
        Ok(result) => result,
        Err(problem) => return application_problem(request_id, problem),
    };
    let outcome = match work_effect(
        registered,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        result,
        observed_at,
        deadline,
    ) {
        Ok(effect) => wrap(effect),
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::WorkApplication {
            scope: context.scope().clone(),
            outcome,
        },
    )
}

fn work_execution_problem(error: &WorkExecutionError) -> DaemonInvocationProblem {
    match error {
        WorkExecutionError::NotFound
        | WorkExecutionError::AlreadyExists
        | WorkExecutionError::StaleLease
        | WorkExecutionError::TerminalConflict => DaemonInvocationProblem::NotFoundOrNotAuthorized,
        WorkExecutionError::Contract(_) => DaemonInvocationProblem::InvalidRequest,
        WorkExecutionError::Persistence(_) => DaemonInvocationProblem::Unavailable,
        WorkExecutionError::Provider(
            tracedecay_application::WorkProviderExecutionError::Unavailable(_),
        ) => DaemonInvocationProblem::Unavailable,
        WorkExecutionError::Provider(
            tracedecay_application::WorkProviderExecutionError::Rejected(_),
        ) => DaemonInvocationProblem::InvalidRequest,
    }
}

async fn execute_work_attempt(
    registered: RegisteredWorkRuntime,
    request_id: String,
    request: WorkAttemptInvocationV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let operation_key = request.operation_key();
    let Some((_, capability, use_case)) = tracedecay_application::WORK_ATTEMPT_OPERATION_IDS_V1
        .iter()
        .find(|(operation, _, _)| *operation == operation_key)
    else {
        return DaemonInvocationResponse::problem(
            request_id,
            DaemonInvocationProblem::InvalidRequest,
        );
    };
    let (context, canonical_request_id, use_case) = match work_request_context(
        &registered,
        &request_id,
        capability,
        use_case,
        observed_at,
        deadline.clone(),
        cancellation,
    ) {
        Ok(context) => context,
        Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
    };
    let input_digest = match canonical_sha256(&request) {
        Ok(digest) => digest,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        }
    };
    let result = match registered.runtime.dispatch(request).await {
        Ok(result) => result,
        Err(error) => {
            return DaemonInvocationResponse::problem(request_id, work_execution_problem(&error));
        }
    };
    let outcome = work_effect(
        &registered,
        &context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        result,
        observed_at,
        deadline,
    );
    match outcome {
        Ok(outcome) => DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::WorkAttempt {
                scope: context.scope().clone(),
                outcome,
            },
        ),
        Err(_) => {
            DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn work_evidence_packet<T>(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    _request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: T,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<EvidencePacket<T>, ApplicationContractError>
where
    T: Serialize,
{
    let policy_digest = canonical_sha256(&(
        "tracedecay.daemon.work-read-policy.v1",
        &registered.policy_digest,
        &registered.grant.digest,
        operation_key,
        &use_case,
    ))?;
    let policy = PolicyDecisionRef::new(
        format!("policy.daemon.work.{operation_key}.v1"),
        1,
        policy_digest,
        ComponentVersion::new("tracedecay.daemon.work-policy.v1").map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "Work read policy evaluator",
            }
        })?,
    )?;
    let authority = AuthorityReceipt::from_context(context, policy, observed_at)?;
    let suffix = input_digest.as_str().strip_prefix("sha256:").ok_or(
        ApplicationContractError::Inconsistent {
            field: "Work read input digest",
        },
    )?;
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )?;
    Ok(EvidencePacket {
        temporal: TemporalState::current(execution.ended_at),
        authority,
        evidence_authorities: vec![EvidenceAuthority {
            evidence_id: EvidenceIdentity::new(format!("evidence.work.{operation_key}.{suffix}"))?,
            source_kind: "work_projection".to_owned(),
            producer: operation_key.to_owned(),
            scope: context.scope().clone(),
            revision: ComponentVersion::new("tracedecay.work-projection.v1")?,
            horizon: Some(execution.ended_at),
        }],
        coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Operational], 1, 1, 1)?,
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new(format!("sort.work.{operation_key}.v1"))?,
            1,
            Some(1),
            1,
        )?,
        execution,
        payload: Some(result),
    })
}

#[allow(clippy::too_many_arguments)]
fn work_effect<T>(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: T,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<ApplicationOutcome<T>, ApplicationContractError>
where
    T: Serialize,
{
    let policy_digest = canonical_sha256(&(
        "tracedecay.daemon.work-policy.v1",
        &registered.policy_digest,
        &registered.grant.digest,
        operation_key,
        &use_case,
    ))?;
    let policy = PolicyDecisionRef::new(
        format!("policy.daemon.work.{operation_key}.v1"),
        1,
        policy_digest,
        ComponentVersion::new("tracedecay.daemon.work-policy.v1").map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "Work policy evaluator",
            }
        })?,
    )?;
    let authority = AuthorityReceipt::from_context(context, policy, observed_at)?;
    let suffix = input_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(ApplicationContractError::Inconsistent {
            field: "Work input digest",
        })?
        .to_owned();
    let idempotency_key = IdempotencyKey::new(format!("work.{operation_key}.{suffix}"))?;
    let expected_state = canonical_sha256(&(
        "tracedecay.work.expected-state.v1",
        operation_key,
        &input_digest,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "Work expected state",
    })?;
    let committed_state =
        canonical_sha256(&("tracedecay.work.committed-state.v1", operation_key, &result)).map_err(
            |_| ApplicationContractError::Inconsistent {
                field: "Work committed state",
            },
        )?;
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )?;
    let receipt = EffectReceipt {
        operation: use_case,
        request_id,
        actor: registered.actor.clone(),
        scope: context.scope().clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: idempotency_key.clone(),
        input_digest,
        expected_state: expected_state.clone(),
        policy_digest: authority.policy.digest.clone(),
        configuration_digest: registered.configuration_digest.clone(),
        catalog_digest: canonical_sha256(&("tracedecay.work.catalog.v1", operation_key)).map_err(
            |_| ApplicationContractError::Inconsistent {
                field: "Work catalog digest",
            },
        )?,
        privacy_digest: canonical_sha256(&(
            "tracedecay.work.privacy.v1",
            context.scope(),
            context.grant().disclosure,
        ))
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "Work privacy digest",
        })?,
        outcome: EffectTermination::Completed,
        committed_state: Some(committed_state),
        external_proof: None,
    };
    Ok(ApplicationOutcome::Effect(EffectResult::new(
        EffectId::new(format!("effect.work.{operation_key}.{suffix}"))?,
        EffectClass::Administrative,
        idempotency_key,
        authority,
        expected_state,
        execution,
        ReconciliationState::Reconciled,
        receipt,
        Some(result),
    )?))
}

#[allow(dead_code)] // PR12 primitive + Plan 37 feedback publication — staged
impl DaemonInvocationService {
    pub(crate) fn operation_events(&self) -> OperationEventAuthority {
        self.operation_events.clone()
    }

    /// Exact in-process handler call for a daemon-retained PR12 primitive.
    /// Callers must supply the authenticated request context minted during
    /// project admission; no path or client selector is resolved here.
    pub(crate) async fn dispatch_pr12_primitive(
        &self,
        project_root: &Path,
        invocation: Pr12PrimitiveInvocation,
        context: RequestContext,
        observed_at: UtcMicros,
    ) -> Option<ApplicationResult<serde_json::Value>> {
        let dispatch = self
            .project_runtimes
            .read(project_root, Pr12PrimitiveProjectRuntime::dispatch)
            .await?;
        Some(dispatch.dispatch(invocation, context, observed_at).await)
    }

    pub(crate) async fn feedback_runtime(
        &self,
        project_root: Option<&Path>,
    ) -> Option<Arc<Pr12FeedbackRuntime>> {
        self.project_runtimes
            .read::<RegisteredFeedbackRuntime, _, _>(project_root?, |registered| {
                registered.runtime.clone()
            })
            .await
    }

    async fn configuration_runtime(
        &self,
        project_root: Option<&Path>,
    ) -> Option<RegisteredConfigurationRuntime> {
        self.project_runtimes.get(project_root?).await
    }

    async fn work_runtime(&self, project_root: Option<&Path>) -> Option<RegisteredWorkRuntime> {
        self.project_runtimes.get(project_root?).await
    }

    pub(crate) async fn semantic_configuration_operation(
        &self,
        project_root: &Path,
    ) -> Option<Arc<ProductionSemanticConfigurationOperationV1>> {
        self.project_runtimes
            .read::<RegisteredConfigurationRuntime, _, _>(project_root, |registered| {
                registered.semantic_operation.get().cloned()
            })
            .await
            .flatten()
    }

    pub(crate) async fn feedback_cycle(
        &self,
        project_root: Option<&Path>,
    ) -> Option<Arc<Pr12FeedbackCycleRuntime>> {
        self.project_runtimes.get(project_root?).await
    }

    async fn feedback_cycle_input(
        &self,
        project_root: Option<&Path>,
    ) -> Option<Arc<dyn FeedbackCycleRuntimePort>> {
        self.project_runtimes
            .get::<Arc<SwitchableFeedbackCycleRuntimeV1>>(project_root?)
            .await
            .map(|input| -> Arc<dyn FeedbackCycleRuntimePort> { input })
    }

    pub(crate) async fn feedback_publication_store(
        &self,
        project_root: Option<&Path>,
    ) -> Option<ProjectFeedbackStore> {
        self.project_runtimes
            .read::<RegisteredFeedbackRuntime, _, _>(project_root?, |registered| {
                registered.runtime.publication_store()
            })
            .await
    }

    async fn install_lsp_owner(&self, project_root: PathBuf, owner: DaemonLspInvocationOwner) {
        // Reinstalled on every project open by the same admission authority.
        self.project_runtimes.publish(project_root, owner).await;
    }

    pub(crate) async fn lsp_owner(
        &self,
        project_root: Option<&Path>,
    ) -> Option<DaemonLspInvocationOwner> {
        let project_root = project_root?;
        if let Some(owner) = self
            .project_runtimes
            .get::<DaemonLspInvocationOwner>(project_root)
            .await
        {
            return Some(owner);
        }
        let canonical_root = project_root.canonicalize().ok()?;
        self.project_runtimes.get(&canonical_root).await
    }

    pub(crate) async fn lsp_owner_matches_scope(
        &self,
        project_root: &Path,
        scope: &ResolvedScope,
    ) -> bool {
        self.lsp_owner(Some(project_root))
            .await
            .and_then(|owner| owner.scope_grant)
            .is_some_and(|grant| grant.scope == *scope)
    }

    pub(crate) async fn multi_root_query_context(
        &self,
        project_root: &Path,
        scope: &ResolvedScope,
        ordinal: usize,
        observed_at: UtcMicros,
    ) -> Option<(RequestContext, ManifestDigest)> {
        let owner = self.lsp_owner(Some(project_root)).await?;
        let grant = owner.scope_grant?;
        if grant.scope != *scope {
            return None;
        }
        let digest = grant.digest.clone();
        let context = RequestContext::new(
            grant.issuer.clone(),
            scope.clone(),
            grant,
            RequestId::new(format!("request.multi-root.query.{ordinal}")).ok()?,
            Deadline::new(UtcMicros(observed_at.0.saturating_add(5 * 60 * 1_000_000))).ok()?,
            CancellationContext::active(format!("cancel.multi-root.query.{ordinal}")).ok()?,
        )
        .ok()?;
        Some((context, digest))
    }

    pub(crate) async fn persisted_scope_set(
        &self,
        project_root: &Path,
        scope_set_id: &ScopeSetId,
    ) -> Option<AuthorizedScopeSet> {
        self.lsp_owner(Some(project_root))
            .await?
            .scope_set_storage?
            .read(scope_set_id)
            .ok()?
    }

    pub(crate) async fn authorize_lsp_workspace(
        &self,
        mut roots: Vec<(PathBuf, String, ResolvedScope)>,
        observed_at: UtcMicros,
    ) -> Option<AuthorizedLspWorkspace> {
        if roots.is_empty() {
            return None;
        }
        if !canonicalize_lsp_roots(&mut roots) {
            return None;
        }
        let mut contexts = Vec::with_capacity(roots.len());
        let mut owners = Vec::with_capacity(roots.len());
        let mut scope_set_storages = Vec::with_capacity(roots.len());
        for (ordinal, (project_root, _, scope)) in roots.iter().enumerate() {
            let owner = self.lsp_owner(Some(project_root)).await?;
            let owner_storage = owner.scope_set_storage.clone()?;
            let grant = owner.scope_grant.clone()?;
            if grant.scope != *scope {
                return None;
            }
            owners.push(owner);
            scope_set_storages.push(owner_storage);
            let request_id = RequestId::new(format!("request.lsp.workspace.{ordinal}")).ok()?;
            let deadline =
                Deadline::new(UtcMicros(observed_at.0.saturating_add(5 * 60 * 1_000_000))).ok()?;
            let cancellation =
                CancellationContext::active(format!("cancel.lsp.workspace.{ordinal}")).ok()?;
            contexts.push(
                RequestContext::new(
                    grant.issuer.clone(),
                    scope.clone(),
                    grant,
                    request_id,
                    deadline,
                    cancellation,
                )
                .ok()?,
            );
        }
        let selector_digest = canonical_sha256(&(
            "tracedecay.daemon.lsp-workspace-selector.v1",
            roots
                .iter()
                .map(|(_, _, scope)| &scope.scope_digest)
                .collect::<Vec<_>>(),
        ))
        .ok()?;
        let scope_set_id = ScopeSetId::new(format!(
            "scope-set.lsp.{}",
            selector_digest.as_str().trim_start_matches("sha256:")
        ))
        .ok()?;
        let capability =
            CapabilityId::new(crate::daemon::project_open_owners::LSP_WORKSPACE_CAPABILITY_ID_V1)
                .ok()?;
        let use_case =
            UseCaseId::new(crate::daemon::project_open_owners::LSP_WORKSPACE_USE_CASE_ID_V1)
                .ok()?;
        let scope_set = AuthorizedScopeSetAuthority::authorize(
            scope_set_id,
            ScopeSetRevision::new(1).ok()?,
            contexts,
            &capability,
            &use_case,
            observed_at,
        )
        .ok()?;
        for storage in &scope_set_storages {
            self.persist_exact_scope_set(storage, &scope_set).ok()??;
        }
        let admitted_roots = roots
            .iter()
            .map(|(_, uri, scope)| {
                AdmittedRoot::authorized(uri.clone(), scope.scope_digest.clone())
            })
            .collect::<Vec<_>>();
        let workspace =
            AuthorizedLspWorkspace::new(Some(scope_set.digest().clone()), admitted_roots.clone())
                .ok()?;
        let factories = admitted_roots
            .into_iter()
            .zip(owners.into_iter().map(|owner| owner.factory))
            .collect();
        let authorized = AuthorizedDaemonLspWorkspace {
            workspace: workspace.clone(),
            scope_set,
            factories,
        };
        let mut workspaces = self.authorized_lsp_workspaces.lock().await;
        if workspaces.len() >= MAX_LSP_SESSIONS
            && !workspaces.contains_key(authorized.scope_set.digest())
        {
            return None;
        }
        workspaces.insert(authorized.scope_set.digest().clone(), authorized);
        Some(workspace)
    }

    pub(crate) async fn compare_and_swap_scope_set(
        &self,
        active_project_root: &Path,
        request: MultiRootScopeSetCasRequestV1,
        mut roots: Vec<(PathBuf, ResolvedScope)>,
        observed_at: UtcMicros,
    ) -> Option<(ResolvedScope, MultiRootScopeSetCasResultV1)> {
        request.validate().ok()?;
        roots.sort_by(|left, right| left.1.scope_digest.cmp(&right.1.scope_digest));
        if roots.is_empty()
            || roots
                .windows(2)
                .any(|pair| pair[0].1.scope_digest == pair[1].1.scope_digest)
        {
            return None;
        }
        let active_owner = self.lsp_owner(Some(active_project_root)).await?;
        let active_scope = active_owner.scope_grant.as_ref()?.scope.clone();
        let active_storage = active_owner.scope_set_storage?;
        let current = active_storage.read(&request.scope_set_id).ok()?;
        let next_revision = match (request.expected_revision, current.as_ref()) {
            (None, None) => ScopeSetRevision::new(1).ok()?,
            (Some(expected), Some(current)) if current.revision() == expected => {
                ScopeSetRevision::new(expected.get().checked_add(1)?).ok()?
            }
            _ => {
                return Some((
                    active_scope,
                    MultiRootScopeSetCasResultV1 {
                        status: MultiRootScopeSetCasStatusV1::Conflict,
                        scope_set: current,
                    },
                ));
            }
        };
        let capability =
            CapabilityId::new(crate::daemon::project_open_owners::LSP_WORKSPACE_CAPABILITY_ID_V1)
                .ok()?;
        let use_case =
            UseCaseId::new(crate::daemon::project_open_owners::LSP_WORKSPACE_USE_CASE_ID_V1)
                .ok()?;
        let mut contexts = Vec::with_capacity(roots.len());
        let mut storages = vec![active_storage.clone()];
        for (ordinal, (project_root, scope)) in roots.iter().enumerate() {
            let owner = self.lsp_owner(Some(project_root)).await?;
            let grant = owner.scope_grant?;
            if grant.scope != *scope {
                return None;
            }
            if let Some(storage) = owner.scope_set_storage {
                storages.push(storage);
            }
            contexts.push(
                RequestContext::new(
                    grant.issuer.clone(),
                    scope.clone(),
                    grant,
                    RequestId::new(format!("request.multi-root.cas.{ordinal}")).ok()?,
                    Deadline::new(UtcMicros(observed_at.0.saturating_add(5 * 60 * 1_000_000)))
                        .ok()?,
                    CancellationContext::active(format!("cancel.multi-root.cas.{ordinal}")).ok()?,
                )
                .ok()?,
            );
        }
        let next = AuthorizedScopeSetAuthority::authorize(
            request.scope_set_id,
            next_revision,
            contexts,
            &capability,
            &use_case,
            observed_at,
        )
        .ok()?;
        for storage in storages {
            match storage
                .compare_and_swap(request.expected_revision, &next)
                .ok()?
            {
                tracedecay_store::runtime::ScopeSetCasOutcomeV1::Applied(_) => {}
                tracedecay_store::runtime::ScopeSetCasOutcomeV1::Conflict { .. } => {
                    let stored = storage.read(next.scope_set_id()).ok()?;
                    if stored.as_ref() != Some(&next) {
                        return None;
                    }
                }
            }
        }
        Some((
            active_scope,
            MultiRootScopeSetCasResultV1 {
                status: MultiRootScopeSetCasStatusV1::Applied,
                scope_set: Some(next),
            },
        ))
    }

    pub(crate) async fn multi_root_evidence<T>(
        &self,
        project_root: &Path,
        request_id: RequestId,
        operation_key: &str,
        payload: T,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Option<(ResolvedScope, ApplicationOutcome<T>)>
    where
        T: Serialize,
    {
        let owner = self.lsp_owner(Some(project_root)).await?;
        let grant = owner.scope_grant?;
        let scope = grant.scope.clone();
        let context = RequestContext::new(
            grant.issuer.clone(),
            scope.clone(),
            grant.clone(),
            request_id,
            deadline.clone(),
            cancellation,
        )
        .ok()?;
        let policy_digest = canonical_sha256(&(
            "tracedecay.daemon.multi-root-policy.v1",
            &grant.digest,
            operation_key,
        ))
        .ok()?;
        let policy = PolicyDecisionRef::new(
            format!("policy.daemon.multi-root.{operation_key}.v1"),
            1,
            policy_digest,
            ComponentVersion::new("tracedecay.daemon.multi-root-policy.v1").ok()?,
        )
        .ok()?;
        let authority = AuthorityReceipt::from_context(&context, policy, observed_at).ok()?;
        let execution = OperationReceipt::completed(
            observed_at,
            current_micros(),
            deadline,
            OperationBudgetUsage::default(),
        )
        .ok()?;
        let evidence_digest = canonical_sha256(&(
            "tracedecay.daemon.multi-root-evidence.v1",
            operation_key,
            &scope,
            &payload,
        ))
        .ok()?;
        let packet = EvidencePacket {
            temporal: TemporalState::current(execution.ended_at),
            authority,
            evidence_authorities: vec![EvidenceAuthority {
                evidence_id: EvidenceIdentity::new(format!(
                    "evidence.multi-root.{}",
                    evidence_digest.as_str().trim_start_matches("sha256:")
                ))
                .ok()?,
                source_kind: "registered_multi_root".to_owned(),
                producer: operation_key.to_owned(),
                scope: scope.clone(),
                revision: ComponentVersion::new("tracedecay.multi-root.v1").ok()?,
                horizon: Some(execution.ended_at),
            }],
            coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Operational], 1, 1, 1)
                .ok()?,
            omissions: Vec::new(),
            scores: Vec::new(),
            contributions: Vec::new(),
            page: PageState::first_page(
                SortContractId::new("sort.multi-root.scope-order.v1").ok()?,
                1,
                Some(1),
                1,
            )
            .ok()?,
            execution,
            payload: Some(payload),
        };
        Some((scope, ApplicationOutcome::Evidence(packet)))
    }

    async fn execute_semantic_evaluation(
        &self,
        project_root: Option<&Path>,
        request_id: String,
        candidate: crate::application::semantic_runtime::SemanticEvaluationProfileCandidateV1,
    ) -> DaemonInvocationResponse {
        let Some(project_root) = project_root else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        };
        let Some(registered) = self.configuration_runtime(Some(project_root)).await else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        };
        let Some(operation) = registered.semantic_operation.get().cloned() else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        };
        let canonical_root = match project_root.canonicalize() {
            Ok(root) => root,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            }
        };
        let authority =
            crate::daemon::semantic_evaluation::DaemonSemanticEvaluationSnapshotAuthorityV1::new(
                canonical_root.clone(),
                registered.scope.clone(),
                self.code_index_schedulers.clone(),
                candidate.clone(),
            );
        match operation
            .evaluate_and_publish_profile(&authority, &canonical_root, candidate)
            .await
        {
            Ok(publication) => DaemonInvocationResponse::with_outcome(
                request_id,
                DaemonInvocationOutcome::SemanticEvaluatedProfilePublished {
                    scope: publication.snapshot.scope,
                    profile_digest: publication.accepted_profile.profile_digest().clone(),
                    report_digest: publication
                        .accepted_profile
                        .evaluation()
                        .report_digest()
                        .clone(),
                    report: publication.report,
                    source_generation: publication.snapshot.code_generation,
                    snapshot_digest: publication.snapshot.code_snapshot_digest,
                },
            ),
            Err(SemanticActivationCoordinationErrorV1::Rejected) => {
                DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::InvalidRequest,
                )
            }
            Err(SemanticActivationCoordinationErrorV1::Conflict) => {
                DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable)
            }
            Err(SemanticActivationCoordinationErrorV1::Runtime(_)) => {
                DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable)
            }
            Err(SemanticActivationCoordinationErrorV1::Unavailable) => {
                DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable)
            }
        }
    }

    /// Executes a closed request after daemon socket authentication. `root` is
    /// supplied only after the daemon has opened and authorized the project;
    /// existing LSP session operations do not re-resolve client paths.
    pub(crate) async fn invoke(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        project_root: Option<&Path>,
        root: Option<AdmittedRoot>,
        git_service: Option<DaemonGitInvocationOwner>,
        request: DaemonInvocationRequest,
    ) -> DaemonInvocationResponse {
        let request_id = request.request_id.clone();
        let operation = request.operation();
        let delivery_route = request.delivery_route;
        // Every per-project component this request may need, taken in one pass
        // so dispatch sees one consistent view of the project.
        let runtimes = self
            .project_runtimes
            .request_runtimes(
                project_root,
                project_root
                    .and_then(|root| root.canonicalize().ok())
                    .as_deref(),
            )
            .await;
        let feedback_runtime = runtimes.feedback;
        let observations = feedback_runtime
            .as_ref()
            .map(|runtime| runtime.source_observation_port());
        let observation_subject = plan26_invocation_subject(&request_id, operation, delivery_route);
        if let Err(problem) = request.validate() {
            if plan26_observable_operation(operation)
                && let Some((argument, rejection)) =
                    plan26_invocation_problem_rejected_argument(problem)
            {
                emit_plan26_invocation_event(
                    observations.as_ref(),
                    observation_subject.as_ref(),
                    current_micros(),
                    Plan26FeedbackSourceEventV1::SurfaceArgumentRejected {
                        operation: plan26_feedback_operation(operation),
                        route: delivery_route,
                        argument,
                        rejection,
                        schema_revision: 1,
                        outcome: Plan26FeedbackOutcomeV1::Rejected,
                    },
                );
            }
            return DaemonInvocationResponse::problem(request_id, problem);
        }
        let dispatched_at = current_micros();
        if plan26_observable_operation(operation) {
            emit_plan26_invocation_event(
                observations.as_ref(),
                observation_subject.as_ref(),
                dispatched_at,
                Plan26FeedbackSourceEventV1::Dispatch {
                    operation: plan26_feedback_operation(operation),
                    outcome: Plan26FeedbackOutcomeV1::Admitted,
                    capacity: 1,
                    admitted: 1,
                },
            );
        }
        let now_ms = now_millis();
        self.expire_sessions(now_ms).await;
        let feedback_service = runtimes.feedback_owner;
        let advisory_cycle_invoker = runtimes.advisory_cycle_invoker;
        let configuration_runtime = runtimes.configuration;
        let lsp_owner = runtimes.lsp_owner;

        let response = match request.payload {
            DaemonInvocationPayload::GitRead {
                surface_operation,
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_git_read(
                    request_id,
                    project_root,
                    git_service,
                    surface_operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::GitPreview {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_git_preview(
                    &self.operation_events,
                    request_id,
                    git_service,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::GitApply {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_git_apply(
                    &self.operation_events,
                    request_id,
                    git_service,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackDiagnostics {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_feedback(
                    request_id,
                    feedback_service,
                    DaemonInvocationOperation::FeedbackDiagnostics,
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackGet {
                request_handle,
                resolved_scope,
                observed_at,
                deadline,
                cancellation,
            } => {
                if !feedback_scope_matches(
                    resolved_scope.as_ref(),
                    project_root,
                    feedback_service.as_ref(),
                ) {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::NotFoundOrNotAuthorized,
                    );
                }
                execute_feedback(
                    request_id,
                    feedback_service,
                    DaemonInvocationOperation::FeedbackGet,
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackExpand {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_feedback(
                    request_id,
                    feedback_service,
                    DaemonInvocationOperation::FeedbackExpand,
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackList {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_feedback(
                    request_id,
                    feedback_service,
                    DaemonInvocationOperation::FeedbackList,
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackAdvisoryCycle {
                document_uri,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_feedback_advisory_cycle(
                    request_id,
                    advisory_cycle_invoker,
                    feedback_service,
                    document_uri,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackImpact {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_feedback(
                    request_id,
                    feedback_service,
                    DaemonInvocationOperation::FeedbackImpact,
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::AffectedTests {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_feedback(
                    request_id,
                    feedback_service,
                    DaemonInvocationOperation::AffectedTests,
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackObserve {
                subject_digest,
                observed_at,
                event,
            } => {
                if let Some(observations) = observations.as_ref() {
                    observations.observe_source_event_for_subject(
                        subject_digest,
                        observed_at,
                        event,
                    );
                    DaemonInvocationResponse::with_outcome(
                        request_id,
                        DaemonInvocationOutcome::ObservationAccepted,
                    )
                } else {
                    DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    )
                }
            }
            DaemonInvocationPayload::PrimitiveImpact {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_primitive(
                    self,
                    project_root,
                    request_id,
                    crate::application_surface::ApplicationSurfaceOperation::FeedbackImpact,
                    Pr12PrimitiveRequest::Impact(request),
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::PrimitiveAffectedTests {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_primitive(
                    self,
                    project_root,
                    request_id,
                    crate::application_surface::ApplicationSurfaceOperation::AffectedTests,
                    Pr12PrimitiveRequest::AffectedFileTests(request),
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::PrimitiveTestResults {
                page,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_primitive(
                    self,
                    project_root,
                    request_id,
                    crate::application_surface::ApplicationSurfaceOperation::TestResults,
                    Pr12PrimitiveRequest::RecentTestResults(page),
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::PrimitiveRead {
                surface_operation,
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_primitive(
                    self,
                    project_root,
                    request_id,
                    surface_operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::PrimitiveCode {
                surface_operation,
                request,
                page,
                observed_at,
                deadline,
                cancellation,
            } => {
                let request = match request.into_primitive(
                    crate::daemon::code_index_scheduler::queries::callable_query_sanitizer_revision(
                    ),
                    crate::daemon::code_index_scheduler::queries::callable_query_normalization_revision(
                    ),
                    page,
                ) {
                    Ok(request) => request,
                    Err(_) => {
                        return DaemonInvocationResponse::problem(
                            request_id,
                            DaemonInvocationProblem::InvalidRequest,
                        );
                    }
                };
                execute_primitive(
                    self,
                    project_root,
                    request_id,
                    surface_operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::CallableCode {
                surface_operation,
                request,
                page,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_callable_code(
                    self,
                    project_root,
                    request_id,
                    surface_operation,
                    request,
                    page,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::Configuration {
                surface_operation,
                request,
                resolved_scope,
                observed_at,
                deadline,
                cancellation,
            } => {
                if !resolved_scope.as_ref().is_none_or(|scope| {
                    configuration_runtime
                        .as_ref()
                        .is_some_and(|registered| &registered.scope == scope)
                }) {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::NotFoundOrNotAuthorized,
                    );
                }
                execute_configuration(
                    request_id,
                    configuration_runtime,
                    surface_operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::ContextScout {
                surface_operation,
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_context_scout(
                    self,
                    request_id,
                    configuration_runtime,
                    surface_operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::MultiRootScopeSetRead {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                let Some(owner) = self.lsp_owner(project_root).await else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    );
                };
                let Some(storage) = owner.scope_set_storage else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    );
                };
                match storage.read(&request.scope_set_id) {
                    Ok(Some(scope_set)) => {
                        let Some(project_root) = project_root else {
                            return DaemonInvocationResponse::problem(
                                request_id,
                                DaemonInvocationProblem::Unavailable,
                            );
                        };
                        let Some((scope, outcome)) = self
                            .multi_root_evidence(
                                project_root,
                                RequestId::new(request_id.clone())
                                    .expect("validated daemon request id"),
                                "scope_set_read",
                                Some(scope_set),
                                observed_at,
                                deadline,
                                cancellation,
                            )
                            .await
                        else {
                            return DaemonInvocationResponse::problem(
                                request_id,
                                DaemonInvocationProblem::Unavailable,
                            );
                        };
                        DaemonInvocationResponse::with_outcome(
                            request_id,
                            DaemonInvocationOutcome::MultiRootScopeSetRead { scope, outcome },
                        )
                    }
                    Ok(None) => DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::NotFoundOrNotAuthorized,
                    ),
                    Err(_) => DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    ),
                }
            }
            DaemonInvocationPayload::MultiRootScopeSetCompareAndSwap { .. }
            | DaemonInvocationPayload::MultiRootExecute { .. } => {
                DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable)
            }
            DaemonInvocationPayload::WorkApplication {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                let Some(registered) = self.work_runtime(project_root).await else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    );
                };
                execute_work_application(
                    registered,
                    request_id,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                )
            }
            DaemonInvocationPayload::WorkAttempt {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                let Some(registered) = self.work_runtime(project_root).await else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    );
                };
                execute_work_attempt(
                    registered,
                    request_id,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::SemanticEvaluateAndPublish { candidate } => {
                self.execute_semantic_evaluation(project_root, request_id, candidate)
                    .await
            }
            DaemonInvocationPayload::LspOpen {
                client_revision,
                requested_root_uri,
                workspace_folders,
            } => {
                self.open_lsp_session(
                    lsp_registry,
                    root,
                    request_id,
                    client_revision,
                    requested_root_uri,
                    workspace_folders,
                    now_ms,
                    lsp_owner,
                )
                .await
            }
            DaemonInvocationPayload::LspFrame { session, frame } => {
                self.send_lsp_frame(lsp_registry, request_id, session, frame, now_ms)
                    .await
            }
            DaemonInvocationPayload::LspPoll { session } => {
                self.poll_lsp_frame(lsp_registry, request_id, session, now_ms)
                    .await
            }
            DaemonInvocationPayload::LspAcknowledge { session } => {
                self.acknowledge_lsp_frame(lsp_registry, request_id, session, now_ms)
                    .await
            }
            DaemonInvocationPayload::LspReconnect { session } => {
                self.reconnect_lsp_session(lsp_registry, request_id, session, now_ms)
                    .await
            }
            DaemonInvocationPayload::LspDetach { session } => {
                self.detach_lsp_session(lsp_registry, request_id, session, now_ms)
                    .await
            }
        };
        if plan26_observable_operation(operation) {
            observe_plan26_invocation_response(
                observations.as_ref(),
                observation_subject.as_ref(),
                operation,
                delivery_route,
                dispatched_at,
                &response,
            );
        }
        response
    }

    pub(crate) async fn expire_all(&self) {
        self.lsp_sessions.lock().await.clear();
        self.context_scout_registries.lock().await.clear();
        self.project_runtimes.shut_down_all().await;
        if let Ok(mut registry) = pr13_hook_orchestration_registry().lock() {
            registry.retain(|_, runtime| runtime.strong_count() > 0);
        }
        self.operation_events.expire_all().await;
    }

    async fn open_lsp_session(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        root: Option<AdmittedRoot>,
        request_id: String,
        client_revision: String,
        requested_root_uri: Option<String>,
        workspace_folders: Vec<String>,
        now_ms: u64,
        lsp_owner: Option<DaemonLspInvocationOwner>,
    ) -> DaemonInvocationResponse {
        let Some(root) = root else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let Some(lsp_owner) = lsp_owner else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let request = LspSessionOpenRequest {
            requested_root_uri,
            workspace_folders,
            client_revision,
        };
        let access = {
            let mut registry = lsp_registry.lock().await;
            let existing = std::mem::take(&mut *registry);
            let mut endpoint = DaemonLspSessionEndpoint::with_registry(
                AdmittedRootSessionAdmission { root: root.clone() },
                existing,
            );
            let result = endpoint.open(request, now_ms);
            *registry = endpoint.into_registry();
            result
        };
        let Ok(access) = access else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let expires_at_ms = now_ms.saturating_add(LSP_SESSION_TTL_MS);
        let session_id = access.session_id().clone();
        let actor = runtime_lsp_actor(root, lsp_owner);
        self.lsp_sessions.lock().await.insert(
            session_id,
            RuntimeLspSession {
                expires_at_ms,
                actor,
            },
        );
        DaemonInvocationResponse::lsp_opened(
            request_id,
            DaemonLspSessionAccess::from_access(&access),
            expires_at_ms,
        )
    }

    async fn send_lsp_frame(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        frame: String,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match self.authenticate(lsp_registry, session, now_ms).await {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let dispatch = session.actor.handle_payload(frame.as_bytes(), now_ms);
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::LspFrameAccepted {
                backpressured: dispatch.backpressured,
                closed: dispatch.closed,
            },
        )
    }

    async fn poll_lsp_frame(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match self.authenticate(lsp_registry, session, now_ms).await {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let dispatch = session.actor.flush_due(now_ms);
        let frame = session
            .actor
            .poll_outbound()
            .and_then(|frame| std::str::from_utf8(frame).ok())
            .map(str::to_owned);
        let closed = dispatch.closed
            || matches!(
                session.actor.lifecycle(),
                SessionLifecycle::Exited | SessionLifecycle::Expired
            );
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::LspFrame { frame, closed },
        )
    }

    async fn acknowledge_lsp_frame(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match self.authenticate(lsp_registry, session, now_ms).await {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::LspAcknowledged {
                acknowledged: session.actor.acknowledge_outbound(),
            },
        )
    }

    async fn detach_lsp_session(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match self.authenticate(lsp_registry, session, now_ms).await {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let endpoint_detached = {
            let mut registry = lsp_registry.lock().await;
            registry.close(&access, now_ms).is_ok()
        };
        let Some(mut session) = self.lsp_sessions.lock().await.remove(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        if !endpoint_detached {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        }
        let _ = session.actor.detach();
        DaemonInvocationResponse::with_outcome(request_id, DaemonInvocationOutcome::LspDetached)
    }

    async fn reconnect_lsp_session(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match session.into_access() {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let mut credential_bytes = [0_u8; 32];
        if getrandom::getrandom(&mut credential_bytes).is_err() {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
        let Ok(credential) = LspSessionCredential::new(credential_bytes.to_vec()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        };
        let reconnected_access = lsp_registry
            .lock()
            .await
            .reconnect_with_credential(&access, credential, now_ms);
        let Ok(reconnected_access) = reconnected_access else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            drop(sessions);
            let _ = lsp_registry.lock().await.close(&reconnected_access, now_ms);
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let actor_reconnected = match session.actor.lifecycle() {
            SessionLifecycle::Detached => session.actor.reconnect().is_ok(),
            SessionLifecycle::AwaitingInitialize
            | SessionLifecycle::AwaitingInitialized
            | SessionLifecycle::Ready
            | SessionLifecycle::Shutdown => true,
            SessionLifecycle::Exited | SessionLifecycle::Expired => false,
        };
        if !actor_reconnected {
            drop(sessions);
            let _ = lsp_registry.lock().await.close(&reconnected_access, now_ms);
            self.lsp_sessions.lock().await.remove(access.session_id());
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        }
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::LspReconnected {
                session: DaemonLspSessionAccess::from_access(&reconnected_access),
            },
        )
    }

    pub(crate) async fn disconnect_lsp_session(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        session: DaemonLspSessionAccess,
    ) {
        let Ok(access) = session.into_access() else {
            return;
        };
        let now_ms = now_millis();
        if lsp_registry.lock().await.detach(&access, now_ms).is_err() {
            return;
        }
        let expires_at_ms = {
            let mut sessions = self.lsp_sessions.lock().await;
            let Some(session) = sessions.get_mut(access.session_id()) else {
                return;
            };
            let _ = session.actor.detach();
            session.expires_at_ms
        };
        let sessions = Arc::clone(&self.lsp_sessions);
        let registry = Arc::clone(lsp_registry);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(
                expires_at_ms.saturating_sub(now_millis()),
            ))
            .await;
            let now_ms = now_millis();
            registry.lock().await.expire_at(now_ms);
            sessions
                .lock()
                .await
                .retain(|_, session| session.expires_at_ms > now_ms);
        });
    }

    async fn authenticate(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> Result<LspSessionAccess, DaemonInvocationProblem> {
        let access = session.into_access()?;
        let authentication = {
            let mut registry = lsp_registry.lock().await;
            registry
                .authenticate(&access, now_ms)
                .map(|_| ())
                .map_err(|error| matches!(error, LspEndpointError::SessionExpired))
        };
        match authentication {
            Ok(()) => Ok(access),
            Err(expired) => {
                if expired {
                    self.lsp_sessions.lock().await.remove(access.session_id());
                }
                Err(DaemonInvocationProblem::NotFoundOrNotAuthorized)
            }
        }
    }

    async fn expire_sessions(&self, now_ms: u64) {
        self.lsp_sessions
            .lock()
            .await
            .retain(|_, session| session.expires_at_ms > now_ms);
    }
}

fn runtime_lsp_actor(root: AdmittedRoot, owner: DaemonLspInvocationOwner) -> RuntimeLspActor {
    owner.factory.open_session(root)
}

fn plan26_invocation_subject(
    request_id: &str,
    operation: DaemonInvocationOperation,
    route: Option<Plan26DeliveryRouteV1>,
) -> Option<ManifestDigest> {
    canonical_sha256(&(
        "tracedecay.feedback.transport-observation.v1",
        request_id,
        operation.as_str(),
        route,
    ))
    .ok()
}

fn plan26_observable_operation(operation: DaemonInvocationOperation) -> bool {
    matches!(
        operation,
        DaemonInvocationOperation::FeedbackDiagnostics
            | DaemonInvocationOperation::FeedbackGet
            | DaemonInvocationOperation::FeedbackExpand
            | DaemonInvocationOperation::FeedbackList
            | DaemonInvocationOperation::FeedbackAdvisoryCycle
            | DaemonInvocationOperation::FeedbackImpact
            | DaemonInvocationOperation::AffectedTests
            | DaemonInvocationOperation::PrimitiveImpact
            | DaemonInvocationOperation::PrimitiveAffectedTests
            | DaemonInvocationOperation::PrimitiveTestResults
            | DaemonInvocationOperation::PrimitiveRead
    )
}

fn plan26_feedback_operation(operation: DaemonInvocationOperation) -> Plan26FeedbackOperationV1 {
    match operation {
        DaemonInvocationOperation::FeedbackDiagnostics => {
            Plan26FeedbackOperationV1::FeedbackDiagnostics
        }
        DaemonInvocationOperation::FeedbackGet => Plan26FeedbackOperationV1::FeedbackGet,
        DaemonInvocationOperation::FeedbackExpand => Plan26FeedbackOperationV1::FeedbackExpand,
        DaemonInvocationOperation::FeedbackList => Plan26FeedbackOperationV1::FeedbackList,
        DaemonInvocationOperation::FeedbackAdvisoryCycle => {
            Plan26FeedbackOperationV1::FeedbackCycle
        }
        DaemonInvocationOperation::FeedbackImpact => Plan26FeedbackOperationV1::PrimitiveImpact,
        DaemonInvocationOperation::AffectedTests => {
            Plan26FeedbackOperationV1::PrimitiveAffectedTests
        }
        DaemonInvocationOperation::PrimitiveImpact => Plan26FeedbackOperationV1::PrimitiveImpact,
        DaemonInvocationOperation::PrimitiveAffectedTests => {
            Plan26FeedbackOperationV1::PrimitiveAffectedTests
        }
        DaemonInvocationOperation::PrimitiveTestResults => {
            Plan26FeedbackOperationV1::PrimitiveTestResults
        }
        DaemonInvocationOperation::LspOpen
        | DaemonInvocationOperation::LspFrame
        | DaemonInvocationOperation::LspPoll
        | DaemonInvocationOperation::LspAcknowledge
        | DaemonInvocationOperation::LspReconnect
        | DaemonInvocationOperation::LspDetach => Plan26FeedbackOperationV1::LspSession,
        DaemonInvocationOperation::FeedbackObserve
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
        | DaemonInvocationOperation::MultiRootScopeSetRead
        | DaemonInvocationOperation::MultiRootScopeSetCompareAndSwap
        | DaemonInvocationOperation::MultiRootExecute
        | DaemonInvocationOperation::WorkApplication
        | DaemonInvocationOperation::WorkAttempt
        | DaemonInvocationOperation::SemanticEvaluateAndPublish
        | DaemonInvocationOperation::GitStatus
        | DaemonInvocationOperation::GitDiff
        | DaemonInvocationOperation::GitHistory
        | DaemonInvocationOperation::GitBlame
        | DaemonInvocationOperation::GitHunks
        | DaemonInvocationOperation::GitPreview
        | DaemonInvocationOperation::GitApply => Plan26FeedbackOperationV1::FeedbackCycle,
    }
}

fn emit_plan26_invocation_event(
    observations: Option<&Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>>,
    subject: Option<&ManifestDigest>,
    observed_at: UtcMicros,
    event: Plan26FeedbackSourceEventV1,
) {
    if let (Some(observations), Some(subject)) = (observations, subject) {
        observations.observe_source_event_for_subject(subject.clone(), observed_at, event);
    }
}

fn plan26_response_outcome(response: &DaemonInvocationResponse) -> Plan26FeedbackOutcomeV1 {
    match &response.outcome {
        DaemonInvocationOutcome::GitRead { .. }
        | DaemonInvocationOutcome::GitPreview { .. }
        | DaemonInvocationOutcome::GitApply { .. }
        | DaemonInvocationOutcome::Configuration { .. }
        | DaemonInvocationOutcome::ContextScout { .. }
        | DaemonInvocationOutcome::MultiRootScopeSetRead { .. }
        | DaemonInvocationOutcome::MultiRootScopeSetCompareAndSwap { .. }
        | DaemonInvocationOutcome::MultiRootQueryPage { .. }
        | DaemonInvocationOutcome::WorkApplication { .. }
        | DaemonInvocationOutcome::WorkAttempt { .. }
        | DaemonInvocationOutcome::SemanticEvaluatedProfilePublished { .. }
        | DaemonInvocationOutcome::ObservationAccepted
        | DaemonInvocationOutcome::LspOpened { .. }
        | DaemonInvocationOutcome::LspAcknowledged { .. }
        | DaemonInvocationOutcome::LspReconnected { .. }
        | DaemonInvocationOutcome::LspDetached => Plan26FeedbackOutcomeV1::Completed,
        DaemonInvocationOutcome::Feedback { result, .. }
        | DaemonInvocationOutcome::Primitive { result, .. }
        | DaemonInvocationOutcome::CallableCode { result, .. } => {
            match result.execution.termination {
                OperationTermination::Completed => Plan26FeedbackOutcomeV1::Completed,
                OperationTermination::Cancelled => Plan26FeedbackOutcomeV1::Cancelled,
                OperationTermination::TimedOut => Plan26FeedbackOutcomeV1::TimedOut,
                OperationTermination::Failed | OperationTermination::EffectUnknown => {
                    Plan26FeedbackOutcomeV1::Failed
                }
                OperationTermination::Partial => Plan26FeedbackOutcomeV1::Partial,
            }
        }
        DaemonInvocationOutcome::LspFrameAccepted { backpressured, .. } => {
            if *backpressured {
                Plan26FeedbackOutcomeV1::AtCapacity
            } else {
                Plan26FeedbackOutcomeV1::Accepted
            }
        }
        DaemonInvocationOutcome::LspFrame { closed, .. } => {
            if *closed {
                Plan26FeedbackOutcomeV1::Disconnected
            } else {
                Plan26FeedbackOutcomeV1::Completed
            }
        }
        DaemonInvocationOutcome::ApplicationProblem { problem } => match problem.kind() {
            ApplicationProblemKind::InvalidRequest => Plan26FeedbackOutcomeV1::Rejected,
            ApplicationProblemKind::NotFoundOrNotAuthorized => Plan26FeedbackOutcomeV1::Denied,
            ApplicationProblemKind::Conflict | ApplicationProblemKind::Stale => {
                Plan26FeedbackOutcomeV1::Stale
            }
            ApplicationProblemKind::Unsupported | ApplicationProblemKind::Unavailable => {
                Plan26FeedbackOutcomeV1::Unavailable
            }
            ApplicationProblemKind::Saturated => Plan26FeedbackOutcomeV1::AtCapacity,
            ApplicationProblemKind::Cancelled => Plan26FeedbackOutcomeV1::Cancelled,
            ApplicationProblemKind::TimedOut => Plan26FeedbackOutcomeV1::TimedOut,
        },
        DaemonInvocationOutcome::Problem { problem } => match problem {
            DaemonInvocationProblem::InvalidRequest
            | DaemonInvocationProblem::UnsupportedRevision => Plan26FeedbackOutcomeV1::Rejected,
            DaemonInvocationProblem::NotFoundOrNotAuthorized => Plan26FeedbackOutcomeV1::Denied,
            DaemonInvocationProblem::Unavailable => Plan26FeedbackOutcomeV1::Unavailable,
        },
    }
}

fn plan26_rejected_argument(
    response: &DaemonInvocationResponse,
) -> Option<(Plan26RejectedArgumentV1, Plan26ArgumentRejectionClassV1)> {
    match &response.outcome {
        DaemonInvocationOutcome::Problem { problem } => {
            plan26_invocation_problem_rejected_argument(*problem)
        }
        DaemonInvocationOutcome::ApplicationProblem { problem }
            if problem.kind() == ApplicationProblemKind::InvalidRequest =>
        {
            Some((
                Plan26RejectedArgumentV1::RequestBody,
                Plan26ArgumentRejectionClassV1::InvalidShape,
            ))
        }
        _ => None,
    }
}

const fn plan26_invocation_problem_rejected_argument(
    problem: DaemonInvocationProblem,
) -> Option<(Plan26RejectedArgumentV1, Plan26ArgumentRejectionClassV1)> {
    match problem {
        DaemonInvocationProblem::InvalidRequest => Some((
            Plan26RejectedArgumentV1::RequestBody,
            Plan26ArgumentRejectionClassV1::InvalidShape,
        )),
        DaemonInvocationProblem::UnsupportedRevision => Some((
            Plan26RejectedArgumentV1::Lifecycle,
            Plan26ArgumentRejectionClassV1::Unsupported,
        )),
        DaemonInvocationProblem::NotFoundOrNotAuthorized | DaemonInvocationProblem::Unavailable => {
            None
        }
    }
}

fn observe_plan26_invocation_response(
    observations: Option<&Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>>,
    subject: Option<&ManifestDigest>,
    operation: DaemonInvocationOperation,
    route: Option<Plan26DeliveryRouteV1>,
    started_at: UtcMicros,
    response: &DaemonInvocationResponse,
) {
    let observed_at = current_micros();
    let outcome = plan26_response_outcome(response);
    let duration_micros = u64::try_from(observed_at.0.saturating_sub(started_at.0)).ok();
    if let Some(route) = route {
        emit_plan26_invocation_event(
            observations,
            subject,
            observed_at,
            Plan26FeedbackSourceEventV1::Delivery {
                operation: plan26_feedback_operation(operation),
                route,
                outcome,
                item_count: match &response.outcome {
                    DaemonInvocationOutcome::Feedback { result, .. }
                    | DaemonInvocationOutcome::Primitive { result, .. }
                    | DaemonInvocationOutcome::CallableCode { result, .. } => {
                        result.page.returned.try_into().unwrap_or(u32::MAX)
                    }
                    _ => 0,
                },
                duration_micros,
            },
        );
    }
    if matches!(
        outcome,
        Plan26FeedbackOutcomeV1::Cancelled | Plan26FeedbackOutcomeV1::TimedOut
    ) {
        emit_plan26_invocation_event(
            observations,
            subject,
            observed_at,
            Plan26FeedbackSourceEventV1::Cancellation {
                operation: plan26_feedback_operation(operation),
                outcome,
            },
        );
    }
    if let Some((argument, rejection)) = plan26_rejected_argument(response) {
        emit_plan26_invocation_event(
            observations,
            subject,
            observed_at,
            Plan26FeedbackSourceEventV1::SurfaceArgumentRejected {
                operation: plan26_feedback_operation(operation),
                route,
                argument,
                rejection,
                schema_revision: 1,
                outcome,
            },
        );
    }
    if let DaemonInvocationOutcome::Feedback { result, .. }
    | DaemonInvocationOutcome::Primitive { result, .. }
    | DaemonInvocationOutcome::CallableCode { result, .. } = &response.outcome
    {
        let omitted = result.page.total.map_or_else(
            || u64::from(result.page.cursor.is_some()),
            |total| total.saturating_sub(result.page.returned),
        );
        if omitted > 0 || result.page.cursor.is_some() {
            emit_plan26_invocation_event(
                observations,
                subject,
                observed_at,
                Plan26FeedbackSourceEventV1::Truncation {
                    operation: plan26_feedback_operation(operation),
                    returned_count: result.page.returned.try_into().unwrap_or(u32::MAX),
                    omitted_count: omitted.try_into().unwrap_or(u32::MAX),
                },
            );
        }
    }
    if operation == DaemonInvocationOperation::FeedbackExpand {
        emit_plan26_invocation_event(
            observations,
            subject,
            observed_at,
            Plan26FeedbackSourceEventV1::AnchorExpansion {
                operation: Plan26AnchorOperationV1::HandleExpansion,
                outcome,
                returned_count: match &response.outcome {
                    DaemonInvocationOutcome::Feedback { result, .. } => {
                        result.page.returned.try_into().unwrap_or(u32::MAX)
                    }
                    _ => 0,
                },
                duration_micros,
            },
        );
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn current_micros() -> UtcMicros {
    UtcMicros(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| i64::try_from(duration.as_micros()).unwrap_or(i64::MAX))
            .unwrap_or_default(),
    )
}

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

#[cfg(test)]
mod tests {
    use super::*;

    struct MintedAdvisoryCycleHandle(&'static str, Pr13AdvisoryCycleTerminalV1);

    impl MintedAdvisoryCycleHandle {
        fn incomplete(request_handle: &'static str) -> Self {
            Self(
                request_handle,
                Pr13AdvisoryCycleTerminalV1 {
                    termination: FeedbackCycleTerminationV1::IncompleteCoverage,
                    provider_states: vec![
                        ProviderEvaluationStateV1::Unavailable,
                        ProviderEvaluationStateV1::SupportedCompletedComplete,
                    ],
                    published: false,
                },
            )
        }
    }

    impl Pr13AdvisoryCycleInvocationPortV1 for MintedAdvisoryCycleHandle {
        fn invoke(
            &self,
            _request: Pr13AdvisoryCycleInvocationRequestV1,
        ) -> Pr13AdvisoryCycleInvocationFutureV1<'_> {
            Box::pin(async {
                Ok(Pr13AdvisoryCycleInvocationOutcomeV1 {
                    request_handle: self.0.to_owned(),
                    cycle: self.1.clone(),
                })
            })
        }
    }

    struct RecordingFeedbackHandle(Arc<std::sync::Mutex<Vec<String>>>);

    impl DaemonFeedbackInvocationPort for RecordingFeedbackHandle {
        fn invoke(
            &self,
            request: DaemonFeedbackInvocationRequest,
        ) -> DaemonFeedbackInvocationFuture<'_> {
            self.0
                .lock()
                .expect("recorded feedback handles")
                .push(request.request_handle);
            Box::pin(async {
                Err(ApplicationProblem::not_found_or_not_authorized(
                    RetryDirective::Never,
                ))
            })
        }
    }

    #[test]
    fn advisory_cycle_wire_request_has_no_client_selected_handle() {
        let request = DaemonInvocationRequest::feedback_advisory_cycle(
            "request.feedback-cycle",
            "file:///project/src/lib.rs".to_owned(),
            UtcMicros(1),
            Deadline::new(UtcMicros(2)).expect("deadline"),
            CancellationContext::active("cancel.feedback-cycle").expect("cancellation"),
        );
        let value = serde_json::to_value(request).expect("wire request");

        assert_eq!(value["operation"], "feedback_advisory_cycle");
        assert_eq!(value["document_uri"], "file:///project/src/lib.rs");
        assert!(value.get("request_handle").is_none());
    }

    #[test]
    fn advisory_cycle_terminal_state_keeps_incomplete_coverage_explicit() {
        let terminal = MintedAdvisoryCycleHandle::incomplete("rh.daemon.minted").1;
        let value = serde_json::to_value(&terminal).expect("advisory cycle terminal");

        assert_eq!(value["termination"], "incomplete_coverage");
        assert_eq!(value["published"], false);
        assert_eq!(
            value["provider_states"],
            serde_json::json!(["unavailable", "supported_completed_complete"])
        );
    }

    #[tokio::test]
    async fn advisory_cycle_reads_diagnostics_with_daemon_minted_handle() {
        let handles = Arc::new(std::sync::Mutex::new(Vec::new()));
        let project_id = ProjectId::new("project.feedback-cycle").expect("project");
        let response = execute_feedback_advisory_cycle(
            "request.feedback-cycle".to_owned(),
            Some(Arc::new(MintedAdvisoryCycleHandle::incomplete(
                "rh.daemon.minted",
            ))),
            Some(DaemonFeedbackInvocationOwner::new(
                project_id,
                Arc::new(RecordingFeedbackHandle(Arc::clone(&handles))),
            )),
            "file:///project/src/lib.rs".to_owned(),
            UtcMicros(1),
            Deadline::new(UtcMicros(2)).expect("deadline"),
            CancellationContext::active("cancel.feedback-cycle").expect("cancellation"),
        )
        .await;

        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::ApplicationProblem {
                problem: ApplicationProblem::NotFoundOrNotAuthorized { .. }
            }
        ));
        assert_eq!(
            handles
                .lock()
                .expect("recorded feedback handles")
                .as_slice(),
            ["rh.daemon.minted"]
        );
    }

    #[test]
    fn direct_configuration_grants_reject_foreign_caller_selected_layers() {
        let exact_project = tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
            project_id: ProjectId::new("project.configuration.exact").expect("project"),
        };
        let exact_profile = tracedecay_domain::configuration::ConfigurationLayerIdV1::UserProfile {
            profile_id: tracedecay_domain::UserProfileId::new("profile.configuration.exact")
                .expect("profile"),
        };
        let exact_collection =
            tracedecay_domain::configuration::ConfigurationLayerIdV1::Collection {
                collection_id: tracedecay_domain::QueryCollectionId::new(
                    "collection.configuration.exact",
                )
                .expect("collection"),
            };
        let authority = DaemonConfigurationGrantAuthority::for_test(
            [
                exact_project.clone(),
                exact_profile.clone(),
                exact_collection.clone(),
            ],
            UtcMicros(100),
        );
        let expected_revision =
            ConfigurationRevisionId::new("configuration.revision.exact").expect("revision");

        for (index, layer) in [exact_project, exact_profile, exact_collection]
            .into_iter()
            .enumerate()
        {
            let mutation = DirectConfigurationMutation::Unset {
                layer,
                key: tracedecay_domain::configuration::SettingKey::new("sync.auto_watch")
                    .expect("setting"),
            };
            assert!(
                authority
                    .issue_direct(
                        &format!("request.configuration.exact.{index}"),
                        &mutation,
                        expected_revision.clone(),
                        UtcMicros(1),
                    )
                    .is_ok()
            );
        }

        for (index, layer) in [
            tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
                project_id: ProjectId::new("project.configuration.foreign").expect("project"),
            },
            tracedecay_domain::configuration::ConfigurationLayerIdV1::UserProfile {
                profile_id: tracedecay_domain::UserProfileId::new("profile.configuration.foreign")
                    .expect("profile"),
            },
            tracedecay_domain::configuration::ConfigurationLayerIdV1::Collection {
                collection_id: tracedecay_domain::QueryCollectionId::new(
                    "collection.configuration.foreign",
                )
                .expect("collection"),
            },
        ]
        .into_iter()
        .enumerate()
        {
            let foreign = DirectConfigurationMutation::Unset {
                layer,
                key: tracedecay_domain::configuration::SettingKey::new("sync.auto_watch")
                    .expect("setting"),
            };
            assert!(matches!(
                authority.issue_direct(
                    &format!("request.configuration.foreign.{index}"),
                    &foreign,
                    expected_revision.clone(),
                    UtcMicros(1),
                ),
                Err(DaemonInvocationProblem::NotFoundOrNotAuthorized)
            ));
        }
    }

    #[test]
    fn mounted_configuration_layers_exclude_stale_collection_provenance() {
        use tracedecay_domain::configuration::{
            CandidateDispositionV1, ConfigurationCandidateV1, ConfigurationSnapshotV1,
            ConfigurationValueV1,
        };

        let project_id = ProjectId::new("project.configuration.mounted").expect("project");
        let profile_id = tracedecay_domain::UserProfileId::new("profile.configuration.mounted")
            .expect("profile");
        let winning = tracedecay_domain::QueryCollectionId::new("collection.configuration.winning")
            .expect("collection");
        let overridden =
            tracedecay_domain::QueryCollectionId::new("collection.configuration.overridden")
                .expect("collection");
        let rejected =
            tracedecay_domain::QueryCollectionId::new("collection.configuration.rejected")
                .expect("collection");
        let key =
            tracedecay_domain::configuration::SettingKey::new("sync.auto_watch").expect("setting");
        let revision =
            ConfigurationRevisionId::new("configuration.revision.mounted").expect("revision");
        let candidate = |collection_id, disposition| ConfigurationCandidateV1 {
            layer: ConfigurationLayerIdV1::Collection { collection_id },
            revision_id: revision.clone(),
            disposition,
            safe_reason: None,
        };
        let snapshot = ConfigurationSnapshotV1::new(
            BTreeMap::from([(key.clone(), ConfigurationValueV1::Boolean(true))]),
            BTreeMap::from([(
                key,
                vec![
                    candidate(winning.clone(), CandidateDispositionV1::Winning),
                    candidate(overridden.clone(), CandidateDispositionV1::Overridden),
                    candidate(rejected.clone(), CandidateDispositionV1::Rejected),
                ],
            )]),
        )
        .expect("snapshot");

        let mounted =
            mounted_configuration_layers(&project_id, &profile_id, &snapshot).expect("layers");
        let contains = |layer: ConfigurationLayerIdV1| {
            let digest = configuration_layer_scope_digest(&layer).expect("digest");
            mounted.get(&digest) == Some(&layer)
        };
        assert!(contains(ConfigurationLayerIdV1::Collection {
            collection_id: winning,
        }));
        assert!(!contains(ConfigurationLayerIdV1::Collection {
            collection_id: overridden,
        }));
        assert!(!contains(ConfigurationLayerIdV1::Collection {
            collection_id: rejected,
        }));
    }

    #[test]
    fn git_read_packet_binds_catalog_authority_and_native_coverage() {
        let scope = ResolvedScope::new(
            ProjectId::new("project.git-read-packet").expect("project"),
            tracedecay_domain::RepositoryId::new("repository.git-read-packet").expect("repository"),
            tracedecay_domain::WorktreeId::new("worktree.git-read-packet").expect("worktree"),
            Some(tracedecay_domain::RefId::new("refs/heads/main").expect("reference")),
        )
        .expect("scope");
        let request = crate::application::git_reads::GitReadRequestV1::Status;
        let capability = tracedecay_tool_catalog::CapabilityId::new(request.capability_id())
            .expect("capability");
        let digest =
            || ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("manifest digest");
        let authority = DaemonGitAuthorityStateV1 {
            scope: scope.clone(),
            requester: ActorId::new("actor.git-read-packet").expect("actor"),
            effective_capabilities: std::collections::BTreeSet::from([capability]),
            grant_expires_at: UtcMicros(i64::MAX),
            policy_revision: 1,
            policy_digest: digest(),
            configuration_digest: digest(),
            catalog_digest: digest(),
            privacy_digest: digest(),
            evaluated_at: UtcMicros(1),
        };
        let result = crate::application::git_reads::GitReadResultV1::Status(
            crate::git_query::GitQueryEnvelopeV1 {
                value: crate::git_query::GitStatusSummaryV1 {
                    repository: scope.repository_id.clone(),
                    head: GitHeadStateV1::Unborn {
                        branch: "refs/heads/main".to_owned(),
                    },
                    operation: tracedecay_domain::git::GitOperationStateV1::None,
                    staged: 0,
                    unstaged: 0,
                    conflicted: 0,
                    untracked: 0,
                    ignored: 0,
                    changed_paths: Vec::new(),
                    schema_version: crate::git_query::GIT_QUERY_SCHEMA_VERSION_V1.to_owned(),
                },
                coverage: tracedecay_domain::git::GitCoverageV1::complete(),
                truncated_by_bound: false,
            },
        );

        let packet = git_read_evidence_packet(
            "request.git-read-packet",
            &request,
            &authority,
            result,
            UtcMicros(2),
            Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
            CancellationContext::active("cancel.git-read-packet").expect("cancellation"),
        )
        .expect("Git read packet");

        assert_eq!(packet.authority.authorized_scope_digest, scope.scope_digest);
        assert_eq!(
            packet.coverage.completeness,
            tracedecay_application::CoverageCompleteness::Complete
        );
        assert_eq!(packet.page.returned, 1);
        assert!(packet.payload.is_some());
        assert!(matches!(
            packet.evidence_authorities.as_slice(),
            [EvidenceAuthority { source_kind, .. }] if source_kind == "native_git"
        ));
        let complete_evidence_id = packet.evidence_authorities[0].evidence_id.clone();

        let partial = git_read_evidence_packet(
            "request.git-read-packet-partial",
            &request,
            &authority,
            crate::application::git_reads::GitReadResultV1::Status(
                crate::git_query::GitQueryEnvelopeV1 {
                    value: crate::git_query::GitStatusSummaryV1 {
                        repository: scope.repository_id,
                        head: GitHeadStateV1::Unborn {
                            branch: "refs/heads/main".to_owned(),
                        },
                        operation: tracedecay_domain::git::GitOperationStateV1::None,
                        staged: 0,
                        unstaged: 0,
                        conflicted: 0,
                        untracked: 0,
                        ignored: 0,
                        changed_paths: Vec::new(),
                        schema_version: crate::git_query::GIT_QUERY_SCHEMA_VERSION_V1.to_owned(),
                    },
                    coverage: tracedecay_domain::git::GitCoverageV1::degraded(vec![
                        tracedecay_domain::git::GitDegradationV1::TruncatedOutput,
                        tracedecay_domain::git::GitDegradationV1::ConflictedState,
                    ]),
                    truncated_by_bound: true,
                },
            ),
            UtcMicros(3),
            Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
            CancellationContext::active("cancel.git-read-packet-partial").expect("cancellation"),
        )
        .expect("partial Git read packet");
        assert_eq!(
            partial.coverage.completeness,
            tracedecay_application::CoverageCompleteness::Partial
        );
        assert!(matches!(
            partial.omissions.as_slice(),
            [
                Omission {
                    domain: EvidenceDomain::Source,
                    reason: OmissionReason::Budget,
                    ..
                },
                Omission {
                    domain: EvidenceDomain::Source,
                    reason: OmissionReason::Conflict,
                    ..
                }
            ]
        ));
        assert_ne!(
            partial.evidence_authorities[0].evidence_id, complete_evidence_id,
            "native Git evidence identity must bind the captured result"
        );
    }

    #[derive(Default)]
    struct RecordingFeedbackCycleObservations(std::sync::Mutex<Vec<Plan26FeedbackSourceEventV1>>);

    impl Plan26FeedbackObservationEmitterV1 for RecordingFeedbackCycleObservations {
        fn observe_source_event(
            &self,
            _input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
            source_event: Plan26FeedbackSourceEventV1,
        ) {
            self.0.lock().expect("observations").push(source_event);
        }

        fn observe_source_event_for_subject(
            &self,
            _subject_digest: ManifestDigest,
            _observed_at: UtcMicros,
            source_event: Plan26FeedbackSourceEventV1,
        ) {
            self.0.lock().expect("observations").push(source_event);
        }
    }

    fn unavailable_feedback_cycle(
        observations: Arc<RecordingFeedbackCycleObservations>,
    ) -> UnavailableFeedbackCycleRuntimeV1 {
        UnavailableFeedbackCycleRuntimeV1::new(
            ProjectId::new("project.feedback-cycle-unavailable").expect("project"),
            observations,
        )
    }

    struct UnavailableDiagnosticAuthority;

    impl CanonicalDiagnosticSnapshotAuthority for UnavailableDiagnosticAuthority {
        fn refresh(
            &self,
            _request: CanonicalDiagnosticRefreshRequest,
        ) -> LspRuntimeFuture<Result<GenerationDiagnostics, LspRuntimeFailure>> {
            Box::pin(async { Err(LspRuntimeFailure::new("test-diagnostics-unavailable")) })
        }
    }

    struct UnavailableCancellationAuthority;

    impl LspAnalyzerCancellationAuthority for UnavailableCancellationAuthority {
        fn cancel_request(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
            false
        }
    }

    struct UnavailableContextAuthority;

    impl CanonicalContextProjectionAuthority for UnavailableContextAuthority {
        fn registrations(&self) -> Vec<ContextProjectionRegistration> {
            Vec::new()
        }

        fn snapshot(
            &self,
            _root: AdmittedRoot,
            _request_id: LspRequestId,
            _request: ContextProjectionRequest,
        ) -> LspRuntimeFuture<ContextProjectionOutcome> {
            Box::pin(async { ContextProjectionOutcome::Unsupported })
        }
    }

    fn unavailable_lsp_session_factory() -> Arc<DaemonLspSessionFactory> {
        Arc::new(DaemonLspSessionFactory::new(
            tokio::runtime::Handle::current(),
            Arc::new(unavailable_feedback_cycle(Arc::new(
                RecordingFeedbackCycleObservations::default(),
            ))),
            Arc::new(UnavailableSemanticProvider),
            Arc::new(UnavailableDiagnosticAuthority),
            Arc::new(UnavailableCancellationAuthority),
            Arc::new(UnavailableContextAuthority),
            GatewayCapabilities::default(),
            UpstreamCapabilities::default(),
        ))
    }

    struct CountingFeedbackCycle(Arc<std::sync::atomic::AtomicUsize>);

    impl FeedbackCycleRuntimePort for CountingFeedbackCycle {
        fn execute(
            &self,
            _request: FeedbackCycleRequest,
        ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
            let calls = Arc::clone(&self.0);
            Box::pin(async move {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn feedback_cycle_router_upgrades_existing_lsp_sessions_to_advisory_runtime() {
        let proximity_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let advisory_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observations = Arc::new(RecordingFeedbackCycleObservations::default());
        let router = SwitchableFeedbackCycleRuntimeV1::new(Arc::new(unavailable_feedback_cycle(
            Arc::clone(&observations),
        )));
        let request = FeedbackCycleRequest {
            root_uri: "file:///project".to_owned(),
            document_uri: "file:///project/src/lib.rs".to_owned(),
            trigger: DiagnosticTrigger::DocumentSave,
        };

        assert!(router.execute(request.clone()).await.is_err());
        assert!(matches!(
            observations.0.lock().expect("observations").as_slice(),
            [Plan26FeedbackSourceEventV1::Delivery {
                operation: Plan26FeedbackOperationV1::FeedbackCycle,
                route: Plan26DeliveryRouteV1::Lsp,
                outcome: Plan26FeedbackOutcomeV1::Unavailable,
                item_count: 0,
                ..
            }]
        ));
        router
            .replace(Arc::new(CountingFeedbackCycle(Arc::clone(
                &proximity_calls,
            ))))
            .unwrap();
        router.execute(request.clone()).await.unwrap();
        router
            .replace(Arc::new(CountingFeedbackCycle(Arc::clone(&advisory_calls))))
            .unwrap();
        router.execute(request).await.unwrap();

        assert_eq!(proximity_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(advisory_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    fn hook_envelope(event: HookEventV2) -> HookEventEnvelopeV2 {
        HookEventEnvelopeV2 {
            schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
            event_id: [1; 16],
            producer: tracedecay_hooks::HookHostV1::Codex,
            protected_session_id: [2; 32],
            project_id: [3; 16],
            repository_id: [4; 16],
            worktree_id: [5; 16],
            worktree_epoch: 1,
            binding_token: [6; 32],
            ordering: tracedecay_hooks::HookOrderingV1::Unknown,
            observed_at: UtcMicros(1),
            event,
        }
    }

    fn hook_binding() -> HookScopeBindingV1 {
        HookScopeBindingV1 {
            host: tracedecay_hooks::HookHostV1::Codex,
            project_id: [3; 16],
            repository_id: [4; 16],
            worktree_id: [5; 16],
            worktree_epoch: 1,
            binding_token: [6; 32],
            capabilities: [
                tracedecay_hooks::HookEventFamily::SessionBoundary,
                tracedecay_hooks::HookEventFamily::PromptBoundary,
                tracedecay_hooks::HookEventFamily::ToolLifecycle,
                tracedecay_hooks::HookEventFamily::SavedEdit,
                tracedecay_hooks::HookEventFamily::TestLifecycle,
            ]
            .into_iter()
            .map(|family| tracedecay_hooks::HookCapabilityV1 {
                family,
                support: tracedecay_hooks::stock_event_support(
                    tracedecay_hooks::HookHostV1::Codex,
                    family,
                ),
            })
            .collect(),
        }
    }

    fn hook_lifecycle() -> ContextScoutLifecycleAddressV1 {
        ContextScoutLifecycleAddressV1 {
            profile_id: tracedecay_domain::UserProfileId::new("profile.pr13-hook").unwrap(),
            provider_id: tracedecay_domain::ProviderId::new("codex").unwrap(),
            project_id: ProjectId::new("project.pr13-hook").unwrap(),
            worktree_id: tracedecay_domain::WorktreeId::new("worktree.pr13-hook").unwrap(),
            session_id: tracedecay_domain::SessionId::new("session.pr13-hook").unwrap(),
            thread_id: tracedecay_domain::ThreadId::new("thread.pr13-hook").unwrap(),
            turn_id: tracedecay_domain::TurnId::new("turn.pr13-hook").unwrap(),
            agent_id: tracedecay_domain::AgentInstanceId::new("agent.pr13-hook").unwrap(),
            logical_message_id: tracedecay_domain::MessageId::new("message.pr13-hook").unwrap(),
        }
    }

    #[test]
    fn pr13_hook_orchestration_admits_only_saved_edit_stop_and_explicit() {
        let saved = Pr13HookOrchestrationRequestV1::from_envelope(
            hook_envelope(HookEventV2::SavedEdit {
                file_id: [7; 16],
                changed_range_count: 1,
            }),
            &hook_binding(),
            Some(hook_lifecycle()),
            1,
            false,
        )
        .unwrap();
        assert_eq!(saved.trigger, Pr13HookOrchestrationTriggerV1::SavedEdit);

        let stop = Pr13HookOrchestrationRequestV1::from_envelope(
            hook_envelope(HookEventV2::SessionBoundary {
                boundary: HookBoundaryV1::TurnComplete,
            }),
            &hook_binding(),
            Some(hook_lifecycle()),
            1,
            false,
        )
        .unwrap();
        assert_eq!(stop.trigger, Pr13HookOrchestrationTriggerV1::Stop);

        let without_scout_lifecycle = Pr13HookOrchestrationRequestV1::from_envelope(
            hook_envelope(HookEventV2::SavedEdit {
                file_id: [7; 16],
                changed_range_count: 1,
            }),
            &hook_binding(),
            None,
            1,
            false,
        )
        .unwrap();
        assert_eq!(
            without_scout_lifecycle.trigger,
            Pr13HookOrchestrationTriggerV1::SavedEdit
        );
        assert!(without_scout_lifecycle.lifecycle.is_none());

        assert!(
            Pr13HookOrchestrationRequestV1::from_envelope(
                hook_envelope(HookEventV2::TestLifecycle {
                    test_run_id: [8; 16],
                    test_count: 1,
                    phase: tracedecay_hooks::HookLifecyclePhaseV1::Completed,
                    receipt_id: Some([9; 16]),
                }),
                &hook_binding(),
                Some(hook_lifecycle()),
                1,
                false,
            )
            .is_none()
        );
        assert_eq!(
            Pr13HookOrchestrationRequestV1::from_envelope(
                hook_envelope(HookEventV2::SessionBoundary {
                    boundary: HookBoundaryV1::Start,
                }),
                &hook_binding(),
                Some(hook_lifecycle()),
                1,
                true,
            )
            .unwrap()
            .trigger,
            Pr13HookOrchestrationTriggerV1::Explicit
        );
    }

    #[tokio::test]
    async fn pr13_hook_orchestration_backpressures_without_waiting() {
        let release = Arc::new(tokio::sync::Notify::new());
        let work_release = Arc::clone(&release);
        let work = move |_| {
            let release = Arc::clone(&work_release);
            async move { release.notified().await }
        };
        let runtime = BoundedPr13HookOrchestratorV1::new(1, work).unwrap();
        let completions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_completions = Arc::clone(&completions);
        let completed = Arc::new(tokio::sync::Notify::new());
        let completion_notification = Arc::clone(&completed);
        let mut request = Pr13HookOrchestrationRequestV1::from_envelope(
            hook_envelope(HookEventV2::SavedEdit {
                file_id: [7; 16],
                changed_range_count: 1,
            }),
            &hook_binding(),
            Some(hook_lifecycle()),
            1,
            false,
        )
        .unwrap();
        request.completion = Some(Arc::new(move || {
            observed_completions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            completion_notification.notify_one();
        }));

        assert_eq!(
            runtime.admit(request.clone()),
            Pr13HookOrchestrationAdmissionV1::Enqueued
        );
        assert_eq!(
            runtime.admit(request),
            Pr13HookOrchestrationAdmissionV1::Backpressured
        );
        assert_eq!(completions.load(std::sync::atomic::Ordering::Relaxed), 0);
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), completed.notified())
            .await
            .expect("producer work completion");
        assert_eq!(
            completions.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "only completed producer work may clear the durable outbox"
        );
    }

    #[tokio::test]
    async fn pr13_hook_orchestration_runs_feedback_work_without_scout_lifecycle() {
        let ran = Arc::new(tokio::sync::Notify::new());
        let work_ran = Arc::clone(&ran);
        let runtime = BoundedPr13HookOrchestratorV1::new(1, move |_| {
            let ran = Arc::clone(&work_ran);
            async move { ran.notify_one() }
        })
        .unwrap();
        let runtime: Arc<dyn Pr13HookOrchestrationPortV1> = runtime;
        pr13_hook_orchestration_registry()
            .lock()
            .unwrap()
            .insert(([3; 16], [5; 16]), Arc::downgrade(&runtime));

        assert_eq!(
            admit_registered_pr13_hook_orchestration(
                hook_envelope(HookEventV2::SavedEdit {
                    file_id: [7; 16],
                    changed_range_count: 1,
                }),
                hook_binding(),
                None,
                1,
                false,
                None,
            ),
            Pr13HookOrchestrationAdmissionV1::Enqueued
        );
        ran.notified().await;
        pr13_hook_orchestration_registry()
            .lock()
            .unwrap()
            .remove(&([3; 16], [5; 16]));
    }

    #[test]
    fn only_explicit_protocol_frames_select_the_invocation_route() {
        assert!(parse_daemon_invocation_request(r#"{"jsonrpc":"2.0","method":"ping"}"#).is_none());
        let request = DaemonInvocationRequest::lsp_open(
            "request.1",
            "client.1",
            Some("file:///untrusted".to_owned()),
            Vec::new(),
        );
        let encoded = serde_json::to_string(&request).expect("encode request");
        assert!(matches!(
            parse_daemon_invocation_request(&encoded),
            Some(Ok(_))
        ));
    }

    #[test]
    fn test_results_invocation_retains_the_transport_page() {
        let page = PageRequest::first(17).expect("page");
        let request = DaemonInvocationRequest::primitive(
            "request.test-results.page",
            crate::application_surface::ApplicationSurfaceOperation::TestResults,
            Pr12PrimitiveRequest::RecentTestResults(page.clone()),
            UtcMicros(1),
            Deadline::new(UtcMicros(2)).expect("deadline"),
            CancellationContext::active("cancel.test-results.page").expect("cancellation"),
        );
        let encoded = serde_json::to_string(&request).expect("encode request");
        let decoded = parse_daemon_invocation_request(&encoded)
            .expect("daemon protocol")
            .expect("valid request");
        let DaemonInvocationPayload::PrimitiveTestResults {
            page: decoded_page, ..
        } = decoded.payload
        else {
            panic!("test-results request must retain its typed payload");
        };
        assert_eq!(decoded_page, page);
    }

    #[tokio::test]
    async fn semantic_scheduler_is_daemon_private_retained_state_not_a_wire_operation() {
        let service = DaemonInvocationService::default();
        let registrar = DaemonSemanticRuntimeRegistrar::new(&service);
        let project_root = PathBuf::from("/project/semantic-runtime");
        let handle = crate::semantic_code::DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20)
            .expect("semantic scheduler");

        registrar
            .register(project_root.clone(), handle.clone())
            .await
            .expect("mount semantic scheduler");
        assert_eq!(
            service
                .semantic_runtime(Some(&project_root))
                .await
                .expect("retained semantic scheduler")
                .status(),
            crate::semantic_code::SemanticRuntimeScheduleStatusV1::Unavailable
        );
        assert!(matches!(
            registrar.register(project_root, handle).await,
            Err(DaemonSemanticRuntimeRegistrationError::AlreadyRegistered)
        ));
        assert!(
            serde_json::to_string(&DaemonInvocationOperation::LspOpen)
                .expect("serialize existing operation")
                .find("semantic")
                .is_none(),
            "semantic scheduling must not add a public daemon operation"
        );
    }

    #[tokio::test]
    async fn context_scout_registry_remounts_same_project_database_after_daemon_restart() {
        let temporary = tempfile::tempdir().unwrap();
        let database_path = temporary.path().join("graph.db");
        let authority = crate::db::DatabaseAuthority::acquire_test(
            &database_path,
            "daemon Context Scout registry",
        )
        .unwrap();
        let database = Database::publish_test_runtime(
            &database_path,
            &authority,
            crate::db::TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap()
        .0;
        let project_id = ProjectId::new("project.scout.daemon-restart").unwrap();

        let first_service = DaemonInvocationService::default();
        let first_registrar = DaemonContextScoutRuntimeRegistrar::new(&first_service);
        let first = first_registrar
            .open_and_register(database.clone(), project_id.clone())
            .await
            .unwrap();
        assert!(Arc::ptr_eq(
            &first,
            &first_registrar.get(&project_id).await.unwrap()
        ));
        first_service.expire_all().await;
        assert!(first_registrar.get(&project_id).await.is_none());

        let restarted_service = DaemonInvocationService::default();
        let restarted_registrar = DaemonContextScoutRuntimeRegistrar::new(&restarted_service);
        let restarted = restarted_registrar
            .open_and_register(database.clone(), project_id.clone())
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &restarted));
        assert!(Arc::ptr_eq(
            &restarted,
            &restarted_registrar.get(&project_id).await.unwrap()
        ));
        assert!(matches!(
            restarted_registrar
                .open_and_register(database, project_id)
                .await,
            Err(DaemonContextScoutRuntimeRegistrationError::AlreadyRegistered)
        ));
    }

    #[test]
    fn feedback_invocation_preserves_transport_deadline_and_cancellation() {
        let deadline = Deadline::new(UtcMicros(90)).expect("deadline");
        let cancellation =
            CancellationContext::cancelled("cancel.feedback.transport", UtcMicros(40))
                .expect("cancellation");
        let request = DaemonInvocationRequest::feedback(
            "request.feedback.transport",
            crate::application_surface::ApplicationSurfaceOperation::FeedbackList,
            "feedback-handle.transport".to_owned(),
            UtcMicros(30),
            deadline.clone(),
            cancellation.clone(),
        );

        assert!(matches!(
            request.payload,
            DaemonInvocationPayload::FeedbackList {
                observed_at: UtcMicros(30),
                deadline: carried_deadline,
                cancellation: carried_cancellation,
                ..
            } if carried_deadline == deadline && carried_cancellation == cancellation
        ));
    }

    #[test]
    fn callable_code_invocation_preserves_typed_request_and_transport_controls() {
        let deadline = Deadline::new(UtcMicros(90)).expect("deadline");
        let cancellation =
            CancellationContext::cancelled("cancel.callable-code.transport", UtcMicros(40))
                .expect("cancellation");
        let phrase = crate::application_surface::CodePhraseSearchSurfaceRequest {
            query: "daemon invocation".to_owned(),
            phrases: vec!["daemon invocation".to_owned()],
            field_filters: vec![tracedecay_application::retrieval::CodeLexicalFieldFilter {
                field: tracedecay_application::retrieval::CodeLexicalField::Path,
                include: true,
            }],
            fuzzy_budget: 7,
            scope: tracedecay_application::CodeQueryScope::new(
                tracedecay_domain::CodeGenerationId::new("generation.callable-code")
                    .expect("generation"),
                Some("src/daemon".to_owned()),
            )
            .expect("scope"),
            meta: crate::application_surface::CallableCodeSurfaceMeta {
                projection: tracedecay_application::ResultProjection::Evidence,
                order: tracedecay_application::RetrievalOrder::Relevance,
                cursor: None,
            },
        };
        let page = tracedecay_application::PageRequest::first(16).expect("page");
        let canonical = phrase
            .clone()
            .into_application_request(
                crate::daemon::code_index_scheduler::queries::callable_query_sanitizer_revision(),
                crate::daemon::code_index_scheduler::queries::callable_query_normalization_revision(
                ),
                page.clone(),
            )
            .expect("canonical phrase request");
        assert_eq!(
            canonical.query.sanitizer_revision().as_str(),
            "query-sanitizer.daemon.v1"
        );
        assert_eq!(
            canonical.query.normalization_revision().as_str(),
            "query-normalization.daemon.v1"
        );
        assert_eq!(
            canonical.field_filters,
            [tracedecay_application::retrieval::CodeLexicalFieldFilter {
                field: tracedecay_application::retrieval::CodeLexicalField::Path,
                include: true,
            }]
        );
        assert_eq!(canonical.fuzzy_budget, 7);
        let request = crate::application_surface::CallableCodeSurfaceRequest::PhraseSearch(phrase);
        let invocation = DaemonInvocationRequest::callable_code(
            "request.callable-code.transport",
            crate::application_surface::ApplicationSurfaceOperation::CodePhraseSearch,
            request,
            page.clone(),
            UtcMicros(30),
            deadline.clone(),
            cancellation.clone(),
        );

        assert_eq!(
            invocation.operation(),
            DaemonInvocationOperation::CodePhraseSearch
        );
        assert!(matches!(
            invocation.payload,
            DaemonInvocationPayload::CallableCode {
                surface_operation:
                    crate::application_surface::ApplicationSurfaceOperation::CodePhraseSearch,
                request:
                    crate::application_surface::CallableCodeSurfaceRequest::PhraseSearch(
                        crate::application_surface::CodePhraseSearchSurfaceRequest {
                            query,
                            phrases,
                            ..
                        }
                    ),
                page: carried_page,
                observed_at: UtcMicros(30),
                deadline: carried_deadline,
                cancellation: carried_cancellation,
            } if query == "daemon invocation"
                && phrases == ["daemon invocation"]
                && carried_page == page
                && carried_deadline == deadline
                && carried_cancellation == cancellation
        ));
    }

    #[test]
    fn callable_code_validation_accepts_only_matching_operation_request_pairs() {
        let scope = tracedecay_application::CodeQueryScope::new(
            tracedecay_domain::CodeGenerationId::new("generation.callable-code")
                .expect("generation"),
            None,
        )
        .expect("scope");
        let meta = crate::application_surface::CallableCodeSurfaceMeta {
            projection: tracedecay_application::ResultProjection::Evidence,
            order: tracedecay_application::RetrievalOrder::Relevance,
            cursor: None,
        };
        #[derive(Clone, Copy)]
        enum RequestCase {
            ExactOccurrence,
            PhraseSearch,
            Callees,
            Facets,
            Timeline,
            Declaration,
            Definition,
            TypeDefinition,
            References,
        }
        let navigation = |node_id: &str| crate::application_surface::CodeNavigationSurfaceRequest {
            node_id: node_id.to_owned(),
            scope: scope.clone(),
            meta: meta.clone(),
        };
        let request = |case| match case {
            RequestCase::ExactOccurrence => {
                crate::application_surface::CallableCodeSurfaceRequest::ExactOccurrence(
                    crate::application_surface::CodeExactOccurrenceSurfaceRequest {
                        literal: "CallableCode".to_owned(),
                        kind: None,
                        scope: scope.clone(),
                        meta: meta.clone(),
                    },
                )
            }
            RequestCase::PhraseSearch => {
                crate::application_surface::CallableCodeSurfaceRequest::PhraseSearch(
                    crate::application_surface::CodePhraseSearchSurfaceRequest {
                        query: "callable code".to_owned(),
                        phrases: vec!["callable code".to_owned()],
                        field_filters: Vec::new(),
                        fuzzy_budget: 0,
                        scope: scope.clone(),
                        meta: meta.clone(),
                    },
                )
            }
            RequestCase::Callees => {
                crate::application_surface::CallableCodeSurfaceRequest::Callees(
                    crate::application_surface::CodeCalleesSurfaceRequest {
                        node_id: "node.callable-code".to_owned(),
                        maximum_depth: 1,
                        resolve_trait_dispatch: false,
                        scope: scope.clone(),
                        meta: meta.clone(),
                    },
                )
            }
            RequestCase::Facets => crate::application_surface::CallableCodeSurfaceRequest::Facets(
                crate::application_surface::CodeFacetSurfaceRequest {
                    dimension: tracedecay_application::retrieval::CodeFacetDimension::Kind,
                    scope: scope.clone(),
                    meta: meta.clone(),
                },
            ),
            RequestCase::Timeline => {
                crate::application_surface::CallableCodeSurfaceRequest::Timeline(
                    crate::application_surface::CodeTimelineSurfaceRequest {
                        scope: scope.clone(),
                        meta: meta.clone(),
                    },
                )
            }
            RequestCase::Declaration => {
                crate::application_surface::CallableCodeSurfaceRequest::Declaration(navigation(
                    "node.declaration",
                ))
            }
            RequestCase::Definition => {
                crate::application_surface::CallableCodeSurfaceRequest::Definition(navigation(
                    "node.definition",
                ))
            }
            RequestCase::TypeDefinition => {
                crate::application_surface::CallableCodeSurfaceRequest::TypeDefinition(navigation(
                    "node.type-definition",
                ))
            }
            RequestCase::References => {
                crate::application_surface::CallableCodeSurfaceRequest::References(navigation(
                    "node.references",
                ))
            }
        };
        let cases = [
            (
                crate::application_surface::ApplicationSurfaceOperation::CodeExactOccurrence,
                RequestCase::ExactOccurrence,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::CodePhraseSearch,
                RequestCase::PhraseSearch,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::CodeCallees,
                RequestCase::Callees,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::CodeFacets,
                RequestCase::Facets,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::CodeTimeline,
                RequestCase::Timeline,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::CodeDeclaration,
                RequestCase::Declaration,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::CodeDefinition,
                RequestCase::Definition,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::CodeTypeDefinition,
                RequestCase::TypeDefinition,
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::CodeReferences,
                RequestCase::References,
            ),
        ];
        let page = tracedecay_application::PageRequest::first(16).expect("page");
        let deadline = Deadline::new(UtcMicros(90)).expect("deadline");
        let cancellation =
            CancellationContext::active("cancel.callable-code.matrix").expect("cancellation");

        for (request_index, (_, request_case)) in cases.iter().enumerate() {
            for (operation_index, (operation, _)) in cases.iter().enumerate() {
                let invocation = DaemonInvocationRequest {
                    protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
                    revision: DAEMON_INVOCATION_REVISION,
                    request_id: format!(
                        "request.callable-code.matrix.{request_index}.{operation_index}"
                    ),
                    delivery_route: None,
                    payload: DaemonInvocationPayload::CallableCode {
                        surface_operation: *operation,
                        request: request(*request_case),
                        page: page.clone(),
                        observed_at: UtcMicros(30),
                        deadline: deadline.clone(),
                        cancellation: cancellation.clone(),
                    },
                };

                if request_index == operation_index {
                    assert!(
                        invocation.validate().is_ok(),
                        "matching callable-code pair {request_index} must validate"
                    );
                } else {
                    assert!(
                        matches!(
                            invocation.validate(),
                            Err(DaemonInvocationProblem::InvalidRequest)
                        ),
                        "cross-pair operation {operation_index} and request {request_index} \
                         must retain InvalidRequest semantics"
                    );
                }
            }
        }
    }

    #[test]
    fn callable_code_outcome_is_distinct_and_context_grant_is_exact() {
        let observed_at = current_micros();
        let completed_at = UtcMicros(
            observed_at
                .0
                .checked_add(1)
                .expect("fixture completion timestamp"),
        );
        let deadline = Deadline::new(UtcMicros(
            observed_at
                .0
                .checked_add(60_000_000)
                .expect("fixture deadline"),
        ))
        .expect("deadline");
        let operation = callable_code_operations()
            .expect("operations")
            .get(CallableCodeOperationKind::ExactOccurrence)
            .clone();
        let scope = ResolvedScope::new(
            ProjectId::new("project.callable-code").expect("project"),
            tracedecay_domain::RepositoryId::new("repository.callable-code").expect("repository"),
            tracedecay_domain::WorktreeId::new("worktree.callable-code").expect("worktree"),
            None,
        )
        .expect("scope");
        let access = ProjectSourceAccessSnapshot {
            scope: scope.clone(),
            requester: ActorId::new("actor.callable-code").expect("actor"),
            binding: tracedecay_domain::configuration::ScopeSourceBinding::new(
                tracedecay_domain::SourceBindingId::new("binding.callable-code").expect("binding"),
                tracedecay_domain::configuration::SourceKindV1::Cursor,
                tracedecay_domain::LocatorDigest::new(format!("sha256:{}", "a".repeat(64)))
                    .expect("locator"),
                tracedecay_domain::configuration::AuthorityRef::Project(scope.project_id.clone()),
            )
            .expect("source binding"),
            configuration_revision: ConfigurationRevisionId::new("revision.callable-code")
                .expect("configuration revision"),
            configuration_digest: canonical_sha256(&"callable-code-configuration")
                .expect("configuration digest"),
            configuration_provenance_digest: canonical_sha256(
                &"callable-code-configuration-provenance",
            )
            .expect("configuration provenance"),
            effective_capabilities: [operation.capability_id().clone()].into_iter().collect(),
            grant_expires_at: deadline.expires_at,
        };
        let expired = callable_code_request_context(
            &scope,
            &access,
            "request.callable-code.expired",
            &operation,
            UtcMicros(1),
            Deadline::new(UtcMicros(observed_at.0.saturating_sub(1))).expect("expired deadline"),
            CancellationContext::active("cancel.callable-code.expired").expect("cancellation"),
        )
        .expect_err("wall-clock-expired deadline must fail despite a stale caller timestamp");
        assert_eq!(expired.kind(), ApplicationProblemKind::TimedOut);
        let context = callable_code_request_context(
            &scope,
            &access,
            "request.callable-code",
            &operation,
            observed_at,
            deadline.clone(),
            CancellationContext::active("cancel.callable-code").expect("cancellation"),
        )
        .expect("context");
        assert_eq!(context.scope(), &scope);
        assert_eq!(
            context.grant().allowed_capabilities,
            [operation.capability_id().clone()].into_iter().collect()
        );

        let authority = AuthorityReceipt::from_context(
            &context,
            PolicyDecisionRef::new(
                "policy.callable-code.fixture",
                1,
                canonical_sha256(&"callable-code-policy").expect("policy digest"),
                ComponentVersion::new("callable-code-policy.v1").expect("policy component"),
            )
            .expect("policy"),
            completed_at,
        )
        .expect("authority");
        let result = DaemonFeedbackResult {
            temporal: TemporalState::current(completed_at),
            authority,
            evidence_authorities: Vec::new(),
            coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Symbol], 0, 0, 0)
                .expect("coverage"),
            omissions: Vec::new(),
            scores: Vec::new(),
            contributions: Vec::new(),
            page: PageState::first_page(
                SortContractId::new("sort.callable-code.fixture").expect("sort"),
                1,
                Some(0),
                0,
            )
            .expect("page"),
            execution: OperationReceipt::completed(
                observed_at,
                completed_at,
                deadline,
                OperationBudgetUsage::default(),
            )
            .expect("execution"),
            payload: Some(serde_json::json!({"generation": "generation.callable-code"})),
        };
        let outcome = DaemonInvocationOutcome::CallableCode { scope, result };
        let encoded = serde_json::to_value(&outcome).expect("encode outcome");
        assert_eq!(encoded["status"], "callable_code");
        assert!(matches!(
            serde_json::from_value(encoded).expect("decode outcome"),
            DaemonInvocationOutcome::CallableCode { .. }
        ));
    }

    #[tokio::test]
    async fn lsp_session_rejects_a_client_root_that_differs_from_the_admitted_root() {
        let service = DaemonInvocationService::default();
        let project_root = PathBuf::from("/authoritative");
        DaemonLspOwnerRegistrar::new(&service)
            .register_factory(project_root.clone(), unavailable_lsp_session_factory())
            .await;
        let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
        let response = service
            .invoke(
                &registry,
                Some(&project_root),
                Some(AdmittedRoot::new("file:///authoritative")),
                None,
                DaemonInvocationRequest::lsp_open(
                    "request.1",
                    "client.1",
                    Some("file:///untrusted".to_owned()),
                    Vec::new(),
                ),
            )
            .await;
        let DaemonInvocationOutcome::LspOpened { session, .. } = response.outcome else {
            panic!("expected an admitted LSP session");
        };

        let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///untrusted","capabilities":{}}}"#;
        let response = service
            .invoke(
                &registry,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_frame("request.2", session.clone(), initialize),
            )
            .await;
        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::LspFrameAccepted { .. }
        ));

        let response = service
            .invoke(
                &registry,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_poll("request.3", session.clone()),
            )
            .await;
        let DaemonInvocationOutcome::LspFrame {
            frame: Some(frame), ..
        } = response.outcome
        else {
            panic!("expected initialize response");
        };
        let response: serde_json::Value =
            serde_json::from_str(&frame).expect("initialize error must be JSON-RPC");
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(
            response["error"]["data"]["detail"],
            "root is not the daemon-admitted root"
        );

        let response = service
            .invoke(
                &registry,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_acknowledge("request.4", session.clone()),
            )
            .await;
        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::LspAcknowledged { acknowledged: true }
        ));

        let initialize = r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"rootUri":"file:///authoritative","capabilities":{"general":{"positionEncodings":["utf-16"]}}}}"#;
        let response = service
            .invoke(
                &registry,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_frame("request.5", session.clone(), initialize),
            )
            .await;
        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::LspFrameAccepted {
                backpressured: false,
                closed: false
            }
        ));

        let response = service
            .invoke(
                &registry,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_poll("request.6", session),
            )
            .await;
        let DaemonInvocationOutcome::LspFrame {
            frame: Some(frame), ..
        } = response.outcome
        else {
            panic!("expected initialize success response");
        };
        let response: serde_json::Value =
            serde_json::from_str(&frame).expect("initialize success must be JSON-RPC");
        assert_eq!(response["id"], 2);
        assert!(response["result"]["capabilities"].is_object());
    }

    #[tokio::test]
    async fn lsp_session_admission_accepts_the_lsp_protocol_revision() {
        let service = DaemonInvocationService::default();
        let project_root = PathBuf::from("/authoritative");
        DaemonLspOwnerRegistrar::new(&service)
            .register_factory(project_root.clone(), unavailable_lsp_session_factory())
            .await;
        let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));

        let response = service
            .invoke(
                &registry,
                Some(&project_root),
                Some(AdmittedRoot::new("file:///authoritative")),
                None,
                DaemonInvocationRequest::lsp_open("request.revision", "3.17", None, Vec::new()),
            )
            .await;

        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::LspOpened { .. }
        ));
        assert_eq!(registry.lock().await.active_sessions(), 1);
        assert_eq!(service.lsp_sessions.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn lsp_disconnect_reconnect_and_final_detach_have_distinct_lifecycles() {
        let service = DaemonInvocationService::default();
        let project_root = PathBuf::from("/authoritative");
        DaemonLspOwnerRegistrar::new(&service)
            .register_factory(project_root.clone(), unavailable_lsp_session_factory())
            .await;
        let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
        let open = |request_id: &'static str| {
            service.invoke(
                &registry,
                Some(&project_root),
                Some(AdmittedRoot::new("file:///authoritative")),
                None,
                DaemonInvocationRequest::lsp_open(
                    request_id,
                    env!("CARGO_PKG_VERSION"),
                    None,
                    Vec::new(),
                ),
            )
        };

        let DaemonInvocationOutcome::LspOpened { session, .. } =
            open("request.open.1").await.outcome
        else {
            panic!("expected first session");
        };
        let detached = service
            .invoke(
                &registry,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_detach("request.detach", session),
            )
            .await;
        assert!(matches!(
            detached.outcome,
            DaemonInvocationOutcome::LspDetached
        ));
        assert_eq!(registry.lock().await.active_sessions(), 0);
        assert!(service.lsp_sessions.lock().await.is_empty());

        let DaemonInvocationOutcome::LspOpened { session, .. } =
            open("request.open.2").await.outcome
        else {
            panic!("released capacity must admit a replacement");
        };
        service
            .disconnect_lsp_session(&registry, session.clone())
            .await;
        assert_eq!(registry.lock().await.active_sessions(), 1);
        assert_eq!(service.lsp_sessions.lock().await.len(), 1);

        let reconnected = service
            .invoke(
                &registry,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_reconnect("request.reconnect", session.clone()),
            )
            .await;
        let DaemonInvocationOutcome::LspReconnected {
            session: reconnected_session,
        } = reconnected.outcome
        else {
            panic!("expected reconnect");
        };
        let takeover = service
            .invoke(
                &registry,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_reconnect(
                    "request.reconnect-race",
                    reconnected_session.clone(),
                ),
            )
            .await;
        let DaemonInvocationOutcome::LspReconnected {
            session: current_session,
        } = takeover.outcome
        else {
            panic!("expected active transport takeover");
        };
        service
            .disconnect_lsp_session(&registry, reconnected_session)
            .await;
        assert_eq!(registry.lock().await.active_sessions(), 1);
        assert_eq!(service.lsp_sessions.lock().await.len(), 1);
        let stale_transport = service
            .invoke(
                &registry,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_poll("request.stale", session),
            )
            .await;
        assert!(matches!(
            stale_transport.outcome,
            DaemonInvocationOutcome::Problem {
                problem: DaemonInvocationProblem::NotFoundOrNotAuthorized
            }
        ));
        assert_eq!(service.lsp_sessions.lock().await.len(), 1);

        let detached = service
            .invoke(
                &registry,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_detach("request.detach.2", current_session),
            )
            .await;
        assert!(matches!(
            detached.outcome,
            DaemonInvocationOutcome::LspDetached
        ));
        assert_eq!(registry.lock().await.active_sessions(), 0);
        assert!(service.lsp_sessions.lock().await.is_empty());
    }

    #[tokio::test]
    async fn feedback_handles_fail_closed_without_an_owner() {
        let service = DaemonInvocationService::default();
        let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
        let response = service
            .invoke(
                &registry,
                None,
                Some(AdmittedRoot::new("file:///authoritative")),
                None,
                DaemonInvocationRequest {
                    protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
                    revision: DAEMON_INVOCATION_REVISION,
                    request_id: "request.1".to_owned(),
                    delivery_route: None,
                    payload: DaemonInvocationPayload::FeedbackList {
                        request_handle: "handle.1".to_owned(),
                        observed_at: UtcMicros(1),
                        deadline: Deadline::new(UtcMicros(2)).expect("deadline"),
                        cancellation: CancellationContext::active("cancel.feedback-owner")
                            .expect("cancellation"),
                    },
                },
            )
            .await;
        // With no feedback owner registered the read service itself is absent,
        // so the daemon fails closed as an application-level Unavailable problem
        // (not concealment — that only applies once the service exists and a
        // caller names an unknown handle). See `execute_feedback`.
        let DaemonInvocationOutcome::ApplicationProblem { problem } = response.outcome else {
            panic!(
                "absent feedback owner must fail closed as an application problem: {:?}",
                response.outcome
            );
        };
        assert_eq!(problem.kind(), ApplicationProblemKind::Unavailable);
    }

    #[test]
    fn feedback_invocation_retains_trusted_delivery_route() {
        let request = DaemonInvocationRequest::feedback(
            "request.delivery-route",
            crate::application_surface::ApplicationSurfaceOperation::FeedbackList,
            "handle.delivery-route".to_owned(),
            UtcMicros(1),
            Deadline::new(UtcMicros(2)).expect("deadline"),
            CancellationContext::active("cancel.delivery-route").expect("cancellation"),
        )
        .with_delivery_route(Plan26DeliveryRouteV1::Mcp);
        assert_eq!(request.delivery_route, Some(Plan26DeliveryRouteV1::Mcp));
        let encoded = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(encoded["delivery_route"], "mcp");
        assert!(request.validate().is_ok());
    }

    #[test]
    fn feedback_cycle_projections_use_distinct_handle_payloads() {
        for (surface_operation, daemon_operation, wire_operation) in [
            (
                crate::application_surface::ApplicationSurfaceOperation::FeedbackImpact,
                DaemonInvocationOperation::FeedbackImpact,
                "feedback_impact",
            ),
            (
                crate::application_surface::ApplicationSurfaceOperation::AffectedTests,
                DaemonInvocationOperation::AffectedTests,
                "affected_tests",
            ),
        ] {
            let request = DaemonInvocationRequest::feedback(
                format!("request.{}", surface_operation.as_str()),
                surface_operation,
                "rh_feedback-cycle.fixture".to_owned(),
                UtcMicros(1),
                Deadline::new(UtcMicros(2)).expect("deadline"),
                CancellationContext::active(format!("cancel.{}", surface_operation.as_str()))
                    .expect("cancellation"),
            );

            assert_eq!(request.operation(), daemon_operation);
            assert!(request.validate().is_ok());
            let encoded = serde_json::to_value(&request).expect("serialize request");
            assert_eq!(encoded["operation"], wire_operation);
            assert_eq!(encoded["request_handle"], "rh_feedback-cycle.fixture");
        }
    }

    #[test]
    fn feedback_rejection_observation_classifies_request_and_revision_failures() {
        let invalid = DaemonInvocationResponse::problem(
            "request.invalid",
            DaemonInvocationProblem::InvalidRequest,
        );
        assert_eq!(
            plan26_rejected_argument(&invalid),
            Some((
                Plan26RejectedArgumentV1::RequestBody,
                Plan26ArgumentRejectionClassV1::InvalidShape,
            ))
        );

        let unsupported = DaemonInvocationResponse::problem(
            "request.unsupported",
            DaemonInvocationProblem::UnsupportedRevision,
        );
        assert_eq!(
            plan26_rejected_argument(&unsupported),
            Some((
                Plan26RejectedArgumentV1::Lifecycle,
                Plan26ArgumentRejectionClassV1::Unsupported,
            ))
        );
    }

    #[test]
    fn feedback_observation_invocation_accepts_only_content_free_events() {
        let subject =
            ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("subject digest");
        let request = DaemonInvocationRequest::feedback_observation(
            "request.feedback-observe",
            subject,
            UtcMicros(1),
            Plan26FeedbackSourceEventV1::SseLifecycle {
                lifecycle: crate::application::feedback::observations::Plan26SseLifecycleV1::Gap,
                sequence: Some(1),
                item_count: 0,
                duration_micros: None,
            },
        );
        assert_eq!(
            request.operation(),
            DaemonInvocationOperation::FeedbackObserve
        );
        assert!(request.validate().is_ok());
        let encoded = serde_json::to_string(&request).expect("serialize request");
        assert!(!encoded.contains("source"));
        assert!(!encoded.contains("comment"));
        assert!(!encoded.contains("log"));
    }

    #[tokio::test]
    async fn registered_work_services_dispatch_the_core_lifecycle() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project root");
        let project_id = ProjectId::new("project.work.core-invocation").expect("project id");
        let host = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
            crate::storage::default_profile_root().expect("profile root"),
            project.path(),
            project_id.clone(),
        )
        .await
        .expect("registered project runtime");
        let database = host
            .project_observation_database_arc_for_test()
            .expect("registered project database");
        let actor = ActorId::new("actor.work.core-invocation").expect("actor id");
        let scope = ResolvedScope::new(
            project_id,
            tracedecay_domain::RepositoryId::new("repository.work.core-invocation")
                .expect("repository id"),
            tracedecay_domain::WorktreeId::new("worktree.work.core-invocation")
                .expect("worktree id"),
            None,
        )
        .expect("resolved scope");
        let grant_digest =
            ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).expect("grant digest");
        let capabilities = tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1
            .iter()
            .map(|(_, capability, _)| CapabilityId::new(*capability).expect("capability"))
            .collect();
        let use_cases = tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1
            .iter()
            .map(|(_, _, use_case)| UseCaseId::new(*use_case).expect("use case"))
            .collect();
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.work.core-invocation").expect("grant id"),
            1,
            grant_digest.clone(),
            actor.clone(),
            UtcMicros(1),
            UtcMicros(10_000),
            scope.clone(),
            capabilities,
            use_cases,
            DisclosureClass::Sensitive,
        )
        .expect("Work grant");
        let authority = WorkAuthority::new(
            scope.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            actor.clone(),
            grant_digest,
        )
        .expect("Work authority");
        let service = DaemonInvocationService::default();
        DaemonWorkRuntimeRegistrar::new(&service)
            .register(
                project.path().to_path_buf(),
                database,
                authority,
                actor,
                grant,
                ManifestDigest::new(format!("sha256:{}", "e".repeat(64))).expect("policy digest"),
                ManifestDigest::new(format!("sha256:{}", "f".repeat(64)))
                    .expect("configuration digest"),
                crate::sessions::codex_app_server::CodexAppServerSummaryConfig {
                    codex_bin: "tracedecay-work-provider-not-used".to_owned(),
                    model: None,
                    timeout: Duration::from_secs(5),
                },
            )
            .await
            .expect("registered Work runtime");
        let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
        let task_id = tracedecay_domain::TaskId::new("task.work.core-invocation").expect("task id");
        let proposal_digest =
            ManifestDigest::new(format!("sha256:{}", "1".repeat(64))).expect("proposal digest");

        macro_rules! invoke {
            ($request_id:literal, $request:expr) => {
                service
                    .invoke(
                        &registry,
                        Some(project.path()),
                        None,
                        None,
                        DaemonInvocationRequest::work_application(
                            $request_id,
                            $request,
                            UtcMicros(100),
                            Deadline::new(UtcMicros(1_000)).expect("deadline"),
                            CancellationContext::active(concat!("cancel.", $request_id))
                                .expect("cancellation"),
                        ),
                    )
                    .await
                    .outcome
            };
        }

        let created = invoke!(
            "request.work.create",
            WorkApplicationInvocationV1::Create(CreateWorkCommand {
                task_id: task_id.clone(),
                title: "Exercise the production Work dispatcher".to_owned(),
                dependencies: std::collections::BTreeSet::new(),
                command_id: tracedecay_domain::WorkCommandId::new("command.work.create")
                    .expect("command id"),
                occurred_at: UtcMicros(10),
            })
        );
        let DaemonInvocationOutcome::WorkApplication {
            outcome: WorkApplicationOutcomeV1::Create(ApplicationOutcome::Effect(created_effect)),
            ..
        } = created
        else {
            panic!("create must return a Work effect: {created:?}");
        };
        let created = created_effect.payload.expect("created projection");
        assert_eq!(created.version(), tracedecay_domain::WorkVersion::initial());

        let snapshot = invoke!(
            "request.work.snapshot",
            WorkApplicationInvocationV1::Snapshot(WorkProjectionSnapshotRequestV1 {
                page_size: 100,
            })
        );
        let DaemonInvocationOutcome::WorkApplication {
            outcome:
                WorkApplicationOutcomeV1::Snapshot(ApplicationOutcome::Evidence(snapshot_packet)),
            ..
        } = snapshot
        else {
            panic!("snapshot must return Work evidence: {snapshot:?}");
        };
        let snapshot = snapshot_packet.payload.expect("snapshot payload");
        assert_eq!(snapshot.projections(), std::slice::from_ref(&created));
        let cursor = tracedecay_rusqlite_runtime::work::WorkSqliteStorage::resume_cursor(&snapshot)
            .expect("snapshot cursor");

        let review = ReviewProposalRequestV1 {
            review: tracedecay_application::ReviewProposalCommand {
                task_id: task_id.clone(),
                proposal_id: tracedecay_domain::ProposalId::new("proposal.work.review")
                    .expect("proposal id"),
                proposal_digest: proposal_digest.clone(),
                expected_version: created.version(),
                command_id: tracedecay_domain::WorkCommandId::new("command.work.review")
                    .expect("command id"),
                occurred_at: UtcMicros(20),
            },
            disposition: tracedecay_application::ReviewProposalDispositionV1::Rejected,
        };
        let reviewed = invoke!(
            "request.work.review",
            WorkApplicationInvocationV1::ReviewProposal(review)
        );
        let DaemonInvocationOutcome::WorkApplication {
            outcome:
                WorkApplicationOutcomeV1::ReviewProposal(ApplicationOutcome::Effect(reviewed_effect)),
            ..
        } = reviewed
        else {
            panic!("review must return a Work effect: {reviewed:?}");
        };
        let reviewed = reviewed_effect.payload.expect("reviewed projection");

        let delta = invoke!(
            "request.work.delta",
            WorkApplicationInvocationV1::Delta(WorkProjectionDeltaRequestV1 {
                cursor,
                page_size: 100,
            })
        );
        let DaemonInvocationOutcome::WorkApplication {
            outcome: WorkApplicationOutcomeV1::Delta(ApplicationOutcome::Evidence(delta_packet)),
            ..
        } = delta
        else {
            panic!("delta must return Work evidence: {delta:?}");
        };
        let delta = delta_packet.payload.expect("delta payload");
        assert_eq!(delta.changed(), std::slice::from_ref(&reviewed));

        let accepted = invoke!(
            "request.work.accept-proposal",
            WorkApplicationInvocationV1::AcceptProposal(AcceptProposalCommand {
                review: tracedecay_application::ReviewProposalCommand {
                    task_id: task_id.clone(),
                    proposal_id: tracedecay_domain::ProposalId::new("proposal.work.accept")
                        .expect("proposal id"),
                    proposal_digest,
                    expected_version: reviewed.version(),
                    command_id: tracedecay_domain::WorkCommandId::new(
                        "command.work.accept-proposal",
                    )
                    .expect("command id"),
                    occurred_at: UtcMicros(30),
                },
            })
        );
        let DaemonInvocationOutcome::WorkApplication {
            outcome:
                WorkApplicationOutcomeV1::AcceptProposal(ApplicationOutcome::Effect(accepted_effect)),
            ..
        } = accepted
        else {
            panic!("proposal acceptance must return a Work effect: {accepted:?}");
        };
        let accepted = accepted_effect
            .payload
            .expect("accepted proposal projection");

        let admitted = invoke!(
            "request.work.admit",
            WorkApplicationInvocationV1::AdmitExecution(AdmitExecutionCommand {
                task_id: task_id.clone(),
                expected_version: accepted.version(),
                command_id: tracedecay_domain::WorkCommandId::new("command.work.admit")
                    .expect("command id"),
                occurred_at: UtcMicros(40),
            })
        );
        let DaemonInvocationOutcome::WorkApplication {
            outcome:
                WorkApplicationOutcomeV1::AdmitExecution(ApplicationOutcome::Effect(admitted_effect)),
            ..
        } = admitted
        else {
            panic!("execution admission must return a Work effect: {admitted:?}");
        };
        let admitted = admitted_effect.payload.expect("admitted projection");

        let with_evidence = invoke!(
            "request.work.attach-evidence",
            WorkApplicationInvocationV1::AttachRuntimeEvidence(AttachRuntimeEvidenceCommand {
                task_id: task_id.clone(),
                evidence: tracedecay_domain::RuntimeEvidenceRef::new(
                    tracedecay_domain::RunId::new("run.work.core-invocation").expect("run id"),
                    ManifestDigest::new(format!("sha256:{}", "2".repeat(64)))
                        .expect("evidence digest"),
                    true,
                )
                .expect("runtime evidence"),
                expected_version: admitted.version(),
                command_id: tracedecay_domain::WorkCommandId::new("command.work.attach-evidence",)
                    .expect("command id"),
                occurred_at: UtcMicros(50),
            })
        );
        let DaemonInvocationOutcome::WorkApplication {
            outcome:
                WorkApplicationOutcomeV1::AttachRuntimeEvidence(ApplicationOutcome::Effect(
                    evidence_effect,
                )),
            ..
        } = with_evidence
        else {
            panic!("runtime evidence must return a Work effect: {with_evidence:?}");
        };
        let with_evidence = evidence_effect.payload.expect("evidence projection");

        let accepted_task = invoke!(
            "request.work.accept-task",
            WorkApplicationInvocationV1::AcceptTask(AcceptTaskCommand {
                task_id,
                expected_version: with_evidence.version(),
                command_id: tracedecay_domain::WorkCommandId::new("command.work.accept-task")
                    .expect("command id"),
                occurred_at: UtcMicros(60),
            })
        );
        let DaemonInvocationOutcome::WorkApplication {
            outcome: WorkApplicationOutcomeV1::AcceptTask(ApplicationOutcome::Effect(task_effect)),
            ..
        } = accepted_task
        else {
            panic!("task acceptance must return a Work effect: {accepted_task:?}");
        };
        assert!(
            task_effect
                .payload
                .expect("accepted task projection")
                .is_task_accepted()
        );
    }

    #[tokio::test]
    async fn registered_work_runtime_dispatches_attempt_requests() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project root");
        let project_id = ProjectId::new("project.work.invocation").expect("project id");
        let host = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
            crate::storage::default_profile_root().expect("profile root"),
            project.path(),
            project_id.clone(),
        )
        .await
        .expect("registered project runtime");
        let database = host
            .project_observation_database_arc_for_test()
            .expect("registered project database");
        let actor = ActorId::new("actor.work.invocation").expect("actor id");
        let scope = ResolvedScope::new(
            project_id.clone(),
            tracedecay_domain::RepositoryId::new("repository.work.invocation")
                .expect("repository id"),
            tracedecay_domain::WorktreeId::new("worktree.work.invocation").expect("worktree id"),
            None,
        )
        .expect("resolved scope");
        let grant_digest =
            ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("grant digest");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.work.invocation").expect("grant id"),
            1,
            grant_digest.clone(),
            actor.clone(),
            UtcMicros(1),
            UtcMicros(10_000),
            scope.clone(),
            std::collections::BTreeSet::from([CapabilityId::new(
                "capability.work.attempt_renew_lease",
            )
            .expect("capability")]),
            std::collections::BTreeSet::from([
                UseCaseId::new("use-case.work.attempt_renew_lease").expect("use case")
            ]),
            DisclosureClass::Sensitive,
        )
        .expect("Work grant");
        let authority = WorkAuthority::new(
            scope.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            actor.clone(),
            grant_digest,
        )
        .expect("Work authority");
        let policy_digest =
            ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).expect("policy digest");
        let configuration_digest = ManifestDigest::new(format!("sha256:{}", "c".repeat(64)))
            .expect("configuration digest");
        let service = DaemonInvocationService::default();
        DaemonWorkRuntimeRegistrar::new(&service)
            .register(
                project.path().to_path_buf(),
                Arc::clone(&database),
                authority.clone(),
                actor.clone(),
                grant.clone(),
                policy_digest.clone(),
                configuration_digest.clone(),
                crate::sessions::codex_app_server::CodexAppServerSummaryConfig {
                    codex_bin: "tracedecay-work-provider-not-used".to_owned(),
                    model: None,
                    timeout: Duration::from_secs(5),
                },
            )
            .await
            .expect("registered Work runtime");
        assert!(
            DaemonWorkRuntimeRegistrar::new(&service)
                .authority_matches(
                    project.path(),
                    &authority,
                    &actor,
                    &grant,
                    &policy_digest,
                    &configuration_digest,
                )
                .await
        );
        let rotated_grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.work.invocation.rotated").expect("grant id"),
            1,
            grant.digest.clone(),
            actor.clone(),
            UtcMicros(2),
            UtcMicros(20_000),
            scope.clone(),
            grant.allowed_capabilities.clone(),
            grant.allowed_use_cases.clone(),
            DisclosureClass::Sensitive,
        )
        .expect("rotated Work grant");
        let rotated_authority = WorkAuthority::new(
            scope.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            actor.clone(),
            rotated_grant.digest.clone(),
        )
        .expect("rotated Work authority");
        DaemonWorkRuntimeRegistrar::new(&service)
            .register(
                project.path().to_path_buf(),
                Arc::clone(&database),
                rotated_authority.clone(),
                actor.clone(),
                rotated_grant.clone(),
                policy_digest.clone(),
                configuration_digest.clone(),
                crate::sessions::codex_app_server::CodexAppServerSummaryConfig {
                    codex_bin: "tracedecay-work-provider-not-used".to_owned(),
                    model: None,
                    timeout: Duration::from_secs(5),
                },
            )
            .await
            .expect("rotated Work runtime authority");
        assert!(
            DaemonWorkRuntimeRegistrar::new(&service)
                .authority_matches(
                    project.path(),
                    &rotated_authority,
                    &actor,
                    &rotated_grant,
                    &policy_digest,
                    &configuration_digest,
                )
                .await
        );
        let mismatched_authority = WorkAuthority::new(
            scope.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            ActorId::new("actor.work.mismatched").expect("mismatched actor"),
            rotated_grant.digest.clone(),
        )
        .expect("mismatched Work authority");
        assert!(
            DaemonWorkRuntimeRegistrar::new(&service)
                .register(
                    project.path().to_path_buf(),
                    database,
                    mismatched_authority,
                    actor.clone(),
                    rotated_grant.clone(),
                    policy_digest.clone(),
                    configuration_digest.clone(),
                    crate::sessions::codex_app_server::CodexAppServerSummaryConfig {
                        codex_bin: "tracedecay-work-provider-not-used".to_owned(),
                        model: None,
                        timeout: Duration::from_secs(5),
                    },
                )
                .await
                .is_err()
        );
        let identity = tracedecay_domain::WorkAttemptIdentityV1::new(
            tracedecay_domain::TaskId::new("task.work.invocation").expect("task id"),
            tracedecay_domain::RunId::new("run.work.invocation").expect("run id"),
            tracedecay_domain::AttemptId::new("attempt.work.invocation").expect("attempt id"),
        )
        .expect("attempt identity");
        let expected = tracedecay_domain::WorkLeaseFenceV1::new(
            tracedecay_domain::WorkLeaseId::new("lease.work.invocation").expect("lease id"),
            tracedecay_domain::WorkFenceEpochV1::new(1).expect("fence epoch"),
        )
        .expect("expected lease");
        let replacement = tracedecay_domain::WorkLeaseFenceV1::new(
            tracedecay_domain::WorkLeaseId::new("lease.work.invocation").expect("lease id"),
            tracedecay_domain::WorkFenceEpochV1::new(2).expect("fence epoch"),
        )
        .expect("replacement lease");
        let request = DaemonInvocationRequest::work_attempt(
            "request.work.invocation",
            WorkAttemptInvocationV1::RenewLease(
                tracedecay_application::WorkAttemptRenewLeaseRequestV1 {
                    identity,
                    expected,
                    replacement,
                },
            ),
            UtcMicros(1),
            Deadline::new(UtcMicros(2)).expect("deadline"),
            CancellationContext::active("cancel.work.invocation").expect("cancellation"),
        );
        assert_eq!(request.operation(), DaemonInvocationOperation::WorkAttempt);
        let response = service
            .invoke(
                &Arc::new(Mutex::new(LspSessionRegistry::default())),
                Some(project.path()),
                None,
                None,
                request,
            )
            .await;
        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::Problem {
                problem: DaemonInvocationProblem::NotFoundOrNotAuthorized
            }
        ));
    }

    /// Daemon expiry must stop the provider processes a registered Work runtime
    /// owns, not merely drop the registry that owned them.
    #[cfg(unix)]
    #[tokio::test]
    async fn expiring_registries_reaps_running_work_executions() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project root");
        let project_id = ProjectId::new("project.work.expire").expect("project id");
        let host = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
            crate::storage::default_profile_root().expect("profile root"),
            project.path(),
            project_id.clone(),
        )
        .await
        .expect("registered project runtime");
        let database = host
            .project_observation_database_arc_for_test()
            .expect("registered project database");
        let actor = ActorId::new("actor.work.expire").expect("actor id");
        let scope = ResolvedScope::new(
            project_id,
            tracedecay_domain::RepositoryId::new("repository.work.expire").expect("repository id"),
            tracedecay_domain::WorktreeId::new("worktree.work.expire").expect("worktree id"),
            None,
        )
        .expect("resolved scope");
        let grant_digest =
            ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).expect("grant digest");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.work.expire").expect("grant id"),
            1,
            grant_digest.clone(),
            actor.clone(),
            UtcMicros(1),
            UtcMicros(10_000),
            scope.clone(),
            std::collections::BTreeSet::from([
                CapabilityId::new("capability.work.expire").expect("capability")
            ]),
            std::collections::BTreeSet::from([
                UseCaseId::new("use-case.work.expire").expect("use case")
            ]),
            DisclosureClass::Sensitive,
        )
        .expect("Work grant");
        let authority = WorkAuthority::new(
            scope.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            actor.clone(),
            grant_digest,
        )
        .expect("Work authority");
        let context = RequestContext::new(
            actor.clone(),
            scope,
            grant.clone(),
            RequestId::new("request.work.expire").expect("request id"),
            Deadline::new(UtcMicros(9_000)).expect("deadline"),
            CancellationContext::active("cancel.work.expire").expect("cancellation"),
        )
        .expect("request context");

        let storage = database.work_storage().expect("Work storage");
        let work = tracedecay_application::WorkService::new(storage);
        let task_id = tracedecay_domain::TaskId::new("task.work.expire").expect("task id");
        work.create(
            &context,
            CreateWorkCommand {
                task_id: task_id.clone(),
                title: "Reap the provider on expiry".to_owned(),
                dependencies: std::collections::BTreeSet::new(),
                command_id: tracedecay_domain::WorkCommandId::new("command.work.expire.create")
                    .expect("command id"),
                occurred_at: UtcMicros(10),
            },
        )
        .expect("created Work");
        work.accept_proposal(
            &context,
            AcceptProposalCommand {
                review: tracedecay_application::ReviewProposalCommand {
                    task_id: task_id.clone(),
                    proposal_id: tracedecay_domain::ProposalId::new("proposal.work.expire")
                        .expect("proposal id"),
                    proposal_digest: ManifestDigest::new(format!("sha256:{}", "e".repeat(64)))
                        .expect("proposal digest"),
                    expected_version: tracedecay_domain::WorkVersion::initial(),
                    command_id: tracedecay_domain::WorkCommandId::new(
                        "command.work.expire.proposal",
                    )
                    .expect("command id"),
                    occurred_at: UtcMicros(20),
                },
            },
        )
        .expect("accepted proposal");
        work.admit_execution(
            &context,
            AdmitExecutionCommand {
                task_id: task_id.clone(),
                expected_version: tracedecay_domain::WorkVersion::new(2).expect("version"),
                command_id: tracedecay_domain::WorkCommandId::new("command.work.expire.admit")
                    .expect("command id"),
                occurred_at: UtcMicros(30),
            },
        )
        .expect("admitted execution");
        let snapshot = tracedecay_domain::WorkProjectionSnapshotV1::new(
            tracedecay_domain::ProjectionGenerationId::new("generation.work.expire")
                .expect("generation id"),
            tracedecay_domain::WorkProjectionSequenceV1::new(3),
            vec![work.load(&context, &task_id).expect("projection")],
            tracedecay_domain::WorkProjectionCoverageV1::complete(1, 1).expect("coverage"),
        )
        .expect("projection snapshot");

        let fixture = project.path().join("codex-work-expire-fixture");
        std::fs::write(
            &fixture,
            "#!/usr/bin/env python3\nimport time\nwhile True:\n    time.sleep(1)\n",
        )
        .expect("fixture");
        let mut permissions = std::fs::metadata(&fixture).expect("metadata").permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&fixture, permissions).expect("fixture mode");

        let service = DaemonInvocationService::default();
        DaemonWorkRuntimeRegistrar::new(&service)
            .register(
                project.path().to_path_buf(),
                Arc::clone(&database),
                authority,
                actor,
                grant,
                ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).expect("policy digest"),
                ManifestDigest::new(format!("sha256:{}", "c".repeat(64)))
                    .expect("configuration digest"),
                crate::sessions::codex_app_server::CodexAppServerSummaryConfig {
                    codex_bin: fixture.to_string_lossy().into_owned(),
                    model: None,
                    timeout: Duration::from_secs(30),
                },
            )
            .await
            .expect("registered Work runtime");
        let registered = service
            .work_runtime(Some(project.path()))
            .await
            .expect("registered Work runtime handle");

        let identity = tracedecay_domain::WorkAttemptIdentityV1::new(
            task_id,
            tracedecay_domain::RunId::new("run.work.expire").expect("run id"),
            tracedecay_domain::AttemptId::new("attempt.work.expire").expect("attempt id"),
        )
        .expect("attempt identity");
        let lease = tracedecay_domain::WorkLeaseFenceV1::new(
            tracedecay_domain::WorkLeaseId::new("lease.work.expire").expect("lease id"),
            tracedecay_domain::WorkFenceEpochV1::new(1).expect("fence epoch"),
        )
        .expect("lease");
        registered
            .runtime
            .acquire_lease(&snapshot, identity.clone(), lease.clone())
            .await
            .expect("leased attempt");
        registered
            .runtime
            .start(
                &identity,
                &lease,
                tracedecay_domain::WorkRecoveryStateV1::Fresh,
            )
            .await
            .expect("started attempt");
        assert_eq!(
            registered.runtime.in_flight(),
            1,
            "the registered runtime must own the provider execution"
        );

        service.expire_all().await;

        assert_eq!(
            registered.runtime.in_flight(),
            0,
            "daemon expiry must stop and join every provider execution"
        );
        assert!(service.project_runtimes.is_empty().await);
    }
}
