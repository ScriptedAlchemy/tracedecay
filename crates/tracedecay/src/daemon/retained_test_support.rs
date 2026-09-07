//! In-process retained application owner for direct MCP test servers.
//!
//! Retained surface tools (`tracedecay_lcm_*`, session and memory reads)
//! execute through the daemon invocation transport in production. A direct
//! test server has no daemon socket, so those tools would truthfully report
//! `application.transport.unavailable`. This composition mounts the same
//! retained owner the daemon registers at project open — the invocation
//! service, the server's retained surface ports, and the project retained
//! grant — behind an in-process executor, so retained recall behavior stays
//! testable against the real owner rather than a stub.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracedecay_lsp::LspSessionRegistry;

use super::project_open_owners::{
    daemon_owned_project_source_access_at, project_open_retained_grant,
};
use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use tracedecay_code_index_runtime::resolved_scope_for_project;
use tracedecay_daemon_protocol::invocation_now_micros;
use tracedecay_daemon_service::{DaemonInvocationService, DaemonRetainedRuntimeRegistrar};
use tracedecay_domain::errors::{Result, TraceDecayError};

#[derive(Clone)]
struct RetainedOwnerTestExecutor {
    service: DaemonInvocationService,
    lsp_registry: Arc<Mutex<LspSessionRegistry>>,
    project_root: PathBuf,
}

impl tracedecay_application::ApplicationInvocationExecutor for RetainedOwnerTestExecutor {
    fn invoke(
        &self,
        _invocation: tracedecay_application::ApplicationInvocation,
    ) -> tracedecay_application::ApplicationInvocationFuture<
        '_,
        std::result::Result<
            tracedecay_application::ApplicationResponse,
            tracedecay_application::InvocationError,
        >,
    > {
        Box::pin(async { Err(tracedecay_application::InvocationError::Unavailable) })
    }
}

impl tracedecay_daemon_protocol::DaemonInvocationExecutor for RetainedOwnerTestExecutor {
    fn invoke_controlled(
        &self,
        request: tracedecay_daemon_protocol::DaemonInvocationRequest,
        deadline: tracedecay_application::Deadline,
        cancellation: tracedecay_application::CancellationSignal,
        _policy: tracedecay_daemon_protocol::InvocationCancellationPolicy,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            tracedecay_daemon_protocol::DaemonInvocationResponse,
            tracedecay_daemon_protocol::DaemonInvocationError,
        >,
    > {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(
                    tracedecay_daemon_protocol::DaemonInvocationError::Cancelled {
                        stage: tracedecay_application::CancellationStage::BeforeAdmission,
                    },
                );
            }
            if tracedecay_daemon_protocol::deadline_remaining(&deadline).is_none() {
                return Err(
                    tracedecay_daemon_protocol::DaemonInvocationError::TimedOut {
                        stage: tracedecay_application::CancellationStage::BeforeAdmission,
                    },
                );
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
        _subject_digest: tracedecay_domain::ManifestDigest,
        _observed_at: tracedecay_domain::UtcMicros,
        _event: tracedecay_application::feedback::observations::FeedbackSourceEventV1,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

/// In-process transport for the project retained owner: the invocation
/// service the owner registers with and the executor the server dispatches
/// through. Both halves must share one service or dispatch resolves no owner.
pub(crate) struct ProjectRetainedOwnerTransport {
    pub(crate) service: DaemonInvocationService,
    pub(crate) executor: Arc<dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
}

/// Builds the in-process retained transport for `project_root`.
pub(crate) fn project_retained_owner_transport(
    project_root: &std::path::Path,
) -> Result<ProjectRetainedOwnerTransport> {
    let project_root = project_root.canonicalize()?;
    let resident_memory = Arc::new(
        tracedecay_runtime_core::resident_memory::ProcessResidentMemoryV1::new(
            tracedecay_runtime_core::resident_memory::DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
        ),
    );
    let service = DaemonInvocationService::with_code_index_schedulers(
        CodeIndexSchedulerRegistryV1::with_resident_memory(1, resident_memory),
    );
    let executor = RetainedOwnerTestExecutor {
        service: service.clone(),
        lsp_registry: Arc::new(Mutex::new(LspSessionRegistry::default())),
        project_root,
    };
    Ok(ProjectRetainedOwnerTransport {
        service,
        executor: Arc::new(executor),
    })
}

/// Registers `server`'s project retained owner on `service`, mirroring the
/// daemon's project-open registration.
pub(crate) async fn register_project_retained_owner_for_test(
    service: &DaemonInvocationService,
    server: &crate::mcp::McpServer,
) -> Result<()> {
    let graph = server.cg().await;
    let project_root = graph.project_root().canonicalize()?;
    let project_id = graph
        .store_layout()
        .identity
        .project_id
        .clone()
        .ok_or_else(|| TraceDecayError::Config {
            message: "retained test owner requires a registered project identity".to_owned(),
        })?;
    let project_id =
        tracedecay_domain::ProjectId::new(project_id).map_err(|error| TraceDecayError::Config {
            message: format!("retained test owner project id is invalid: {error}"),
        })?;
    let scope = resolved_scope_for_project(&project_root, &project_id).map_err(|error| {
        TraceDecayError::Config {
            message: format!("retained test owner scope is invalid: {error}"),
        }
    })?;
    let observed_at = invocation_now_micros();
    let configuration = graph
        .configuration_runtime()
        .client()
        .current()
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("retained test owner configuration is unavailable: {error}"),
        })?;
    let access =
        daemon_owned_project_source_access_at(&scope, &project_root, &configuration, observed_at)
            .map_err(|error| TraceDecayError::Config {
            message: format!("retained test owner access is invalid: {error}"),
        })?;
    let grant = project_open_retained_grant(&access, observed_at).map_err(|error| {
        TraceDecayError::Config {
            message: format!("retained test owner grant is invalid: {error}"),
        }
    })?;
    let ports = server.retained_surface_ports(
        &project_root,
        scope.project_id.clone(),
        access.configuration_digest.clone(),
    );
    DaemonRetainedRuntimeRegistrar::new(service)
        .register(project_root, scope, access.requester, grant, ports)
        .await
}

/// Constructs an MCP server from `context` with the project retained owner
/// mounted in process, mirroring the daemon's project-open registration.
#[cfg(test)]
pub(crate) async fn mcp_server_with_project_retained_owner_for_test(
    context: crate::mcp::server::McpServerConstructionContext,
) -> Result<Arc<crate::mcp::McpServer>> {
    let transport = project_retained_owner_transport(context.cg.project_root())?;
    let context = context.with_application_invocation_executor(Arc::clone(&transport.executor));
    let server = crate::mcp::McpServer::new_with_context(context).await;
    register_project_retained_owner_for_test(&transport.service, server.as_ref()).await?;
    Ok(server)
}
