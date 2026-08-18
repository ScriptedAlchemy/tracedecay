//! Project-open composition for the canonical feedback and advisory owners.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    CiFailureParserIdentityV1, FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewReadOperationV1,
};
use tracedecay_domain::{CommitId, HostKindV1, ProviderId, UtcMicros, canonical_sha256};
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
use tracedecay_usecases::feedback::{
    FeedbackCycleLspInput, FeedbackCycleRuntime, ProductionFeedbackCycleAuthorizationFuture,
    ProductionFeedbackCycleAuthorizationPort, ProductionFeedbackCycleOpenV1,
    ProductionFeedbackRuntimeStateV1, resolve_production_feedback_cycle_parts,
};
use tracedecay_usecases::lsp_runtime::DaemonLspSessionFactory;
use tracedecay_usecases::operation_stream::OperationKind;

use super::{
    DaemonInvocationState, POLICY_REVISION_V1, daemon_owned_project_source_access_at,
    register_semantic_activation_owner,
};
use crate::daemon::service::invocation::{
    DaemonAdvisoryCycleInvocationFuture, DaemonAdvisoryCycleInvocationOwner,
    DaemonAdvisoryCycleInvocationPort, DaemonAdvisoryCycleInvocationRequest,
    advisory_cycle_invocation_result, daemon_operation_event_authority,
};
use crate::daemon::service::project_runtime::RegisteredDeliveryReadAuthorityV1;
use crate::errors::{Result, TraceDecayError};

mod deferred;
mod model;
pub(crate) use model::ProjectOpenDependentOwnerState;
use model::advisory_monotonic_deadline;
#[cfg(test)]
mod tests;

#[derive(Clone)]
struct ProjectOpenAdvisoryFeedbackCycleV1 {
    registration: Arc<AdvisoryProductionStartupRegistrationV1>,
    lsp_input: FeedbackCycleLspInput,
    root_uri: String,
    feedback_scope: FeedbackScopeV1,
    github_pull_request_id: Option<GitHubPullRequestIdV1>,
    ci_discovery_config: Option<ProductionCiProviderConfigV1>,
    hook_config_root: std::path::PathBuf,
}

struct ProjectOpenAdvisoryCycleExecutionV1 {
    context: RequestContext,
    outcome: AdvisoryCycleOutcome,
}

struct PublishedAdvisoryRuntimeV1 {
    _registration: Arc<AdvisoryProductionStartupRegistrationV1>,
    _hook_notices: AdvisoryHookNoticeRegistrationV1,
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

impl ProjectOpenAdvisoryFeedbackCycleV1 {
    async fn run_cycle(
        &self,
        request: FeedbackCycleRequest,
        deadline: MonotonicDeadline,
    ) -> std::result::Result<ProjectOpenAdvisoryCycleExecutionV1, LspRuntimeFailure> {
        let invocation = (self.lsp_input)(request).await?;
        let observed_at = invocation.request.input.observed_at;
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

pub(super) async fn register_production_feedback_and_advisory(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &ProjectOpenDependentOwnerState,
    lsp_session_factory: Arc<DaemonLspSessionFactory>,
) -> Result<()> {
    let (feedback_cycle, feedback_scope, lsp_input) =
        register_production_feedback_cycle(invocation, project_root, state).await?;
    register_production_advisory_owner(
        invocation,
        project_root,
        state,
        feedback_cycle,
        feedback_scope,
        lsp_input,
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
        } else {
            tracing::info!(
                event = "project_open_owner_phase",
                project = %project_root.display(),
                phase = "feedback_advisory_registered",
            );
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
) -> Result<(
    Arc<FeedbackCycleRuntime>,
    FeedbackScopeV1,
    FeedbackCycleLspInput,
)> {
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
    let lsp_input = Arc::clone(&parts.lsp_input);
    if let Some(runtime) = invocation.service.feedback_cycle(Some(project_root)).await {
        return Ok((runtime, feedback_scope, lsp_input));
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
    Ok((runtime, feedback_scope, lsp_input))
}

async fn register_production_advisory_owner(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &ProjectOpenDependentOwnerState,
    feedback_cycle: Arc<FeedbackCycleRuntime>,
    feedback_scope: FeedbackScopeV1,
    lsp_input: FeedbackCycleLspInput,
    lsp_session_factory: Arc<DaemonLspSessionFactory>,
) -> Result<()> {
    let remote =
        resolve_production_github_provider_config(invocation, project_root, state, &feedback_scope)
            .await;
    let delivery_read = match &remote {
        Ok(remote) => {
            let review_bodies = github_anchor_authorities_arc_v1(
                state.database.clone(),
                project_root.to_path_buf(),
                feedback_scope.clone(),
                Arc::clone(&state.code_graph),
                Arc::new(invocation.code_index_schedulers.clone()),
            )
            .map(|authorities| ProjectDeliveryReviewBodySourceV1 {
                evidence: authorities.github_anchors,
                source_access: Arc::clone(&remote.github_source_access),
            });
            match open_project_delivery_read_authority_v1(ProjectDeliveryReadOpenV1 {
                database: state.database.clone(),
                profile_id: state.session_db.binding().shard_id.profile_id.clone(),
                resolved_scope: state.scope.clone(),
                feedback_scope: feedback_scope.clone(),
                github_target: remote.target.clone(),
                github_http: remote.http.clone(),
                review_bodies,
            }) {
                ProjectDeliveryReadAuthorityOpenOutcomeV1::Ready(handle) => {
                    Some(RegisteredDeliveryReadAuthorityV1::new(
                        project_root.to_path_buf(),
                        state.scope.clone(),
                        Arc::clone(state.graph.configuration_runtime()),
                        handle,
                    ))
                }
                ProjectDeliveryReadAuthorityOpenOutcomeV1::Unavailable => None,
            }
        }
        // The provider mount gate is retained as a typed Delivery answer so
        // the dashboard can tell "configure a token" apart from "broken".
        Err(gate) => Some(RegisteredDeliveryReadAuthorityV1::new(
            project_root.to_path_buf(),
            state.scope.clone(),
            Arc::clone(state.graph.configuration_runtime()),
            gated_project_delivery_read_handle_v1(feedback_scope.clone(), *gate),
        )),
    };
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
    let published_registration: Arc<dyn std::any::Any + Send + Sync> =
        Arc::new(PublishedAdvisoryRuntimeV1 {
            _registration: Arc::clone(&registration),
            _hook_notices: hook_notice_registration,
        });
    let advisory_cycle = Arc::new(ProjectOpenAdvisoryFeedbackCycleV1 {
        registration: Arc::clone(&registration),
        lsp_input,
        root_uri: state.admitted_root_uri.clone(),
        feedback_scope: feedback_scope.clone(),
        github_pull_request_id,
        ci_discovery_config,
        hook_config_root: state.graph.hook_store_layout().data_root.clone(),
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
            delivery_read,
            invocation_owner,
            advisory_cycle as Arc<dyn FeedbackCycleRuntimePort>,
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open advisory runtime publication failed: {error}"),
        })
}

struct ProductionGitHubProviderConfigV1 {
    target: GitHubCiRepositoryTargetV1,
    http: GitHubHttpReadConfigV1,
    github: Option<GitHubReviewRuntimeOwnerConfigV1>,
    github_source_access: Arc<dyn GitHubSourceAccessAuthorityV1>,
    ci: Option<ProductionCiProviderConfigV1>,
}

async fn resolve_production_github_provider_config(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &ProjectOpenDependentOwnerState,
    feedback_scope: &FeedbackScopeV1,
) -> std::result::Result<ProductionGitHubProviderConfigV1, ProjectDeliveryProviderMountGateV1> {
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
    let http = GitHubHttpReadConfigV1::default();
    let ci = if credential.permits(GitHubReadPermissionV1::Actions)
        && credential.permits(GitHubReadPermissionV1::Checks)
    {
        production_ci_provider_config(&target, &credential, &http, ci_source_access)
    } else {
        None
    };
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
                },
            )
        }
        _ => None,
    };
    Ok(ProductionGitHubProviderConfigV1 {
        target,
        http,
        github,
        github_source_access: source_access,
        ci,
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
