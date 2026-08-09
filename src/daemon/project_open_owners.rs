//! Project-open registration for Git, feedback, and advisory production owners.
//!
//! After Scout bootstrap and successful cache publication, the daemon mounts
//! concrete feedback, cycle, primitive, LSP, advisory, and Hook/Scout host-
//! delivery owners from the admitted project identity. Cycle/LSP/advisory mount
//! only when their real upstream authorities resolve; missing identity fails
//! closed and placeholder owners are never installed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tracedecay_application::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1,
    FEEDBACK_EXPAND_CAPABILITY_ID_V1, FEEDBACK_GET_CAPABILITY_ID_V1,
    FEEDBACK_LIST_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
    GitHubReviewReadRequestV1, PROXIMITY_CAPABILITY_ID_V1, ProximityEvaluationRequestV1,
};
use tracedecay_application::{
    ApplicationContractError, ApplicationProblem, RequestContext, ResolvedScope, SafeDiagnostic,
    now_micros,
};
use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, AuthorityRef, CapabilityResolutionContextV1, ConfigurationValueV1,
    SOURCE_BINDINGS_SETTING_KEY, ScopeSourceBinding, SettingKey, SourceBindingId, SourceKindV1,
    resolve_restrictive_capabilities,
};
use tracedecay_domain::feedback::{
    CiFailureParserIdentityV1, FeedbackScopeV1, FeedbackTriggerV1, GitHubPullRequestIdV1,
    GitHubReviewReadOperationV1,
};
use tracedecay_domain::{
    ActorId, CapabilityId as DomainCapabilityId, CommitId, LocatorDigest, ProjectId, ProviderId,
    RefId, UtcMicros, canonical_sha256,
};
use tracedecay_hooks::{HookFeedbackDeliveryRouteV1, HookFeedbackRollbackSwitchV1, HookHostV1};
use tracedecay_lsp::{
    DiagnosticTrigger, FeedbackCycleRequest, FeedbackCycleRuntimePort, LspRuntimeFailure,
    LspRuntimeFuture,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::service::invocation::HookOrchestrationPortV1;
use super::{
    BoundedHookOrchestratorV1, DaemonAdvisoryCycleInvocationFuture,
    DaemonAdvisoryCycleInvocationOwner, DaemonAdvisoryCycleInvocationPort,
    DaemonAdvisoryCycleInvocationRequest, DaemonContextScoutRuntimeRegistrationError,
    DaemonFeedbackRuntimeRegistrationError, DaemonInvocationState,
    DaemonPrimitiveRuntimeRegistrationError, HookOrchestrationRequestV1,
    HookOrchestrationTriggerV1, advisory_cycle_invocation_result,
};
use crate::agents::context_scout_ports::{
    ContextScoutAuthorityPinV1, ContextScoutCanonicalInputAssemblerV1,
    ContextScoutConfigurationPinV1, ProjectContextScoutAddressRegistryV1,
};
use crate::request_identity::{
    GlobalRequestSurface, PreviewIdentityDomain, derive_preview_identity, mint_global_request_id,
};

const SOURCE_EDIT_PRIVACY_KEY_EPOCH_V1: u64 = 1;
use crate::agents::context_scout_v2::{
    ContextScoutDeliverySelectionInputV1, ContextScoutOutcomeV1, ContextScoutServiceStateV1,
    ContextScoutTriggerV1,
};
use crate::agents::host_bundle_v2::HostKindV1;
use crate::daemon::context_scout_lifecycle::AuthorityRegistrationV1;
use crate::daemon::git_transactions::DaemonGitIndexTransactionServiceRegistry;
use crate::daemon::native_integration::DaemonNativeIntegrationServiceRegistry;
use crate::daemon::service::invocation::{
    daemon_operation_event_authority, observe_accepted_feedback_cycle_terminal,
};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::configuration::OwnedGlobalDbConfigurationControlStore;
use crate::mcp::McpServer;
use crate::mcp::tools::handlers::hook_runtime::daemon_mint_hook_v2_file_id;
use tracedecay_lsp::analyzer::broker::{AdmittedLspProvider, MountedLspProvider};
use tracedecay_lsp::analyzer::client::LspRefreshTimeouts;
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};
use tracedecay_usecases::advisory::github_runtime::{
    ConfiguredGitHubSourceAccessAuthorityV1, GitHubDiscoveryControlV1,
    GitHubExactCommitDiscoveryOutcomeV1, GitHubProviderLifecycleV1, GitHubSourceAccessAuthorityV1,
    ProfileGitHubReadOnlyCredentialMountOutcomeV1, discover_exact_commit_pull_request_v1,
    resolve_registered_github_read_only_credential_v1,
};
use tracedecay_usecases::advisory::{
    AdvisoryCycleControl, AdvisoryCycleOutcome, AdvisoryCycleRequest, AdvisoryHookLookupNoticeV1,
    AdvisoryHookNoticeQueueV1, AdvisoryHookNoticeSinkV1, AdvisoryProductionOpenV1,
    AdvisoryProductionStartupRegistrationV1, AdvisoryRuntimeOpenV1, CiSourceAccessAuthorityV1,
    GitHubCiRepositoryTargetV1, GitHubHttpReadConfigV1, GitHubReadOnlyCredentialV1,
    GitHubReadPermissionV1, GitHubRepositoryTargetV1, GitHubReviewProviderIdentityV1,
    GitHubReviewRuntimeOwnerConfigV1, ProductionCiProviderConfigV1, ProjectCiCodeAnchorStoreV1,
    ProjectCiRetainedObservationStoreV1, discover_production_ci_failure_request_v1,
    register_advisory_hook_notice_queue, unregister_advisory_hook_notice_queue,
};
use tracedecay_usecases::feedback::observations::{
    FeedbackDeliveryRouteV1, FeedbackObservationEmitterV1, FeedbackOperationV1, FeedbackOutcomeV1,
    FeedbackSourceEventV1,
};
use tracedecay_usecases::feedback::{
    FeedbackCycleInvocation, FeedbackCycleLspInput, FeedbackCycleRuntime,
    ProductionFeedbackCycleAuthorizationFuture, ProductionFeedbackCycleAuthorizationPort,
    ProductionFeedbackCycleOpenV1, ProductionFeedbackRuntimeStateV1,
    resolve_production_feedback_cycle_parts,
};
use tracedecay_usecases::lsp_runtime::DaemonLspSessionFactory;
use tracedecay_usecases::operation_stream::OperationKind;
use tracedecay_usecases::primitives::{
    ProductionPrimitiveOpenRequestV1, admitted_root_uri_for_project, locator_digest_for_project,
    open_production_primitive_runtime,
};
use tracedecay_usecases::source_authorization::ProjectSourceAccessSnapshot;

mod advisory_upgrade;
mod lsp_registration;
mod query_authority_upgrade;
mod source_edit_owner;

use lsp_registration::production_lsp_registration;
use source_edit_owner::{
    install_project_open_source_edit_rollback_owner, source_edit_authority_error,
    source_edit_contract_error, source_edit_request_context,
};

#[cfg(test)]
use crate::graph_semantic_capabilities;
#[cfg(test)]
use std::collections::BTreeMap;

const DAEMON_REQUESTER: &str = "actor.tracedecay-daemon.project-open";
const DAEMON_BINDING: &str = "binding.tracedecay-daemon.project-open";
const GRANT_HORIZON: Duration = Duration::from_hours(24);
const POLICY_REVISION_V1: u64 = 1;
const LSP_DIAGNOSTICS_QUIET: Duration = Duration::from_secs(2);
pub(super) const LSP_WORKSPACE_CAPABILITY_ID_V1: &str =
    "capability.application.lsp.workspace-folders";
pub(super) const LSP_WORKSPACE_USE_CASE_ID_V1: &str = "use-case.application.lsp.workspace-folders";

#[derive(Clone)]
struct ProjectOpenAdvisoryFeedbackCycleV1 {
    registration: Arc<AdvisoryProductionStartupRegistrationV1>,
    lsp_input: FeedbackCycleLspInput,
    root_uri: String,
    feedback_scope: FeedbackScopeV1,
    github_pull_request_id: Option<GitHubPullRequestIdV1>,
    ci_discovery_config: Option<ProductionCiProviderConfigV1>,
}

struct ProjectOpenAdvisoryCycleExecution {
    context: RequestContext,
    outcome: AdvisoryCycleOutcome,
}

/// Process-wide discovery registrations (Scout lifecycle authority + hook
/// notice queue) held by one advisory setup. Dropping the lease before its
/// publication commits unwinds exactly the registrations this setup made,
/// never an incumbent's or a successor's.
struct AdvisoryDiscoveryRegistrationLeaseV1 {
    hook_project_id: [u8; 16],
    hook_worktree_id: [u8; 16],
    lifecycle_session_db: Arc<crate::global_db::RegisteredGlobalDb>,
    lifecycle_registered_here: bool,
    hook_notices: Arc<AdvisoryHookNoticeQueueV1>,
    hook_notices_registered: bool,
}

/// The complete published advisory bundle retained by the project runtime.
/// Its lifetime carries the startup registration and the discovery lease, so
/// withdrawing the runtime unwinds both.
struct PublishedAdvisoryRuntimeV1 {
    _registration: Arc<AdvisoryProductionStartupRegistrationV1>,
    _discovery: AdvisoryDiscoveryRegistrationLeaseV1,
}

impl Drop for AdvisoryDiscoveryRegistrationLeaseV1 {
    fn drop(&mut self) {
        if self.hook_notices_registered {
            unregister_advisory_hook_notice_queue(
                self.hook_project_id,
                self.hook_worktree_id,
                &self.hook_notices,
            );
        }
        if self.lifecycle_registered_here {
            crate::daemon::context_scout_lifecycle::unregister_context_scout_lifecycle_authority(
                self.hook_project_id,
                self.hook_worktree_id,
                &self.lifecycle_session_db,
            );
        }
    }
}

impl ProjectOpenAdvisoryFeedbackCycleV1 {
    async fn run_cycle(
        &self,
        request: FeedbackCycleRequest,
        deadline: MonotonicDeadline,
    ) -> std::result::Result<ProjectOpenAdvisoryCycleExecution, LspRuntimeFailure> {
        let registration = Arc::clone(&self.registration);
        let lsp_input = Arc::clone(&self.lsp_input);
        let feedback_scope = self.feedback_scope.clone();
        let github_pull_request_id = self.github_pull_request_id.clone();
        let ci_discovery_config = self.ci_discovery_config.clone();
        let terminal_request = request.clone();
        let invocation = match lsp_input(request).await {
            Ok(invocation) => invocation,
            Err(error) => {
                observe_accepted_feedback_cycle_terminal(
                    &registration.host_delivery.source_observations,
                    &feedback_scope.project_id,
                    &terminal_request,
                    FeedbackOutcomeV1::Unavailable,
                );
                return Err(error);
            }
        };
        let observed_at = invocation.request.input.observed_at;
        let ci = match ci_discovery_config.as_ref() {
            Some(config) => {
                discover_production_ci_failure_request_v1(
                    &invocation.context,
                    config,
                    &feedback_scope,
                )
                .await
            }
            None => {
                tracedecay_usecases::advisory::ProductionCiFailureDiscoveryOutcomeV1::NotConfigured
            }
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
                // `LspRuntimeFailure` is a bounded protocol-safe class with no room for
                // a cause, so the underlying authority error is only recoverable from the
                // daemon log: emit it here rather than discarding it at the boundary.
                tracing::warn!(
                    target: "tracedecay::feedback_advisory_cycle",
                    project_id = self.feedback_scope.project_id.as_str(),
                    worktree_id = self.feedback_scope.worktree_id.as_str(),
                    %error,
                    "advisory feedback cycle could not begin its operation event"
                );
                LspRuntimeFailure::new("feedback-cycle-advisory-operation")
            })?;
        let advisory = AdvisoryCycleRequest {
            feedback: invocation.request,
            github: github_pull_request_id.map(|pull_request_id| GitHubReviewReadRequestV1 {
                operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
                scope: feedback_scope.clone(),
                pull_request_id,
            }),
            ci,
            proximity: Some(ProximityEvaluationRequestV1 {
                scope: feedback_scope,
                observed_at,
            }),
            validity: tracedecay_application::AdvisoryFindingValidityWindowV1 {
                valid_at: observed_at,
                expires_at,
            },
        };
        let outcome = registration
            .runtime()
            .run_once(
                &invocation.context,
                AdvisoryCycleControl {
                    operation,
                    deadline,
                },
                advisory,
            )
            .await
            .map_err(|error| {
                // Same boundary constraint as the operation-begin arm above: keep the
                // contract error attributable in the daemon log before it is narrowed to
                // an opaque runtime failure class.
                tracing::warn!(
                    target: "tracedecay::feedback_advisory_cycle",
                    project_id = self.feedback_scope.project_id.as_str(),
                    worktree_id = self.feedback_scope.worktree_id.as_str(),
                    %error,
                    "advisory feedback cycle execution failed"
                );
                LspRuntimeFailure::new("feedback-cycle-advisory-execution")
            })?;
        Ok(ProjectOpenAdvisoryCycleExecution {
            context: invocation.context,
            outcome,
        })
    }
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
            let remaining_micros = request.deadline.expires_at.0.saturating_sub(now_micros().0);
            if remaining_micros <= 0 {
                return Err(ApplicationProblem::timed_out_before_admission());
            }
            let execution = owner
                .run_cycle(
                    FeedbackCycleRequest {
                        root_uri: owner.root_uri.clone(),
                        document_uri: request.document_uri,
                        trigger: DiagnosticTrigger::ExplicitDocumentDiagnostics,
                    },
                    MonotonicDeadline::at(
                        Instant::now() + Duration::from_micros(remaining_micros as u64),
                    ),
                )
                .await
                .map_err(|failure| {
                    // The runtime failure class names the stage that failed
                    // (lsp-input, advisory-operation, advisory-execution, ...). Carrying
                    // it into the typed problem detail is what makes an unavailable
                    // advisory cycle attributable to a caller; the class is already
                    // bounded and protocol-safe by `LspRuntimeFailure::new`.
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
    scope: ResolvedScope,
    configuration: Arc<tracedecay_usecases::configuration::ProjectConfigurationRuntime>,
}

#[derive(Clone)]
struct ProjectOpenSourceEditAuthorizationV1 {
    project_root: std::path::PathBuf,
    scope: ResolvedScope,
    configuration: Arc<tracedecay_usecases::configuration::ProjectConfigurationRuntime>,
}

struct CurrentSourceEditAuthorityV1 {
    receipt: tracedecay_application::AuthorityReceipt,
    proof: tracedecay_application::SourceEditEffectProofV1,
}

impl ProjectOpenSourceEditAuthorizationV1 {
    async fn current_access(
        &self,
        observed_at: UtcMicros,
    ) -> std::result::Result<ProjectSourceAccessSnapshot, tracedecay_application::ApplicationProblem>
    {
        let current = self
            .configuration
            .client()
            .current()
            .await
            .map_err(|_| concealed_source_edit_problem())?;
        daemon_owned_project_source_access_at(
            &self.scope,
            &self.project_root,
            &current,
            observed_at,
        )
        .map_err(|_| concealed_source_edit_problem())
    }

    async fn current_authority(
        &self,
        context: &tracedecay_application::RequestContext,
        operation: &tracedecay_application::ApplicationOperation,
        observed_at: UtcMicros,
    ) -> std::result::Result<CurrentSourceEditAuthorityV1, tracedecay_application::ApplicationProblem>
    {
        let access = self.current_access(observed_at).await?;
        if context.admission_at(observed_at) != tracedecay_application::RequestAdmission::Admitted
            || !access.allows(context, operation, observed_at)
        {
            return Err(concealed_source_edit_problem());
        }
        let catalog = crate::catalog_composition::build_application_catalog_snapshot()
            .map_err(|_| concealed_source_edit_problem())?;
        let manifest = catalog
            .capability(operation.capability_id())
            .ok_or_else(concealed_source_edit_problem)?;
        let catalog_digest = tracedecay_domain::ManifestDigest::new(catalog.digest().to_string())
            .map_err(|_| concealed_source_edit_problem())?;
        let privacy_domain_id = tracedecay_domain::PrivacyDomainId::new(format!(
            "privacy.local-source-edit.{}",
            access.scope.project_id.as_str()
        ))
        .map_err(|_| concealed_source_edit_problem())?;
        let privacy_digest = canonical_sha256(&(
            "tracedecay.daemon.source-edit-privacy.v1",
            &privacy_domain_id,
            SOURCE_EDIT_PRIVACY_KEY_EPOCH_V1,
            manifest.privacy(),
            manifest.denied_disclosure(),
            manifest.scope(),
            &access.binding,
            &access.configuration_provenance_digest,
        ))
        .map_err(|_| concealed_source_edit_problem())?;
        let policy_digest = canonical_sha256(&(
            "tracedecay.daemon.source-edit-policy.v1",
            &access.scope,
            &access.requester,
            &access.binding,
            &access.configuration_digest,
            &access.configuration_provenance_digest,
            operation.capability_id(),
            operation.use_case_id(),
            &catalog_digest,
            &privacy_digest,
        ))
        .map_err(|_| concealed_source_edit_problem())?;
        let policy = tracedecay_application::PolicyDecisionRef::new(
            "policy.daemon.source-edit.v1",
            POLICY_REVISION_V1,
            policy_digest,
            tracedecay_domain::ComponentVersion::new("tracedecay.daemon.source-edit-policy.v1")
                .map_err(|_| concealed_source_edit_problem())?,
        )
        .map_err(|_| concealed_source_edit_problem())?;
        let receipt =
            tracedecay_application::AuthorityReceipt::from_context(context, policy, observed_at)
                .map_err(|_| concealed_source_edit_problem())?;
        let proof = tracedecay_application::SourceEditEffectProofV1 {
            policy_digest: receipt.policy.digest.clone(),
            configuration_revision_id: access.configuration_revision,
            configuration_digest: access.configuration_digest,
            catalog_revision: manifest.routing().revision(),
            catalog_digest,
            privacy_domain_id,
            privacy_key_epoch: SOURCE_EDIT_PRIVACY_KEY_EPOCH_V1,
            privacy_digest,
            external_proof: None,
        };
        proof
            .validate_for(&receipt)
            .map_err(|_| concealed_source_edit_problem())?;
        Ok(CurrentSourceEditAuthorityV1 { receipt, proof })
    }
}

impl tracedecay_application::SourceEditAuthorizationPort for ProjectOpenSourceEditAuthorizationV1 {
    fn admit<'a>(
        &'a self,
        context: &'a tracedecay_application::RequestContext,
        operation: &'a tracedecay_application::ApplicationOperation,
        observed_at: UtcMicros,
    ) -> tracedecay_application::SourceEditAuthorizationFuture<'a> {
        Box::pin(async move {
            self.current_authority(context, operation, observed_at)
                .await
                .and_then(|current| {
                    tracedecay_application::SourceEditAuthorizationAdmissionV1::new(
                        current.receipt,
                        current.proof,
                        context.scope(),
                    )
                    .map_err(|_| concealed_source_edit_problem())
                })
        })
    }

    fn recheck_effect<'a>(
        &'a self,
        context: &'a tracedecay_application::RequestContext,
        operation: &'a tracedecay_application::ApplicationOperation,
        admission: &'a tracedecay_application::SourceEditAuthorizationAdmissionV1,
        observed_at: UtcMicros,
    ) -> tracedecay_application::SourceEditAuthorizationFuture<'a> {
        Box::pin(async move {
            let current = self
                .current_authority(context, operation, observed_at)
                .await?;
            if current.receipt.grant_id != admission.receipt.grant_id
                || current.receipt.grant_revision != admission.receipt.grant_revision
                || current.receipt.grant_digest != admission.receipt.grant_digest
                || current.receipt.authorized_scope_digest
                    != admission.receipt.authorized_scope_digest
                || current.receipt.disclosure != admission.receipt.disclosure
                || current.receipt.policy != admission.receipt.policy
                || current.proof != admission.proof
            {
                return Err(concealed_source_edit_problem());
            }
            tracedecay_application::SourceEditAuthorizationAdmissionV1::new(
                current.receipt,
                current.proof,
                context.scope(),
            )
            .map_err(|_| concealed_source_edit_problem())
        })
    }
}

fn concealed_source_edit_problem() -> tracedecay_application::ApplicationProblem {
    tracedecay_application::ApplicationProblem::not_found_or_not_authorized(
        tracedecay_application::RetryDirective::Never,
    )
}

async fn invoke_project_open_source_edit(
    graph: Arc<crate::tracedecay::TraceDecay>,
    code_graph: Arc<dyn tracedecay_usecases::graph::CodeGraphProjectionReadPort>,
    authorization: ProjectOpenSourceEditAuthorizationV1,
    invocation: crate::mcp::server::SourceEditInvocationV1,
) -> Result<tracedecay_usecases::edit::SourceEditSurfaceResultV1> {
    let observed_at = now_micros();
    let operation = tracedecay_application::source_edit_operation(invocation.edit.kind())
        .map_err(source_edit_contract_error)?;
    let access = authorization
        .current_access(observed_at)
        .await
        .map_err(|_| source_edit_authority_error())?;
    let context = source_edit_request_context(
        &access,
        invocation.request_id,
        &operation,
        observed_at,
        invocation.deadline.clone(),
        invocation.cancellation.context(),
    )?;
    let effect_control = tracedecay_usecases::edit::SourceEditEffectControlV1::new(
        context.deadline().clone(),
        invocation.cancellation.clone(),
    );
    let current = authorization
        .current_authority(&context, &operation, observed_at)
        .await
        .map_err(|_| source_edit_authority_error())?;
    let dry_run = invocation.edit.dry_run();
    let idempotency_key = match invocation.idempotency_key {
        Some(key) => key,
        None if dry_run => {
            let preview_identity = derive_preview_identity(
                PreviewIdentityDomain::SourceEdit,
                context.request_id(),
                &invocation.edit,
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!("source edit preview identity failed: {error}"),
            })?;
            tracedecay_application::IdempotencyKey::new(format!("preview.{preview_identity}"))
                .map_err(source_edit_contract_error)?
        }
        None => {
            return Err(TraceDecayError::Config {
                message: "source edit apply requires an idempotency key".to_owned(),
            });
        }
    };
    let expected_state = match invocation.expected_state {
        Some(state) => state,
        None if dry_run => canonical_sha256(&(
            "tracedecay.source-edit-preview-unbound-state.v1",
            context.request_id(),
            &invocation.edit,
        ))
        .map_err(|error| TraceDecayError::Config {
            message: format!("source edit preview state identity failed: {error}"),
        })?,
        None => {
            return Err(TraceDecayError::Config {
                message: "source edit apply requires an expected state".to_owned(),
            });
        }
    };
    let request = tracedecay_application::SourceEditEffectRequestV1 {
        context,
        authority: current.receipt.clone(),
        edit: invocation.edit,
        idempotency_key,
        expected_state,
        proof: current.proof,
        observed_at,
    };
    tracedecay_usecases::edit::execute_source_edit_with_control(
        &*graph,
        code_graph.as_ref(),
        &operation,
        request,
        &authorization,
        &effect_control,
    )
    .await
}

async fn invoke_project_open_source_edit_reconciliation(
    graph: Arc<crate::tracedecay::TraceDecay>,
    authorization: ProjectOpenSourceEditAuthorizationV1,
    invocation: crate::mcp::server::SourceEditReconciliationInvocationV1,
) -> Result<tracedecay_usecases::edit::SourceEditSurfaceResultV1> {
    let observed_at = now_micros();
    let effect_control = tracedecay_usecases::edit::SourceEditEffectControlV1::new(
        invocation.deadline.clone(),
        invocation.cancellation.clone(),
    );
    let operation = tracedecay_application::source_edit_reconciliation_operation()
        .map_err(source_edit_contract_error)?;
    let access = authorization
        .current_access(observed_at)
        .await
        .map_err(|_| source_edit_authority_error())?;
    let context = source_edit_request_context(
        &access,
        invocation.request_id,
        &operation,
        observed_at,
        invocation.deadline,
        invocation.cancellation.context(),
    )?;
    let current = authorization
        .current_authority(&context, &operation, observed_at)
        .await
        .map_err(|_| source_edit_authority_error())?;
    let request = tracedecay_application::SourceEditReconciliationRequestV1 {
        context,
        authority: current.receipt.clone(),
        kind: invocation.kind,
        effect_id: invocation.effect_id,
        idempotency_key: invocation.idempotency_key,
        attempt_idempotency_key: invocation.attempt_idempotency_key,
        input_digest: invocation.input_digest,
        disposition: invocation.disposition,
        proof: current.proof,
        observed_at,
    };
    tracedecay_usecases::edit::reconcile_source_edit_effect_unknown_with_control(
        &*graph,
        request,
        &authorization,
        &effect_control,
    )
    .await
}

/// Publication state of the daemon-owned source-edit mutation lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceEditMutationState {
    /// Owner registration is still mounting the exact mutation authority. A
    /// caller that retries later can succeed.
    Warming,
    /// The mutation authority is published.
    Ready,
    /// Owner registration failed. This server's mutation lane never opens, so
    /// retrying the same request against it cannot succeed.
    Failed,
}

/// Gates source-edit mutations on the state of their daemon-owned authority.
///
/// The preview executors are installed with the read-only core, so a mutation
/// request can arrive before owner registration reaches the Git transaction
/// authority — or after registration failed outright. Both cases fail closed,
/// but only the first is retryable: reporting a failed publication as "warming"
/// invites a caller to retry a lane that will never open.
#[derive(Debug)]
pub(crate) struct SourceEditMutationGate {
    state: AtomicU8,
}

impl SourceEditMutationGate {
    const WARMING: u8 = 0;
    const READY: u8 = 1;
    const FAILED: u8 = 2;

    pub(crate) fn warming() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU8::new(Self::WARMING),
        })
    }

    #[cfg(feature = "test-transport")]
    pub(crate) fn ready() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU8::new(Self::READY),
        })
    }

    pub(crate) fn state(&self) -> SourceEditMutationState {
        match self.state.load(Ordering::Acquire) {
            Self::READY => SourceEditMutationState::Ready,
            Self::FAILED => SourceEditMutationState::Failed,
            _ => SourceEditMutationState::Warming,
        }
    }

    pub(crate) fn mark_ready(&self) {
        self.state.store(Self::READY, Ordering::Release);
    }

    /// Retires the mutation lane. A publication that failed after opening the
    /// lane still retires the whole server, so this overwrites `Ready` rather
    /// than leaving mutations authorized against a server being torn down.
    pub(crate) fn mark_failed(&self) {
        self.state.store(Self::FAILED, Ordering::Release);
    }

    pub(crate) fn authorize_mutation(&self, lane: &str) -> Result<()> {
        match self.state() {
            SourceEditMutationState::Ready => Ok(()),
            SourceEditMutationState::Warming => Err(TraceDecayError::Config {
                message: format!("daemon-owned source edit {lane} authority is warming"),
            }),
            SourceEditMutationState::Failed => Err(TraceDecayError::Config {
                message: format!(
                    "daemon-owned source edit {lane} authority failed to publish; reopen the project"
                ),
            }),
        }
    }
}

struct ProjectCodeGraphProjectionReadPortV1 {
    schedulers: crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: PathBuf,
    scope: ResolvedScope,
}

impl tracedecay_usecases::graph::CodeGraphProjectionReadPort
    for ProjectCodeGraphProjectionReadPortV1
{
    fn open<'a>(
        &'a self,
        request: tracedecay_usecases::graph::CodeGraphReadRequest<'a>,
    ) -> tracedecay_usecases::graph::CodeGraphReadFuture<'a> {
        Box::pin(async move {
            use tracedecay_usecases::graph::{CodeGraphReadError, VerifiedCodeGraphRead};

            request
                .context
                .validate()
                .map_err(|error| CodeGraphReadError::InvalidRequest {
                    detail: error.to_string(),
                })?;
            if request.context.scope() != &self.scope {
                return Err(CodeGraphReadError::Denied);
            }
            if request.cancellation.is_cancelled() {
                return Err(CodeGraphReadError::Cancelled);
            }
            match request.context.admission_at(request.observed_at) {
                tracedecay_application::RequestAdmission::Admitted => {}
                tracedecay_application::RequestAdmission::Cancelled => {
                    return Err(CodeGraphReadError::Cancelled);
                }
                tracedecay_application::RequestAdmission::TimedOut => {
                    return Err(CodeGraphReadError::TimedOut);
                }
            }
            let latest = self
                .schedulers
                .latest_complete_ready_decoded_for_root_scope(&self.project_root, &self.scope)
                .await
                .ok_or_else(|| CodeGraphReadError::Unavailable {
                    detail: "the verified code graph is not ready for the exact project root"
                        .to_owned(),
                })?;
            let store = latest.interactive_graph_store().map_err(|error| {
                CodeGraphReadError::Unavailable {
                    detail: error.to_string(),
                }
            })?;
            if request.cancellation.is_cancelled() {
                return Err(CodeGraphReadError::Cancelled);
            }
            VerifiedCodeGraphRead::new(self.scope.clone(), store)
        })
    }
}

pub(crate) fn project_code_graph_projection_read_port(
    schedulers: crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: PathBuf,
    scope: ResolvedScope,
) -> Arc<dyn tracedecay_usecases::graph::CodeGraphProjectionReadPort> {
    Arc::new(ProjectCodeGraphProjectionReadPortV1 {
        schedulers,
        project_root,
        scope,
    })
}

fn install_project_open_source_edit_owners(
    server: &McpServer,
    graph: Arc<crate::tracedecay::TraceDecay>,
    code_graph: Arc<dyn tracedecay_usecases::graph::CodeGraphProjectionReadPort>,
    authorization: ProjectOpenSourceEditAuthorizationV1,
    mutation: Arc<SourceEditMutationGate>,
) -> Result<()> {
    let source_edit_graph = Arc::clone(&graph);
    let source_edit_code_graph = Arc::clone(&code_graph);
    let source_edit_reconciliation_authorization = authorization.clone();
    let source_edit_rollback_authorization = authorization.clone();
    let source_edit_mutation = Arc::clone(&mutation);
    server
        .install_source_edit_executor(Arc::new(move |request| {
            let graph = Arc::clone(&source_edit_graph);
            let code_graph = Arc::clone(&source_edit_code_graph);
            let authorization = authorization.clone();
            let mutation = Arc::clone(&source_edit_mutation);
            Box::pin(async move {
                if !request.edit.dry_run() {
                    mutation.authorize_mutation("mutation")?;
                }
                invoke_project_open_source_edit(graph, code_graph, authorization, request).await
            })
        }))
        .map_err(|_| TraceDecayError::Config {
            message: "project-open source edit authority was already installed".to_owned(),
        })?;
    install_project_open_source_edit_rollback_owner(
        server,
        Arc::clone(&graph),
        source_edit_rollback_authorization,
        Arc::clone(&mutation),
    )?;
    server
        .install_source_edit_reconciliation_executor(Arc::new(move |request| {
            let graph = Arc::clone(&graph);
            let authorization = source_edit_reconciliation_authorization.clone();
            let mutation = Arc::clone(&mutation);
            Box::pin(async move {
                mutation.authorize_mutation("reconciliation")?;
                invoke_project_open_source_edit_reconciliation(graph, authorization, request).await
            })
        }))
        .map_err(|_| TraceDecayError::Config {
            message: "project-open source edit reconciliation authority was already installed"
                .to_owned(),
        })?;
    Ok(())
}

pub(crate) async fn install_project_open_source_edit_preview_owner(
    server: &McpServer,
    graph: Arc<crate::tracedecay::TraceDecay>,
    code_graph: Arc<dyn tracedecay_usecases::graph::CodeGraphProjectionReadPort>,
    project_root: &Path,
    project_id: &str,
) -> Result<Arc<SourceEditMutationGate>> {
    let project_id =
        ProjectId::new(project_id.to_owned()).map_err(|_| TraceDecayError::Config {
            message: "project-open source edit preview requires authoritative project identity"
                .to_owned(),
        })?;
    let scope = resolved_scope_for_project(project_root, &project_id).map_err(|error| {
        TraceDecayError::Config {
            message: format!("project-open source edit preview scope denied: {error}"),
        }
    })?;
    let authorization = ProjectOpenSourceEditAuthorizationV1 {
        project_root: project_root.to_path_buf(),
        scope,
        configuration: Arc::clone(graph.configuration_runtime()),
    };
    let mutation = SourceEditMutationGate::warming();
    install_project_open_source_edit_owners(
        server,
        graph,
        code_graph,
        authorization,
        Arc::clone(&mutation),
    )?;
    Ok(mutation)
}

#[cfg(feature = "test-transport")]
pub(crate) async fn install_project_open_source_edit_owners_for_test(
    server: &McpServer,
) -> Result<()> {
    let graph = server.cg().await;
    let code_graph =
        server
            .code_graph_projection_read_port()
            .ok_or_else(|| TraceDecayError::Config {
                message:
                    "test source-edit owner requires the production code-graph projection port"
                        .to_owned(),
            })?;
    let project_root = graph.project_root().to_path_buf();
    let project_id = graph
        .configuration_runtime()
        .configuration_target()
        .project_id
        .clone();
    let scope = resolved_scope_for_project(&project_root, &project_id).map_err(|error| {
        TraceDecayError::Config {
            message: format!("test project-open resolved scope denied: {error}"),
        }
    })?;
    let authorization = ProjectOpenSourceEditAuthorizationV1 {
        project_root,
        scope,
        configuration: Arc::clone(graph.configuration_runtime()),
    };
    install_project_open_source_edit_owners(
        server,
        graph,
        code_graph,
        authorization,
        SourceEditMutationGate::ready(),
    )
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
    owner: &crate::agents::context_scout_owner::ProjectContextScoutOwnerV1,
    pin: ContextScoutConfigurationPinV1,
    model_config: &tracedecay_agent_hosts::automation::config::AutomationConfig,
) -> Result<()> {
    let admitted_model_config = pin.control().model_path.and_then(|expected| {
        (crate::agents::context_scout_model::context_scout_backend_from_automation_config(
            &model_config,
        ) == expected)
            .then_some(&model_config)
    });
    owner
        .install_configuration(pin, admitted_model_config)
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open Context Scout configuration failed: {error}"),
        })
}

/// State retained after independent owners publish and consumed only after the
/// durable code-index generation has mounted.
pub(crate) struct ProjectOpenDependentOwnerState {
    database: crate::db::Database,
    session_db: Arc<crate::global_db::RegisteredGlobalDb>,
    graph: Arc<crate::tracedecay::TraceDecay>,
    code_graph: Arc<dyn tracedecay_usecases::graph::CodeGraphProjectionReadPort>,
    scope: ResolvedScope,
    access: ProjectSourceAccessSnapshot,
    configuration: tracedecay_usecases::config::PinnedRuntimeConfiguration,
    requester: ActorId,
    mounted_providers: Vec<MountedLspProvider>,
    lsp_session_factory: Arc<DaemonLspSessionFactory>,
    scout_registry: Arc<ProjectContextScoutAddressRegistryV1>,
    scout_configuration: tracedecay_usecases::configuration::ConfigurationCurrentStateV1,
    admitted_root_uri: String,
    indexed_files: Vec<String>,
}

/// Registers code-index-independent owners for one newly inserted project.
pub(super) async fn register_project_open_production_owners(
    invocation: &DaemonInvocationState,
    git_transactions: &DaemonGitIndexTransactionServiceRegistry,
    native_integration: &DaemonNativeIntegrationServiceRegistry,
    project_root: &Path,
    project_id: &str,
    server: &McpServer,
    source_edit_mutation: Arc<SourceEditMutationGate>,
) -> Result<ProjectOpenDependentOwnerState> {
    let owner_registration_started = Instant::now();
    let mut owner_phase_started = owner_registration_started;
    let project_id =
        ProjectId::new(project_id.to_owned()).map_err(|_| TraceDecayError::Config {
            message: "project-open owners require an authoritative project identity".to_owned(),
        })?;
    let graph = server.cg().await;
    let code_graph =
        server
            .code_graph_projection_read_port()
            .ok_or_else(|| TraceDecayError::Config {
                message: "project-open owners require the verified code-graph projection port"
                    .to_owned(),
            })?;
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "graph_snapshot_acquired",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );
    owner_phase_started = Instant::now();
    let database = graph.db().clone();
    let session_db = server
        .project_session_db()
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open owners require the daemon-owned project session database"
                .to_owned(),
        })?;
    let scope = resolved_scope_for_project(project_root, &project_id).map_err(|error| {
        TraceDecayError::Config {
            message: format!("project-open resolved scope denied: {error}"),
        }
    })?;
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "owner_scope_resolved",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );
    owner_phase_started = Instant::now();
    let configuration = graph
        .configuration_runtime()
        .client()
        .current()
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open configuration currentness failed: {error}"),
        })?;
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "configuration_current",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );
    owner_phase_started = Instant::now();
    let scout_configuration = tracedecay_usecases::configuration::ConfigurationCurrentStateV1 {
        revision_id: configuration.revision_id.clone(),
        snapshot: configuration.snapshot.clone(),
    };
    let scout_registry = match invocation
        .context_scout_runtime_registrar()
        .open_and_register(database.clone(), project_id.clone())
        .await
    {
        Ok(registry) => registry,
        Err(DaemonContextScoutRuntimeRegistrationError::AlreadyRegistered) => invocation
            .context_scout_runtime_registrar()
            .get(&project_id)
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: "project-open Context Scout registry disappeared".to_owned(),
            })?,
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!("project-open Context Scout registry failed: {error}"),
            });
        }
    };
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "context_scout_registered",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );
    owner_phase_started = Instant::now();
    let access =
        daemon_owned_project_source_access_at(&scope, project_root, &configuration, now_micros())
            .map_err(|error| TraceDecayError::Config {
            message: format!("project-open source access denied: {error}"),
        })?;
    let grant_expires_at = access.grant_expires_at;
    let requester = access.requester.clone();
    if let Some(repository_root) = crate::worktree::git_worktree_root(project_root) {
        git_transactions
            .install_authority(
                &repository_root,
                access.clone(),
                Arc::clone(&session_db),
                tokio::runtime::Handle::current(),
            )
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!("project-open Git authority registration failed: {error}"),
            })?;
    }
    // Preview executors were published with the read-only core. Open their
    // mutation lane only after the exact Git transaction authority exists.
    // A later failure in this function retires the whole server, and project
    // open marks the lane failed as it does so, so the lane never stays warming.
    source_edit_mutation.mark_ready();
    let configuration_policy_digest =
        super::project_delivery_mount::ensure_project_delivery_settlement(
            invocation,
            project_root,
            Arc::clone(&session_db),
            &scope,
            &access,
        )
        .await?;
    let work_evidence_retrieval = server
        .work_evidence_retrieval()?
        .with_federated_authority(invocation.work_federated_query_authority());
    invocation
        .configuration_runtime_registrar()
        .register(
            project_root.to_path_buf(),
            Arc::clone(graph.configuration_runtime()),
            scope.clone(),
            server
                .profile_identity()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "project-open configuration requires exact profile authority"
                        .to_owned(),
                })?
                .profile_id()
                .clone(),
            requester.clone(),
            grant_expires_at,
            None,
            configuration_policy_digest.clone(),
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open configuration runtime registration failed: {error}"),
        })?;
    let retained_observed_at = now_micros();
    let retained_grant =
        project_open_retained_grant(&access, retained_observed_at).map_err(|error| {
            TraceDecayError::Config {
                message: format!("project-open retained grant is invalid: {error}"),
            }
        })?;
    let retained_ports = server.retained_surface_ports(
        project_root,
        scope.project_id.clone(),
        access.configuration_digest.clone(),
    );
    invocation
        .retained_runtime_registrar()
        .register(
            project_root.to_path_buf(),
            scope.clone(),
            requester.clone(),
            retained_grant,
            retained_ports,
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open retained runtime registration failed: {error}"),
        })?;
    // Mount the native-integration authority under the same pinned policy
    // digest the configuration runtime just registered, so the coordinator's
    // stale/denied predicates and the handler's minted grants agree on one
    // policy identity. Non-Git projects advertise no native mutation
    // authority; the handler keeps answering the typed unavailable result.
    let native_owner = if let Some(repository_root) =
        crate::worktree::git_worktree_root(project_root)
    {
        let native_owner = native_integration
            .ensure(
                Arc::clone(&session_db),
                repository_root,
                scope.project_id.clone(),
                scope.repository_id.clone(),
                configuration_policy_digest.clone(),
                now_micros(),
            )
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "project-open native integration authority registration failed: {error}"
                ),
            })?;
        invocation
            .service
            .install_worktree_cleanup_recovery_fences(&native_owner)
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!("project-open worktree cleanup recovery fencing failed: {error}"),
            })?;
        Some(native_owner)
    } else {
        None
    };
    let work_grant = project_open_work_grant(&access, now_micros()).map_err(|error| {
        TraceDecayError::Config {
            message: format!("project-open Work grant is invalid: {error}"),
        }
    })?;
    let work_authority = tracedecay_domain::WorkAuthority::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        requester.clone(),
        work_grant.digest.clone(),
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("project-open Work authority is invalid: {error}"),
    })?;
    let work_topology_policy =
        crate::config::topology::resolved_work_topology_policy(&configuration.snapshot)
            .map_err(|error| TraceDecayError::Config {
                message: format!("project-open work topology policy is unavailable: {error}"),
            })?
            .clone();
    let work_proposal_routing =
        crate::daemon::service::invocation::DaemonWorkProposalRoutingAuthorityV1::mount(
            scope.clone(),
            configuration.revision_id.clone(),
            &configuration.snapshot,
            &access.configuration_digest,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open Work proposal routing is unavailable: {error}"),
        })?;
    // Project-open has no authenticated GitHub response or persisted source
    // record. It mounts policy and delivery only; the review refresh owner is
    // the sole producer of canonical provider observations and anchors.
    if crate::tracedecay::git_remote_url(project_root)
        .as_deref()
        .and_then(github_repository_from_remote)
        .is_some()
    {
        let stack_coordinator = invocation.github_stack_coordinator();
        stack_coordinator
            .register_scope(
                &scope,
                work_topology_policy.review_topology.github_stacked_prs,
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "project-open GitHub stack coordinator registration failed: {error:?}"
                ),
            })?;
        if let Some(native_owner) = native_owner.as_ref() {
            let stack_runtime = native_owner
                .mount_github_stack_runtime(
                    Arc::clone(&session_db),
                    scope.clone(),
                    access.clone(),
                    Arc::clone(&stack_coordinator),
                )
                .map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "project-open GitHub stack delivery runtime registration failed: {error:?}"
                    ),
                })?;
            crate::daemon::native_integration::register_github_stack_hook_runtime(
                &scope,
                &stack_runtime,
            );
        }
    }
    invocation
        .work_runtime_registrar()
        .register(
            project_root.to_path_buf(),
            Arc::clone(&session_db),
            work_authority.clone(),
            requester.clone(),
            work_grant.clone(),
            configuration_policy_digest.clone(),
            access.configuration_digest.clone(),
            work_topology_policy,
            work_proposal_routing,
            work_evidence_retrieval,
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open Workflow authority registration failed: {error}"),
        })?;
    if !invocation
        .work_runtime_registrar()
        .authority_matches(
            project_root,
            &work_authority,
            &requester,
            &work_grant,
            &configuration_policy_digest,
            &access.configuration_digest,
        )
        .await
    {
        return Err(TraceDecayError::Config {
            message:
                "project-open Workflow authority registration did not match the admitted project"
                    .to_owned(),
        });
    }
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "configuration_runtime_registered",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );
    owner_phase_started = Instant::now();
    match invocation
        .feedback_runtime_registrar()
        .open_and_register(
            database.clone(),
            project_root.to_path_buf(),
            scope.clone(),
            access.clone(),
            Arc::clone(graph.configuration_runtime()),
        )
        .await
    {
        Ok(_) | Err(DaemonFeedbackRuntimeRegistrationError::AlreadyRegistered) => {}
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!("project-open feedback runtime registration failed: {error:?}"),
            });
        }
    }
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "feedback_runtime_registered",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );
    owner_phase_started = Instant::now();

    let admitted_root_uri =
        admitted_root_uri_for_project(project_root).map_err(|error| TraceDecayError::Config {
            message: format!("project-open admitted root URI denied: {error}"),
        })?;
    let primitive_runtime =
        open_production_primitive_runtime(ProductionPrimitiveOpenRequestV1::new(
            graph.clone(),
            Arc::clone(&code_graph),
            Arc::clone(&session_db),
            Arc::new(invocation.code_index_schedulers.clone()),
            Arc::new(invocation.code_index_schedulers.clone()),
            access.clone(),
            admitted_root_uri.clone(),
            daemon_operation_event_authority(),
        ))
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open primitive runtime open failed: {error}"),
        })?;
    match invocation
        .primitive_runtime_registrar()
        .register(project_root.to_path_buf(), primitive_runtime)
        .await
    {
        Ok(_) | Err(DaemonPrimitiveRuntimeRegistrationError::AlreadyRegistered) => {}
        Err(DaemonPrimitiveRuntimeRegistrationError::RegistryClosed) => {
            return Err(TraceDecayError::Config {
                message: "project-open primitive runtime registration failed: the daemon project runtime registry is closed".to_owned(),
            });
        }
    }
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "primitive_runtime_registered",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );
    owner_phase_started = Instant::now();

    let indexed_files =
        graph
            .get_all_file_paths()
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!("project-open LSP language discovery failed: {error}"),
            })?;
    let diagnostic_broker = server.diagnostics_lsp();
    let admitted_providers = diagnostic_broker
        .lock()
        .await
        .admitted_providers_for_files(&indexed_files);
    let mounted_providers = admitted_providers
        .iter()
        .filter_map(AdmittedLspProvider::mounted)
        .collect::<Vec<_>>();
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "lsp_languages_discovered",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );
    owner_phase_started = Instant::now();

    // Feedback runtime registration installed a typed unavailable cycle. The
    // LSP gateway can therefore publish now and switches to the exact
    // production cycle after code-index mount.
    let lsp_scope_grant = project_open_lsp_scope_grant(&access, now_micros()).map_err(|error| {
        TraceDecayError::Config {
            message: format!("project-open LSP workspace grant is invalid: {error}"),
        }
    })?;
    let lsp_session_factory = register_production_lsp_owner(
        invocation,
        project_root,
        lsp_scope_grant,
        Arc::clone(&session_db),
        database.clone(),
        diagnostic_broker,
        &admitted_providers,
        admitted_root_uri.clone(),
    )
    .await?;
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "lsp_owner_registered",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );

    // Hook V2 envelopes that missed their synchronous budget are durable in
    // the per-host transport spool. Replay is project-scoped, not Git-scoped:
    // non-Git and unborn projects must drain their admitted envelopes too.
    let delivery_settlements = invocation
        .service
        .delivery_settlement_authority(Some(project_root))
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("hook delivery settlement authority is invalid: {error}"),
        })?
        .ok_or_else(|| TraceDecayError::Config {
            message: "hook delivery settlement authority is unavailable".to_owned(),
        })?;
    crate::daemon::hook_v2_replay::register_hook_v2_replay_consumer(
        Arc::clone(&graph),
        delivery_settlements,
    );

    // Semantic restore can decode a large durable generation. Keep that
    // capability-specific warm-up behind every independent production owner
    // so diagnostics, tests, feedback, and LSP reads remain available while
    // semantic retrieval truthfully reports generation_unavailable.
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "independent_owners_registered",
    );

    Ok(ProjectOpenDependentOwnerState {
        database,
        session_db,
        graph,
        code_graph,
        scope,
        access,
        configuration,
        requester,
        mounted_providers,
        lsp_session_factory,
        scout_registry,
        scout_configuration,
        admitted_root_uri,
        indexed_files,
    })
}

/// Registers owners whose exact authority depends on a mounted code index.
pub(super) async fn register_project_open_dependent_owners(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: ProjectOpenDependentOwnerState,
) -> Result<()> {
    let state = Arc::new(state);
    // Subscribe before the first registration attempt so a generation that
    // publishes between the attempt and a later subscription cannot be missed.
    let generation_publications = invocation
        .code_index_schedulers
        .subscribe_generation_publications();
    match register_production_feedback_cycle(invocation, project_root, &state).await? {
        ProductionFeedbackCycleRegistrationV1::Registered {
            runtime,
            feedback_scope,
            lsp_input,
        } => {
            tracing::info!(
                event = "project_open_owner_phase",
                project = %project_root.display(),
                phase = "feedback_cycle_registered",
            );
            advisory_upgrade::spawn_advisory_owner_upgrade(
                invocation.clone(),
                project_root.to_path_buf(),
                Arc::clone(&state),
                runtime,
                feedback_scope,
                lsp_input,
            );
            tracing::info!(
                event = "project_open_owner_phase",
                project = %project_root.display(),
                phase = "advisory_owner_scheduled",
            );
        }
        ProductionFeedbackCycleRegistrationV1::SkippedWithoutGitScope { reason } => {
            // Skipping feedback (and with it the advisory cycle) is a valid
            // open outcome, but a silent one made the disabled journey look
            // registered; record which precondition disabled it.
            tracing::info!(
                event = "project_open_owner_phase",
                project = %project_root.display(),
                phase = "feedback_cycle_skipped",
                reason = reason,
            );
        }
        ProductionFeedbackCycleRegistrationV1::SkippedUnindexed => {
            // A cold open reaches this point before the first code-index
            // generation seals, so the provider identity is a transient gap,
            // not a session-permanent one. Defer registration until the
            // scheduler publishes a generation for this project.
            tracing::info!(
                event = "project_open_owner_phase",
                project = %project_root.display(),
                phase = "feedback_cycle_deferred",
                reason = "project-open provider code-index identity",
            );
            advisory_upgrade::spawn_deferred_feedback_cycle_upgrade(
                invocation.clone(),
                project_root.to_path_buf(),
                Arc::clone(&state),
                generation_publications,
            );
        }
    }

    let semantic_activation_started = Instant::now();
    register_semantic_activation_owner(
        invocation,
        project_root,
        &state.graph,
        Arc::clone(&state.session_db),
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
    Ok(())
}

async fn register_semantic_activation_owner(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    graph: &Arc<crate::tracedecay::TraceDecay>,
    session_db: Arc<crate::global_db::RegisteredGlobalDb>,
    scope: ResolvedScope,
    configuration: &tracedecay_usecases::configuration::ConfigurationCurrentStateV1,
) -> Result<()> {
    let configuration_pin =
        tracedecay_usecases::semantic_runtime::SemanticConfigurationPinV1::from_current(
            configuration,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("semantic retrieval configuration pin failed: {error}"),
        })?;
    let configuration_store =
        tracedecay_usecases::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1::open(
            graph.configuration_runtime().registered_database(),
            scope.clone(),
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("semantic retrieval configuration store unavailable: {error}"),
        })?;
    let accepted_profiles = Arc::new(
        tracedecay_usecases::semantic_runtime::RegisteredSemanticAcceptedProfileAuthorityV1::new(
            graph.configuration_runtime().registered_database(),
        ),
    );
    let current_state = configuration_store
        .current_state_if_present()
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("semantic retrieval current state unavailable: {error}"),
        })?;
    let observer = invocation.query_activation_registrar(project_root, Arc::clone(&session_db));
    if let Some(current_state) = current_state {
        if current_state.audit().is_empty() {
            let cursor_keys = Arc::new(
                session_db
                    .load_session_cursor_key_provider_result()
                    .await
                    .map_err(|error| TraceDecayError::Config {
                        message: format!("query cursor key authority unavailable: {error}"),
                    })?,
            );
            invocation
                .restore_initial_query_authority_for_project(
                    project_root,
                    scope.clone(),
                    current_state,
                    cursor_keys,
                )
                .map_err(|error| TraceDecayError::Config {
                    message: format!("evaluated query initial authority restore failed: {error}"),
                })?;
        } else {
            let committed = configuration_store
                .current_committed_state()
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!("semantic retrieval committed state unavailable: {error}"),
                })?
                .ok_or_else(|| TraceDecayError::Config {
                    message: "semantic retrieval state has no current committed transition"
                        .to_owned(),
                })?;
            observer
                .activation_committed(committed)
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!("semantic retrieval activation restore failed: {error}"),
                })?;
        }
        if let Err(error) = invocation
            .mount_query_authority_for_project(project_root, &scope)
            .await
        {
            tracing::debug!(
                event = "query_authority_mount",
                outcome = "unavailable",
                project_id = %scope.project_id,
                reason = %error,
                "query search authority unavailable; non-search project surfaces remain mounted"
            );
            if matches!(
                error,
                crate::daemon::code_index_scheduler::query_runtime::QueryRuntimeMountErrorV1::GenerationUnavailable
            ) {
                query_authority_upgrade::spawn_deferred_query_authority_mount(
                    invocation.clone(),
                    project_root.to_path_buf(),
                    scope.clone(),
                    query_authority_upgrade::DeferredQueryAuthorityMountV1::Configured,
                );
            }
        }
        if let Err(error) = crate::daemon::code_index_scheduler::semantic_query_runtime::
            mount_current_semantic_query_authority_on_project_open(
                &invocation.code_index_schedulers,
                project_root,
                &scope,
                &configuration_store,
                &configuration_pin,
            )
            .await
        {
            tracing::debug!(
                event = "semantic_query_authority_mount",
                outcome = "unavailable",
                project_id = %scope.project_id,
                reason = %error,
                "semantic query authority unavailable; project surfaces remain mounted"
            );
        }
    } else {
        let core_query_available = match session_db.load_session_cursor_key_provider_result().await
        {
            Ok(cursor_keys) => {
                if let Err(error) = invocation
                    .mount_core_query_authority_for_project(project_root, &scope, &cursor_keys)
                    .await
                {
                    tracing::debug!(
                        event = "query_authority_mount",
                        outcome = "unavailable",
                        project_id = %scope.project_id,
                        reason = %error,
                        "core query fallback is unavailable; project admission continues"
                    );
                    if matches!(
                        error,
                        crate::daemon::code_index_scheduler::query_runtime::QueryRuntimeMountErrorV1::GenerationUnavailable
                    ) {
                        query_authority_upgrade::spawn_deferred_query_authority_mount(
                            invocation.clone(),
                            project_root.to_path_buf(),
                            scope.clone(),
                            query_authority_upgrade::DeferredQueryAuthorityMountV1::CoreFallback {
                                session_db: Arc::clone(&session_db),
                            },
                        );
                    }
                    false
                } else {
                    true
                }
            }
            Err(error) => {
                tracing::debug!(
                    event = "query_authority_mount",
                    outcome = "unavailable",
                    project_id = %scope.project_id,
                    reason = %error,
                    "durable query cursor key is unavailable; project admission continues"
                );
                false
            }
        };
        tracing::debug!(
            event = "semantic_activation_registration",
            outcome = "unavailable",
            project_id = %scope.project_id,
            core_query_available,
            "no genuinely evaluated optional-stage profile is published"
        );
    }
    let Some(inspector) =
        tracedecay_usecases::semantic_runtime::project_semantic_production_runtime(project_root)
    else {
        return Ok(());
    };
    let lifecycle_events = inspector.verified_ready_events();
    let owner = Arc::new(
        tracedecay_usecases::semantic_runtime::ProductionSemanticActivationCoordinatorV1::new(
            configuration_store,
            graph.configuration_runtime().configuration_store(),
            inspector,
            observer,
        ),
    );
    graph
        .configuration_runtime()
        .install_semantic_runtime(Arc::clone(&owner))?;
    let reconciler = Arc::new(
        crate::daemon::semantic_activation_reconciler::DaemonSemanticActivationReconcilerV1::spawn(
            owner,
            lifecycle_events,
        ),
    );
    invocation
        .configuration_runtime_registrar()
        .install_semantic_activation_reconciler(project_root, reconciler)
        .await?;
    let operation = Arc::new(
        tracedecay_usecases::semantic_runtime::ProductionSemanticConfigurationOperationV1::new(
            Arc::clone(graph.configuration_runtime()),
            accepted_profiles,
        ),
    );
    invocation
        .configuration_runtime_registrar()
        .install_semantic_operation(project_root, operation)
        .await
}

/// Typed outcome of one production feedback-cycle registration attempt.
///
/// The skip variants are valid open outcomes, not failures, but they differ in
/// whether a later daemon event can lift them: an unindexed project becomes
/// registrable once the first complete code-index generation publishes, while
/// a project without git scope stays skipped for the whole session.
enum ProductionFeedbackCycleRegistrationV1 {
    Registered {
        runtime: Arc<FeedbackCycleRuntime>,
        feedback_scope: FeedbackScopeV1,
        lsp_input: FeedbackCycleLspInput,
    },
    /// The provider code-index identity has no complete generation yet.
    SkippedUnindexed,
    /// The project has no feedback-eligible branch or head commit.
    SkippedWithoutGitScope { reason: &'static str },
}

async fn register_production_feedback_cycle(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    state: &ProjectOpenDependentOwnerState,
) -> Result<ProductionFeedbackCycleRegistrationV1> {
    let database = state.database.clone();
    let project_runtime_db = Arc::clone(&state.session_db);
    let graph = Arc::clone(&state.graph);
    let code_graph = Arc::clone(&state.code_graph);
    let scope = state.scope.clone();
    let configuration = state.configuration.clone();
    let requester = state.requester.clone();
    let mounted_providers = state.mounted_providers.clone();
    let configuration_digest = &configuration.snapshot.effective_behavior_digest;
    let policy_digest = canonical_sha256(&(
        "tracedecay.project-open.policy.v1",
        configuration_digest,
        POLICY_REVISION_V1,
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("project-open feedback policy digest failed: {error}"),
    })?;
    let runtime_state = Arc::new(ProductionFeedbackRuntimeStateV1::new(
        graph.clone(),
        configuration_digest.clone(),
        policy_digest,
    ));
    let authorization: Arc<dyn ProductionFeedbackCycleAuthorizationPort> =
        Arc::new(ProjectOpenFeedbackCycleAuthorizationV1 {
            project_root: project_root.to_path_buf(),
            scope: scope.clone(),
            configuration: Arc::clone(graph.configuration_runtime()),
        });
    let parts = match resolve_production_feedback_cycle_parts(ProductionFeedbackCycleOpenV1 {
        project_root: project_root.to_path_buf(),
        project_runtime_db,
        scope,
        access_configuration: tracedecay_usecases::configuration::ConfigurationCurrentStateV1 {
            revision_id: configuration.revision_id,
            snapshot: configuration.snapshot,
        },
        requester,
        authorization,
        graph: graph.clone(),
        code_graph,
        runtime_state: Arc::clone(&runtime_state) as _,
        document_identity: Arc::new(invocation.code_index_schedulers.clone()),
        code_index_identity: Arc::new(invocation.code_index_schedulers.clone()),
        test_attribution: Arc::new(invocation.code_index_schedulers.clone()),
        mounted_providers,
    })
    .await
    {
        Ok(parts) => parts,
        Err(ApplicationContractError::Inconsistent {
            field: "project-open provider code-index identity",
        }) => {
            return Ok(ProductionFeedbackCycleRegistrationV1::SkippedUnindexed);
        }
        Err(ApplicationContractError::Inconsistent {
            field: field @ ("project-open feedback branch" | "project-open feedback head commit"),
        }) => {
            return Ok(
                ProductionFeedbackCycleRegistrationV1::SkippedWithoutGitScope { reason: field },
            );
        }
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!("project-open feedback cycle parts failed: {error}"),
            });
        }
    };
    let feedback_scope = parts.feedback_scope.clone();
    let feedback_lsp_input = Arc::clone(&parts.lsp_input);
    invocation
        .feedback_runtime_registrar()
        .open_cycle_and_register(
            project_root.to_path_buf(),
            database,
            parts.runtime_state,
            parts.policy_context,
            parts.evidence_horizon,
            parts.evaluated_at,
            parts.provider_candidates,
            graph,
            parts.affected_tests,
            parts.operation,
            parts.graph_operation,
            parts.tests_operation,
            parts.lsp_input,
            parts.proximity,
        )
        .await
        .map(
            |runtime| ProductionFeedbackCycleRegistrationV1::Registered {
                runtime,
                feedback_scope,
                lsp_input: feedback_lsp_input,
            },
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open feedback cycle registration failed: {error}"),
        })
}

async fn register_production_lsp_owner(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    scope_grant: tracedecay_application::CapabilityGrantSnapshot,
    registered_database: Arc<crate::global_db::RegisteredGlobalDb>,
    database: crate::db::Database,
    diagnostic_broker: Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>,
    admitted_providers: &[AdmittedLspProvider],
    root_uri: String,
) -> Result<Arc<DaemonLspSessionFactory>> {
    let (languages, gateway_capabilities) = production_lsp_registration(admitted_providers);
    invocation
        .lsp_owner_registrar()
        .build_and_register(
            project_root.to_path_buf(),
            scope_grant,
            registered_database,
            database,
            Arc::new(invocation.code_index_schedulers.clone()),
            tokio::runtime::Handle::current(),
            diagnostic_broker,
            &languages,
            root_uri,
            LspRefreshTimeouts::from_diagnostics_quiet_window(LSP_DIAGNOSTICS_QUIET),
            LSP_DIAGNOSTICS_QUIET,
            gateway_capabilities,
        )
        .await
}

async fn register_production_advisory_owner(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    database: crate::db::Database,
    project_runtime_db: Arc<crate::global_db::RegisteredGlobalDb>,
    graph: Arc<crate::tracedecay::TraceDecay>,
    code_graph: Arc<dyn tracedecay_usecases::graph::CodeGraphProjectionReadPort>,
    resolved_scope: ResolvedScope,
    source_access: ProjectSourceAccessSnapshot,
    feedback_scope: FeedbackScopeV1,
    feedback_cycle: Arc<FeedbackCycleRuntime>,
    feedback_lsp_input: FeedbackCycleLspInput,
    lsp_session_factory: Arc<DaemonLspSessionFactory>,
    scout_registry: Arc<ProjectContextScoutAddressRegistryV1>,
    scout_configuration: tracedecay_usecases::configuration::ConfigurationCurrentStateV1,
    root_uri: String,
    indexed_files: Vec<String>,
    setup_cancellation: CancellationToken,
) -> Result<crate::daemon::project_open_advisory::PreparedAdvisoryRuntimeV1> {
    if setup_cancellation.is_cancelled() {
        return Err(TraceDecayError::Config {
            message: "advisory runtime setup was cancelled".to_owned(),
        });
    }
    let scout_configuration = ContextScoutConfigurationPinV1::from_current(&scout_configuration)
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open Context Scout configuration is unavailable".to_owned(),
        })?;
    let scout_owner =
        graph
            .context_scout_owner()
            .cloned()
            .ok_or_else(|| TraceDecayError::Config {
                message: "project-open Context Scout owner is unavailable".to_owned(),
            })?;
    let configuration = graph
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
    if !scout_configuration.matches_current(&current_configuration) {
        return Err(TraceDecayError::Config {
            message: "project-open Context Scout configuration changed before model installation"
                .to_owned(),
        });
    }
    let model_config = tracedecay_agent_hosts::automation::config::from_configuration_snapshot(
        &configuration.snapshot,
    )?;
    install_project_open_context_scout_configuration(
        scout_owner.as_ref(),
        scout_configuration.clone(),
        &model_config,
    )
    .await?;
    if setup_cancellation.is_cancelled() {
        return Err(TraceDecayError::Config {
            message: "advisory runtime setup was cancelled".to_owned(),
        });
    }
    let remote = resolve_production_github_provider_config(
        invocation,
        project_root,
        database.clone(),
        Arc::clone(&project_runtime_db),
        resolved_scope.clone(),
        &source_access,
        feedback_scope.clone(),
        setup_cancellation.clone(),
    )
    .await;
    if setup_cancellation.is_cancelled() {
        return Err(TraceDecayError::Config {
            message: "advisory runtime setup was cancelled".to_owned(),
        });
    }
    let (github, github_provider, github_source_access, ci_config) =
        remote.map_or((None, None, None, None), |remote| {
            let provider = remote
                .github
                .as_ref()
                .map(|github| github.identity.provider.clone());
            (
                remote.github,
                provider,
                Some(remote.github_source_access),
                remote.ci,
            )
        });
    let github_pull_request_id = github
        .as_ref()
        .map(|github| github.target.pull_request_id.clone());
    let ci_discovery_config = ci_config.clone();
    let ci_retained = Arc::new(
        ProjectCiRetainedObservationStoreV1::new(database.clone(), feedback_scope.clone())
            .ok_or_else(|| TraceDecayError::Config {
                message: "project-open CI retained store failed: invalid feedback scope"
                    .to_string(),
            })?,
    ) as _;
    let ci_code_anchors = Arc::new(
        ProjectCiCodeAnchorStoreV1::new_with_code_index_identity(
            graph.clone(),
            feedback_scope.clone(),
            Arc::clone(&code_graph),
            Arc::new(invocation.code_index_schedulers.clone()),
        )
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open CI anchor store failed: invalid feedback scope".to_string(),
        })?,
    ) as _;
    let hook_notices = AdvisoryHookNoticeQueueV1::new(feedback_scope.clone());
    let hook_v2 = hook_notices.sink();
    let legacy_hook = unavailable_advisory_hook_sink();
    let (hook_project_id, hook_worktree_id) = crate::hooks::hook_scope_locators(&resolved_scope);
    let feedback_runtime = feedback_cycle.feedback_runtime();
    let feedback_scope_for_work = feedback_scope.clone();
    let input = AdvisoryRuntimeOpenV1 {
        database: database.clone(),
        project_root: project_root.to_path_buf(),
        resolved_scope,
        feedback_scope: feedback_scope.clone(),
        github,
        feedback_cycle,
    };
    let scout_claim_graph = Arc::clone(&graph);
    let lifecycle_session_db = Arc::clone(&project_runtime_db);
    let external_store = crate::daemon::external_acquisition::open_external_source_store(
        &project_runtime_db,
        github_provider.as_ref(),
    )?;
    let production = AdvisoryProductionOpenV1 {
        project_runtime_db: Arc::clone(&project_runtime_db),
        graph,
        code_graph,
        code_index_identity: Arc::new(invocation.code_index_schedulers.clone()),
        project_root: project_root.to_path_buf(),
        feedback_scope,
        ci_config,
        github_source_access,
        ci_retained,
        ci_code_anchors,
        hook_v2,
        legacy_hook,
    };
    let registration = match invocation
        .advisory_runtime_registrar()
        .build_production(
            project_root.to_path_buf(),
            input,
            production,
            lsp_session_factory,
        )
        .await
    {
        Ok(registration) => registration,
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!("project-open advisory runtime construction failed: {error}"),
            });
        }
    };
    let external_acquisition_request =
        github_pull_request_id
            .clone()
            .map(|pull_request_id| GitHubReviewReadRequestV1 {
                operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
                scope: feedback_scope_for_work.clone(),
                pull_request_id,
            });
    let external_acquisition_context = external_acquisition_request.as_ref().and_then(|_| {
        github_discovery_authorization_context(&source_access, &feedback_scope_for_work)
    });
    let external_acquisition =
        crate::daemon::external_acquisition::mount_production_github_external_acquisition(
            invocation,
            project_root,
            registration.as_ref(),
            Arc::clone(&project_runtime_db),
            external_acquisition_context,
            external_acquisition_request,
            github_provider,
            external_store,
        )
        .await?;
    let advisory_cycle = Arc::new(ProjectOpenAdvisoryFeedbackCycleV1 {
        registration: Arc::clone(&registration),
        lsp_input: Arc::clone(&feedback_lsp_input),
        root_uri: root_uri.clone(),
        feedback_scope: feedback_scope_for_work.clone(),
        github_pull_request_id: github_pull_request_id.clone(),
        ci_discovery_config: ci_discovery_config.clone(),
    });
    let published_registration = Arc::clone(&registration);
    let published_cycle = Arc::clone(&advisory_cycle);
    let published_project_id = feedback_scope_for_work.project_id.clone();
    let published_worktree_id = feedback_scope_for_work.worktree_id.clone();
    let work_feedback_scope = feedback_scope_for_work.clone();
    let work_root = project_root.to_path_buf();
    let work = move |request: HookOrchestrationRequestV1, work_cancellation: CancellationToken| {
        let registration = Arc::clone(&registration);
        let feedback_lsp_input = Arc::clone(&feedback_lsp_input);
        let graph = Arc::clone(&scout_claim_graph);
        let scout_owner = Arc::clone(&scout_owner);
        let scout_registry = Arc::clone(&scout_registry);
        let feedback_runtime = Arc::clone(&feedback_runtime);
        let github_pull_request_id = github_pull_request_id.clone();
        let ci_discovery_config = ci_discovery_config.clone();
        let feedback_scope = work_feedback_scope.clone();
        let project_root = work_root.clone();
        let root_uri = root_uri.clone();
        let indexed_files = indexed_files.clone();
        let external_acquisition = external_acquisition.clone();
        async move {
            run_production_hook_cycle(
                request,
                registration,
                feedback_lsp_input,
                graph,
                scout_owner,
                scout_registry,
                feedback_runtime,
                github_pull_request_id,
                ci_discovery_config,
                feedback_scope,
                project_root,
                root_uri,
                indexed_files,
                external_acquisition,
                work_cancellation,
            )
            .await;
        }
    };
    let orchestrator =
        BoundedHookOrchestratorV1::new(1, work).ok_or_else(|| TraceDecayError::Config {
            message: "project-open hook orchestration capacity is invalid".to_owned(),
        })?;
    if setup_cancellation.is_cancelled() {
        return Err(TraceDecayError::Config {
            message: "advisory runtime setup was cancelled".to_owned(),
        });
    }
    let lifecycle_registration =
        crate::daemon::context_scout_lifecycle::register_context_scout_lifecycle_authority(
            hook_project_id,
            hook_worktree_id,
            published_project_id.clone(),
            published_worktree_id,
            &lifecycle_session_db,
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
    let mut discovery_registration = AdvisoryDiscoveryRegistrationLeaseV1 {
        hook_project_id,
        hook_worktree_id,
        lifecycle_session_db,
        lifecycle_registered_here,
        hook_notices: Arc::clone(&hook_notices),
        hook_notices_registered: false,
    };
    if !register_advisory_hook_notice_queue(hook_project_id, hook_worktree_id, &hook_notices) {
        return Err(TraceDecayError::Config {
            message: "advisory Hook notice queue registration failed".to_owned(),
        });
    }
    discovery_registration.hook_notices_registered = true;
    if setup_cancellation.is_cancelled() {
        return Err(TraceDecayError::Config {
            message: "advisory runtime setup was cancelled".to_owned(),
        });
    }
    let published: Arc<dyn std::any::Any + Send + Sync> = Arc::new(PublishedAdvisoryRuntimeV1 {
        _registration: published_registration,
        _discovery: discovery_registration,
    });
    let publication = match invocation
        .advisory_runtime_registrar()
        .publish(
            project_root,
            published,
            DaemonAdvisoryCycleInvocationOwner::new(published_project_id, published_cycle),
            advisory_cycle,
            &setup_cancellation,
        )
        .await
    {
        Ok(publication) => publication,
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!("advisory runtime publication failed: {error}"),
            });
        }
    };
    let orchestrator: Arc<dyn HookOrchestrationPortV1> = orchestrator;
    Ok(
        crate::daemon::project_open_advisory::PreparedAdvisoryRuntimeV1::new(
            orchestrator,
            move || publication.commit(),
        ),
    )
}

#[allow(clippy::too_many_arguments)]
async fn run_production_hook_cycle(
    request: HookOrchestrationRequestV1,
    registration: Arc<AdvisoryProductionStartupRegistrationV1>,
    feedback_lsp_input: FeedbackCycleLspInput,
    graph: Arc<crate::tracedecay::TraceDecay>,
    scout_owner: Arc<crate::agents::context_scout_owner::ProjectContextScoutOwnerV1>,
    scout_registry: Arc<ProjectContextScoutAddressRegistryV1>,
    feedback_runtime: Arc<tracedecay_usecases::feedback::concrete::FeedbackRuntime>,
    github_pull_request_id: Option<GitHubPullRequestIdV1>,
    ci_discovery_config: Option<ProductionCiProviderConfigV1>,
    feedback_scope: FeedbackScopeV1,
    project_root: std::path::PathBuf,
    root_uri: String,
    indexed_files: Vec<String>,
    external_acquisition: Option<
        Arc<dyn crate::daemon::external_acquisition::DaemonExternalAcquisitionRuntimeV1>,
    >,
    work_cancellation: CancellationToken,
) {
    if work_cancellation.is_cancelled() {
        return;
    }
    let Some(document_uri) = hook_feedback_document_uri_or_observe(
        &project_root,
        &indexed_files,
        &request,
        &registration.host_delivery.source_observations,
    ) else {
        return;
    };
    let diagnostic_trigger = match request.trigger {
        HookOrchestrationTriggerV1::SavedEdit => DiagnosticTrigger::DocumentSave,
        HookOrchestrationTriggerV1::Stop | HookOrchestrationTriggerV1::Explicit => {
            DiagnosticTrigger::ExplicitDocumentDiagnostics
        }
    };
    let feedback_request = FeedbackCycleRequest {
        root_uri,
        document_uri,
        trigger: diagnostic_trigger,
    };
    let mut invocation = match feedback_lsp_input(feedback_request.clone()).await {
        Ok(invocation) => invocation,
        Err(_) => {
            observe_accepted_feedback_cycle_terminal(
                &registration.host_delivery.source_observations,
                &feedback_scope.project_id,
                &feedback_request,
                FeedbackOutcomeV1::Unavailable,
            );
            return;
        }
    };
    if request.trigger == HookOrchestrationTriggerV1::Stop {
        invocation.request.input.request.trigger = FeedbackTriggerV1::AgentStopGate;
        let Ok(validated) = FeedbackCycleInvocation::new(invocation.context, invocation.request)
        else {
            observe_hook_feedback_cycle_terminal(
                &registration.host_delivery.source_observations,
                &request,
                FeedbackOutcomeV1::Partial,
            );
            return;
        };
        invocation = validated;
    }
    if work_cancellation.is_cancelled() {
        return;
    }
    let observed_at = invocation.request.input.observed_at;
    let expires_at = UtcMicros(observed_at.0.saturating_add(5 * 60 * 1_000_000));
    let operation_authority = daemon_operation_event_authority();
    let Ok(operation) = operation_authority
        .begin(
            &invocation.context,
            OperationKind::FeedbackDiagnostics,
            observed_at,
        )
        .await
    else {
        observe_hook_feedback_cycle_terminal(
            &registration.host_delivery.source_observations,
            &request,
            FeedbackOutcomeV1::Unavailable,
        );
        return;
    };
    let ci = match ci_discovery_config.as_ref() {
        Some(config) => {
            discover_production_ci_failure_request_v1(&invocation.context, config, &feedback_scope)
                .await
        }
        None => tracedecay_usecases::advisory::ProductionCiFailureDiscoveryOutcomeV1::NotConfigured,
    };
    if work_cancellation.is_cancelled() {
        return;
    }
    let advisory = AdvisoryCycleRequest {
        feedback: invocation.request,
        github: github_pull_request_id.map(|pull_request_id| GitHubReviewReadRequestV1 {
            operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
            scope: feedback_scope.clone(),
            pull_request_id,
        }),
        ci,
        proximity: Some(ProximityEvaluationRequestV1 {
            scope: feedback_scope.clone(),
            observed_at,
        }),
        validity: tracedecay_application::AdvisoryFindingValidityWindowV1 {
            valid_at: observed_at,
            expires_at,
        },
    };
    let acquisition_outcome = crate::daemon::external_acquisition::handle_github_hook_event(
        external_acquisition.as_ref(),
        &invocation.context,
        advisory.github.as_ref(),
        request.hook.envelope(),
        observed_at,
    )
    .await;
    acquisition_outcome.observe(&feedback_scope.project_id, external_acquisition.is_some());
    let feedback_configuration_digest =
        advisory.feedback.input.request.configuration_digest.clone();
    let host = host_kind_for_hook(request.hook.envelope().producer);
    let rollback = HookFeedbackRollbackSwitchV1 {
        configuration_revision: request.hook_configuration_revision,
        route: HookFeedbackDeliveryRouteV1::HookV2,
    };
    if registration
        .run_once(
            &invocation.context,
            AdvisoryCycleControl {
                operation,
                deadline: MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            },
            advisory,
            host,
            rollback,
        )
        .await
        .is_err()
    {
        observe_hook_feedback_cycle_terminal(
            &registration.host_delivery.source_observations,
            &request,
            FeedbackOutcomeV1::Unavailable,
        );
        return;
    }
    let Ok(pinned_configuration) = graph.configuration_runtime().client().current().await else {
        return;
    };
    let current_configuration = tracedecay_usecases::configuration::ConfigurationCurrentStateV1 {
        revision_id: pinned_configuration.revision_id.clone(),
        snapshot: pinned_configuration.snapshot.clone(),
    };
    let Some(scout_configuration) =
        ContextScoutConfigurationPinV1::from_current(&current_configuration)
    else {
        return;
    };
    if scout_configuration.configuration_digest() != &feedback_configuration_digest {
        return;
    }
    let Ok(model_config) = tracedecay_agent_hosts::automation::config::from_configuration_snapshot(
        &pinned_configuration.snapshot,
    ) else {
        return;
    };
    if install_project_open_context_scout_configuration(
        scout_owner.as_ref(),
        scout_configuration.clone(),
        &model_config,
    )
    .await
    .is_err()
    {
        return;
    }
    let Some(lifecycle) = request.lifecycle else {
        return;
    };
    let Some(pin) = ContextScoutAuthorityPinV1::new(
        &invocation.context,
        feedback_scope,
        scout_configuration,
        observed_at,
    ) else {
        return;
    };
    let assembler = ContextScoutCanonicalInputAssemblerV1::new(
        scout_registry.as_ref(),
        feedback_runtime.as_ref(),
    );
    let Some(canonical) = assembler
        .bind_and_assemble(
            &request.hook,
            &pin,
            lifecycle.clone(),
            &invocation.context,
            observed_at,
        )
        .await
    else {
        return;
    };
    let trigger = match request.trigger {
        HookOrchestrationTriggerV1::SavedEdit => ContextScoutTriggerV1::SavedEdit,
        HookOrchestrationTriggerV1::Stop => ContextScoutTriggerV1::StopBoundary,
        HookOrchestrationTriggerV1::Explicit => ContextScoutTriggerV1::ExplicitRequest,
    };
    let recent = scout_owner.recent_exact(canonical.address, 32).await.ok();
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
        return;
    };
    let outcome = scout_owner
        .prepare_configured(
            &selection,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            work_cancellation.clone(),
        )
        .await;
    if matches!(
        outcome,
        Ok(crate::agents::context_scout_v2::ContextScoutRuntimeOutcomeV1::Enqueued { .. })
    ) {
        let _ = graph
            .mount_current_context_scout_claim_authority(
                scout_registry,
                &request.hook,
                pin,
                invocation.context,
                lifecycle,
                canonical.address,
                selection.input_watermark,
                observed_at,
            )
            .await;
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

fn production_ci_provider_configuration(
    target: GitHubCiRepositoryTargetV1,
    credential: GitHubReadOnlyCredentialV1,
    http: GitHubHttpReadConfigV1,
    source_access: Arc<dyn CiSourceAccessAuthorityV1>,
) -> Option<ProductionCiProviderConfigV1> {
    Some(ProductionCiProviderConfigV1 {
        provider: ProviderId::new("provider.github-actions").ok()?,
        parser: CiFailureParserIdentityV1 {
            parser_id: "parser.github-actions.v1".to_owned(),
            parser_version: "1".to_owned(),
        },
        target,
        credential,
        http,
        source_access,
    })
}

const fn host_kind_for_hook(host: HookHostV1) -> HostKindV1 {
    match host {
        HookHostV1::ClaudeCode => HostKindV1::ClaudeCode,
        HookHostV1::Codex => HostKindV1::Codex,
        HookHostV1::CursorDesktop => HostKindV1::CursorDesktop,
        HookHostV1::CursorCloud => HostKindV1::CursorCloud,
        HookHostV1::Hermes => HostKindV1::Hermes,
        HookHostV1::Kiro => HostKindV1::Kiro,
        HookHostV1::KimiCode => HostKindV1::KimiCode,
        HookHostV1::OpenCode => HostKindV1::OpenCode,
        HookHostV1::Cline => HostKindV1::Cline,
        HookHostV1::RooCode => HostKindV1::RooCode,
        HookHostV1::Kilo => HostKindV1::Kilo,
    }
}

struct ProductionGitHubProviderConfigV1 {
    github: Option<GitHubReviewRuntimeOwnerConfigV1>,
    github_source_access: Arc<dyn GitHubSourceAccessAuthorityV1>,
    ci: Option<ProductionCiProviderConfigV1>,
}

async fn resolve_production_github_provider_config(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    database: crate::db::Database,
    project_runtime_db: Arc<crate::global_db::RegisteredGlobalDb>,
    resolved_scope: ResolvedScope,
    project_source_access: &ProjectSourceAccessSnapshot,
    feedback_scope: FeedbackScopeV1,
    setup_cancellation: CancellationToken,
) -> Option<ProductionGitHubProviderConfigV1> {
    let (owner, repository) =
        github_repository_from_remote(&crate::tracedecay::git_remote_url(project_root)?)?;
    let profile_id = &project_runtime_db.binding().shard_id.profile_id;
    let credential = match invocation.mount_github_read_only_credential_authority_for_project(
        profile_id,
        &owner,
        &repository,
    ) {
        ProfileGitHubReadOnlyCredentialMountOutcomeV1::Public => {
            GitHubReadOnlyCredentialV1::anonymous()
        }
        ProfileGitHubReadOnlyCredentialMountOutcomeV1::NotConfigured
        | ProfileGitHubReadOnlyCredentialMountOutcomeV1::Rejected => return None,
        ProfileGitHubReadOnlyCredentialMountOutcomeV1::Mounted => {
            match resolve_registered_github_read_only_credential_v1(&owner, &repository) {
                tracedecay_usecases::advisory::github_runtime::RegisteredGitHubReadOnlyCredentialV1::Verified(
                    credential,
                ) => credential,
                tracedecay_usecases::advisory::github_runtime::RegisteredGitHubReadOnlyCredentialV1::Missing
                | tracedecay_usecases::advisory::github_runtime::RegisteredGitHubReadOnlyCredentialV1::Rejected => {
                    return None;
                }
            }
        }
    };
    let configuration = OwnedGlobalDbConfigurationControlStore::from_registered_project_runtime_db(
        project_runtime_db,
    );
    let configured_source_access = Arc::new(ConfiguredGitHubSourceAccessAuthorityV1::new(
        configuration,
        resolved_scope.clone(),
        &owner,
        &repository,
    )?);
    let source_access: Arc<dyn GitHubSourceAccessAuthorityV1> = configured_source_access.clone();
    let ci_source_access: Arc<dyn CiSourceAccessAuthorityV1> = configured_source_access;
    let ci = (credential.permits(GitHubReadPermissionV1::Actions)
        && credential.permits(GitHubReadPermissionV1::Checks))
    .then(|| {
        production_ci_provider_configuration(
            GitHubCiRepositoryTargetV1 {
                owner: owner.clone(),
                repository: repository.clone(),
            },
            credential.clone(),
            GitHubHttpReadConfigV1::default(),
            ci_source_access,
        )
    })
    .flatten();
    let review_discovery_authority =
        github_discovery_authorization_context(project_source_access, &feedback_scope)
            .zip(github_discovery_source_access_request(&feedback_scope));
    let head_commit_id = feedback_scope.head_commit_id.clone();
    let http = GitHubHttpReadConfigV1::default();
    let discovery_http = http.clone();
    let discovery_credential = credential.clone();
    let discovery = match review_discovery_authority.as_ref() {
        Some((authorization_context, discovery_request)) => {
            discover_github_pull_request_after_authorization(
                || source_access.authorize(authorization_context, discovery_request),
                setup_cancellation,
                move |control| {
                    discover_exact_commit_pull_request_v1(
                        &owner,
                        &repository,
                        &head_commit_id,
                        &discovery_http,
                        &discovery_credential,
                        &control,
                    )
                },
            )
            .await
        }
        None => None,
    };
    let github = match (discovery, review_discovery_authority.as_ref()) {
        (
            Some(GitHubExactCommitDiscoveryOutcomeV1::Found(pull)),
            Some((authorization_context, _)),
        ) => {
            let target = pull.target.clone();
            let exact_request = GitHubReviewReadRequestV1 {
                operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
                scope: feedback_scope.clone(),
                pull_request_id: target.pull_request_id.clone(),
            };
            if source_access
                .authorize(authorization_context, &exact_request)
                .await
                != GitHubProviderLifecycleV1::Ready
            {
                None
            } else {
                resolve_production_github_identity(project_root, &feedback_scope, &target, pull)
                    .map(|identity| GitHubReviewRuntimeOwnerConfigV1 {
                        database,
                        resolved_scope,
                        feedback_scope,
                        target,
                        credential,
                        http,
                        identity,
                    })
            }
        }
        _ => None,
    };
    Some(ProductionGitHubProviderConfigV1 {
        github,
        github_source_access: source_access,
        ci,
    })
}

/// Wall-clock budget a single discovery owner may hold. Feedback never waits on
/// a remote read, so the owner is bounded and its blocking clone is cancelled
/// and joined before the budget is reported as unavailable.
const GITHUB_DISCOVERY_BUDGET: Duration = Duration::from_secs(15);

async fn discover_github_pull_request_after_authorization<A, AF, F>(
    authorize: A,
    cancellation: CancellationToken,
    discover: F,
) -> Option<GitHubExactCommitDiscoveryOutcomeV1>
where
    A: FnOnce() -> AF,
    AF: std::future::Future<Output = GitHubProviderLifecycleV1>,
    F: FnOnce(GitHubDiscoveryControlV1) -> GitHubExactCommitDiscoveryOutcomeV1 + Send + 'static,
{
    if cancellation.is_cancelled()
        || authorize().await != GitHubProviderLifecycleV1::Ready
        || cancellation.is_cancelled()
    {
        return None;
    }
    let control = GitHubDiscoveryControlV1::bounded(Instant::now() + GITHUB_DISCOVERY_BUDGET);
    let blocking_control = control.clone();
    let mut task = tokio::task::spawn_blocking(move || discover(blocking_control));
    tokio::select! {
        result = &mut task => result.ok(),
        // Cancel first, then join: the blocking owner observes the flag at
        // its next page boundary and returns, so no ureq call outlives the
        // budget or its retired setup and no partial page escapes as a
        // terminal result.
        () = cancellation.cancelled() => {
            control.cancel();
            let _ = task.await;
            None
        }
        () = tokio::time::sleep(GITHUB_DISCOVERY_BUDGET) => {
            control.cancel();
            let _ = task.await;
            None
        }
    }
}

fn github_discovery_source_access_request(
    feedback_scope: &FeedbackScopeV1,
) -> Option<GitHubReviewReadRequestV1> {
    let request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
        scope: feedback_scope.clone(),
        // Authorization binds repository access to this exact commit before
        // discovery; this authorization-only identifier is never sent.
        pull_request_id: GitHubPullRequestIdV1::new(format!(
            "discovery.commit.{}",
            feedback_scope.head_commit_id.as_str()
        ))
        .ok()?,
    };
    request.validate().is_ok().then_some(request)
}

fn github_discovery_authorization_context(
    access: &ProjectSourceAccessSnapshot,
    feedback_scope: &FeedbackScopeV1,
) -> Option<tracedecay_application::RequestContext> {
    let observed_at = now_micros();
    if feedback_scope.validate().is_err()
        || access.scope.project_id != feedback_scope.project_id
        || access.scope.repository_id != feedback_scope.repository_id
        || access.scope.worktree_id != feedback_scope.worktree_id
        || access.scope.reference.as_ref().map(RefId::as_str)
            != Some(feedback_scope.branch_ref.as_str())
        || observed_at >= access.grant_expires_at
    {
        return None;
    }
    let capability = CapabilityId::new(GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1.to_owned()).ok()?;
    if !access.effective_capabilities.contains(&capability) {
        return None;
    }
    let use_case = tracedecay_tool_catalog::UseCaseId::new(
        tracedecay_application::feedback::GITHUB_REVIEW_INGEST_USE_CASE_ID_V1.to_owned(),
    )
    .ok()?;
    let grant_digest = canonical_sha256(&(
        "tracedecay.project-open.github-discovery-grant.v1",
        &access.scope,
        &access.requester,
        &access.configuration_digest,
        &feedback_scope.head_commit_id,
        &capability,
        &use_case,
        observed_at,
        access.grant_expires_at,
    ))
    .ok()?;
    let grant = tracedecay_application::CapabilityGrantSnapshot::new(
        tracedecay_application::CapabilityGrantId::new(format!(
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
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        tracedecay_application::DisclosureClass::Evidence,
    )
    .ok()?;
    let request_id =
        mint_global_request_id(GlobalRequestSurface::ProjectOpenGithubDiscovery).ok()?;
    tracedecay_application::RequestContext::new(
        access.requester.clone(),
        access.scope.clone(),
        grant,
        request_id.clone(),
        tracedecay_application::Deadline::new(access.grant_expires_at).ok()?,
        tracedecay_application::CancellationContext::active(format!(
            "cancel.project-open.github-discovery.{}",
            request_id.as_str()
        ))
        .ok()?,
    )
    .ok()
}

fn github_repository_from_remote(remote: &str) -> Option<(String, String)> {
    let (owner, repository) = if let Ok(url) = url::Url::parse(remote) {
        if (url.scheme() != "https" && url.scheme() != "ssh")
            || !url.host_str()?.eq_ignore_ascii_case("github.com")
            || url.password().is_some()
            || (url.scheme() == "https" && !url.username().is_empty())
            || (url.scheme() == "ssh" && url.username() != "git")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        let segments = url.path_segments()?.collect::<Vec<_>>();
        if segments.len() != 2 {
            return None;
        }
        (segments[0].to_owned(), segments[1].to_owned())
    } else {
        let remote = remote.strip_prefix("git@github.com:")?;
        let mut segments = remote.split('/');
        let owner = segments.next()?;
        let repository = segments.next()?;
        if segments.next().is_some() {
            return None;
        }
        (owner.to_owned(), repository.to_owned())
    };
    let repository = repository
        .strip_suffix(".git")
        .unwrap_or(&repository)
        .to_owned();
    let target = GitHubRepositoryTargetV1 {
        owner,
        repository,
        pull_request_number: 1,
        pull_request_id: GitHubPullRequestIdV1::new("1").ok()?,
    };
    target
        .validate()
        .then_some((target.owner, target.repository))
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
        // Keep the advisory target bound to the admitted feedback head.
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

pub(super) fn daemon_owned_project_source_access_at(
    scope: &ResolvedScope,
    project_root: &Path,
    configuration: &tracedecay_usecases::config::PinnedRuntimeConfiguration,
    observed_at: UtcMicros,
) -> std::result::Result<ProjectSourceAccessSnapshot, ApplicationContractError> {
    let locator = locator_digest_for_project(project_root)?;
    let locator = LocatorDigest::new(locator.as_str().to_owned()).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open locator digest",
        }
    })?;
    let binding = ScopeSourceBinding::new(
        SourceBindingId::new(DAEMON_BINDING.to_owned()).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "project-open source binding id",
            }
        })?,
        SourceKindV1::Cursor,
        locator,
        AuthorityRef::Project(scope.project_id.clone()),
    )
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open source binding",
    })?;
    if configuration.target.project_id != scope.project_id {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open configuration project",
        });
    }
    configuration
        .snapshot
        .validate()
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open configuration snapshot",
        })?;
    let requester = ActorId::new(DAEMON_REQUESTER.to_owned()).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open requester",
        }
    })?;
    let authority = AuthorityRef::Project(scope.project_id.clone());
    let bindings_key = SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open source bindings key",
        }
    })?;
    let Some(ConfigurationValueV1::SourceBindings(bindings)) =
        configuration.snapshot.effective_values.get(&bindings_key)
    else {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open source bindings",
        });
    };
    let configured_bindings = bindings
        .iter()
        .filter(|candidate| {
            candidate.source_kind == binding.source_kind && candidate.authority == authority
        })
        .collect::<Vec<_>>();
    if configured_bindings.len() != 1
        || configured_bindings.first().is_none_or(|candidate| {
            candidate.source_locator_digest != binding.source_locator_digest
        })
    {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open source binding authority",
        });
    }
    let binding = configured_bindings
        .first()
        .map(|configured| (**configured).clone())
        .ok_or(ApplicationContractError::Inconsistent {
            field: "project-open source binding authority",
        })?;
    let access_rules_key = SettingKey::new(ACCESS_RULES_SETTING_KEY).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open access rules key",
        }
    })?;
    let Some(ConfigurationValueV1::AccessRules(access_rules)) = configuration
        .snapshot
        .effective_values
        .get(&access_rules_key)
    else {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open access rules",
        });
    };
    let granted_capabilities = production_owner_capabilities()?
        .into_iter()
        .map(|capability| DomainCapabilityId::new(capability.as_str().to_owned()))
        .collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open granted capabilities",
        })?;
    let resolution = resolve_restrictive_capabilities(
        granted_capabilities,
        access_rules,
        &CapabilityResolutionContextV1 {
            actor: requester.clone(),
            operation: None,
            source_kind: binding.source_kind,
            authority,
            evaluated_at: observed_at,
        },
    )
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open capability resolution",
    })?;
    let effective_capabilities = resolution
        .effective
        .into_iter()
        .map(|capability| CapabilityId::new(capability.as_str().to_owned()))
        .collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open effective capabilities",
        })?;
    Ok(ProjectSourceAccessSnapshot {
        scope: scope.clone(),
        requester,
        binding,
        configuration_revision: configuration.revision_id.clone(),
        configuration_digest: configuration.snapshot.effective_behavior_digest.clone(),
        configuration_provenance_digest: configuration
            .snapshot
            .resolution_provenance_digest
            .clone(),
        effective_capabilities,
        grant_expires_at: UtcMicros(
            observed_at
                .0
                .saturating_add(i64::try_from(GRANT_HORIZON.as_micros()).unwrap_or(i64::MAX)),
        ),
    })
}

fn project_open_work_grant(
    access: &ProjectSourceAccessSnapshot,
    observed_at: UtcMicros,
) -> std::result::Result<tracedecay_application::CapabilityGrantSnapshot, ApplicationContractError>
{
    let capabilities = tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1
        .iter()
        .chain(tracedecay_application::WORKFLOW_APPLICATION_OPERATION_IDS.iter())
        .chain(tracedecay_application::HANDOFF_APPLICATION_OPERATION_IDS_V1.iter())
        .map(|(_, capability, _)| CapabilityId::new(*capability))
        .collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open Work capabilities",
        })?;
    if observed_at >= access.grant_expires_at
        || !capabilities
            .iter()
            .all(|capability| access.effective_capabilities.contains(capability))
    {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open Work capability grant",
        });
    }
    let use_cases = tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1
        .iter()
        .chain(tracedecay_application::WORKFLOW_APPLICATION_OPERATION_IDS.iter())
        .chain(tracedecay_application::HANDOFF_APPLICATION_OPERATION_IDS_V1.iter())
        .map(|(_, _, use_case)| tracedecay_tool_catalog::UseCaseId::new(*use_case))
        .collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open Work use cases",
        })?;
    let grant_digest = canonical_sha256(&(
        "tracedecay.project-open.work-grant.v1",
        &access.scope,
        &access.requester,
        &access.configuration_digest,
        &access.configuration_provenance_digest,
        &capabilities,
        &use_cases,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open Work grant digest",
    })?;
    tracedecay_application::CapabilityGrantSnapshot::new(
        tracedecay_application::CapabilityGrantId::new(format!(
            "grant.tracedecay-daemon.project-open.work.{}",
            grant_digest.as_str().trim_start_matches("sha256:")
        ))?,
        POLICY_REVISION_V1,
        grant_digest,
        access.requester.clone(),
        observed_at,
        access.grant_expires_at,
        access.scope.clone(),
        capabilities,
        use_cases,
        tracedecay_application::DisclosureClass::Sensitive,
    )
}

fn project_open_retained_grant(
    access: &ProjectSourceAccessSnapshot,
    observed_at: UtcMicros,
) -> std::result::Result<tracedecay_application::CapabilityGrantSnapshot, ApplicationContractError>
{
    let operations = tracedecay_application::RetainedSurfaceOperation::CALLABLE
        .into_iter()
        .map(tracedecay_application::retained_surface_application_operation)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let capabilities = operations
        .iter()
        .map(|operation| operation.capability_id().clone())
        .collect::<BTreeSet<_>>();
    if observed_at >= access.grant_expires_at
        || !capabilities
            .iter()
            .all(|capability| access.effective_capabilities.contains(capability))
    {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open retained capability grant",
        });
    }
    let use_cases = operations
        .iter()
        .map(|operation| operation.use_case_id().clone())
        .collect::<BTreeSet<_>>();
    let grant_digest = canonical_sha256(&(
        "tracedecay.project-open.retained-grant.v1",
        &access.scope,
        &access.requester,
        &access.configuration_digest,
        &access.configuration_provenance_digest,
        &capabilities,
        &use_cases,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open retained grant digest",
    })?;
    tracedecay_application::CapabilityGrantSnapshot::new(
        tracedecay_application::CapabilityGrantId::new(format!(
            "grant.tracedecay-daemon.project-open.retained.{}",
            grant_digest.as_str().trim_start_matches("sha256:")
        ))?,
        POLICY_REVISION_V1,
        grant_digest,
        access.requester.clone(),
        observed_at,
        access.grant_expires_at,
        access.scope.clone(),
        capabilities,
        use_cases,
        tracedecay_application::DisclosureClass::Sensitive,
    )
}

pub(super) fn project_open_lsp_scope_grant(
    access: &ProjectSourceAccessSnapshot,
    observed_at: UtcMicros,
) -> std::result::Result<tracedecay_application::CapabilityGrantSnapshot, ApplicationContractError>
{
    let capability = CapabilityId::new(LSP_WORKSPACE_CAPABILITY_ID_V1).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open LSP workspace capability",
        }
    })?;
    if observed_at >= access.grant_expires_at
        || !access.effective_capabilities.contains(&capability)
    {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open LSP workspace capability grant",
        });
    }
    let use_case = UseCaseId::new(LSP_WORKSPACE_USE_CASE_ID_V1).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open LSP workspace use case",
        }
    })?;
    let capabilities = BTreeSet::from([capability]);
    let use_cases = BTreeSet::from([use_case]);
    let grant_digest = canonical_sha256(&(
        "tracedecay.project-open.lsp-workspace-grant.v1",
        &access.scope,
        &access.requester,
        &access.configuration_digest,
        &access.configuration_provenance_digest,
        &capabilities,
        &use_cases,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open LSP workspace grant digest",
    })?;
    tracedecay_application::CapabilityGrantSnapshot::new(
        tracedecay_application::CapabilityGrantId::new(format!(
            "grant.tracedecay-daemon.project-open.lsp-workspace.{}",
            grant_digest.as_str().trim_start_matches("sha256:")
        ))?,
        POLICY_REVISION_V1,
        grant_digest,
        access.requester.clone(),
        observed_at,
        access.grant_expires_at,
        access.scope.clone(),
        capabilities,
        use_cases,
        tracedecay_application::DisclosureClass::Sensitive,
    )
}

fn production_owner_capabilities()
-> std::result::Result<BTreeSet<CapabilityId>, ApplicationContractError> {
    let mut capabilities = BTreeSet::new();
    for capability in [
        "capability.diagnostics.current",
        FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1,
        FEEDBACK_GET_CAPABILITY_ID_V1,
        FEEDBACK_EXPAND_CAPABILITY_ID_V1,
        FEEDBACK_LIST_CAPABILITY_ID_V1,
        "capability.application.feedback.impact",
        "capability.application.feedback.affected-tests",
        "capability.application.feedback.test-results",
        "capability.application.code-query.exact-occurrence",
        "capability.application.code-query.phrase-search",
        "capability.application.code-query.callees",
        "capability.application.code-query.facets",
        "capability.application.code-query.timeline",
        "capability.application.code-query.declaration",
        "capability.application.code-query.definition",
        "capability.application.code-query.type-definition",
        "capability.application.code-query.references",
        "capability.application.symbol-search",
        "capability.application.primitive.code-signature-search",
        "capability.application.primitive.code-implementations",
        "capability.application.primitive.code-type-hierarchy",
        "capability.application.primitive.code-callers",
        GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
        CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
        PROXIMITY_CAPABILITY_ID_V1,
        "capability.application.primitive.session-lookup",
        "capability.application.primitive.qualified-name",
        "capability.application.primitive.call-chain",
        "capability.application.primitive.file-dependents",
        "capability.application.primitive.source-lines",
        "capability.application.primitive.source-body",
        "capability.application.primitive.source-outline",
        "capability.application.primitive.module-api",
        "capability.application.primitive.file-metadata",
        "capability.application.primitive.health-read",
        "capability.application.primitive.health-delta",
        "capability.application.primitive.storage-status",
        "capability.application.primitive.diagnostics-read",
        "capability.application.git.status",
        "capability.application.git.diff",
        "capability.application.git.history",
        "capability.application.git.blame",
        "capability.application.git.hunks",
        LSP_WORKSPACE_CAPABILITY_ID_V1,
        "capability.application.source-edit.ast-grep-rewrite",
        "capability.application.source-edit.insert-at",
        "capability.application.source-edit.insert-at-symbol",
        "capability.application.source-edit.move-symbol",
        "capability.application.source-edit.multi-str-replace",
        "capability.application.source-edit.rename-symbol",
        "capability.application.source-edit.replace-symbol",
        "capability.application.source-edit.reconcile",
        "capability.application.source-edit.rollback",
        "capability.application.source-edit.str-replace",
        "capability.git.stage-hunks",
        "capability.git.unstage-hunks",
        "capability.git.commit-index",
    ] {
        capabilities.insert(CapabilityId::new(capability.to_owned()).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "project-open capability",
            }
        })?);
    }
    for (_, capability, _) in tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1
        .into_iter()
        .chain(tracedecay_application::WORKFLOW_APPLICATION_OPERATION_IDS)
        .chain(tracedecay_application::HANDOFF_APPLICATION_OPERATION_IDS_V1)
    {
        capabilities.insert(CapabilityId::new(capability).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "project-open Work capability",
            }
        })?);
    }
    for operation in tracedecay_application::RetainedSurfaceOperation::CALLABLE {
        let operation = tracedecay_application::retained_surface_application_operation(operation)?;
        capabilities.insert(operation.capability_id().clone());
    }
    Ok(capabilities)
}

pub(crate) fn resolved_scope_for_project(
    project_root: &Path,
    project_id: &ProjectId,
) -> std::result::Result<ResolvedScope, ApplicationContractError> {
    let repository_id = crate::daemon::code_index_scheduler::identity::repository_id_for(
        project_root,
    )
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open repository id",
    })?;
    let worktree_id = crate::daemon::code_index_scheduler::identity::worktree_id_for(project_root)
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open worktree id",
        })?;
    let reference = crate::branch::current_branch(project_root)
        .and_then(|branch| RefId::new(format!("refs/heads/{branch}")).ok());
    ResolvedScope::new(project_id.clone(), repository_id, worktree_id, reference).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open resolved scope",
        }
    })
}

#[cfg(test)]
mod scout_journey_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::RepositoryId;

    #[test]
    fn production_project_owner_grants_every_cataloged_git_read() {
        let capabilities = production_owner_capabilities().expect("production capabilities");

        for capability in [
            "capability.application.git.status",
            "capability.application.git.diff",
            "capability.application.git.history",
            "capability.application.git.blame",
            "capability.application.git.hunks",
        ] {
            let capability = CapabilityId::new(capability).expect("Git read capability");
            assert!(
                capabilities.contains(&capability),
                "{} must be granted to the daemon-owned project route",
                capability.as_str()
            );
        }
    }

    #[test]
    fn production_project_owner_grants_every_work_operation() {
        let capabilities = production_owner_capabilities().expect("production capabilities");

        for (_, capability, _) in tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1
            .into_iter()
            .chain(tracedecay_application::WORKFLOW_APPLICATION_OPERATION_IDS)
            .chain(tracedecay_application::HANDOFF_APPLICATION_OPERATION_IDS_V1)
        {
            let capability = CapabilityId::new(capability).expect("Work attempt capability");
            assert!(
                capabilities.contains(&capability),
                "{} must be granted to the daemon-owned Work route",
                capability.as_str()
            );
        }
    }

    #[test]
    fn production_project_owner_grants_every_retained_operation() {
        let capabilities = production_owner_capabilities().expect("production capabilities");

        for operation in tracedecay_application::RetainedSurfaceOperation::CALLABLE {
            let operation =
                tracedecay_application::retained_surface_application_operation(operation)
                    .expect("retained application operation");
            assert!(
                capabilities.contains(operation.capability_id()),
                "{} must be granted to the daemon-owned retained route",
                operation.capability_id().as_str()
            );
        }
    }

    #[derive(Default)]
    struct RecordingHookCycleObservations(std::sync::Mutex<Vec<FeedbackSourceEventV1>>);

    impl FeedbackObservationEmitterV1 for RecordingHookCycleObservations {
        fn observe_source_event(
            &self,
            _input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
            source_event: FeedbackSourceEventV1,
        ) {
            self.0.lock().expect("observations").push(source_event);
        }

        fn observe_source_event_for_subject(
            &self,
            _subject_digest: tracedecay_domain::ManifestDigest,
            _observed_at: UtcMicros,
            source_event: FeedbackSourceEventV1,
        ) {
            self.0.lock().expect("observations").push(source_event);
        }
    }

    fn saved_edit_hook_request(file_id: [u8; 16]) -> HookOrchestrationRequestV1 {
        let capabilities = vec![tracedecay_hooks::HookCapabilityV1 {
            family: tracedecay_hooks::HookEventFamily::SavedEdit,
            support: tracedecay_hooks::stock_event_support(
                tracedecay_hooks::HookHostV1::Codex,
                tracedecay_hooks::HookEventFamily::SavedEdit,
            ),
        }];
        let binding = tracedecay_hooks::HookScopeBindingV1 {
            host: tracedecay_hooks::HookHostV1::Codex,
            project_id: [3; 16],
            repository_id: [4; 16],
            worktree_id: [5; 16],
            worktree_epoch: 1,
            binding_token: [6; 32],
            capabilities,
        };
        HookOrchestrationRequestV1::from_envelope(
            crate::mcp::tools::handlers::hook_runtime::daemon_mint_hook_v2_envelope(
                &tracedecay_hooks::HookEventEnvelopeV2 {
                    schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
                    event_id: [1; 16],
                    producer: tracedecay_hooks::HookHostV1::Codex,
                    protected_session_id: [2; 32],
                    project_id: binding.project_id,
                    repository_id: binding.repository_id,
                    worktree_id: binding.worktree_id,
                    worktree_epoch: binding.worktree_epoch,
                    binding_token: binding.binding_token,
                    ordering: tracedecay_hooks::HookOrderingV1::Unknown,
                    observed_at: UtcMicros(10),
                    event: tracedecay_hooks::HookEventV2::SavedEdit {
                        file_id,
                        changed_range_count: 1,
                    },
                },
            ),
            &binding,
            None,
            7,
            false,
        )
        .expect("admitted saved edit")
    }

    fn admitted(language: &str, analyzer_available: bool) -> AdmittedLspProvider {
        AdmittedLspProvider {
            language: language.to_owned(),
            command: format!("{language}-language-server"),
            analyzer_available,
        }
    }

    fn configured_model_pin() -> ContextScoutConfigurationPinV1 {
        let setting_key = tracedecay_domain::configuration::SettingKey::new(
            tracedecay_domain::configuration::CONTEXT_SCOUT_SETTINGS_SETTING_KEY,
        )
        .expect("Scout setting key");
        let revision =
            tracedecay_domain::configuration::ConfigurationRevisionId::new("revision.scout.model")
                .expect("configuration revision");
        let settings = tracedecay_domain::configuration::ContextScoutSettingsV1 {
            schema_version:
                tracedecay_domain::configuration::ContextScoutSettingsV1::SCHEMA_VERSION,
            state: tracedecay_domain::configuration::ContextScoutConfigurationStateV1::Active,
            mode: tracedecay_domain::configuration::ContextScoutConfigurationModeV1::ConfiguredModel,
            limits:
                tracedecay_domain::configuration::ContextScoutConfigurationLimitsV1::bounded_defaults(),
            model_path: Some(
                tracedecay_domain::configuration::ContextScoutConfiguredModelPathV1::CodexAppServer,
            ),
        };
        let snapshot = tracedecay_domain::configuration::ConfigurationSnapshotV1::new(
            BTreeMap::from([(
                setting_key.clone(),
                ConfigurationValueV1::ContextScoutSettings(settings),
            )]),
            BTreeMap::from([(
                setting_key,
                vec![tracedecay_domain::configuration::ConfigurationCandidateV1 {
                    layer: tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
                        project_id: ProjectId::new("project.scout.model").expect("project id"),
                    },
                    revision_id: revision.clone(),
                    disposition: tracedecay_domain::configuration::CandidateDispositionV1::Winning,
                    safe_reason: None,
                }],
            )]),
        )
        .expect("configuration snapshot");
        ContextScoutConfigurationPinV1::from_current(
            &tracedecay_usecases::configuration::ConfigurationCurrentStateV1 {
                revision_id: revision,
                snapshot,
            },
        )
        .expect("configured-model pin")
    }

    async fn test_scout_owner(
        temporary: &tempfile::TempDir,
        name: &str,
    ) -> Arc<crate::agents::context_scout_owner::ProjectContextScoutOwnerV1> {
        crate::daemon::store_runtime::register_registered_schema_installer();
        let database_path = temporary.path().join(format!("{name}.db"));
        let database_authority = crate::db::DatabaseAuthority::acquire_test(&database_path, name)
            .expect("database authority");
        let database = crate::db::Database::publish_test_runtime(
            &database_path,
            &database_authority,
            crate::db::TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("project database")
        .0;
        crate::agents::context_scout_owner::ProjectContextScoutOwnerV1::startup(
            database,
            [8; 16],
            UtcMicros(1),
            None,
        )
        .await
        .expect("Scout owner")
    }

    fn configured_model_input(
        configuration_revision: [u8; 32],
    ) -> crate::agents::context_scout_v2::ContextScoutSelectionInputV1 {
        crate::agents::context_scout_v2::ContextScoutSelectionInputV1 {
            address: crate::agents::context_scout_v2::ContextScoutAddressV1 {
                profile_id: [1; 16],
                provider_id: [2; 16],
                protected_session_id: [3; 32],
                thread_id: [4; 16],
                turn_id: [5; 16],
                agent_id: [6; 16],
                logical_message_id: [7; 16],
                project_id: [8; 16],
            },
            input_watermark: [9; 32],
            configuration_revision,
            envelope_id: [10; 16],
            now: UtcMicros(10),
            delivery_window:
                crate::agents::context_scout_v2::ContextScoutDeliveryWindowV1::Immediate,
            delivered_dedupe_keys: BTreeSet::new(),
            candidates: vec![crate::agents::context_scout_v2::ContextScoutCandidateV1 {
                dedupe_key: [11; 32],
                category: crate::agents::context_scout_v2::ContextScoutCategoryV1::Retrieval,
                relevance_score: 10,
                suggestion_text: "Use the admitted evidence.".to_owned(),
                evidence: super::scout_journey_tests::configured_model_evidence(10),
                expires_at: UtcMicros(100),
            }],
        }
    }

    #[test]
    fn production_registration_mounts_dynamic_workspace_diagnostics_without_analyzer() {
        let admitted = [admitted("rust", false)];
        let (languages, gateway) = production_lsp_registration(&admitted);

        assert_eq!(languages, vec!["rust"]);
        assert!(gateway.supports_document_diagnostics);
        assert!(gateway.supports_managed_diagnostics);
        assert!(gateway.supports_workspace_diagnostics);
        assert_eq!(gateway.semantic, graph_semantic_capabilities());
    }

    #[test]
    fn empty_project_and_unmappable_edit_persist_typed_feedback_terminals() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let request = saved_edit_hook_request([99; 16]);
        let observations = Arc::new(RecordingHookCycleObservations::default());
        let observation_port =
            Arc::clone(&observations) as Arc<dyn FeedbackObservationEmitterV1 + Send + Sync>;

        assert!(
            hook_feedback_document_uri_or_observe(
                temporary.path(),
                &[],
                &request,
                &observation_port,
            )
            .is_none()
        );
        assert!(matches!(
            observations.0.lock().expect("observations").as_slice(),
            [FeedbackSourceEventV1::Delivery {
                outcome: FeedbackOutcomeV1::Unavailable,
                ..
            }]
        ));

        observations.0.lock().expect("observations").clear();
        assert!(
            hook_feedback_document_uri_or_observe(
                temporary.path(),
                &["src/lib.rs".to_owned()],
                &request,
                &observation_port,
            )
            .is_none()
        );
        assert!(matches!(
            observations.0.lock().expect("observations").as_slice(),
            [FeedbackSourceEventV1::Delivery {
                outcome: FeedbackOutcomeV1::Partial,
                ..
            }]
        ));
    }

    #[test]
    fn canonical_saved_edit_identity_resolves_its_exact_indexed_file() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let absolute_path = temporary.path().join("src/lib.rs");
        let request = saved_edit_hook_request(hash16(absolute_path.to_string_lossy().as_bytes()));

        assert_eq!(
            hook_feedback_document_uri(
                temporary.path(),
                &["src/lib.rs".to_owned(), "src/other.rs".to_owned()],
                &request,
            ),
            url::Url::from_file_path(absolute_path).ok().map(Into::into),
        );
    }

    #[test]
    fn registration_preserves_every_admitted_project_language() {
        for language in ["python", "typescript"] {
            let admitted = [
                admitted("rust", false),
                admitted(language, true),
                admitted("go", false),
            ];
            let (selected, gateway) = production_lsp_registration(&admitted);

            assert_eq!(selected, vec!["rust", language, "go"]);
            assert_eq!(gateway.semantic, graph_semantic_capabilities());
        }
    }

    #[test]
    fn unavailable_legacy_hook_accepts_each_notice_borrow_without_retaining_it() {
        fn notice(suffix: &str) -> AdvisoryHookLookupNoticeV1 {
            AdvisoryHookLookupNoticeV1 {
                scope: FeedbackScopeV1 {
                    project_id: ProjectId::new("project.hook-lifetime").expect("project"),
                    repository_id: RepositoryId::new("repository.hook-lifetime")
                        .expect("repository"),
                    worktree_id: tracedecay_domain::WorktreeId::new("worktree.hook-lifetime")
                        .expect("worktree"),
                    branch_ref: "refs/heads/main".to_owned(),
                    head_commit_id: CommitId::new("a".repeat(40)).expect("commit"),
                },
                result_id: tracedecay_domain::feedback::FeedbackResultId::new(format!(
                    "result.{suffix}"
                ))
                .expect("result"),
                cycle_id: tracedecay_domain::feedback::FeedbackCycleId::new(format!(
                    "cycle.{suffix}"
                ))
                .expect("cycle"),
                generation_id: tracedecay_domain::CodeGenerationId::new(format!(
                    "generation.{suffix}"
                ))
                .expect("generation"),
                generation_digest: tracedecay_domain::ManifestDigest::new(format!(
                    "sha256:{}",
                    "b".repeat(64)
                ))
                .expect("generation digest"),
                returned_findings: 0,
                omitted_findings: 0,
            }
        }

        let sink = unavailable_advisory_hook_sink();
        let first = notice("first");
        assert_eq!(
            sink(&first),
            tracedecay_hooks::HookFeedbackDeliveryOutcomeV1::Unavailable
        );
        let second = notice("second");
        assert_eq!(
            sink(&second),
            tracedecay_hooks::HookFeedbackDeliveryOutcomeV1::Unavailable
        );
    }

    #[tokio::test]
    async fn denied_or_stale_github_source_access_makes_zero_discovery_network_calls() {
        for lifecycle in [
            GitHubProviderLifecycleV1::Denied,
            GitHubProviderLifecycleV1::Stale,
        ] {
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let calls_for_discovery = Arc::clone(&calls);

            let discovery = discover_github_pull_request_after_authorization(
                || async { lifecycle },
                CancellationToken::new(),
                move |_control| {
                    calls_for_discovery.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    GitHubExactCommitDiscoveryOutcomeV1::Unavailable
                },
            )
            .await;

            assert!(discovery.is_none());
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn cancelled_github_discovery_joins_its_bounded_blocking_owner() {
        let cancellation = CancellationToken::new();
        let cancellation_for_task = cancellation.clone();
        let (started, started_receiver) = tokio::sync::oneshot::channel();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed_finished = Arc::clone(&finished);
        let discovery = tokio::spawn(async move {
            discover_github_pull_request_after_authorization(
                || async { GitHubProviderLifecycleV1::Ready },
                cancellation_for_task,
                move |_| {
                    let _ = started.send(());
                    std::thread::sleep(Duration::from_millis(50));
                    observed_finished.store(true, std::sync::atomic::Ordering::Release);
                    GitHubExactCommitDiscoveryOutcomeV1::Unavailable
                },
            )
            .await
        });
        started_receiver.await.expect("blocking discovery started");
        cancellation.cancel();
        assert!(discovery.await.expect("discovery owner joined").is_none());
        assert!(
            finished.load(std::sync::atomic::Ordering::Acquire),
            "cancellation must retain and join the bounded blocking task"
        );
    }

    #[tokio::test]
    async fn project_open_unavailable_configured_backend_keeps_deterministic_scout() {
        use tracedecay_agent_hosts::automation::config::{AutomationBackend, AutomationConfig};

        for (name, model_config) in [
            (
                "automation-disabled",
                AutomationConfig {
                    enabled: false,
                    backend: AutomationBackend::CodexAppServer,
                    ..AutomationConfig::default()
                },
            ),
            (
                "backend-disabled",
                AutomationConfig {
                    enabled: true,
                    backend: AutomationBackend::Disabled,
                    model_id: None,
                    ..AutomationConfig::default()
                },
            ),
        ] {
            let temporary = tempfile::tempdir().expect("temporary directory");
            let pin = configured_model_pin();
            let control = pin.control();
            let owner = test_scout_owner(&temporary, name).await;

            install_project_open_context_scout_configuration(owner.as_ref(), pin, &model_config)
                .await
                .expect("install unavailable configured backend");
            let outcome = owner
                .prepare_configured(
                    &configured_model_input(control.configuration_revision),
                    MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
                    CancellationToken::new(),
                )
                .await
                .expect("deterministic fallback");
            let crate::agents::context_scout_v2::ContextScoutRuntimeOutcomeV1::Enqueued {
                entry,
                ..
            } = outcome
            else {
                panic!("deterministic fallback should enqueue: {outcome:?}");
            };

            assert_eq!(
                entry.route,
                crate::agents::context_scout_v2::ContextScoutRouteV1::DeterministicFallback
            );
            assert_eq!(
                entry.model_outcome,
                crate::agents::context_scout_v2::ContextScoutModelRunOutcomeV1::Unavailable
            );
            assert!(entry.model_receipt.is_none());
        }
    }

    #[tokio::test]
    async fn project_open_configured_backend_receipt_uses_pinned_automation_configuration() {
        use tracedecay_agent_hosts::automation::config::{AutomationBackend, AutomationConfig};

        let temporary = tempfile::tempdir().expect("temporary directory");
        let model_config = AutomationConfig {
            enabled: true,
            backend: AutomationBackend::CodexAppServer,
            timeout_secs: 73,
            ..AutomationConfig::default()
        };

        let setting_key = tracedecay_domain::configuration::SettingKey::new(
            tracedecay_domain::configuration::CONTEXT_SCOUT_SETTINGS_SETTING_KEY,
        )
        .expect("Scout setting key");
        let revision =
            tracedecay_domain::configuration::ConfigurationRevisionId::new("revision.scout.model")
                .expect("configuration revision");
        let settings = tracedecay_domain::configuration::ContextScoutSettingsV1 {
            schema_version:
                tracedecay_domain::configuration::ContextScoutSettingsV1::SCHEMA_VERSION,
            state: tracedecay_domain::configuration::ContextScoutConfigurationStateV1::Active,
            mode: tracedecay_domain::configuration::ContextScoutConfigurationModeV1::ConfiguredModel,
            limits:
                tracedecay_domain::configuration::ContextScoutConfigurationLimitsV1::bounded_defaults(),
            model_path: Some(
                tracedecay_domain::configuration::ContextScoutConfiguredModelPathV1::CodexAppServer,
            ),
        };
        let snapshot = tracedecay_domain::configuration::ConfigurationSnapshotV1::new(
            BTreeMap::from([(
                setting_key.clone(),
                ConfigurationValueV1::ContextScoutSettings(settings),
            )]),
            BTreeMap::from([(
                setting_key,
                vec![tracedecay_domain::configuration::ConfigurationCandidateV1 {
                    layer: tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
                        project_id: ProjectId::new("project.scout.model").expect("project id"),
                    },
                    revision_id: revision.clone(),
                    disposition: tracedecay_domain::configuration::CandidateDispositionV1::Winning,
                    safe_reason: None,
                }],
            )]),
        )
        .expect("configuration snapshot");
        let pin = ContextScoutConfigurationPinV1::from_current(
            &tracedecay_usecases::configuration::ConfigurationCurrentStateV1 {
                revision_id: revision,
                snapshot,
            },
        )
        .expect("configured-model pin");
        crate::daemon::store_runtime::register_registered_schema_installer();
        let database_path = temporary.path().join("scout.db");
        let database_authority =
            crate::db::DatabaseAuthority::acquire_test(&database_path, "project-open Scout model")
                .expect("database authority");
        let database = crate::db::Database::publish_test_runtime(
            &database_path,
            &database_authority,
            crate::db::TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("project database")
        .0;
        let owner = crate::agents::context_scout_owner::ProjectContextScoutOwnerV1::startup(
            database,
            [7; 16],
            UtcMicros(1),
            None,
        )
        .await
        .expect("Scout owner");

        install_project_open_context_scout_configuration(owner.as_ref(), pin, &model_config)
            .await
            .expect("install configured backend");
        let receipt =
            tracedecay_agent_hosts::automation::backend::backend_availability(&model_config);
        let status = owner.configured_status().await.expect("configured status");

        assert!(model_config.enabled);
        assert_eq!(model_config.backend, AutomationBackend::CodexAppServer);
        assert_eq!(model_config.timeout_secs, 73);
        assert_eq!(receipt.backend, AutomationBackend::CodexAppServer);
        assert_eq!(
            status.model_path,
            Some(crate::agents::context_scout_v2::ContextScoutModelBackendV1::CodexAppServer)
        );
    }

    #[test]
    fn configured_github_mounts_real_ci_provider_configuration() {
        struct ReadyGitHubSourceAccess;

        impl CiSourceAccessAuthorityV1 for ReadyGitHubSourceAccess {
            fn authorize_ci<'a>(
                &'a self,
                _context: &'a tracedecay_application::RequestContext,
                _scope: &'a FeedbackScopeV1,
            ) -> tracedecay_application::feedback::FeedbackPortFuture<
                'a,
                tracedecay_usecases::advisory::CiSourceAccessOutcomeV1,
            > {
                Box::pin(async { tracedecay_usecases::advisory::CiSourceAccessOutcomeV1::Ready })
            }
        }

        let target = GitHubCiRepositoryTargetV1 {
            owner: "ScriptedAlchemy".to_owned(),
            repository: "tracedecay".to_owned(),
        };
        let source_access: Arc<dyn CiSourceAccessAuthorityV1> = Arc::new(ReadyGitHubSourceAccess);
        let ci = production_ci_provider_configuration(
            target.clone(),
            GitHubReadOnlyCredentialV1::anonymous(),
            GitHubHttpReadConfigV1::default(),
            Arc::clone(&source_access),
        )
        .expect("static CI provider configuration");

        assert_eq!(
            ci.provider,
            ProviderId::new("provider.github-actions").expect("provider")
        );
        assert_eq!(ci.parser.parser_id, "parser.github-actions.v1");
        assert_eq!(ci.parser.parser_version, "1");
        assert_eq!(ci.target, target);
        assert!(Arc::ptr_eq(&ci.source_access, &source_access));
    }
}
