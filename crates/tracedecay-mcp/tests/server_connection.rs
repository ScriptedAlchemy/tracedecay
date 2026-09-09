use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use tracedecay_mcp::server::{
    McpConnectionContext, McpConnectionServer, McpConnectionState, McpDispatchRequest,
    McpResponseLease,
};
use tracedecay_mcp::{JsonRpcResponse, McpTransport};

struct TestTransport {
    incoming: tokio::sync::mpsc::UnboundedReceiver<String>,
    outgoing: tokio::sync::mpsc::UnboundedSender<String>,
    reads: Arc<std::sync::atomic::AtomicUsize>,
}

impl McpTransport for TestTransport {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        let line = self.incoming.recv().await;
        if line.is_some() {
            self.reads.fetch_add(1, Ordering::Release);
        }
        Ok(line)
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.outgoing
            .send(line.to_owned())
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::BrokenPipe, error))
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct TestConnection {
    scope: String,
}

struct TestLease {
    revoked: tracedecay_session_memory::context::CancellationToken,
}

impl McpResponseLease for TestLease {
    fn revoked(&self) -> &tracedecay_session_memory::context::CancellationToken {
        &self.revoked
    }
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
    slow_release: tokio::sync::Notify,
    shutdown: AtomicBool,
    cancellation_registered: tokio::sync::Notify,
    max_concurrent_reads: usize,
}

impl TestContext {
    fn new() -> Self {
        Self::with_max_concurrent_reads(2)
    }

    fn with_max_concurrent_reads(max_concurrent_reads: usize) -> Self {
        Self {
            slow_release: tokio::sync::Notify::new(),
            shutdown: AtomicBool::new(false),
            cancellation_registered: tokio::sync::Notify::new(),
            max_concurrent_reads,
        }
    }
}

impl McpConnectionContext for TestContext {
    type Connection = TestConnection;

    fn new_connection(&self) -> tracedecay_domain::errors::Result<Self::Connection> {
        Ok(TestConnection {
            scope: "test-connection".to_owned(),
        })
    }

    fn timings_enabled(&self) -> bool {
        false
    }

    fn build_version(&self) -> tracedecay_domain::errors::Result<&'static str> {
        Ok("test")
    }

    fn max_concurrent_reads(&self) -> usize {
        self.max_concurrent_reads
    }

    fn tool_is_read_only(&self, _tool_name: &str) -> bool {
        true
    }

    fn tool_supports_live_cancellation(&self, _tool_name: &str) -> bool {
        true
    }

    fn dispatch<'a>(
        &'a self,
        request: McpDispatchRequest<'a>,
        _timings_enabled: bool,
        _connection: &'a mut Self::Connection,
        pre_cancelled: bool,
    ) -> Pin<Box<dyn Future<Output = Option<JsonRpcResponse>> + Send + 'a>> {
        Box::pin(async move {
            let id = request.cloned_id()?;
            if pre_cancelled {
                return Some(JsonRpcResponse::error(
                    id,
                    tracedecay_mcp::ErrorCode::RequestCancelled,
                    "cancelled before dispatch".to_owned(),
                ));
            }
            if id == json!("slow") {
                self.slow_release.notified().await;
            }
            Some(JsonRpcResponse::success(id, json!({"served": true})))
        })
    }

    fn cancel_request(&self, _id: &Value, _connection_scope: &str) -> bool {
        false
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
        Box::pin(async move {
            self.shutdown.store(true, Ordering::Release);
        })
    }
}

async fn response_id(responses: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> Value {
    response(responses).await["id"].clone()
}

async fn response(responses: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> Value {
    let line = tokio::time::timeout(std::time::Duration::from_secs(5), responses.recv())
        .await
        .expect("response timeout")
        .expect("response line");
    serde_json::from_str::<Value>(line.trim()).expect("JSON-RPC response")
}

#[tokio::test]
async fn queued_cancellation_is_delivered_before_the_request_dispatches() {
    let context = Arc::new(TestContext::with_max_concurrent_reads(1));
    let server = McpConnectionServer::new(Arc::clone(&context));
    let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (response_tx, mut response_rx) = tokio::sync::mpsc::unbounded_channel();
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut transport = TestTransport {
        incoming: request_rx,
        outgoing: response_tx,
        reads: Arc::clone(&reads),
    };
    let serving = tokio::spawn(async move { server.run_connection(&mut transport).await });

    for id in [json!("slow"), json!("cancelled")] {
        request_tx
            .send(
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "tools/call",
                    "params": {"name": "tracedecay_search", "arguments": {}}
                })
                .to_string(),
            )
            .expect("queued request");
    }
    request_tx
        .send(
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": "cancelled"}
            })
            .to_string(),
        )
        .expect("queued cancellation");

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while reads.load(Ordering::Acquire) < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("connection did not consume queued cancellation");
    context.slow_release.notify_one();
    assert_eq!(response_id(&mut response_rx).await, json!("slow"));
    let cancelled = response(&mut response_rx).await;
    assert_eq!(cancelled["id"], json!("cancelled"));
    assert_eq!(cancelled["error"]["code"], json!(-32800));

    drop(request_tx);
    serving
        .await
        .expect("connection task")
        .expect("connection result");
}

#[tokio::test]
async fn independent_reads_settle_out_of_order_and_half_close_drains() {
    let context = Arc::new(TestContext::new());
    let server = McpConnectionServer::new(Arc::clone(&context));
    let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (response_tx, mut response_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut transport = TestTransport {
        incoming: request_rx,
        outgoing: response_tx,
        reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };
    let serving = tokio::spawn(async move { server.run_connection(&mut transport).await });

    request_tx
        .send(
            json!({
                "jsonrpc": "2.0",
                "id": "slow",
                "method": "tools/call",
                "params": {"name": "tracedecay_search", "arguments": {}}
            })
            .to_string(),
        )
        .expect("slow request");
    request_tx
        .send(
            json!({
                "jsonrpc": "2.0",
                "id": "fast",
                "method": "tools/call",
                "params": {"name": "tracedecay_status", "arguments": {}}
            })
            .to_string(),
        )
        .expect("fast request");

    assert_eq!(response_id(&mut response_rx).await, json!("fast"));
    context.slow_release.notify_one();
    assert_eq!(response_id(&mut response_rx).await, json!("slow"));

    drop(request_tx);
    serving
        .await
        .expect("connection task")
        .expect("connection result");
    assert!(
        !context.shutdown.load(Ordering::Acquire),
        "a daemon-style connection close must not stop the shared server"
    );
}
