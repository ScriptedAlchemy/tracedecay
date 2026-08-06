//! Broker-side MCP transport (`BrokerStreamTransport`) bridging a split
//! `BrokerStream` to the daemon's `McpTransport` and rmcp `Transport` traits.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic,
//! signatures, or behavior changed.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use tokio::io::AsyncWriteExt;

use crate::mcp::{JsonRpcResponse, McpTransport};

use super::BrokerStream;
use super::transport::{BrokerReadHalf, BrokerWriteHalf};
use super::*;

const RMCP_WRITE_DEADLINE: Duration = Duration::from_millis(100);

pub(super) struct BrokerStreamTransport {
    reader: tokio::io::BufReader<BrokerReadHalf>,
    writer: Arc<tokio::sync::Mutex<Option<BrokerWriteHalf>>>,
    active_requests: Arc<std::sync::Mutex<HashSet<String>>>,
    request_ingress: crate::mcp::server::RmcpRequestIngressRegistry,
    replay: VecDeque<String>,
    response_lifecycle: Option<crate::mcp::server::ProjectServerResponseLifecycle>,
}

impl BrokerStreamTransport {
    pub(super) fn new(stream: BrokerStream) -> Self {
        let (reader, writer) = stream.into_owned_split();
        Self {
            reader: tokio::io::BufReader::new(reader),
            writer: Arc::new(tokio::sync::Mutex::new(Some(writer))),
            active_requests: Arc::new(std::sync::Mutex::new(HashSet::new())),
            request_ingress: crate::mcp::server::RmcpRequestIngressRegistry::default(),
            replay: VecDeque::new(),
            response_lifecycle: None,
        }
    }

    pub(super) fn request_ingress_registry(
        &self,
    ) -> crate::mcp::server::RmcpRequestIngressRegistry {
        self.request_ingress.clone()
    }

    pub(super) fn push_replay(&mut self, line: String) -> std::io::Result<()> {
        if line.len() > crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES {
            let prefix = line.as_bytes()[..line
                .len()
                .min(crate::application::host_admission::MCP_OVERSIZE_ID_INSPECT_BYTES)]
                .to_vec();
            return Err(
                crate::application::host_admission::wire_oversized_io_error_with_prefix(prefix),
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

    async fn write_if_active(
        writer: Arc<tokio::sync::Mutex<Option<BrokerWriteHalf>>>,
        active_requests: Arc<std::sync::Mutex<HashSet<String>>>,
        request_key: Option<String>,
        bytes: Vec<u8>,
    ) -> std::io::Result<()> {
        if let Some(request_key) = request_key
            && !active_requests
                .lock()
                .map_err(|_| std::io::Error::other("active RMCP request registry poisoned"))?
                .remove(&request_key)
        {
            return Ok(());
        }
        Self::write_all_and_flush(writer, bytes).await
    }

    async fn write_before_deadline(
        writer: Arc<tokio::sync::Mutex<Option<BrokerWriteHalf>>>,
        active_requests: Arc<std::sync::Mutex<HashSet<String>>>,
        request_key: Option<String>,
        bytes: Vec<u8>,
        deadline: Option<tokio::time::Instant>,
    ) -> std::io::Result<()> {
        let write = Self::write_if_active(writer, active_requests, request_key, bytes);
        let Some(deadline) = deadline else {
            return write.await;
        };
        tokio::select! {
            biased;
            () = tokio::time::sleep_until(deadline) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "RMCP response write exceeded the absolute request deadline",
            )),
            result = write => result,
        }
    }

    fn is_canonical_retirement_response(value: &serde_json::Value) -> bool {
        crate::mcp::server::is_project_retirement_reason_code(
            value
                .get("error")
                .and_then(|error| error.get("data"))
                .and_then(|data| data.get("reason_code"))
                .and_then(serde_json::Value::as_str),
        )
    }

    async fn observe_incoming_message(
        &self,
        value: &serde_json::Value,
        started: crate::mcp::server::McpRequestStart,
    ) {
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
            let Some((response, response_deadline)) =
                self.request_ingress.cancelled_response(request_id)
            else {
                return;
            };
            let cancelled = self
                .active_requests
                .lock()
                .is_ok_and(|mut active| active.remove(&request_key));
            if !cancelled {
                return;
            }
            if let Ok(mut bytes) = serde_json::to_vec(&response) {
                bytes.push(b'\n');
                let bounded_write_deadline = tokio::time::Instant::now()
                    .checked_add(RMCP_WRITE_DEADLINE)
                    .unwrap_or_else(tokio::time::Instant::now);
                let write_deadline = response_deadline
                    .map(|deadline| deadline.min(bounded_write_deadline))
                    .unwrap_or(bounded_write_deadline);
                let _ = Self::write_before_deadline(
                    Arc::clone(&self.writer),
                    Arc::clone(&self.active_requests),
                    None,
                    bytes,
                    Some(write_deadline),
                )
                .await;
            }
            return;
        }
        let Some(request_id) = value.get("id") else {
            return;
        };
        let Some(request_key) = Self::request_key(request_id) else {
            return;
        };
        self.request_ingress
            .record(request_id, method, value.get("params"), started);
        if let Ok(mut active) = self.active_requests.lock() {
            active.insert(request_key);
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
        crate::application::host_admission::read_bounded_mcp_line(&mut self.reader).await
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
        let request_ingress = self.request_ingress.clone();
        async move {
            let value = serde_json::to_value(&item)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            let request_key = Self::response_request_key(&value);
            let response_deadline = request_key
                .as_deref()
                .and_then(|request_key| request_ingress.response_deadline(request_key));
            let bounded_write_deadline = tokio::time::Instant::now()
                .checked_add(RMCP_WRITE_DEADLINE)
                .unwrap_or_else(tokio::time::Instant::now);
            let write_deadline = response_deadline
                .map(|deadline| deadline.min(bounded_write_deadline))
                .unwrap_or(bounded_write_deadline);
            let canonical_retirement = Self::is_canonical_retirement_response(&value);
            let mut bytes = serde_json::to_vec(&value)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            bytes.push(b'\n');
            let write = Self::write_before_deadline(
                writer,
                active_requests,
                request_key.clone(),
                bytes,
                Some(write_deadline),
            );
            let result = match response_lifecycle {
                None => write.await,
                Some(lifecycle)
                    if lifecycle.response_revoked().is_cancelled() && !canonical_retirement =>
                {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "project server response was revoked",
                    ))
                }
                Some(_) if canonical_retirement => write.await,
                Some(lifecycle) => tokio::select! {
                    biased;
                    () = lifecycle.response_revoked().cancelled() => Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "project server response was revoked",
                    )),
                    result = write => result,
                },
            };
            if let Some(request_key) = request_key {
                request_ingress.finish(&request_key);
            }
            result
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
                    if crate::application::host_admission::is_wire_oversized_io_error(&error) =>
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
            let started = crate::mcp::server::McpRequestStart::now();
            match serde_json::from_str::<serde_json::Value>(&line) {
                Ok(value) => {
                    self.observe_incoming_message(&value, started).await;
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn non_cancellable_notification_preserves_the_real_response_owner() {
        let (server, mut client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
        let transport = BrokerStreamTransport::new(BrokerStream::Unix(server));
        let request_id = serde_json::json!("request-1");
        let request_key = BrokerStreamTransport::request_key(&request_id).expect("request key");
        transport
            .observe_incoming_message(
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_diagnostics",
                        "arguments": {}
                    }
                }),
                crate::mcp::server::McpRequestStart::now(),
            )
            .await;
        transport
            .observe_incoming_message(
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": {"requestId": "request-1"}
                }),
                crate::mcp::server::McpRequestStart::now(),
            )
            .await;
        assert!(
            transport
                .active_requests
                .lock()
                .expect("active request registry")
                .contains(&request_key),
            "cancellation must leave response ownership with the adapter"
        );

        let canonical = br#"{"jsonrpc":"2.0","id":"request-1","result":{"content":[],"_meta":{"tracedecay/execution_receipt":{"terminal":"completed"}}}}"#.to_vec();
        BrokerStreamTransport::write_if_active(
            Arc::clone(&transport.writer),
            Arc::clone(&transport.active_requests),
            Some(request_key),
            canonical.clone(),
        )
        .await
        .expect("canonical receipt write");
        let mut received = vec![0; canonical.len()];
        client
            .read_exact(&mut received)
            .await
            .expect("read canonical receipt");
        assert_eq!(received, canonical);
        assert!(
            transport
                .active_requests
                .lock()
                .expect("active request registry")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn cancellable_tool_notification_claims_response_and_emits_receipt() {
        let (server, mut client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
        let transport = BrokerStreamTransport::new(BrokerStream::Unix(server));
        let request_id = serde_json::json!("request-cancellable");
        let request_key = BrokerStreamTransport::request_key(&request_id).expect("request key");
        transport
            .observe_incoming_message(
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_search",
                        "arguments": {"query": "cancel"}
                    }
                }),
                crate::mcp::server::McpRequestStart::now(),
            )
            .await;
        transport
            .observe_incoming_message(
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": {"requestId": "request-cancellable"}
                }),
                crate::mcp::server::McpRequestStart::now(),
            )
            .await;

        let mut bytes = vec![0; 4096];
        let count = tokio::time::timeout(Duration::from_secs(1), client.read(&mut bytes))
            .await
            .expect("cancellation response deadline")
            .expect("cancellation response read");
        let response: serde_json::Value =
            serde_json::from_slice(&bytes[..count]).expect("cancellation response JSON");
        assert_eq!(
            response["error"]["data"]["tracedecay/execution_receipt"]["terminal"],
            "cancelled"
        );
        assert!(
            !transport
                .active_requests
                .lock()
                .expect("active request registry")
                .contains(&request_key),
            "the cancellation response becomes the sole response owner"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn rmcp_notifications_have_a_bounded_write() {
        let (server, _client) = tokio::net::UnixStream::pair().expect("UnixStream pair");
        let mut transport = BrokerStreamTransport::new(BrokerStream::Unix(server));
        let writer = Arc::clone(&transport.writer);
        let _blocked_writer = writer.lock().await;
        let notification: rmcp::service::TxJsonRpcMessage<rmcp::RoleServer> =
            serde_json::from_value(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/message",
                "params": {
                    "level": "info",
                    "data": "bounded"
                }
            }))
            .expect("server notification");

        let send = rmcp::transport::Transport::send(&mut transport, notification);
        let error = tokio::time::timeout(Duration::from_secs(1), send)
            .await
            .expect("RMCP transport must bound every write")
            .expect_err("blocked write must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
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
}
