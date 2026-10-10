//! Verified file-inventory admission for `tracedecay://files`.

use serde_json::Value;
use tracedecay_contracts::RequestId;
use tracedecay_contracts::graph_tool::GraphToolResultV1;
use tracedecay_contracts::retrieval::{
    CallableCodeOperationKind, FilesLayoutV1, FilesSurfaceRequestV1, ServedCodeGraphGenerationV1,
    callable_code_operation,
};
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_graph_query::VerifiedGraphQueryRequest;
use tracedecay_mcp::handlers::graph::{freshness_lines, graph_read_freshness};
use tracedecay_mcp::handlers::info::{compute_files, render_files_md};
use tracedecay_mcp::server::DispatchControl;
use tracedecay_runtime_core::cancellation::CancellationToken;

use super::{JsonRpcResponse, McpServer};

const FILES_RESOURCE_URI: &str = "tracedecay://files";
const FILES_RESOURCE_MIME: &str = "text/plain";

impl McpServer {
    /// Admits the verified generation's file inventory for the active project.
    ///
    /// The resource uses the same source-metadata graph operation, request
    /// identity, deadline, and cancellation the `tracedecay_files` tool
    /// carries. A missing port, cancelled or expired admission, or an
    /// unready generation stays a typed unavailable body. The listing is
    /// never backfilled from a raw or stale store.
    pub(crate) async fn read_resource_files(
        &self,
        id: Value,
        connection_scope: &str,
        transport_cancellation: CancellationToken,
    ) -> JsonRpcResponse {
        let prepared = match self.prepare_dispatch_control(
            &id,
            "tracedecay_files",
            connection_scope,
            transport_cancellation.is_cancelled(),
            None,
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                return Self::resource_contents(
                    id,
                    FILES_RESOURCE_URI,
                    FILES_RESOURCE_MIME,
                    &files_resource_from_error(&error),
                );
            }
        };
        let Some(request_id) = prepared.request_id.clone() else {
            return Self::resource_contents(
                id,
                FILES_RESOURCE_URI,
                FILES_RESOURCE_MIME,
                &files_resource_unavailable("authority_unavailable"),
            );
        };
        let worker_server = self.dispatch_authority.server();
        let worker_control = prepared.control.clone();
        let worker = async move {
            let server = worker_server.upgrade().ok_or_else(|| {
                TraceDecayError::project_route(
                    "tool_dispatch_shutdown",
                    true,
                    "MCP server was released before file inventory admission",
                )
            })?;
            server
                .admit_verified_file_inventory(request_id, worker_control)
                .await
        };
        let outcome = {
            let dispatch = prepared
                .control
                .run_retained(self.dispatch_authority.registry(), worker);
            tokio::pin!(dispatch);
            tokio::select! {
                biased;
                () = transport_cancellation.cancelled() => {
                    let _ = prepared.control.cancellation().cancel(tracedecay_contracts::now_micros());
                    dispatch.await
                }
                outcome = &mut dispatch => outcome,
            }
        };
        let text = match outcome.result {
            Ok(text) => text,
            Err(failure) => files_resource_from_error(failure.error()),
        };
        drop(prepared);
        Self::resource_contents(id, FILES_RESOURCE_URI, FILES_RESOURCE_MIME, &text)
    }

    async fn admit_verified_file_inventory(
        &self,
        request_id: RequestId,
        control: DispatchControl,
    ) -> Result<String, TraceDecayError> {
        let port = self.verified_graph_query_port.as_deref().ok_or_else(|| {
            TraceDecayError::project_route(
                "verified-code-graph-read-unavailable",
                false,
                "the exact project verified graph query is not mounted",
            )
        })?;
        let cancellation = control.cancellation();
        let operation = callable_code_operation(CallableCodeOperationKind::SourceMetadata)
            .map_err(|error| TraceDecayError::Config {
                message: format!("could not resolve file inventory operation: {error}"),
            })?;
        let graph = port
            .open(VerifiedGraphQueryRequest::new(
                &operation,
                request_id,
                control.deadline(),
                &cancellation,
            ))
            .await?;
        if cancellation.is_cancelled() {
            return Err(TraceDecayError::project_route(
                "cancelled",
                true,
                "file inventory admission was cancelled",
            ));
        }

        let cg = self.reopen_if_branch_drifted().await;
        let freshness_payload = if let Some(reader) =
            self.dashboard_code_index_freshness_reader.as_ref()
        {
            match reader(cg.project_root().to_path_buf()).await {
                Ok(worktree) => Some(
                    tracedecay_contracts::code_index_freshness::CodeIndexFreshnessPayloadV1::from_scheduler_read(
                        worktree,
                    ),
                ),
                Err(failure) => Some(
                    tracedecay_contracts::code_index_freshness::CodeIndexFreshnessPayloadV1::from_read_failure(
                        failure,
                    ),
                ),
            }
        } else {
            None
        };
        let worktree_omitted_sources = freshness_payload.as_ref().and_then(|payload| {
            payload
                .worktrees
                .first()
                .filter(|worktree| {
                    worktree.latest_generation_id.as_deref() == Some(graph.generation().as_str())
                })
                .and_then(|worktree| worktree.omitted_sources.clone())
        });
        let completion = compute_files(
            &graph,
            FilesSurfaceRequestV1 {
                path: None,
                pattern: None,
                layout: Some(FilesLayoutV1::Grouped),
            },
            self.scope_prefix(),
            worktree_omitted_sources,
        )
        .await?;
        let GraphToolResultV1::Files(result) = completion.result else {
            return Err(TraceDecayError::Config {
                message: "file inventory operation returned another graph result".to_owned(),
            });
        };
        let freshness = graph_read_freshness(
            &ServedCodeGraphGenerationV1 {
                generation: graph.generation().as_str().to_owned(),
                freshness: graph.freshness(),
                worktree: None,
            },
            freshness_payload.as_ref(),
        );
        let mut text = freshness_lines(&freshness);
        text.push_str(&render_files_md(&result));
        Ok(text)
    }
}

fn files_resource_unavailable(reason: &str) -> String {
    format!("status: unavailable\nreason: {reason}")
}

fn files_resource_from_error(error: &TraceDecayError) -> String {
    files_resource_unavailable(files_resource_reason(error))
}

fn files_resource_reason(error: &TraceDecayError) -> &'static str {
    match error.project_route_context() {
        Some((reason_code, _, _)) => files_resource_reason_code(reason_code),
        None => "authority_unavailable",
    }
}

fn files_resource_reason_code(reason_code: &str) -> &'static str {
    match reason_code {
        "verified-code-graph-read-unavailable" => "authority_unavailable",
        "store_open_cancelled" | "cancelled" => "cancelled",
        "timed_out" | "deadline_exceeded" => "timed_out",
        "generation_unavailable" => "generation_unavailable",
        "generation_unverified" => "generation_unverified",
        "graph_warming" => "graph_warming",
        _ if reason_code.contains("cancel") => "cancelled",
        _ if reason_code.contains("deadline") || reason_code.contains("timeout") => "timed_out",
        _ if reason_code.contains("warming") => "graph_warming",
        _ if reason_code.contains("unverified") => "generation_unverified",
        _ if reason_code.contains("generation") => "generation_unavailable",
        _ => "authority_unavailable",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use tracedecay_mcp::server::McpDispatchRequest;
    use tracedecay_mcp::transport::JsonRpcRequest;
    use tracedecay_runtime_core::cancellation::CancellationToken;

    use super::{McpServer, files_resource_reason, files_resource_unavailable};
    use tracedecay_domain::errors::TraceDecayError;

    async fn ready_files_server() -> (
        tempfile::TempDir,
        crate::daemon::ProductionProjectCompositionHarnessV1,
        Arc<McpServer>,
    ) {
        tracedecay_project::product_runtime::register_fixture_product_runtime();
        let isolation = tempfile::TempDir::new().unwrap();
        let root = isolation.path().join("project");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn resource_value() -> u32 { 7 }\n",
        )
        .unwrap();
        let git = super::super::writer_test_support::git;
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.email", "files@test.invalid"]);
        git(&root, &["config", "user.name", "Files Test"]);
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "fixture"]);
        let harness = Box::pin(crate::daemon::ProductionProjectCompositionHarnessV1::open(
            isolation.path(),
            [root.clone()],
        ))
        .await
        .unwrap();
        let status = harness
            .call_tool(
                &root,
                "tracedecay_status",
                json!({
                    "format": "json",
                    "wait_for": { "state": "ready", "timeout_ms": 20_000 },
                }),
            )
            .await
            .unwrap();
        assert!(status.error.is_none(), "{status:?}");
        let server = harness.server(&root).unwrap();
        let response = server.handle_request(&files_request()).await.unwrap();
        assert!(
            resource_text(&response).contains("lib.rs ("),
            "{response:?}"
        );
        (isolation, harness, server)
    }

    fn files_request() -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: Some(json!(420)),
            method: "resources/read".to_owned(),
            params: Some(json!({ "uri": "tracedecay://files" })),
        }
    }

    fn resource_text(response: &super::JsonRpcResponse) -> &str {
        assert!(response.error.is_none(), "{response:?}");
        response.result.as_ref().unwrap()["contents"][0]["text"]
            .as_str()
            .unwrap()
    }

    #[tokio::test]
    async fn a_pre_cancelled_files_resource_returns_cancelled() {
        let (_isolation, harness, server) = ready_files_server().await;
        let mut connection = server.new_connection_route_state().unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let request = files_request();
        let response = server
            .dispatch_envelope(
                McpDispatchRequest::raw(&request),
                false,
                &mut connection,
                cancellation,
            )
            .await
            .unwrap();
        assert_eq!(
            resource_text(&response),
            "status: unavailable\nreason: cancelled"
        );
        harness.shutdown().await;
    }

    async fn cancel_registered_files_resource(transport_cancel: bool) {
        let (_isolation, harness, server) = ready_files_server().await;
        let mut connection = server.new_connection_route_state().unwrap();
        let scope = connection.memory_request_scope().to_owned();
        let other_connection = server.new_connection_route_state().unwrap();
        let cancellation = CancellationToken::new();
        let request = files_request();
        let registered = server
            .dispatch_authority
            .cancellation_registered()
            .notified();
        tokio::pin!(registered);
        registered.as_mut().enable();
        let read = server.dispatch_envelope(
            McpDispatchRequest::raw(&request),
            false,
            &mut connection,
            cancellation.clone(),
        );
        tokio::pin!(read);
        tokio::select! {
            biased;
            response = &mut read => panic!("resource settled before registering cancellation: {response:?}"),
            () = &mut registered => {},
        }
        assert!(!server.cancel_application_surface_request(
            &json!(420),
            other_connection.memory_request_scope(),
        ));
        if transport_cancel {
            cancellation.cancel();
        } else {
            assert!(server.cancel_application_surface_request(&json!(420), &scope));
        }
        let response = read.await.unwrap();
        assert_eq!(
            resource_text(&response),
            "status: unavailable\nreason: cancelled"
        );
        assert!(!server.cancel_application_surface_request(&json!(420), &scope));
        harness.shutdown().await;
    }

    #[tokio::test]
    async fn an_in_flight_files_resource_observes_connection_scoped_cancellation() {
        cancel_registered_files_resource(false).await;
    }

    #[tokio::test]
    async fn an_in_flight_files_resource_observes_transport_cancellation() {
        cancel_registered_files_resource(true).await;
    }

    #[test]
    fn missing_graph_authority_stays_typed_unavailable() {
        let error = TraceDecayError::project_route(
            "verified-code-graph-read-unavailable",
            false,
            "the exact project verified graph query is not mounted",
        );
        assert_eq!(files_resource_reason(&error), "authority_unavailable");
        assert_eq!(
            files_resource_unavailable("authority_unavailable"),
            "status: unavailable\nreason: authority_unavailable"
        );
    }

    #[test]
    fn converging_and_cancelled_admissions_keep_their_typed_reasons() {
        assert_eq!(
            files_resource_reason(&TraceDecayError::project_route(
                "generation_unavailable",
                true,
                "the verified generation is still converging",
            )),
            "generation_unavailable"
        );
        assert_eq!(
            files_resource_reason(&TraceDecayError::project_route(
                "graph_warming",
                true,
                "the admitted generation is reopening",
            )),
            "graph_warming"
        );
        assert_eq!(
            files_resource_reason(&TraceDecayError::project_route(
                "store_open_cancelled",
                true,
                "the caller cancelled before admission finished",
            )),
            "cancelled"
        );
    }
}
