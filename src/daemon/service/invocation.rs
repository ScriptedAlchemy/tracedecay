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
    callable_code_operations,
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
    ContextScoutSurfaceRequest, GitApplySurfaceRequest, GitPreviewSurfaceRequest,
    GitReadSurfaceRequest,
};
use crate::daemon::callable_code_authorization::DaemonCallableCodeAuthorizationSource;
use crate::daemon::git_transactions::{
    DaemonGitAuthorityStateV1, DaemonGitInvocationOwner, DaemonProjectGitIndexTransactionService,
    capture_exact_snapshot,
};
use crate::daemon::native_integration::DaemonNativeIntegrationOwner;
use tracedecay_application::ConfigurationWireRequestV1;
use tracedecay_usecases::ProjectSourceAccessSnapshot;
use tracedecay_usecases::configuration::{
    AuthorizedActor, ConfigurationAuditQuery, ConfigurationError, ConfigurationMutationAuthority,
    ConfigurationMutationGrantAuthority, ConfigurationMutationGrantAuthorityError,
    ConfigurationMutationGrantAuthorityFuture, ConfigurationRollbackRequest,
    CredentialWriteHandleV1, DirectConfigurationMutation,
    PolicyBackedConfigurationMutationAuthorization, ProjectConfigurationRuntime,
    ScopeResolutionPort, ScopeRevalidationEvidenceV1, WriteOnlyCredentialMutation,
    configuration_layer_scope_digest,
};

use tracedecay_usecases::advisory::{
    AdvisoryDaemonStartupErrorV1, AdvisoryProductionOpenErrorV1, AdvisoryProductionOpenV1,
    AdvisoryProductionStartupRegistrationV1, AdvisoryRuntimeOpenV1,
    open_advisory_production_authorities, register_advisory_daemon_startup,
};
use tracedecay_usecases::feedback::concrete::{
    FeedbackRuntime, FeedbackRuntimeError, ProjectFeedbackStore, open_feedback_runtime,
};
use tracedecay_usecases::feedback::cycle_production::production_proximity_feedback_cycle_input;
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
    ProductionFeedbackCycleProximityPortV1, open_feedback_cycle_runtime,
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
    WorkApplicationOutcomeV1,
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
#[cfg(test)]
use tracedecay_application::{
    CancellationStage, MultiRootExecuteRequestV1, MultiRootScopeSetReadRequestV1,
    ProblemTerminality,
};
use tracedecay_hooks::{HookBoundaryV1, HookEventEnvelopeV2, HookEventV2, HookScopeBindingV1};

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
pub(crate) use primitive::callable_code_request_context;
mod registrars;
mod retained;
pub(in crate::daemon) mod semantic_evaluation;
#[cfg(test)]
mod tests;
mod types;
mod work;
mod work_attempt_exec;
mod work_blocked_interval_recovery;
mod work_routing;

use clock::{current_micros, now_micros, now_millis};
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
#[cfg(test)]
use lsp::*;
#[cfg(test)]
use lsp_delivery::lsp_delivery_attempt;
use lsp_delivery::retain_lsp_delivery_attempt;
use native_integration::execute_native_integration;
use observatory::execute_observatory_read;
use primitive::*;
use registrars::*;
use retained::*;
use types::*;
use work::*;
pub(in crate::daemon) use work_routing::DaemonWorkProposalRoutingAuthorityV1;

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
pub(crate) use types::{
    BoundedHookOrchestratorV1, DaemonLspInvocationOwner, HookOrchestrationAdmissionV1,
    HookOrchestrationRequestV1, HookOrchestrationTriggerV1, HookOrchestrationWorkOutcomeV1,
    admit_registered_hook_orchestration, register_hook_orchestration_runtime,
    unregister_hook_orchestration_runtime,
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
    RegisteredRetainedRuntime, RegisteredWorkRuntime, SwitchableFeedbackCycleRuntimeV1,
    UnavailableFeedbackCycleRuntimeV1,
};

#[derive(Debug)]
pub(crate) enum RegisteredRetainedRequestContextError {
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
pub(crate) struct DaemonInvocationService {
    code_index_schedulers: crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    lsp_admission_open: Arc<Mutex<bool>>,
    lsp_sessions: Arc<Mutex<BTreeMap<LspSessionId, RuntimeLspSession>>>,
    lsp_lease_tasks: Arc<LspLeaseTaskRegistry>,
    authorized_lsp_workspaces: Arc<Mutex<BTreeMap<ManifestDigest, AuthorizedDaemonLspWorkspace>>>,
    context_scout_registries: Arc<
        Mutex<
            BTreeMap<InvocationProjectRuntimeIdentityV1, Arc<ProjectContextScoutAddressRegistryV1>>,
        >,
    >,
    /// Every per-project component, published together under one lock. See
    /// [`ProjectRuntimeRegistryV1`] for why these are not twelve maps.
    project_runtimes: ProjectRuntimeRegistryV1,
    operation_events: OperationEventAuthority,
    github_stack_coordinator:
        Arc<tracedecay_usecases::stack_coordinator::DaemonGitHubStackCoordinatorV1>,
    work_attempt_processes: Arc<work_attempt_exec::WorkAttemptProcessRegistryV1>,
    worktree_holder_admission: crate::daemon::native_integration::WorktreeHolderAdmissionFenceV1,
    session_holder_databases:
        Arc<Mutex<BTreeMap<PathBuf, crate::global_db::RegisteredGlobalDbLeaseV1>>>,
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
            github_stack_coordinator: Arc::new(
                tracedecay_usecases::stack_coordinator::DaemonGitHubStackCoordinatorV1::default(),
            ),
            work_attempt_processes: Arc::new(
                work_attempt_exec::WorkAttemptProcessRegistryV1::default(),
            ),
            worktree_holder_admission:
                crate::daemon::native_integration::daemon_worktree_holder_admission_fence(),
            session_holder_databases: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub(crate) fn github_stack_coordinator(
        &self,
    ) -> Arc<tracedecay_usecases::stack_coordinator::DaemonGitHubStackCoordinatorV1> {
        Arc::clone(&self.github_stack_coordinator)
    }

    pub(crate) async fn registered_retained_request_context(
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
    pub(crate) async fn install_worktree_cleanup_recovery_fences(
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
    pub(crate) async fn mount_session_holder_databases(
        &self,
        databases: impl IntoIterator<Item = crate::global_db::RegisteredGlobalDbLeaseV1>,
    ) {
        let mut mounted = self.session_holder_databases.lock().await;
        for database in databases {
            mounted.insert(database.db_path().to_path_buf(), database);
        }
    }
}

impl crate::daemon::DaemonInvocationState {
    pub(in crate::daemon) fn github_stack_coordinator(
        &self,
    ) -> Arc<tracedecay_usecases::stack_coordinator::DaemonGitHubStackCoordinatorV1> {
        self.service.github_stack_coordinator()
    }
}
