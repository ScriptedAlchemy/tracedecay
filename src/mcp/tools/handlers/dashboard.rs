//! Handler for the `tracedecay_dashboard` MCP tool.
//!
//! Starts (or stops) the project dashboard HTTP server as a managed background
//! tokio task inside the running MCP server process. Idempotent: returns the
//! existing URL if already running for this process. Supports optional `stop`
//! action to shut down a previously-started instance.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay_application::{
    ApplicationProblem, ApplicationProblemEnvelope, RequestId, SafeDiagnostic,
};
use tracedecay_domain::ProjectId;
use tracedecay_domain::configuration::{
    ConfigurationIdempotencyKey, ConfigurationRevisionId, UserProfileId,
};
use tracedecay_usecases::configuration::DirectConfigurationMutation;

use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDbLeaseV1;
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;
use super::dashboard_lcm::DashboardLcmReadAdapter;
use super::support::generic_tool_result;

use crate::dashboard::{
    AutomationSchedulerReconciler, DEFAULT_PORT, DashboardApplicationRouters,
    DashboardApplicationRuntime, DashboardAutomationWriter, DashboardConfigurationApplyError,
    DashboardConfigurationApplyFuture, DashboardDaemonReadUnavailableV1,
    DashboardHttpRequestControlV1, DashboardScopeSetReadFuture, DashboardStateCompositionV1,
    bind_dashboard, build_state_with_automation_reconciler, router, validate_dashboard_host,
};

struct DashboardInvocationExecutorAdapter {
    executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
    configuration_batch_contract: tracedecay_application::ResultContractRef,
    user_profile_id: Option<UserProfileId>,
}

impl DashboardInvocationExecutorAdapter {
    fn new(
        executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
        user_profile_id: Option<UserProfileId>,
    ) -> Result<Self> {
        let operation =
            tracedecay_application::configuration_surface_operation("configuration_batch")
                .map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "dashboard configuration batch application contract is invalid: {error}"
                    ),
                })?
                .ok_or_else(|| TraceDecayError::Config {
                    message:
                        "dashboard configuration batch application operation is not registered"
                            .to_owned(),
                })?;
        Ok(Self {
            executor,
            configuration_batch_contract: operation.result_contract().clone(),
            user_profile_id,
        })
    }
}

impl DashboardApplicationRuntime for DashboardInvocationExecutorAdapter {
    fn user_profile_id(&self) -> Option<&UserProfileId> {
        self.user_profile_id.as_ref()
    }

    fn for_project_root(
        &self,
        project_root: &std::path::Path,
    ) -> std::result::Result<Arc<dyn DashboardApplicationRuntime>, String> {
        let handshake = crate::daemon::DaemonHandshake::for_current_client(
            Some(project_root.to_path_buf()),
            None,
            false,
            false,
        )
        .map_err(|error| error.to_string())?;
        let executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor> = Arc::new(
            crate::daemon_client::DaemonInvocationClient::for_current(handshake)
                .map_err(|error| error.to_string())?,
        );
        Self::new(executor, self.user_profile_id.clone())
            .map(|runtime| Arc::new(runtime) as Arc<dyn DashboardApplicationRuntime>)
            .map_err(|error| error.to_string())
    }

    fn routers(
        &self,
        active_project_id: ProjectId,
    ) -> std::result::Result<DashboardApplicationRouters, String> {
        let http = crate::application_surface::http_application_router_with_executor(
            Arc::clone(&self.executor),
            tracedecay_usecases::operation_stream::OperationEventAuthority::default(),
            active_project_id,
        )
        .map_err(|error| error.to_string())?;
        let configuration =
            crate::application_surface::dashboard_configuration_application_router_with_executor(
                Arc::clone(&self.executor),
            )
            .map_err(|error| error.to_string())?;
        let feedback =
            crate::application_surface::dashboard_feedback_application_router_with_executor(
                Arc::clone(&self.executor),
            )
            .map_err(|error| error.to_string())?;
        let work = crate::application_surface::dashboard_work_application_router_with_executor(
            Arc::clone(&self.executor),
        )
        .map_err(|error| error.to_string())?;
        Ok(DashboardApplicationRouters {
            http,
            configuration,
            feedback,
            work,
        })
    }

    fn apply_configuration_batch(
        &self,
        request_id: RequestId,
        mutations: Vec<DirectConfigurationMutation>,
        expected_revision: ConfigurationRevisionId,
        idempotency_key: ConfigurationIdempotencyKey,
    ) -> DashboardConfigurationApplyFuture<'_> {
        let executor = Arc::clone(&self.executor);
        let configuration_batch_contract = self.configuration_batch_contract.clone();
        let mut direct_mutations = Vec::new();
        for mutation in mutations {
            append_direct_configuration_mutations(mutation, &mut direct_mutations);
        }
        Box::pin(async move {
            let error_request_id = request_id.clone();
            match crate::application_surface::resolve_dashboard_application_surface(
                crate::application_surface::ApplicationSurfaceOperation::ConfigurationBatch,
                request_id,
                crate::application_surface::ApplicationSurfaceRequest::Configuration(
                    tracedecay_application::ConfigurationWireRequestV1::Batch(
                        tracedecay_application::ConfigurationBatchRequestV1 {
                            mutations: direct_mutations,
                            expected_revision,
                            idempotency_key,
                        },
                    ),
                ),
                crate::daemon_client::RequestedOutputFormat::Json,
                Some(executor.as_ref()),
            )
            .await
            {
                Ok(result) => result
                    .result
                    .map(|envelope| envelope.outcome)
                    .map_err(DashboardConfigurationApplyError::ApplicationProblem),
                Err(_) => match dashboard_configuration_unavailable(
                    configuration_batch_contract,
                    error_request_id,
                ) {
                    Ok(problem) => Err(DashboardConfigurationApplyError::ApplicationProblem(
                        problem,
                    )),
                    Err(error) => {
                        Err(DashboardConfigurationApplyError::ApplicationContractViolation(error))
                    }
                },
            }
        })
    }

    fn read_multi_root_scope_set(
        &self,
        control: DashboardHttpRequestControlV1,
        scope_set_id: tracedecay_domain::ScopeSetId,
    ) -> DashboardScopeSetReadFuture<'_> {
        let executor = Arc::clone(&self.executor);
        Box::pin(async move {
            let request = tracedecay_application::MultiRootScopeSetReadRequestV1::new(scope_set_id)
                .map_err(|error| DashboardDaemonReadUnavailableV1 {
                    detail: error.to_string(),
                })?;
            let invocation =
                crate::daemon_contract::DaemonInvocationRequest::multi_root_scope_set_read(
                    control.request_id().as_str(),
                    request,
                    control.observed_at(),
                    control.deadline(),
                    control.cancellation().context(),
                );
            let response = executor
                .invoke_controlled(
                    invocation,
                    control.deadline(),
                    control.cancellation().clone(),
                    crate::daemon_client::InvocationCancellationPolicy::ReadOnly,
                )
                .await
                .map_err(|error| DashboardDaemonReadUnavailableV1 {
                    detail: format!("the daemon multi-root read transport failed: {error:?}"),
                })?;
            match response.outcome {
                crate::daemon_contract::DaemonInvocationOutcome::MultiRootScopeSetRead {
                    outcome: tracedecay_application::ApplicationOutcome::Evidence(packet),
                    ..
                } => packet
                    .payload
                    .ok_or_else(|| DashboardDaemonReadUnavailableV1 {
                        detail: "the daemon multi-root read returned no evidence payload"
                            .to_owned(),
                    }),
                crate::daemon_contract::DaemonInvocationOutcome::ApplicationProblem { problem } => {
                    Err(DashboardDaemonReadUnavailableV1 {
                        detail: format!(
                            "the daemon rejected the multi-root read: {}",
                            problem.safe_message()
                        ),
                    })
                }
                crate::daemon_contract::DaemonInvocationOutcome::Problem { problem } => {
                    Err(DashboardDaemonReadUnavailableV1 {
                        detail: format!("the daemon refused the multi-root read: {problem:?}"),
                    })
                }
                _ => Err(DashboardDaemonReadUnavailableV1 {
                    detail: "the daemon multi-root read answered with a foreign outcome".to_owned(),
                }),
            }
        })
    }

    fn native_integration_status(
        &self,
        control: DashboardHttpRequestControlV1,
        transaction_id: tracedecay_domain::NativeIntegrationTransactionId,
    ) -> crate::dashboard::DashboardNativeIntegrationStatusFuture<'_> {
        let executor = Arc::clone(&self.executor);
        Box::pin(async move {
            dashboard_native_integration_status(executor.as_ref(), &control, transaction_id).await
        })
    }
}

/// Resolves one native-integration status read over the catalog-bound
/// dashboard surface, answering the same application result CLI and MCP
/// project.
pub(crate) async fn dashboard_native_integration_status(
    executor: &dyn crate::daemon_client::DaemonInvocationExecutor,
    control: &crate::dashboard::DashboardHttpRequestControlV1,
    transaction_id: tracedecay_domain::NativeIntegrationTransactionId,
) -> std::result::Result<
    tracedecay_application::NativeIntegrationSurfaceResultV1,
    crate::dashboard::DashboardDaemonReadUnavailableV1,
> {
    use crate::dashboard::DashboardDaemonReadUnavailableV1;

    let request = crate::daemon_contract::DaemonInvocationRequest::native_integration(
        control.request_id().as_str(),
        crate::application_surface::ApplicationSurfaceOperation::NativeIntegrationStatus,
        crate::application_surface::NativeIntegrationSurfaceRequest::Status(
            tracedecay_application::NativeIntegrationStatusSurfaceRequest { transaction_id },
        ),
        control.observed_at(),
        control.deadline(),
        control.cancellation().context(),
    );
    let response = executor
        .invoke_controlled(
            request,
            control.deadline(),
            control.cancellation().clone(),
            crate::daemon_client::InvocationCancellationPolicy::ReadOnly,
        )
        .await
        .map_err(|error| DashboardDaemonReadUnavailableV1 {
            detail: format!("the dashboard native-integration transport failed: {error:?}"),
        })?;
    let payload = match response.outcome {
        crate::daemon_contract::DaemonInvocationOutcome::NativeIntegration {
            outcome: tracedecay_application::ApplicationOutcome::Evidence(packet),
            ..
        } => packet
            .payload
            .ok_or_else(|| DashboardDaemonReadUnavailableV1 {
                detail: "the native-integration status read returned no evidence payload"
                    .to_owned(),
            })?,
        crate::daemon_contract::DaemonInvocationOutcome::ApplicationProblem { problem } => {
            return Err(DashboardDaemonReadUnavailableV1 {
                detail: format!(
                    "the daemon rejected the native-integration status read: {}",
                    problem.safe_message()
                ),
            });
        }
        crate::daemon_contract::DaemonInvocationOutcome::Problem { problem } => {
            return Err(DashboardDaemonReadUnavailableV1 {
                detail: format!(
                    "the daemon refused the native-integration status read: {problem:?}"
                ),
            });
        }
        _ => {
            return Err(DashboardDaemonReadUnavailableV1 {
                detail: "the native-integration status read answered with a foreign outcome"
                    .to_owned(),
            });
        }
    };
    serde_json::from_value(payload).map_err(|_| DashboardDaemonReadUnavailableV1 {
        detail: "the native-integration status payload violated its wire contract".to_owned(),
    })
}

fn append_direct_configuration_mutations(
    mutation: DirectConfigurationMutation,
    direct_mutations: &mut Vec<tracedecay_application::ConfigurationDirectMutationRequestV1>,
) {
    match mutation {
        DirectConfigurationMutation::Set { layer, key, value } => {
            direct_mutations.push(
                tracedecay_application::ConfigurationDirectMutationRequestV1::Set {
                    layer,
                    key,
                    value,
                },
            );
        }
        DirectConfigurationMutation::Unset { layer, key } => {
            direct_mutations.push(
                tracedecay_application::ConfigurationDirectMutationRequestV1::Unset { layer, key },
            );
        }
        DirectConfigurationMutation::Batch { mutations } => {
            for mutation in mutations {
                append_direct_configuration_mutations(mutation, direct_mutations);
            }
        }
    }
}

fn dashboard_configuration_unavailable(
    contract: tracedecay_application::ResultContractRef,
    request_id: RequestId,
) -> std::result::Result<ApplicationProblemEnvelope, tracedecay_application::ApplicationContractError>
{
    ApplicationProblemEnvelope::new(
        contract,
        request_id,
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.surface.unavailable".to_owned(),
            message: "The dashboard configuration application service is unavailable".to_owned(),
        }),
    )
}

/// Internal handle for a managed dashboard instance.
struct RunningDashboard {
    url: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<Result<()>>,
    completed: Arc<tokio::sync::Notify>,
}

impl RunningDashboard {
    fn request_shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

struct DashboardTaskCompletion(Arc<tokio::sync::Notify>);

impl Drop for DashboardTaskCompletion {
    fn drop(&mut self) {
        self.0.notify_waiters();
    }
}

/// Global manager for at most one dashboard per MCP server process.
/// Uses `OnceLock` + inner `Mutex` so it can be initialized on first use from async.
static DASHBOARD_MANAGER: std::sync::OnceLock<tokio::sync::Mutex<Option<RunningDashboard>>> =
    std::sync::OnceLock::new();

fn get_manager() -> &'static tokio::sync::Mutex<Option<RunningDashboard>> {
    DASHBOARD_MANAGER.get_or_init(|| tokio::sync::Mutex::new(None))
}

async fn take_finished_dashboard() -> Option<RunningDashboard> {
    let mut manager = get_manager().lock().await;
    if manager
        .as_ref()
        .is_some_and(|dashboard| dashboard.task.is_finished())
    {
        manager.take()
    } else {
        None
    }
}

async fn join_dashboard(dashboard: RunningDashboard, exceeded_deadline: bool) -> Result<()> {
    let url = dashboard.url;
    match dashboard.task.await {
        Ok(Ok(())) if !exceeded_deadline => Ok(()),
        Ok(Ok(())) => Err(TraceDecayError::Config {
            message: format!("dashboard '{url}' exceeded its shutdown deadline"),
        }),
        Ok(Err(error)) => Err(error),
        Err(error) if error.is_cancelled() && exceeded_deadline => Err(TraceDecayError::Config {
            message: format!("dashboard '{url}' was aborted after its shutdown deadline"),
        }),
        Err(error) => Err(TraceDecayError::Config {
            message: format!("dashboard '{url}' task failed: {error}"),
        }),
    }
}

/// Stops the process-local dashboard and joins its serving task. Once the
/// deadline expires the task is aborted, but its handle stays retained until
/// the cancellation has actually joined.
pub(crate) async fn shutdown_dashboard_until(deadline: tokio::time::Instant) -> Result<()> {
    {
        let mut manager = get_manager().lock().await;
        let Some(dashboard) = manager.as_mut() else {
            return Ok(());
        };
        dashboard.request_shutdown();
    }

    let mut exceeded_deadline = false;
    loop {
        if let Some(dashboard) = take_finished_dashboard().await {
            return join_dashboard(dashboard, exceeded_deadline).await;
        }
        let completed = {
            let manager = get_manager().lock().await;
            let Some(dashboard) = manager.as_ref() else {
                return Ok(());
            };
            Arc::clone(&dashboard.completed)
        };
        let notified = completed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(dashboard) = take_finished_dashboard().await {
            return join_dashboard(dashboard, exceeded_deadline).await;
        }
        if exceeded_deadline {
            notified.as_mut().await;
            continue;
        }
        tokio::select! {
            biased;
            () = notified.as_mut() => {}
            () = tokio::time::sleep_until(deadline) => {
                let mut manager = get_manager().lock().await;
                if let Some(dashboard) = manager.as_mut() {
                    dashboard.task.abort();
                }
                exceeded_deadline = true;
            }
        }
    }
}

pub(crate) async fn shutdown_dashboard() -> Result<()> {
    shutdown_dashboard_until(tokio::time::Instant::now() + crate::daemon::DAEMON_SHUTDOWN_DEADLINE)
        .await
}

fn dashboard_tool_result(cg: &TraceDecay, args: &Value, payload: &Value) -> ToolResult {
    generic_tool_result(Some(cg.project_root()), args, payload, vec![])
}

/// Handles `tracedecay_dashboard` tool calls.
pub(super) async fn handle_dashboard(
    cg: &TraceDecay,
    args: Value,
    retained_project_server_resolver: Option<crate::mcp::server::RetainedProjectServerResolver>,
    code_graph_read_admission: Option<crate::mcp::server::CodeGraphReadAdmissionPort>,
    code_graph_projection_read_port: Option<crate::mcp::server::CodeGraphProjectionReadPort>,
    registered_project_session_db: Option<RegisteredGlobalDbLeaseV1>,
    daemon_user_profile_id: Option<UserProfileId>,
    daemon_profile_root: Option<PathBuf>,
    session_retrieval: Option<
        Arc<dyn crate::daemon::session_retrieval::SessionApplicationRetrievalPortV1>,
    >,
    session_identity: Option<tracedecay_usecases::context::ResolvedSessionIdentity>,
    registered_savings_db: Option<RegisteredGlobalDbLeaseV1>,
    automation_scheduler_reconciler: Option<AutomationSchedulerReconciler>,
    automation_writer: DashboardAutomationWriter,
    doctor_report_reader: Option<crate::dashboard::DoctorReportReader>,
    remote_operational_status: Option<
        crate::daemon::remote_protocol::RemoteOperationalStatusProviderV1,
    >,
    code_index_freshness_reader: Option<
        crate::dashboard::code_index_freshness_api::CodeIndexFreshnessReader,
    >,
    explorer_semantic_reader: Option<crate::dashboard::ExplorerSemanticReader>,
    feedback_status_reader: Option<crate::dashboard::feedback_api::FeedbackStatusReader>,
    code_diagnostics_broker: Option<
        Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>,
    >,
    application_invocation_executor: Option<
        Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
    >,
    delivery_settlement_authority: Option<
        Arc<tracedecay_usecases::observability::DeliverySettlementAuthorityV1>,
    >,
    daemon_invocation_service: Option<crate::daemon::DaemonInvocationService>,
) -> Result<ToolResult> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("start");

    match action {
        "stop" => {
            let previous_url = {
                let manager = get_manager().lock().await;
                manager.as_ref().map(|dashboard| dashboard.url.clone())
            };
            let payload = if let Some(previous_url) = previous_url {
                shutdown_dashboard().await?;
                json!({ "status": "stopped", "previous_url": previous_url })
            } else {
                json!({ "status": "not_running" })
            };
            Ok(dashboard_tool_result(cg, &args, &payload))
        }
        "start" | "" => {
            if let Some(finished) = take_finished_dashboard().await {
                join_dashboard(finished, false).await?;
            }
            let host = args
                .get("host")
                .and_then(|v| v.as_str())
                .map(validate_dashboard_host)
                .transpose()?
                .unwrap_or("127.0.0.1")
                .to_string();
            let port = args
                .get("port")
                .and_then(serde_json::Value::as_u64)
                .and_then(|p| u16::try_from(p).ok())
                .unwrap_or(DEFAULT_PORT);

            let manager = get_manager();
            let mut guard = manager.lock().await;

            if let Some(handle) = guard.as_ref() {
                let status = if handle.shutdown.is_some() {
                    "already_running"
                } else {
                    "stopping"
                };
                return Ok(dashboard_tool_result(
                    cg,
                    &args,
                    &json!({
                        "status": status,
                        "url": handle.url
                    }),
                ));
            }

            // Shared construction with the CLI path: resolved LCM/session store
            // selection included. No catch-up ingest spawn here — the host
            // MCP server already swept hookless transcripts at startup.
            let retained_server = retained_project_server_resolver.as_ref().ok_or_else(|| {
                TraceDecayError::Config {
                    message: "retained dashboard project server resolver is unavailable"
                        .to_string(),
                }
            })?(
                crate::mcp::server::RetainedProjectGraphRequest::for_mounted_root(
                    cg.project_root().to_path_buf(),
                ),
            )
            .await?
            .ok_or_else(|| TraceDecayError::Config {
                message: "retained dashboard project server is unavailable".to_string(),
            })?;
            if let Some(expected_profile_id) = daemon_user_profile_id.as_ref()
                && retained_server
                    .profile_identity()
                    .is_none_or(|identity| identity.profile_id() != expected_profile_id)
            {
                return Err(TraceDecayError::project_route(
                    "project_route_not_authorized",
                    false,
                    "retained dashboard project belongs to another profile",
                ));
            }
            let retained_graph = retained_server.cg_snapshot().await;
            let retained_root = retained_graph
                .project_root()
                .canonicalize()
                .map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "retained dashboard project root '{}' is unavailable: {error}",
                        retained_graph.project_root().display()
                    ),
                })?;
            let requested_root =
                cg.project_root()
                    .canonicalize()
                    .map_err(|error| TraceDecayError::Config {
                        message: format!(
                            "dashboard project root '{}' is unavailable: {error}",
                            cg.project_root().display()
                        ),
                    })?;
            if retained_root != requested_root {
                return Err(TraceDecayError::project_route(
                    "project_route_unavailable",
                    true,
                    "retained dashboard project server resolved a different root",
                ));
            }
            let retained_cg: Arc<dyn crate::dashboard::DashboardProjectRuntime> = retained_graph;
            let dashboard_project_graph_resolver = retained_project_server_resolver
                .clone()
                .zip(daemon_user_profile_id.clone())
                .map(|(resolver, profile_id)| {
                    crate::mcp::server::dashboard_retained_project_graph_resolver(
                        resolver, profile_id,
                    )
                });
            let automation_observation = daemon_invocation_service
                .clone()
                .map(crate::daemon::dashboard_automation::dashboard_automation_observation_port);
            let automation_authority = match (
                daemon_profile_root,
                daemon_user_profile_id.clone(),
                retained_project_server_resolver.clone(),
                daemon_invocation_service.clone(),
            ) {
                (
                    Some(profile_root),
                    Some(profile_id),
                    Some(project_graph_resolver),
                    Some(invocation_service),
                ) => Some(
                    crate::daemon::dashboard_automation::compose_dashboard_automation_authority(
                        profile_root,
                        profile_id,
                        project_graph_resolver,
                        Arc::clone(&automation_writer),
                        invocation_service,
                    )?,
                ),
                (None, None, _, None) => None,
                _ => {
                    return Err(TraceDecayError::Config {
                        message: "dashboard automation requires one complete daemon profile and project authority"
                            .to_owned(),
                    });
                }
            };
            // The profile write resolves its configuration layer through the
            // profile identity the daemon handshake bound, which every
            // daemon-owned server carries. Reading it from the project-session
            // store instead withheld every profile mutation on the core server
            // that answers tool calls before the session authorities mount.
            let application_invocation_executor = application_invocation_executor
                .map(|executor| {
                    DashboardInvocationExecutorAdapter::new(executor, daemon_user_profile_id)
                        .map(|adapter| Arc::new(adapter) as Arc<dyn DashboardApplicationRuntime>)
                })
                .transpose()?;
            let lcm_read_authority = session_retrieval
                .zip(session_identity)
                .and_then(|(retrieval, identity)| DashboardLcmReadAdapter::new(retrieval, identity))
                .map(|adapter| {
                    Arc::new(adapter) as Arc<dyn crate::dashboard::DashboardLcmReadPortV1>
                });
            // Loom's git sources read the verified session-git-evidence
            // projection through the same registered store; a state composed
            // without it reports those sources unavailable.
            let git_correlation_read_authority =
                registered_project_session_db.as_ref().map(|database| {
                    Arc::new(
                        super::dashboard_git_correlation::DashboardGitCorrelationReadAdapter::new(
                            database.clone(),
                        ),
                    )
                        as Arc<dyn crate::dashboard::DashboardGitCorrelationReadPortV1>
                });
            let delivery_read_authority = daemon_invocation_service.map(|service| {
                let adapter = super::dashboard_delivery::DashboardDeliveryReadAdapter::new(
                    service,
                    retained_cg.project_root().to_path_buf(),
                );
                Arc::new(adapter) as Arc<dyn crate::dashboard::DashboardDeliveryReadPortV1>
            });
            crate::hooks::install_dashboard_hook_readiness_projection()?;
            let state = build_state_with_automation_reconciler(
                retained_cg.clone(),
                DashboardStateCompositionV1 {
                    project_graph_resolver: dashboard_project_graph_resolver,
                    code_graph_read_admission,
                    code_graph_projection_read_port,
                    registered_project_session_db,
                    lcm_read_authority,
                    git_correlation_read_authority,
                    delivery_read_authority,
                    registered_savings_db,
                    automation_scheduler_reconciler,
                    automation_authority,
                    automation_observation,
                    automation_writer,
                    doctor_report_reader,
                    remote_operational_status_reader: remote_operational_status.map(|provider| {
                        Arc::new(move || provider())
                            as crate::dashboard::RemoteOperationalStatusReader
                    }),
                    code_index_freshness_reader,
                    explorer_semantic_reader,
                    feedback_status_reader,
                    code_diagnostics_broker,
                    application_invocation_executor,
                    delivery_settlement_authority,
                },
            )
            .await?;

            let app = router(retained_cg.as_ref(), state, crate::dashboard::spa_router()).await?;
            let (listener, addr) = bind_dashboard(&host, port).await?;
            let app = crate::dashboard::with_dashboard_http_admission(app, addr);
            let url = format!("http://{addr}/");

            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
            let completed = Arc::new(tokio::sync::Notify::new());
            let task_completion = DashboardTaskCompletion(Arc::clone(&completed));
            let task = tokio::spawn(async move {
                let _completion = task_completion;
                axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .map_err(|error| TraceDecayError::Config {
                        message: format!("dashboard server failed: {error}"),
                    })
            });

            *guard = Some(RunningDashboard {
                url: url.clone(),
                shutdown: Some(shutdown_tx),
                task,
                completed,
            });

            Ok(dashboard_tool_result(
                cg,
                &args,
                &json!({
                    "status": "started",
                    "url": url,
                    "host": host,
                    "port": addr.port()
                }),
            ))
        }
        other => Err(TraceDecayError::Config {
            message: format!(
                "unknown action for tracedecay_dashboard: {other} (use 'start' or 'stop')"
            ),
        }),
    }
}

#[cfg(test)]
mod tests;
