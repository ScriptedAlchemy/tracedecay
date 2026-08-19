use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use tokio::sync::Mutex;
use tracedecay_application::{
    ApplicationInvocation, ApplicationInvocationExecutor, ApplicationInvocationFuture,
    ApplicationProblem, ApplicationProblemEnvelope, ApplicationResponse, InvocationError,
    RequestId, ResultContractRef, SafeDiagnostic,
};
use tracedecay_domain::configuration::{
    ConfigurationIdempotencyKey, ConfigurationRevisionId, UserProfileId,
};
use tracedecay_domain::{ActorId, ManifestDigest, ProjectId, UtcMicros};
use tracedecay_lsp::LspSessionRegistry;
use tracedecay_usecases::configuration::DirectConfigurationMutation;

use super::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use super::service::invocation::{
    DaemonConfigurationRuntimeRegistrar, DaemonInvocationService, DaemonRetainedRuntimeRegistrar,
};
use crate::application_surface::ApplicationSurfaceOperation;
use crate::daemon_client::invocation_now_micros;
use crate::daemon_contract::{DaemonInvocationOutcome, DaemonInvocationRequest};
use crate::dashboard::{
    DashboardApplicationRouters, DashboardApplicationRuntime, DashboardConfigurationApplyError,
    DashboardConfigurationApplyFuture, DashboardDaemonReadUnavailableV1,
    DashboardHttpRequestControlV1, DashboardScopeSetReadFuture,
};
use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;
use tracedecay_application::{
    ConfigurationBatchRequestV1, ConfigurationDirectMutationRequestV1, ConfigurationWireRequestV1,
};

const CONFIGURATION_REQUEST_DEADLINE_MICROS: i64 = 15_000_000;
const CONFIGURATION_AUTHORITY_LIFETIME_MICROS: i64 = 3_600_000_000;

#[derive(Clone)]
struct DashboardConfigurationRuntimeForTestV1 {
    service: DaemonInvocationService,
    lsp_registry: Arc<Mutex<LspSessionRegistry>>,
    project_root: PathBuf,
    scope: tracedecay_application::ResolvedScope,
    user_profile_id: UserProfileId,
    result_contract: ResultContractRef,
}

impl DashboardApplicationRuntime for DashboardConfigurationRuntimeForTestV1 {
    fn user_profile_id(&self) -> Option<&UserProfileId> {
        Some(&self.user_profile_id)
    }

    fn routers(
        &self,
        active_project_id: ProjectId,
    ) -> std::result::Result<DashboardApplicationRouters, String> {
        let http = crate::application_surface::http_application_router_with_executor(
            Arc::new(self.clone()),
            tracedecay_usecases::operation_stream::OperationEventAuthority::default(),
            active_project_id,
        )
        .map_err(|error| error.to_string())?;
        Ok(DashboardApplicationRouters {
            http,
            configuration: Router::new(),
            feedback: Router::new(),
            work: Router::new(),
        })
    }

    fn apply_configuration_batch(
        &self,
        request_id: RequestId,
        mutations: Vec<DirectConfigurationMutation>,
        expected_revision: ConfigurationRevisionId,
        idempotency_key: ConfigurationIdempotencyKey,
    ) -> DashboardConfigurationApplyFuture<'_> {
        let mut direct_mutations = Vec::new();
        for mutation in mutations {
            flatten_configuration_mutation(mutation, &mut direct_mutations);
        }
        Box::pin(async move {
            let observed_at = invocation_now_micros();
            let deadline = tracedecay_application::Deadline::new(UtcMicros(
                observed_at
                    .0
                    .saturating_add(CONFIGURATION_REQUEST_DEADLINE_MICROS),
            ))
            .map_err(|_| unavailable_error(&self.result_contract, request_id.clone()))?;
            let cancellation = tracedecay_application::CancellationSignal::active(format!(
                "cancellation.dashboard.configuration.{}",
                request_id.as_str()
            ))
            .map_err(|_| unavailable_error(&self.result_contract, request_id.clone()))?;
            let request = DaemonInvocationRequest::configuration(
                request_id.as_str(),
                ApplicationSurfaceOperation::ConfigurationBatch,
                ConfigurationWireRequestV1::Batch(ConfigurationBatchRequestV1 {
                    mutations: direct_mutations,
                    expected_revision,
                    idempotency_key,
                }),
                observed_at,
                deadline,
                cancellation.context(),
            )
            .with_resolved_scope(Some(self.scope.clone()))
            .with_delivery_route(
                tracedecay_usecases::feedback::observations::FeedbackDeliveryRouteV1::Http,
            );
            let response = self
                .service
                .invoke_with_cancellation(
                    &self.lsp_registry,
                    Some(&self.project_root),
                    None,
                    None,
                    None,
                    request,
                    None,
                )
                .await;
            match response.outcome {
                DaemonInvocationOutcome::Configuration { outcome, .. } => Ok(outcome),
                DaemonInvocationOutcome::ApplicationProblem { problem } => Err(
                    application_problem_error(&self.result_contract, request_id, problem),
                ),
                _ => Err(unavailable_error(&self.result_contract, request_id)),
            }
        })
    }

    fn read_multi_root_scope_set(
        &self,
        _control: DashboardHttpRequestControlV1,
        _scope_set_id: tracedecay_domain::ScopeSetId,
    ) -> DashboardScopeSetReadFuture<'_> {
        // This runtime registers only the configuration and retained owners;
        // multi-root scope-set reads route through the daemon project
        // invocation owner, which is deliberately absent here.
        Box::pin(async {
            Err(DashboardDaemonReadUnavailableV1 {
                detail: "the dashboard configuration test runtime serves no multi-root scope-set reads"
                    .to_owned(),
            })
        })
    }

    fn native_integration_status(
        &self,
        control: DashboardHttpRequestControlV1,
        transaction_id: tracedecay_domain::NativeIntegrationTransactionId,
    ) -> crate::dashboard::DashboardNativeIntegrationStatusFuture<'_> {
        Box::pin(async move {
            crate::mcp::tools::handlers::dashboard::dashboard_native_integration_status(
                self,
                &control,
                transaction_id,
            )
            .await
        })
    }
}

impl ApplicationInvocationExecutor for DashboardConfigurationRuntimeForTestV1 {
    fn invoke(
        &self,
        _invocation: ApplicationInvocation,
    ) -> ApplicationInvocationFuture<'_, std::result::Result<ApplicationResponse, InvocationError>>
    {
        Box::pin(async { Err(InvocationError::Unavailable) })
    }
}

impl crate::daemon_client::DaemonInvocationExecutor for DashboardConfigurationRuntimeForTestV1 {
    fn invoke_controlled(
        &self,
        request: DaemonInvocationRequest,
        deadline: tracedecay_application::Deadline,
        cancellation: tracedecay_application::CancellationSignal,
        _policy: crate::daemon_client::InvocationCancellationPolicy,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            crate::daemon_contract::DaemonInvocationResponse,
            crate::daemon_client::DaemonInvocationError,
        >,
    > {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(crate::daemon_client::DaemonInvocationError::Cancelled {
                    stage: tracedecay_application::CancellationStage::BeforeAdmission,
                });
            }
            if crate::daemon_client::deadline_remaining(&deadline).is_none() {
                return Err(crate::daemon_client::DaemonInvocationError::TimedOut {
                    stage: tracedecay_application::CancellationStage::BeforeAdmission,
                });
            }
            Ok(self
                .service
                .invoke_with_cancellation(
                    &self.lsp_registry,
                    Some(&self.project_root),
                    None,
                    None,
                    None,
                    request,
                    None,
                )
                .await)
        })
    }

    fn observe_feedback(
        &self,
        _subject_digest: ManifestDigest,
        _observed_at: UtcMicros,
        _event: tracedecay_usecases::feedback::observations::FeedbackSourceEventV1,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

fn flatten_configuration_mutation(
    mutation: DirectConfigurationMutation,
    output: &mut Vec<ConfigurationDirectMutationRequestV1>,
) {
    match mutation {
        DirectConfigurationMutation::Set { layer, key, value } => {
            output.push(ConfigurationDirectMutationRequestV1::Set { layer, key, value });
        }
        DirectConfigurationMutation::Unset { layer, key } => {
            output.push(ConfigurationDirectMutationRequestV1::Unset { layer, key });
        }
        DirectConfigurationMutation::Batch { mutations } => {
            for mutation in mutations {
                flatten_configuration_mutation(mutation, output);
            }
        }
    }
}

fn application_problem_error(
    contract: &ResultContractRef,
    request_id: RequestId,
    problem: ApplicationProblem,
) -> DashboardConfigurationApplyError {
    match ApplicationProblemEnvelope::new(contract.clone(), request_id, problem) {
        Ok(problem) => DashboardConfigurationApplyError::ApplicationProblem(problem),
        Err(error) => DashboardConfigurationApplyError::ApplicationContractViolation(error),
    }
}

fn unavailable_error(
    contract: &ResultContractRef,
    request_id: RequestId,
) -> DashboardConfigurationApplyError {
    application_problem_error(
        contract,
        request_id,
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.surface.unavailable".to_owned(),
            message: "The dashboard configuration application service is unavailable".to_owned(),
        }),
    )
}

pub(crate) async fn dashboard_configuration_runtime_for_test(
    cg: Arc<TraceDecay>,
) -> Result<Arc<dyn DashboardApplicationRuntime>> {
    let project_root = cg.project_root().canonicalize()?;
    let project_id = cg
        .configuration_runtime()
        .configuration_target()
        .project_id
        .clone();
    let scope = super::project_open_owners::resolved_scope_for_project(&project_root, &project_id)
        .map_err(|error| TraceDecayError::Config {
            message: format!("dashboard test configuration scope is invalid: {error}"),
        })?;
    let resident_memory = Arc::new(
        tracedecay_runtime_core::resident_memory::ProcessResidentMemoryV1::new(
            tracedecay_runtime_core::resident_memory::DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
        ),
    );
    let service = DaemonInvocationService::with_code_index_schedulers(
        CodeIndexSchedulerRegistryV1::with_resident_memory(1, resident_memory),
    );
    let observed_at = invocation_now_micros();
    let expires_at = UtcMicros(
        observed_at
            .0
            .saturating_add(CONFIGURATION_AUTHORITY_LIFETIME_MICROS),
    );
    let policy_digest =
        ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).map_err(|error| {
            TraceDecayError::Config {
                message: format!("dashboard test configuration policy digest is invalid: {error}"),
            }
        })?;
    let actor = ActorId::new("actor.dashboard.configuration-test").map_err(|error| {
        TraceDecayError::Config {
            message: format!("dashboard test configuration actor is invalid: {error}"),
        }
    })?;
    let user_profile_id = cg.store_runtime_registry().profile_id().clone();
    DaemonConfigurationRuntimeRegistrar::new(&service)
        .register(
            project_root.clone(),
            Arc::clone(cg.configuration_runtime()),
            scope.clone(),
            user_profile_id.clone(),
            actor,
            expires_at,
            None,
            policy_digest,
        )
        .await?;
    register_dashboard_test_retained_runtime(&service, &cg, project_root.clone(), project_id)
        .await?;
    let operation = tracedecay_application::configuration_surface_operation(
        ApplicationSurfaceOperation::ConfigurationBatch.as_str(),
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("dashboard configuration contract is invalid: {error}"),
    })?
    .ok_or_else(|| TraceDecayError::Config {
        message: "dashboard configuration batch operation is not registered".to_owned(),
    })?;
    Ok(Arc::new(DashboardConfigurationRuntimeForTestV1 {
        service,
        lsp_registry: Arc::new(Mutex::new(LspSessionRegistry::default())),
        project_root,
        scope,
        user_profile_id,
        result_contract: operation.result_contract().clone(),
    }))
}

/// Registers the same retained application runtime production project-open
/// mounts, on a dashboard integration-test invocation service. User-job
/// admission goes through `AutomationEffectAuthority::prepare`, which fails
/// closed without this exact authority.
pub(crate) async fn register_dashboard_test_retained_runtime(
    service: &DaemonInvocationService,
    cg: &Arc<TraceDecay>,
    project_root: PathBuf,
    project_id: ProjectId,
) -> Result<()> {
    let scope = super::project_open_owners::resolved_scope_for_project(&project_root, &project_id)
        .map_err(|error| TraceDecayError::Config {
            message: format!("dashboard test retained scope is invalid: {error}"),
        })?;
    let observed_at = invocation_now_micros();
    let configuration = cg
        .configuration_runtime()
        .client()
        .current()
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("dashboard test retained configuration is unavailable: {error}"),
        })?;
    let retained_access = super::project_open_owners::daemon_owned_project_source_access_at(
        &scope,
        &project_root,
        &configuration,
        observed_at,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("dashboard test retained access is invalid: {error}"),
    })?;
    let retained_grant =
        super::project_open_owners::project_open_retained_grant(&retained_access, observed_at)
            .map_err(|error| TraceDecayError::Config {
                message: format!("dashboard test retained grant is invalid: {error}"),
            })?;
    let retained_ports = super::retained_owner::retained_surface_ports(
        super::retained_owner::ProductionRetainedAuthoritiesV1 {
            cg: Arc::new(tokio::sync::RwLock::new(Arc::clone(cg))),
            project_root: project_root.clone(),
            project_id,
            mounted_profile_id: None,
            mounted_session_store_id: None,
            mounted_session_root_id: None,
            registered_session_db: None,
            project_refresh: None,
            project_retrieval: None,
            project_workflow_index: None,
            project_lcm: None,
            invocation_service: Some(service.clone()),
            configuration_digest: retained_access.configuration_digest.clone(),
        },
    );
    DaemonRetainedRuntimeRegistrar::new(service)
        .register(
            project_root,
            scope,
            retained_access.requester,
            retained_grant,
            retained_ports,
        )
        .await
}
