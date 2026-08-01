//! Handler for the `tracedecay_dashboard` MCP tool.
//!
//! Starts (or stops) the project dashboard HTTP server as a managed background
//! tokio task inside the running MCP server process. Idempotent: returns the
//! existing URL if already running for this process. Supports optional `stop`
//! action to shut down a previously-started instance.

use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay_application::{
    ApplicationProblem, ApplicationProblemEnvelope, RequestId, SafeDiagnostic,
};
use tracedecay_domain::ProjectId;
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_usecases::configuration::DirectConfigurationMutation;

use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;
use super::support::generic_tool_result;

use crate::dashboard::{
    AutomationSchedulerReconciler, DEFAULT_PORT, DashboardApplicationRouters,
    DashboardApplicationRuntime, DashboardAutomationWriter, DashboardConfigurationApplyFuture,
    DashboardStateCompositionV1, bind_dashboard, build_state_with_automation_reconciler, router,
    validate_dashboard_host,
};

struct DashboardInvocationExecutorAdapter {
    executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
}

impl DashboardInvocationExecutorAdapter {
    fn new(executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>) -> Self {
        Self { executor }
    }
}

impl DashboardApplicationRuntime for DashboardInvocationExecutorAdapter {
    fn routers(
        &self,
        active_project_id: ProjectId,
    ) -> std::result::Result<DashboardApplicationRouters, String> {
        let http = crate::application_surface::http_application_router_with_executor(
            Arc::clone(&self.executor),
            crate::application::operation_stream::OperationEventAuthority::default(),
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

    fn apply_configuration_batch<'a>(
        &'a self,
        request_id: RequestId,
        mutations: Vec<DirectConfigurationMutation>,
        expected_revision: ConfigurationRevisionId,
    ) -> DashboardConfigurationApplyFuture<'a> {
        let executor = Arc::clone(&self.executor);
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
                    crate::application_surface::ConfigurationSurfaceRequest::Batch(
                        crate::application_surface::ConfigurationBatchSurfaceRequest {
                            mutations: direct_mutations,
                            expected_revision,
                        },
                    ),
                ),
                crate::daemon_client::RequestedOutputFormat::Json,
                Some(executor.as_ref()),
            )
            .await
            {
                Ok(result) => result.result.map(|_| ()),
                Err(_) => Err(dashboard_configuration_unavailable(error_request_id)),
            }
        })
    }
}

fn append_direct_configuration_mutations(
    mutation: DirectConfigurationMutation,
    direct_mutations: &mut Vec<
        crate::application_surface::ConfigurationDirectMutationSurfaceRequest,
    >,
) {
    match mutation {
        DirectConfigurationMutation::Set { layer, key, value } => {
            direct_mutations.push(
                crate::application_surface::ConfigurationDirectMutationSurfaceRequest::Set {
                    layer,
                    key,
                    value,
                },
            );
        }
        DirectConfigurationMutation::Unset { layer, key } => {
            direct_mutations.push(
                crate::application_surface::ConfigurationDirectMutationSurfaceRequest::Unset {
                    layer,
                    key,
                },
            );
        }
        DirectConfigurationMutation::Batch { mutations } => {
            for mutation in mutations {
                append_direct_configuration_mutations(mutation, direct_mutations);
            }
        }
    }
}

fn dashboard_configuration_unavailable(request_id: RequestId) -> ApplicationProblemEnvelope {
    let operation = tracedecay_application::configuration_surface_operation("configuration_batch")
        .expect("configuration batch application contract is valid")
        .expect("configuration batch application operation is registered");
    ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
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
    shutdown: tokio::sync::oneshot::Sender<()>,
}

/// Global manager for at most one dashboard per MCP server process.
/// Uses `OnceLock` + inner `Mutex` so it can be initialized on first use from async.
static DASHBOARD_MANAGER: std::sync::OnceLock<tokio::sync::Mutex<Option<RunningDashboard>>> =
    std::sync::OnceLock::new();

fn get_manager() -> &'static tokio::sync::Mutex<Option<RunningDashboard>> {
    DASHBOARD_MANAGER.get_or_init(|| tokio::sync::Mutex::new(None))
}

fn dashboard_tool_result(cg: &TraceDecay, args: &Value, payload: &Value) -> ToolResult {
    generic_tool_result(Some(cg.project_root()), args, payload, vec![])
}

/// Handles `tracedecay_dashboard` tool calls.
pub(super) async fn handle_dashboard(
    cg: &TraceDecay,
    args: Value,
    retained_project_graph_resolver: Option<crate::mcp::server::RetainedProjectGraphResolver>,
    registered_project_session_db: Option<Arc<RegisteredGlobalDb>>,
    registered_savings_db: Option<Arc<RegisteredGlobalDb>>,
    automation_scheduler_reconciler: Option<AutomationSchedulerReconciler>,
    automation_writer: DashboardAutomationWriter,
    doctor_report_reader: Option<crate::dashboard::DoctorReportReader>,
    doctor_remediation_dispatcher: Option<crate::dashboard::DoctorRemediationDispatcherV1>,
    code_index_freshness_reader: Option<
        crate::dashboard::code_index_freshness_api::CodeIndexFreshnessReader,
    >,
    feedback_status_reader: Option<crate::dashboard::feedback_api::FeedbackStatusReader>,
    code_diagnostics_broker: Option<
        Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>,
    >,
    application_invocation_executor: Option<
        Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
    >,
) -> Result<ToolResult> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("start");

    match action {
        "stop" => {
            let manager = get_manager();
            let mut guard = manager.lock().await;
            let payload = if let Some(handle) = guard.take() {
                let _ = handle.shutdown.send(());
                json!({ "status": "stopped", "previous_url": handle.url })
            } else {
                json!({ "status": "not_running" })
            };
            Ok(dashboard_tool_result(cg, &args, &payload))
        }
        "start" | "" => {
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
                // already running — idempotent return
                return Ok(dashboard_tool_result(
                    cg,
                    &args,
                    &json!({
                        "status": "already_running",
                        "url": handle.url
                    }),
                ));
            }

            // Shared construction with the CLI path: resolved LCM/session store
            // selection included. No catch-up ingest spawn here — the host
            // MCP server already swept hookless transcripts at startup.
            let retained_cg = retained_project_graph_resolver.as_ref().ok_or_else(|| {
                TraceDecayError::Config {
                    message: "retained dashboard project graph resolver is unavailable".to_string(),
                }
            })?(
                crate::mcp::server::RetainedProjectGraphRequest::for_mounted_root(
                    cg.project_root().to_path_buf(),
                ),
            )
            .await?
            .ok_or_else(|| TraceDecayError::Config {
                message: "retained dashboard project graph is unavailable".to_string(),
            })?;
            let dashboard_project_graph_resolver = retained_project_graph_resolver
                .map(crate::mcp::server::dashboard_retained_project_graph_resolver);
            let application_invocation_executor = application_invocation_executor.map(|executor| {
                Arc::new(DashboardInvocationExecutorAdapter::new(executor))
                    as Arc<dyn DashboardApplicationRuntime>
            });
            let state = build_state_with_automation_reconciler(
                retained_cg.clone(),
                DashboardStateCompositionV1 {
                    project_graph_resolver: dashboard_project_graph_resolver,
                    registered_project_session_db,
                    registered_savings_db,
                    automation_scheduler_reconciler,
                    automation_writer,
                    doctor_report_reader,
                    doctor_remediation_dispatcher,
                    code_index_freshness_reader,
                    feedback_status_reader,
                    code_diagnostics_broker,
                    application_invocation_executor,
                },
            )
            .await?;

            let app = router(retained_cg.as_ref(), state, crate::dashboard::spa_router()).await?;
            let (listener, addr) = bind_dashboard(&host, port).await?;
            let app = crate::dashboard::with_dashboard_http_admission(app, addr);
            let url = format!("http://{addr}/");

            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

            tokio::spawn(async move {
                // Use with_graceful_shutdown so `stop` can cleanly terminate serve.
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = shutdown_rx.await;
                    })
                    .await;
            });

            *guard = Some(RunningDashboard {
                url: url.clone(),
                shutdown: shutdown_tx,
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
