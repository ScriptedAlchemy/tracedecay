use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rmcp::model::{
    ClientCapabilities, ErrorCode, PaginatedRequestParams, ProtocolVersion, RequestMetaObject,
};
use rmcp::transport::IntoTransport;
use rmcp::{ClientLifecycleMode, ClientServiceExt, RoleClient, ServiceExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracedecay_daemon_protocol::{BrokerListener, DaemonEndpoint, default_loopback_endpoint};
use tracedecay_mcp::jsonrpc::JsonRpcError;
use tracedecay_mcp::server::{
    McpConnectionContext, McpConnectionState, McpDispatchRequest, McpResponseLease,
    RmcpConnectionAdapter,
};
use tracedecay_mcp::{BrokerStreamTransport, JsonRpcResponse};

struct TestLease {
    revoked: tracedecay_runtime_core::cancellation::CancellationToken,
}

impl McpResponseLease for TestLease {
    fn revoked(&self) -> &tracedecay_runtime_core::cancellation::CancellationToken {
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

    fn take_selected_response_lease(&mut self) -> Option<Self::ResponseLease> {
        None
    }
}

struct TestContext {
    cancellation_registered: tokio::sync::Notify,
    cancellations: std::sync::Mutex<Vec<(Value, String)>>,
    dispatch_started: tokio::sync::Notify,
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

    fn dispatch<'a>(
        &'a self,
        request: McpDispatchRequest<'a>,
        _timings_enabled: bool,
        _connection: &'a mut Self::Connection,
        cancellation: tracedecay_runtime_core::cancellation::CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Option<JsonRpcResponse>> + Send + 'a>> {
        Box::pin(async move {
            let id = request.cloned_id()?;
            let result = match (request.method(), request.tool_name()) {
                ("initialize", _) => {
                    tracedecay_mcp::server::initialize_result("9.8.7", "RMCP transport test", None)
                }
                ("tools/list", _) => json!({"tools": []}),
                ("resources/list", _) => json!({"resources": []}),
                ("resources/read", _) => {
                    return Some(JsonRpcResponse {
                        error: Some(JsonRpcError {
                            code: ErrorCode::RESOURCE_NOT_FOUND.0,
                            message: "resource not found".to_owned(),
                            data: None,
                        }),
                        ..JsonRpcResponse::success(id, Value::Null)
                    });
                }
                ("tools/call", Some("connection_scope")) => call_tool_text(
                    CONNECTION_SCOPE
                        .try_with(|scope| *scope)
                        .unwrap_or("outside the connection scope"),
                ),
                ("tools/call", Some("panic")) => panic!("tool handler exploded"),
                ("tools/call", Some("wait_for_cancel")) => {
                    self.dispatch_started.notify_one();
                    cancellation.cancelled().await;
                    call_tool_text("cancelled")
                }
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
}

type ClientStream = tokio::net::TcpStream;
type SessionOutcome = tracedecay_domain::errors::Result<()>;

/// A daemon-side broker stream and its connected client socket.
async fn broker_pair() -> (BrokerStreamTransport, ClientStream) {
    let (listener, endpoint) = BrokerListener::bind(&default_loopback_endpoint())
        .await
        .expect("bind loopback listener");
    let DaemonEndpoint::Loopback(address) = endpoint else {
        panic!("loopback bind returned {endpoint}");
    };
    let client = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect loopback client");
    let server = listener.accept().await.expect("accept loopback client");
    (BrokerStreamTransport::new(server), client)
}

/// Serves one session inside a task-local scope standing in for the daemon's
/// per-connection admission scope.
async fn open_session(context: &Arc<TestContext>) -> TestSession {
    let adapter =
        RmcpConnectionAdapter::new(Arc::clone(context), false, None, None).expect("RMCP adapter");
    let (transport, client) = broker_pair().await;
    let (shutdown, draining) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(CONNECTION_SCOPE.scope("connection-scope", async move {
        adapter
            .serve(transport, async move {
                let _ = draining.await;
            })
            .await
    }));
    let (reader, writer) = tokio::io::split(client);
    TestSession {
        lines: BufReader::new(reader).lines(),
        writer,
        shutdown: Some(shutdown),
        serving,
    }
}

#[tokio::test]
async fn rmcp_client_handshake_typed_list_and_cancellation_share_the_dispatch_context() {
    let context = new_context();
    let adapter =
        RmcpConnectionAdapter::new(Arc::clone(&context), false, None, None).expect("RMCP adapter");
    let (transport, client_io) = broker_pair().await;
    let serving = tokio::spawn(async move {
        adapter
            .serve(transport, std::future::pending())
            .await
            .expect("serve MCP session");
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

/// A default `rmcp` 3.5 client sends `initialize` naming 2026-07-28, a revision
/// without a handshake, so the session settles on the newest one that has it.
/// An auto-lifecycle client probes `server/discover` and stays per-request.
#[tokio::test]
async fn rmcp_3_5_clients_open_a_session_on_either_lifecycle() {
    let context = new_context();
    for (lifecycle, negotiated) in [
        (
            ClientLifecycleMode::Initialize,
            ProtocolVersion::LATEST_WITH_INITIALIZE,
        ),
        (
            ClientLifecycleMode::Auto {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                legacy_version: None,
            },
            ProtocolVersion::V_2026_07_28,
        ),
    ] {
        let adapter = RmcpConnectionAdapter::new(Arc::clone(&context), false, None, None)
            .expect("RMCP adapter");
        let (transport, client_io) = broker_pair().await;
        let serving = tokio::spawn(async move {
            adapter
                .serve(transport, std::future::pending())
                .await
                .expect("serve MCP session");
        });
        let mut client = ()
            .serve_with_lifecycle(
                IntoTransport::<RoleClient, _, _>::into_transport(client_io),
                lifecycle.clone(),
            )
            .await
            .expect("open RMCP client session");
        assert_eq!(
            client.peer_info().expect("server info").protocol_version,
            negotiated,
            "{lifecycle:?}"
        );
        let tools = client
            .list_tools(None::<PaginatedRequestParams>)
            .await
            .expect("typed tools/list");
        assert!(tools.tools.is_empty(), "{lifecycle:?}");
        client.close().await.expect("close RMCP client");
        serving.await.expect("join RMCP server");
    }
}

tokio::task_local! {
    static CONNECTION_SCOPE: &'static str;
}

fn call_tool_text(text: &str) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

fn new_context() -> Arc<TestContext> {
    Arc::new(TestContext {
        cancellation_registered: tokio::sync::Notify::new(),
        cancellations: std::sync::Mutex::new(Vec::new()),
        dispatch_started: tokio::sync::Notify::new(),
    })
}

/// `_meta` carrying the SEP-2575 per-request client context for `version`.
fn request_context(version: ProtocolVersion) -> Value {
    let mut context = RequestMetaObject::new();
    context.set_protocol_version(version);
    context.set_client_capabilities(ClientCapabilities::default());
    serde_json::to_value(context.0).expect("request context")
}

fn initialize_frame(id: u64, version: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": version,
            "capabilities": {},
            "clientInfo": {"name": "session-policy", "version": "0"},
        },
    })
}

fn request_frame(id: u64, method: &str, mut params: Value, meta: Option<Value>) -> Value {
    if let Some(meta) = meta {
        params["_meta"] = meta;
    }
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

struct TestSession {
    lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<ClientStream>>>,
    writer: tokio::io::WriteHalf<ClientStream>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    serving: tokio::task::JoinHandle<SessionOutcome>,
}

impl TestSession {
    async fn send(&mut self, frame: &Value) {
        let mut line = serde_json::to_vec(frame).expect("encode frame");
        line.push(b'\n');
        self.writer.write_all(&line).await.expect("write frame");
        self.writer.flush().await.expect("flush frame");
    }

    async fn recv(&mut self) -> Option<Value> {
        self.lines
            .next_line()
            .await
            .expect("read frame")
            .map(|line| serde_json::from_str(&line).expect("server frame is JSON"))
    }

    async fn exchange(&mut self, frame: &Value) -> Value {
        self.send(frame).await;
        let response = self.recv().await.expect("a response frame");
        assert_eq!(response["id"], frame["id"], "{response}");
        response
    }

    async fn initialized(context: &Arc<TestContext>, version: &str) -> Self {
        let mut session = open_session(context).await;
        let initialized = session.exchange(&initialize_frame(0, version)).await;
        assert_eq!(
            initialized["result"]["protocolVersion"],
            json!(version),
            "{initialized}"
        );
        session
    }
}

#[tokio::test]
async fn requests_dispatch_inside_the_connection_future() {
    let context = new_context();
    let mut session = TestSession::initialized(&context, "2025-06-18").await;

    let response = session
        .exchange(&request_frame(
            1,
            "tools/call",
            json!({"name": "connection_scope", "arguments": {}}),
            None,
        ))
        .await;

    assert_eq!(
        response["result"]["content"][0]["text"],
        json!("connection-scope"),
        "a request handler must observe the connection task's task-local scope: {response}"
    );
}

#[tokio::test]
async fn the_session_opens_only_on_a_well_formed_initialize() {
    let context = new_context();
    let mut session = open_session(&context).await;

    let ping = session
        .exchange(&json!({"jsonrpc": "2.0", "id": "early", "method": "ping"}))
        .await;
    assert_eq!(ping["result"], json!({}), "{ping}");

    session
        .send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await;
    let refused = session
        .exchange(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))
        .await;
    assert_eq!(refused["error"]["code"], json!(-32602), "{refused}");

    let initialized = session.exchange(&initialize_frame(2, "2025-06-18")).await;
    assert_eq!(
        initialized["result"]["protocolVersion"],
        json!("2025-06-18"),
        "{initialized}"
    );
    assert_eq!(
        initialized["result"]["serverInfo"]["version"],
        json!("9.8.7")
    );
}

#[tokio::test]
async fn initialize_naming_a_per_request_revision_negotiates_the_newest_handshake_revision() {
    let context = new_context();
    let mut session = open_session(&context).await;

    let initialized = session.exchange(&initialize_frame(1, "2026-07-28")).await;

    assert_eq!(
        initialized["result"]["protocolVersion"],
        json!(ProtocolVersion::LATEST_WITH_INITIALIZE.as_str()),
        "{initialized}"
    );
}

#[tokio::test]
async fn refused_inline_openers_leave_the_session_open_for_initialize() {
    let context = new_context();
    let mut session = open_session(&context).await;
    let mut partial = request_context(ProtocolVersion::V_2026_07_28);
    partial
        .as_object_mut()
        .expect("context object")
        .retain(|key, _| key.ends_with("protocolVersion"));

    let refused = session
        .exchange(&request_frame(1, "tools/list", json!({}), Some(partial)))
        .await;
    assert_eq!(refused["error"]["code"], json!(-32602), "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|message| message.starts_with("request _meta is missing")),
        "{refused}"
    );

    let unsupported = session
        .exchange(&request_frame(
            2,
            "tools/list",
            json!({}),
            Some(request_context(
                serde_json::from_value(json!("1999-01-01")).expect("version"),
            )),
        ))
        .await;
    assert_eq!(
        unsupported["error"]["code"],
        json!(ErrorCode::UNSUPPORTED_PROTOCOL_VERSION.0),
        "{unsupported}"
    );

    let initialized = session.exchange(&initialize_frame(3, "2025-06-18")).await;
    assert_eq!(
        initialized["result"]["protocolVersion"],
        json!("2025-06-18"),
        "{initialized}"
    );
}

/// An `rmcp` 3.5 auto-lifecycle client probes `server/discover` before it
/// falls back to `initialize`; the probe must not commit the session to the
/// per-request lifecycle.
#[tokio::test]
async fn a_discover_probe_leaves_the_session_open_for_initialize() {
    let context = new_context();
    let mut session = open_session(&context).await;

    let discovered = session
        .exchange(&request_frame(
            1,
            "server/discover",
            json!({}),
            Some(request_context(ProtocolVersion::V_2026_07_28)),
        ))
        .await;
    assert!(
        discovered["result"]["supportedVersions"]
            .as_array()
            .is_some_and(|versions| versions.contains(&json!("2025-06-18"))),
        "{discovered}"
    );

    let initialized = session.exchange(&initialize_frame(2, "2025-06-18")).await;
    assert_eq!(
        initialized["result"]["protocolVersion"],
        json!("2025-06-18"),
        "{initialized}"
    );

    let listed = session
        .exchange(&request_frame(3, "tools/list", json!({}), None))
        .await;
    assert_eq!(
        listed["result"],
        json!({"tools": []}),
        "the initialized session keeps the legacy lifecycle: {listed}"
    );
}

#[tokio::test]
async fn an_inline_session_applies_the_2026_07_28_request_policy() {
    let context = new_context();
    let mut session = open_session(&context).await;
    let inline = || Some(request_context(ProtocolVersion::V_2026_07_28));

    let listed = session
        .exchange(&request_frame(1, "tools/list", json!({}), inline()))
        .await;
    assert_eq!(
        listed["result"]["resultType"],
        json!("complete"),
        "{listed}"
    );
    assert_eq!(listed["result"]["ttlMs"], json!(0), "{listed}");

    let ping = session
        .exchange(&request_frame(2, "ping", json!({}), inline()))
        .await;
    assert_eq!(ping["result"], json!({}), "{ping}");

    let missing = session
        .exchange(&request_frame(
            3,
            "resources/read",
            json!({"uri": "test://missing"}),
            inline(),
        ))
        .await;
    assert_eq!(
        missing["error"]["code"],
        json!(ErrorCode::INVALID_PARAMS.0),
        "a 2026-07-28 peer gets invalid params for a missing resource: {missing}"
    );

    let unsupported = session
        .exchange(&request_frame(
            4,
            "tools/list",
            json!({}),
            Some(request_context(
                serde_json::from_value(json!("1999-01-01")).expect("version"),
            )),
        ))
        .await;
    assert_eq!(
        unsupported["error"]["code"],
        json!(ErrorCode::UNSUPPORTED_PROTOCOL_VERSION.0),
        "{unsupported}"
    );

    let not_found = session
        .exchange(&request_frame(
            5,
            "prompts/get",
            json!({"name": "absent"}),
            inline(),
        ))
        .await;
    assert_eq!(
        not_found["error"],
        json!({"code": -32601, "message": "prompts/get"}),
        "{not_found}"
    );

    let discovered = session
        .exchange(&request_frame(6, "server/discover", json!({}), inline()))
        .await;
    assert!(
        discovered["result"]["supportedVersions"]
            .as_array()
            .is_some_and(|versions| versions.contains(&json!("2026-07-28"))),
        "{discovered}"
    );

    let unmetered = session
        .exchange(&request_frame(7, "tools/list", json!({}), None))
        .await;
    assert_eq!(
        unmetered["error"]["code"],
        json!(-32602),
        "an inline session requires per-request context on every request: {unmetered}"
    );
}

#[tokio::test]
async fn an_initialized_legacy_session_keeps_the_legacy_wire_shape() {
    let context = new_context();
    let mut session = TestSession::initialized(&context, "2025-06-18").await;

    let listed = session
        .exchange(&request_frame(1, "tools/list", json!({}), None))
        .await;
    assert_eq!(listed["result"], json!({"tools": []}), "{listed}");

    let missing = session
        .exchange(&request_frame(
            2,
            "resources/read",
            json!({"uri": "test://missing"}),
            None,
        ))
        .await;
    assert_eq!(
        missing["error"]["code"],
        json!(ErrorCode::RESOURCE_NOT_FOUND.0),
        "a legacy peer keeps the resource-not-found code: {missing}"
    );

    let mut partial = request_context(ProtocolVersion::V_2026_07_28);
    partial
        .as_object_mut()
        .expect("context object")
        .retain(|key, _| key.ends_with("protocolVersion"));
    let partial = session
        .exchange(&request_frame(3, "tools/list", json!({}), Some(partial)))
        .await;
    assert_eq!(
        partial["error"]["code"],
        json!(-32602),
        "a request declaring 2026-07-28 must carry the full request context: {partial}"
    );
}

#[tokio::test]
async fn shutdown_cancels_in_flight_requests_and_answers_them_before_the_session_ends() {
    let context = new_context();
    let mut session = TestSession::initialized(&context, "2025-06-18").await;
    let started = context.dispatch_started.notified();
    session
        .send(&request_frame(
            1,
            "tools/call",
            json!({"name": "wait_for_cancel", "arguments": {}}),
            None,
        ))
        .await;
    started.await;

    session
        .shutdown
        .take()
        .expect("shutdown trigger")
        .send(())
        .expect("session still serving");
    (&mut session.serving)
        .await
        .expect("join session")
        .expect("a drained session ends cleanly");

    let answered = session
        .recv()
        .await
        .expect("the cancelled request is answered");
    assert_eq!(answered["id"], json!(1), "{answered}");
    assert_eq!(
        answered["result"]["content"][0]["text"],
        json!("cancelled"),
        "{answered}"
    );
    assert_eq!(
        session.recv().await,
        None,
        "the session closed after draining"
    );
}

#[tokio::test]
async fn a_panicking_request_is_answered_and_its_session_keeps_serving() {
    let context = new_context();
    let mut session = TestSession::initialized(&context, "2025-06-18").await;
    let started = context.dispatch_started.notified();
    session
        .send(&request_frame(
            1,
            "tools/call",
            json!({"name": "wait_for_cancel", "arguments": {}}),
            None,
        ))
        .await;
    started.await;

    let panicked = session
        .exchange(&request_frame(
            2,
            "tools/call",
            json!({"name": "panic", "arguments": {}}),
            None,
        ))
        .await;
    assert_eq!(
        panicked["error"]["code"],
        json!(ErrorCode::INTERNAL_ERROR.0),
        "{panicked}"
    );
    assert!(
        panicked["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("tool handler exploded")),
        "{panicked}"
    );

    let following = session
        .exchange(&request_frame(
            3,
            "tools/call",
            json!({"name": "connection_scope", "arguments": {}}),
            None,
        ))
        .await;
    assert_eq!(
        following["result"]["content"][0]["text"],
        json!("connection-scope"),
        "{following}"
    );

    session
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": 1},
        }))
        .await;
    let in_flight = session
        .recv()
        .await
        .expect("the in-flight request is answered");
    assert_eq!(in_flight["id"], json!(1), "{in_flight}");
    assert_eq!(
        in_flight["error"]["data"]["reason_code"],
        json!("request_cancelled"),
        "{in_flight}"
    );
}
