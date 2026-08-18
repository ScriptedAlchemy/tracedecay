//! Project-route selection, tool-dispatch assembly, and identical-read sharing.

use super::*;
use crate::mcp::tools::{ToolCallRegistryOptions, handle_tool_call_with_registry_options};

use super::super::read_coalescing::{ReadFlightClaim, tool_allows_identical_read_coalescing};

impl McpServer {
    pub(super) async fn route_tool_arguments(
        &self,
        id: &Value,
        tool_name: &str,
        arguments: Value,
        route_cache: &HookProjectRouteCache,
        initialize_route: Option<&crate::mcp::project_route::WorkspaceProjectRoute>,
        memory_request_scope: &str,
    ) -> Result<RoutedToolCall> {
        let private_reader =
            crate::mcp::tools::tool_dispatches_registered_project_reader(tool_name);
        let cached_private_route = private_reader
            .then(|| route_cache.workspace_route_for_arguments(&arguments))
            .flatten();
        if private_reader
            && cached_private_route.is_none()
            && crate::mcp::project_route::arguments_have_structural_route_identity(&arguments)
        {
            return Err(TraceDecayError::project_route(
                "project_route_not_found",
                false,
                "explicit session or thread identity has no registered private project route",
            ));
        }
        let private_route = cached_private_route.or(initialize_route);
        let mut handler_arguments = arguments;
        let routed_project = match private_route {
            Some(_)
                if crate::mcp::project_route::arguments_have_project_selector(
                    &handler_arguments,
                ) =>
            {
                return Err(TraceDecayError::project_route(
                    "project_route_invalid_selector",
                    false,
                    "a private hook or initialize route cannot be overridden by caller project selectors",
                ));
            }
            Some(crate::mcp::project_route::WorkspaceProjectRoute::Resolved(route)) => {
                Some(route.as_ref().clone())
            }
            Some(crate::mcp::project_route::WorkspaceProjectRoute::Failed(failure)) => {
                return Err(failure.clone().into_error());
            }
            None => None,
        };
        if crate::analytics::is_skill_view_tool(tool_name)
            && let Some(request_id) = json_rpc_request_id_string(id)
            && let Some(map) = handler_arguments.as_object_mut()
        {
            map.insert("__mcp_request_id".to_string(), json!(request_id));
        }
        if tool_supports_live_cancellation(tool_name)
            && let Some(map) = handler_arguments.as_object_mut()
        {
            map.remove("__mcp_request_id");
            if let Some(request_id) = application_surface_request_id(id, memory_request_scope) {
                map.insert("__mcp_request_id".to_owned(), json!(request_id));
            }
        }
        let selected_project = match routed_project {
            Some(project) => Some(project),
            None => {
                crate::mcp::tools::handlers::resolve_registered_project_route_for_tool(
                    tool_name.to_owned(),
                    handler_arguments.clone(),
                    self.registry_db.as_deref(),
                    self.retained_project_server_resolver.clone(),
                )
                .await?
            }
        };
        let selected_server = selected_project
            .as_ref()
            .map(crate::mcp::project_route::ResolvedProjectRoute::retained_server)
            .transpose()?;
        Ok(RoutedToolCall {
            arguments: handler_arguments,
            selected_project,
            selected_server,
        })
    }

    #[cfg(feature = "test-transport")]
    #[doc(hidden)]
    pub async fn call_tool_for_test(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<ToolResult> {
        let route_cache = HookProjectRouteCache::default();
        let routed = self
            .route_tool_arguments(
                &Value::Null,
                tool_name,
                arguments,
                &route_cache,
                None,
                "test-transport",
            )
            .await?;
        let dispatch_server = routed.selected_server.as_deref().unwrap_or(self);
        let cg = dispatch_server.cg().await;
        let application_invocation_target =
            invocation_target_for_route(routed.selected_project.as_ref());
        let ApplicationSurfaceDispatch {
            invocation_executor: application_invocation_executor,
            ..
        } = dispatch_server
            .prepare_application_surface_dispatch(&cg, tool_name)
            .await;
        dispatch_server
            .execute_tool_dispatch(
                cg.as_ref(),
                tool_name,
                routed.arguments,
                routed.selected_project.as_ref(),
                None,
                application_invocation_executor,
                application_invocation_target,
                None,
                None,
                None,
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn dispatch_routed_tool_call(
        &self,
        tool_name: &str,
        routed: RoutedToolCall,
        timings_enabled: bool,
        publish_activity: bool,
        application_request_id: Option<tracedecay_application::RequestId>,
        dispatch_control: DispatchControl,
    ) -> DispatchedToolCall {
        let handler_start = timings_enabled.then(std::time::Instant::now);
        let selected_owner = routed
            .selected_project
            .as_ref()
            .map(|selected| selected.owner.clone());
        let selected_scope = routed
            .selected_project
            .as_ref()
            .map(|selected| selected.scope.clone());
        // The parent admits this method on the exact server retained in
        // `routed.selected_server`; an unselected call is admitted on `self`.
        // Never resolve or fall back to another project inside the worker.
        let dispatch_server = self;
        let (cg, live_branch) = dispatch_server.reopen_if_branch_drifted_memoized().await;
        let project_reader_preselected = routed.selected_project.is_some();
        let application_invocation_target =
            invocation_target_for_route(routed.selected_project.as_ref());

        dispatch_server
            .begin_tool_dispatch(
                tool_name,
                &cg,
                &live_branch,
                project_reader_preselected,
                publish_activity,
            )
            .await;
        let server_stats = if tool_name == "tracedecay_status" {
            Some(dispatch_server.server_stats_json().await)
        } else {
            None
        };
        let ApplicationSurfaceDispatch {
            invocation_executor: application_invocation_executor,
            ..
        } = dispatch_server
            .prepare_application_surface_dispatch(&cg, tool_name)
            .await;
        let outcome = dispatch_server
            .execute_tool_dispatch(
                &cg,
                tool_name,
                routed.arguments,
                routed.selected_project.as_ref(),
                server_stats,
                application_invocation_executor,
                application_invocation_target,
                application_request_id.clone(),
                Some(dispatch_control.deadline()),
                Some(dispatch_control.cancellation()),
            )
            .await;
        DispatchedToolCall {
            cg,
            selected_owner,
            selected_scope,
            outcome,
            elapsed_us: handler_start.map(|t| t.elapsed().as_micros() as u64),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_tool_dispatch(
        &self,
        cg: &TraceDecay,
        tool_name: &str,
        handler_arguments: Value,
        resolved_project_route: Option<&crate::mcp::project_route::ResolvedProjectRoute>,
        server_stats: Option<Value>,
        application_invocation_executor: Option<
            &dyn crate::daemon_client::DaemonInvocationExecutor,
        >,
        application_invocation_target: tracedecay_application::InvocationTarget,
        application_request_id: Option<tracedecay_application::RequestId>,
        application_deadline: Option<tracedecay_application::Deadline>,
        application_cancellation: Option<tracedecay_application::CancellationSignal>,
    ) -> Result<ToolResult> {
        let engine_identity = cg.db_path();
        let read_flight = tool_allows_identical_read_coalescing(tool_name).then(|| {
            self.identical_read_coalescer.claim(
                engine_identity.to_string_lossy().as_ref(),
                tool_name,
                &handler_arguments,
                self.scope_prefix(),
            )
        });
        let session_sync_service = self
            .session_sync_service
            .as_ref()
            .and_then(std::sync::Weak::upgrade);
        let dispatch: std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + '_>,
        > = handle_tool_call_with_registry_options(
            cg,
            tool_name,
            handler_arguments,
            server_stats,
            self.scope_prefix(),
            ToolCallRegistryOptions {
                global_db: self.registry_db.as_ref(),
                project_registry_reads: self.project_registry_reads.as_deref(),
                accounting_db: self.accounting_db.as_deref(),
                registered_project_session_db: self.registered_session_db.clone(),
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
                profile_root: self.profile_root.as_deref(),
                resolved_project_route,
                automation_scheduler_reconciler: self.automation_scheduler_reconciler.clone(),
                automation_writer: self.dashboard_automation_writer.clone(),
                doctor_report_reader: self.dashboard_doctor_report_reader.clone(),
                remote_operational_status: self.remote_operational_status.clone(),
                code_index_freshness_reader: self.dashboard_code_index_freshness_reader.clone(),
                explorer_semantic_reader: self.dashboard_explorer_semantic_reader.clone(),
                feedback_status_reader: self.dashboard_feedback_status_reader.clone(),
                diagnostics_cache: Some(&self.diagnostics_cache),
                diagnostics_lsp: Some(Arc::clone(&self.diagnostics_lsp)),
                application_invocation_executor,
                application_invocation_target,
                dashboard_application_invocation_executor: self
                    .application_invocation_executor
                    .clone(),
                daemon_invocation_service: self.daemon_invocation_service.as_ref(),
                dashboard_delivery_settlement_authority: self.delivery_settlement_authority.clone(),
                application_request_id,
                application_deadline,
                application_cancellation,
                code_index_publication_identity: self.code_index_publication_identity.clone(),
                code_index_reconcile_sink: self.code_index_reconcile_sink.clone(),
                code_index_search_executor: self.code_index_search_executor.clone(),
                code_index_branch_diff_executor: self.code_index_branch_diff_executor.clone(),
                source_edit_executor: self.source_edit_executor.get().cloned(),
                source_edit_reconciliation_executor: self
                    .source_edit_reconciliation_executor
                    .get()
                    .cloned(),
                source_edit_rollback_executor: self.source_edit_rollback_executor.get().cloned(),
                code_index_search_authority: self.code_index_search_authority.clone(),
                code_graph_projection_read_port: self.code_graph_projection_read_port.clone(),
                code_graph_read_admission_port: self.code_graph_read_admission_port.clone(),
                code_index_ignored_dependency_admission: self
                    .code_index_ignored_dependency_admission
                    .clone(),
                generation_census_reader: self.generation_census_reader(),
                retained_project_server_resolver: self.retained_project_server_resolver.clone(),
                session_sync_service: session_sync_service.as_deref(),
                session_authorities: crate::mcp::tools::SessionAuthorities::new(
                    self.session_db.as_ref(),
                    self.user_session_db.as_ref(),
                )
                .with_profile_identity(self.profile_identity.as_ref())
                .with_profile_retained_authority(self.profile_retained_authority.as_ref())
                .with_registered_databases(
                    self.registered_session_db.as_ref(),
                    self.registered_user_session_db.as_ref(),
                )
                .with_lcm_authorities(
                    self.project_lcm_authority.as_deref(),
                    self.user_lcm_authority.as_deref(),
                ),
            },
        );
        if let Some(read_flight) = read_flight {
            match read_flight {
                ReadFlightClaim::Leader(leader) => match dispatch.await {
                    Ok(result) => Ok((*leader.complete(result)).clone()),
                    Err(error) => Err(error),
                },
                ReadFlightClaim::Follower(follower) => match follower.wait().await {
                    Some(result) => Ok((*result).clone()),
                    None => dispatch.await,
                },
            }
        } else {
            dispatch.await
        }
    }
}
