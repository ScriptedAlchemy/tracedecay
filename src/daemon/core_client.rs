//! Daemon client side: connection discovery, restart-grace connects, and
//! one-shot JSON-RPC tool calls against the daemon.

use std::path::{Path, PathBuf};

use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::time::{Duration, Instant, timeout};

#[cfg(unix)]
use super::unavailable_error;
use super::{
    BrokerStream, DaemonAuthPreface, DaemonClientDeadline, DaemonEndpoint, DaemonHandshake,
    JsonRpcRequest, JsonRpcResponse, PROJECT_OPEN_RETRY_GRACE, PROJECT_OPEN_RETRY_INTERVAL, Result,
    TraceDecayError, authority, default_socket_path, error_message_is_project_open_retryable,
};

pub(crate) const DAEMON_TOOL_LIVENESS_POLL_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const DAEMON_TOOL_HEALTH_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

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
pub const DAEMON_TOOL_RESPONSE_GRACE: Duration = Duration::from_secs(30);

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

#[derive(Clone)]
pub(crate) struct DaemonConnection {
    pub(crate) endpoint: DaemonEndpoint,
    pub(crate) auth_token: Option<String>,
    pub(super) authority_record: Option<authority::DaemonAuthorityRecord>,
}

impl DaemonConnection {
    #[cfg(test)]
    pub(crate) fn unauthenticated_for_test(endpoint: DaemonEndpoint) -> Self {
        Self {
            endpoint,
            auth_token: None,
            authority_record: None,
        }
    }
}

pub(crate) fn current_daemon_connection() -> Result<DaemonConnection> {
    let profile_root = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
    let record =
        authority::current_record(&profile_root)?.ok_or_else(|| TraceDecayError::Config {
            message:
                "TraceDecay daemon authority record is not available. Start or restart the daemon."
                    .to_string(),
        })?;
    Ok(DaemonConnection {
        endpoint: record.endpoint.clone(),
        auth_token: Some(record.auth_token.clone()),
        authority_record: Some(record),
    })
}

#[cfg(unix)]
pub(crate) fn connection_for_socket_path(socket_path: &Path) -> DaemonConnection {
    if let Ok(connection) = current_daemon_connection()
        && let DaemonEndpoint::Unix(authority_path) = &connection.endpoint
        && authority::canonical_identity_path(authority_path).ok()
            == authority::canonical_identity_path(socket_path).ok()
    {
        return connection;
    }
    if let Some(profile_root) = socket_path.parent()
        && let Ok(Some(record)) = authority::current_record(profile_root)
        && let DaemonEndpoint::Unix(authority_path) = &record.endpoint
        && authority::canonical_identity_path(authority_path).ok()
            == authority::canonical_identity_path(socket_path).ok()
    {
        return DaemonConnection {
            endpoint: record.endpoint.clone(),
            auth_token: Some(record.auth_token.clone()),
            authority_record: Some(record),
        };
    }
    // Explicit paths are retained for test harnesses and legacy one-shot
    // callers without a discoverable authority record. Default production
    // routing always uses the authority record.
    DaemonConnection {
        endpoint: DaemonEndpoint::Unix(socket_path.to_path_buf()),
        auth_token: None,
        authority_record: None,
    }
}

pub(crate) async fn ensure_daemon_connection_live(
    connection: &DaemonConnection,
    request_label: &str,
) -> Result<()> {
    if let Some(expected) = connection.authority_record.as_ref() {
        let current = authority::current_record(&expected.profile_root)?;
        let Some(current) = current else {
            return Err(TraceDecayError::Config {
                message: format!(
                    "daemon authority disappeared while request '{request_label}' was awaiting a response; the request was already sent and was not retried"
                ),
            });
        };
        if current.epoch != expected.epoch || current.process_run_id != expected.process_run_id {
            return Err(TraceDecayError::Config {
                message: format!(
                    "daemon restarted while request '{request_label}' was awaiting a response (expected epoch {}, current epoch {}); the request was already sent and was not retried",
                    expected.epoch, current.epoch
                ),
            });
        }
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

pub(crate) async fn next_daemon_response_line<R>(
    reader: &mut R,
    connection: &DaemonConnection,
    request_label: &str,
    liveness_poll_interval: Duration,
) -> Result<Option<String>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tracedecay_usecases::host_admission::{is_wire_oversized_io_error, read_bounded_mcp_line};

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
                                "daemon {request_label} response exceeded wire message bound ({})",
                                tracedecay_usecases::host_admission::WIRE_RECORD_TOO_LARGE
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

// Windows discovers the current daemon through a fallible endpoint lookup;
// Unix keeps the same cross-platform contract even though its path is infallible.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn client_connection(socket_path: &Path) -> Result<DaemonConnection> {
    #[cfg(unix)]
    {
        Ok(connection_for_socket_path(socket_path))
    }
    #[cfg(not(unix))]
    {
        let _ = socket_path;
        current_daemon_connection()
    }
}

pub(crate) async fn write_daemon_preamble(
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
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

pub(crate) async fn connect_to_daemon_connection(
    connection: &DaemonConnection,
) -> Result<BrokerStream> {
    connect_to_daemon_connection_within(connection, None).await
}

pub(crate) async fn connect_to_daemon_connection_within(
    connection: &DaemonConnection,
    client_deadline: Option<DaemonClientDeadline>,
) -> Result<BrokerStream> {
    let grace = match client_deadline {
        Some(deadline) => deadline.remaining()?.min(DAEMON_RESTART_GRACE),
        None => DAEMON_RESTART_GRACE,
    };
    let (_, stream) = connect_with_restart_grace_resolving(
        || Ok(connection.clone()),
        grace,
        DAEMON_RESTART_POLL_INTERVAL,
    )
    .await?;
    Ok(stream)
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
                    return Err(TraceDecayError::Config {
                        message: format!(
                            "could not connect to TraceDecay daemon endpoint '{}': {err}. The daemon may be restarting (e.g. after `tracedecay update`) — retry shortly, or check `tracedecay daemon status`.",
                            connection.endpoint
                        ),
                    });
                }
                tokio::time::sleep(poll_interval).await;
            }
            Err(error) => return Err(error),
        }
    }
}

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
    let (reader, mut writer) = stream.into_split();
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
            crate::mcp::tool_call_deadline::tool_call_deadline_meta(wire_request_deadline_micros(
                deadline,
            )),
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
                let remaining = DaemonClientDeadline::until(deadline)?.remaining()?;
                tokio::time::sleep(remaining.min(PROJECT_OPEN_RETRY_INTERVAL)).await;
            }
            result => return result,
        }
    }
}

pub async fn call_default_tool(
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value> {
    let socket_path = default_available_socket_path()?;
    match call_tool(&socket_path, handshake, tool_name, arguments.clone()).await {
        Err(error) if is_project_open_retryable_error(&error) => {
            call_tool_with_project_open_retry(
                &socket_path,
                handshake,
                tool_name,
                arguments,
                Instant::now() + PROJECT_OPEN_RETRY_GRACE,
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
) -> crate::errors::Result<serde_json::Value> {
    let blocks = result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| crate::errors::TraceDecayError::Config {
            message: format!("daemon tool {tool_name} returned no content blocks"),
        })?;
    let mut payloads = blocks
        .iter()
        .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
        .filter_map(|text| serde_json::from_str(text).ok());
    let payload = payloads
        .next()
        .ok_or_else(|| crate::errors::TraceDecayError::Config {
            message: format!("daemon tool {tool_name} returned no JSON payload"),
        })?;
    if payloads.next().is_some() {
        return Err(crate::errors::TraceDecayError::Config {
            message: format!("daemon tool {tool_name} returned multiple JSON payloads"),
        });
    }
    Ok(payload)
}
