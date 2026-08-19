//! Project-open composition for the canonical feedback and advisory owners.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tracedecay_application::feedback::{
    FeedbackRuntimeStatePort, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
    GITHUB_REVIEW_INGEST_USE_CASE_ID_V1, GitHubReviewReadRequestV1, ProximityEvaluationRequestV1,
};
use tracedecay_application::{
    ApplicationProblem, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, RequestContext, SafeDiagnostic, now_micros,
};
use tracedecay_domain::GitHeadStateV1;
use tracedecay_domain::feedback::{
    CiFailureParserIdentityV1, FeedbackScopeV1, FeedbackTriggerV1, GitHubPullRequestIdV1,
    GitHubReviewReadOperationV1,
};
use tracedecay_domain::{
    CommitId, HostKindV1, ManifestDigest, ProviderId, UtcMicros, canonical_sha256,
};
use tracedecay_global_db::configuration::OwnedGlobalDbConfigurationControlStore;
use tracedecay_hooks::{
    HookConfigurationFileReaderV1, HookConfigurationReadOutcomeV1, HookConfigurationSubscriberV1,
    HookFeedbackDeliveryRouteV1, HookFeedbackRollbackSwitchV1, hook_configuration_path,
};
use tracedecay_lsp::{
    DiagnosticTrigger, FeedbackCycleRequest, FeedbackCycleRuntimePort, LspRuntimeFailure,
    LspRuntimeFuture,
};
use tracedecay_usecases::advisory::github_runtime::{
    ConfiguredGitHubSourceAccessAuthorityV1, GitHubDiscoveryControlV1,
    GitHubExactCommitDiscoveryOutcomeV1, GitHubProviderLifecycleV1, GitHubSourceAccessAuthorityV1,
    ProfileGitHubReadOnlyCredentialMountOutcomeV1, RegisteredGitHubReadOnlyCredentialV1,
    discover_exact_commit_pull_request_v1, resolve_registered_github_read_only_credential_v1,
};
use tracedecay_usecases::advisory::{
    AdvisoryCycleControl, AdvisoryCycleOutcome, AdvisoryCycleRequest, AdvisoryHookDeliveryV1,
    AdvisoryHookLookupNoticeV1, AdvisoryHookNoticeQueueV1, AdvisoryHookNoticeSinkV1,
    AdvisoryProductionOpenV1, AdvisoryProductionStartupRegistrationV1, AdvisoryRuntimeOpenV1,
    CiSourceAccessAuthorityV1, GitHubCiRepositoryTargetV1, GitHubHttpReadConfigV1,
    GitHubReadOnlyCredentialV1, GitHubReadPermissionV1, GitHubRepositoryTargetV1,
    GitHubReviewProviderIdentityV1, GitHubReviewRuntimeOwnerConfigV1,
    ProductionCiFailureDiscoveryOutcomeV1, ProductionCiProviderConfigV1,
    ProjectCiCodeAnchorStoreV1, ProjectCiRetainedObservationStoreV1,
    discover_production_ci_failure_request_v1, github_anchor_authorities_arc_v1,
    register_advisory_hook_notice_queue, unregister_advisory_hook_notice_queue,
};
use tracedecay_usecases::context::MonotonicDeadline;
use tracedecay_usecases::delivery::{
    ProjectDeliveryProviderMountGateV1, ProjectDeliveryReadAuthorityOpenOutcomeV1,
    ProjectDeliveryReadOpenV1, ProjectDeliveryReviewBodySourceV1,
    gated_project_delivery_read_handle_v1, open_project_delivery_read_authority_v1,
};
use tracedecay_usecases::feedback::concrete::FeedbackRuntime;
use tracedecay_usecases::feedback::observations::{
    FeedbackDeliveryRouteV1, FeedbackObservationEmitterV1, FeedbackOperationV1, FeedbackOutcomeV1,
    FeedbackSourceEventV1,
};
use tracedecay_usecases::feedback::{
    FeedbackCycleInvocation, FeedbackCycleLspInput, FeedbackCycleRuntime,
    ProductionFeedbackCycleAuthorizationFuture, ProductionFeedbackCycleAuthorizationPort,
    ProductionFeedbackCycleOpenV1, ProductionFeedbackRuntimeStateV1,
    resolve_production_feedback_cycle_parts, resolve_project_feedback_scope_v1,
};
use tracedecay_usecases::lsp_runtime::DaemonLspSessionFactory;
use tracedecay_usecases::operation_stream::OperationKind;

use super::{
    DaemonInvocationState, POLICY_REVISION_V1, daemon_owned_project_source_access_at,
    register_semantic_activation_owner,
};
use crate::agents::context_scout_owner::ProjectContextScoutOwnerV1;
use crate::agents::context_scout_ports::{
    ContextScoutAuthorityPinV1, ContextScoutCanonicalInputAssemblerV1,
    ContextScoutConfigurationPinV1, ProjectContextScoutAddressRegistryV1,
};
use crate::agents::context_scout_v2::{
    ContextScoutDeliverySelectionInputV1, ContextScoutOutcomeV1, ContextScoutRuntimeOutcomeV1,
    ContextScoutServiceStateV1, ContextScoutTriggerV1,
};
use crate::daemon::context_scout_lifecycle::{
    AuthorityRegistrationV1, register_context_scout_lifecycle_authority,
    unregister_context_scout_lifecycle_authority,
};
use crate::daemon::service::invocation::{
    BoundedHookOrchestratorV1, DaemonAdvisoryCycleInvocationFuture,
    DaemonAdvisoryCycleInvocationOwner, DaemonAdvisoryCycleInvocationPort,
    DaemonAdvisoryCycleInvocationRequest, HookOrchestrationRequestV1, HookOrchestrationTriggerV1,
    HookOrchestrationWorkOutcomeV1, advisory_cycle_invocation_result,
    daemon_operation_event_authority, register_hook_orchestration_runtime,
    unregister_hook_orchestration_runtime,
};
use crate::daemon::service::project_runtime::RegisteredDeliveryReadAuthorityV1;
use crate::errors::{Result, TraceDecayError};
use crate::mcp::tools::handlers::hook_runtime::daemon_mint_hook_v2_file_id;

mod deferred;
mod model;
pub(crate) use model::ProjectOpenDependentOwnerState;
use model::advisory_monotonic_deadline;
#[cfg(test)]
mod current_input_tests;
#[cfg(test)]
mod scout_journey_tests;
#[cfg(test)]
mod tests;

#[derive(Clone)]
struct ProjectOpenAdvisoryFeedbackCycleV1 {
    registration: Arc<AdvisoryProductionStartupRegistrationV1>,
    producer: Arc<ProjectOpenScoutProducerV1>,
    root_uri: String,
    feedback_scope: FeedbackScopeV1,
    github_pull_request_id: Option<GitHubPullRequestIdV1>,
    ci_discovery_config: Option<ProductionCiProviderConfigV1>,
    hook_config_root: std::path::PathBuf,
}

struct ProjectOpenAdvisoryCycleExecutionV1 {
    context: RequestContext,
    outcome: AdvisoryCycleOutcome,
    observed_at: UtcMicros,
    configuration_digest: ManifestDigest,
}

struct PublishedAdvisoryRuntimeV1 {
    _registration: Arc<AdvisoryProductionStartupRegistrationV1>,
    _hook_notices: AdvisoryHookNoticeRegistrationV1,
    _scout_hooks: ScoutHookRegistrationV1,
}

struct AdvisoryHookNoticeRegistrationV1 {
    hook_project_id: [u8; 16],
    hook_worktree_id: [u8; 16],
    hook_notices: Arc<AdvisoryHookNoticeQueueV1>,
}

impl Drop for AdvisoryHookNoticeRegistrationV1 {
    fn drop(&mut self) {
        unregister_advisory_hook_notice_queue(
            self.hook_project_id,
            self.hook_worktree_id,
            &self.hook_notices,
        );
    }
}

/// Retains this runtime's hook-cycle registrations: the bounded orchestrator
/// published under the authenticated hook locators and, when this setup
/// installed it, the Scout lifecycle authority. Dropping the lease removes
/// exactly this runtime's entries; an incumbent registered by a live peer is
/// never swept.
struct ScoutHookRegistrationV1 {
    hook_project_id: [u8; 16],
    hook_worktree_id: [u8; 16],
    lifecycle_sessions: crate::global_db::RegisteredGlobalDbLeaseV1,
    lifecycle_registered_here: bool,
    orchestrator: Arc<BoundedHookOrchestratorV1>,
}

impl Drop for ScoutHookRegistrationV1 {
    fn drop(&mut self) {
        unregister_hook_orchestration_runtime(
            self.hook_project_id,
            self.hook_worktree_id,
            &self.orchestrator,
        );
        if self.lifecycle_registered_here {
            unregister_context_scout_lifecycle_authority(
                self.hook_project_id,
                self.hook_worktree_id,
                &self.lifecycle_sessions,
            );
        }
    }
}

impl ProjectOpenAdvisoryFeedbackCycleV1 {
    /// Resolves the cycle input from the current configuration revision and
    /// the current sealed code-index generation on every invocation. A Plan 20
    /// settings PATCH landing after project open (for example enabling the
    /// Context Scout checkbox) therefore remounts the producer path on the
    /// next cycle instead of rejecting every cycle as
    /// `feedback-cycle-configuration-drift` until the project is reopened, and
    /// files sealed by later generations stay eligible without a reopen.
    async fn run_cycle(
        &self,
        request: FeedbackCycleRequest,
        deadline: MonotonicDeadline,
        agent_stop_gate: bool,
    ) -> std::result::Result<ProjectOpenAdvisoryCycleExecutionV1, LspRuntimeFailure> {
        let inputs = &self.producer.cycle_inputs;
        let indexed_files = current_indexed_files(
            &inputs.code_index_schedulers,
            &inputs.project_root,
            &inputs.scope,
        )
        .await
        .ok_or_else(|| LspRuntimeFailure::new("feedback-cycle-current-census"))?;
        let lsp_input = current_feedback_lsp_input(inputs, &indexed_files).await?;
        self.run_cycle_with_lsp_input(lsp_input, request, deadline, agent_stop_gate)
            .await
    }

    async fn run_cycle_with_lsp_input(
        &self,
        lsp_input: FeedbackCycleLspInput,
        request: FeedbackCycleRequest,
        deadline: MonotonicDeadline,
        agent_stop_gate: bool,
    ) -> std::result::Result<ProjectOpenAdvisoryCycleExecutionV1, LspRuntimeFailure> {
        let mut invocation = lsp_input(request).await?;
        if agent_stop_gate {
            invocation.request.input.request.trigger = FeedbackTriggerV1::AgentStopGate;
            invocation = FeedbackCycleInvocation::new(invocation.context, invocation.request)
                .map_err(|_| LspRuntimeFailure::new("feedback-cycle-advisory-stop-gate"))?;
        }
        let observed_at = invocation.request.input.observed_at;
        let configuration_digest = invocation
            .request
            .input
            .request
            .configuration_digest
            .clone();
        let ci = match self.ci_discovery_config.as_ref() {
            Some(config) => {
                discover_production_ci_failure_request_v1(
                    &invocation.context,
                    config,
                    &self.feedback_scope,
                )
                .await
            }
            None => ProductionCiFailureDiscoveryOutcomeV1::NotConfigured,
        };
        let expires_at = UtcMicros(observed_at.0.saturating_add(5 * 60 * 1_000_000));
        let operation = daemon_operation_event_authority()
            .begin(
                &invocation.context,
                OperationKind::FeedbackDiagnostics,
                observed_at,
            )
            .await
            .map_err(|error| {
                tracing::warn!(
                    target: "tracedecay::feedback_advisory_cycle",
                    project_id = self.feedback_scope.project_id.as_str(),
                    worktree_id = self.feedback_scope.worktree_id.as_str(),
                    %error,
                    "advisory feedback cycle could not begin its operation event"
                );
                LspRuntimeFailure::new("feedback-cycle-advisory-operation")
            })?;
        let outcome = self
            .registration
            .runtime()
            .run_once(
                &invocation.context,
                AdvisoryCycleControl {
                    operation,
                    deadline,
                },
                AdvisoryCycleRequest {
                    feedback: invocation.request,
                    github: self.github_pull_request_id.clone().map(|pull_request_id| {
                        GitHubReviewReadRequestV1 {
                            operation:
                                GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
                            scope: self.feedback_scope.clone(),
                            pull_request_id,
                        }
                    }),
                    ci,
                    proximity: Some(ProximityEvaluationRequestV1 {
                        scope: self.feedback_scope.clone(),
                        observed_at,
                    }),
                    validity: tracedecay_application::AdvisoryFindingValidityWindowV1 {
                        valid_at: observed_at,
                        expires_at,
                    },
                },
            )
            .await
            .map_err(|error| {
                tracing::warn!(
                    target: "tracedecay::feedback_advisory_cycle",
                    project_id = self.feedback_scope.project_id.as_str(),
                    worktree_id = self.feedback_scope.worktree_id.as_str(),
                    %error,
                    "advisory feedback cycle execution failed"
                );
                LspRuntimeFailure::new("feedback-cycle-advisory-execution")
            })?;
        if outcome.publication().is_some() {
            self.deliver_completed_publication(&outcome);
        }
        Ok(ProjectOpenAdvisoryCycleExecutionV1 {
            context: invocation.context,
            outcome,
            observed_at,
            configuration_digest,
        })
    }

    /// Drives the mounted host-delivery half for one atomically recorded
    /// publication: the content-free Hook V2 lookup notice is enqueued for the
    /// bound hosts' next admission, while MCP/CLI/LSP callers keep reading the
    /// same publication store. Every non-delivered state stays typed and
    /// reported; none of them fails the already-completed cycle.
    fn deliver_completed_publication(&self, outcome: &AdvisoryCycleOutcome) {
        let project_id = self.feedback_scope.project_id.as_str();
        let worktree_id = self.feedback_scope.worktree_id.as_str();
        let Some((host, rollback)) =
            advisory_hook_notice_dispatch(&self.hook_config_root, now_micros())
        else {
            tracing::warn!(
                target: "tracedecay::feedback_advisory_cycle",
                project_id,
                worktree_id,
                "advisory hook notice delivery is unavailable: no live daemon hook binding"
            );
            return;
        };
        match self
            .registration
            .consume_completed_publication(host, outcome, rollback)
        {
            // The daemon retains the LSP session factory itself, so the
            // returned provider-bundle mount is already owned by the live LSP
            // sessions; only the hook delivery outcome needs reporting here.
            Ok(delivery) => match delivery.hook {
                AdvisoryHookDeliveryV1::Delivered { outcome, .. } => {
                    tracing::debug!(
                        target: "tracedecay::feedback_advisory_cycle",
                        project_id,
                        worktree_id,
                        ?outcome,
                        "advisory hook lookup notice delivered for a completed publication"
                    );
                }
                AdvisoryHookDeliveryV1::SinkUnavailable => {
                    tracing::warn!(
                        target: "tracedecay::feedback_advisory_cycle",
                        project_id,
                        worktree_id,
                        "advisory hook notice sink is unavailable for a completed publication"
                    );
                }
                AdvisoryHookDeliveryV1::Unavailable(reason) => {
                    tracing::warn!(
                        target: "tracedecay::feedback_advisory_cycle",
                        project_id,
                        worktree_id,
                        ?reason,
                        "advisory hook route is unavailable for a completed publication"
                    );
                }
            },
            Err(error) => {
                tracing::warn!(
                    target: "tracedecay::feedback_advisory_cycle",
                    project_id,
                    worktree_id,
                    %error,
                    "advisory host delivery failed for a completed publication"
                );
            }
        }
    }
}

/// Selects the daemon-published hook binding that authorizes one Hook V2
/// lookup-notice delivery. Project admission publishes bindings for every
/// native hook host together, so the first live binding carries the daemon's
/// current hook configuration revision. `None` is the typed unbound state
/// (expired or never-published bindings) under which no host could
/// acknowledge a notice.
fn advisory_hook_notice_dispatch(
    hook_config_root: &Path,
    now: UtcMicros,
) -> Option<(HostKindV1, HookFeedbackRollbackSwitchV1)> {
    crate::hooks::NATIVE_HOOK_HOSTS.iter().find_map(|host| {
        let subscriber = HookConfigurationSubscriberV1::new(HookConfigurationFileReaderV1::new(
            hook_configuration_path(hook_config_root, *host),
        ));
        match subscriber.load_current(*host, now) {
            HookConfigurationReadOutcomeV1::Bound(snapshot) => Some((
                host.host_kind(),
                HookFeedbackRollbackSwitchV1 {
                    configuration_revision: snapshot.revision,
                    route: HookFeedbackDeliveryRouteV1::HookV2,
                },
            )),
            _ => None,
        }
    })
}

impl FeedbackCycleRuntimePort for ProjectOpenAdvisoryFeedbackCycleV1 {
    fn execute(
        &self,
        request: FeedbackCycleRequest,
    ) -> LspRuntimeFuture<std::result::Result<(), LspRuntimeFailure>> {
        let owner = self.clone();
        Box::pin(async move {
            owner
                .run_cycle(
                    request,
                    MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
                    false,
                )
                .await?;
            Ok(())
        })
    }
}

impl DaemonAdvisoryCycleInvocationPort for ProjectOpenAdvisoryFeedbackCycleV1 {
    fn invoke(
        &self,
        request: DaemonAdvisoryCycleInvocationRequest,
    ) -> DaemonAdvisoryCycleInvocationFuture<'_> {
        let owner = self.clone();
        Box::pin(async move {
            if request.cancellation.is_cancelled() {
                return Err(ApplicationProblem::cancelled_before_admission());
            }
            let monotonic_deadline = advisory_monotonic_deadline(&request.deadline, now_micros())?;
            let execution = owner
                .run_cycle(
                    FeedbackCycleRequest {
                        root_uri: owner.root_uri.clone(),
                        document_uri: request.document_uri,
                        trigger: DiagnosticTrigger::ExplicitDocumentDiagnostics,
                    },
                    monotonic_deadline,
                    false,
                )
                .await
                .map_err(|failure| {
                    ApplicationProblem::unavailable(SafeDiagnostic {
                        code: "feedback.advisory-cycle.execution".to_owned(),
                        message: format!(
                            "The advisory feedback cycle could not execute ({})",
                            failure.class()
                        ),
                    })
                })?;
            advisory_cycle_invocation_result(
                &execution.context,
                request.observed_at,
                request.deadline,
                request.cancellation,
                execution.outcome,
            )
        })
    }
}

struct ProjectOpenFeedbackCycleAuthorizationV1 {
    project_root: std::path::PathBuf,
    scope: tracedecay_application::ResolvedScope,
    configuration: Arc<tracedecay_usecases::configuration::ProjectConfigurationRuntime>,
}

impl ProductionFeedbackCycleAuthorizationPort for ProjectOpenFeedbackCycleAuthorizationV1 {
    fn authorize(&self, observed_at: UtcMicros) -> ProductionFeedbackCycleAuthorizationFuture<'_> {
        Box::pin(async move {
            let current = self
                .configuration
                .client()
                .current()
                .await
                .map_err(|_| LspRuntimeFailure::new("feedback-cycle-authorization"))?;
            daemon_owned_project_source_access_at(
                &self.scope,
                &self.project_root,
                &current,
                observed_at,
            )
            .map_err(|_| LspRuntimeFailure::new("feedback-cycle-authorization"))
        })
    }
}

fn unavailable_advisory_hook_notice(
    _notice: &AdvisoryHookLookupNoticeV1,
) -> tracedecay_hooks::HookFeedbackDeliveryOutcomeV1 {
    tracedecay_hooks::HookFeedbackDeliveryOutcomeV1::Unavailable
}

fn unavailable_advisory_hook_sink() -> Arc<AdvisoryHookNoticeSinkV1> {
    Arc::new(unavailable_advisory_hook_notice)
}

async fn install_project_open_context_scout_configuration(
    owner: &ProjectContextScoutOwnerV1,
    pin: ContextScoutConfigurationPinV1,
    model_config: &tracedecay_agent_hosts::automation::config::AutomationConfig,
) -> Result<()> {
    let admitted_model_config = pin.control().model_path.and_then(|expected| {
        (crate::agents::context_scout_model::context_scout_backend_from_automation_config(
            model_config,
        ) == expected)
            .then_some(model_config)
    });
    owner
        .install_configuration(pin, admitted_model_config)
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open Context Scout configuration failed: {error}"),
        })
}

/// Cycle inputs resolved fresh for every producer-path cycle: the current
/// configuration revision through the graph's configuration runtime and the
/// current sealed code-index generation through the scheduler registry.
/// Nothing here pins project-open state, so a settings PATCH or a later
/// sealed generation is picked up by the next cycle without a reopen.
struct ProjectOpenCycleInputsV1 {
    graph: Arc<crate::tracedecay::TraceDecay>,
    project_root: std::path::PathBuf,
    scope: tracedecay_application::ResolvedScope,
    code_index_schedulers: crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    session_db: crate::global_db::RegisteredGlobalDbLeaseV1,
    code_graph: Arc<dyn tracedecay_usecases::graph::CodeGraphProjectionReadPort>,
    requester: tracedecay_domain::ActorId,
    diagnostic_broker: Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>,
}

/// Producer inputs retained for the bounded hook cycle: the fresh cycle
/// inputs plus the durable Scout owner, address registry, and the
/// committed-publication read port for the Scout tail.
struct ProjectOpenScoutProducerV1 {
    cycle_inputs: Arc<ProjectOpenCycleInputsV1>,
    scout_owner: Arc<ProjectContextScoutOwnerV1>,
    scout_registry: Arc<ProjectContextScoutAddressRegistryV1>,
    feedback_runtime: Arc<FeedbackRuntime>,
}

/// Sorted logical paths from the current sealed code-index generation,
/// resolved per cycle. The one-time project-open census is never retained, so
/// files sealed by later generations map saved-edit hooks and mount providers
/// without a project reopen. `None` is the typed no-sealed-generation state.
async fn current_indexed_files(
    code_index_schedulers: &crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    scope: &tracedecay_application::ResolvedScope,
) -> Option<Vec<String>> {
    let generation = code_index_schedulers
        .latest_complete_ready_decoded_for_root_scope(project_root, scope)
        .await?;
    let mut indexed_files = generation
        .generation()
        .snapshot()
        .files
        .iter()
        .map(|file| file.logical_path.clone())
        .collect::<Vec<_>>();
    indexed_files.sort();
    Some(indexed_files)
}

async fn current_feedback_lsp_input(
    inputs: &ProjectOpenCycleInputsV1,
    indexed_files: &[String],
) -> std::result::Result<FeedbackCycleLspInput, LspRuntimeFailure> {
    let pinned_configuration = inputs
        .graph
        .configuration_runtime()
        .client()
        .current()
        .await
        .map_err(|_| LspRuntimeFailure::new("feedback-cycle-current-configuration"))?;
    let current_configuration = tracedecay_usecases::configuration::ConfigurationCurrentStateV1 {
        revision_id: pinned_configuration.revision_id,
        snapshot: pinned_configuration.snapshot,
    };
    let configuration_digest = current_configuration
        .snapshot
        .effective_behavior_digest
        .clone();
    let policy_digest = canonical_sha256(&(
        "tracedecay.project-open.policy.v1",
        &configuration_digest,
        POLICY_REVISION_V1,
    ))
    .map_err(|_| LspRuntimeFailure::new("feedback-cycle-current-policy"))?;
    let runtime_state: Arc<dyn FeedbackRuntimeStatePort + Send + Sync> =
        Arc::new(ProductionFeedbackRuntimeStateV1::new(
            Arc::clone(&inputs.code_graph),
            configuration_digest,
            policy_digest,
        ));
    let authorization: Arc<dyn ProductionFeedbackCycleAuthorizationPort> =
        Arc::new(ProjectOpenFeedbackCycleAuthorizationV1 {
            project_root: inputs.project_root.clone(),
            scope: inputs.scope.clone(),
            configuration: Arc::clone(inputs.graph.configuration_runtime()),
        });
    let mounted_providers = inputs
        .diagnostic_broker
        .lock()
        .await
        .mounted_providers_for_files(indexed_files);
    resolve_production_feedback_cycle_parts(ProductionFeedbackCycleOpenV1 {
        project_root: inputs.project_root.clone(),
        scope: inputs.scope.clone(),
        access_configuration: current_configuration,
        requester: inputs.requester.clone(),
        authorization,
        code_graph: Arc::clone(&inputs.code_graph),
        project_runtime_db: inputs.session_db.clone(),
        runtime_state,
        document_identity: Arc::new(inputs.code_index_schedulers.clone()),
        code_index_identity: Arc::new(inputs.code_index_schedulers.clone()),
        test_attribution: Arc::new(inputs.code_index_schedulers.clone()),
        mounted_providers,
    })
    .await
    .map(|parts| parts.lsp_input)
    .map_err(|_| LspRuntimeFailure::new("feedback-cycle-current-input"))
}

/// One admitted hook boundary's advisory-and-Scout cycle: the Plan 09
/// one-shot advisory/hook-notice run, then the Scout producer tail —
/// canonical input assembly from the latest committed publication, daemon-side
/// delivery selection, `prepare_configured`, and a claim-authority mount for
/// the enqueued generation. Every early return is a typed fail-closed state;
/// none of them invents guidance.
async fn run_production_hook_cycle(
    cycle: Arc<ProjectOpenAdvisoryFeedbackCycleV1>,
    request: HookOrchestrationRequestV1,
    work_cancellation: tracedecay_runtime_core::cancellation::CancellationToken,
) -> HookOrchestrationWorkOutcomeV1 {
    if work_cancellation.is_cancelled() {
        return HookOrchestrationWorkOutcomeV1::RetryableFailure;
    }
    let producer = Arc::clone(&cycle.producer);
    let inputs = Arc::clone(&producer.cycle_inputs);
    let Some(indexed_files) = current_indexed_files(
        &inputs.code_index_schedulers,
        &inputs.project_root,
        &inputs.scope,
    )
    .await
    else {
        observe_hook_feedback_cycle_terminal(
            &cycle.registration.host_delivery.source_observations,
            &request,
            FeedbackOutcomeV1::Unavailable,
        );
        return HookOrchestrationWorkOutcomeV1::RetryableFailure;
    };
    let Some(document_uri) = hook_feedback_document_uri_or_observe(
        &inputs.project_root,
        &indexed_files,
        &request,
        &cycle.registration.host_delivery.source_observations,
    ) else {
        return HookOrchestrationWorkOutcomeV1::RetryableFailure;
    };
    let Ok(lsp_input) = current_feedback_lsp_input(&inputs, &indexed_files).await else {
        observe_hook_feedback_cycle_terminal(
            &cycle.registration.host_delivery.source_observations,
            &request,
            FeedbackOutcomeV1::Unavailable,
        );
        return HookOrchestrationWorkOutcomeV1::RetryableFailure;
    };
    let diagnostic_trigger = match request.trigger {
        HookOrchestrationTriggerV1::SavedEdit => DiagnosticTrigger::DocumentSave,
        HookOrchestrationTriggerV1::Stop | HookOrchestrationTriggerV1::Explicit => {
            DiagnosticTrigger::ExplicitDocumentDiagnostics
        }
    };
    let execution = match cycle
        .run_cycle_with_lsp_input(
            lsp_input,
            FeedbackCycleRequest {
                root_uri: cycle.root_uri.clone(),
                document_uri,
                trigger: diagnostic_trigger,
            },
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            request.trigger == HookOrchestrationTriggerV1::Stop,
        )
        .await
    {
        Ok(execution) => execution,
        Err(_) => {
            observe_hook_feedback_cycle_terminal(
                &cycle.registration.host_delivery.source_observations,
                &request,
                FeedbackOutcomeV1::Unavailable,
            );
            return HookOrchestrationWorkOutcomeV1::RetryableFailure;
        }
    };
    if work_cancellation.is_cancelled() {
        return HookOrchestrationWorkOutcomeV1::RetryableFailure;
    }
    let observed_at = execution.observed_at;
    // The Scout tail re-pins the current Plan 20 configuration: a revision
    // that landed while the advisory half ran must not produce guidance
    // under the superseded control state.
    let Ok(pinned_configuration) = inputs
        .graph
        .configuration_runtime()
        .client()
        .current()
        .await
    else {
        return HookOrchestrationWorkOutcomeV1::RetryableFailure;
    };
    let current_configuration = tracedecay_usecases::configuration::ConfigurationCurrentStateV1 {
        revision_id: pinned_configuration.revision_id.clone(),
        snapshot: pinned_configuration.snapshot.clone(),
    };
    let Some(scout_configuration) =
        ContextScoutConfigurationPinV1::from_current(&current_configuration)
    else {
        return HookOrchestrationWorkOutcomeV1::RetryableFailure;
    };
    if scout_configuration.configuration_digest() != &execution.configuration_digest {
        return HookOrchestrationWorkOutcomeV1::RetryableFailure;
    }
    let Ok(model_config) = tracedecay_agent_hosts::automation::config::from_configuration_snapshot(
        &pinned_configuration.snapshot,
    ) else {
        return HookOrchestrationWorkOutcomeV1::RetryableFailure;
    };
    if install_project_open_context_scout_configuration(
        producer.scout_owner.as_ref(),
        scout_configuration.clone(),
        &model_config,
    )
    .await
    .is_err()
    {
        return HookOrchestrationWorkOutcomeV1::RetryableFailure;
    }
    let Some(lifecycle) = request.lifecycle else {
        return HookOrchestrationWorkOutcomeV1::RetryableFailure;
    };
    let Some(pin) = ContextScoutAuthorityPinV1::new(
        &execution.context,
        cycle.feedback_scope.clone(),
        scout_configuration,
        observed_at,
    ) else {
        return HookOrchestrationWorkOutcomeV1::RetryableFailure;
    };
    let assembler = ContextScoutCanonicalInputAssemblerV1::new(
        producer.scout_registry.as_ref(),
        producer.feedback_runtime.as_ref(),
    );
    let Some(canonical) = assembler
        .bind_and_assemble(
            &request.hook,
            &pin,
            lifecycle.clone(),
            &execution.context,
            observed_at,
        )
        .await
    else {
        return HookOrchestrationWorkOutcomeV1::RetryableFailure;
    };
    let trigger = match request.trigger {
        HookOrchestrationTriggerV1::SavedEdit => ContextScoutTriggerV1::SavedEdit,
        HookOrchestrationTriggerV1::Stop => ContextScoutTriggerV1::StopBoundary,
        HookOrchestrationTriggerV1::Explicit => ContextScoutTriggerV1::ExplicitRequest,
    };
    let recent = producer
        .scout_owner
        .recent_exact(canonical.address, 32)
        .await
        .ok();
    let has_recent_delivery = recent
        .as_ref()
        .is_some_and(|recent| !recent.deliveries.is_empty());
    let has_unresolved_interaction = recent.as_ref().is_some_and(|recent| {
        !recent.pending.is_empty()
            || recent.deliveries.iter().any(|delivery| {
                delivery.feedback.is_none()
                    && matches!(
                        delivery.receipt.outcome,
                        ContextScoutOutcomeV1::Attempted
                            | ContextScoutOutcomeV1::Delayed
                            | ContextScoutOutcomeV1::Displayed
                            | ContextScoutOutcomeV1::Expanded
                            | ContextScoutOutcomeV1::Unknown
                    )
            })
    });
    let Some(selection) = canonical.selection_input(
        &request.hook,
        observed_at,
        ContextScoutDeliverySelectionInputV1 {
            trigger,
            quiet_mode: canonical.control.state != ContextScoutServiceStateV1::Active,
            has_recent_delivery,
            has_unresolved_interaction,
            critical_safety_evidence: false,
            delivered_dedupe_keys: recent
                .as_ref()
                .into_iter()
                .flat_map(|recent| recent.deliveries.iter())
                .map(|delivery| delivery.entry.envelope.candidate.dedupe_key)
                .collect(),
        },
    ) else {
        return HookOrchestrationWorkOutcomeV1::Completed;
    };
    let outcome = producer
        .scout_owner
        .prepare_configured(
            &selection,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            work_cancellation.clone(),
        )
        .await;
    match outcome {
        Ok(ContextScoutRuntimeOutcomeV1::Enqueued { .. }) => inputs
            .graph
            .mount_current_context_scout_claim_authority(
                Arc::clone(&producer.scout_registry),
                &request.hook,
                pin,
                execution.context,
                lifecycle,
                canonical.address,
                selection.input_watermark,
                observed_at,
            )
            .await
            .then_some(HookOrchestrationWorkOutcomeV1::Completed)
            .unwrap_or(HookOrchestrationWorkOutcomeV1::RetryableFailure),
        Ok(ContextScoutRuntimeOutcomeV1::Suppressed { .. }) => {
            HookOrchestrationWorkOutcomeV1::Completed
        }
        Ok(ContextScoutRuntimeOutcomeV1::Unavailable) | Err(_) => {
            HookOrchestrationWorkOutcomeV1::RetryableFailure
        }
    }
}

fn observe_hook_feedback_cycle_terminal(
    observations: &Arc<dyn FeedbackObservationEmitterV1 + Send + Sync>,
    request: &HookOrchestrationRequestV1,
    outcome: FeedbackOutcomeV1,
) {
    let envelope = request.hook.envelope();
    let trigger = match request.trigger {
        HookOrchestrationTriggerV1::SavedEdit => "saved_edit",
        HookOrchestrationTriggerV1::Stop => "stop",
        HookOrchestrationTriggerV1::Explicit => "explicit",
    };
    let Ok(subject) = canonical_sha256(&(
        "tracedecay.feedback.accepted-hook-cycle.v1",
        envelope.event_id,
        envelope.project_id,
        envelope.repository_id,
        envelope.worktree_id,
        &request.hook_configuration_revision,
        trigger,
    )) else {
        return;
    };
    observations.observe_source_event_for_subject(
        subject,
        envelope.observed_at,
        FeedbackSourceEventV1::Delivery {
            operation: FeedbackOperationV1::FeedbackCycle,
            route: FeedbackDeliveryRouteV1::HookV2,
            outcome,
            item_count: 0,
            duration_micros: None,
        },
    );
}

fn hook_feedback_document_uri_or_observe(
    project_root: &Path,
    indexed_files: &[String],
    request: &HookOrchestrationRequestV1,
    observations: &Arc<dyn FeedbackObservationEmitterV1 + Send + Sync>,
) -> Option<String> {
    let document_uri = hook_feedback_document_uri(project_root, indexed_files, request);
    if document_uri.is_none() {
        observe_hook_feedback_cycle_terminal(
            observations,
            request,
            if indexed_files.is_empty() {
                FeedbackOutcomeV1::Unavailable
            } else {
                FeedbackOutcomeV1::Partial
            },
        );
    }
    document_uri
}

fn hook_feedback_document_uri(
    project_root: &Path,
    indexed_files: &[String],
    request: &HookOrchestrationRequestV1,
) -> Option<String> {
    let logical_path = match &request.hook.envelope().event {
        tracedecay_hooks::HookEventV2::SavedEdit { file_id, .. } => {
            indexed_files.iter().find(|logical_path| {
                let logical_file_id = daemon_mint_hook_v2_file_id(
                    request.hook.envelope(),
                    hash16(logical_path.as_bytes()),
                );
                let absolute_file_id = daemon_mint_hook_v2_file_id(
                    request.hook.envelope(),
                    hash16(project_root.join(logical_path).to_string_lossy().as_bytes()),
                );
                logical_file_id == *file_id || absolute_file_id == *file_id
            })?
        }
        _ => indexed_files.first()?,
    };
    url::Url::from_file_path(project_root.join(logical_path))
        .ok()
        .map(Into::into)
}

fn hash16(value: &[u8]) -> [u8; 16] {
    let digest = Sha256::digest(value);
    let mut value = [0_u8; 16];
    value.copy_from_slice(&digest[..16]);
    value
}

pub(super) async fn register_production_feedback_and_advisory(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &ProjectOpenDependentOwnerState,
    lsp_session_factory: Arc<DaemonLspSessionFactory>,
) -> Result<()> {
    let (feedback_cycle, feedback_scope) =
        register_production_feedback_cycle(invocation, project_root, state).await?;
    register_production_advisory_owner(
        invocation,
        project_root,
        state,
        feedback_cycle,
        feedback_scope,
        lsp_session_factory,
    )
    .await
}

/// Registers owners whose exact authority depends on a mounted code index.
pub(in crate::daemon) async fn register_project_open_dependent_owners(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: ProjectOpenDependentOwnerState,
) -> Result<()> {
    let state = state;
    if !matches!(
        tracedecay_usecases::git_intelligence::NativeGitIntelligence::new(
            project_root,
            state.scope.repository_id.clone(),
            state.scope.worktree_id.clone(),
        )
        .head(),
        Ok(GitHeadStateV1::Attached { .. })
    ) {
        register_semantic_activation_owner(
            invocation,
            project_root,
            &state.graph,
            state.session_db.clone(),
            state.scope,
            &state.scout_configuration,
        )
        .await?;
        tracing::info!(
            event = "project_open_owner_phase",
            project = %project_root.display(),
            phase = "feedback_advisory_unavailable",
            reason = "the admitted checkout has no attached branch",
        );
        return Ok(());
    }
    register_project_delivery_read_authority(invocation, project_root, &state).await?;
    if let Some(lsp_session_factory) = state.lsp_session_factory.as_ref() {
        if let Err(error) = register_production_feedback_and_advisory(
            invocation,
            project_root,
            &state,
            Arc::clone(lsp_session_factory),
        )
        .await
        {
            tracing::warn!(
                event = "feedback_advisory_mount",
                outcome = "deferred",
                project = %project_root.display(),
                reason = %error,
                "initial advisory mount raced its generation authority"
            );
            register_semantic_activation_owner(
                invocation,
                project_root,
                &state.graph,
                state.session_db.clone(),
                state.scope.clone(),
                &state.scout_configuration,
            )
            .await?;
            deferred::spawn(invocation.clone(), project_root.to_path_buf(), state);
            return Ok(());
        }
        tracing::info!(
            event = "project_open_owner_phase",
            project = %project_root.display(),
            phase = "feedback_advisory_registered",
        );
        let semantic_activation_started = Instant::now();
        register_semantic_activation_owner(
            invocation,
            project_root,
            &state.graph,
            state.session_db.clone(),
            state.scope.clone(),
            &state.scout_configuration,
        )
        .await?;
        tracing::info!(
            event = "project_open_owner_phase",
            project = %project_root.display(),
            phase = "semantic_activation_resolved",
            elapsed_ms = semantic_activation_started.elapsed().as_millis(),
        );
        return Ok(());
    }

    let semantic_activation_started = Instant::now();
    register_semantic_activation_owner(
        invocation,
        project_root,
        &state.graph,
        state.session_db.clone(),
        state.scope.clone(),
        &state.scout_configuration,
    )
    .await?;
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "semantic_activation_resolved",
        elapsed_ms = semantic_activation_started.elapsed().as_millis(),
    );
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "feedback_advisory_deferred",
        reason = "current sealed code-index generation is unavailable",
    );
    deferred::spawn(invocation.clone(), project_root.to_path_buf(), state);
    Ok(())
}

async fn register_production_feedback_cycle(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &ProjectOpenDependentOwnerState,
) -> Result<(Arc<FeedbackCycleRuntime>, FeedbackScopeV1)> {
    let configuration_digest = &state.scout_configuration.snapshot.effective_behavior_digest;
    let policy_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.project-open.policy.v1",
        configuration_digest,
        POLICY_REVISION_V1,
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("project-open feedback policy digest failed: {error}"),
    })?;
    let runtime_state: Arc<dyn FeedbackRuntimeStatePort + Send + Sync> =
        Arc::new(ProductionFeedbackRuntimeStateV1::new(
            Arc::clone(&state.code_graph),
            configuration_digest.clone(),
            policy_digest,
        ));
    let authorization: Arc<dyn ProductionFeedbackCycleAuthorizationPort> =
        Arc::new(ProjectOpenFeedbackCycleAuthorizationV1 {
            project_root: project_root.to_path_buf(),
            scope: state.scope.clone(),
            configuration: Arc::clone(state.graph.configuration_runtime()),
        });
    let parts = resolve_production_feedback_cycle_parts(ProductionFeedbackCycleOpenV1 {
        project_root: project_root.to_path_buf(),
        scope: state.scope.clone(),
        access_configuration: state.scout_configuration.clone(),
        requester: state.requester.clone(),
        authorization,
        code_graph: Arc::clone(&state.code_graph),
        project_runtime_db: state.session_db.clone(),
        runtime_state,
        document_identity: Arc::new(invocation.code_index_schedulers.clone()),
        code_index_identity: Arc::new(invocation.code_index_schedulers.clone()),
        test_attribution: Arc::new(invocation.code_index_schedulers.clone()),
        mounted_providers: state.mounted_providers.clone(),
    })
    .await
    .map_err(|error| TraceDecayError::Config {
        message: format!("project-open feedback cycle parts failed: {error}"),
    })?;
    let feedback_scope = parts.feedback_scope.clone();
    if let Some(runtime) = invocation.service.feedback_cycle(Some(project_root)).await {
        return Ok((runtime, feedback_scope));
    }
    let runtime = invocation
        .feedback_runtime_registrar()
        .open_cycle_and_register(
            project_root.to_path_buf(),
            state.database.clone(),
            parts.runtime_state,
            parts.policy_context,
            parts.evidence_horizon,
            parts.evaluated_at,
            parts.provider_candidates,
            Arc::clone(&state.code_graph),
            parts.affected_tests,
            parts.operation,
            parts.graph_operation,
            parts.tests_operation,
            parts.lsp_input,
            parts.proximity,
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open feedback cycle registration failed: {error}"),
        })?;
    Ok((runtime, feedback_scope))
}

async fn register_production_advisory_owner(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &ProjectOpenDependentOwnerState,
    feedback_cycle: Arc<FeedbackCycleRuntime>,
    feedback_scope: FeedbackScopeV1,
    lsp_session_factory: Arc<DaemonLspSessionFactory>,
) -> Result<()> {
    let scout_owner =
        state
            .graph
            .context_scout_owner()
            .cloned()
            .ok_or_else(|| TraceDecayError::Config {
                message: "project-open Context Scout owner is unavailable".to_owned(),
            })?;
    let configuration = state
        .graph
        .configuration_runtime()
        .client()
        .current()
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open automation configuration is unavailable: {error}"),
        })?;
    let current_configuration = tracedecay_usecases::configuration::ConfigurationCurrentStateV1 {
        revision_id: configuration.revision_id.clone(),
        snapshot: configuration.snapshot.clone(),
    };
    // The Plan 20 control pin and the model configuration are read from the
    // same current snapshot: a settings PATCH that landed after project open
    // (a deferred mount, or the user enabling the Context Scout checkbox)
    // mounts the updated state here instead of failing until reopen.
    let scout_configuration = ContextScoutConfigurationPinV1::from_current(&current_configuration)
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open Context Scout configuration is unavailable".to_owned(),
        })?;
    let model_config = tracedecay_agent_hosts::automation::config::from_configuration_snapshot(
        &configuration.snapshot,
    )?;
    install_project_open_context_scout_configuration(
        scout_owner.as_ref(),
        scout_configuration,
        &model_config,
    )
    .await?;
    let scout_registry = invocation
        .context_scout_runtime_registrar()
        .get(
            &state.session_db.binding().shard_id.profile_id,
            &state.scope.project_id,
            project_root,
        )
        .await
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open Context Scout address registry is unavailable".to_owned(),
        })?;
    let remote =
        resolve_production_github_provider_config(invocation, project_root, state, &feedback_scope)
            .await;
    let (github, github_source_access, ci_config) = remote.map_or((None, None, None), |remote| {
        (remote.github, Some(remote.github_source_access), remote.ci)
    });
    let github_pull_request_id = github
        .as_ref()
        .map(|github| github.target.pull_request_id.clone());
    let ci_discovery_config = ci_config.clone();
    let ci_retained = Arc::new(
        ProjectCiRetainedObservationStoreV1::new(state.database.clone(), feedback_scope.clone())
            .ok_or_else(|| TraceDecayError::Config {
                message: "project-open CI retained store rejected the feedback scope".to_owned(),
            })?,
    ) as _;
    let ci_code_anchors = Arc::new(
        ProjectCiCodeAnchorStoreV1::new_with_code_index_identity(
            project_root.to_path_buf(),
            feedback_scope.clone(),
            Arc::clone(&state.code_graph),
            Arc::new(invocation.code_index_schedulers.clone()),
        )
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open CI anchor store rejected the feedback scope".to_owned(),
        })?,
    ) as _;
    let hook_notices = AdvisoryHookNoticeQueueV1::new(feedback_scope.clone());
    let (hook_project_id, hook_worktree_id) = crate::hooks::hook_scope_locators(&state.scope);
    if !register_advisory_hook_notice_queue(hook_project_id, hook_worktree_id, &hook_notices) {
        return Err(TraceDecayError::Config {
            message: "project-open advisory hook notice authority is unavailable".to_owned(),
        });
    }
    let hook_notice_registration = AdvisoryHookNoticeRegistrationV1 {
        hook_project_id,
        hook_worktree_id,
        hook_notices: Arc::clone(&hook_notices),
    };
    let input = AdvisoryRuntimeOpenV1 {
        database: state.database.clone(),
        project_root: project_root.to_path_buf(),
        resolved_scope: state.scope.clone(),
        feedback_scope: feedback_scope.clone(),
        github,
        feedback_cycle: Arc::clone(&feedback_cycle),
    };
    let production = AdvisoryProductionOpenV1 {
        project_runtime_db: state.session_db.clone(),
        database: state.database.clone(),
        code_graph: Arc::clone(&state.code_graph),
        code_index_identity: Arc::new(invocation.code_index_schedulers.clone()),
        project_root: project_root.to_path_buf(),
        feedback_scope: feedback_scope.clone(),
        ci_config,
        github_source_access,
        ci_retained,
        ci_code_anchors,
        hook_v2: hook_notices.sink(),
        legacy_hook: unavailable_advisory_hook_sink(),
    };
    let registration = invocation
        .advisory_runtime_registrar()
        .build_production(project_root, input, production, lsp_session_factory)
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open advisory runtime construction failed: {error}"),
        })?;
    let producer = Arc::new(ProjectOpenScoutProducerV1 {
        cycle_inputs: Arc::new(ProjectOpenCycleInputsV1 {
            graph: Arc::clone(&state.graph),
            project_root: project_root.to_path_buf(),
            scope: state.scope.clone(),
            code_index_schedulers: invocation.code_index_schedulers.clone(),
            session_db: state.session_db.clone(),
            code_graph: Arc::clone(&state.code_graph),
            requester: state.requester.clone(),
            diagnostic_broker: Arc::clone(&state.diagnostic_broker),
        }),
        scout_owner,
        scout_registry,
        feedback_runtime: feedback_cycle.feedback_runtime(),
    });
    let advisory_cycle = Arc::new(ProjectOpenAdvisoryFeedbackCycleV1 {
        registration: Arc::clone(&registration),
        producer,
        root_uri: state.admitted_root_uri.clone(),
        feedback_scope: feedback_scope.clone(),
        github_pull_request_id,
        ci_discovery_config,
        hook_config_root: state.graph.hook_store_layout().data_root.clone(),
    });
    let work_cycle = Arc::clone(&advisory_cycle);
    let work =
        move |request: HookOrchestrationRequestV1,
              work_cancellation: tracedecay_runtime_core::cancellation::CancellationToken| {
            let cycle = Arc::clone(&work_cycle);
            async move { run_production_hook_cycle(cycle, request, work_cancellation).await }
        };
    let orchestrator =
        BoundedHookOrchestratorV1::new(1, work).ok_or_else(|| TraceDecayError::Config {
            message: "project-open hook orchestration capacity is invalid".to_owned(),
        })?;
    let lifecycle_registration = register_context_scout_lifecycle_authority(
        hook_project_id,
        hook_worktree_id,
        feedback_scope.project_id.clone(),
        feedback_scope.worktree_id.clone(),
        &state.session_db,
    );
    let lifecycle_registered_here = match lifecycle_registration {
        AuthorityRegistrationV1::Registered => true,
        AuthorityRegistrationV1::AlreadyRegistered => false,
        // A live authority already owns this hook locator pair under a
        // *different* native identity: the incumbent keeps serving lookups,
        // so this setup must fail rather than silently route Scout lifecycle
        // resolution at another project or worktree.
        AuthorityRegistrationV1::Conflict => {
            return Err(TraceDecayError::Config {
                message: "Context Scout lifecycle authority conflicts with the admitted hook scope"
                    .to_owned(),
            });
        }
        AuthorityRegistrationV1::Rejected(reason) => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "Context Scout lifecycle authority registration failed: {}",
                    reason.as_str()
                ),
            });
        }
    };
    let scout_hook_registration = ScoutHookRegistrationV1 {
        hook_project_id,
        hook_worktree_id,
        lifecycle_sessions: state.session_db.clone(),
        lifecycle_registered_here,
        orchestrator: Arc::clone(&orchestrator),
    };
    if !register_hook_orchestration_runtime(hook_project_id, hook_worktree_id, &orchestrator) {
        return Err(TraceDecayError::Config {
            message: "project-open hook orchestration authority is unavailable".to_owned(),
        });
    }
    let published_registration: Arc<dyn std::any::Any + Send + Sync> =
        Arc::new(PublishedAdvisoryRuntimeV1 {
            _registration: Arc::clone(&registration),
            _hook_notices: hook_notice_registration,
            _scout_hooks: scout_hook_registration,
        });
    let invocation_owner = DaemonAdvisoryCycleInvocationOwner::new(
        feedback_scope.project_id,
        Arc::clone(&advisory_cycle) as Arc<dyn DaemonAdvisoryCycleInvocationPort>,
    );
    invocation
        .advisory_runtime_registrar()
        .publish(
            project_root,
            published_registration,
            invocation_owner,
            advisory_cycle as Arc<dyn FeedbackCycleRuntimePort>,
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open advisory runtime publication failed: {error}"),
        })
}

/// Registers the daemon-owned Delivery read authority for this admitted
/// checkout as its own project-open component, before and independent of the
/// feedback/advisory owners whose mounts can stay deferred behind a sealed
/// code-index generation. A provider mount gate is retained as a typed
/// Delivery answer so the dashboard can tell "configure a token" apart from
/// "broken" even while the advisory chain never mounts.
async fn register_project_delivery_read_authority(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &ProjectOpenDependentOwnerState,
) -> Result<()> {
    let feedback_scope = match resolve_project_feedback_scope_v1(project_root, &state.scope) {
        Ok(scope) => scope,
        Err(error) => {
            // The admitted checkout raced away from its attached-branch
            // identity; without an exact feedback scope no Delivery authority
            // (ready or gated) can truthfully exist, so the dashboard keeps
            // its typed not-mounted answer.
            tracing::warn!(
                event = "delivery_read_mount",
                outcome = "unavailable",
                project = %project_root.display(),
                reason = %error,
                "project-open delivery read has no resolvable feedback scope"
            );
            return Ok(());
        }
    };
    let handle = match resolve_production_github_provider_access(invocation, project_root, state) {
        Ok(access) => {
            let review_bodies = github_anchor_authorities_arc_v1(
                state.database.clone(),
                project_root.to_path_buf(),
                feedback_scope.clone(),
                Arc::clone(&state.code_graph),
                Arc::new(invocation.code_index_schedulers.clone()),
            )
            .map(|authorities| ProjectDeliveryReviewBodySourceV1 {
                evidence: authorities.github_anchors,
                source_access: Arc::clone(&access.source_access),
            });
            match open_project_delivery_read_authority_v1(ProjectDeliveryReadOpenV1 {
                database: state.database.clone(),
                profile_id: state.session_db.binding().shard_id.profile_id.clone(),
                resolved_scope: state.scope.clone(),
                feedback_scope: feedback_scope.clone(),
                github_target: access.target.clone(),
                github_http: access.http.clone(),
                review_bodies,
            }) {
                ProjectDeliveryReadAuthorityOpenOutcomeV1::Ready(handle) => {
                    tracing::info!(
                        event = "delivery_read_mount",
                        outcome = "ready",
                        project = %project_root.display(),
                    );
                    handle
                }
                ProjectDeliveryReadAuthorityOpenOutcomeV1::Unavailable => {
                    tracing::warn!(
                        event = "delivery_read_mount",
                        outcome = "unavailable",
                        project = %project_root.display(),
                        "project-open delivery read authority could not open its retained stores"
                    );
                    return Ok(());
                }
            }
        }
        Err(gate) => {
            tracing::info!(
                event = "delivery_read_mount",
                outcome = "gated",
                project = %project_root.display(),
                gate = ?gate,
            );
            gated_project_delivery_read_handle_v1(feedback_scope, gate)
        }
    };
    invocation
        .advisory_runtime_registrar()
        .publish_delivery_read(
            project_root,
            RegisteredDeliveryReadAuthorityV1::new(
                project_root.to_path_buf(),
                state.scope.clone(),
                Arc::clone(state.graph.configuration_runtime()),
                handle,
            ),
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open delivery read registration failed: {error}"),
        })
}

struct ProductionGitHubProviderConfigV1 {
    github: Option<GitHubReviewRuntimeOwnerConfigV1>,
    github_source_access: Arc<dyn GitHubSourceAccessAuthorityV1>,
    ci: Option<ProductionCiProviderConfigV1>,
}

/// Locally resolved GitHub provider access for this admitted checkout: the
/// remote identity, mounted read-only credential, and source-access
/// authorities. Resolution is bounded local work (no network discovery), so
/// the Delivery read registration can consume it during project open.
struct ProductionGitHubProviderAccessV1 {
    owner: String,
    repository: String,
    credential: GitHubReadOnlyCredentialV1,
    source_access: Arc<dyn GitHubSourceAccessAuthorityV1>,
    ci_source_access: Arc<dyn CiSourceAccessAuthorityV1>,
    target: GitHubCiRepositoryTargetV1,
    http: GitHubHttpReadConfigV1,
}

fn resolve_production_github_provider_access(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &ProjectOpenDependentOwnerState,
) -> std::result::Result<ProductionGitHubProviderAccessV1, ProjectDeliveryProviderMountGateV1> {
    let Some(remote_url) = crate::tracedecay::git_remote_url(project_root) else {
        return Err(ProjectDeliveryProviderMountGateV1::NoGitRemote);
    };
    let Some((owner, repository)) = super::github_repository_from_remote(&remote_url) else {
        return Err(ProjectDeliveryProviderMountGateV1::NoGitRemote);
    };
    let profile_id = &state.session_db.binding().shard_id.profile_id;
    let credential = match invocation.mount_github_read_only_credential_authority_for_project(
        profile_id,
        &owner,
        &repository,
    ) {
        ProfileGitHubReadOnlyCredentialMountOutcomeV1::Public => {
            GitHubReadOnlyCredentialV1::anonymous()
        }
        ProfileGitHubReadOnlyCredentialMountOutcomeV1::NotConfigured => {
            return Err(ProjectDeliveryProviderMountGateV1::GitHubCredentialNotConfigured);
        }
        ProfileGitHubReadOnlyCredentialMountOutcomeV1::Rejected => {
            return Err(ProjectDeliveryProviderMountGateV1::GitHubAccessRefused);
        }
        ProfileGitHubReadOnlyCredentialMountOutcomeV1::Mounted => {
            match resolve_registered_github_read_only_credential_v1(&owner, &repository) {
                RegisteredGitHubReadOnlyCredentialV1::Verified(credential) => credential,
                RegisteredGitHubReadOnlyCredentialV1::Missing
                | RegisteredGitHubReadOnlyCredentialV1::Rejected => {
                    return Err(ProjectDeliveryProviderMountGateV1::GitHubAccessRefused);
                }
            }
        }
    };
    let configuration = OwnedGlobalDbConfigurationControlStore::from_registered_project_runtime_db(
        state.session_db.clone(),
    );
    let Some(configured_source_access) = ConfiguredGitHubSourceAccessAuthorityV1::new(
        configuration,
        state.scope.clone(),
        &owner,
        &repository,
    ) else {
        return Err(ProjectDeliveryProviderMountGateV1::GitHubSourceAccessUnavailable);
    };
    let configured_source_access = Arc::new(configured_source_access);
    let source_access: Arc<dyn GitHubSourceAccessAuthorityV1> = configured_source_access.clone();
    let ci_source_access: Arc<dyn CiSourceAccessAuthorityV1> = configured_source_access;
    let target = GitHubCiRepositoryTargetV1 {
        owner: owner.clone(),
        repository: repository.clone(),
    };
    Ok(ProductionGitHubProviderAccessV1 {
        owner,
        repository,
        credential,
        source_access,
        ci_source_access,
        target,
        http: GitHubHttpReadConfigV1::default(),
    })
}

async fn resolve_production_github_provider_config(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &ProjectOpenDependentOwnerState,
    feedback_scope: &FeedbackScopeV1,
) -> std::result::Result<ProductionGitHubProviderConfigV1, ProjectDeliveryProviderMountGateV1> {
    let ProductionGitHubProviderAccessV1 {
        owner,
        repository,
        credential,
        source_access,
        ci_source_access,
        target,
        http,
    } = resolve_production_github_provider_access(invocation, project_root, state)?;
    let ci = if credential.permits(GitHubReadPermissionV1::Actions)
        && credential.permits(GitHubReadPermissionV1::Checks)
    {
        production_ci_provider_config(&target, &credential, &http, ci_source_access)
    } else {
        None
    };
    let stack_observability =
        resolve_github_stack_observability(invocation, project_root, state, &owner, &repository)
            .await;
    let authorization_context =
        github_discovery_authorization_context(&state.access, feedback_scope);
    let discovery_request = github_discovery_source_access_request(feedback_scope);
    let head_commit_id = feedback_scope.head_commit_id.clone();
    let discovery_http = GitHubHttpReadConfigV1::default();
    let discovery_credential = credential.clone();
    let discovery = match authorization_context
        .as_ref()
        .zip(discovery_request.as_ref())
    {
        Some((context, request))
            if source_access.authorize(context, request).await
                == GitHubProviderLifecycleV1::Ready =>
        {
            let control =
                GitHubDiscoveryControlV1::bounded(Instant::now() + Duration::from_secs(15));
            let blocking_control = control.clone();
            tokio::task::spawn_blocking(move || {
                discover_exact_commit_pull_request_v1(
                    &owner,
                    &repository,
                    &head_commit_id,
                    &discovery_http,
                    &discovery_credential,
                    &blocking_control,
                )
            })
            .await
            .ok()
        }
        _ => None,
    };
    let github = match discovery {
        Some(GitHubExactCommitDiscoveryOutcomeV1::Found(pull)) => {
            let target = pull.target.clone();
            resolve_production_github_identity(project_root, feedback_scope, &target, pull).map(
                |identity| GitHubReviewRuntimeOwnerConfigV1 {
                    database: state.database.clone(),
                    resolved_scope: state.scope.clone(),
                    feedback_scope: feedback_scope.clone(),
                    target,
                    credential,
                    http: GitHubHttpReadConfigV1::default(),
                    identity,
                    stack_coordinator: invocation.github_stack_coordinator(),
                    stack_anchor_db: state.session_db.clone(),
                    stack_observability,
                },
            )
        }
        _ => None,
    };
    Ok(ProductionGitHubProviderConfigV1 {
        github,
        github_source_access: source_access,
        ci,
    })
}

/// Mounts the canonical Observatory lane for GitHub stack observations.
/// Telemetry mounting failure is logged and yields `None` — the review
/// refresh owner keeps its product path either way.
async fn resolve_github_stack_observability(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &ProjectOpenDependentOwnerState,
    github_owner: &str,
    github_repository: &str,
) -> Option<tracedecay_usecases::advisory::GitHubStackObservabilityV1> {
    let unavailable = |reason: &str, detail: String| {
        tracing::warn!(
            event = "github_stack_observability_mount",
            outcome = "unavailable",
            reason,
            detail,
            project = %project_root.display(),
            "GitHub stack observability lane is not mounted"
        );
    };
    let topology_policy = match crate::config::topology::resolved_work_topology_policy(
        &state.scout_configuration.snapshot,
    ) {
        Ok(policy) => policy.clone(),
        Err(error) => {
            unavailable("work_topology_policy", format!("{error:?}"));
            return None;
        }
    };
    // Mirrors the native-integration mount condition at project open: the
    // standard pull-request fallback exists exactly when this project is an
    // admitted Git worktree (project open fails earlier otherwise).
    let native_git_fallback_mounted = crate::worktree::git_worktree_root(project_root).is_some();
    let probe_owner = match tracedecay_usecases::observability::GitHubStackProbeOwnerV1::mount(
        state.scope.clone(),
        topology_policy,
        github_owner,
        github_repository,
        native_git_fallback_mounted,
    ) {
        Ok(probe_owner) => probe_owner,
        Err(error) => {
            unavailable("probe_owner", format!("{error:?}"));
            return None;
        }
    };
    let Some(producer) = invocation
        .service
        .observability_producer(Some(project_root))
        .await
    else {
        unavailable("producer_unmounted", String::new());
        return None;
    };
    Some(tracedecay_usecases::advisory::GitHubStackObservabilityV1 {
        probe_owner,
        producer,
        observation_db: state.session_db.clone(),
    })
}

/// Assembles the CI provider config for a credential that already proved
/// Actions and Checks read permissions. `None` covers only the statically
/// impossible identity-constant failures, never a permission decision.
fn production_ci_provider_config(
    target: &GitHubCiRepositoryTargetV1,
    credential: &GitHubReadOnlyCredentialV1,
    http: &GitHubHttpReadConfigV1,
    source_access: Arc<dyn CiSourceAccessAuthorityV1>,
) -> Option<ProductionCiProviderConfigV1> {
    Some(ProductionCiProviderConfigV1 {
        provider: ProviderId::new("provider.github-actions").ok()?,
        parser: CiFailureParserIdentityV1 {
            parser_id: "parser.github-actions.v1".to_owned(),
            parser_version: "1".to_owned(),
        },
        target: target.clone(),
        credential: credential.clone(),
        http: http.clone(),
        source_access,
    })
}

fn github_discovery_source_access_request(
    feedback_scope: &FeedbackScopeV1,
) -> Option<GitHubReviewReadRequestV1> {
    Some(GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
        scope: feedback_scope.clone(),
        pull_request_id: GitHubPullRequestIdV1::new(format!(
            "discovery.commit.{}",
            feedback_scope.head_commit_id.as_str()
        ))
        .ok()?,
    })
}

fn github_discovery_authorization_context(
    access: &tracedecay_usecases::source_authorization::ProjectSourceAccessSnapshot,
    feedback_scope: &FeedbackScopeV1,
) -> Option<RequestContext> {
    let observed_at = now_micros();
    if feedback_scope.validate().is_err()
        || access.scope.project_id != feedback_scope.project_id
        || access.scope.repository_id != feedback_scope.repository_id
        || access.scope.worktree_id != feedback_scope.worktree_id
        || access
            .scope
            .reference
            .as_ref()
            .map(tracedecay_domain::RefId::as_str)
            != Some(feedback_scope.branch_ref.as_str())
        || observed_at >= access.grant_expires_at
    {
        return None;
    }
    let capability = tracedecay_tool_catalog::CapabilityId::new(
        GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1.to_owned(),
    )
    .ok()?;
    let use_case =
        tracedecay_tool_catalog::UseCaseId::new(GITHUB_REVIEW_INGEST_USE_CASE_ID_V1.to_owned())
            .ok()?;
    if !access.effective_capabilities.contains(&capability) {
        return None;
    }
    let grant_digest = canonical_sha256(&(
        "tracedecay.project-open.github-discovery-grant.v1",
        &access.scope,
        &access.requester,
        &access.configuration_digest,
        &feedback_scope.head_commit_id,
        observed_at,
        access.grant_expires_at,
    ))
    .ok()?;
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!(
            "grant.tracedecay-daemon.project-open.github-discovery.{}",
            grant_digest.as_str().trim_start_matches("sha256:")
        ))
        .ok()?,
        POLICY_REVISION_V1,
        grant_digest,
        access.requester.clone(),
        observed_at,
        access.grant_expires_at,
        access.scope.clone(),
        std::collections::BTreeSet::from([capability]),
        std::collections::BTreeSet::from([use_case]),
        DisclosureClass::Evidence,
    )
    .ok()?;
    let request_id = tracedecay_usecases::request_identity::mint_global_request_id(
        tracedecay_usecases::request_identity::GlobalRequestSurface::ProjectOpenGithubDiscovery,
    )
    .ok()?;
    RequestContext::new(
        access.requester.clone(),
        access.scope.clone(),
        grant,
        request_id.clone(),
        Deadline::new(access.grant_expires_at).ok()?,
        CancellationContext::active(format!("cancel.{}", request_id.as_str())).ok()?,
    )
    .ok()
}

fn resolve_production_github_identity(
    project_root: &Path,
    feedback_scope: &FeedbackScopeV1,
    target: &GitHubRepositoryTargetV1,
    pull: tracedecay_usecases::advisory::github_runtime::GitHubExactCommitPullRequestV1,
) -> Option<GitHubReviewProviderIdentityV1> {
    let base = pull.base_commit_id;
    let head = pull.head_commit_id;
    if pull.target != *target || head != feedback_scope.head_commit_id {
        return None;
    }
    let merge_base = Command::new(crate::git::git_program())
        .args([
            "-C",
            &project_root.to_string_lossy(),
            "merge-base",
            base.as_str(),
            head.as_str(),
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| {
            matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        })?;
    let identity = GitHubReviewProviderIdentityV1 {
        provider: ProviderId::new("provider.github").ok()?,
        repository_owner: target.owner.clone(),
        repository_name: target.repository.clone(),
        pull_request_number: target.pull_request_number,
        base_commit_id: base,
        head_commit_id: head,
        merge_base_commit_id: CommitId::new(merge_base).ok()?,
    };
    identity.validate().then_some(identity)
}
