//! Broker-side MCP transport (`BrokerStreamTransport`) bridging a split
//! `BrokerStream` to the daemon's `McpTransport` and rmcp `Transport` traits.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic,
//! signatures, or behavior changed.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use serde_json::json;
use tokio::io::AsyncWriteExt;

use crate::mcp::server::{RmcpSelectedProjectResponseAuthority, RmcpWorkDeliverySettlement};
use crate::mcp::{JsonRpcResponse, McpTransport};

use super::BrokerStream;
use super::transport::{BrokerReadHalf, BrokerWriteHalf};
use super::*;

pub(super) struct BrokerStreamTransport {
    // Every daemon read of this transport races something else in a
    // `tokio::select!` — draining, an owner open, a completed handler. The
    // bounded reader owns the partial-frame accumulator so a read dropped by a
    // lost race resumes instead of restarting mid-frame and desynchronizing
    // JSON-RPC framing for the rest of the connection.
    reader: tracedecay_usecases::host_admission::BoundedLineReader<
        tokio::io::BufReader<BrokerReadHalf>,
    >,
    writer: Arc<tokio::sync::Mutex<Option<BrokerWriteHalf>>>,
    active_requests: Arc<
        std::sync::Mutex<HashMap<String, Option<tracedecay_domain::DeliverySettlementAttemptV1>>>,
    >,
    replay: VecDeque<String>,
    response_lifecycle: Option<crate::mcp::server::ProjectServerResponseLifecycle>,
    selected_project_responses: Option<RmcpSelectedProjectResponseAuthority>,
    work_delivery_settlement: Option<RmcpWorkDeliverySettlement>,
}

enum RmcpResponseWrite {
    Suppressed,
    Write(Option<tracedecay_domain::DeliverySettlementAttemptV1>),
}

enum RmcpResponseWriteFailure {
    Cancelled,
    Transport(std::io::Error),
}

impl RmcpResponseWriteFailure {
    fn into_io_error(self) -> std::io::Error {
        match self {
            Self::Cancelled => std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "project server response was revoked",
            ),
            Self::Transport(error) => error,
        }
    }
}

impl BrokerStreamTransport {
    pub(super) fn new(stream: BrokerStream) -> Self {
        let (reader, writer) = stream.into_owned_split();
        Self {
            reader: tracedecay_usecases::host_admission::BoundedLineReader::new(
                tokio::io::BufReader::new(reader),
            ),
            writer: Arc::new(tokio::sync::Mutex::new(Some(writer))),
            active_requests: Arc::new(std::sync::Mutex::new(HashMap::new())),
            replay: VecDeque::new(),
            response_lifecycle: None,
            selected_project_responses: None,
            work_delivery_settlement: None,
        }
    }

    pub(super) fn push_replay(&mut self, line: String) -> std::io::Result<()> {
        if line.len() > tracedecay_usecases::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES {
            let prefix = line.as_bytes()[..line
                .len()
                .min(tracedecay_usecases::host_admission::MCP_OVERSIZE_ID_INSPECT_BYTES)]
                .to_vec();
            return Err(
                tracedecay_usecases::host_admission::wire_oversized_io_error_with_prefix(prefix),
            );
        }
        self.replay.push_back(line);
        Ok(())
    }

    pub(super) fn with_project_response_lifecycle(
        mut self,
        lifecycle: crate::mcp::server::ProjectServerResponseLifecycle,
    ) -> Self {
        self.response_lifecycle = Some(lifecycle);
        self
    }

    pub(super) fn with_rmcp_work_delivery_settlement(
        mut self,
        settlement: RmcpWorkDeliverySettlement,
    ) -> Self {
        self.work_delivery_settlement = Some(settlement);
        self
    }

    pub(super) fn with_rmcp_selected_project_responses(
        mut self,
        responses: RmcpSelectedProjectResponseAuthority,
    ) -> Self {
        self.selected_project_responses = Some(responses);
        self
    }

    async fn write_all_and_flush(
        writer: Arc<tokio::sync::Mutex<Option<BrokerWriteHalf>>>,
        bytes: Vec<u8>,
    ) -> std::io::Result<()> {
        let mut writer = writer.lock().await;
        let writer = writer.as_mut().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "daemon broker transport closed",
            )
        })?;
        writer.write_all(&bytes).await?;
        writer.flush().await
    }

    fn request_key(id: &serde_json::Value) -> Option<String> {
        (!id.is_null())
            .then(|| serde_json::to_string(id).ok())
            .flatten()
    }

    fn response_request_key(value: &serde_json::Value) -> Option<String> {
        (value.get("result").is_some() || value.get("error").is_some())
            .then(|| value.get("id"))
            .flatten()
            .and_then(Self::request_key)
    }

    fn take_response_write(
        active_requests: Arc<
            std::sync::Mutex<
                HashMap<String, Option<tracedecay_domain::DeliverySettlementAttemptV1>>,
            >,
        >,
        request_key: Option<String>,
    ) -> std::io::Result<RmcpResponseWrite> {
        let Some(request_key) = request_key else {
            return Ok(RmcpResponseWrite::Write(None));
        };
        let response = active_requests
            .lock()
            .map_err(|_| std::io::Error::other("active RMCP request registry poisoned"))?
            .remove(&request_key);
        match response {
            Some(delivery_attempt) => Ok(RmcpResponseWrite::Write(delivery_attempt)),
            None => Ok(RmcpResponseWrite::Suppressed),
        }
    }

    fn settle_work_delivery(
        settlement: Option<&RmcpWorkDeliverySettlement>,
        delivery_attempt: Option<tracedecay_domain::DeliverySettlementAttemptV1>,
        outcome: tracedecay_domain::DeliverySettlementOutcomeV1,
        drop_reason: Option<tracedecay_domain::DeliveryDropReasonV1>,
    ) {
        if let (Some(settlement), Some(attempt)) = (settlement, delivery_attempt) {
            settlement.settle(attempt, outcome, drop_reason);
        }
    }

    async fn observe_incoming_message(&self, value: &serde_json::Value) {
        let Some(method) = value.get("method").and_then(serde_json::Value::as_str) else {
            return;
        };
        if method == "notifications/cancelled" {
            let Some(request_id) = value
                .get("params")
                .and_then(|params| params.get("requestId"))
            else {
                return;
            };
            let Some(request_key) = Self::request_key(request_id) else {
                return;
            };
            let delivery_attempt = self
                .active_requests
                .lock()
                .ok()
                .and_then(|mut active| active.remove(&request_key));
            let Some(delivery_attempt) = delivery_attempt else {
                return;
            };
            let response = JsonRpcResponse::error_with_data(
                request_id.clone(),
                ErrorCode::RequestCancelled,
                "MCP request cancelled".to_owned(),
                Some(json!({"reason_code": "request_cancelled"})),
            );
            if let Ok(mut bytes) = serde_json::to_vec(&response) {
                bytes.push(b'\n');
                match Self::write_all_and_flush(Arc::clone(&self.writer), bytes).await {
                    Ok(()) => Self::settle_work_delivery(
                        self.work_delivery_settlement.as_ref(),
                        delivery_attempt,
                        tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                        Some(tracedecay_domain::DeliveryDropReasonV1::Cancelled),
                    ),
                    Err(_) => Self::settle_work_delivery(
                        self.work_delivery_settlement.as_ref(),
                        delivery_attempt,
                        tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                        Some(tracedecay_domain::DeliveryDropReasonV1::Disconnected),
                    ),
                }
            }
            return;
        }
        let Some(request_key) = value.get("id").and_then(Self::request_key) else {
            return;
        };
        if let Ok(mut active) = self.active_requests.lock() {
            let delivery_attempt = self
                .work_delivery_settlement
                .as_ref()
                .and_then(|settlement| settlement.attempt_for_request(value));
            active.insert(request_key, delivery_attempt);
        }
    }

    async fn wait_for_peer_full_close(writer: Arc<tokio::sync::Mutex<Option<BrokerWriteHalf>>>) {
        loop {
            let full_close = {
                let writer = writer.lock().await;
                let Some(writer) = writer.as_ref() else {
                    return;
                };
                match writer.peer_write_readiness_now().await {
                    // The readiness future is deliberately polled once. If
                    // it is pending, release the writer mutex and retry on
                    // the next 100ms interval so a blocked response write is
                    // never starved by the close monitor.
                    None => false,
                    Some(Ok(ready)) if ready.is_write_closed() => true,
                    Some(Ok(_)) => match writer.consume_write_readiness() {
                        Ok(()) => false,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
                        Err(_) => true,
                    },
                    Some(Err(_)) => true,
                }
            };
            if full_close {
                return;
            }
            // WRITABLE is level-triggered, so avoid a busy loop while a
            // legitimate half-closed one-shot client is still computing its
            // response. No request deadline is imposed here.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}

impl crate::mcp::McpTransport for BrokerStreamTransport {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        if let Some(line) = self.replay.pop_front() {
            return Ok(Some(line));
        }
        self.reader.read_mcp_line().await
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        let mut writer = self.writer.lock().await;
        let writer = writer.as_mut().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "daemon broker transport closed",
            )
        })?;
        writer.write_all(line.as_bytes()).await
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        let mut writer = self.writer.lock().await;
        let writer = writer.as_mut().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "daemon broker transport closed",
            )
        })?;
        writer.flush().await
    }

    fn peer_fully_closed_after_eof(
        &self,
    ) -> impl std::future::Future<Output = ()> + Send + 'static {
        Self::wait_for_peer_full_close(Arc::clone(&self.writer))
    }
}

impl rmcp::transport::Transport<rmcp::RoleServer> for BrokerStreamTransport {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: rmcp::service::TxJsonRpcMessage<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = std::result::Result<(), Self::Error>> + Send + 'static
    {
        let writer = Arc::clone(&self.writer);
        let active_requests = Arc::clone(&self.active_requests);
        let response_lifecycle = self.response_lifecycle.clone();
        let selected_project_responses = self.selected_project_responses.clone();
        let work_delivery_settlement = self.work_delivery_settlement.clone();
        async move {
            let value = serde_json::to_value(&item)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            let request_key = Self::response_request_key(&value);
            let selected_response_lease = match selected_project_responses {
                Some(authority) => authority
                    .take(value.get("id"))
                    .map_err(|error| std::io::Error::other(error.to_string()))?,
                None => None,
            };
            let mut bytes = serde_json::to_vec(&value)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            bytes.push(b'\n');
            let RmcpResponseWrite::Write(delivery_attempt) =
                Self::take_response_write(active_requests, request_key)?
            else {
                return Ok(());
            };
            let response_revoked = selected_response_lease
                .as_ref()
                .map(crate::mcp::server::SelectedProjectResponseLease::revoked)
                .or_else(|| {
                    response_lifecycle
                        .as_ref()
                        .map(crate::mcp::server::ProjectServerResponseLifecycle::response_revoked)
                });
            let write_result = match response_revoked {
                None => Self::write_all_and_flush(writer, bytes)
                    .await
                    .map_err(RmcpResponseWriteFailure::Transport),
                Some(response_revoked) if response_revoked.is_cancelled() => {
                    Err(RmcpResponseWriteFailure::Cancelled)
                }
                Some(response_revoked) => {
                    tokio::select! {
                        biased;
                        () = response_revoked.cancelled() => {
                            Err(RmcpResponseWriteFailure::Cancelled)
                        }
                        result = Self::write_all_and_flush(writer, bytes) => {
                            result.map_err(RmcpResponseWriteFailure::Transport)
                        }
                    }
                }
            };
            if let Some(attempt) = delivery_attempt {
                match &write_result {
                    Ok(()) => Self::settle_work_delivery(
                        work_delivery_settlement.as_ref(),
                        Some(attempt),
                        tracedecay_domain::DeliverySettlementOutcomeV1::Delivered,
                        None,
                    ),
                    Err(RmcpResponseWriteFailure::Cancelled) => Self::settle_work_delivery(
                        work_delivery_settlement.as_ref(),
                        Some(attempt),
                        tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                        Some(tracedecay_domain::DeliveryDropReasonV1::Cancelled),
                    ),
                    Err(RmcpResponseWriteFailure::Transport(_)) => Self::settle_work_delivery(
                        work_delivery_settlement.as_ref(),
                        Some(attempt),
                        tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                        Some(tracedecay_domain::DeliveryDropReasonV1::Disconnected),
                    ),
                }
            }
            write_result.map_err(RmcpResponseWriteFailure::into_io_error)
        }
    }

    async fn receive(&mut self) -> Option<rmcp::service::RxJsonRpcMessage<rmcp::RoleServer>> {
        loop {
            let line = match self.read_line().await {
                Ok(Some(line)) => line,
                Ok(None) => {
                    // A one-shot client may half-close its request side while
                    // still waiting for an in-flight response. Keep rmcp's
                    // receive loop alive until the native transport observes
                    // the peer's full close; otherwise rmcp tears down the
                    // service and strands the request permit.
                    self.peer_fully_closed_after_eof().await;
                    return None;
                }
                Err(error)
                    if tracedecay_usecases::host_admission::is_wire_oversized_io_error(&error) =>
                {
                    let _ =
                        crate::mcp::transport::write_wire_oversized_rejection(self, &error).await;
                    return None;
                }
                Err(error) => {
                    tracing::warn!(%error, "daemon broker MCP transport read failed");
                    return None;
                }
            };
            match serde_json::from_str::<serde_json::Value>(&line) {
                Ok(value) => {
                    self.observe_incoming_message(&value).await;
                    match serde_json::from_value(value) {
                        Ok(message) => return Some(message),
                        Err(error) => {
                            let response = JsonRpcResponse::error(
                                serde_json::Value::Null,
                                ErrorCode::ParseError,
                                format!("failed to parse JSON-RPC request: {error}"),
                            );
                            if let Ok(line) = serde_json::to_string(&response) {
                                let _ = self.write_line(&format!("{line}\n")).await;
                                let _ = self.flush().await;
                            }
                        }
                    }
                }
                Err(error) => {
                    let response = JsonRpcResponse::error(
                        serde_json::Value::Null,
                        ErrorCode::ParseError,
                        format!("failed to parse JSON-RPC request: {error}"),
                    );
                    if let Ok(line) = serde_json::to_string(&response) {
                        let _ = self.write_line(&format!("{line}\n")).await;
                        let _ = self.flush().await;
                    }
                }
            }
        }
    }

    async fn close(&mut self) -> std::result::Result<(), Self::Error> {
        self.writer.lock().await.take();
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod peer_close_tests {
    use super::*;
    use crate::mcp::McpTransport;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use tracedecay_application::{
        ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1,
    };
    use tracedecay_domain::{ObservabilityPayloadV1, ProjectId};
    use tracedecay_usecases::observability::{
        BoundedDeliverySettlementRecorderV1, BoundedObservabilityProducerV1,
        DeliverySettlementAuthorityV1, ObservabilityProducerIdentityV1,
        RegisteredObservabilityPortV1,
    };

    struct DeliverySettlementFixture {
        _pin: tracedecay_runtime_core::config::PinnedUserDataDir,
        _project: tempfile::TempDir,
        recorder: Arc<BoundedDeliverySettlementRecorderV1>,
        authority: Arc<DeliverySettlementAuthorityV1>,
        producer: Arc<BoundedObservabilityProducerV1>,
        db: crate::global_db::RegisteredGlobalDbLeaseV1,
        project_id: ProjectId,
    }

    async fn delivery_settlement_fixture() -> DeliverySettlementFixture {
        let pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project");
        let project_id = ProjectId::new("project.rmcp.delivery").expect("project id");
        let runtime = crate::global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
            tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
            project.path(),
            project_id.clone(),
        )
        .await
        .expect("registered runtime");
        let db = runtime.project_database_arc().expect("project database");
        let identity = ObservabilityProducerIdentityV1 {
            authorized_scope_ref: project_id.as_str().to_owned(),
            process_boot_id: "boot:rmcp-delivery".to_owned(),
            producer_revision: "rmcp-delivery-producer.v1".to_owned(),
            configuration_revision: "rmcp-delivery-config.v1".to_owned(),
            policy_revision: "rmcp-delivery-policy.v1".to_owned(),
        };
        let producer = Arc::new(
            BoundedObservabilityProducerV1::start(db.clone(), identity.clone(), 8)
                .expect("producer"),
        );
        let authority = Arc::new(
            DeliverySettlementAuthorityV1::new(db.clone(), Arc::clone(&producer), identity)
                .expect("settlement authority"),
        );
        let recorder = Arc::new(
            BoundedDeliverySettlementRecorderV1::start(Arc::clone(&authority), 8)
                .expect("settlement recorder"),
        );
        DeliverySettlementFixture {
            _pin: pin,
            _project: project,
            recorder,
            authority,
            producer,
            db,
            project_id,
        }
    }

    async fn settled_fanout(
        fixture: DeliverySettlementFixture,
    ) -> tracedecay_domain::WorkDeliveryFanoutObservedV1 {
        let summary = fixture
            .recorder
            .shutdown()
            .await
            .expect("drain settlement recorder");
        assert_eq!(summary.settled, 1, "one RMCP Work response must settle");
        assert_eq!(summary.failed, 0, "transport settlement must persist");

        drop(fixture.recorder);
        drop(fixture.authority);
        let Ok(producer) = Arc::try_unwrap(fixture.producer) else {
            panic!("settlement authority releases producer")
        };
        producer
            .shutdown()
            .await
            .expect("flush observability producer");

        let page = RegisteredObservabilityPortV1::new(fixture.db.as_ref())
            .query(ObservabilityQueryV1 {
                authorized_scope_ref: fixture.project_id.as_str().to_owned(),
                event_kinds: vec!["work.delivery_fanout.observed.v1".to_owned()],
                horizon: ObservabilityHorizonV1 {
                    since_micros: 0,
                    until_micros: i64::MAX,
                },
                after_watermark: None,
                limit: 8,
            })
            .await
            .expect("read settled delivery fanout");
        assert_eq!(
            page.events.len(),
            1,
            "settlement must be durably observable"
        );
        let ObservabilityPayloadV1::WorkDeliveryFanout(fanout) = page.events[0].payload.clone()
        else {
            panic!("expected Work delivery fanout observation");
        };
        fanout
    }

    #[tokio::test]
    async fn full_close_wait_ignores_request_half_close() {
        let (server, client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
        let transport = BrokerStreamTransport::new(BrokerStream::Unix(server));
        let (client_reader, mut client_writer) = client.into_split();

        client_writer
            .shutdown()
            .await
            .expect("half-close client request side");
        let mut peer_close = Box::pin(transport.peer_fully_closed_after_eof());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut peer_close)
                .await
                .is_err(),
            "request-half close must not cancel the response"
        );

        drop(client_writer);
        drop(client_reader);
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut peer_close)
            .await
            .expect("full peer close must be observed");
    }

    #[tokio::test]
    async fn rmcp_receive_waits_for_full_close_after_request_half_close() {
        let (server, client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
        let mut transport = BrokerStreamTransport::new(BrokerStream::Unix(server));
        let (client_reader, mut client_writer) = client.into_split();

        client_writer
            .shutdown()
            .await
            .expect("half-close client request side");
        let mut receive = Box::pin(<BrokerStreamTransport as rmcp::transport::Transport<
            rmcp::RoleServer,
        >>::receive(&mut transport));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut receive)
                .await
                .is_err(),
            "rmcp receive must not treat a request-half close as full peer loss"
        );

        drop(client_writer);
        drop(client_reader);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), &mut receive)
                .await
                .expect("rmcp receive must finish after full peer close")
                .is_none()
        );
    }

    #[tokio::test]
    async fn rmcp_selected_target_retirement_between_handler_and_send_suppresses_response() {
        let (server, client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
        let active_lifecycle = crate::mcp::server::ProjectServerResponseLifecycle::default();
        let target_lifecycle = crate::mcp::server::ProjectServerResponseLifecycle::default();
        let selected_responses =
            crate::mcp::server::RmcpSelectedProjectResponseAuthority::default();
        let mut transport = BrokerStreamTransport::new(BrokerStream::Unix(server))
            .with_project_response_lifecycle(active_lifecycle.clone())
            .with_rmcp_selected_project_responses(selected_responses.clone());
        let (client_reader, mut client_writer) = client.into_split();

        client_writer
            .write_all(
                br#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"tracedecay_files","arguments":{}}}
"#,
            )
            .await
            .expect("selected-target request");
        client_writer
            .flush()
            .await
            .expect("flush selected-target request");
        assert!(
            <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::receive(
                &mut transport
            )
            .await
            .is_some(),
            "RMCP transport must retain the selected-target response slot"
        );

        let response_guard = Arc::clone(target_lifecycle.response_gate())
            .read_owned()
            .await;
        selected_responses
            .retain(
                &serde_json::json!(7),
                crate::mcp::server::SelectedProjectResponseLease::new(
                    response_guard,
                    target_lifecycle.response_revoked().clone(),
                ),
            )
            .expect("handler-to-transport selected response handoff");

        // The active connection remains live. Only the selected target is
        // retired after handler completion but before rmcp calls `send`.
        target_lifecycle.revoke();
        assert!(!active_lifecycle.response_revoked().is_cancelled());
        let target_drain = tokio::spawn({
            let target_lifecycle = target_lifecycle.clone();
            async move { target_lifecycle.wait_for_request_drain().await }
        });
        tokio::task::yield_now().await;
        assert!(
            !target_drain.is_finished(),
            "the handler lease must keep target retirement from completing before send"
        );

        let response = serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": {"content": [{"type": "text", "text": "must-not-leak"}]}
        }))
        .expect("typed selected-target response");
        let error = <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::send(
            &mut transport,
            response,
        )
        .await
        .expect_err("retired selected target must suppress its response");
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        tokio::time::timeout(std::time::Duration::from_secs(1), target_drain)
            .await
            .expect("selected response lease must release after suppression")
            .expect("join target drain");

        let mut client_reader = tokio::io::BufReader::new(client_reader);
        let mut line = String::new();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                client_reader.read_line(&mut line),
            )
            .await
            .is_err(),
            "the live active server must not authorize a retired target payload: {line}"
        );
    }

    #[tokio::test]
    async fn rmcp_selected_target_response_does_not_fall_back_to_retired_active_server() {
        let (server, client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
        let active_lifecycle = crate::mcp::server::ProjectServerResponseLifecycle::default();
        let target_lifecycle = crate::mcp::server::ProjectServerResponseLifecycle::default();
        let selected_responses =
            crate::mcp::server::RmcpSelectedProjectResponseAuthority::default();
        let mut transport = BrokerStreamTransport::new(BrokerStream::Unix(server))
            .with_project_response_lifecycle(active_lifecycle.clone())
            .with_rmcp_selected_project_responses(selected_responses.clone());
        let (client_reader, mut client_writer) = client.into_split();

        client_writer
            .write_all(
                br#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"tracedecay_files","arguments":{}}}
"#,
            )
            .await
            .expect("selected-target request");
        client_writer
            .flush()
            .await
            .expect("flush selected-target request");
        assert!(
            <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::receive(
                &mut transport
            )
            .await
            .is_some(),
            "RMCP transport must retain the selected-target response slot"
        );

        let response_guard = Arc::clone(target_lifecycle.response_gate())
            .read_owned()
            .await;
        selected_responses
            .retain(
                &serde_json::json!(8),
                crate::mcp::server::SelectedProjectResponseLease::new(
                    response_guard,
                    target_lifecycle.response_revoked().clone(),
                ),
            )
            .expect("handler-to-transport selected response handoff");

        // The accepted connection's original project retires after target
        // selection. Its lifecycle must neither suppress nor authorize a
        // response owned by the still-live selected target.
        active_lifecycle.revoke();
        assert!(!target_lifecycle.response_revoked().is_cancelled());

        let response = serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 8,
            "result": {"content": [{"type": "text", "text": "target-owned"}]}
        }))
        .expect("typed selected-target response");
        <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::send(
            &mut transport,
            response,
        )
        .await
        .expect("live selected target must retain response authority");

        let mut client_reader = tokio::io::BufReader::new(client_reader);
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client_reader.read_line(&mut line),
        )
        .await
        .expect("selected-target response timeout")
        .expect("read selected-target response");
        assert!(line.contains("target-owned"), "unexpected response: {line}");
    }

    #[tokio::test]
    async fn rmcp_initialize_then_work_response_settles_after_transport_flush() {
        let fixture = delivery_settlement_fixture().await;
        let (server, client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
        let mut transport = BrokerStreamTransport::new(BrokerStream::Unix(server))
            .with_rmcp_work_delivery_settlement(
                crate::mcp::server::RmcpWorkDeliverySettlement::new(
                    Some(Arc::clone(&fixture.recorder)),
                    "rmcp-transport-settlement-test".to_owned(),
                ),
            );
        let (client_reader, mut client_writer) = client.into_split();

        client_writer
            .write_all(
                br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"settlement-test","version":"1"}}}
"#,
            )
            .await
            .expect("initialize request");
        client_writer
            .flush()
            .await
            .expect("flush initialize request");
        assert!(
            <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::receive(
                &mut transport
            )
            .await
            .is_some(),
            "RMCP transport must accept initialize before Work"
        );

        client_writer
            .write_all(
                br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tracedecay_work_start_attempt","arguments":{}}}
"#,
            )
            .await
            .expect("Work request");
        client_writer.flush().await.expect("flush Work request");
        assert!(
            <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::receive(
                &mut transport
            )
            .await
            .is_some(),
            "RMCP transport must retain the pending Work response"
        );

        let response = serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {"content": [{"type": "text", "text": "delivered"}]}
        }))
        .expect("typed RMCP Work response");
        <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::send(
            &mut transport,
            response,
        )
        .await
        .expect("transport response write and flush");

        let mut client_reader = tokio::io::BufReader::new(client_reader);
        let mut line = String::new();
        client_reader
            .read_line(&mut line)
            .await
            .expect("flushed Work response");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&line).expect("response JSON")["id"],
            serde_json::json!(2),
            "the client must observe the response before it is recorded as delivered"
        );

        drop(transport);
        let fanout = settled_fanout(fixture).await;
        assert_eq!(fanout.delivered, 1);
        assert_eq!(fanout.dropped, 0);
        assert_eq!(fanout.unknown, 0);
    }

    /// The peer vanishes after the daemon accepted a Work request but before
    /// the response reaches the wire. The attempt must settle as a typed drop
    /// rather than being stranded as unknown or reported as delivered.
    #[tokio::test]
    async fn rmcp_peer_disconnect_mid_delivery_settles_dropped_rather_than_unknown() {
        let fixture = delivery_settlement_fixture().await;
        let (server, client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
        let mut transport = BrokerStreamTransport::new(BrokerStream::Unix(server))
            .with_rmcp_work_delivery_settlement(
                crate::mcp::server::RmcpWorkDeliverySettlement::new(
                    Some(Arc::clone(&fixture.recorder)),
                    "rmcp-transport-disconnect-test".to_owned(),
                ),
            );
        let (client_reader, mut client_writer) = client.into_split();

        client_writer
            .write_all(
                br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tracedecay_work_start_attempt","arguments":{}}}
"#,
            )
            .await
            .expect("Work request");
        client_writer.flush().await.expect("flush Work request");
        assert!(
            <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::receive(
                &mut transport
            )
            .await
            .is_some(),
            "RMCP transport must retain the pending Work response"
        );

        // The client is gone before the daemon can write its response.
        drop(client_reader);
        drop(client_writer);

        let response = serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {"content": [{"type": "text", "text": "never observed"}]}
        }))
        .expect("typed RMCP Work response");
        let write = <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::send(
            &mut transport,
            response,
        )
        .await;
        assert!(
            write.is_err(),
            "a disconnected peer must fail the response write instead of reporting delivery"
        );

        drop(transport);
        let fanout = settled_fanout(fixture).await;
        assert_eq!(
            fanout.delivered, 0,
            "a response the client never observed is never delivered"
        );
        assert_eq!(
            fanout.dropped, 1,
            "disconnect settles the attempt as dropped"
        );
        assert_eq!(
            fanout.unknown, 0,
            "disconnect settles a typed terminal rather than stranding the attempt"
        );
    }

    /// A client cancels an in-flight Work request. The transport must settle
    /// the pending attempt as a cancelled drop and hand the client a typed
    /// cancellation, never leaving the attempt open for a later response.
    #[tokio::test]
    async fn rmcp_client_cancellation_settles_dropped_without_stranding_the_attempt() {
        let fixture = delivery_settlement_fixture().await;
        let (server, client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
        let mut transport = BrokerStreamTransport::new(BrokerStream::Unix(server))
            .with_rmcp_work_delivery_settlement(
                crate::mcp::server::RmcpWorkDeliverySettlement::new(
                    Some(Arc::clone(&fixture.recorder)),
                    "rmcp-transport-cancellation-test".to_owned(),
                ),
            );
        let (client_reader, mut client_writer) = client.into_split();

        client_writer
            .write_all(
                br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tracedecay_work_start_attempt","arguments":{}}}
"#,
            )
            .await
            .expect("Work request");
        client_writer.flush().await.expect("flush Work request");
        assert!(
            <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::receive(
                &mut transport
            )
            .await
            .is_some(),
            "RMCP transport must retain the pending Work response"
        );

        client_writer
            .write_all(
                br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2,"reason":"client cancelled"}}
"#,
            )
            .await
            .expect("cancellation notification");
        client_writer.flush().await.expect("flush cancellation");

        // Settlement happens while the transport observes the notification,
        // before the message itself is handed to `rmcp`.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            <BrokerStreamTransport as rmcp::transport::Transport<rmcp::RoleServer>>::receive(
                &mut transport,
            ),
        )
        .await;

        let mut client_reader = tokio::io::BufReader::new(client_reader);
        let mut line = String::new();
        client_reader
            .read_line(&mut line)
            .await
            .expect("flushed cancellation response");
        let cancelled =
            serde_json::from_str::<serde_json::Value>(&line).expect("cancellation response JSON");
        assert_eq!(
            cancelled["id"],
            serde_json::json!(2),
            "the cancelled request must receive its own typed terminal"
        );
        assert_eq!(
            cancelled["error"]["data"]["reason_code"],
            serde_json::json!("request_cancelled"),
            "the client must observe a typed cancellation reason"
        );

        drop(transport);
        let fanout = settled_fanout(fixture).await;
        assert_eq!(
            fanout.delivered, 0,
            "a cancelled request is never reported as delivered"
        );
        assert_eq!(
            fanout.dropped, 1,
            "cancellation settles the pending attempt as dropped"
        );
        assert_eq!(
            fanout.unknown, 0,
            "cancellation settles a typed terminal rather than stranding the attempt"
        );
    }
}
