//! The project's graph-tool owner: the daemon invocation service computes
//! graph and port reads through the serving MCP server's admitted
//! authorities.

use std::path::Path;
use std::sync::{Arc, Weak};

use tracedecay_contracts::ApplicationProblem;
use tracedecay_contracts::ResolvedScope;
use tracedecay_contracts::graph_tool::GraphToolResultV1;
use tracedecay_contracts::retrieval::HookRuntimeResultV1;
use tracedecay_daemon_service::{
    DaemonInvocationService, GraphToolFuture, GraphToolInvocationV1, ProjectGraphToolPortV1,
    RegisteredGraphToolOwnerV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_mcp::handlers::graph_tool::graph_tool_owner_stores;
use tracedecay_mcp::server::join_hook_ingest_refresh;
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, OwnerStoresV1};

use super::McpServer;
use crate::mcp::tools::{
    ToolCallRegistryOptions, compute_graph_tool_for_owner, graph_tool_error_problem,
};

struct McpGraphToolPort {
    server: Weak<McpServer>,
}

impl ProjectGraphToolPortV1 for McpGraphToolPort {
    fn execute(&self, invocation: GraphToolInvocationV1) -> GraphToolFuture<'_> {
        Box::pin(async move {
            let Some(server) = self.server.upgrade() else {
                return Err(graph_tool_error_problem(&TraceDecayError::project_route(
                    "tool_dispatch_shutdown",
                    true,
                    "the MCP server was released before the graph read was admitted",
                )));
            };
            // Project open registers the core server as owner before the
            // full server, which mounts the session stores and session sync
            // owner, replaces it. A call that needs them meanwhile is still
            // mounting.
            if server.project_session_db.is_none()
                && graph_tool_owner_stores(invocation.operation, &invocation.arguments)
                    == OwnerStoresV1::ProjectSessions
            {
                return Err(ApplicationProblem::runtime_mounting());
            }
            server
                .compute_graph_tool(invocation)
                .await
                .map_err(|error| graph_tool_error_problem(&error))
        })
    }
}

impl McpServer {
    async fn compute_graph_tool(
        &self,
        invocation: GraphToolInvocationV1,
    ) -> Result<tracedecay_contracts::graph_tool::GraphToolCompletionV1> {
        let (cg, _live_branch) = self.reopen_if_branch_drifted_memoized().await;
        let server_stats = match invocation.operation {
            ApplicationSurfaceOperation::Status => Some(self.server_stats_json().await),
            _ => None,
        };
        let session_sync_service = self
            .session_sync_service
            .as_ref()
            .and_then(std::sync::Weak::upgrade);
        let options = ToolCallRegistryOptions {
            global_db: self.registry_db.as_ref(),
            accounting_db: self.accounting_db.as_deref(),
            profile: self.profile.as_ref(),
            session_authorities: tracedecay_mcp::handlers::SessionAuthorities::new(
                self.project_session_db.as_ref(),
                self.profile_session_db.as_ref(),
            )
            .with_profile_identity(self.profile_identity.clone())
            .with_background_cpu(self.background_cpu.clone())
            .with_project_lcm_authority(self.project_lcm_authority.as_deref()),
            session_sync_service: session_sync_service.as_deref(),
            server_stats,
            code_index_readiness_waiter: self.code_index_readiness_waiter.clone(),
            generation_census_reader: self.generation_census_reader(),
            registered_project_session_db: self.project_session_db.clone(),
            application_request_id: Some(invocation.request_id),
            application_deadline: Some(invocation.deadline),
            application_cancellation: Some(invocation.cancellation),
            code_index_search_executor: self.code_index_search_executor.clone(),
            code_index_similar_executor: self.code_index_similar_executor.clone(),
            code_index_redundancy_executor: self.code_index_redundancy_executor.clone(),
            code_index_branch_diff_executor: self.code_index_branch_diff_executor.clone(),
            code_index_search_authority: self.code_index_search_authority.clone(),
            admitted_project_scope: self.admitted_project_scope.clone(),
            verified_graph_query_port: self.verified_graph_query_port.clone(),
            code_index_freshness_reader: self.dashboard_code_index_freshness_reader.clone(),
            code_index_publication_identity: self.code_index_publication_identity.clone(),
            code_index_reconcile_sink: self.code_index_reconcile_sink.clone(),
            code_index_ignored_dependency_admission: self
                .code_index_ignored_dependency_admission
                .clone(),
            // The dashboard the owner binds composes these daemon-owned
            // readers, writers, and session authorities.
            registered_profile_session_db: self.profile_session_db.clone(),
            registered_savings_db: self.accounting_db.clone(),
            dashboard_session_retrieval_service: self
                .project_application_retrieval
                .as_ref()
                .map(|mounted| Arc::clone(&mounted.service)),
            dashboard_session_retrieval_identity: self
                .project_application_retrieval
                .as_ref()
                .map(|mounted| mounted.identity.clone()),
            daemon_user_profile_id: self
                .profile_identity
                .as_ref()
                .map(|identity| identity.profile_id().clone()),
            automation_scheduler_reconciler: self.automation_scheduler_reconciler.clone(),
            automation_writer: self.dashboard_automation_writer.clone(),
            doctor_report_reader: self.dashboard_doctor_report_reader.get().cloned(),
            remote_operational_status: self.remote_operational_status.clone(),
            feedback_status_reader: self.dashboard_feedback_status_reader.clone(),
            pr_autotrack_reader: self.dashboard_pr_autotrack_reader.clone(),
            diagnostics_lsp: Some(Arc::clone(&self.diagnostics_lsp)),
            dashboard_application_invocation_executor: self.application_invocation_executor.clone(),
            daemon_invocation_service: self.daemon_invocation_service.as_ref(),
            dashboard_delivery_settlement_authority: self.delivery_settlement_authority.clone(),
            code_graph_projection_read_port: self.code_graph_projection_read_port.clone(),
            code_graph_read_admission_port: self.code_graph_read_admission_port.clone(),
            retained_project_server_resolver: self.retained_project_server_resolver.clone(),
            ..ToolCallRegistryOptions::default()
        };
        let computed = compute_graph_tool_for_owner(
            cg.as_ref(),
            invocation.operation,
            serde_json::Value::Object(invocation.arguments),
            self.scope_prefix(),
            options,
        );
        // This server's profile owns every transcript a hook action ingests.
        let completion = match self.profile.clone() {
            Some(profile) => {
                tracedecay_sessions::runtime::with_transcript_source_profile(profile, computed)
                    .await
            }
            None => computed.await,
        }?;
        if let GraphToolResultV1::HookRuntime(HookRuntimeResultV1::IngestTranscript(ingest)) =
            &completion.result
        {
            // The ingest wrote through this server's session stores; answer
            // only once their refresh owner has published it.
            join_hook_ingest_refresh(
                ingest.user_scope,
                self.project_session_refresh_wake.as_deref(),
                self.user_session_refresh_wake.as_deref(),
            )
            .await?;
        }
        Ok(completion)
    }

    /// Registers this server as its project's graph-tool owner, replacing an
    /// earlier server for the same authorized scope.
    pub(crate) async fn register_graph_tool_owner(
        &self,
        project_root: &Path,
        scope: ResolvedScope,
    ) -> Result<()> {
        let Some(service) = self.daemon_invocation_service() else {
            return Err(TraceDecayError::Config {
                message: "the graph-tool owner requires the daemon invocation service".to_owned(),
            });
        };
        self.register_graph_tool_owner_on(service, project_root, scope)
            .await
    }

    /// Registers this server as the graph-tool owner on `service`, the
    /// invocation service that routes this project's graph reads.
    pub(crate) async fn register_graph_tool_owner_on(
        &self,
        service: &DaemonInvocationService,
        project_root: &Path,
        scope: ResolvedScope,
    ) -> Result<()> {
        let port: Arc<dyn ProjectGraphToolPortV1> = Arc::new(McpGraphToolPort {
            server: self.dispatch_authority.server(),
        });
        service
            .register_graph_tool_owner(
                project_root.to_path_buf(),
                RegisteredGraphToolOwnerV1::new(scope, port),
            )
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!("the graph-tool owner failed to register: {error}"),
            })
    }
}
