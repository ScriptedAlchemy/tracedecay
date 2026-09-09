use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rmcp::model::PaginatedRequestParams;
use rmcp::transport::IntoTransport;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};
use tracedecay_mcp::JsonRpcResponse;
use tracedecay_mcp::server::{
    McpConnectionContext, McpConnectionState, McpDispatchRequest, McpResponseLease,
    RmcpConnectionAdapter,
};

struct TestLease {
    revoked: tracedecay_session_memory::context::CancellationToken,
}

impl McpResponseLease for TestLease {
    fn revoked(&self) -> &tracedecay_session_memory::context::CancellationToken {
        &self.revoked
    }
}

struct TestConnection {
    scope: String,
}

impl McpConnectionState for TestConnection {
    type ResponseLease = TestLease;

    fn memory_request_scope(&self) -> &str {
        &self.scope
    }

    fn fork_for_independent_read(&self) -> Self {
        Self {
            scope: self.scope.clone(),
        }
    }

    fn fork_for_connection_owned_read(&self) -> Self {
        self.fork_for_independent_read()
    }

    fn take_selected_response_lease(&mut self) -> Option<Self::ResponseLease> {
        None
    }
}

struct TestContext {
    cancellation_registered: tokio::sync::Notify,
    cancellations: std::sync::Mutex<Vec<(Value, String)>>,
}

impl McpConnectionContext for TestContext {
    type Connection = TestConnection;

    fn new_connection(&self) -> tracedecay_domain::errors::Result<Self::Connection> {
        Ok(TestConnection {
            scope: "rmcp-test".to_owned(),
        })
    }

    fn timings_enabled(&self) -> bool {
        false
    }

    fn build_version(&self) -> tracedecay_domain::errors::Result<&'static str> {
        Ok("9.8.7")
    }

    fn max_concurrent_reads(&self) -> usize {
        2
    }

    fn tool_is_read_only(&self, _tool_name: &str) -> bool {
        true
    }

    fn tool_supports_live_cancellation(&self, _tool_name: &str) -> bool {
        false
    }

    fn dispatch<'a>(
        &'a self,
        request: McpDispatchRequest<'a>,
        _timings_enabled: bool,
        _connection: &'a mut Self::Connection,
        _pre_cancelled: bool,
    ) -> Pin<Box<dyn Future<Output = Option<JsonRpcResponse>> + Send + 'a>> {
        Box::pin(async move {
            let id = request.cloned_id()?;
            let result = match request.method() {
                "initialize" => {
                    tracedecay_mcp::server::initialize_result("9.8.7", "RMCP transport test")
                }
                "tools/list" => json!({"tools": []}),
                "resources/list" => json!({"resources": []}),
                _ => json!({}),
            };
            Some(JsonRpcResponse::success(id, result))
        })
    }

    fn cancel_request(&self, id: &Value, connection_scope: &str) -> bool {
        self.cancellations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((id.clone(), connection_scope.to_owned()));
        true
    }

    fn cancellation_registered(&self) -> &tokio::sync::Notify {
        &self.cancellation_registered
    }

    fn take_pending_notifications(&self) -> Vec<Value> {
        Vec::new()
    }

    fn run_in_connection_admission<'a, T, F>(
        &'a self,
        future: F,
    ) -> Pin<Box<dyn Future<Output = T> + Send + 'a>>
    where
        T: Send + 'a,
        F: Future<Output = T> + Send + 'a,
    {
        Box::pin(future)
    }

    fn shutdown(self: Arc<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async {})
    }
}

#[tokio::test]
async fn rmcp_duplex_handshake_and_typed_list_share_the_dispatch_context() {
    let context = Arc::new(TestContext {
        cancellation_registered: tokio::sync::Notify::new(),
        cancellations: std::sync::Mutex::new(Vec::new()),
    });
    let adapter =
        RmcpConnectionAdapter::new(Arc::clone(&context), false, None, None).expect("RMCP adapter");
    let (server_io, client_io) = tokio::io::duplex(256 * 1024);
    let serving = tokio::spawn(async move {
        let running = adapter
            .serve(IntoTransport::into_transport(server_io))
            .await
            .expect("serve RMCP");
        running.waiting().await.expect("RMCP server task");
    });
    let mut client =
        ().serve(IntoTransport::<RoleClient, _, _>::into_transport(client_io))
            .await
            .expect("initialize RMCP client");

    let tools = client
        .list_tools(None::<PaginatedRequestParams>)
        .await
        .expect("typed tools/list");
    assert!(tools.tools.is_empty());

    client
        .peer()
        .notify_cancelled(rmcp::model::CancelledNotificationParam::new(
            Some(rmcp::model::RequestId::String(Arc::from("cancelled"))),
            Some("test cancellation".to_owned()),
        ))
        .await
        .expect("send typed cancellation");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let cancellation_observed = !context
                .cancellations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty();
            if cancellation_observed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("RMCP cancellation route");
    assert_eq!(
        *context
            .cancellations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![(json!("cancelled"), "rmcp-test".to_owned())],
    );

    client.close().await.expect("close RMCP client");
    serving.await.expect("join RMCP server");
}
