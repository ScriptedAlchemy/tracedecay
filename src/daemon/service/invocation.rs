//! Closed, authenticated daemon invocation protocol.
//!
//! This module deliberately accepts a small typed operation set after the
//! daemon handshake. It is not a generic application invoke endpoint and it
//! never accepts a raw Git request, database selector, or LSP socket address.
//! LSP frames are handled by a daemon-owned protocol actor; the bridge only
//! receives the actor's bounded responses through explicit frame operations.

use std::any::Any;
use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::BTreeSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, RwLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use thiserror::Error;
use tokio::sync::{Mutex, Semaphore};
use tracedecay_application::clock::now_micros;
use tracedecay_application::feedback::{
    FeedbackReadPort, FeedbackRouteAuthorizationPort, FeedbackRuntimeStatePort,
};
use tracedecay_application::{
    AffectedTestsRetrievalPort, AnalyzerAdmittedDiagnosticProviderV1, ApplicationContractError,
    ApplicationOperation, ApplicationOutcome, ApplicationProblem, ApplicationProblemKind,
    ApplicationResult, AuthorityReceipt, AuthorizedScopeSet, AuthorizedScopeSetAuthority,
    CallableCodeAuthorizationPort, CallableCodeOperationKind, CallableCodeQueryService,
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, CoverageCompleteness,
    CoverageDomainState, Deadline, DiagnosticProviderIdentity, DisclosureClass, EffectId,
    EffectReceipt, EffectResult, EffectTermination, EvidenceAuthority, EvidenceCoverage,
    EvidenceDomain, EvidenceIdentity, EvidencePacket, GitIndexApplyPortResultV1,
    GitIndexApplyRequestV1, GitIndexEffectProofV1, GitIndexOperationBindingV1,
    GitIndexPreviewPortResultV1, GitIndexPreviewRequestV1, GitIndexRecoveryRequestV1,
    GitIndexTransactionApplicationError, GitIndexTransactionPort, GitIndexTransactionPortError,
    GitIndexTransactionService, IdempotencyKey, MultiRootScopeSetCasRequestV1,
    MultiRootScopeSetCasResultV1, MultiRootScopeSetCasStatusV1, Omission, OmissionReason,
    OperationBudgetUsage, OperationReceipt, OperationTermination, PageRequest, PageState,
    PolicyDecisionRef, PolicyEvaluationContextV1, PolicyEvaluatorCompositionV1,
    PolicyEvidenceHorizonV1, PreviewId, PreviewResult, ReconciliationState, RequestAdmission,
    RequestContext, RequestId, ResolvedScope, RetryDirective, SafeDiagnostic, TaskHandoffError,
    TaskHandoffRedeemedV1, TaskHandoffToken, TemporalState, WorkExecutionError,
    WorkProjectionApplicationError, WorkflowCoordinationError, WorkflowFanOutRuntimeError,
    callable_code_operations,
};
use tracedecay_domain::configuration::{
    CandidateDispositionV1, ConfigurationGrantId, ConfigurationGrantReceiptId,
    ConfigurationLayerIdV1, ConfigurationMutationEffectV1, ConfigurationMutationGrantReceiptV1,
    ConfigurationMutationOperationV1, ConfigurationMutationSinkV1, ConfigurationRevisionId,
    ConfigurationSnapshotV1, ProtectedApplyRequest,
};
use tracedecay_domain::{
    AccessPolicyDigest, ActorId, ComponentVersion, GitHeadStateV1, GitIndexPreviewId,
    GitIndexTransactionOperationV1, GitIndexTransactionReceiptV1, ManifestDigest, ProjectId,
    ScopeSetId, ScopeSetRevision, UserProfileId, UtcMicros, WorkAuthority, canonical_sha256,
};
use tracedecay_lsp::analyzer::broker::DiagnosticBroker;
use tracedecay_lsp::analyzer::client::LspRefreshTimeouts;
use tracedecay_lsp::{
    AdmittedRoot, AuthorizedLspSession, AuthorizedLspWorkspace, DaemonLspRuntimeSession,
    DaemonLspSessionEndpoint, DiagnosticTrigger, FeedbackCycleRequest, FeedbackCycleRuntimePort,
    GatewayCapabilities, LSP_SESSION_TTL_MS, LspEndpointError, LspRuntimeFailure, LspRuntimeFuture,
    LspSessionAccess, LspSessionAdmissionPort, LspSessionCredential, LspSessionId,
    LspSessionOpenRequest, LspSessionRegistry, SessionLifecycle, UpstreamCapabilities,
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

use super::project_runtime::{
    FeedbackCyclePublicationError, ProjectRuntimeAlreadyRegistered, ProjectRuntimeRegistryError,
    ProjectRuntimeRegistryV1,
};
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
use crate::daemon::work_runtime::DaemonWorkRuntimeV1;
use crate::daemon::workflow_runtime::execute_canonical_workflow;
// Re-exported so the long tail of daemon-internal call sites can keep naming the
// contract through `service::invocation::` while the split settles.
#[cfg(test)]
pub(crate) use crate::daemon_contract::{
    DAEMON_INVOCATION_PROTOCOL, DAEMON_INVOCATION_REVISION, parse_daemon_invocation_request,
};
pub(crate) use crate::daemon_contract::{
    DaemonFeedbackResult, DaemonGitEffectResult, DaemonGitPreviewResult, DaemonInvocationOperation,
    DaemonInvocationOutcome, DaemonInvocationPayload, DaemonInvocationProblem,
    DaemonInvocationRequest, DaemonInvocationResponse, DaemonLspSessionAccess,
    WorkApplicationInvocationV1, WorkApplicationOutcomeV1, WorkAttemptInvocationV1,
    WorkflowApplicationInvocationV1, WorkflowApplicationOutcomeV1,
};
// Wire-shape fixtures build application commands the dispatch path no longer
// names directly now that request construction lives with the contract.
use crate::db::Database;
use crate::errors::TraceDecayError;
use crate::production_semantic_authorities;
use crate::request_identity::{
    GlobalOpaqueIdentityKind, LogicalEffectIdempotencyDomain, derive_logical_effect_idempotency,
    mint_global_opaque_id,
};
use crate::tracedecay::TraceDecay;
#[cfg(test)]
use tracedecay_application::{
    AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand, AttachRuntimeEvidenceCommand,
    CreateWorkCommand, MultiRootExecuteRequestV1, MultiRootScopeSetReadRequestV1,
    ReviewProposalRequestV1, WorkProjectionDeltaRequestV1, WorkProjectionSnapshotRequestV1,
};
use tracedecay_hooks::{
    HookBoundaryV1, HookEventEnvelopeV2, HookEventV2, HookFeedbackDeliveryPortV1,
    HookScopeBindingV1,
};

// Structural split: production logic now lives in the child modules below;
// this file remains the stable external path (`service::invocation::*`).
mod configuration;
mod dispatch;
mod feedback;
mod git;
mod lsp;
mod plan26;
mod primitive;
mod registrars;
#[cfg(test)]
mod tests;
mod types;
mod work;

use configuration::*;
use feedback::*;
use git::*;
#[cfg(test)]
use lsp::*;
use plan26::*;
use primitive::*;
use registrars::*;
use types::*;
use work::*;

pub(crate) use configuration::{
    DaemonSemanticRuntimeRegistrar, DaemonSemanticRuntimeRegistrationError,
};
pub(crate) use feedback::{DaemonFeedbackInvocationOwner, daemon_operation_event_authority};
pub(crate) use primitive::{
    DaemonContextScoutRuntimeRegistrar, DaemonContextScoutRuntimeRegistrationError,
    DaemonPrimitiveRuntimeRegistrar, DaemonPrimitiveRuntimeRegistrationError,
};
pub(in crate::daemon) use types::observe_accepted_feedback_cycle_terminal;
pub(crate) use types::{
    BoundedPr13HookOrchestratorV1, DaemonLspInvocationOwner, Pr13HookOrchestrationAdmissionV1,
    Pr13HookOrchestrationPortV1, Pr13HookOrchestrationRequestV1, Pr13HookOrchestrationTriggerV1,
    admit_registered_pr13_hook_orchestration,
};
// `pub(super)` on these shapes, in their original flat-file home, meant
// "visible to `daemon::service`" (their home's actual parent); nesting them
// one level deeper under `invocation::types` would silently narrow that to
// "visible to `invocation`" only, which breaks the existing sibling reads
// from `service::project_runtime`. Re-export at the same absolute reach the
// definitions themselves now declare via `pub(in crate::daemon::service)`.
pub(crate) use registrars::{
    DaemonAdvisoryRuntimeRegistrar, DaemonAdvisoryRuntimeRegistrationError,
    DaemonConfigurationRuntimeRegistrar, DaemonFeedbackRuntimeRegistrar,
    DaemonFeedbackRuntimeRegistrationError, DaemonLspOwnerRegistrar, DaemonWorkRuntimeRegistrar,
    DoctorConfigurationOutcomeV1,
};
pub(in crate::daemon::service) use types::{
    RegisteredCallableCodeRuntime, RegisteredConfigurationRuntime, RegisteredFeedbackRuntime,
    RegisteredWorkRuntime, SwitchableFeedbackCycleRuntimeV1, UnavailableFeedbackCycleRuntimeV1,
};

#[derive(Clone)]
pub(crate) struct DaemonInvocationService {
    code_index_schedulers: crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    lsp_sessions: Arc<Mutex<BTreeMap<LspSessionId, RuntimeLspSession>>>,
    authorized_lsp_workspaces: Arc<Mutex<BTreeMap<ManifestDigest, AuthorizedDaemonLspWorkspace>>>,
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
            authorized_lsp_workspaces: Arc::new(Mutex::new(BTreeMap::new())),
            context_scout_registries: Arc::new(Mutex::new(BTreeMap::new())),
            project_runtimes: ProjectRuntimeRegistryV1::default(),
            operation_events: daemon_operation_event_authority(),
        }
    }
}
