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
#[cfg(any(test, feature = "test-helpers"))]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, RwLock, Weak};
use std::time::Duration;

use serde::Serialize;
use thiserror::Error;
use tokio::sync::{Mutex, Notify, Semaphore};
use tracedecay_application::feedback::{
    FeedbackReadPort, FeedbackRouteAuthorizationPort, FeedbackRuntimeStatePort,
};
use tracedecay_application::{
    AffectedTestsRetrievalPort, AnalyzerAdmittedDiagnosticProviderV1, ApplicationContractError,
    ApplicationOperation, ApplicationOutcome, ApplicationProblem, ApplicationProblemKind,
    ApplicationResult, AuthorityReceipt, AuthorizedScopeSet, AuthorizedScopeSetAuthority,
    CallableCodeAuthorizationPort, CallableCodeOperationKind, CallableCodeQueryService,
    CancellationContext, CancellationState, CapabilityGrantId, CapabilityGrantSnapshot,
    CoverageCompleteness, CoverageDomainState, Deadline, DiagnosticProviderIdentity,
    DisclosureClass, EffectId, EffectReceipt, EffectResult, EffectTermination, EvidenceAuthority,
    EvidenceCoverage, EvidenceDomain, EvidenceIdentity, EvidencePacket, GitIndexApplyPortResultV1,
    GitIndexApplyRequestV1, GitIndexEffectProofV1, GitIndexOperationBindingV1,
    GitIndexPreviewPortResultV1, GitIndexPreviewRequestV1, GitIndexRecoveryRequestV1,
    GitIndexTransactionApplicationError, GitIndexTransactionPort, GitIndexTransactionPortError,
    GitIndexTransactionService, IdempotencyKey, MultiRootScopeSetCasRequestV1,
    MultiRootScopeSetCasResultV1, MultiRootScopeSetCasStatusV1, Omission, OmissionReason,
    OperationBudgetUsage, OperationReceipt, OperationTermination, PageRequest, PageState,
    PolicyDecisionRef, PolicyEvaluationContextV1, PolicyEvaluatorCompositionV1,
    PolicyEvidenceHorizonV1, PreviewId, PreviewResult, ReconciliationState, RequestAdmission,
    RequestContext, RequestId, ResolvedScope, RetryDirective, SafeDiagnostic, TemporalState,
    WorkEvidenceRetrievalPortV1, callable_code_operations,
};
use tracedecay_domain::configuration::{
    CandidateDispositionV1, ConfigurationGrantId, ConfigurationGrantReceiptId,
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationMutationEffectV1,
    ConfigurationMutationGrantReceiptV1, ConfigurationMutationOperationV1,
    ConfigurationMutationSinkV1, ConfigurationRevisionId, ConfigurationSnapshotV1,
    ProtectedApplyRequest,
};
use tracedecay_domain::{
    AccessPolicyDigest, ActorId, ComponentVersion, GitHeadStateV1, GitIndexPreviewId,
    GitIndexPreviewInputV1, GitIndexTransactionOperationV1, GitIndexTransactionReceiptV1,
    ManifestDigest, ProjectId, ScopeSetId, ScopeSetRevision, UserProfileId, UtcMicros,
    WorkAuthority, canonical_sha256,
};
use tracedecay_lsp::analyzer::broker::DiagnosticBroker;
use tracedecay_lsp::analyzer::client::LspRefreshTimeouts;
use tracedecay_lsp::{
    AdmittedRoot, AuthorizedLspSession, AuthorizedLspWorkspace, ClientFrameAdmission,
    DaemonLspRuntimeSession, DaemonLspSessionEndpoint, DiagnosticTrigger, FeedbackCycleRequest,
    FeedbackCycleRuntimePort, GatewayCapabilities, LSP_SESSION_TTL_MS, LspEndpointError,
    LspRuntimeFailure, LspRuntimeFuture, LspSessionAdmissionPort, LspSessionOpenRequest,
    LspSessionRegistry, SessionLifecycle, UpstreamCapabilities,
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

use crate::project_runtime::{
    FeedbackCyclePublicationError, ProjectRuntimeAlreadyRegistered, ProjectRuntimeRegistryError,
    ProjectRuntimeRegistryV1, RegisteredObservabilityProducerV1,
    RegisteredSemanticActivationOwnerV1, StoreObservabilityMountErrorV1, StoreObservabilityMountV1,
    StoreObservabilityRegistryV1,
};
use tracedecay_agent_hosts::agents::context_scout_ports::{
    AdmittedContextScoutHookV1, ContextScoutLifecycleAddressV1,
    ProjectContextScoutAddressRegistryV1,
};
use tracedecay_agent_hosts::native_integration::DaemonNativeIntegrationOwner;
use tracedecay_application::ConfigurationWireRequestV1;
use tracedecay_application::git::{GitApplySurfaceRequest, GitPreviewSurfaceRequest};
use tracedecay_code_index_runtime::git_transactions::{
    DaemonGitAuthorityStateV1, DaemonGitInvocationOwner, DaemonProjectGitIndexTransactionService,
    capture_exact_snapshot,
};
use tracedecay_configuration::{
    AuthorizedActor, ConfigurationAuditQuery, ConfigurationError, ConfigurationMutationAuthority,
    ConfigurationMutationGrantAuthority, ConfigurationMutationGrantAuthorityError,
    ConfigurationMutationGrantAuthorityFuture, ConfigurationRollbackRequest,
    CredentialWriteHandleV1, DirectConfigurationMutation,
    PolicyBackedConfigurationMutationAuthorization, ProjectConfigurationRuntime,
    ScopeResolutionPort, ScopeRevalidationEvidenceV1, WriteOnlyCredentialMutation,
    configuration_layer_scope_digest,
};
use tracedecay_daemon_protocol::{ContextScoutSurfaceRequest, GitReadSurfaceRequest};
use tracedecay_usecases::CallableCodeAuthorizationSourcePort;
use tracedecay_usecases::ProjectSourceAccessSnapshot;

use tracedecay_application::feedback::observations::{
    FeedbackAnchorOperationV1, FeedbackArgumentRejectionClassV1, FeedbackDeliveryRouteV1,
    FeedbackOperationV1, FeedbackOutcomeV1, FeedbackRejectedArgumentV1, FeedbackSourceEventV1,
};
use tracedecay_application::request_identity::{
    GlobalOpaqueIdentityKind, LogicalEffectIdempotencyDomain, derive_logical_effect_idempotency,
    mint_global_opaque_id,
};
use tracedecay_application::retrieval::{PrimitiveInvocation, PrimitiveRequest};
#[cfg(test)]
use tracedecay_application::{
    CancellationStage, MultiRootExecuteRequestV1, MultiRootScopeSetReadRequestV1,
    ProblemTerminality,
};
#[cfg(test)]
pub(crate) use tracedecay_daemon_protocol::{
    DAEMON_INVOCATION_PROTOCOL, DAEMON_INVOCATION_REVISION, parse_daemon_invocation_request,
};
pub(crate) use tracedecay_daemon_protocol::{
    DaemonFeedbackResult, DaemonGitEffectResult, DaemonGitPreviewResult, DaemonInvocationOperation,
    DaemonInvocationOutcome, DaemonInvocationPayload, DaemonInvocationProblem,
    DaemonInvocationRequest, DaemonInvocationResponse, DaemonLspSessionAccess,
    HandoffApplicationInvocationV1, HandoffApplicationOutcomeV1, LspSessionAccess,
    LspSessionCredential, LspSessionId, WorkApplicationInvocationV1, WorkApplicationOutcomeV1,
};
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_hooks::{HookBoundaryV1, HookEventEnvelopeV2, HookEventV2, HookScopeBindingV1};
use tracedecay_runtime_core::db::Database;
use tracedecay_usecases::advisory::{
    AdvisoryDaemonStartupErrorV1, AdvisoryProductionOpenErrorV1, AdvisoryProductionOpenV1,
    AdvisoryProductionStartupRegistrationV1, AdvisoryRuntimeOpenV1,
    open_advisory_production_authorities, register_advisory_daemon_startup,
};
use tracedecay_usecases::feedback::concrete::{
    FeedbackRuntime, FeedbackRuntimeError, ProjectFeedbackStore, open_feedback_runtime,
};
use tracedecay_usecases::feedback::cycle_production::production_proximity_feedback_cycle_input;
use tracedecay_usecases::feedback::observations::FeedbackObservationEmitterV1;
use tracedecay_usecases::feedback::owner::{
    DaemonFeedbackReadOwnerV1, FeedbackCanonicalProjectionKindV1, FeedbackReadInvocationResultV1,
    FeedbackReadOperationV1, FeedbackReadOwnerErrorV1, FeedbackReadRequestAuthority,
};
use tracedecay_usecases::feedback::{
    FeedbackCycleLspInput, FeedbackCycleRuntime, FeedbackCycleRuntimeError,
    ProductionFeedbackCycleProximityPortV1, open_feedback_cycle_runtime,
};
use tracedecay_usecases::lsp_runtime::{
    DaemonLspSessionFactory, LspCodeIndexProjectionIdentityPort, lsp_session_factory,
    production_semantic_authorities,
};
use tracedecay_usecases::operation_stream::{
    OperationEmitter, OperationEventAuthority, OperationKind, operation_event_authority,
};
use tracedecay_usecases::primitives::{PrimitiveDispatch, PrimitiveProjectRuntime};
use tracedecay_usecases::semantic_runtime::{
    ProductionSemanticConfigurationOperationV1, SemanticActivationCoordinationErrorV1,
    SemanticProtectedActivationOperationV1, SemanticProtectedRollbackOperationV1,
};

// Structural split: production logic now lives in the child modules below;
// this file remains the stable external path (`service::invocation::*`).
mod administrative_effect;
mod clock;
mod configuration;
mod dispatch;
mod feedback;
mod git;
mod github_stack_signal;
mod handoff;
mod invocation_observability;
mod lsp;
mod lsp_delivery;
mod native_integration;
mod observability_producer;
mod observatory;
mod primitive;
pub use primitive::callable_code_request_context;
mod recovery_schedule;
mod registrars;
mod retained;
mod semantic_activation;
pub mod semantic_evaluation;
#[cfg(test)]
mod tests;
mod types;
mod work;
mod work_attempt_exec;
mod work_blocked_interval_recovery;
mod work_routing;

use clock::now_micros;
pub use clock::{current_micros, now_millis};
use configuration::*;
use feedback::*;
use git::*;
use github_stack_signal::execute_github_stack_signal_expand;
use handoff::*;
use invocation_observability::{
    emit_invocation_observation, feedback_observation_operation, invocation_observation_subject,
    invocation_problem_rejected_argument, is_observable_operation, observe_invocation_response,
};
#[cfg(test)]
use invocation_observability::{invocation_rejected_argument, invocation_response_outcome};
use lsp::PublishedCodeIndexWorkspaceDocuments;
pub use lsp_delivery::{
    LspDeliverySettlementAdmissionV1, lsp_delivery_attempt, retain_lsp_delivery_attempt,
};
use native_integration::execute_native_integration;
use observatory::execute_observatory_read;
use primitive::*;
use retained::*;
use types::*;
use work::*;
pub use work_routing::DaemonWorkProposalRoutingAuthorityV1;

pub use configuration::{DaemonSemanticRuntimeRegistrar, DaemonSemanticRuntimeRegistrationError};
pub use feedback::{
    DaemonAdvisoryCycleInvocationFuture, DaemonAdvisoryCycleInvocationOwner,
    DaemonAdvisoryCycleInvocationPort, DaemonAdvisoryCycleInvocationRequest,
    DaemonFeedbackInvocationOwner, advisory_cycle_invocation_result,
    daemon_operation_event_authority,
};
pub use primitive::{
    DaemonContextScoutRuntimeRegistrar, DaemonContextScoutRuntimeRegistrationError,
    DaemonPrimitiveRuntimeRegistrar, DaemonPrimitiveRuntimeRegistrationError,
};
pub use types::{
    BoundedHookOrchestratorV1, DaemonLspInvocationOwner, HookOrchestrationAdmissionV1,
    HookOrchestrationRequestV1, HookOrchestrationTriggerV1, HookOrchestrationWorkOutcomeV1,
    MAX_COALESCED_HOOK_COMPLETIONS, admit_registered_hook_orchestration,
    register_hook_orchestration_runtime, unregister_hook_orchestration_runtime,
};
// `pub(super)` on these shapes, in their original flat-file home, meant
// "visible to `daemon::service`" (their home's actual parent); nesting them
// one level deeper under `invocation::types` would silently narrow that to
// "visible to `invocation`" only, which breaks the existing sibling reads
// from `service::project_runtime`. Re-export at the same absolute reach the
// definitions themselves now declare via `pub`.
#[cfg(any(test, feature = "test-helpers"))]
pub use lsp::canonicalize_lsp_roots;
pub use lsp::{LSP_WORKSPACE_CAPABILITY_ID_V1, LSP_WORKSPACE_USE_CASE_ID_V1};
#[cfg(any(test, feature = "test-helpers"))]
pub use registrars::DaemonFeedbackPublicationTestGate;
#[cfg(any(test, feature = "test-helpers"))]
pub use registrars::mounted_configuration_layers;
pub use registrars::{
    DaemonAdvisoryRuntimeRegistrar, DaemonAdvisoryRuntimeRegistrationError,
    DaemonConfigurationGrantAuthority, DaemonConfigurationRuntimeRegistrar,
    DaemonFeedbackRuntimeRegistrar, DaemonFeedbackRuntimeRegistrationError,
    DaemonLspOwnerRegistrar, DaemonNativeIntegrationRuntimeRegistrar,
    DaemonRetainedRuntimeRegistrar, DaemonWorkRuntimeRegistrar,
};
#[cfg(any(test, feature = "test-helpers"))]
pub use types::{
    AuthorizedDaemonLspWorkspace, InvocationProjectRuntimeIdentityV1, LspLeaseTaskRegistry,
    RuntimeLspSession,
};
pub use types::{
    RegisteredCallableCodeRuntime, RegisteredConfigurationRuntime, RegisteredFeedbackRuntime,
    RegisteredRetainedRuntime, RegisteredWorkRuntime, SwitchableFeedbackCycleRuntimeV1,
    UnavailableFeedbackCycleRuntimeV1,
};
#[cfg(any(test, feature = "test-helpers"))]
pub use work::execute_work_application;
#[cfg(any(test, feature = "test-helpers"))]
pub use work_attempt_exec::WorkAttemptProcessRegistryV1;

#[derive(Debug)]
pub enum RegisteredRetainedRequestContextError {
    Application(ApplicationProblem),
    Runtime(TraceDecayError),
}

fn retained_request_admission_problem(admission: RequestAdmission) -> Option<ApplicationProblem> {
    match admission {
        RequestAdmission::Admitted => None,
        RequestAdmission::Cancelled => Some(ApplicationProblem::cancelled_before_admission()),
        RequestAdmission::TimedOut => Some(ApplicationProblem::timed_out_before_admission()),
    }
}

#[derive(Clone)]
pub struct DaemonInvocationService {
    code_index_schedulers:
        tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    #[cfg(any(test, feature = "test-helpers"))]
    pub lsp_admission_open: Arc<Mutex<bool>>,
    #[cfg(not(any(test, feature = "test-helpers")))]
    lsp_admission_open: Arc<Mutex<bool>>,
    #[cfg(any(test, feature = "test-helpers"))]
    pub lsp_sessions: Arc<Mutex<BTreeMap<LspSessionId, RuntimeLspSession>>>,
    #[cfg(not(any(test, feature = "test-helpers")))]
    lsp_sessions: Arc<Mutex<BTreeMap<LspSessionId, RuntimeLspSession>>>,
    #[cfg(any(test, feature = "test-helpers"))]
    pub lsp_lease_tasks: Arc<LspLeaseTaskRegistry>,
    #[cfg(not(any(test, feature = "test-helpers")))]
    lsp_lease_tasks: Arc<LspLeaseTaskRegistry>,
    #[cfg(any(test, feature = "test-helpers"))]
    pub authorized_lsp_workspaces:
        Arc<Mutex<BTreeMap<ManifestDigest, AuthorizedDaemonLspWorkspace>>>,
    #[cfg(not(any(test, feature = "test-helpers")))]
    authorized_lsp_workspaces: Arc<Mutex<BTreeMap<ManifestDigest, AuthorizedDaemonLspWorkspace>>>,
    context_scout_registries: Arc<
        Mutex<
            BTreeMap<InvocationProjectRuntimeIdentityV1, Arc<ProjectContextScoutAddressRegistryV1>>,
        >,
    >,
    /// Every per-project component, published together under one lock. See
    /// [`ProjectRuntimeRegistryV1`] for why these are not twelve maps.
    pub project_runtimes: ProjectRuntimeRegistryV1,
    /// Observability owners keyed by exact registered-store authority.
    /// Project roots registered in [`Self::project_runtimes`] hold aliases
    /// onto these, so linked worktrees share one producer and one
    /// store-keyed delivery settlement recorder.
    store_observability: StoreObservabilityRegistryV1,
    operation_events: OperationEventAuthority,
    github_stack_coordinator:
        Arc<tracedecay_usecases::stack_coordinator::DaemonGitHubStackCoordinatorV1>,
    #[cfg(any(test, feature = "test-helpers"))]
    pub work_attempt_processes: Arc<work_attempt_exec::WorkAttemptProcessRegistryV1>,
    #[cfg(not(any(test, feature = "test-helpers")))]
    work_attempt_processes: Arc<work_attempt_exec::WorkAttemptProcessRegistryV1>,
    worktree_holder_admission:
        tracedecay_agent_hosts::native_integration::WorktreeHolderAdmissionFenceV1,
    session_holder_databases:
        Arc<Mutex<BTreeMap<PathBuf, tracedecay_global_db::RegisteredGlobalDbLeaseV1>>>,
    /// Per-project fan-out of observed native-integration transaction
    /// statuses. The invocation handler publishes; LSP sessions read and
    /// notify. Created on demand under one project-root key shared by both.
    native_integration_status_broadcasts: Arc<
        Mutex<
            BTreeMap<
                PathBuf,
                Arc<tracedecay_usecases::native_integration::NativeIntegrationStatusBroadcastV1>,
            >,
        >,
    >,
}

impl Default for DaemonInvocationService {
    fn default() -> Self {
        Self::with_code_index_schedulers(
            tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1::with_resident_memory_and_progress_producer_incarnation(
                1,
                std::sync::Arc::new(
                    tracedecay_runtime_core::resident_memory::ProcessResidentMemoryV1::new(
                        tracedecay_runtime_core::resident_memory::DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
                    ),
                ),
                1,
            ),
        )
    }
}

impl DaemonInvocationService {
    pub fn with_code_index_schedulers(
        code_index_schedulers: tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    ) -> Self {
        Self {
            code_index_schedulers,
            lsp_admission_open: Arc::new(Mutex::new(true)),
            lsp_sessions: Arc::new(Mutex::new(BTreeMap::new())),
            lsp_lease_tasks: Arc::new(LspLeaseTaskRegistry::default()),
            authorized_lsp_workspaces: Arc::new(Mutex::new(BTreeMap::new())),
            context_scout_registries: Arc::new(Mutex::new(BTreeMap::new())),
            project_runtimes: ProjectRuntimeRegistryV1::default(),
            store_observability: StoreObservabilityRegistryV1::default(),
            operation_events: daemon_operation_event_authority(),
            github_stack_coordinator: Arc::new(
                tracedecay_usecases::stack_coordinator::DaemonGitHubStackCoordinatorV1::default(),
            ),
            work_attempt_processes: Arc::new(
                work_attempt_exec::WorkAttemptProcessRegistryV1::default(),
            ),
            worktree_holder_admission:
                tracedecay_agent_hosts::native_integration::daemon_worktree_holder_admission_fence(),
            session_holder_databases: Arc::new(Mutex::new(BTreeMap::new())),
            native_integration_status_broadcasts: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// The one status broadcast shared by the native-integration invocation
    /// handler and every LSP session factory registered for `project_root`.
    #[hotpath::skip]
    pub async fn native_integration_status_broadcast(
        &self,
        project_root: &Path,
    ) -> Arc<tracedecay_usecases::native_integration::NativeIntegrationStatusBroadcastV1> {
        let mut broadcasts = self.native_integration_status_broadcasts.lock().await;
        Arc::clone(broadcasts.entry(project_root.to_path_buf()).or_default())
    }

    pub fn github_stack_coordinator(
        &self,
    ) -> Arc<tracedecay_usecases::stack_coordinator::DaemonGitHubStackCoordinatorV1> {
        Arc::clone(&self.github_stack_coordinator)
    }

    #[hotpath::measure(label = "daemon.service.invocation.retained_context", future = true)]
    pub async fn registered_retained_request_context(
        &self,
        project_root: &Path,
        request_id: RequestId,
        deadline: Deadline,
        cancellation: CancellationContext,
        observed_at: UtcMicros,
        operation: &ApplicationOperation,
    ) -> Result<RequestContext, RegisteredRetainedRequestContextError> {
        let registered = self
            .project_runtimes
            .get::<RegisteredRetainedRuntime>(project_root)
            .await
            .ok_or_else(|| {
                RegisteredRetainedRequestContextError::Runtime(TraceDecayError::Config {
                    message: "automation retained application authority is unavailable".to_owned(),
                })
            })?;
        let effective_deadline = Deadline {
            expires_at: UtcMicros(deadline.expires_at.0.min(registered.grant.expires_at.0)),
        };
        let context = RequestContext::new(
            registered.actor,
            registered.scope,
            registered.grant,
            request_id,
            effective_deadline,
            cancellation,
        )
        .map_err(|error| {
            RegisteredRetainedRequestContextError::Runtime(TraceDecayError::Config {
                message: format!("automation retained request context is invalid: {error}"),
            })
        })?;
        if let Some(problem) = retained_request_admission_problem(context.admission_at(observed_at))
        {
            return Err(RegisteredRetainedRequestContextError::Application(problem));
        }
        if !context.allows(operation.capability_id(), operation.use_case_id()) {
            return Err(RegisteredRetainedRequestContextError::Runtime(
                TraceDecayError::Config {
                    message: "automation retained application request is not admitted".to_owned(),
                },
            ));
        }
        Ok(context)
    }

    /// Installs every durable worktree-cleanup recovery fence before project
    /// open publishes holder-capable Work and LSP runtimes.
    #[hotpath::measure(
        label = "daemon.service.invocation.install_cleanup_fences",
        future = true
    )]
    pub async fn install_worktree_cleanup_recovery_fences(
        &self,
        owner: &DaemonNativeIntegrationOwner,
    ) -> Result<(), tracedecay_application::NativeIntegrationPortError> {
        let roots = owner.cleanup_recovery_roots()?;
        self.worktree_holder_admission
            .mark_recovery_required(roots)
            .await;
        owner.recover_worktree_cleanups().await?;
        Ok(())
    }

    /// Retains canonical profile/user session stores whose active rows remain
    /// cleanup holders even when no project-store mirror exists.
    #[hotpath::skip]
    pub async fn mount_session_holder_databases(
        &self,
        databases: impl IntoIterator<Item = tracedecay_global_db::RegisteredGlobalDbLeaseV1>,
    ) {
        let mut mounted = self.session_holder_databases.lock().await;
        for database in databases {
            mounted.insert(database.db_path().to_path_buf(), database);
        }
    }
}
