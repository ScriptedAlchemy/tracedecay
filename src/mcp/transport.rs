//! JSON-RPC 2.0 transport types for the MCP server.
//!
//! Provides serialization and deserialization of JSON-RPC 2.0 messages
//! used to communicate between the MCP client and server over stdio.

pub use tracedecay_jsonrpc::{
    ErrorCode, JsonRpcError, JsonRpcRequest, JsonRpcResponse, McpTransport,
};

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
    pub fn push_replay(&mut self, line: String) {
        self.replay.push_back(line);
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
}

/// Real stdio transport — reads from stdin, writes to stdout.
pub struct StdioTransport {
    reader: tokio::io::Lines<tokio::io::BufReader<tokio::io::Stdin>>,
    writer: tokio::io::Stdout,
}

impl Default for StdioTransport {
    fn default() -> Self {
        use tokio::io::AsyncBufReadExt;
        Self {
            reader: tokio::io::BufReader::new(tokio::io::stdin()).lines(),
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
        self.reader.next_line().await
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
        Ok(self.rx.recv().await)
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
