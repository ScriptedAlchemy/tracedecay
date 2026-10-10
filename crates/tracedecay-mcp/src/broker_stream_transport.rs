//! Broker-side MCP transport over the daemon protocol's native stream.
//!
//! The transport owns bounded framing, RMCP translation, response revocation,
//! and delivery settlement at the write-and-flush boundary. Daemon-owned
//! lifecycle authorities implement the narrow collaboration traits below.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use rmcp::model::{ClientNotification, ClientRequest, ProtocolVersion, ServerConfig, ServerResult};
use rmcp::service::{NotificationContext, RequestContext};
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tracedecay_daemon_protocol::{BrokerReadHalf, BrokerStream, BrokerWriteHalf};
use tracedecay_framing::{
    BoundedLineReader, MAX_MCP_JSONRPC_FRAME_BYTES, MCP_OVERSIZE_ID_INSPECT_BYTES,
    is_wire_oversized_io_error, wire_oversized_io_error_with_prefix,
};
use tracedecay_runtime_core::cancellation::CancellationToken;

use crate::lifecycle::ProjectServerResponseLifecycle;
use crate::server::{RmcpSelectedProjectResponseAuthority, RmcpWorkDeliverySettlement};
use tracedecay_domain::errors::TraceDecayError;

use crate::{ErrorCode, JsonRpcDecodeError, JsonRpcResponse, McpTransport};

/// Response-revocation authority retained for one selected project server.
pub trait BrokerResponseLifecycle: Send + Sync {
    fn response_revoked(&self) -> &CancellationToken;
}

/// Selected-project response lease held through the transport write boundary.
pub trait BrokerSelectedResponseLease: Send + Sync {
    fn response_revoked(&self) -> &CancellationToken;
}

/// Connection-local handoff from selected-project dispatch to response write.
pub trait BrokerSelectedResponseAuthority: Send + Sync {
    fn take_response(
        &self,
        id: Option<&serde_json::Value>,
    ) -> std::io::Result<Option<Box<dyn BrokerSelectedResponseLease>>>;
}

/// Work-delivery ledger settled only after the response write is known.
pub trait BrokerWorkDeliverySettlement: Send + Sync {
    fn attempt_for_request(
        &self,
        request: &serde_json::Value,
    ) -> Option<tracedecay_domain::DeliverySettlementAttemptV1>;

    fn settle(
        &self,
        attempt: tracedecay_domain::DeliverySettlementAttemptV1,
        outcome: tracedecay_domain::DeliverySettlementOutcomeV1,
        drop_reason: Option<tracedecay_domain::DeliveryDropReasonV1>,
    );
}

impl BrokerResponseLifecycle for ProjectServerResponseLifecycle {
    fn response_revoked(&self) -> &CancellationToken {
        ProjectServerResponseLifecycle::response_revoked(self)
    }
}

impl<L: BrokerSelectedResponseLease + 'static> BrokerSelectedResponseAuthority
    for RmcpSelectedProjectResponseAuthority<L>
{
    fn take_response(
        &self,
        id: Option<&serde_json::Value>,
    ) -> std::io::Result<Option<Box<dyn BrokerSelectedResponseLease>>> {
        self.take(id)
            .map(|lease| lease.map(|lease| Box::new(lease) as Box<dyn BrokerSelectedResponseLease>))
            .map_err(selected_response_io_error)
    }
}

fn selected_response_io_error(error: TraceDecayError) -> std::io::Error {
    std::io::Error::other(error)
}

impl BrokerWorkDeliverySettlement for RmcpWorkDeliverySettlement {
    fn attempt_for_request(
        &self,
        request: &serde_json::Value,
    ) -> Option<tracedecay_domain::DeliverySettlementAttemptV1> {
        RmcpWorkDeliverySettlement::attempt_for_request(self, request)
    }

    fn settle(
        &self,
        attempt: tracedecay_domain::DeliverySettlementAttemptV1,
        outcome: tracedecay_domain::DeliverySettlementOutcomeV1,
        drop_reason: Option<tracedecay_domain::DeliveryDropReasonV1>,
    ) {
        RmcpWorkDeliverySettlement::settle(self, attempt, outcome, drop_reason);
    }
}

pub struct BrokerStreamTransport {
    // Every daemon read of this transport races something else in a
    // `tokio::select!`, draining, an owner open, a completed handler. The
    // bounded reader owns the partial-frame accumulator so a read dropped by a
    // lost race resumes instead of restarting mid-frame and desynchronizing
    // JSON-RPC framing for the rest of the connection.
    reader: BoundedLineReader<tokio::io::BufReader<BrokerReadHalf>>,
    writer: Arc<tokio::sync::Mutex<Option<BrokerWriteHalf>>>,
    active_requests: ActiveRmcpRequests,
    replay: VecDeque<String>,
    response_lifecycle: Option<Arc<dyn BrokerResponseLifecycle>>,
    selected_project_responses: Option<Arc<dyn BrokerSelectedResponseAuthority>>,
    work_delivery_settlement: Option<Arc<dyn BrokerWorkDeliverySettlement>>,
}

type ActiveRmcpRequests = Arc<std::sync::Mutex<HashMap<String, ActiveRmcpRequest>>>;

struct ActiveRmcpRequest {
    delivery_attempt: Option<tracedecay_domain::DeliverySettlementAttemptV1>,
    cancelled: bool,
}

#[derive(Clone)]
struct BrokerResponseWriter {
    writer: Arc<tokio::sync::Mutex<Option<BrokerWriteHalf>>>,
    active_requests: ActiveRmcpRequests,
    response_lifecycle: Option<Arc<dyn BrokerResponseLifecycle>>,
    selected_project_responses: Option<Arc<dyn BrokerSelectedResponseAuthority>>,
    work_delivery_settlement: Option<Arc<dyn BrokerWorkDeliverySettlement>>,
}

struct BrokerStreamService<S> {
    inner: S,
    responses: BrokerResponseWriter,
}

enum RmcpResponseWrite {
    Suppressed,
    Write(ActiveRmcpRequest),
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
    pub fn new(stream: BrokerStream) -> Self {
        let (reader, writer) = stream.into_owned_split();
        Self {
            reader: BoundedLineReader::new(tokio::io::BufReader::new(reader)),
            writer: Arc::new(tokio::sync::Mutex::new(Some(writer))),
            active_requests: Arc::new(std::sync::Mutex::new(HashMap::new())),
            replay: VecDeque::new(),
            response_lifecycle: None,
            selected_project_responses: None,
            work_delivery_settlement: None,
        }
    }

    fn response_writer(&self) -> BrokerResponseWriter {
        BrokerResponseWriter {
            writer: Arc::clone(&self.writer),
            active_requests: Arc::clone(&self.active_requests),
            response_lifecycle: self.response_lifecycle.clone(),
            selected_project_responses: self.selected_project_responses.clone(),
            work_delivery_settlement: self.work_delivery_settlement.clone(),
        }
    }

    /// RMCP suppresses cancelled responses; the broker still owes a settled terminal.
    pub async fn serve<S>(
        self,
        service: S,
    ) -> Result<
        rmcp::service::RunningService<rmcp::RoleServer, impl rmcp::Service<rmcp::RoleServer>>,
        Box<rmcp::service::ServerInitializeError>,
    >
    where
        S: rmcp::Service<rmcp::RoleServer>,
    {
        let service = BrokerStreamService {
            inner: service,
            responses: self.response_writer(),
        };
        crate::server::serve_guarded_rmcp_connection(service, self).await
    }

    pub fn push_replay(&mut self, line: String) -> std::io::Result<()> {
        if line.len() > MAX_MCP_JSONRPC_FRAME_BYTES {
            let prefix = line.as_bytes()[..line.len().min(MCP_OVERSIZE_ID_INSPECT_BYTES)].to_vec();
            return Err(wire_oversized_io_error_with_prefix(prefix));
        }
        self.replay.push_back(line);
        Ok(())
    }

    #[must_use]
    pub fn with_project_response_lifecycle<T>(mut self, lifecycle: T) -> Self
    where
        T: BrokerResponseLifecycle + 'static,
    {
        self.response_lifecycle = Some(Arc::new(lifecycle));
        self
    }

    #[must_use]
    pub fn with_rmcp_work_delivery_settlement<T>(mut self, settlement: T) -> Self
    where
        T: BrokerWorkDeliverySettlement + 'static,
    {
        self.work_delivery_settlement = Some(Arc::new(settlement));
        self
    }

    #[must_use]
    pub fn with_rmcp_selected_project_responses<T>(mut self, responses: T) -> Self
    where
        T: BrokerSelectedResponseAuthority + 'static,
    {
        self.selected_project_responses = Some(Arc::new(responses));
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

    fn outbound_response_id(
        item: &rmcp::service::TxJsonRpcMessage<rmcp::RoleServer>,
    ) -> Option<serde_json::Value> {
        match item {
            rmcp::model::JsonRpcMessage::Response(response) => {
                Some(response.id.clone().into_json_value())
            }
            rmcp::model::JsonRpcMessage::Error(error) => error
                .id
                .clone()
                .map(rmcp::model::NumberOrString::into_json_value),
            rmcp::model::JsonRpcMessage::Request(_)
            | rmcp::model::JsonRpcMessage::Notification(_) => None,
        }
    }

    fn typed_response_request_key(
        item: &rmcp::service::TxJsonRpcMessage<rmcp::RoleServer>,
        response_id: Option<&serde_json::Value>,
    ) -> Option<String> {
        match item {
            rmcp::model::JsonRpcMessage::Response(_) | rmcp::model::JsonRpcMessage::Error(_) => {
                response_id.and_then(Self::request_key)
            }
            rmcp::model::JsonRpcMessage::Request(_)
            | rmcp::model::JsonRpcMessage::Notification(_) => None,
        }
    }

    fn take_response_write(
        active_requests: &ActiveRmcpRequests,
        request_key: Option<String>,
    ) -> std::io::Result<RmcpResponseWrite> {
        let Some(request_key) = request_key else {
            return Ok(RmcpResponseWrite::Write(ActiveRmcpRequest {
                delivery_attempt: None,
                cancelled: false,
            }));
        };
        let response = active_requests
            .lock()
            .map_err(|_| std::io::Error::other("active RMCP request registry poisoned"))?
            .remove(&request_key);
        match response {
            Some(request) => Ok(RmcpResponseWrite::Write(request)),
            None => Ok(RmcpResponseWrite::Suppressed),
        }
    }

    fn settle_work_delivery(
        settlement: Option<&dyn BrokerWorkDeliverySettlement>,
        delivery_attempt: Option<tracedecay_domain::DeliverySettlementAttemptV1>,
        outcome: tracedecay_domain::DeliverySettlementOutcomeV1,
        drop_reason: Option<tracedecay_domain::DeliveryDropReasonV1>,
    ) {
        if let (Some(settlement), Some(attempt)) = (settlement, delivery_attempt) {
            settlement.settle(attempt, outcome, drop_reason);
        }
    }

    fn observe_incoming_message(&self, value: &serde_json::Value) {
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
            if let Ok(mut active) = self.active_requests.lock()
                && let Some(request) = active.get_mut(&request_key)
            {
                // Retain the request until its handler returns, including Git lock cleanup.
                request.cancelled = true;
            }
            return;
        }
        let Some(request_key) = value.get("id").and_then(Self::request_key) else {
            return;
        };
        if let Ok(mut active) = self.active_requests.lock() {
            let delivery_attempt = self
                .work_delivery_settlement
                .as_deref()
                .and_then(|settlement| settlement.attempt_for_request(value));
            active.insert(
                request_key,
                ActiveRmcpRequest {
                    delivery_attempt,
                    cancelled: false,
                },
            );
        }
    }

    /// Resolves once every accepted request has settled, delivered,
    /// suppressed, or answered with its typed cancellation. After the peer
    /// half-closes its request side, a connection in that state, including
    /// one that only carried notifications or refused frames, has nothing left
    /// it could ever deliver, so waiting for the peer's full close would only
    /// strand clients that hold their read half open awaiting the daemon's EOF
    /// (a cancelling client does exactly that).
    #[tracing::instrument(name = "daemon.broker.eof_settled_wait", level = "trace", skip_all)]
    async fn wait_for_accepted_requests_settled(active_requests: ActiveRmcpRequests) {
        loop {
            if active_requests.lock().is_ok_and(|active| active.is_empty()) {
                return;
            }
            // Settlement lands through independently spawned response
            // writers; poll on the same bounded interval the
            // full-close monitor uses rather than threading a notifier
            // through every removal site.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
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

impl BrokerResponseWriter {
    fn send(
        self,
        item: rmcp::service::TxJsonRpcMessage<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = std::result::Result<(), std::io::Error>> + Send + 'static
    {
        let writer = Arc::clone(&self.writer);
        let active_requests = Arc::clone(&self.active_requests);
        let response_lifecycle = self.response_lifecycle.clone();
        let selected_project_responses = self.selected_project_responses.clone();
        let work_delivery_settlement = self.work_delivery_settlement.clone();
        tracing::Instrument::instrument(
            async move {
                let response_id = BrokerStreamTransport::outbound_response_id(&item);
                let is_response = matches!(
                    item,
                    rmcp::model::JsonRpcMessage::Response(_)
                        | rmcp::model::JsonRpcMessage::Error(_)
                );
                let request_key =
                    BrokerStreamTransport::typed_response_request_key(&item, response_id.as_ref());
                let selected_response_lease = match selected_project_responses {
                    Some(authority) => authority.take_response(response_id.as_ref())?,
                    None => None,
                };
                let RmcpResponseWrite::Write(request) =
                    BrokerStreamTransport::take_response_write(&active_requests, request_key)?
                else {
                    return Ok(());
                };
                let bytes = if request.cancelled {
                    let id = response_id.ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "cancelled MCP response has no request id",
                        )
                    })?;
                    serde_json::to_vec(&JsonRpcResponse::error_with_data(
                        id,
                        ErrorCode::RequestCancelled,
                        "MCP request cancelled".to_owned(),
                        Some(json!({"reason_code": "request_cancelled"})),
                    ))
                } else {
                    serde_json::to_vec(&item)
                };
                let mut bytes = bytes
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
                bytes.push(b'\n');
                let response_revoked = selected_response_lease
                    .as_deref()
                    .map(BrokerSelectedResponseLease::response_revoked)
                    .or_else(|| {
                        response_lifecycle
                            .as_deref()
                            .map(BrokerResponseLifecycle::response_revoked)
                    });
                let write_result = match response_revoked {
                    None => BrokerStreamTransport::write_all_and_flush(writer, bytes)
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
                            result = BrokerStreamTransport::write_all_and_flush(writer, bytes) => {
                                result.map_err(RmcpResponseWriteFailure::Transport)
                            }
                        }
                    }
                };
                if let Some(attempt) = request.delivery_attempt {
                    match &write_result {
                        Ok(()) if request.cancelled => BrokerStreamTransport::settle_work_delivery(
                            work_delivery_settlement.as_deref(),
                            Some(attempt),
                            tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                            Some(tracedecay_domain::DeliveryDropReasonV1::Cancelled),
                        ),
                        Ok(()) => BrokerStreamTransport::settle_work_delivery(
                            work_delivery_settlement.as_deref(),
                            Some(attempt),
                            tracedecay_domain::DeliverySettlementOutcomeV1::Delivered,
                            None,
                        ),
                        Err(RmcpResponseWriteFailure::Cancelled) => {
                            BrokerStreamTransport::settle_work_delivery(
                                work_delivery_settlement.as_deref(),
                                Some(attempt),
                                tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                                Some(tracedecay_domain::DeliveryDropReasonV1::Cancelled),
                            );
                        }
                        Err(RmcpResponseWriteFailure::Transport(_)) => {
                            BrokerStreamTransport::settle_work_delivery(
                                work_delivery_settlement.as_deref(),
                                Some(attempt),
                                tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                                Some(tracedecay_domain::DeliveryDropReasonV1::Disconnected),
                            );
                        }
                    }
                }
                match write_result {
                    // The client closed before its response, as a hook does
                    // when it abandons the call at its own deadline and spools
                    // the event for replay. A work delivery riding on the
                    // response is settled above as `Disconnected`; nothing is
                    // left to deliver and the daemon did not fail, so the
                    // serve loop must not report a transport error. A request
                    // or notification the daemon initiates still fails, so its
                    // sender never believes a vanished peer received it.
                    Err(RmcpResponseWriteFailure::Transport(error))
                        if is_response
                            && matches!(
                                error.kind(),
                                std::io::ErrorKind::BrokenPipe
                                    | std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::NotConnected
                            ) =>
                    {
                        tracing::debug!(
                            %error,
                            "client closed before its response; response dropped as disconnected"
                        );
                        Ok(())
                    }
                    result => result.map_err(RmcpResponseWriteFailure::into_io_error),
                }
            },
            tracing::trace_span!("daemon.broker.send"),
        )
    }
}

impl<S: rmcp::Service<rmcp::RoleServer>> rmcp::Service<rmcp::RoleServer>
    for BrokerStreamService<S>
{
    async fn handle_request(
        &self,
        request: ClientRequest,
        context: RequestContext<rmcp::RoleServer>,
    ) -> Result<ServerResult, rmcp::ErrorData> {
        let id = context.id.clone();
        let result = self.inner.handle_request(request, context).await;
        let response = match &result {
            Ok(result) => rmcp::model::JsonRpcMessage::response(result.clone(), id),
            Err(error) => rmcp::model::JsonRpcMessage::error(error.clone(), Some(id)),
        };
        self.responses
            .clone()
            .send(response)
            .await
            .map_err(|error| {
                rmcp::ErrorData::internal_error(
                    format!("broker response delivery failed: {error}"),
                    None,
                )
            })?;
        result
    }

    fn handle_notification(
        &self,
        notification: ClientNotification,
        context: NotificationContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<(), rmcp::ErrorData>> + Send + '_ {
        self.inner.handle_notification(notification, context)
    }

    fn get_info(&self) -> ServerConfig {
        self.inner.get_info()
    }

    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        self.inner.supported_protocol_versions()
    }
}

impl McpTransport for BrokerStreamTransport {
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
    ) -> impl std::future::Future<Output = std::io::Result<()>> + Send + 'static {
        self.response_writer().send(item)
    }

    #[tracing::instrument(name = "daemon.broker.receive", level = "trace", skip_all)]
    async fn receive(&mut self) -> Option<rmcp::service::RxJsonRpcMessage<rmcp::RoleServer>> {
        loop {
            let line = match self.read_line().await {
                Ok(Some(line)) => line,
                Ok(None) => {
                    // A one-shot client may half-close its request side while
                    // still waiting for an in-flight response. Keep rmcp's
                    // receive loop alive until the native transport observes
                    // the peer's full close; otherwise rmcp tears down the
                    // service and strands the request permit. Once every
                    // accepted request has settled, though, the half-open
                    // connection has nothing left to deliver, and a client
                    // that reads until daemon EOF, a cancelling client does,
                    // needs this side to close first.
                    let settled =
                        Self::wait_for_accepted_requests_settled(Arc::clone(&self.active_requests));
                    let peer_full_close = self.peer_fully_closed_after_eof();
                    tokio::select! {
                        () = peer_full_close => {
                        }
                        () = settled => {
                        }
                    }
                    return None;
                }
                Err(error) if is_wire_oversized_io_error(&error) => {
                    let _ = crate::transport::write_wire_oversized_rejection(self, &error).await;
                    return None;
                }
                Err(error) => {
                    tracing::warn!(%error, "daemon broker MCP transport read failed");
                    return None;
                }
            };
            // The envelope rule runs before cancellation matching so a frame
            // with a foreign protocol version is never treated as work, and a
            // refused frame is answered here rather than registered as an
            // accepted request that EOF would wait on.
            let decoded = match serde_json::from_str::<serde_json::Value>(&line) {
                Ok(value) => {
                    crate::jsonrpc::decode_envelope(&value).map(|message| (value, message))
                }
                Err(error) => Err(JsonRpcDecodeError::Parse(error)),
            };
            match decoded {
                Ok((value, message)) => {
                    self.observe_incoming_message(&value);
                    return Some(message);
                }
                Err(error) => {
                    if let Ok(line) = serde_json::to_string(&error.into_response()) {
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

#[cfg(test)]
mod selected_response_error_tests {
    use super::*;

    #[test]
    fn io_boundary_retains_typed_project_route_classification() {
        let error = TraceDecayError::project_route(
            "project_route_unavailable",
            true,
            "selected response authority is warming",
        );

        let error = selected_response_io_error(error);
        let source = error
            .get_ref()
            .and_then(|source| source.downcast_ref::<TraceDecayError>())
            .expect("I/O error must retain the typed TraceDecay source");

        assert_eq!(
            source.project_route_context(),
            Some((
                "project_route_unavailable",
                true,
                "selected response authority is warming",
            ))
        );
    }
}
