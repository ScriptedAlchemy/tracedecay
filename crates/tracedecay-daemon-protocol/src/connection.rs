//! Leaf connection helpers for the daemon invocation client.
//!
//! Authority-record discovery lives in `tracedecay-daemon-identity`; this
//! crate never performs it. This module owns endpoint connect, handshake
//! preamble, and bounded response reads.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::time::Instant;

use crate::handshake::DaemonHandshake;
use crate::transport::{BrokerStream, DaemonAuthPreface, DaemonEndpoint};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_framing::{
    WIRE_RECORD_TOO_LARGE, is_wire_oversized_io_error, read_bounded_mcp_line,
};

pub const DAEMON_TOOL_LIVENESS_POLL_INTERVAL: Duration = Duration::from_secs(5);
pub const DAEMON_TOOL_RESPONSE_GRACE: Duration = Duration::from_secs(30);
pub const DAEMON_RESTART_GRACE: Duration = Duration::from_secs(8);
pub const DAEMON_RESTART_POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Canonical execution budget for daemon-owned retained operations.
pub const DEFAULT_DAEMON_OPERATION_DEADLINE: Duration = Duration::from_secs(30);

/// Default caller request deadline for one-shot daemon tool clients.
///
/// Shared by `tracedecay tool` and every `call_default_tool` / `daemon_tool_json`
/// path. Override with [`TOOL_REQUEST_DEADLINE_ENV`].
pub const DEFAULT_TOOL_REQUEST_DEADLINE: Duration = Duration::from_mins(2);
/// Upper bound matching the CLI's supported monotonic deadline range.
pub const MAX_TOOL_REQUEST_DEADLINE: Duration = Duration::from_hours(24);
/// Millisecond override for [`DEFAULT_TOOL_REQUEST_DEADLINE`].
pub const TOOL_REQUEST_DEADLINE_ENV: &str = "TRACEDECAY_TOOL_DEADLINE_MS";

/// Connect failed with `NotFound` / `ConnectionRefused` after restart grace.
pub const DAEMON_CONNECT_DOWN: &str = "daemon_connect_down";
/// Connect failed with `WouldBlock` after restart grace (listen backlog full).
pub const DAEMON_CONNECT_SATURATED: &str = "daemon_connect_saturated";
/// Connected, but no response frame arrived before the client deadline.
pub const DAEMON_RESPONSE_STALLED: &str = "daemon_response_stalled";

/// Authority-aware liveness check supplied by the composition root.
pub trait DaemonLivenessProbe: Send + Sync {
    fn ensure_live(&self, request_label: &str) -> Result<()>;
}

#[derive(Clone)]
pub struct DaemonConnection {
    pub endpoint: DaemonEndpoint,
    pub auth_token: Option<String>,
    /// The daemon version advertised by the authority record that named this
    /// endpoint. Lets transport failures name version skew instead of hiding
    /// it behind a raw io error.
    pub daemon_version: Option<String>,
    liveness: Option<Arc<dyn DaemonLivenessProbe>>,
}

impl DaemonConnection {
    pub fn new(endpoint: DaemonEndpoint, auth_token: Option<String>) -> Self {
        Self {
            endpoint,
            auth_token,
            daemon_version: None,
            liveness: None,
        }
    }

    #[must_use]
    pub fn with_liveness(mut self, probe: Arc<dyn DaemonLivenessProbe>) -> Self {
        self.liveness = Some(probe);
        self
    }

    #[must_use]
    pub fn with_daemon_version(mut self, daemon_version: impl Into<String>) -> Self {
        self.daemon_version = Some(daemon_version.into());
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

/// Parse [`TOOL_REQUEST_DEADLINE_ENV`] the same way `tracedecay tool` does.
///
/// Missing, empty, zero, or unparsable values fall back to
/// [`DEFAULT_TOOL_REQUEST_DEADLINE`]. Values above [`MAX_TOOL_REQUEST_DEADLINE`]
/// fail closed.
pub fn tool_request_deadline() -> Result<Duration> {
    tool_request_deadline_from(std::env::var(TOOL_REQUEST_DEADLINE_ENV).ok())
}

fn tool_request_deadline_from(raw: Option<String>) -> Result<Duration> {
    let deadline = raw
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(DEFAULT_TOOL_REQUEST_DEADLINE, Duration::from_millis);
    if deadline > MAX_TOOL_REQUEST_DEADLINE {
        return Err(TraceDecayError::Config {
            message: format!(
                "{TOOL_REQUEST_DEADLINE_ENV} exceeds the supported monotonic deadline range"
            ),
        });
    }
    Ok(deadline)
}

/// Typed connect failure after restart grace (or the same kinds immediately
/// when the grace is already spent).
pub fn daemon_connect_failure(
    endpoint: impl std::fmt::Display,
    err: &std::io::Error,
) -> TraceDecayError {
    let reason_code = if is_saturated_daemon_connect_error(err.kind()) {
        DAEMON_CONNECT_SATURATED
    } else {
        DAEMON_CONNECT_DOWN
    };
    TraceDecayError::project_route(
        reason_code,
        true,
        format!(
            "could not connect to TraceDecay daemon endpoint '{endpoint}': {err}. {}",
            daemon_connect_failure_advice(err.kind())
        ),
    )
}

/// Connected (or past the request deadline) with no response frame.
pub fn daemon_response_stalled(elapsed: Duration) -> TraceDecayError {
    TraceDecayError::project_route(
        DAEMON_RESPONSE_STALLED,
        true,
        format!(
            "daemon did not answer after {}s; stalled or saturated — run `tracedecay daemon status`",
            elapsed.as_secs()
        ),
    )
}

/// [`daemon_response_stalled`] with the in-flight stage and request named, for
/// deadline runners that know which stage of which request timed out.
pub fn daemon_response_stalled_during(
    stage: &'static str,
    request_label: &str,
    elapsed: Duration,
) -> TraceDecayError {
    TraceDecayError::project_route(
        DAEMON_RESPONSE_STALLED,
        true,
        format!(
            "daemon did not answer after {}s ({stage} stage of '{request_label}'); stalled or saturated — run `tracedecay daemon status`",
            elapsed.as_secs()
        ),
    )
}

#[hotpath::measure(label = "daemon_protocol.client.ensure_live", future = true)]
pub async fn ensure_daemon_connection_live(
    connection: &DaemonConnection,
    request_label: &str,
) -> Result<()> {
    if let Some(probe) = connection.liveness.as_ref() {
        probe.ensure_live(request_label)?;
    }
    Ok(())
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
                                "daemon {request_label} response exceeded wire message bound ({WIRE_RECORD_TOO_LARGE})"
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
    connect_with_restart_grace(
        connection,
        DAEMON_RESTART_GRACE,
        DAEMON_RESTART_POLL_INTERVAL,
    )
    .await
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
                    return Err(if is_transient_daemon_connect_error(err.kind()) {
                        daemon_connect_failure(&connection.endpoint, &err)
                    } else {
                        TraceDecayError::Config {
                            message: format!(
                                "could not connect to TraceDecay daemon endpoint '{}': {err}. {}",
                                connection.endpoint,
                                daemon_connect_failure_advice(err.kind())
                            ),
                        }
                    });
                }
                tokio::time::sleep(poll_interval).await;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_request_deadline_defaults_and_rejects_oversized() {
        assert_eq!(
            tool_request_deadline_from(None).expect("default"),
            DEFAULT_TOOL_REQUEST_DEADLINE
        );
        assert_eq!(
            tool_request_deadline_from(Some("0".to_owned())).expect("zero falls back"),
            DEFAULT_TOOL_REQUEST_DEADLINE
        );
        assert_eq!(
            tool_request_deadline_from(Some("1500".to_owned())).expect("millis"),
            Duration::from_millis(1500)
        );
        let oversized = tool_request_deadline_from(Some(u64::MAX.to_string()))
            .expect_err("oversized must fail closed");
        assert!(
            oversized.to_string().contains(TOOL_REQUEST_DEADLINE_ENV),
            "range error must name the env var, got: {oversized}"
        );
    }

    #[test]
    fn connect_failures_are_typed_reason_codes() {
        let down = daemon_connect_failure(
            "/tmp/daemon.sock",
            &std::io::Error::from(std::io::ErrorKind::NotFound),
        );
        let saturated = daemon_connect_failure(
            "/tmp/daemon.sock",
            &std::io::Error::from(std::io::ErrorKind::WouldBlock),
        );
        assert_eq!(
            down.project_route_context()
                .map(|(code, retryable, _)| (code, retryable)),
            Some((DAEMON_CONNECT_DOWN, true))
        );
        assert_eq!(
            saturated
                .project_route_context()
                .map(|(code, retryable, _)| (code, retryable)),
            Some((DAEMON_CONNECT_SATURATED, true))
        );
        assert!(down.to_string().contains("may be restarting"));
        assert!(saturated.to_string().contains("up but not accepting"));
    }

    #[test]
    fn stalled_response_names_wait_and_status() {
        let error = daemon_response_stalled(Duration::from_secs(12));
        assert_eq!(
            error
                .project_route_context()
                .map(|(code, retryable, _)| (code, retryable)),
            Some((DAEMON_RESPONSE_STALLED, true))
        );
        let message = error.to_string();
        assert!(
            message.contains("did not answer after 12s"),
            "stalled detail must name the wait, got: {message}"
        );
        assert!(
            message.contains("tracedecay daemon status"),
            "stalled detail must point at daemon status, got: {message}"
        );
    }
}
