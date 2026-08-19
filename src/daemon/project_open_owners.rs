//! Project-open registration for the daemon's production owners.
//!
//! After Scout bootstrap and successful cache publication, the daemon mounts
//! each concrete owner from the admitted project identity. Owners mount only
//! when their real upstream authorities resolve; missing identity fails closed
//! and placeholder owners are never installed.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use tracedecay_application::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1,
    FEEDBACK_EXPAND_CAPABILITY_ID_V1, FEEDBACK_GET_CAPABILITY_ID_V1,
    FEEDBACK_LIST_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
    PROXIMITY_CAPABILITY_ID_V1,
};
use tracedecay_application::{ApplicationContractError, ResolvedScope, now_micros};
use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, AuthorityRef, CapabilityResolutionContextV1, ConfigurationValueV1,
    SOURCE_BINDINGS_SETTING_KEY, ScopeSourceBinding, SettingKey, SourceBindingId, SourceKindV1,
    resolve_restrictive_capabilities,
};
use tracedecay_domain::feedback::GitHubPullRequestIdV1;
use tracedecay_domain::{
    ActorId, CapabilityId as DomainCapabilityId, LocatorDigest, ProjectId, RefId, UtcMicros,
    canonical_sha256,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
use tracedecay_usecases::advisory::GitHubRepositoryTargetV1;

use super::{
    DaemonContextScoutRuntimeRegistrationError, DaemonFeedbackRuntimeRegistrationError,
    DaemonInvocationState,
};
use crate::request_identity::{PreviewIdentityDomain, derive_preview_identity};

const SOURCE_EDIT_PRIVACY_KEY_EPOCH_V1: u64 = 1;
use crate::daemon::git_transactions::DaemonGitIndexTransactionServiceRegistry;
use crate::daemon::native_integration::DaemonNativeIntegrationServiceRegistry;
use crate::errors::{Result, TraceDecayError};
use crate::mcp::McpServer;
use tracedecay_lsp::analyzer::broker::AdmittedLspProvider;
use tracedecay_lsp::analyzer::client::LspRefreshTimeouts;
use tracedecay_usecases::lsp_runtime::DaemonLspSessionFactory;
use tracedecay_usecases::primitives::{admitted_root_uri_for_project, locator_digest_for_project};
use tracedecay_usecases::source_authorization::ProjectSourceAccessSnapshot;

mod advisory_runtime;
mod automation_effect_recovery;
mod code_index_reads;
mod lsp_registration;
mod primitive_runtime;
mod query_authority_upgrade;
mod source_edit_owner;
#[cfg(test)]
mod work_grant_tests;

pub(crate) use advisory_runtime::ProjectOpenDependentOwnerState;
pub(super) use advisory_runtime::register_project_open_dependent_owners;
pub(crate) use automation_effect_recovery::reconcile_project_open_automation_effects;
pub(crate) use code_index_reads::{
    project_code_graph_projection_read_port, project_code_index_generation_census_reader,
    project_code_index_ignored_dependency_admission_port,
};

use lsp_registration::production_lsp_registration;
use primitive_runtime::open_and_register_project_primitive_runtime;
use source_edit_owner::{
    install_project_open_source_edit_rollback_owner, source_edit_authority_error,
    source_edit_contract_error, source_edit_request_context, source_edit_surface_result,
};

const DAEMON_REQUESTER: &str = "actor.tracedecay-daemon.project-open";
const DAEMON_BINDING: &str = "binding.tracedecay-daemon.project-open";
const GRANT_HORIZON: Duration = Duration::from_hours(24);
const POLICY_REVISION_V1: u64 = 1;
const LSP_DIAGNOSTICS_QUIET: Duration = Duration::from_secs(2);
pub(super) const LSP_WORKSPACE_CAPABILITY_ID_V1: &str =
    "capability.application.lsp.workspace-folders";
pub(super) const LSP_WORKSPACE_USE_CASE_ID_V1: &str = "use-case.application.lsp.workspace-folders";

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
) -> Result<tracedecay_application::source_edit::SourceEditSurfaceResultV1> {
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
    .and_then(source_edit_surface_result)
}

async fn invoke_project_open_source_edit_reconciliation(
    graph: Arc<crate::tracedecay::TraceDecay>,
    authorization: ProjectOpenSourceEditAuthorizationV1,
    invocation: crate::mcp::server::SourceEditReconciliationInvocationV1,
) -> Result<tracedecay_application::source_edit::SourceEditSurfaceResultV1> {
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
    .and_then(source_edit_surface_result)
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
        phase = "registered_project_storage_acquired",
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
    let _scout_registry = match invocation
        .context_scout_runtime_registrar()
        .open_and_register(
            database.clone(),
            session_db.binding().shard_id.profile_id.clone(),
            project_id.clone(),
            project_root.to_path_buf(),
        )
        .await
    {
        Ok(registry) => registry,
        Err(DaemonContextScoutRuntimeRegistrationError::AlreadyRegistered) => invocation
            .context_scout_runtime_registrar()
            .get(
                &session_db.binding().shard_id.profile_id,
                &project_id,
                project_root,
            )
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
                session_db.clone(),
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
            session_db.clone(),
            &scope,
            &access,
        )
        .await?;
    let work_evidence_retrieval =
        server.work_evidence_retrieval(&scope, invocation.work_federated_query_authority())?;
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
                session_db.clone(),
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
                    session_db.clone(),
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
            session_db.clone(),
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
    open_and_register_project_primitive_runtime(
        invocation,
        project_root,
        graph.clone(),
        server,
        session_db.clone(),
        access.clone(),
        &admitted_root_uri,
    )
    .await?;
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "primitive_runtime_registered",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );
    owner_phase_started = Instant::now();

    let mut mounted_providers = Vec::new();
    let mut lsp_session_factory = None;
    let diagnostic_broker = server.diagnostics_lsp();
    let indexed_generation = invocation
        .code_index_schedulers
        .latest_complete_ready_decoded_for_root_scope(project_root, &scope)
        .await;
    if let Some(generation) = indexed_generation {
        let mut indexed_files = generation
            .generation()
            .snapshot()
            .files
            .iter()
            .map(|file| file.logical_path.clone())
            .collect::<Vec<_>>();
        indexed_files.sort();
        let admitted_providers = {
            let mut broker = diagnostic_broker.lock().await;
            let admitted = broker.admitted_providers_for_files(&indexed_files);
            mounted_providers = broker.mounted_providers_for_files(&indexed_files);
            admitted
        };
        tracing::info!(
            event = "project_open_owner_phase",
            project = %project_root.display(),
            phase = "lsp_languages_discovered",
            step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
            elapsed_ms = owner_registration_started.elapsed().as_millis(),
        );
        owner_phase_started = Instant::now();

        // Feedback runtime registration installed a typed unavailable cycle.
        // The LSP gateway publishes only against a real sealed file census.
        let lsp_scope_grant =
            project_open_lsp_scope_grant(&access, now_micros()).map_err(|error| {
                TraceDecayError::Config {
                    message: format!("project-open LSP workspace grant is invalid: {error}"),
                }
            })?;
        lsp_session_factory = Some(
            register_production_lsp_owner(
                invocation,
                project_root,
                lsp_scope_grant,
                session_db.clone(),
                database.clone(),
                Arc::clone(&diagnostic_broker),
                &admitted_providers,
                admitted_root_uri.clone(),
            )
            .await?,
        );
        tracing::info!(
            event = "project_open_owner_phase",
            project = %project_root.display(),
            phase = "lsp_owner_registered",
            step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
            elapsed_ms = owner_registration_started.elapsed().as_millis(),
        );
    } else {
        // Protocol sessions (initialize / shutdown / exit) must be admissible
        // as soon as the project route is published. Diagnostics still wait
        // for a sealed census; the deferred owner upgrade replaces this
        // warming registration when that generation arrives.
        let lsp_scope_grant =
            project_open_lsp_scope_grant(&access, now_micros()).map_err(|error| {
                TraceDecayError::Config {
                    message: format!("project-open LSP workspace grant is invalid: {error}"),
                }
            })?;
        register_production_lsp_owner(
            invocation,
            project_root,
            lsp_scope_grant,
            session_db.clone(),
            database.clone(),
            Arc::clone(&diagnostic_broker),
            &[],
            admitted_root_uri.clone(),
        )
        .await?;
        tracing::info!(
            event = "project_open_owner_phase",
            project = %project_root.display(),
            phase = "lsp_owner_registered",
            reason = "warming_without_sealed_generation",
            step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
            elapsed_ms = owner_registration_started.elapsed().as_millis(),
        );
    }

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

    // Once-per-project-open adoption-eligibility census over the composed
    // capability catalog, recorded through the project-bound session
    // authority. Fire-and-forget telemetry: project open never blocks or
    // fails on observation storage.
    let census_db = session_db.clone();
    let census_project_root = project_root.to_path_buf();
    tokio::spawn(async move {
        super::adoption_observation::record_project_open_adoption_census(
            census_db.as_ref(),
            &census_project_root,
        )
        .await;
    });

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
        scout_configuration,
        requester,
        mounted_providers,
        admitted_root_uri,
        diagnostic_broker,
        lsp_session_factory,
    })
}

async fn register_semantic_activation_owner(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    graph: &Arc<crate::tracedecay::TraceDecay>,
    session_db: crate::global_db::RegisteredGlobalDbLeaseV1,
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
    let profile_id = session_db.binding().shard_id.profile_id.clone();
    let observer = invocation.query_activation_registrar(project_root, session_db.clone());
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
                    profile_id.clone(),
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
            .mount_query_authority_for_project(project_root, &profile_id, &scope)
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
                    query_authority_upgrade::DeferredQueryAuthorityMountV1::Configured {
                        profile_id,
                    },
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
                                session_db: session_db.clone(),
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

async fn register_production_lsp_owner(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    scope_grant: tracedecay_application::CapabilityGrantSnapshot,
    registered_database: crate::global_db::RegisteredGlobalDbLeaseV1,
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
        &access.binding,
        &access.effective_capabilities,
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

pub(super) fn project_open_retained_grant(
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
        GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
        CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
        PROXIMITY_CAPABILITY_ID_V1,
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
    for descriptor in
        tracedecay_application::retrieval::catalog::primitive_read_handler_descriptors()?
    {
        capabilities.insert(descriptor.operation().capability_id().clone());
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
mod tests {
    use super::*;

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
    fn production_project_owner_grants_every_primitive_read() {
        let capabilities = production_owner_capabilities().expect("production capabilities");

        for descriptor in
            tracedecay_application::retrieval::catalog::primitive_read_handler_descriptors()
                .expect("primitive read descriptors")
        {
            assert!(
                capabilities.contains(descriptor.operation().capability_id()),
                "{} must be granted to the daemon-owned project route",
                descriptor.operation().capability_id().as_str()
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

    fn admitted(language: &str, analyzer_available: bool) -> AdmittedLspProvider {
        AdmittedLspProvider {
            language: language.to_owned(),
            command: format!("{language}-language-server"),
            analyzer_available,
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
        assert_eq!(
            gateway.semantic,
            tracedecay_lsp::SemanticCapability::ALL
                .into_iter()
                .collect()
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
            assert_eq!(
                gateway.semantic,
                tracedecay_lsp::SemanticCapability::ALL
                    .into_iter()
                    .collect()
            );
        }
    }
}
