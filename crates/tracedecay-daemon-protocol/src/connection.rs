//! Leaf connection helpers for the daemon invocation client.
//!
//! Authority-record discovery lives in `tracedecay-daemon-identity`; this
//! crate never performs it. This module owns endpoint connect, handshake
//! preamble, and bounded response reads.

use std::sync::Arc;
use std::time::{Duration, Instant};

use asupersync::io::AsyncWriteExt;

use crate::handshake::DaemonHandshake;
use crate::transport::{
    DaemonAuthPreface, DaemonClientStream, DaemonClientWriteHalf, DaemonEndpoint,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_framing::{BoundedLineReader, WIRE_RECORD_TOO_LARGE, is_wire_oversized_io_error};

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
    pub auth_token: String,
    /// The daemon version advertised by the authority record that named this
    /// endpoint. Lets transport failures name version skew instead of hiding
    /// it behind a raw io error.
    pub daemon_version: Option<String>,
    liveness: Option<Arc<dyn DaemonLivenessProbe>>,
}

impl DaemonConnection {
    pub fn new(endpoint: DaemonEndpoint, auth_token: String) -> Self {
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
            "daemon did not answer after {}s; stalled or saturated. Run `tracedecay daemon status`",
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
            "daemon did not answer after {}s ({stage} stage of '{request_label}'); stalled or saturated. Run `tracedecay daemon status`",
            elapsed.as_secs()
        ),
    )
}

#[tracing::instrument(name = "daemon_protocol.client.ensure_live", level = "trace", skip_all)]
pub async fn ensure_daemon_connection_live(
    connection: &DaemonConnection,
    request_label: &str,
) -> Result<()> {
    if let Some(probe) = connection.liveness.as_ref() {
        probe.ensure_live(request_label)?;
    }
    Ok(())
}

/// Reads the next daemon frame, polling `ensure_live` whenever the read is
/// still pending.
///
/// A delivered frame always wins over the probe: the read is polled first,
/// keeps running while the probe runs, and a failed probe ends the wait only
/// if one more read still blocks. Every poll of the socket reads it directly,
/// so that read sees everything the daemon wrote before it stopped listening.
///
/// `read_mcp_line` is dropped when another branch wins `select!`; the partial
/// frame lives on the caller's line reader, so it also survives this call.
pub async fn poll_daemon_response_line<R, F, Fut>(
    line_reader: &mut BoundedLineReader<R>,
    request_label: &str,
    liveness_poll_interval: Duration,
    mut ensure_live: F,
) -> Result<Option<String>>
where
    R: asupersync::io::AsyncBufRead + Unpin,
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    loop {
        tokio::select! {
            biased;
            result = line_reader.read_mcp_line() => return response_line(result, request_label),
            () = asupersync::time::sleep(asupersync::time::wall_now(), liveness_poll_interval) => {}
        }
        let probe = ensure_live();
        tokio::pin!(probe);
        let probe_failure = tokio::select! {
            biased;
            result = line_reader.read_mcp_line() => return response_line(result, request_label),
            probe_result = &mut probe => match probe_result {
                Ok(()) => continue,
                Err(error) => error,
            },
        };
        tokio::select! {
            biased;
            result = line_reader.read_mcp_line() => return response_line(result, request_label),
            () = std::future::ready(()) => return Err(probe_failure),
        }
    }
}

fn response_line(
    result: std::io::Result<Option<String>>,
    request_label: &str,
) -> Result<Option<String>> {
    match result {
        Ok(line) => Ok(line),
        Err(error) if is_wire_oversized_io_error(&error) => Err(TraceDecayError::Config {
            message: format!(
                "daemon {request_label} response exceeded wire message bound ({WIRE_RECORD_TOO_LARGE})"
            ),
        }),
        Err(error) => Err(error.into()),
    }
}

#[tracing::instrument(
    name = "daemon_protocol.client.response.wait",
    level = "trace",
    skip_all
)]
pub async fn next_daemon_response_line<R>(
    reader: &mut BoundedLineReader<R>,
    connection: &DaemonConnection,
    request_label: &str,
    liveness_poll_interval: Duration,
) -> Result<Option<String>>
where
    R: asupersync::io::AsyncBufRead + Unpin,
{
    poll_daemon_response_line(reader, request_label, liveness_poll_interval, || {
        ensure_daemon_connection_live(connection, request_label)
    })
    .await
}

/// Writes the auth preface and the handshake line.
///
/// Callers that already hold a [`DaemonConnection`] use
/// [`write_daemon_preamble`]. The composition-root client uses this directly
/// because its connection type is the authority record, not the protocol
/// connection.
pub async fn write_daemon_handshake_preamble(
    writer: &mut (impl asupersync::io::AsyncWrite + Unpin),
    auth_token: &str,
    handshake: &DaemonHandshake,
) -> Result<()> {
    writer
        .write_all(DaemonAuthPreface::new(auth_token).to_line()?.as_bytes())
        .await?;
    writer.write_all(b"\n").await?;
    writer.write_all(handshake.to_line()?.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    Ok(())
}

#[tracing::instrument(name = "daemon_protocol.client.preamble", level = "trace", skip_all)]
pub async fn write_daemon_preamble(
    writer: &mut DaemonClientWriteHalf,
    connection: &DaemonConnection,
    handshake: &DaemonHandshake,
) -> Result<()> {
    write_daemon_handshake_preamble(writer, &connection.auth_token, handshake).await
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
        "The daemon is up but not accepting connections, likely overloaded. Retry shortly, or check `tracedecay daemon status`."
    } else {
        "The daemon may be restarting (e.g. after `tracedecay update`). Retry shortly, or check `tracedecay daemon status`."
    }
}

pub async fn connect_to_daemon_connection(
    connection: &DaemonConnection,
) -> Result<DaemonClientStream> {
    connect_with_restart_grace(
        connection,
        DAEMON_RESTART_GRACE,
        DAEMON_RESTART_POLL_INTERVAL,
    )
    .await
}

/// The endpoint a restart-grace connect attempt dials.
///
/// The kernel returns the connection object resolved for the successful
/// attempt, so each caller keeps its own credential and liveness authority
/// while sharing the retry loop.
pub trait DaemonConnectTarget {
    fn daemon_connect_endpoint(&self) -> &DaemonEndpoint;
}

impl DaemonConnectTarget for DaemonConnection {
    fn daemon_connect_endpoint(&self) -> &DaemonEndpoint {
        &self.endpoint
    }
}

#[tracing::instrument(name = "daemon_protocol.client.connect", level = "trace", skip_all)]
pub async fn connect_with_restart_grace(
    connection: &DaemonConnection,
    grace: Duration,
    poll_interval: Duration,
) -> Result<DaemonClientStream> {
    let (_, stream) = connect_with_restart_grace_resolving(
        || Ok(connection.clone()),
        |_| false,
        grace,
        poll_interval,
    )
    .await?;
    Ok(stream)
}

/// Connects to the daemon, tolerating a short restart outage.
///
/// `resolve` runs once per attempt. A caller whose target is fixed returns it
/// unchanged; a caller whose authority record can rotate resolves it afresh
/// each time, because a daemon restart rotates both its authority epoch and
/// authentication token, and a daemon's first start writes its record only
/// moments before it binds. `retry_resolution` classifies a failed resolution:
/// only errors it accepts are retried within `grace`; every other resolution
/// and connect error is terminal.
///
/// Retrying here is safe: nothing has been written yet, so no request can be
/// duplicated. Non-transient connect errors (e.g. permission denied) fail
/// immediately.
#[tracing::instrument(
    name = "daemon_protocol.client.connect_restart_grace",
    level = "trace",
    skip_all
)]
pub async fn connect_with_restart_grace_resolving<C>(
    mut resolve: impl FnMut() -> Result<C>,
    retry_resolution: impl Fn(&TraceDecayError) -> bool,
    grace: Duration,
    poll_interval: Duration,
) -> Result<(C, DaemonClientStream)>
where
    C: DaemonConnectTarget,
{
    let deadline = Instant::now() + grace;
    loop {
        let connection = match resolve() {
            Ok(connection) => connection,
            Err(error) if retry_resolution(&error) && Instant::now() < deadline => {
                asupersync::time::sleep(asupersync::time::wall_now(), poll_interval).await;
                continue;
            }
            Err(error) => return Err(error),
        };
        match DaemonClientStream::connect(connection.daemon_connect_endpoint()).await {
            Ok(stream) => return Ok((connection, stream)),
            Err(TraceDecayError::Io(err)) => {
                let endpoint = connection.daemon_connect_endpoint();
                if !is_transient_daemon_connect_error(err.kind()) || Instant::now() >= deadline {
                    return Err(if is_transient_daemon_connect_error(err.kind()) {
                        daemon_connect_failure(endpoint, &err)
                    } else {
                        TraceDecayError::Config {
                            message: format!(
                                "could not connect to TraceDecay daemon endpoint '{endpoint}': {err}. {}",
                                daemon_connect_failure_advice(err.kind())
                            ),
                        }
                    });
                }
                asupersync::time::sleep(asupersync::time::wall_now(), poll_interval).await;
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

    /// A daemon whose listen backlog is full must surface as the typed
    /// saturated refusal at once instead of a connect that never settles.
    /// Delete with the `connect_unix` workaround on asupersync 0.6.0.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn full_listen_backlog_fails_fast_as_saturated() {
        use socket2::{Domain, SockAddr, Socket, Type};

        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("daemon.sock");
        let address = SockAddr::unix(&socket).expect("socket address");
        let listener = Socket::new(Domain::UNIX, Type::STREAM, None).expect("listener socket");
        listener.bind(&address).expect("bind");
        listener.listen(0).expect("listen");
        let mut queued = Vec::new();
        loop {
            assert!(queued.len() < 64, "the listen backlog never filled");
            let peer = Socket::new(Domain::UNIX, Type::STREAM, None).expect("peer socket");
            peer.set_nonblocking(true).expect("non-blocking peer");
            match peer.connect(&address) {
                Ok(()) => queued.push(peer),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("filling the listen backlog failed: {error}"),
            }
        }
        let connection = DaemonConnection::new(DaemonEndpoint::Unix(socket), String::new());

        let error = tokio::time::timeout(
            Duration::from_secs(5),
            connect_with_restart_grace(&connection, Duration::ZERO, Duration::ZERO),
        )
        .await
        .expect("a saturated connect must fail instead of waiting")
        .expect_err("a full backlog must refuse the connect");

        assert_eq!(
            error
                .project_route_context()
                .map(|(code, retryable, _)| (code, retryable)),
            Some((DAEMON_CONNECT_SATURATED, true)),
            "{error}"
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

    /// The daemon side of one accepted request connection, driven by the
    /// client's liveness probe so the stop lands exactly while the probe runs.
    #[cfg(unix)]
    struct StoppingDaemon {
        listener: tokio::net::UnixListener,
        stream: tokio::net::UnixStream,
    }

    #[cfg(unix)]
    async fn await_response_while_daemon_stops(
        reply: Option<&'static [u8]>,
    ) -> Result<Option<String>> {
        use tokio::io::AsyncWriteExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("daemon.sock");
        let endpoint = DaemonEndpoint::Unix(socket.clone());
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind");
        let (client, _writer) = DaemonClientStream::connect(&endpoint)
            .await
            .expect("connect")
            .into_split();
        let (stream, _) = listener.accept().await.expect("accept");
        let daemon = std::cell::RefCell::new(Some(StoppingDaemon { listener, stream }));
        // The stopped daemon keeps the request connection open, so only the
        // probe can end a read that still blocks.
        let held_connection = std::cell::RefCell::new(None);
        let mut reader = BoundedLineReader::new(asupersync::io::BufReader::new(client));
        poll_daemon_response_line(&mut reader, "tracedecay_status", Duration::ZERO, || {
            let stopping = daemon.borrow_mut().take();
            let socket = socket.clone();
            let endpoint = &endpoint;
            let held_connection = &held_connection;
            async move {
                if let Some(StoppingDaemon {
                    listener,
                    mut stream,
                }) = stopping
                {
                    if let Some(reply) = reply {
                        stream.write_all(reply).await?;
                    }
                    drop(listener);
                    std::fs::remove_file(&socket)?;
                    held_connection.replace(Some(stream));
                }
                DaemonClientStream::connect(endpoint).await.map(drop)
            }
        })
        .await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reply_written_before_the_daemon_stops_listening_wins_over_the_failed_probe() {
        let answered = await_response_while_daemon_stops(Some(b"{\"jsonrpc\":\"2.0\",\"id\":1}\n"))
            .await
            .expect("a delivered reply must not be replaced by the probe failure");
        assert_eq!(answered.as_deref(), Some("{\"jsonrpc\":\"2.0\",\"id\":1}"));

        let unanswered = await_response_while_daemon_stops(None)
            .await
            .expect_err("a read that still blocks must end with the probe failure");
        assert!(
            matches!(&unanswered, TraceDecayError::Io(error) if error.kind() == std::io::ErrorKind::NotFound),
            "unanswered request must surface the probe's connect failure, got: {unanswered:?}"
        );

        let partial = await_response_while_daemon_stops(Some(b"{\"jsonrpc\":\"2.0\""))
            .await
            .expect_err("an incomplete frame still blocks, so the probe failure wins");
        assert!(
            matches!(&partial, TraceDecayError::Io(error) if error.kind() == std::io::ErrorKind::NotFound),
            "partial frame must surface the probe's connect failure, got: {partial:?}"
        );
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

#[cfg(test)]
mod restart_tests {
    #[cfg(unix)]
    #[test]
    fn transient_daemon_connect_errors_cover_restart_window_only() {
        assert!(super::is_transient_daemon_connect_error(
            std::io::ErrorKind::NotFound
        ));
        assert!(super::is_transient_daemon_connect_error(
            std::io::ErrorKind::ConnectionRefused
        ));
        assert!(
            super::is_transient_daemon_connect_error(std::io::ErrorKind::WouldBlock),
            "unix-socket EAGAIN (listen backlog full) must ride the restart grace"
        );
        assert!(!super::is_transient_daemon_connect_error(
            std::io::ErrorKind::PermissionDenied
        ));
    }

    /// A daemon restart rotates the authority record. A resolver that yields
    /// the replacement endpoint must have that *second* connection honoured,
    /// not the endpoint the first attempt tried.
    #[cfg(unix)]
    #[tokio::test]
    async fn resolving_connect_honours_a_rotated_authority_on_retry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stale_socket = dir.path().join("stale.sock");
        let live_socket = dir.path().join("live.sock");
        let _listener = tokio::net::UnixListener::bind(&live_socket).expect("bind live daemon");
        let mut attempts = 0_u32;

        let (connection, _stream) = super::connect_with_restart_grace_resolving(
            || {
                attempts += 1;
                let (socket, token) = if attempts == 1 {
                    (stale_socket.clone(), "stale")
                } else {
                    (live_socket.clone(), "rotated")
                };
                Ok(super::DaemonConnection::new(
                    super::DaemonEndpoint::Unix(socket),
                    token.to_owned(),
                ))
            },
            |_| false,
            std::time::Duration::from_secs(2),
            std::time::Duration::from_millis(10),
        )
        .await
        .expect("the successful attempt's connection must be returned");

        assert_eq!(attempts, 2, "the stale endpoint must be retried once");
        assert_eq!(connection.auth_token, "rotated");
        assert_eq!(
            connection.endpoint,
            super::DaemonEndpoint::Unix(live_socket)
        );
    }

    /// A retryable resolution failure (the daemon has not written its authority
    /// record yet) consumes an attempt without a connect; the next resolution
    /// inside the grace proceeds normally.
    #[cfg(unix)]
    #[tokio::test]
    async fn retryable_resolution_failures_are_retried_within_the_grace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let live_socket = dir.path().join("live.sock");
        let _listener = tokio::net::UnixListener::bind(&live_socket).expect("bind live daemon");
        let mut attempts = 0_u32;

        let (connection, _stream) = super::connect_with_restart_grace_resolving(
            || {
                attempts += 1;
                if attempts == 1 {
                    Err(tracedecay_domain::errors::TraceDecayError::project_route(
                        "daemon_authority_unavailable",
                        true,
                        "authority record not written yet",
                    ))
                } else {
                    Ok(super::DaemonConnection::new(
                        super::DaemonEndpoint::Unix(live_socket.clone()),
                        "token".to_owned(),
                    ))
                }
            },
            |error| {
                error
                    .project_route_context()
                    .is_some_and(|(code, retryable, _)| {
                        code == "daemon_authority_unavailable" && retryable
                    })
            },
            std::time::Duration::from_secs(2),
            std::time::Duration::from_millis(10),
        )
        .await
        .expect("a retryable resolution failure must be retried");

        assert_eq!(attempts, 2);
        assert_eq!(connection.auth_token, "token");
    }

    /// A fixed target keeps one connection for every attempt: the retry count
    /// stays bounded by the grace and the poll interval, exactly as before the
    /// kernel was shared.
    #[cfg(unix)]
    #[tokio::test]
    async fn fixed_target_connect_retries_the_same_endpoint_until_the_grace_ends() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("never-bound.sock");
        let connection =
            super::DaemonConnection::new(super::DaemonEndpoint::Unix(socket), "fixed".to_owned());
        let grace = std::time::Duration::from_millis(200);
        let poll = std::time::Duration::from_millis(20);
        let mut attempts = 0_u32;
        let started = std::time::Instant::now();

        let error = match super::connect_with_restart_grace_resolving(
            || {
                attempts += 1;
                Ok(connection.clone())
            },
            |_| false,
            grace,
            poll,
        )
        .await
        {
            Ok(_) => panic!("no daemon ever binds the fixed endpoint"),
            Err(error) => error,
        };

        assert!(
            started.elapsed() >= grace,
            "restart grace must be fully observed"
        );
        assert!(attempts > 1, "fixed-target connects must retry: {attempts}");
        assert!(
            u128::from(attempts) <= grace.as_millis() / poll.as_millis() + 2,
            "retries must stop within one poll interval after grace: {attempts} attempts"
        );
        assert_eq!(
            error
                .project_route_context()
                .map(|(code, retryable, _)| (code, retryable)),
            Some((super::DAEMON_CONNECT_DOWN, true)),
            "{error}"
        );
    }
}
