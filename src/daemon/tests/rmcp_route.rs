use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use super::*;

const PHASE_TIMEOUT: Duration = Duration::from_secs(20);
const AUTH_TOKEN: &str = "0123456789abcdef0123456789abcdef";

struct RmcpRouteFixture {
    _temp: TempDir,
    _database_scope: crate::db::DaemonDatabaseScope,
    store_administration: StoreAdministration,
    #[cfg(unix)]
    engine: DaemonEngine,
    handshake: DaemonHandshake,
    server: Arc<crate::mcp::McpServer>,
}

async fn rmcp_route_fixture(label: &str) -> RmcpRouteFixture {
    let temp = TempDir::new().expect("route fixture");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(project.join("src")).expect("fixture source directory");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("fixture source");
    let client_identity = test_client_identity_for(profile_root.clone());
    initialize_test_project(&project, &client_identity).await;
    let _database_scope = enter_test_daemon_database_scope(&profile_root, label);
    let handshake = DaemonHandshake {
        project_path: Some(project),
        client_identity,
        client_instance_id: format!("rmcp-route-{label}"),
        ..test_handshake_defaults()
    };
    register_mcp_route_observer(&handshake.client_instance_id);

    #[cfg(unix)]
    let (engine, store_administration, server) = {
        let engine = test_daemon_engine_for_profile(&profile_root);
        let server = engine
            .project_server(&handshake)
            .await
            .expect("open production project server");
        let store_administration = engine.store_administration.clone();
        (engine, store_administration, server)
    };
    #[cfg(not(unix))]
    let (store_administration, server) = {
        let store_administration = test_store_administration_for_profile(&profile_root);
        let server = Box::pin(super::super::portable_project_server_for_request(
            DaemonLifecycle::default(),
            store_administration.clone(),
            Arc::new(tokio::sync::Mutex::new(
                super::super::ProjectOpenGates::default(),
            )),
            super::super::DaemonInvocationState::default(),
            super::super::http_application::DaemonHttpApplicationRegistry::default(),
            &handshake,
            super::super::ProjectServerRequirement::Core,
            None,
        ))
        .await
        .expect("open portable production project server");
        (store_administration, server)
    };

    RmcpRouteFixture {
        _temp: temp,
        _database_scope,
        store_administration,
        #[cfg(unix)]
        engine,
        handshake,
        server,
    }
}

async fn write_line(writer: &mut (impl AsyncWrite + Unpin), value: &Value) {
    writer
        .write_all(value.to_string().as_bytes())
        .await
        .expect("write JSON-RPC value");
    writer.write_all(b"\n").await.expect("write newline");
}

async fn read_value(reader: &mut (impl AsyncBufRead + Unpin), message: &str) -> Value {
    let mut line = String::new();
    let bytes = tokio::time::timeout(PHASE_TIMEOUT, reader.read_line(&mut line))
        .await
        .expect(message)
        .expect("read JSON-RPC response");
    assert_ne!(bytes, 0, "{message}: connection closed");
    serde_json::from_str(line.trim()).expect("JSON-RPC response")
}

async fn read_to_eof(reader: &mut (impl AsyncBufRead + Unpin)) -> Vec<Value> {
    let mut responses = Vec::new();
    loop {
        let mut line = String::new();
        if reader
            .read_line(&mut line)
            .await
            .expect("read remaining response")
            == 0
        {
            return responses;
        }
        responses.push(serde_json::from_str(line.trim()).expect("remaining JSON-RPC response"));
    }
}

fn initialize_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "rmcp-production-route-test", "version": "1"}
        }
    })
}

fn blocked_tool_request(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_configuration_get",
            "arguments": {"key": "sync.session_start_sync"}
        }
    })
}

fn ping_request(id: u64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "ping"})
}

fn cancellation(request_id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": request_id, "reason": "test cancellation"}
    })
}

fn assert_delivered_cancellation(responses: &[Value], request_id: u64, context: &str) {
    let response = responses
        .iter()
        .find(|response| response["id"] == json!(request_id))
        .unwrap_or_else(|| panic!("{context}: no cancellation response for request {request_id}"));
    assert!(
        response.get("result").is_none(),
        "{context}: cancelled request returned partial success: {response}"
    );
    assert_eq!(
        response["error"]["code"],
        json!(-32800),
        "{context}: cancellation response used the wrong error code: {response}"
    );
    assert_eq!(
        response["error"]["data"]["reason_code"],
        json!("request_cancelled"),
        "{context}: cancellation response omitted its typed reason: {response}"
    );
}

fn assert_response_order(responses: &[Value], expected: &[u64], context: &str) {
    let ids = responses
        .iter()
        .filter_map(|response| response["id"].as_u64())
        .collect::<Vec<_>>();
    assert_eq!(ids, expected, "{context}: response order drifted");
}

async fn assert_initialized_route_is_rmcp<R, W>(
    mut reader: R,
    mut writer: W,
    handshake: &DaemonHandshake,
    server: &Arc<crate::mcp::McpServer>,
    server_task: tokio::task::JoinHandle<crate::errors::Result<()>>,
) where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    writer
        .write_all(handshake.to_line().expect("handshake").as_bytes())
        .await
        .expect("write handshake");
    writer.write_all(b"\n").await.expect("handshake newline");
    write_line(&mut writer, &initialize_request()).await;
    let initialized = read_value(&mut reader, "initialize response timed out").await;
    assert_eq!(initialized["id"], json!(1));
    assert!(initialized.get("result").is_some(), "{initialized}");
    wait_for_mcp_routes(&handshake.client_instance_id, &[ObservedMcpRoute::Rmcp]).await;

    let response_lifecycle = server.project_server_response_lifecycle();
    let response_gate = Arc::clone(response_lifecycle.response_gate());
    let gate = response_gate.write().await;
    write_line(&mut writer, &blocked_tool_request(2)).await;
    write_line(&mut writer, &ping_request(3)).await;

    let ping = read_value(
        &mut reader,
        "initialized production connection did not dispatch ping concurrently",
    )
    .await;
    assert_eq!(
        ping["id"],
        json!(3),
        "RMCP must serve ping while the earlier tool request is in flight: {ping}"
    );
    assert!(ping.get("result").is_some(), "{ping}");

    write_line(&mut writer, &cancellation(2)).await;
    writer
        .shutdown()
        .await
        .expect("shutdown initialized client");
    let (remaining, served) = tokio::time::timeout(PHASE_TIMEOUT, async {
        tokio::join!(read_to_eof(&mut reader), server_task)
    })
    .await
    .expect("cancelled RMCP route did not terminate while the response gate remained held");
    served
        .expect("join production RMCP connection")
        .expect("serve production RMCP connection");
    assert_delivered_cancellation(
        &remaining,
        2,
        "response-gate cancellation before request registration",
    );
    drop(gate);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unix_production_route_selects_rmcp_only_after_initialize() {
    let fixture = rmcp_route_fixture("unix-rmcp-production-route").await;

    let (server_stream, client_stream) =
        tokio::net::UnixStream::pair().expect("production route socket pair");
    let engine = fixture.engine.clone();
    let initialized_task = tokio::spawn(async move {
        Box::pin(super::super::serve_socket_client(server_stream, engine)).await
    });
    let (reader, writer) = client_stream.into_split();
    assert_initialized_route_is_rmcp(
        tokio::io::BufReader::new(reader),
        writer,
        &fixture.handshake,
        &fixture.server,
        initialized_task,
    )
    .await;

    let fixture = rmcp_route_fixture("unix-legacy-production-route").await;
    let response_lifecycle = fixture.server.project_server_response_lifecycle();
    let response_gate = Arc::clone(response_lifecycle.response_gate());
    let gate = response_gate.write().await;
    let (server_stream, client_stream) =
        tokio::net::UnixStream::pair().expect("legacy route socket pair");
    let engine = fixture.engine.clone();
    let legacy_task = tokio::spawn(async move {
        Box::pin(super::super::serve_socket_client(server_stream, engine)).await
    });
    let (reader, mut writer) = client_stream.into_split();
    let mut reader = tokio::io::BufReader::new(reader);
    writer
        .write_all(
            fixture
                .handshake
                .to_line()
                .expect("legacy handshake")
                .as_bytes(),
        )
        .await
        .expect("write legacy handshake");
    writer.write_all(b"\n").await.expect("handshake newline");
    write_line(&mut writer, &blocked_tool_request(4)).await;
    write_line(&mut writer, &ping_request(5)).await;
    wait_for_mcp_routes(
        &fixture.handshake.client_instance_id,
        &[ObservedMcpRoute::Legacy],
    )
    .await;
    writer.shutdown().await.expect("shutdown legacy client");
    drop(gate);
    let responses = tokio::time::timeout(PHASE_TIMEOUT, read_to_eof(&mut reader))
        .await
        .expect("legacy replay responses timed out");
    legacy_task
        .await
        .expect("join legacy route")
        .expect("serve legacy route");
    assert_response_order(
        &responses,
        &[4, 5],
        "Unix non-initialize traffic must remain sequential legacy replay",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn portable_production_route_selects_rmcp_after_initialize() {
    let fixture = rmcp_route_fixture("portable-rmcp-production-route").await;
    let (listener, endpoint) = super::super::transport::BrokerListener::bind(
        &super::super::transport::default_loopback_endpoint(),
    )
    .await
    .expect("portable route listener");
    let lifecycle = DaemonLifecycle::default();
    let store_administration = fixture.store_administration.clone();
    let server_task = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept portable client");
        Box::pin(super::super::serve_windows_broker_client(
            stream,
            AUTH_TOKEN,
            &lifecycle,
            store_administration,
            Arc::new(tokio::sync::Mutex::new(
                super::super::ProjectOpenGates::default(),
            )),
            None,
        ))
        .await
    });
    let stream = super::super::transport::BrokerStream::connect(&endpoint)
        .await
        .expect("connect portable client");
    let (reader, mut writer) = stream.into_split();
    let preface = super::super::transport::DaemonAuthPreface::new(AUTH_TOKEN)
        .to_line()
        .expect("portable auth preface");
    writer
        .write_all(preface.as_bytes())
        .await
        .expect("write auth preface");
    writer.write_all(b"\n").await.expect("auth newline");
    assert_initialized_route_is_rmcp(
        tokio::io::BufReader::new(reader),
        writer,
        &fixture.handshake,
        &fixture.server,
        server_task,
    )
    .await;

    let fixture = rmcp_route_fixture("portable-legacy-production-route").await;
    let response_lifecycle = fixture.server.project_server_response_lifecycle();
    let response_gate = Arc::clone(response_lifecycle.response_gate());
    let gate = response_gate.write().await;
    let (listener, endpoint) = super::super::transport::BrokerListener::bind(
        &super::super::transport::default_loopback_endpoint(),
    )
    .await
    .expect("portable legacy route listener");
    let lifecycle = DaemonLifecycle::default();
    let store_administration = fixture.store_administration.clone();
    let legacy_task = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept legacy client");
        Box::pin(super::super::serve_windows_broker_client(
            stream,
            AUTH_TOKEN,
            &lifecycle,
            store_administration,
            Arc::new(tokio::sync::Mutex::new(
                super::super::ProjectOpenGates::default(),
            )),
            None,
        ))
        .await
    });
    let stream = super::super::transport::BrokerStream::connect(&endpoint)
        .await
        .expect("connect portable legacy client");
    let (reader, mut writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(reader);
    let preface = super::super::transport::DaemonAuthPreface::new(AUTH_TOKEN)
        .to_line()
        .expect("portable legacy auth preface");
    writer
        .write_all(preface.as_bytes())
        .await
        .expect("write legacy auth preface");
    writer.write_all(b"\n").await.expect("auth newline");
    writer
        .write_all(
            fixture
                .handshake
                .to_line()
                .expect("portable legacy handshake")
                .as_bytes(),
        )
        .await
        .expect("write portable legacy handshake");
    writer.write_all(b"\n").await.expect("handshake newline");
    write_line(&mut writer, &blocked_tool_request(4)).await;
    write_line(&mut writer, &ping_request(5)).await;
    wait_for_mcp_routes(
        &fixture.handshake.client_instance_id,
        &[ObservedMcpRoute::Legacy],
    )
    .await;
    writer.shutdown().await.expect("shutdown legacy client");
    drop(gate);
    let responses = tokio::time::timeout(PHASE_TIMEOUT, read_to_eof(&mut reader))
        .await
        .expect("portable legacy replay responses timed out");
    legacy_task
        .await
        .expect("join portable legacy route")
        .expect("serve portable legacy route");
    assert_response_order(
        &responses,
        &[4, 5],
        "portable non-initialize traffic must remain sequential legacy replay",
    );
}

#[cfg(unix)]
struct ControlledCancellationExecutor {
    started: AtomicUsize,
    cancellation_observed: AtomicUsize,
    completed: AtomicUsize,
    pre_cancelled: AtomicUsize,
    release_first: AtomicBool,
}

#[cfg(unix)]
impl ControlledCancellationExecutor {
    fn new() -> Self {
        Self {
            started: AtomicUsize::new(0),
            cancellation_observed: AtomicUsize::new(0),
            completed: AtomicUsize::new(0),
            pre_cancelled: AtomicUsize::new(0),
            release_first: AtomicBool::new(false),
        }
    }
}

#[cfg(unix)]
impl tracedecay_application::ApplicationInvocationExecutor for ControlledCancellationExecutor {
    fn invoke(
        &self,
        invocation: tracedecay_application::ApplicationInvocation,
    ) -> tracedecay_application::ApplicationInvocationFuture<
        '_,
        std::result::Result<
            tracedecay_application::ApplicationResponse,
            tracedecay_application::InvocationError,
        >,
    > {
        Box::pin(async move {
            let (context, _) = invocation.into_parts();
            let (_, _, _, cancellation) = context.into_parts();
            let ordinal = self.started.fetch_add(1, Ordering::SeqCst);
            if cancellation.is_cancelled() {
                self.pre_cancelled.fetch_add(1, Ordering::SeqCst);
            }
            while !cancellation.is_cancelled() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            self.cancellation_observed.fetch_add(1, Ordering::SeqCst);
            if ordinal == 0 {
                while !self.release_first.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
            self.completed.fetch_add(1, Ordering::SeqCst);
            Err(tracedecay_application::InvocationError::Cancelled)
        })
    }
}

#[cfg(unix)]
impl crate::daemon_client::DaemonInvocationExecutor for ControlledCancellationExecutor {
    fn invoke_controlled(
        &self,
        _request: super::super::DaemonInvocationRequest,
        _deadline: tracedecay_application::Deadline,
        _cancellation: tracedecay_application::CancellationSignal,
        _policy: crate::daemon_client::InvocationCancellationPolicy,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            super::super::DaemonInvocationResponse,
            crate::daemon_client::DaemonInvocationError,
        >,
    > {
        Box::pin(async { panic!("controlled RMCP fixture uses application invocation") })
    }

    fn observe_plan26_feedback(
        &self,
        _subject_digest: tracedecay_domain::ManifestDigest,
        _observed_at: tracedecay_domain::UtcMicros,
        _event: crate::application::feedback::observations::Plan26FeedbackSourceEventV1,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<'_, crate::errors::Result<()>> {
        Box::pin(async { panic!("controlled RMCP fixture does not observe feedback") })
    }
}

#[cfg(unix)]
async fn wait_for_count(counter: &AtomicUsize, expected: usize, message: &str) {
    tokio::time::timeout(PHASE_TIMEOUT, async {
        while counter.load(Ordering::SeqCst) < expected {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect(message);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_rmcp_cancels_registered_and_pre_registration_requests() {
    let fixture = rmcp_route_fixture("rmcp-live-cancellation").await;
    let executor = Arc::new(ControlledCancellationExecutor::new());
    let project_path = fixture
        .handshake
        .project_path
        .as_deref()
        .expect("fixture project");
    let cg = super::super::open_project_for_handshake(
        project_path,
        &fixture.handshake,
        &fixture.engine.store_administration,
    )
    .await
    .expect("open controlled project");
    let key = ProjectServerKey::from_open_project(&cg, &fixture.handshake)
        .expect("controlled project key");
    let route =
        ProjectRouteKey::from_handshake(project_path, &fixture.handshake).expect("project route");
    let context = crate::mcp::server::McpServerConstructionContext::direct(cg, None)
        .with_application_invocation_executor(executor.clone());
    let controlled_server = crate::mcp::McpServer::new_with_context(context).await;
    {
        let mut owners = fixture
            .engine
            .store_administration
            .project_servers()
            .lock()
            .await;
        owners.insert_route(route, key.clone(), controlled_server);
        assert!(owners.mark_ready(&key));
    }

    let (server_stream, client_stream) =
        tokio::net::UnixStream::pair().expect("cancellation socket pair");
    let engine = fixture.engine.clone();
    let server_task = tokio::spawn(async move {
        Box::pin(super::super::serve_socket_client(server_stream, engine)).await
    });
    let (reader, mut writer) = client_stream.into_split();
    let mut reader = tokio::io::BufReader::new(reader);
    writer
        .write_all(
            fixture
                .handshake
                .to_line()
                .expect("cancellation handshake")
                .as_bytes(),
        )
        .await
        .expect("write cancellation handshake");
    writer.write_all(b"\n").await.expect("handshake newline");
    write_line(&mut writer, &initialize_request()).await;
    assert_eq!(
        read_value(&mut reader, "initialize cancellation route").await["id"],
        json!(1)
    );

    write_line(&mut writer, &blocked_tool_request(10)).await;
    wait_for_count(&executor.started, 1, "live request never reached executor").await;
    write_line(&mut writer, &blocked_tool_request(11)).await;
    write_line(&mut writer, &cancellation(11)).await;
    write_line(&mut writer, &cancellation(10)).await;
    wait_for_count(
        &executor.cancellation_observed,
        1,
        "typed cancellation never reached live request",
    )
    .await;
    assert_eq!(
        executor.started.load(Ordering::SeqCst),
        1,
        "second request registered before its earlier cancellation was observed"
    );
    executor.release_first.store(true, Ordering::SeqCst);
    wait_for_count(
        &executor.completed,
        2,
        "cancelled RMCP requests did not terminate",
    )
    .await;
    assert_eq!(
        executor.pre_cancelled.load(Ordering::SeqCst),
        1,
        "queued request was not cancelled before entering the application executor"
    );

    writer
        .shutdown()
        .await
        .expect("shutdown cancellation client");
    let (responses, served) = tokio::time::timeout(PHASE_TIMEOUT, async {
        tokio::join!(read_to_eof(&mut reader), server_task)
    })
    .await
    .expect("RMCP connection did not close after both cancellation responses were delivered");
    served
        .expect("join cancelled RMCP task")
        .expect("serve cancelled RMCP task");
    assert_delivered_cancellation(&responses, 10, "registered in-flight cancellation");
    assert_delivered_cancellation(&responses, 11, "pre-registration cancellation");
}
