//! JSON-RPC 2.0 transport types for the MCP server.
//!
//! The protocol messages, error codes, and the [`McpTransport`] contract are
//! host-neutral and live in [`tracedecay_jsonrpc`]; they are re-exported here so
//! every existing `crate::mcp::transport::…` path keeps resolving. What remains
//! is root-coupled: the concrete stdio/channel/replay transports, which depend
//! on this crate's host-admission frame bounds, and the split read/write halves,
//! whose implementations target foreign `tokio` types and therefore cannot be
//! expressed outside the crate that owns them.

pub use tracedecay_jsonrpc::{
    ErrorCode, JsonRpcError, JsonRpcRequest, JsonRpcResponse, McpTransport,
};

/// Read half of a transport whose input and output can be driven concurrently.
pub trait McpTransportReader {
    /// Read the next line from the transport. Returns `None` on EOF.
    fn read_line(
        &mut self,
    ) -> impl std::future::Future<Output = std::io::Result<Option<String>>> + Send;
}

/// Write half of a transport whose input and output can be driven concurrently.
pub trait McpTransportWriter {
    /// Write a complete line (including trailing newline) to the transport.
    fn write_line(
        &mut self,
        line: &str,
    ) -> impl std::future::Future<Output = std::io::Result<()>> + Send;

    /// Flush any buffered output.
    fn flush(&mut self) -> impl std::future::Future<Output = std::io::Result<()>> + Send;
}

/// A line transport that exposes independent read and write halves.
pub trait McpDuplexTransport: McpTransport {
    /// Borrowed read half.
    type Reader<'a>: McpTransportReader + Send + 'a
    where
        Self: 'a;

    /// Borrowed write half.
    type Writer<'a>: McpTransportWriter + Send + 'a
    where
        Self: 'a;

    /// Split the transport into independently borrowable halves.
    fn split(&mut self) -> (Self::Reader<'_>, Self::Writer<'_>);
}

/// Wraps a transport with a queue of already-consumed input lines that must
/// be re-delivered before reading from the underlying transport again.
///
/// The daemon consumes the first MCP request while resolving initialize roots
/// and selecting a project server. Replaying that request into the selected
/// server preserves it, along with any pipelined input buffered by the inner
/// reader.
pub struct ReplayTransport<T: McpTransport + Send> {
    replay: std::collections::VecDeque<String>,
    inner: T,
}

impl<T: McpTransport + Send> ReplayTransport<T> {
    pub fn new(inner: T) -> Self {
        Self {
            replay: std::collections::VecDeque::new(),
            inner,
        }
    }

    /// Queues a line to be re-delivered by the next `read_line` calls, ahead
    /// of any new input from the inner transport.
    ///
    /// Rejects oversized lines before enqueue so the replay deque never retains
    /// attacker payload bytes.
    pub fn push_replay(&mut self, line: String) -> std::io::Result<()> {
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
}

impl<T: McpTransport + Send> McpTransport for ReplayTransport<T> {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        if let Some(line) = self.replay.pop_front() {
            return Ok(Some(line));
        }
        self.inner.read_line().await
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.inner.write_line(line).await
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush().await
    }

    fn peer_fully_closed_after_eof(
        &self,
    ) -> impl std::future::Future<Output = ()> + Send + 'static {
        self.inner.peer_fully_closed_after_eof()
    }
}

/// Real stdio transport — reads from stdin, writes to stdout.
pub struct StdioTransport {
    reader: tokio::io::BufReader<tokio::io::Stdin>,
    writer: tokio::io::Stdout,
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self {
            reader: tokio::io::BufReader::new(tokio::io::stdin()),
            writer: tokio::io::stdout(),
        }
    }
}

impl StdioTransport {
    pub fn new() -> Self {
        Self::default()
    }
}

impl McpTransport for StdioTransport {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        crate::application::host_admission::read_bounded_mcp_line(&mut self.reader).await
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        self.writer.write_all(line.as_bytes()).await
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        self.writer.flush().await
    }
}

impl McpTransportReader for &mut tokio::io::BufReader<tokio::io::Stdin> {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        crate::application::host_admission::read_bounded_mcp_line(&mut **self).await
    }
}

impl McpTransportWriter for &mut tokio::io::Stdout {
    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        tokio::io::AsyncWriteExt::write_all(&mut **self, line.as_bytes()).await
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        tokio::io::AsyncWriteExt::flush(&mut **self).await
    }
}

impl McpDuplexTransport for StdioTransport {
    type Reader<'a> = &'a mut tokio::io::BufReader<tokio::io::Stdin>;
    type Writer<'a> = &'a mut tokio::io::Stdout;

    fn split(&mut self) -> (Self::Reader<'_>, Self::Writer<'_>) {
        (&mut self.reader, &mut self.writer)
    }
}

/// In-memory transport for tests — backed by tokio mpsc channels.
#[cfg(any(test, feature = "test-transport"))]
pub struct ChannelTransport {
    rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
}

#[cfg(any(test, feature = "test-transport"))]
impl ChannelTransport {
    /// Create a transport and the handles needed by test code.
    ///
    /// Returns `(transport, sender_to_server, receiver_from_server)`.
    pub fn new() -> (
        Self,
        tokio::sync::mpsc::UnboundedSender<String>,
        tokio::sync::mpsc::UnboundedReceiver<String>,
    ) {
        let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel();
        let (output_tx, output_rx) = tokio::sync::mpsc::unbounded_channel();
        (
            Self {
                rx: input_rx,
                tx: output_tx,
            },
            input_tx,
            output_rx,
        )
    }
}

#[cfg(any(test, feature = "test-transport"))]
impl McpTransport for ChannelTransport {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        match self.rx.recv().await {
            Some(line)
                if line.len() > crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES =>
            {
                let prefix = line.as_bytes()[..line
                    .len()
                    .min(crate::application::host_admission::MCP_OVERSIZE_ID_INSPECT_BYTES)]
                    .to_vec();
                Err(crate::application::host_admission::wire_oversized_io_error_with_prefix(prefix))
            }
            other => Ok(other),
        }
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.tx
            .send(line.to_string())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string()))
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(any(test, feature = "test-transport"))]
impl McpTransportReader for &mut tokio::sync::mpsc::UnboundedReceiver<String> {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        match self.recv().await {
            Some(line)
                if line.len() > crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES =>
            {
                let prefix = line.as_bytes()[..line
                    .len()
                    .min(crate::application::host_admission::MCP_OVERSIZE_ID_INSPECT_BYTES)]
                    .to_vec();
                Err(crate::application::host_admission::wire_oversized_io_error_with_prefix(prefix))
            }
            other => Ok(other),
        }
    }
}

#[cfg(any(test, feature = "test-transport"))]
impl McpTransportWriter for &mut tokio::sync::mpsc::UnboundedSender<String> {
    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.send(line.to_string())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string()))
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(any(test, feature = "test-transport"))]
impl McpDuplexTransport for ChannelTransport {
    type Reader<'a> = &'a mut tokio::sync::mpsc::UnboundedReceiver<String>;
    type Writer<'a> = &'a mut tokio::sync::mpsc::UnboundedSender<String>;

    fn split(&mut self) -> (Self::Reader<'_>, Self::Writer<'_>) {
        (&mut self.rx, &mut self.tx)
    }
}

/// Typed non-durable rejection for oversized MCP/daemon frames. Never echoes
/// full payload bytes. When a bounded leading prefix safely yields a JSON-RPC
/// request `id`, the error correlates to that ID as `InvalidRequest`; otherwise
/// it uses `ParseError` with a null ID (JSON-RPC 2.0 parse-error semantics).
pub(crate) async fn write_wire_oversized_rejection(
    transport: &mut impl McpTransport,
    error: &std::io::Error,
) -> std::io::Result<()> {
    use crate::application::host_admission::{
        HostAdmissionOutcome, WIRE_RECORD_TOO_LARGE, wire_oversized_inspect_prefix,
    };

    let inspect_prefix = wire_oversized_inspect_prefix(error);
    let (id, code) = match peek_jsonrpc_request_id(inspect_prefix) {
        Some(id) => (id, ErrorCode::InvalidRequest),
        None => (serde_json::Value::Null, ErrorCode::ParseError),
    };
    let outcome = HostAdmissionOutcome::wire_record_too_large();
    let response = JsonRpcResponse::error_with_data(
        id,
        code,
        WIRE_RECORD_TOO_LARGE.to_string(),
        Some(serde_json::json!({ "admission": outcome })),
    );
    let line = serde_json::to_string(&response)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    transport.write_line(&format!("{line}\n")).await?;
    transport.flush().await
}

/// Bounded inspection of a leading frame prefix for a top-level JSON-RPC `id`.
///
/// Only examines at most [`crate::application::host_admission::MCP_OVERSIZE_ID_INSPECT_BYTES`]
/// bytes. Never materializes or parses the full oversized payload.
pub(crate) fn peek_jsonrpc_request_id(prefix: &[u8]) -> Option<serde_json::Value> {
    use crate::application::host_admission::MCP_OVERSIZE_ID_INSPECT_BYTES;

    let prefix = if prefix.len() > MCP_OVERSIZE_ID_INSPECT_BYTES {
        &prefix[..MCP_OVERSIZE_ID_INSPECT_BYTES]
    } else {
        prefix
    };
    let bytes = prefix;
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'{' {
        return None;
    }
    i += 1;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'}' {
            return None;
        }

        let (key, key_bytes) = parse_prefix_value(&bytes[i..])?;
        let serde_json::Value::String(key) = key else {
            return None;
        };
        i = i.checked_add(key_bytes)?;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b':' {
            return None;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }

        let (value, value_bytes) = parse_prefix_value(&bytes[i..])?;
        i = i.checked_add(value_bytes)?;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || !matches!(bytes[i], b',' | b'}') {
            return None;
        }

        if key == "id" {
            return match value {
                serde_json::Value::Null
                | serde_json::Value::Number(_)
                | serde_json::Value::String(_) => Some(value),
                _ => None,
            };
        }
        if bytes[i] == b'}' {
            return None;
        }
        i += 1;
    }
}

fn parse_prefix_value(input: &[u8]) -> Option<(serde_json::Value, usize)> {
    let mut values = serde_json::Deserializer::from_slice(input).into_iter();
    let value = values.next()?.ok()?;
    Some((value, values.byte_offset()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn channel_transport_rejects_oversized_line_without_returning_payload() {
        // ChannelTransport receives already-allocated Strings (test harness).
        // Product stdio/daemon paths use read_bounded_line before allocation;
        // this asserts the harness still maps oversized to the typed IO error
        // without returning payload bytes on the Result::Ok path.
        let (mut transport, tx, _rx) = ChannelTransport::new();
        tx.send("a".repeat(crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES))
            .unwrap();
        assert_eq!(
            transport.read_line().await.unwrap().unwrap().len(),
            crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES
        );
        let hostile =
            "x".repeat(crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES + 1);
        tx.send(hostile).unwrap();
        let err = transport.read_line().await.unwrap_err();
        assert!(crate::application::host_admission::is_wire_oversized_io_error(&err));
        assert_eq!(
            err.to_string(),
            crate::application::host_admission::WIRE_RECORD_TOO_LARGE
        );
        assert!(!err.to_string().contains('x'));
    }

    #[tokio::test]
    async fn stdio_style_bounded_line_streams_hostile_input_without_retaining_payload() {
        use std::pin::Pin;
        use std::task::{Context, Poll};

        use tokio::io::{AsyncRead, BufReader, ReadBuf};

        use crate::application::host_admission::{
            MAX_MCP_JSONRPC_FRAME_BYTES, WIRE_RECORD_TOO_LARGE, line_outcome_to_io,
            read_bounded_line, wire_oversized_io_error,
        };

        /// Streams `total` hostile bytes in chunks, then a fixed suffix, without
        /// pre-materializing the full hostile value for the product reader.
        struct AsyncHostileThenSuffix {
            remaining: usize,
            chunk: Vec<u8>,
            suffix: &'static [u8],
            suffix_offset: usize,
        }

        impl AsyncRead for AsyncHostileThenSuffix {
            fn poll_read(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<std::io::Result<()>> {
                if self.remaining > 0 {
                    let n = buf.remaining().min(self.chunk.len()).min(self.remaining);
                    buf.put_slice(&self.chunk[..n]);
                    self.remaining -= n;
                    return Poll::Ready(Ok(()));
                }
                if self.suffix_offset < self.suffix.len() {
                    let rest = &self.suffix[self.suffix_offset..];
                    let n = buf.remaining().min(rest.len());
                    buf.put_slice(&rest[..n]);
                    self.suffix_offset += n;
                }
                Poll::Ready(Ok(()))
            }
        }

        let max = 64;
        let stream = AsyncHostileThenSuffix {
            remaining: max + 256 * 1024,
            chunk: vec![b'z'; 1024],
            suffix: b"\n{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n",
            suffix_offset: 0,
        };
        let mut reader = BufReader::new(stream);

        // Exercises the shared bounded-line primitive at a small cap; the
        // dedicated MCP helper's exact production cap is covered in wire tests.
        let first = line_outcome_to_io(read_bounded_line(&mut reader, max).await.unwrap());
        let err = first.unwrap_err();
        assert!(crate::application::host_admission::is_wire_oversized_io_error(&err));
        assert_eq!(err.to_string(), WIRE_RECORD_TOO_LARGE);
        assert!(!err.to_string().contains('z'));

        let second = line_outcome_to_io(
            read_bounded_line(&mut reader, MAX_MCP_JSONRPC_FRAME_BYTES)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            second.as_deref(),
            Some(r#"{"jsonrpc":"2.0","method":"ping"}"#)
        );

        let (mut transport, _tx, mut rx) = ChannelTransport::new();
        write_wire_oversized_rejection(&mut transport, &wire_oversized_io_error())
            .await
            .unwrap();
        let rejection = rx.recv().await.unwrap();
        assert!(rejection.contains(WIRE_RECORD_TOO_LARGE));
        assert!(rejection.contains("\"code\":-32700") || rejection.contains("-32700"));
        assert!(rejection.contains("wire_record_too_large") || rejection.contains("admission"));
        assert!(!rejection.contains('z'));
    }

    #[test]
    fn replay_transport_rejects_oversized_before_enqueue() {
        let (inner, _tx, _rx) = ChannelTransport::new();
        let mut replay = ReplayTransport::new(inner);
        let hostile =
            "y".repeat(crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES + 1);
        let err = replay.push_replay(hostile).unwrap_err();
        assert!(crate::application::host_admission::is_wire_oversized_io_error(&err));
        assert!(!err.to_string().contains('y'));
    }

    #[test]
    fn peek_jsonrpc_request_id_recovers_number_and_string_ids() {
        assert_eq!(
            peek_jsonrpc_request_id(
                br#"{"jsonrpc":"2.0","id":42,"method":"tools/call","params":{"x":""#
            ),
            Some(json!(42))
        );
        assert_eq!(
            peek_jsonrpc_request_id(
                br#"{"id":"req-9","jsonrpc":"2.0","method":"tools/call","params":{"x":""#
            ),
            Some(json!("req-9"))
        );
        assert_eq!(
            peek_jsonrpc_request_id(
                br#"{"jsonrpc":"2.0","params":{"nested":{"id":"not-request"}},"id":null,"method":"tools/call","#
            ),
            Some(serde_json::Value::Null)
        );
    }

    #[test]
    fn peek_jsonrpc_request_id_after_oversized_params_is_unrecoverable() {
        use crate::application::host_admission::MCP_OVERSIZE_ID_INSPECT_BYTES;

        let mut frame = br#"{"jsonrpc":"2.0","params":{"payload":""#.to_vec();
        frame.extend(std::iter::repeat_n(b'x', MCP_OVERSIZE_ID_INSPECT_BYTES));
        frame.extend_from_slice(br#""},"id":77,"method":"tools/call"}"#);
        assert_eq!(
            peek_jsonrpc_request_id(&frame[..MCP_OVERSIZE_ID_INSPECT_BYTES]),
            None
        );
    }

    #[test]
    fn peek_jsonrpc_request_id_missing_malformed_or_incomplete_yields_none() {
        assert_eq!(
            peek_jsonrpc_request_id(br#"{"jsonrpc":"2.0","method":"initialized""#),
            None
        );
        assert_eq!(peek_jsonrpc_request_id(b"zzzz"), None);
        assert_eq!(
            peek_jsonrpc_request_id(br#"{"jsonrpc":"2.0","id":{"nested":1},"method":"x""#),
            None
        );
        assert_eq!(
            peek_jsonrpc_request_id(br#"{"jsonrpc":"2.0" "id":8,"method":"x",""#),
            None
        );
        assert_eq!(
            peek_jsonrpc_request_id(br#"{"jsonrpc":"2.0","id":"incomplete"#),
            None
        );
        assert_eq!(
            peek_jsonrpc_request_id(br#"{"jsonrpc":"2.0","id":12"#),
            None
        );
    }

    #[tokio::test]
    async fn oversized_rejection_preserves_recoverable_request_id() {
        use crate::application::host_admission::wire_oversized_io_error_with_prefix;

        let (mut transport, _tx, mut rx) = ChannelTransport::new();
        let err = wire_oversized_io_error_with_prefix(
            br#"{"jsonrpc":"2.0","id":99,"method":"tools/call","params":{"n":""#.to_vec(),
        );
        write_wire_oversized_rejection(&mut transport, &err)
            .await
            .unwrap();
        let rejection: serde_json::Value =
            serde_json::from_str(rx.recv().await.unwrap().trim()).unwrap();
        assert_eq!(rejection["id"], json!(99));
        assert_eq!(rejection["error"]["code"], json!(-32600));
        assert_eq!(
            rejection["error"]["message"],
            json!("wire_record_too_large")
        );
    }

    #[tokio::test]
    async fn oversized_rejection_uses_parse_error_when_id_unrecoverable() {
        use crate::application::host_admission::wire_oversized_io_error;

        let (mut transport, _tx, mut rx) = ChannelTransport::new();
        write_wire_oversized_rejection(&mut transport, &wire_oversized_io_error())
            .await
            .unwrap();
        let rejection: serde_json::Value =
            serde_json::from_str(rx.recv().await.unwrap().trim()).unwrap();
        assert!(rejection["id"].is_null());
        assert_eq!(rejection["error"]["code"], json!(-32700));
    }

    #[test]
    fn mcp_frame_limit_exceeds_host_event_wire_cap() {
        assert_eq!(
            crate::application::host_admission::MAX_WIRE_MESSAGE_BYTES,
            1024 * 1024
        );
        assert_eq!(
            crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES,
            16 * 1024 * 1024
        );
        const {
            assert!(
                crate::application::host_admission::MAX_MCP_JSONRPC_FRAME_BYTES
                    > crate::application::host_admission::MAX_WIRE_MESSAGE_BYTES
            );
        }
    }
}
