//! Root type aliases for the transport-owned `rmcp` adapter. Production
//! composes `tracedecay_mcp::server::RmcpConnectionAdapter` directly over
//! `ProductionMcpConnectionContext` in `daemon::connection_serving`.

#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use rmcp::RoleServer;
#[cfg(test)]
use tracedecay_mcp::server::RmcpConnectionAdapter;
#[cfg(test)]
use tracedecay_mcp::transport::JsonRpcResponse;

#[cfg(test)]
use super::McpServer;
#[cfg(test)]
use super::connection::ProductionMcpConnectionContext;
#[cfg(test)]
use super::routing::SelectedProjectResponseLease;

pub(crate) type RmcpInitializeResponseDecorator =
    tracedecay_mcp::server::RmcpInitializeResponseDecorator;
#[cfg(test)]
pub(crate) type RmcpSelectedProjectResponseAuthority =
    tracedecay_mcp::server::RmcpSelectedProjectResponseAuthority<SelectedProjectResponseLease>;
#[cfg(test)]
pub(crate) type RmcpWorkDeliverySettlement = tracedecay_mcp::server::RmcpWorkDeliverySettlement;

#[cfg(test)]
use tracedecay_mcp::server::{
    await_dispatch_with_cancellation, project_server_retired_error, rmcp_response_result,
};

#[cfg(test)]
use rmcp::model::{ErrorCode, InitializeResult, ListToolsResult};
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use tracedecay_mcp::transport::JsonRpcRequest;

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rmcp::model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ReadResourceRequestParams,
    };
    use rmcp::service::ServiceRole;
    use rmcp::transport::{IntoTransport, Transport};
    use rmcp::{RoleClient, ServiceExt};
    use serde::Serialize;
    use serde_json::json;

    use super::*;

    /// The same composition `daemon::connection_serving` performs.
    fn production_adapter(
        server: &Arc<McpServer>,
        initialize_response_decorator: Option<RmcpInitializeResponseDecorator>,
    ) -> RmcpConnectionAdapter<ProductionMcpConnectionContext> {
        RmcpConnectionAdapter::new(
            ProductionMcpConnectionContext::new(Arc::clone(server)),
            false,
            initialize_response_decorator,
            server.delivery_settlement_recorder.clone(),
        )
        .expect("RMCP adapter")
    }

    struct RecordingTransport<R, T>
    where
        R: ServiceRole,
    {
        inner: T,
        messages: Arc<std::sync::Mutex<Vec<Value>>>,
        _role: std::marker::PhantomData<R>,
    }

    impl<R, T> RecordingTransport<R, T>
    where
        R: ServiceRole,
    {
        fn new(inner: T, messages: Arc<std::sync::Mutex<Vec<Value>>>) -> Self {
            Self {
                inner,
                messages,
                _role: std::marker::PhantomData,
            }
        }
    }

    impl<R, T> Transport<R> for RecordingTransport<R, T>
    where
        R: ServiceRole,
        T: Transport<R> + 'static,
        rmcp::service::TxJsonRpcMessage<R>: Serialize,
    {
        type Error = T::Error;

        fn name() -> Cow<'static, str> {
            "rmcp-wire-recording".into()
        }

        fn send(
            &mut self,
            item: rmcp::service::TxJsonRpcMessage<R>,
        ) -> impl std::future::Future<Output = std::result::Result<(), Self::Error>> + Send + 'static
        {
            let encoded = serde_json::to_value(&item).expect("record RMCP wire message");
            self.messages
                .lock()
                .expect("RMCP wire recording lock")
                .push(encoded);
            self.inner.send(item)
        }

        fn receive(
            &mut self,
        ) -> impl std::future::Future<Output = Option<rmcp::service::RxJsonRpcMessage<R>>> + Send
        {
            self.inner.receive()
        }

        fn close(
            &mut self,
        ) -> impl std::future::Future<Output = std::result::Result<(), Self::Error>> + Send
        {
            self.inner.close()
        }
    }

    struct RmcpWireFixture {
        client: rmcp::service::RunningService<RoleClient, ()>,
        server: Arc<McpServer>,
        client_messages: Arc<std::sync::Mutex<Vec<Value>>>,
        server_messages: Arc<std::sync::Mutex<Vec<Value>>>,
        serving: tokio::task::JoinHandle<()>,
        _repo: tempfile::TempDir,
        _authority: crate::mcp::server::writer_test_support::WriterTestFixtureAuthority,
    }

    impl RmcpWireFixture {
        async fn start() -> Self {
            crate::product_runtime::register_fixture_product_runtime();
            let (cg, repo, authority) =
                crate::mcp::server::writer_test_support::init_indexed_repo().await;
            let context =
                crate::mcp::server::writer_test_support::registered_context(cg, &authority);
            let server = McpServer::new_with_registered_test_context(context, Vec::new())
                .await
                .expect("registered RMCP wire server");
            let adapter = production_adapter(
                &server,
                Some(Arc::new(|response| {
                    response.result.as_mut().expect("initialize result")["_meta"]["tracedecayInitializeRoute"] = json!({
                        "projectPath": "/wire/oracle",
                        "allowInit": false,
                    });
                })),
            );
            let (server_io, client_io) = tokio::io::duplex(2 * 1024 * 1024);
            let server_messages = Arc::new(std::sync::Mutex::new(Vec::new()));
            let server_transport = RecordingTransport::<RoleServer, _>::new(
                IntoTransport::<RoleServer, _, _>::into_transport(server_io),
                Arc::clone(&server_messages),
            );
            let serving = tokio::spawn(async move {
                let running = adapter
                    .serve(server_transport)
                    .await
                    .expect("serve RMCP adapter");
                running.waiting().await.expect("RMCP adapter task");
            });
            let client_messages = Arc::new(std::sync::Mutex::new(Vec::new()));
            let client_transport = RecordingTransport::<RoleClient, _>::new(
                IntoTransport::<RoleClient, _, _>::into_transport(client_io),
                Arc::clone(&client_messages),
            );
            let client = ().serve(client_transport).await.expect("initialize RMCP client");
            Self {
                client,
                server,
                client_messages,
                server_messages,
                serving,
                _repo: repo,
                _authority: authority,
            }
        }

        fn last_request(&self) -> JsonRpcRequest {
            let response_id = self.last_response()["id"].clone();
            let messages = self
                .client_messages
                .lock()
                .expect("client wire recording lock");
            let request = messages
                .iter()
                .rev()
                .find(|message| message.get("id") == Some(&response_id))
                .expect("recorded client request for response");
            serde_json::from_value(request.clone()).expect("legacy request shape")
        }

        fn last_response(&self) -> Value {
            self.server_messages
                .lock()
                .expect("server wire recording lock")
                .last()
                .expect("recorded server response")
                .clone()
        }

        async fn assert_last_response_matches_legacy(&self, decorate_initialize: bool) {
            let request = self.last_request();
            let mut expected = self
                .server
                .handle_request(&request)
                .await
                .expect("legacy response");
            if decorate_initialize {
                expected.result.as_mut().expect("legacy initialize result")["_meta"]["tracedecayInitializeRoute"] = json!({
                    "projectPath": "/wire/oracle",
                    "allowInit": false,
                });
            }
            assert_eq!(
                self.last_response(),
                serde_json::to_value(expected).expect("serialize legacy response"),
            );
        }

        async fn shutdown(mut self) {
            self.client.close().await.expect("close RMCP client");
            self.serving.await.expect("join RMCP server");
            self.server.shutdown().await;
        }
    }

    #[tokio::test]
    async fn malformed_initialize_is_refused_typed_and_a_corrected_handshake_still_serves() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        crate::product_runtime::register_fixture_product_runtime();
        let (cg, repo, authority) =
            crate::mcp::server::writer_test_support::init_indexed_repo().await;
        let context = crate::mcp::server::writer_test_support::registered_context(cg, &authority);
        let server = McpServer::new_with_registered_test_context(context, Vec::new())
            .await
            .expect("registered RMCP handshake server");
        let adapter = production_adapter(&server, None);
        let (server_io, client_io) = tokio::io::duplex(256 * 1024);
        let serving = tokio::spawn(async move {
            let running = adapter
                .serve(IntoTransport::<RoleServer, _, _>::into_transport(server_io))
                .await
                .expect("a malformed initialize must not fail connection initialization");
            let _ = running.waiting().await;
        });

        let (client_read, mut client_write) = tokio::io::split(client_io);
        let mut client_read = tokio::io::BufReader::new(client_read);
        let mut line = String::new();

        client_write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
            .await
            .expect("send malformed initialize");
        client_read
            .read_line(&mut line)
            .await
            .expect("read the typed refusal");
        let refusal: Value = serde_json::from_str(&line).expect("refusal is a JSON-RPC frame");
        assert_eq!(refusal["id"], json!(1), "the refusal answers the sent id");
        assert_eq!(
            refusal["error"]["code"],
            json!(-32602),
            "a malformed initialize is invalid params, not a dropped connection: {line}"
        );

        line.clear();
        client_write
            .write_all(
                br#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"handshake-retry","version":"0"}}}
"#,
            )
            .await
            .expect("send corrected initialize");
        client_read
            .read_line(&mut line)
            .await
            .expect("read the initialize result");
        let initialized: Value =
            serde_json::from_str(&line).expect("initialize is a JSON-RPC frame");
        assert_eq!(initialized["id"], json!(2));
        assert!(
            initialized["result"]["serverInfo"]["name"].is_string(),
            "the connection stayed usable for a corrected handshake: {line}"
        );

        drop(client_write);
        drop(client_read);
        serving.await.expect("join RMCP server");
        server.shutdown().await;
        drop(repo);
    }

    #[tokio::test]
    async fn rmcp_wire_matrix_matches_legacy_initialize_tools_and_resources() {
        let fixture = RmcpWireFixture::start().await;
        let initialize_id = fixture.last_response()["id"].clone();
        let mut initialize_result =
            crate::mcp::server::initialize_result(crate::mcp::server::SERVER_INSTRUCTIONS)
                .expect("initialize oracle");
        initialize_result["protocolVersion"] = json!("2025-11-25");
        initialize_result["_meta"]["tracedecayInitializeRoute"] = json!({
            "projectPath": "/wire/oracle",
            "allowInit": false,
        });
        assert_eq!(
            fixture.last_response(),
            serde_json::to_value(JsonRpcResponse::success(initialize_id, initialize_result))
                .expect("serialize initialize oracle"),
            "rmcp negotiates the client protocol version while preserving the legacy payload",
        );
        assert_eq!(
            fixture.last_response()["result"]["_meta"]["tracedecayInitializeRoute"],
            json!({"projectPath": "/wire/oracle", "allowInit": false}),
            "rmcp InitializeResult must preserve daemon-selected route metadata",
        );

        fixture
            .client
            .list_tools(None)
            .await
            .expect("RMCP tools/list");
        fixture.assert_last_response_matches_legacy(false).await;

        fixture
            .client
            .call_tool(
                CallToolRequestParams::new("tracedecay_status").with_arguments(
                    json!({"admission_only": true, "format": "json"})
                        .as_object()
                        .cloned()
                        .expect("object arguments"),
                ),
            )
            .await
            .expect("RMCP tools/call success");
        assert_eq!(
            fixture.last_response()["result"]["content"][0]["type"],
            json!("text"),
        );
        assert!(
            fixture.last_response()["result"]["content"][0]["text"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "RMCP success must preserve the real handler's text content",
        );

        let handler_error = fixture
            .client
            .call_tool(
                CallToolRequestParams::new("tracedecay_not_a_tool")
                    .with_arguments(serde_json::Map::new()),
            )
            .await
            .expect_err("unknown tool must be a JSON-RPC error");
        fixture.assert_last_response_matches_legacy(false).await;
        assert_eq!(
            fixture.last_response()["error"]["code"],
            json!(-32603),
            "handler error code is a host-visible protocol contract",
        );
        assert!(
            handler_error.to_string().contains("unknown tool"),
            "typed rmcp client must receive the handler error",
        );

        fixture
            .client
            .call_tool(
                CallToolRequestParams::new("tracedecay_changelog").with_arguments(
                    json!({"from_ref": "missing-rmcp-oracle-ref", "to_ref": "HEAD"})
                        .as_object()
                        .cloned()
                        .expect("object arguments"),
                ),
            )
            .await
            .expect("semantic refusal stays a completed tool result");
        assert_eq!(
            fixture.last_response()["result"]["isError"],
            json!(true),
            "typed refusals must remain successful JSON-RPC responses with isError=true",
        );

        fixture
            .client
            .list_resources(None)
            .await
            .expect("RMCP resources/list");
        fixture.assert_last_response_matches_legacy(false).await;

        fixture
            .client
            .read_resource(ReadResourceRequestParams::new("tracedecay://schema"))
            .await
            .expect("RMCP resources/read");
        fixture.assert_last_response_matches_legacy(false).await;

        let unknown_resource = fixture
            .client
            .read_resource(ReadResourceRequestParams::new(
                "tracedecay://not-a-resource",
            ))
            .await
            .expect_err("an unknown resource URI must be a JSON-RPC error");
        fixture.assert_last_response_matches_legacy(false).await;
        assert_eq!(
            fixture.last_response()["error"]["code"],
            json!(-32602),
            "the typed resources/read refusal keeps the legacy invalid-params code",
        );
        assert!(
            unknown_resource
                .to_string()
                .contains("unknown resource URI: tracedecay://not-a-resource"),
            "the typed rmcp client must receive the handler's own refusal text",
        );

        for index in 0..8 {
            let seeded = fixture
                .client
                .call_tool(
                    CallToolRequestParams::new("tracedecay_fact_store_add").with_arguments(
                        json!({
                            "content": format!(
                                "RMCP_WIRE_ORACLE_{index:02}: {}",
                                "large response remains retrievable ".repeat(180),
                            ),
                            "category": "project",
                            "trust": 0.9,
                            "format": "json",
                        })
                        .as_object()
                        .cloned()
                        .expect("object arguments"),
                    ),
                )
                .await
                .expect("seed large RMCP tools/call");
            assert_ne!(
                seeded.is_error,
                Some(true),
                "large-response seed {index} was refused: {seeded:?}"
            );
        }
        let listed = fixture
            .client
            .call_tool(
                CallToolRequestParams::new("tracedecay_fact_store_list").with_arguments(
                    json!({
                        "category": "project",
                        "min_trust": 0.0,
                        "limit": 200,
                        "format": "json",
                    })
                    .as_object()
                    .cloned()
                    .expect("object arguments"),
                ),
            )
            .await
            .expect("large RMCP tools/call");
        assert_ne!(
            listed.is_error,
            Some(true),
            "large-response list was refused: {listed:?}"
        );
        let large_response = fixture.last_response();
        let large_text = large_response["result"]["content"][0]["text"]
            .as_str()
            .expect("large response text");
        let large_envelope: Value =
            serde_json::from_str(large_text).expect("large response truncation envelope");
        assert_eq!(
            large_envelope["truncated"],
            json!(true),
            "large-response list did not return a truncation envelope: {listed:?}"
        );
        assert!(
            large_envelope["original_chars"]
                .as_u64()
                .unwrap_or_default()
                >= 15_000,
            "large response must cross the production response budget",
        );
        assert!(
            large_envelope["handle"]
                .as_str()
                .is_some_and(|handle| handle.starts_with("rh_")),
            "large response must retain a typed retrieval handle",
        );
        fixture.shutdown().await;
    }

    /// The legacy raw JSON-RPC transport must stay byte-for-byte what it was
    /// before the typed envelope: the envelope is an internal representation,
    /// never a wire change. These are the shapes a host actually parses —
    /// method refusals, param refusals, the trivial ack, and a resource body —
    /// pinned as exact serialized frames rather than as structural matches.
    #[tokio::test]
    async fn legacy_json_rpc_wire_frames_are_unchanged_by_the_typed_envelope() {
        crate::product_runtime::register_fixture_product_runtime();
        let (cg, _repo, authority) =
            crate::mcp::server::writer_test_support::init_indexed_repo().await;
        let context = crate::mcp::server::writer_test_support::registered_context(cg, &authority);
        let server = McpServer::new_with_registered_test_context(context, Vec::new())
            .await
            .expect("registered legacy wire server");

        for (request_line, expected) in [
            (
                r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#,
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found: nope"}}"#,
            ),
            (
                r#"{"jsonrpc":"2.0","id":2,"method":"resources/read","params":{}}"#,
                r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32602,"message":"missing 'uri' in resources/read params"}}"#,
            ),
            (
                r#"{"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"tracedecay://nope"}}"#,
                r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"unknown resource URI: tracedecay://nope"}}"#,
            ),
            (
                r#"{"jsonrpc":"2.0","id":4,"method":"tools/call"}"#,
                r#"{"jsonrpc":"2.0","id":4,"error":{"code":-32602,"message":"missing params for tools/call"}}"#,
            ),
            (
                r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"arguments":{}}}"#,
                r#"{"jsonrpc":"2.0","id":5,"error":{"code":-32602,"message":"missing 'name' in tools/call params"}}"#,
            ),
            (
                r#"{"jsonrpc":"2.0","id":"ack","method":"ping"}"#,
                r#"{"jsonrpc":"2.0","id":"ack","result":{}}"#,
            ),
        ] {
            let request: JsonRpcRequest =
                serde_json::from_str(request_line).expect("legacy request line");
            let response = server
                .handle_request(&request)
                .await
                .expect("legacy response");
            assert_eq!(
                serde_json::to_string(&response).expect("serialize legacy response"),
                expected,
                "legacy wire frame changed for {request_line}",
            );
        }

        // A resource body is too large to pin whole; its frame *shape* — field
        // order included — is the part hosts depend on.
        let schema_request: JsonRpcRequest = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":6,"method":"resources/read","params":{"uri":"tracedecay://schema"}}"#,
        )
        .expect("legacy request line");
        let schema = serde_json::to_string(
            &server
                .handle_request(&schema_request)
                .await
                .expect("legacy response"),
        )
        .expect("serialize legacy response");
        assert!(
            schema.starts_with(
                r#"{"jsonrpc":"2.0","id":6,"result":{"contents":[{"mimeType":"text/markdown","text":"#
            ) && schema.ends_with(r#","uri":"tracedecay://schema"}]}}"#),
            "legacy resources/read frame shape changed: {schema}",
        );

        // Notifications stay responseless on the legacy transport.
        for notification in [
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#,
        ] {
            let request: JsonRpcRequest =
                serde_json::from_str(notification).expect("legacy notification line");
            assert!(
                server.handle_request(&request).await.is_none(),
                "legacy notification produced a response: {notification}",
            );
        }
        server.shutdown().await;
    }

    #[test]
    fn rmcp_selected_project_retirement_error_is_stable() {
        let error = project_server_retired_error();
        assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(
            error.message,
            "tool project route failed: project server was retired",
        );
        assert_eq!(
            error.data,
            Some(json!({
                "reason_code": "project_server_retired",
                "retryable": true,
                "detail": "the retained project server was replaced or revoked; retry against the current owner",
            })),
        );
    }

    #[test]
    fn response_conversion_preserves_tool_content_and_rpc_errors() {
        let complete: CallToolResponse =
            rmcp_response_result::<CallToolResult>(JsonRpcResponse::success(
                json!(7),
                json!({"content": [{"type": "text", "text": "ok"}]}),
            ))
            .map(Into::into)
            .expect("tool response");
        let CallToolResponse::Complete(CallToolResult { content, .. }) = complete else {
            panic!("ordinary TraceDecay tool responses must stay complete");
        };
        assert_eq!(
            content[0].as_text().map(|text| text.text.as_str()),
            Some("ok")
        );

        let error = rmcp_response_result::<ListToolsResult>(JsonRpcResponse::error_with_data(
            json!("request"),
            tracedecay_mcp::transport::ErrorCode::InvalidParams,
            "invalid arguments".to_owned(),
            Some(json!({"reason": "missing_query"})),
        ))
        .expect_err("error response");
        assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
        assert_eq!(error.message, "invalid arguments");
        assert_eq!(error.data, Some(json!({"reason": "missing_query"})));
    }

    #[test]
    fn adapter_accepts_the_legacy_initialize_response_shape() {
        crate::product_runtime::register_fixture_product_runtime();
        let initialized: InitializeResult = rmcp_response_result(JsonRpcResponse::success(
            json!(1),
            crate::mcp::server::initialize_result("TraceDecay instructions")
                .expect("fixture product runtime registered"),
        ))
        .expect("rmcp must preserve legacy MCP initialization compatibility");

        assert_eq!(
            serde_json::to_value(&initialized).expect("serialize initialized response")["protocolVersion"],
            json!("2024-11-05")
        );
        assert!(initialized.capabilities.tools.is_some());
        assert!(initialized.capabilities.resources.is_some());
    }

    #[tokio::test]
    async fn cancellation_stops_dispatch_before_live_request_registration() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let cancel_attempts = Arc::clone(&attempts);

        let result = await_dispatch_with_cancellation(
            std::future::pending::<()>(),
            std::future::ready(()),
            move || {
                cancel_attempts.fetch_add(1, Ordering::SeqCst);
                false
            },
        )
        .await;

        assert_eq!(result, None);
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "an unregistered request has no admitted work to poll for cancellation"
        );
    }
}
