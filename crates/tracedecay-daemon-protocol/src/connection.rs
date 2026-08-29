//! Leaf connection helpers for the daemon invocation client.
//!
//! Authority-record discovery stays in the composition root. This module owns
//! endpoint connect, handshake preamble, and bounded response reads.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::time::{Instant, timeout};

use crate::handshake::DaemonHandshake;
use crate::transport::{BrokerStream, DaemonAuthPreface, DaemonEndpoint};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

pub const DAEMON_TOOL_LIVENESS_POLL_INTERVAL: Duration = Duration::from_secs(5);
pub const DAEMON_TOOL_HEALTH_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
pub const DAEMON_TOOL_RESPONSE_GRACE: Duration = Duration::from_secs(30);
pub const DAEMON_RESTART_GRACE: Duration = Duration::from_secs(8);
pub const DAEMON_RESTART_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Authority-aware liveness check supplied by the composition root.
pub trait DaemonLivenessProbe: Send + Sync {
    fn ensure_live(&self, request_label: &str) -> Result<()>;
}

#[derive(Clone)]
pub struct DaemonConnection {
    pub endpoint: DaemonEndpoint,
    pub auth_token: Option<String>,
    liveness: Option<Arc<dyn DaemonLivenessProbe>>,
}

impl DaemonConnection {
    pub fn new(endpoint: DaemonEndpoint, auth_token: Option<String>) -> Self {
        Self {
            endpoint,
            auth_token,
            liveness: None,
        }
    }

    pub fn with_liveness(mut self, probe: Arc<dyn DaemonLivenessProbe>) -> Self {
        self.liveness = Some(probe);
        self
    }

    pub fn unauthenticated_for_test(endpoint: DaemonEndpoint) -> Self {
        Self::new(endpoint, None)
    }
}

/// The local read bound for a request whose caller deadline is `request_deadline`.
pub fn daemon_tool_response_bound(request_deadline: Instant) -> Result<Instant> {
    request_deadline
        .checked_add(DAEMON_TOOL_RESPONSE_GRACE)
        .ok_or_else(|| TraceDecayError::Config {
            message: "daemon tool response bound exceeds the supported monotonic range".to_string(),
        })
}

#[hotpath::measure(label = "daemon_protocol.client.ensure_live", future = true)]
pub async fn ensure_daemon_connection_live(
    connection: &DaemonConnection,
    request_label: &str,
) -> Result<()> {
    if let Some(probe) = connection.liveness.as_ref() {
        probe.ensure_live(request_label)?;
    }
    timeout(
        DAEMON_TOOL_HEALTH_CONNECT_TIMEOUT,
        BrokerStream::connect(&connection.endpoint),
    )
    .await
    .map_err(|_| TraceDecayError::Config {
        message: format!(
            "daemon health check timed out at '{}' while request '{request_label}' was awaiting a response; the request was already sent and was not retried",
            connection.endpoint
        ),
    })?
    .map(|_| ())
    .map_err(|error| TraceDecayError::Config {
        message: format!(
            "daemon became unreachable at '{}' while request '{request_label}' was awaiting a response: {error}; the request was already sent and was not retried",
            connection.endpoint
        ),
    })
}

#[hotpath::measure(label = "daemon_protocol.client.response.wait", future = true)]
pub async fn next_daemon_response_line<R>(
    reader: &mut R,
    connection: &DaemonConnection,
    request_label: &str,
    liveness_poll_interval: Duration,
) -> Result<Option<String>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tracedecay_sessions::admission::{is_wire_oversized_io_error, read_bounded_mcp_line};

    let read = read_bounded_mcp_line(reader);
    tokio::pin!(read);
    loop {
        tokio::select! {
            result = &mut read => {
                return match result {
                    Ok(line) => Ok(line),
                    Err(error) if is_wire_oversized_io_error(&error) => {
                        Err(TraceDecayError::Config {
                            message: format!(
                                "daemon {request_label} response exceeded wire message bound ({})",
                                tracedecay_sessions::admission::WIRE_RECORD_TOO_LARGE
                            ),
                        })
                    }
                    Err(error) => Err(error.into()),
                };
            }
            () = tokio::time::sleep(liveness_poll_interval) => {
                ensure_daemon_connection_live(connection, request_label).await?;
            }
        }
    }
}

#[hotpath::measure(label = "daemon_protocol.client.preamble", future = true)]
pub async fn write_daemon_preamble(
    writer: &mut tokio::io::WriteHalf<BrokerStream>,
    connection: &DaemonConnection,
    handshake: &DaemonHandshake,
) -> Result<()> {
    if let Some(token) = connection.auth_token.as_deref() {
        writer
            .write_all(DaemonAuthPreface::new(token).to_line()?.as_bytes())
            .await?;
        writer.write_all(b"\n").await?;
    }
    writer.write_all(handshake.to_line()?.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    Ok(())
}

pub fn is_transient_daemon_connect_error(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::WouldBlock
    )
}

pub fn is_saturated_daemon_connect_error(kind: std::io::ErrorKind) -> bool {
    kind == std::io::ErrorKind::WouldBlock
}

pub fn daemon_connect_failure_advice(kind: std::io::ErrorKind) -> &'static str {
    if is_saturated_daemon_connect_error(kind) {
        "The daemon is up but not accepting connections — likely overloaded. Retry shortly, or check `tracedecay daemon status`."
    } else {
        "The daemon may be restarting (e.g. after `tracedecay update`) — retry shortly, or check `tracedecay daemon status`."
    }
}

pub async fn connect_to_daemon_connection(connection: &DaemonConnection) -> Result<BrokerStream> {
    connect_with_restart_grace(connection, DAEMON_RESTART_GRACE, DAEMON_RESTART_POLL_INTERVAL).await
}

#[hotpath::measure(label = "daemon_protocol.client.connect", future = true)]
pub async fn connect_with_restart_grace(
    connection: &DaemonConnection,
    grace: Duration,
    poll_interval: Duration,
) -> Result<BrokerStream> {
    let deadline = Instant::now() + grace;
    loop {
        match BrokerStream::connect(&connection.endpoint).await {
            Ok(stream) => return Ok(stream),
            Err(TraceDecayError::Io(err)) => {
                if !is_transient_daemon_connect_error(err.kind()) || Instant::now() >= deadline {
                    return Err(TraceDecayError::Config {
                        message: format!(
                            "could not connect to TraceDecay daemon endpoint '{}': {err}. {}",
                            connection.endpoint,
                            daemon_connect_failure_advice(err.kind())
                        ),
                    });
                }
                tokio::time::sleep(poll_interval).await;
            }
            Err(error) => return Err(error),
        }
    }
}
