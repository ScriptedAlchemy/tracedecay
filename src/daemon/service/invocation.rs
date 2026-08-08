//! Closed, authenticated daemon invocation protocol.
//!
//! This module deliberately accepts a small typed operation set after the
//! daemon handshake. It is not a generic application invoke endpoint and it
//! never accepts a raw Git request, database selector, or LSP socket address.
//! LSP frames are handled by a daemon-owned protocol actor; the bridge only
//! receives the actor's bounded responses through explicit frame operations.

use std::any::Any;
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, RwLock, Weak};
use std::time::Duration;

use serde::Serialize;
use thiserror::Error;
use tokio::sync::{Mutex, Semaphore};
use tracedecay_application::feedback::{
    FeedbackReadPort, FeedbackRouteAuthorizationPort, FeedbackRuntimeStatePort,
};
use tracedecay_application::{
    AffectedTestsRetrievalPort, AnalyzerAdmittedDiagnosticProviderV1, ApplicationContractError,
    ApplicationOperation, ApplicationOutcome, ApplicationProblem, ApplicationProblemKind,
    ApplicationResult, AuthorityReceipt, AuthorizedScopeSet, AuthorizedScopeSetAuthority,
    CallableCodeAuthorizationPort, CallableCodeOperationKind, CallableCodeQueryService,
    CancellationContext, CancellationObservation, CancellationStage, CancellationState,
    CapabilityGrantId, CapabilityGrantSnapshot, CoverageCompleteness, CoverageDomainState,
    Deadline, DiagnosticProviderIdentity, DisclosureClass, EffectId, EffectReceipt, EffectResult,
    EffectTermination, EvidenceAuthority, EvidenceCoverage, EvidenceDomain, EvidenceIdentity,
    EvidencePacket, GitIndexApplyPortResultV1, GitIndexApplyRequestV1, GitIndexEffectProofV1,
    GitIndexOperationBindingV1, GitIndexPreviewPortResultV1, GitIndexPreviewRequestV1,
    GitIndexRecoveryRequestV1, GitIndexTransactionApplicationError, GitIndexTransactionPort,
    GitIndexTransactionPortError, GitIndexTransactionService, IdempotencyKey,
    MultiRootScopeSetCasRequestV1, MultiRootScopeSetCasResultV1, MultiRootScopeSetCasStatusV1,
    Omission, OmissionReason, OperationBudgetUsage, OperationReceipt, OperationTermination,
    PageRequest, PageState, PolicyDecisionRef, PolicyEvaluationContextV1,
    PolicyEvaluatorCompositionV1, PolicyEvidenceHorizonV1, PreviewId, PreviewResult,
    ReconciliationState, RequestAdmission, RequestContext, RequestId, ResolvedScope,
    RetryDirective, SafeDiagnostic, TaskHandoffError, TaskHandoffGrant, TaskHandoffRedeemed,
    TaskHandoffToken, TemporalState, WorkProjectionApplicationError, WorkflowCoordinationError,
    WorkflowDefinitionDisposition, WorkflowDefinitionLifecycleCommand,
    WorkflowEffectAuthorityPortV1, WorkflowEffectIdentityV1, WorkflowEffectOperationV1,
    WorkflowEffectOutcomeV1, WorkflowEffectPreparedV1, WorkflowEffectProblemV1,
    WorkflowEffectReceiptContextV1, WorkflowEffectSuccessV1, WorkflowEffectTerminalV1,
    WorkflowLifecycleOperation, callable_code_operations, prepare_task_handoff_issue,
    prepare_task_handoff_redeem, prepare_workflow_definition_registration,
};
use tracedecay_domain::configuration::{
    CandidateDispositionV1, ConfigurationGrantId, ConfigurationGrantReceiptId,
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationMutationEffectV1,
    ConfigurationMutationGrantReceiptV1, ConfigurationMutationOperationV1,
    ConfigurationMutationSinkV1, ConfigurationRevisionId, ConfigurationSnapshotV1,
    ProtectedApplyRequest,
};
use tracedecay_domain::{
    AccessPolicyDigest, ActorId, ComponentVersion, FeedbackCycleTerminationV1, GitHeadStateV1,
    GitIndexPreviewId, GitIndexPreviewInputV1, GitIndexTransactionOperationV1,
    GitIndexTransactionReceiptV1, ManifestDigest, ProjectId, ScopeSetId, ScopeSetRevision,
    UserProfileId, UtcMicros, WorkAuthority, canonical_sha256,
};
use tracedecay_lsp::analyzer::broker::DiagnosticBroker;
use tracedecay_lsp::analyzer::client::LspRefreshTimeouts;
use tracedecay_lsp::{
    AdmittedRoot, AuthorizedLspSession, AuthorizedLspWorkspace, ClientFrameAdmission,
    DaemonLspRuntimeSession, DaemonLspSessionEndpoint, DiagnosticTrigger, FeedbackCycleRequest,
    FeedbackCycleRuntimePort, GatewayCapabilities, LSP_SESSION_TTL_MS, LspEndpointError,
    LspRuntimeFailure, LspRuntimeFuture, LspSessionAccess, LspSessionAdmissionPort,
    LspSessionCredential, LspSessionId, LspSessionOpenRequest, LspSessionRegistry,
    SessionLifecycle, UpstreamCapabilities,
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
    ProjectRuntimeRegistryV1, RegisteredObservabilityProducerV1,
};
use crate::agents::context_scout_ports::{
    AdmittedContextScoutHookV1, ContextScoutLifecycleAddressV1,
    ProjectContextScoutAddressRegistryV1,
};
use crate::application_surface::{
    ConfigurationSurfaceRequest, ContextScoutSurfaceRequest, GitApplySurfaceRequest,
    GitPreviewSurfaceRequest, GitReadSurfaceRequest,
};
use crate::daemon::callable_code_authorization::DaemonCallableCodeAuthorizationSource;
use crate::daemon::git_transactions::{
    DaemonGitAuthorityStateV1, DaemonGitInvocationOwner, DaemonProjectGitIndexTransactionService,
    capture_exact_snapshot,
};
use crate::daemon::native_integration::DaemonNativeIntegrationOwner;
use tracedecay_usecases::ProjectSourceAccessSnapshot;
use tracedecay_usecases::advisory::{
    AdvisoryCycleOutcome, AdvisoryDaemonStartupErrorV1, AdvisoryDaemonStartupRegistrationV1,
    AdvisoryHookLookupNoticeV1, AdvisoryProductionOpenErrorV1, AdvisoryProductionOpenV1,
    AdvisoryProductionStartupRegistrationV1, AdvisoryProviderAuthoritiesV1, AdvisoryRuntimeOpenV1,
    CanonicalProximityEvidenceAuthorityV1, CiExactEvidenceAuthorityV1, CiReadOnlyProviderArchiveV1,
    GitHubCanonicalReviewAnchorAuthorityV1, GitHubCurrentBranchRemapper,
    open_advisory_production_authorities, register_advisory_daemon_startup,
};
use tracedecay_usecases::configuration::{
    AuthorizedActor, ConfigurationAuditQuery, ConfigurationControlStore, ConfigurationError,
    ConfigurationMutationAuthority, ConfigurationMutationGrantAuthority,
    ConfigurationMutationGrantAuthorityError, ConfigurationMutationGrantAuthorityFuture,
    ConfigurationRollbackRequest, CredentialWriteHandleV1, DirectConfigurationMutation,
    PolicyBackedConfigurationMutationAuthorization, ProjectConfigurationRuntime,
    ScopeResolutionPort, ScopeRevalidationEvidenceV1, WriteOnlyCredentialMutation,
    configuration_layer_scope_digest,
};
use tracedecay_usecases::feedback::concrete::{
    FeedbackRuntime, FeedbackRuntimeError, ProjectFeedbackStore, open_feedback_runtime,
};
use tracedecay_usecases::feedback::cycle_production::{
    ProductionFeedbackCycleProximityPortV1, production_proximity_feedback_cycle_input,
};
use tracedecay_usecases::feedback::observations::{
    FeedbackAnchorOperationV1, FeedbackArgumentRejectionClassV1, FeedbackDeliveryRouteV1,
    FeedbackObservationEmitterV1, FeedbackOperationV1, FeedbackOutcomeV1,
    FeedbackRejectedArgumentV1, FeedbackSourceEventV1,
};
use tracedecay_usecases::feedback::owner::{
    DaemonFeedbackReadOwnerV1, FeedbackCanonicalProjectionKindV1, FeedbackReadInvocationResultV1,
    FeedbackReadOperationV1, FeedbackReadOwnerErrorV1, FeedbackReadRequestAuthority,
};
use tracedecay_usecases::feedback::{
    FeedbackCycleLspInput, FeedbackCycleRuntime, FeedbackCycleRuntimeError,
    open_feedback_cycle_runtime,
};
use tracedecay_usecases::lsp_runtime::{
    DaemonLspSessionFactory, LspCodeIndexProjectionIdentityPort, lsp_session_factory,
};
use tracedecay_usecases::operation_stream::{
    OperationEmitter, OperationEventAuthority, OperationKind, operation_event_authority,
};
use tracedecay_usecases::primitives::{
    PrimitiveDispatch, PrimitiveInvocation, PrimitiveProjectRuntime, PrimitiveRequest,
};
use tracedecay_usecases::semantic_runtime::{
    ProductionSemanticConfigurationOperationV1, SemanticActivationCoordinationErrorV1,
    SemanticProtectedActivationOperationV1, SemanticProtectedRollbackOperationV1,
};
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
    HandoffApplicationInvocationV1, HandoffApplicationOutcomeV1, WorkApplicationInvocationV1,
    WorkApplicationOutcomeV1, WorkflowApplicationInvocation, WorkflowApplicationOutcome,
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
use tracedecay_application::{MultiRootExecuteRequestV1, MultiRootScopeSetReadRequestV1};
use tracedecay_hooks::{
    HookBoundaryV1, HookEventEnvelopeV2, HookEventV2, HookFeedbackDeliveryPortV1,
    HookScopeBindingV1,
};

// Structural split: production logic now lives in the child modules below;
// this file remains the stable external path (`service::invocation::*`).
mod clock;
mod configuration;
mod dispatch;
mod feedback;
mod git;
mod handoff;
mod invocation_observability;
mod lsp;
mod lsp_delivery;
mod native_integration;
mod observability_producer;
mod primitive;
mod registrars;
mod retained;
pub(in crate::daemon) mod semantic_evaluation;
#[cfg(test)]
mod tests;
mod types;
mod work;
mod work_attempt_exec;
mod work_blocked_interval_recovery;

use clock::{current_micros, now_micros, now_millis};
use configuration::*;
use feedback::*;
use git::*;
use handoff::*;
#[cfg(test)]
use invocation_observability::invocation_rejected_argument;
use invocation_observability::{
    emit_invocation_observation, feedback_observation_operation, invocation_observation_subject,
    invocation_problem_rejected_argument, is_observable_operation, observe_invocation_response,
};
use lsp::PublishedCodeIndexWorkspaceDocuments;
#[cfg(test)]
use lsp::*;
#[cfg(test)]
use lsp_delivery::lsp_delivery_attempt;
use lsp_delivery::retain_lsp_delivery_attempt;
use native_integration::execute_native_integration;
use primitive::*;
use registrars::*;
use retained::*;
use types::*;
use work::*;

pub(crate) use configuration::{
    DaemonSemanticRuntimeRegistrar, DaemonSemanticRuntimeRegistrationError,
};
pub(crate) use feedback::{
    DaemonAdvisoryCycleInvocationFuture, DaemonAdvisoryCycleInvocationOwner,
    DaemonAdvisoryCycleInvocationPort, DaemonAdvisoryCycleInvocationRequest,
    DaemonFeedbackInvocationOwner, advisory_cycle_invocation_result,
    daemon_operation_event_authority,
};
pub(crate) use primitive::{
    DaemonContextScoutRuntimeRegistrar, DaemonContextScoutRuntimeRegistrationError,
    DaemonPrimitiveRuntimeRegistrar, DaemonPrimitiveRuntimeRegistrationError,
};
pub(in crate::daemon) use types::observe_accepted_feedback_cycle_terminal;
pub(crate) use types::{
    AdvisoryRuntimeReadinessV1, AdvisoryRuntimeUnavailableReasonV1, BoundedHookOrchestratorV1,
    DaemonLspInvocationOwner, DeferredHookOrchestratorV1, HookOrchestrationAdmissionV1,
    HookOrchestrationPortV1, HookOrchestrationRequestV1, HookOrchestrationTriggerV1,
    admit_registered_hook_orchestration,
};
// `pub(super)` on these shapes, in their original flat-file home, meant
// "visible to `daemon::service`" (their home's actual parent); nesting them
// one level deeper under `invocation::types` would silently narrow that to
// "visible to `invocation`" only, which breaks the existing sibling reads
// from `service::project_runtime`. Re-export at the same absolute reach the
// definitions themselves now declare via `pub(in crate::daemon::service)`.
pub(crate) use registrars::{
    DaemonAdvisoryRuntimeRegistrar, DaemonConfigurationRuntimeRegistrar,
    DaemonFeedbackRuntimeRegistrar, DaemonFeedbackRuntimeRegistrationError,
    DaemonLspOwnerRegistrar, DaemonRetainedRuntimeRegistrar, DaemonWorkRuntimeRegistrar,
};
pub(in crate::daemon::service) use types::{
    RegisteredCallableCodeRuntime, RegisteredConfigurationRuntime, RegisteredFeedbackRuntime,
    RegisteredHookOrchestrationRuntimeV1, RegisteredRetainedRuntime, RegisteredWorkRuntime,
    SwitchableFeedbackCycleRuntimeV1, UnavailableFeedbackCycleRuntimeV1,
};

#[derive(Clone)]
pub(crate) struct DaemonInvocationService {
    code_index_schedulers: crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    lsp_admission_open: Arc<Mutex<bool>>,
    lsp_sessions: Arc<Mutex<BTreeMap<LspSessionId, RuntimeLspSession>>>,
    lsp_lease_tasks: Arc<LspLeaseTaskRegistry>,
    authorized_lsp_workspaces: Arc<Mutex<BTreeMap<ManifestDigest, AuthorizedDaemonLspWorkspace>>>,
    context_scout_registries:
        Arc<Mutex<BTreeMap<ProjectId, Arc<ProjectContextScoutAddressRegistryV1>>>>,
    /// Every per-project component, published together under one lock. See
    /// [`ProjectRuntimeRegistryV1`] for why these are not twelve maps.
    project_runtimes: ProjectRuntimeRegistryV1,
    operation_events: OperationEventAuthority,
    work_attempt_processes: Arc<work_attempt_exec::WorkAttemptProcessRegistryV1>,
    worktree_holder_admission: crate::daemon::native_integration::WorktreeHolderAdmissionFenceV1,
    session_holder_databases:
        Arc<Mutex<BTreeMap<PathBuf, Arc<crate::global_db::RegisteredGlobalDb>>>>,
}

#[cfg(test)]
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
            lsp_admission_open: Arc::new(Mutex::new(true)),
            lsp_sessions: Arc::new(Mutex::new(BTreeMap::new())),
            lsp_lease_tasks: Arc::new(LspLeaseTaskRegistry::default()),
            authorized_lsp_workspaces: Arc::new(Mutex::new(BTreeMap::new())),
            context_scout_registries: Arc::new(Mutex::new(BTreeMap::new())),
            project_runtimes: ProjectRuntimeRegistryV1::default(),
            operation_events: daemon_operation_event_authority(),
            work_attempt_processes: Arc::new(
                work_attempt_exec::WorkAttemptProcessRegistryV1::default(),
            ),
            worktree_holder_admission:
                crate::daemon::native_integration::daemon_worktree_holder_admission_fence(),
            session_holder_databases: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Installs every durable worktree-cleanup recovery fence before project
    /// open publishes holder-capable Work and LSP runtimes.
    pub(crate) async fn install_worktree_cleanup_recovery_fences(
        &self,
        owner: &DaemonNativeIntegrationOwner,
    ) -> Result<(), tracedecay_application::NativeIntegrationPortError> {
        let roots = owner.cleanup_recovery_roots()?;
        self.worktree_holder_admission
            .mark_recovery_required(roots)
            .await;
        Ok(())
    }

    /// Retains canonical profile/user session stores whose active rows remain
    /// cleanup holders even when no project-store mirror exists.
    pub(crate) async fn mount_session_holder_databases(
        &self,
        databases: impl IntoIterator<Item = Arc<crate::global_db::RegisteredGlobalDb>>,
    ) {
        let mut mounted = self.session_holder_databases.lock().await;
        for database in databases {
            mounted.insert(database.db_path().to_path_buf(), database);
        }
    }
}
