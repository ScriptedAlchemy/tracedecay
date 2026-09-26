//! The project's graph-tool owner: the daemon invocation service computes
//! graph and port reads through the serving MCP server's admitted
//! authorities.

use std::path::Path;
use std::sync::{Arc, Weak};

use tracedecay_contracts::ResolvedScope;
use tracedecay_daemon_service::{
    DaemonInvocationService, GraphToolFuture, GraphToolInvocationV1, ProjectGraphToolPortV1,
    RegisteredGraphToolOwnerV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};

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
        let options = ToolCallRegistryOptions {
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
            code_index_ignored_dependency_admission: self
                .code_index_ignored_dependency_admission
                .clone(),
            ..ToolCallRegistryOptions::default()
        };
        compute_graph_tool_for_owner(
            cg.as_ref(),
            invocation.operation,
            serde_json::Value::Object(invocation.arguments),
            self.scope_prefix(),
            options,
        )
        .await
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
