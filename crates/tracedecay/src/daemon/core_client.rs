//! Daemon client side: restart-grace connects and one-shot JSON-RPC tool
//! calls against the daemon. Connection discovery, resolving the profile's
//! authority record into an endpoint plus credential, lives in
//! `tracedecay-daemon-identity`; this module only consumes the
//! [`ResolvedDaemonConnection`] it resolves.

use std::path::{Path, PathBuf};

use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::time::{Duration, Instant, timeout};
use tracedecay_daemon_control::default_socket_path;
#[cfg(not(unix))]
use tracedecay_daemon_identity::current_daemon_connection;
use tracedecay_daemon_identity::{
    DAEMON_AUTHORITY_UNAVAILABLE, ResolvedDaemonConnection, client_connection,
};
pub(crate) use tracedecay_daemon_protocol::DAEMON_TOOL_LIVENESS_POLL_INTERVAL;
pub(crate) use tracedecay_daemon_protocol::connection::{
    DAEMON_RESTART_GRACE, DAEMON_RESTART_POLL_INTERVAL, daemon_connect_failure_advice,
    is_transient_daemon_connect_error,
};
pub use tracedecay_daemon_protocol::daemon_tool_response_bound;
use tracedecay_daemon_protocol::tool_request_deadline;
use tracedecay_mcp::server::attach_stateless_request_context;

use super::{
    BrokerStream, DaemonClientDeadline, DaemonHandshake, JsonRpcError, JsonRpcRequest,
    JsonRpcResponse, PROJECT_OPEN_RETRY_GRACE, PROJECT_OPEN_RETRY_INTERVAL, Result,
    TraceDecayError, error_is_project_open_retryable, tool_call_transport_error_is_retryable,
};
#[cfg(unix)]
use tracedecay_daemon_service::logging::unavailable_error;

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
    let now = tracedecay_contracts::clock::now_micros();
    tracedecay_domain::UtcMicros(
        now.0
            .saturating_add(i64::try_from(remaining.as_micros()).unwrap_or(i64::MAX)),
    )
}

/// How long a liveness probe waits for the daemon endpoint to accept a
/// connection before the in-flight request is declared unreachable.
const DAEMON_TOOL_HEALTH_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// Liveness for a one-shot request that is already on the wire.
///
/// Two independent facts have to hold, and neither implies the other:
///
/// * the authority record that named this endpoint is still the current one,
///   which catches a daemon that restarted under a rotated epoch while its
///   old connection was never closed; and
/// * the endpoint still accepts connections, which catches a daemon that
///   stopped listening (socket unlinked, listener dropped) while holding this
///   connection open. Nothing on the read half distinguishes that from a
///   healthy daemon still computing a long answer, so without the probe the
///   caller waits out its whole deadline on a daemon that can never answer.
///
/// The one-shot tool-call and stdio-proxy clients open exactly one connection
/// per request, so a probe connection here costs one accept per poll interval
/// and never competes with a pooled connection budget.
#[hotpath::measure(label = "daemon.core.ensure_connection_live", future = true)]
pub(crate) async fn ensure_daemon_connection_live(
    connection: &ResolvedDaemonConnection,
    request_label: &str,
) -> Result<()> {
    connection.ensure_authority_current(request_label)?;

    timeout(
        DAEMON_TOOL_HEALTH_CONNECT_TIMEOUT,
        BrokerStream::connect(connection.endpoint()),
    )
    .await
    .map_err(|_| TraceDecayError::Config {
        message: format!(
            "daemon health check timed out at '{}' while request '{request_label}' was awaiting a response; the request was already sent and was not retried",
            connection.endpoint()
        ),
    })?
    .map(|_| ())
    .map_err(|error| TraceDecayError::Config {
        message: format!(
            "daemon became unreachable at '{}' while request '{request_label}' was awaiting a response: {error}; the request was already sent and was not retried",
            connection.endpoint()
        ),
    })
}

#[hotpath::measure(label = "daemon.core.next_response", future = true)]
pub(crate) async fn next_daemon_response_line<R>(
    reader: &mut R,
    connection: &ResolvedDaemonConnection,
    request_label: &str,
    liveness_poll_interval: Duration,
) -> Result<Option<String>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    tracedecay_daemon_protocol::poll_daemon_response_line(
        reader,
        request_label,
        liveness_poll_interval,
        || ensure_daemon_connection_live(connection, request_label),
    )
    .await
}

pub(crate) async fn write_daemon_preamble(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    connection: &ResolvedDaemonConnection,
    handshake: &DaemonHandshake,
) -> Result<()> {
    tracedecay_daemon_protocol::write_daemon_handshake_preamble(
        writer,
        connection.auth_token(),
        handshake,
    )
    .await
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

pub(crate) async fn connect_to_current_daemon_within(
    socket_path: &Path,
    client_deadline: Option<DaemonClientDeadline>,
) -> Result<(ResolvedDaemonConnection, BrokerStream)> {
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
    socket_path: &Path,
    grace: Duration,
    poll_interval: Duration,
) -> Result<BrokerStream> {
    let (_, stream) = connect_with_restart_grace_resolving(
        || client_connection(socket_path),
        grace,
        poll_interval,
    )
    .await?;
    Ok(stream)
}

/// Resolves endpoint authority on every retry because a daemon restart rotates
/// both its authority epoch and authentication token, and a daemon's first
/// start writes its record only moments before it binds.
#[hotpath::measure(label = "daemon.core.connect_restart_grace", future = true)]
async fn connect_with_restart_grace_resolving(
    mut resolve: impl FnMut() -> Result<ResolvedDaemonConnection>,
    grace: Duration,
    poll_interval: Duration,
) -> Result<(ResolvedDaemonConnection, BrokerStream)> {
    let deadline = Instant::now() + grace;
    loop {
        let connection = match resolve() {
            Ok(connection) => connection,
            Err(error) if authority_absent(&error) && Instant::now() < deadline => {
                tokio::time::sleep(poll_interval).await;
                continue;
            }
            Err(error) => return Err(error),
        };
        match BrokerStream::connect(connection.endpoint()).await {
            Ok(stream) => return Ok((connection, stream)),
            Err(TraceDecayError::Io(err)) => {
                if !is_transient_daemon_connect_error(err.kind()) || Instant::now() >= deadline {
                    return Err(if is_transient_daemon_connect_error(err.kind()) {
                        tracedecay_daemon_protocol::daemon_connect_failure(
                            connection.endpoint(),
                            &err,
                        )
                    } else {
                        TraceDecayError::Config {
                            message: format!(
                                "could not connect to TraceDecay daemon endpoint '{}': {err}. {}",
                                connection.endpoint(),
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

fn authority_absent(error: &TraceDecayError) -> bool {
    error
        .project_route_context()
        .is_some_and(|(code, retryable, _)| code == DAEMON_AUTHORITY_UNAVAILABLE && retryable)
}

#[hotpath::measure(label = "daemon.core.call_tool", future = true)]
#[cfg_attr(
    not(feature = "hotpath"),
    expect(
        clippy::too_many_lines,
        reason = "A tool call and its liveness poll share one client deadline and must complete as one RPC."
    )
)]
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
    let mut request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(id.clone()),
        method: "tools/call".to_string(),
        params: Some(params),
    };
    attach_stateless_request_context(&mut request);

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
            return Err(daemon_tool_call_error(error));
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
/// on that deadline plus [`tracedecay_daemon_protocol::DAEMON_TOOL_RESPONSE_GRACE`] so a deadline-elapsed
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

/// Transport errors the one-shot client rides out on its own cadence: a
/// project open that has not finished (warming, deferred discovery, a
/// saturated open queue) and a retained project server retired mid-response
/// during a composition upgrade. The daemon types every one of these
/// `retryable: true`; a client that honours only the open subset reports the
/// upgrade window as a hard failure.
fn is_project_open_retryable_error(error: &TraceDecayError) -> bool {
    error_is_project_open_retryable(error) || tool_call_transport_error_is_retryable(error)
}

/// Reconstruct a typed daemon tool refusal from the JSON-RPC error frame.
///
/// Warming, deferred discovery, capacity, and response-revoked refusals carry
/// `data.reason_code`; those must round-trip as [`TraceDecayError::ProjectRoute`]
/// so journey/client retry keys on the code rather than English detail.
fn daemon_tool_call_error(error: JsonRpcError) -> TraceDecayError {
    // A refused persisted shape stays the typed reset state across the wire:
    // the CLI names the refused authority and the exact reset command from it.
    if let Some(data) = error.data.as_ref()
        && data.get("kind").and_then(serde_json::Value::as_str) == Some("reset_required")
        && let Some(authority) = data.get("authority").and_then(serde_json::Value::as_str)
    {
        let reason = data
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(error.message.as_str());
        return TraceDecayError::reset_required(authority, reason);
    }
    if let Some(data) = error.data.as_ref()
        && let Some(reason_code) = data.get("reason_code").and_then(serde_json::Value::as_str)
    {
        let retryable = data
            .get("retryable")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let detail = data
            .get("detail")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(error.message.as_str());
        return TraceDecayError::project_route(reason_code, retryable, detail);
    }
    TraceDecayError::Config {
        message: format!("daemon tool call failed: {}", error.message),
    }
}

/// The delay before re-sending a completed tool result, when that result is
/// the publication-window mounting refusal.
///
/// A project-scoped owner that registers behind the core publication answers
/// `application.runtime.mounting` while it is still mounting. The daemon
/// renders that record under the tool result's `problem` member. An admitted
/// terminal, and every other completed problem (a retained authority that is
/// unavailable, a saturated owner, an observed diagnostic), is the answer:
/// its `after_delay` directive is for the caller, not a transport loop.
fn tool_result_retry_after_delay(result: &serde_json::Value) -> Option<Duration> {
    let record: tracedecay_contracts::ApplicationProblemRecord =
        serde_json::from_value(result.get("problem")?.clone()).ok()?;
    record.owner_mount_resend_delay()
}

/// How long to wait before re-sending the request whose outcome is `result`,
/// or `None` when that outcome is the answer.
///
/// Two states are ridden out to `deadline`: the daemon's project-open refusal
/// (a JSON-RPC error carrying the warming hint or a saturated open queue) on
/// the client's own cadence, and a completed mounting refusal on the delay
/// that result names. Every other completed result is returned on the first
/// observation.
fn project_open_retry_wait(
    result: &Result<serde_json::Value>,
    deadline: Instant,
) -> Option<Duration> {
    let remaining = DaemonClientDeadline::until(deadline)
        .and_then(|client_deadline| client_deadline.remaining())
        .ok()?;
    match result {
        Err(error) if is_project_open_retryable_error(error) => {
            Some(remaining.min(PROJECT_OPEN_RETRY_INTERVAL))
        }
        Err(_) => None,
        Ok(result) => {
            let delay = tool_result_retry_after_delay(result)?;
            (remaining > delay).then_some(delay)
        }
    }
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
        let result = call_tool_within(
            socket_path,
            handshake,
            tool_name,
            arguments.clone(),
            deadline,
        )
        .await;
        let Some(wait) = project_open_retry_wait(&result, deadline) else {
            return result;
        };
        tokio::time::sleep(wait).await;
    }
}

/// Calls a daemon tool with the shared [`tool_request_deadline`] envelope.
///
/// Production one-shot clients must not read forever against a stalled-but
/// accepting daemon. The request deadline travels on the wire; the local read
/// waits that deadline plus the 30s response grace. A warming project, or an
/// owner still mounting behind its core publication, still retries for at
/// most the 15s open grace, never past this envelope. A completed result that
/// is not that mounting refusal is returned on the first observation.
/// Callers that need a different budget use [`call_default_tool_within`] or
/// [`call_default_tool_awaiting_project_open`].
pub async fn call_default_tool(
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value> {
    let socket_path = default_available_socket_path()?;
    let deadline = Instant::now() + tool_request_deadline()?;
    let result = call_tool_within(
        &socket_path,
        handshake,
        tool_name,
        arguments.clone(),
        deadline,
    )
    .await;
    let retry_deadline = (Instant::now() + PROJECT_OPEN_RETRY_GRACE).min(deadline);
    let Some(wait) = project_open_retry_wait(&result, retry_deadline) else {
        return result;
    };
    tokio::time::sleep(wait).await;
    call_tool_with_project_open_retry(
        &socket_path,
        handshake,
        tool_name,
        arguments,
        retry_deadline,
    )
    .await
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

/// Calls a daemon tool, waiting out a warming project, and the owners that
/// mount behind its core publication, until `deadline`.
///
/// Bootstrap callers deliberately trigger the cold open they are waiting for,
/// so a transport-level warming hint is progress rather than an answer:
/// `tracedecay init` asks for a status it can only get after the open completes.
/// A completed mounting refusal is the same kind of progress and is re-sent
/// until `deadline`. Every other completed result is returned immediately.
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::{
        JsonRpcError, PROJECT_SERVER_CAPACITY_REASON_CODE,
        PROJECT_SERVER_RESPONSE_REVOKED_REASON_CODE, PROJECT_WARMING_REASON_CODE,
        error_is_project_open_retryable, tool_call_transport_error_is_retryable,
    };
    use super::daemon_tool_call_error;

    #[test]
    fn daemon_tool_call_error_round_trips_typed_warming_and_revoked() {
        let warming = daemon_tool_call_error(JsonRpcError {
            code: -32603,
            message: "prose must not decide retry".to_owned(),
            data: Some(json!({
                "reason_code": PROJECT_WARMING_REASON_CODE,
                "retryable": true,
                "detail": "TraceDecay project '/tmp/fixture' is warming",
            })),
        });
        assert_eq!(
            warming.project_route_context(),
            Some((
                PROJECT_WARMING_REASON_CODE,
                true,
                "TraceDecay project '/tmp/fixture' is warming"
            ))
        );
        assert!(tool_call_transport_error_is_retryable(&warming));

        let revoked = daemon_tool_call_error(JsonRpcError {
            code: -32603,
            message: "the retained project server was retired before response completion"
                .to_owned(),
            data: Some(json!({
                "reason_code": PROJECT_SERVER_RESPONSE_REVOKED_REASON_CODE,
                "retryable": true,
                "detail": "the retained project server was retired before response completion",
            })),
        });
        assert_eq!(
            revoked.project_route_context(),
            Some((
                PROJECT_SERVER_RESPONSE_REVOKED_REASON_CODE,
                true,
                "the retained project server was retired before response completion"
            ))
        );
        assert!(tool_call_transport_error_is_retryable(&revoked));
        assert!(
            super::is_project_open_retryable_error(&revoked),
            "the one-shot client rides out a mid-response retirement like a warming open"
        );
        assert!(
            super::project_open_retry_wait(
                &Err(revoked),
                tokio::time::Instant::now() + std::time::Duration::from_secs(5)
            )
            .is_some(),
            "a revoked response is re-sent, not returned"
        );
    }

    #[test]
    fn daemon_tool_call_error_round_trips_the_typed_reset_state() {
        let refused = daemon_tool_call_error(JsonRpcError {
            code: -32603,
            message: "session temporal persisted shape requires reset: published v3 shape"
                .to_owned(),
            data: Some(json!({
                "kind": "reset_required",
                "retryable": false,
                "authority": "session temporal",
                "reason": "published v3 shape",
            })),
        });
        assert_eq!(
            refused.reset_required_context(),
            Some(("session temporal", "published v3 shape"))
        );
        assert!(!error_is_project_open_retryable(&refused));
        assert!(!tool_call_transport_error_is_retryable(&refused));

        let unnamed = daemon_tool_call_error(JsonRpcError {
            code: -32603,
            message: "shape requires reset".to_owned(),
            data: Some(json!({ "kind": "reset_required" })),
        });
        assert!(
            unnamed.reset_required_context().is_none(),
            "a reset state without its authority cannot name a reset command and stays untyped"
        );
    }

    #[test]
    fn daemon_tool_call_error_without_reason_code_stays_untyped() {
        let error = daemon_tool_call_error(JsonRpcError {
            code: -32603,
            message: "project is warming in the background; retry the same tool shortly".to_owned(),
            data: None,
        });
        assert!(error.project_route_context().is_none());
        assert!(!tool_call_transport_error_is_retryable(&error));
        assert!(
            !error_is_project_open_retryable(&error),
            "warming prose without a reason code must not ride project-open retry"
        );
    }

    #[test]
    fn daemon_tool_call_error_round_trips_capacity_without_using_prose() {
        let capacity = daemon_tool_call_error(JsonRpcError {
            code: -32603,
            message: "prose must not decide retry".to_owned(),
            data: Some(json!({
                "reason_code": PROJECT_SERVER_CAPACITY_REASON_CODE,
                "retryable": true,
                "detail": "daemon project server capacity reached (capacity=8); retry after active clients finish",
                "kind": PROJECT_SERVER_CAPACITY_REASON_CODE,
                "capacity": 8,
            })),
        });
        assert!(error_is_project_open_retryable(&capacity));
        assert!(
            !tool_call_transport_error_is_retryable(&capacity),
            "capacity is a project-open retry, not a journey-transport retry"
        );

        let prose_only = daemon_tool_call_error(JsonRpcError {
            code: -32603,
            message: "daemon project server capacity reached (capacity=8); retry after active clients finish"
                .to_owned(),
            data: Some(json!({
                "kind": PROJECT_SERVER_CAPACITY_REASON_CODE,
                "retryable": true,
                "capacity": 8,
            })),
        });
        assert!(!error_is_project_open_retryable(&prose_only));
    }
}
