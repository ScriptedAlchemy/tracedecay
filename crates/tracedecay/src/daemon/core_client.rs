//! Daemon client side: restart-grace connects and one-shot JSON-RPC tool
//! calls against the daemon. Connection discovery — resolving the profile's
//! authority record into an endpoint plus credential — lives in
//! `tracedecay-daemon-identity`; this module only consumes the
//! [`DaemonConnection`] it resolves.

use std::path::{Path, PathBuf};

use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::time::{Duration, Instant, timeout};
use tracedecay_daemon_control::default_socket_path;
#[cfg(not(unix))]
use tracedecay_daemon_identity::current_daemon_connection;
use tracedecay_daemon_identity::{DaemonConnection, client_connection};
use tracedecay_framing::{
    WIRE_RECORD_TOO_LARGE, is_wire_oversized_io_error, read_bounded_mcp_line,
};

pub(crate) use tracedecay_daemon_protocol::DAEMON_TOOL_LIVENESS_POLL_INTERVAL;
pub use tracedecay_daemon_protocol::{
    DAEMON_CONNECT_DOWN, DAEMON_CONNECT_SATURATED, DAEMON_RESPONSE_STALLED,
    DAEMON_TOOL_RESPONSE_GRACE, DEFAULT_TOOL_REQUEST_DEADLINE, MAX_TOOL_REQUEST_DEADLINE,
    TOOL_REQUEST_DEADLINE_ENV, tool_request_deadline,
};

#[cfg(unix)]
use super::unavailable_error;
use super::{
    BrokerStream, DaemonAuthPreface, DaemonClientDeadline, DaemonHandshake, JsonRpcRequest,
    JsonRpcResponse, PROJECT_OPEN_RETRY_GRACE, PROJECT_OPEN_RETRY_INTERVAL, Result,
    TraceDecayError, error_message_is_project_open_retryable,
};

/// Bounded grace a client keeps reading for *after* the caller's request
/// deadline has elapsed.
///
/// The request deadline belongs to the daemon: it is what admission measures
/// and what the retained owners settle against, and its whole purpose is to
/// produce a typed terminal — a `PartialEffect` carrying a committed receipt, a
/// typed timeout — rather than silence. Bounding the client's *read* by that
/// same instant made every one of those terminals unobservable through this
/// transport: the client abandoned the connection moments before the envelope
/// it had asked for arrived and reported "outcome may be unknown" while the
/// outcome was already on the wire. The read bound must therefore outlive the
/// request deadline; this is by how much. It bounds only a dead or wedged
/// daemon, never the request.
/// The local read bound for a request whose caller deadline is `request_deadline`.
pub fn daemon_tool_response_bound(request_deadline: Instant) -> Result<Instant> {
    request_deadline
        .checked_add(DAEMON_TOOL_RESPONSE_GRACE)
        .ok_or_else(|| TraceDecayError::Config {
            message: "daemon tool response bound exceeds the supported monotonic range".to_string(),
        })
}

/// The caller's request deadline as an absolute wall-clock instant, for the
/// wire.
///
/// The monotonic `Instant` a CLI caller holds cannot cross a process boundary;
/// the daemon measures admission against UTC micros. Converting the *remaining*
/// budget at send time keeps the two clocks independent and makes a re-send
/// (project-open retry) carry the correctly shrunken budget rather than the
/// original one.
fn wire_request_deadline_micros(request_deadline: Instant) -> tracedecay_domain::UtcMicros {
    let remaining = request_deadline.saturating_duration_since(Instant::now());
    let now = tracedecay_application::clock::now_micros();
    tracedecay_domain::UtcMicros(
        now.0
            .saturating_add(i64::try_from(remaining.as_micros()).unwrap_or(i64::MAX)),
    )
}

/// How long daemon clients keep retrying a failed connect before giving up.
///
/// `tracedecay update` restarts the daemon service (`systemctl --user restart`);
/// between the old daemon unlinking its socket and the new one binding it,
/// connects fail with `NotFound` or `ConnectionRefused`. Long-lived MCP
/// sessions (Cursor's `tracedecay serve` stdio proxy) reconnect per request,
/// so retrying inside this window lets a live session ride out a self-update
/// instead of surfacing a hard JSON-RPC error.
pub(crate) const DAEMON_RESTART_GRACE: Duration = Duration::from_secs(8);
pub(crate) const DAEMON_RESTART_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[hotpath::measure(label = "daemon.core.ensure_connection_live", future = true)]
pub(crate) async fn ensure_daemon_connection_live(
    connection: &DaemonConnection,
    request_label: &str,
) -> Result<()> {
    connection.ensure_authority_current(request_label)?;
    Ok(())
}

#[hotpath::measure(label = "daemon.core.next_response", future = true)]
pub(crate) async fn next_daemon_response_line<R>(
    reader: &mut R,
    connection: &DaemonConnection,
    request_label: &str,
    liveness_poll_interval: Duration,
) -> Result<Option<String>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    // Pin one frame-read future for the whole wait. Liveness polls must not
    // recreate `read_bounded_mcp_line`: that future owns the partial-frame
    // accumulator after bytes have already been consumed from `reader`.
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

pub(crate) async fn write_daemon_preamble(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
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

pub(crate) fn default_available_socket_path() -> Result<PathBuf> {
    let socket_path = default_socket_path()?;
    #[cfg(unix)]
    {
        if socket_path.exists() {
            Ok(socket_path)
        } else {
            Err(unavailable_error(&socket_path))
        }
    }
    #[cfg(not(unix))]
    {
        current_daemon_connection()?;
        Ok(socket_path)
    }
}

pub(crate) fn is_transient_daemon_connect_error(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::WouldBlock
    )
}

pub(crate) fn is_saturated_daemon_connect_error(kind: std::io::ErrorKind) -> bool {
    kind == std::io::ErrorKind::WouldBlock
}

pub(crate) fn daemon_connect_failure_advice(kind: std::io::ErrorKind) -> &'static str {
    if is_saturated_daemon_connect_error(kind) {
        "The daemon is up but not accepting connections — likely overloaded. Retry shortly, or check `tracedecay daemon status`."
    } else {
        "The daemon may be restarting (e.g. after `tracedecay update`) — retry shortly, or check `tracedecay daemon status`."
    }
}

pub(crate) async fn connect_to_current_daemon_within(
    socket_path: &Path,
    client_deadline: Option<DaemonClientDeadline>,
) -> Result<(DaemonConnection, BrokerStream)> {
    let grace = match client_deadline {
        Some(deadline) => deadline.remaining()?.min(DAEMON_RESTART_GRACE),
        None => DAEMON_RESTART_GRACE,
    };
    connect_with_restart_grace_resolving(
        || client_connection(socket_path),
        grace,
        DAEMON_RESTART_POLL_INTERVAL,
    )
    .await
}

/// Connects to the daemon socket, tolerating a short restart outage.
///
/// Retrying here is safe: nothing has been written yet, so no request can be
/// duplicated. Non-transient errors (e.g. permission denied) fail immediately.
#[cfg(unix)]
pub(crate) async fn connect_with_restart_grace(
    connection: &DaemonConnection,
    grace: Duration,
    poll_interval: Duration,
) -> Result<BrokerStream> {
    let (_, stream) =
        connect_with_restart_grace_resolving(|| Ok(connection.clone()), grace, poll_interval)
            .await?;
    Ok(stream)
}

/// Resolves endpoint authority on every retry because a daemon restart rotates
/// both its authority epoch and authentication token.
#[hotpath::measure(label = "daemon.core.connect_restart_grace", future = true)]
async fn connect_with_restart_grace_resolving(
    mut resolve: impl FnMut() -> Result<DaemonConnection>,
    grace: Duration,
    poll_interval: Duration,
) -> Result<(DaemonConnection, BrokerStream)> {
    let deadline = Instant::now() + grace;
    loop {
        let connection = resolve()?;
        match BrokerStream::connect(&connection.endpoint).await {
            Ok(stream) => return Ok((connection, stream)),
            Err(TraceDecayError::Io(err)) => {
                if !is_transient_daemon_connect_error(err.kind()) || Instant::now() >= deadline {
                    return Err(if is_transient_daemon_connect_error(err.kind()) {
                        tracedecay_daemon_protocol::daemon_connect_failure(
                            &connection.endpoint,
                            &err,
                        )
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

#[hotpath::measure(label = "daemon.core.call_tool", future = true)]
pub(crate) async fn call_tool_with_liveness_poll(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: serde_json::Value,
    liveness_poll_interval: Duration,
    request_deadline: Option<Instant>,
) -> Result<serde_json::Value> {
    // Two different bounds, deliberately: the caller's deadline travels to the
    // daemon so admission and settlement measure the budget the caller actually
    // asked for, while the local I/O bound is that deadline plus a bounded
    // response grace so the typed terminal the deadline produces is still read.
    let client_deadline = match request_deadline {
        Some(deadline) => Some(DaemonClientDeadline::until(daemon_tool_response_bound(
            deadline,
        )?)?),
        None => None,
    };
    let (connection, stream) = match client_deadline {
        Some(deadline) => {
            deadline
                .run("connect", tool_name, async {
                    connect_to_current_daemon_within(socket_path, Some(deadline)).await
                })
                .await?
        }
        None => connect_to_current_daemon_within(socket_path, None).await?,
    };
    let (reader, mut writer) = stream.into_owned_split();
    let id = json!(1);
    let mut params = json!({
        "name": tool_name,
        "arguments": arguments,
    });
    if let Some(deadline) = request_deadline
        && let Some(params) = params.as_object_mut()
    {
        params.insert(
            "_meta".to_owned(),
            tracedecay_mcp::tool_call_deadline_meta(wire_request_deadline_micros(deadline)),
        );
    }
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(id.clone()),
        method: "tools/call".to_string(),
        params: Some(params),
    };

    let write = async {
        write_daemon_preamble(&mut writer, &connection, handshake).await?;
        writer
            .write_all(serde_json::to_string(&request)?.as_bytes())
            .await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    };
    match client_deadline {
        Some(deadline) => deadline.run("write", tool_name, write).await?,
        None => write.await?,
    }

    let mut reader = tokio::io::BufReader::new(reader);
    loop {
        let read =
            next_daemon_response_line(&mut reader, &connection, tool_name, liveness_poll_interval);
        let line = match client_deadline {
            Some(deadline) => deadline.run("read", tool_name, read).await?,
            None => read.await?,
        };
        let Some(line) = line else {
            return Err(TraceDecayError::Config {
                message: "daemon closed the connection after the tool request was sent but before returning a result; the outcome is unknown and the request was not retried"
                    .to_string(),
            });
        };
        // A daemon that refused this connection's preamble answers with one
        // refusal frame (no JSON-RPC id) before EOF; skipping it as a
        // non-matching response line reported the definitive refusal as a
        // closed-connection mystery.
        if let Some(refusal) = tracedecay_daemon_protocol::DaemonHandshakeRefusal::from_line(&line)
        {
            return Err(tracedecay_daemon_protocol::handshake_refusal_error(
                &refusal, handshake,
            ));
        }
        let response = if let Some(deadline) = client_deadline {
            deadline
                .run("decode", tool_name, async {
                    let value: serde_json::Value =
                        serde_json::from_str(&line).map_err(|error| TraceDecayError::Config {
                            message: format!("daemon tool response JSON decode failed: {error}"),
                        })?;
                    if value.get("id") != Some(&id) {
                        return Ok(None);
                    }
                    let response: JsonRpcResponse =
                        serde_json::from_value(value).map_err(|error| TraceDecayError::Config {
                            message: format!(
                                "daemon tool response JSON-RPC decode failed: {error}"
                            ),
                        })?;
                    Ok(Some(response))
                })
                .await?
        } else {
            let value: serde_json::Value =
                serde_json::from_str(&line).map_err(|error| TraceDecayError::Config {
                    message: format!("daemon tool response JSON decode failed: {error}"),
                })?;
            if value.get("id") == Some(&id) {
                Some(
                    serde_json::from_value(value).map_err(|error| TraceDecayError::Config {
                        message: format!("daemon tool response JSON-RPC decode failed: {error}"),
                    })?,
                )
            } else {
                None
            }
        };
        let Some(response) = response else {
            continue;
        };
        if let Some(error) = response.error {
            return Err(TraceDecayError::Config {
                message: format!("daemon tool call failed: {}", error.message),
            });
        }
        return response.result.ok_or_else(|| TraceDecayError::Config {
            message: "daemon tool call response did not include a result".to_string(),
        });
    }
}

/// Unbounded one-shot call. Production clients use [`call_default_tool`] or
/// [`call_tool_within`]; this primitive stays for tests and harnesses that
/// supply their own outer deadline.
pub async fn call_tool(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value> {
    call_tool_with_liveness_poll(
        socket_path,
        handshake,
        tool_name,
        arguments,
        DAEMON_TOOL_LIVENESS_POLL_INTERVAL,
        None,
    )
    .await
}

/// Calls a daemon tool with `deadline` as the *caller's request deadline*.
///
/// The deadline is sent to the daemon, which enforces it; the local read runs
/// on that deadline plus [`DAEMON_TOOL_RESPONSE_GRACE`] so a deadline-elapsed
/// typed terminal is read rather than raced.
pub async fn call_tool_within(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: serde_json::Value,
    deadline: Instant,
) -> Result<serde_json::Value> {
    call_tool_with_liveness_poll(
        socket_path,
        handshake,
        tool_name,
        arguments,
        DAEMON_TOOL_LIVENESS_POLL_INTERVAL,
        Some(deadline),
    )
    .await
}

fn is_project_open_retryable_error(error: &TraceDecayError) -> bool {
    error_message_is_project_open_retryable(&error.to_string())
}

#[hotpath::measure(label = "daemon.core.call_tool_retry", future = true)]
async fn call_tool_with_project_open_retry(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: serde_json::Value,
    deadline: Instant,
) -> Result<serde_json::Value> {
    loop {
        match call_tool_within(
            socket_path,
            handshake,
            tool_name,
            arguments.clone(),
            deadline,
        )
        .await
        {
            Err(error) if is_project_open_retryable_error(&error) => {
                // When the budget is spent, the daemon's own typed state (a
                // warming project, a still-mounting authority) is the truthful
                // answer; the client's deadline bookkeeping error is not.
                let remaining = DaemonClientDeadline::until(deadline)
                    .and_then(|client_deadline| client_deadline.remaining());
                match remaining {
                    Ok(remaining) => {
                        tokio::time::sleep(remaining.min(PROJECT_OPEN_RETRY_INTERVAL)).await;
                    }
                    Err(_) => return Err(error),
                }
            }
            result => return result,
        }
    }
}

/// Calls a daemon tool with the shared [`tool_request_deadline`] envelope.
///
/// Production one-shot clients must not read forever against a stalled-but
/// accepting daemon. The request deadline travels on the wire; the local read
/// waits that deadline plus the 30s response grace. A warming project still
/// retries for at most the 15s open grace, never past this envelope. Callers
/// that need a different budget use [`call_default_tool_within`] or
/// [`call_default_tool_awaiting_project_open`].
pub async fn call_default_tool(
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value> {
    let socket_path = default_available_socket_path()?;
    let deadline = Instant::now() + tool_request_deadline()?;
    match call_tool_within(
        &socket_path,
        handshake,
        tool_name,
        arguments.clone(),
        deadline,
    )
    .await
    {
        Err(error) if is_project_open_retryable_error(&error) => {
            let retry_deadline = Instant::now() + PROJECT_OPEN_RETRY_GRACE;
            call_tool_with_project_open_retry(
                &socket_path,
                handshake,
                tool_name,
                arguments,
                retry_deadline.min(deadline),
            )
            .await
        }
        result => result,
    }
}

pub async fn call_default_tool_within(
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: serde_json::Value,
    deadline: Instant,
) -> Result<serde_json::Value> {
    let socket_path = default_available_socket_path()?;
    // Deadline-aware application callers need the daemon's typed warming
    // response. Retrying that response until `deadline` turns a useful
    // temporary state into a client-side timeout with no response body.
    call_tool_within(&socket_path, handshake, tool_name, arguments, deadline).await
}

/// Calls a daemon tool, waiting out a warming project until `deadline`.
///
/// Bootstrap callers deliberately trigger the cold open they are waiting for,
/// so the warming hint is progress rather than an answer: `tracedecay init`
/// asks for a status it can only get after the open completes. That is the
/// opposite of [`call_default_tool_within`], whose callers want the typed
/// warming state returned to them, and wider than [`call_default_tool`], whose
/// grace is sized for an already-open project rather than a first index.
pub async fn call_default_tool_awaiting_project_open(
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: serde_json::Value,
    deadline: Instant,
) -> Result<serde_json::Value> {
    let socket_path = default_available_socket_path()?;
    call_tool_with_project_open_retry(&socket_path, handshake, tool_name, arguments, deadline).await
}

/// Extracts the single JSON payload from an MCP tool result while ignoring
/// human-facing notice blocks.
#[doc(hidden)]
pub fn tool_json_payload(
    result: &serde_json::Value,
    tool_name: &str,
) -> tracedecay_domain::errors::Result<serde_json::Value> {
    let blocks = result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("daemon tool {tool_name} returned no content blocks"),
        })?;
    let mut payloads = blocks
        .iter()
        .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
        .filter_map(|text| serde_json::from_str(text).ok());
    let payload =
        payloads
            .next()
            .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("daemon tool {tool_name} returned no JSON payload"),
            })?;
    if payloads.next().is_some() {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("daemon tool {tool_name} returned multiple JSON payloads"),
        });
    }
    Ok(payload)
}
