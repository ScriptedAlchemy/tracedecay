//! Stdio MCP proxy: forwards host traffic to the daemon over the broker
//! transport, tracking initialize-route and tool-catalog metadata.

#[cfg(unix)]
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::time::{Duration, Instant};

use super::{
    DAEMON_TOOL_LIVENESS_POLL_INTERVAL, DaemonClientDeadline, DaemonHandshake,
    PROJECT_OPEN_RETRY_GRACE, PROJECT_OPEN_RETRY_INTERVAL, connect_to_current_daemon_within,
    json_rpc_error_is_project_open_retryable, next_daemon_response_line, write_daemon_preamble,
};
#[cfg(unix)]
use super::{
    binary_version, connect_with_restart_grace, connection_for_socket_path, log_daemon_event,
    version_skew_action,
};
use crate::errors::{Result, TraceDecayError};
#[cfg(not(unix))]
use crate::mcp::McpTransport;
#[cfg(unix)]
use crate::mcp::transport::{McpDuplexTransport, McpTransportReader, McpTransportWriter};
#[cfg(unix)]
use crate::mcp::{ErrorCode, JsonRpcResponse};
use crate::mcp::{JsonRpcRequest, StdioTransport};

/// Decides at `tracedecay serve` startup whether to proxy to the daemon.
///
/// A missing socket usually means "no daemon", but `tracedecay update`
/// restarts the daemon service and shutdown unlinks the socket before the new
/// daemon rebinds it; a serve process starting inside that window would
/// otherwise silently commit to in-process mode for its whole lifetime. When
/// a daemon service is installed for this socket, wait out that window with
/// the same grace used for per-request connects before falling back.
#[cfg(unix)]
pub async fn should_proxy_serve_to_daemon(socket_path: &Path) -> bool {
    let installed_socket = super::installed_service_socket_path().ok().flatten();
    should_proxy_serve_to_daemon_with(
        socket_path,
        installed_socket.as_deref(),
        super::DAEMON_RESTART_GRACE,
        super::DAEMON_RESTART_POLL_INTERVAL,
    )
    .await
}

#[cfg(any(test, not(unix)))]
pub(crate) fn proxy_required_by_platform(transport_supported: bool, endpoint_exists: bool) -> bool {
    !transport_supported || endpoint_exists
}

/// Non-Unix clients always use the authenticated loopback broker. There is no
/// in-process `SQLite` fallback.
#[cfg(not(unix))]
#[allow(clippy::unused_async)] // Preserve parity with the Unix async routing probe.
pub async fn should_proxy_serve_to_daemon(socket_path: &Path) -> bool {
    proxy_required_by_platform(false, socket_path.exists())
}

#[cfg(unix)]
pub async fn proxy_stdio_to_daemon(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    replay_line: Option<String>,
) -> Result<()> {
    let mut transport = StdioTransport::new();
    proxy_transport_to_daemon(socket_path, handshake, replay_line, &mut transport).await
}

#[cfg(not(unix))]
pub async fn proxy_stdio_to_daemon(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    replay_line: Option<String>,
) -> Result<()> {
    let mut transport = StdioTransport::new();
    if let Some(line) = replay_line {
        proxy_one_request(socket_path, handshake, &line, &mut transport).await?;
    }
    while let Some(line) = transport.read_line().await? {
        proxy_one_request(socket_path, handshake, &line, &mut transport).await?;
    }
    Ok(())
}

#[cfg(unix)]
#[derive(Default)]
pub(crate) struct ProxyInitializeMetadata {
    daemon_version: Option<String>,
    tool_list_changed: bool,
    route: Option<InitializeRouteMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InitializeRouteMetadata {
    pub(super) project_path: PathBuf,
    pub(super) allow_init: bool,
}

#[cfg(unix)]
pub(crate) async fn should_proxy_serve_to_daemon_with(
    socket_path: &Path,
    installed_service_socket: Option<&Path>,
    grace: Duration,
    poll_interval: Duration,
) -> bool {
    if socket_path.exists() {
        return true;
    }
    // Only wait when an installed service is expected to rebind this exact
    // socket; otherwise in-process startup must stay instant.
    if installed_service_socket != Some(socket_path) {
        return false;
    }
    let connection = connection_for_socket_path(socket_path);
    connect_with_restart_grace(&connection, grace, poll_interval)
        .await
        .is_ok()
}

#[cfg(unix)]
pub async fn proxy_transport_to_daemon(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    replay_line: Option<String>,
    transport: &mut impl McpDuplexTransport,
) -> Result<()> {
    let (mut reader, mut writer) = transport.split();
    let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel();
    let (eof_tx, mut eof_rx) = tokio::sync::watch::channel(false);

    let read_host = async {
        loop {
            match reader.read_line().await {
                Ok(Some(line)) => {
                    if input_tx.send(line).is_err() {
                        return Ok(());
                    }
                }
                Ok(None) => {
                    let _ = eof_tx.send(true);
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            }
        }
    };
    let proxy = proxy_host_input_to_daemon(
        socket_path,
        handshake,
        replay_line,
        &mut input_rx,
        &mut eof_rx,
        &mut writer,
    );
    tokio::try_join!(read_host, proxy)?;
    Ok(())
}

#[cfg(unix)]
async fn proxy_host_input_to_daemon(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    replay_line: Option<String>,
    input: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    eof: &mut tokio::sync::watch::Receiver<bool>,
    writer: &mut impl McpTransportWriter,
) -> Result<()> {
    let mut routed_handshake = handshake.clone();
    let mut pending = VecDeque::new();
    if let Some(line) = replay_line {
        pending.push_back(line);
    }

    loop {
        while let Ok(line) = input.try_recv() {
            pending.push_back(line);
        }
        if *eof.borrow() && pending.is_empty() {
            return Ok(());
        }
        let line = match pending.pop_front() {
            Some(line) => line,
            None => {
                tokio::select! {
                    changed = eof.changed() => {
                        changed.map_err(|error| TraceDecayError::Config {
                            message: format!("host EOF monitor closed unexpectedly: {error}"),
                        })?;
                        if *eof.borrow() {
                            while let Ok(line) = input.try_recv() {
                                pending.push_back(line);
                            }
                            if pending.is_empty() {
                                return Ok(());
                            }
                        }
                        continue;
                    }
                    line = input.recv() => {
                        let Some(line) = line else {
                            return Ok(());
                        };
                        line
                    }
                }
            }
        };
        reset_proxy_handshake_for_initialize(handshake, &mut routed_handshake, &line);
        if line.trim().is_empty() {
            continue;
        }

        let result = {
            let daemon_request = send_daemon_request_line_with_project_open_retry(
                socket_path,
                &routed_handshake,
                &line,
            );
            tokio::pin!(daemon_request);
            loop {
                if *eof.borrow() && !pending.is_empty() {
                    break daemon_request.await;
                }
                tokio::select! {
                    result = &mut daemon_request => break result,
                    changed = eof.changed() => {
                        changed.map_err(|error| TraceDecayError::Config {
                            message: format!("host EOF monitor closed unexpectedly: {error}"),
                        })?;
                        if *eof.borrow() {
                            while let Ok(line) = input.try_recv() {
                                pending.push_back(line);
                            }
                            if pending.is_empty() {
                                return Ok(());
                            }
                        }
                    }
                    next = input.recv() => {
                        let Some(next) = next else {
                            return Ok(());
                        };
                        pending.push_back(next);
                    }
                }
            }
        };
        let metadata = write_proxy_request_result(&line, result, writer).await?;
        apply_proxy_initialize_metadata(&mut routed_handshake, metadata);
    }
}

#[cfg(unix)]
pub(crate) fn apply_proxy_initialize_metadata(
    handshake: &mut DaemonHandshake,
    metadata: ProxyInitializeMetadata,
) {
    if let Some(route) = metadata.route {
        if handshake.project_path.as_deref() != Some(route.project_path.as_path()) {
            handshake.scope_prefix = None;
        }
        handshake.project_path = Some(route.project_path);
        handshake.allow_init = route.allow_init;
    }
    if metadata.tool_list_changed {
        handshake.tool_list_changed_capable = true;
        if let Some(version) = metadata.daemon_version {
            handshake.catalog_version = version;
        }
    }
}

#[cfg(unix)]
pub(crate) fn reset_proxy_handshake_for_initialize(
    base_handshake: &DaemonHandshake,
    handshake: &mut DaemonHandshake,
    line: &str,
) {
    let Ok(request) = serde_json::from_str::<JsonRpcRequest>(line.trim()) else {
        return;
    };
    if request.method != "initialize" {
        return;
    }
    *handshake = base_handshake.clone();
}

pub(crate) async fn resolve_daemon_initialize_route(
    params: Option<&serde_json::Value>,
    registry: Option<&crate::global_db::RegisteredGlobalDb>,
) -> crate::errors::Result<Option<InitializeRouteMetadata>> {
    let roots = crate::mcp::server::initialize_root_paths(params);
    if let Some(registry) = registry {
        for root in &roots {
            let mut candidate = root.canonicalize().unwrap_or_else(|_| root.clone());
            loop {
                if registry
                    .project_registry_context_by_alias(&candidate)
                    .await?
                    .is_some()
                {
                    return Ok(Some(InitializeRouteMetadata {
                        project_path: candidate,
                        allow_init: false,
                    }));
                }
                if !candidate.pop() {
                    break;
                }
            }
            if let Some(git_root) = crate::worktree::git_worktree_root(root) {
                let git_common_dir = crate::worktree::git_common_dir(&git_root);
                if registry
                    .project_registry_context_by_identity(&git_root, git_common_dir.as_deref())
                    .await?
                    .is_some()
                {
                    return Ok(Some(InitializeRouteMetadata {
                        project_path: git_root,
                        allow_init: false,
                    }));
                }
            }
        }
    }
    if let Some(project_path) =
        crate::mcp::server::resolve_initialize_roots_project_path(params, registry).await
    {
        return Ok(Some(InitializeRouteMetadata {
            project_path,
            allow_init: false,
        }));
    }

    for root in roots {
        if let Some(project_path) = crate::config::discover_project_root(&root) {
            return Ok(Some(InitializeRouteMetadata {
                project_path,
                allow_init: false,
            }));
        }
        if let Some(git_root) = crate::worktree::git_worktree_root(&root) {
            // An initialize route has no retained configuration authority.
            // Never revive legacy-file fallback here — but a fresh repo with
            // no published snapshot follows the schema default (auto-init
            // enabled), not fail-closed: treating a missing snapshot as
            // "disabled" contradicted the config default and left explicit
            // initialize-roots repos unable to open at all.
            let allow_init = crate::config::cached_sync_config(&git_root).map_or_else(
                |_| crate::config::SyncConfig::default().auto_init,
                |config| config.auto_init,
            );
            return Ok(Some(InitializeRouteMetadata {
                project_path: git_root,
                allow_init,
            }));
        }
    }
    Ok(None)
}

#[cfg(unix)]
async fn write_proxy_request_result(
    line: &str,
    result: Result<Vec<String>>,
    writer: &mut impl McpTransportWriter,
) -> Result<ProxyInitializeMetadata> {
    match result {
        Ok(responses) => {
            let metadata = proxy_initialize_metadata(line, &responses);
            if let Some(warning) = daemon_version_skew_warning(line, &responses, binary_version()) {
                eprintln!("[tracedecay] warning: {warning}");
            }
            for response in responses {
                writer.write_line(&response).await?;
                if !response.ends_with('\n') {
                    writer.write_line("\n").await?;
                }
            }
            writer.flush().await?;
            Ok(metadata)
        }
        Err(err) => {
            if let Some(response) = daemon_proxy_error_response(line, &err) {
                let json_line = serde_json::to_string(&response)?;
                writer.write_line(&json_line).await?;
                writer.write_line("\n").await?;
                writer.flush().await?;
            } else {
                log_daemon_event(
                    "daemon_proxy_drop",
                    &[
                        ("outcome", "dropped_notification".to_string()),
                        ("error", err.to_string()),
                    ],
                );
            }
            Ok(ProxyInitializeMetadata::default())
        }
    }
}

pub(crate) async fn send_daemon_request_line(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    line: &str,
) -> Result<Vec<String>> {
    send_daemon_request_line_with_liveness_poll(
        socket_path,
        handshake,
        line,
        DAEMON_TOOL_LIVENESS_POLL_INTERVAL,
        None,
    )
    .await
}

fn responses_are_project_open_retryable(responses: &[String]) -> bool {
    responses.len() == 1
        && serde_json::from_str::<serde_json::Value>(&responses[0])
            .ok()
            .and_then(|response| response.get("error").cloned())
            .as_ref()
            .is_some_and(json_rpc_error_is_project_open_retryable)
}

async fn send_daemon_request_line_with_project_open_retry(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    line: &str,
) -> Result<Vec<String>> {
    let deadline = Instant::now() + PROJECT_OPEN_RETRY_GRACE;
    let mut responses = send_daemon_request_line(socket_path, handshake, line).await?;
    while responses_are_project_open_retryable(&responses) {
        let Some(remaining) = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
        else {
            break;
        };
        tokio::time::sleep(remaining.min(PROJECT_OPEN_RETRY_INTERVAL)).await;
        responses = send_daemon_request_line(socket_path, handshake, line).await?;
    }
    Ok(responses)
}

pub(crate) async fn send_daemon_request_line_with_liveness_poll(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    line: &str,
    liveness_poll_interval: Duration,
    client_deadline: Option<DaemonClientDeadline>,
) -> Result<Vec<String>> {
    let request = serde_json::from_str::<JsonRpcRequest>(line).ok();
    let request_id = request.as_ref().and_then(|request| request.id.clone());
    let request_label = request
        .as_ref()
        .map_or("daemon request", |request| request.method.as_str())
        .to_string();
    let (connection, stream) = match client_deadline {
        Some(deadline) => {
            deadline
                .run("connect", &request_label, async {
                    connect_to_current_daemon_within(socket_path, Some(deadline)).await
                })
                .await?
        }
        None => connect_to_current_daemon_within(socket_path, None).await?,
    };
    let (reader, mut writer) = stream.into_split();

    let write = async {
        write_daemon_preamble(&mut writer, &connection, handshake).await?;
        writer.write_all(line.as_bytes()).await?;
        if !line.ends_with('\n') {
            writer.write_all(b"\n").await?;
        }
        writer.flush().await?;
        Ok(())
    };
    match client_deadline {
        Some(deadline) => deadline.run("write", &request_label, write).await?,
        None => write.await?,
    }

    let mut reader = tokio::io::BufReader::new(reader);
    let mut responses = Vec::new();
    let mut matched_response = request_id.is_none();
    loop {
        let read = next_daemon_response_line(
            &mut reader,
            &connection,
            &request_label,
            liveness_poll_interval,
        );
        let response_line = match client_deadline {
            Some(deadline) => deadline.run("read", &request_label, read).await?,
            None => read.await?,
        };
        let Some(response_line) = response_line else {
            break;
        };
        if response_line.trim().is_empty() {
            continue;
        }
        let is_matching_response = match client_deadline {
            Some(deadline) => {
                deadline
                    .run("decode", &request_label, async {
                        Ok(request_id.as_ref().is_some_and(|id| {
                            serde_json::from_str::<serde_json::Value>(&response_line)
                                .ok()
                                .and_then(|value| value.get("id").cloned())
                                .as_ref()
                                == Some(id)
                        }))
                    })
                    .await?
            }
            None => request_id.as_ref().is_some_and(|id| {
                serde_json::from_str::<serde_json::Value>(&response_line)
                    .ok()
                    .and_then(|value| value.get("id").cloned())
                    .as_ref()
                    == Some(id)
            }),
        };
        responses.push(format!("{response_line}\n"));
        if is_matching_response {
            matched_response = true;
            break;
        }
    }
    if !matched_response {
        return Err(TraceDecayError::Config {
            message: "daemon closed the connection after the request was sent but before returning a matching response; the outcome is unknown and the request was not retried"
                .to_string(),
        });
    }
    Ok(responses)
}

/// Extracts the daemon's advertised version from a proxied `initialize`
/// response (`result.serverInfo.version`, which daemons have always sent).
///
/// This works against daemons older than the handshake version field, so a
/// freshly-updated client can still detect a stale daemon left running by a
/// non-systemd setup or a plain `tracedecay upgrade`.
#[cfg(unix)]
pub(crate) fn proxy_initialize_metadata(
    request_line: &str,
    responses: &[String],
) -> ProxyInitializeMetadata {
    let Ok(request) = serde_json::from_str::<JsonRpcRequest>(request_line) else {
        return ProxyInitializeMetadata::default();
    };
    if request.method != "initialize" {
        return ProxyInitializeMetadata::default();
    }
    let mut metadata = ProxyInitializeMetadata::default();
    for line in responses {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if metadata.daemon_version.is_none() {
            metadata.daemon_version = value
                .pointer("/result/serverInfo/version")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
        }
        metadata.tool_list_changed |= value
            .pointer("/result/capabilities/tools/listChanged")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if metadata.route.is_none() {
            metadata.route = value
                .pointer("/result/_meta/tracedecayInitializeRoute")
                .cloned()
                .and_then(|route| serde_json::from_value(route).ok());
        }
    }
    metadata
}

#[cfg(unix)]
fn daemon_version_from_initialize_response(
    request_line: &str,
    responses: &[String],
) -> Option<String> {
    proxy_initialize_metadata(request_line, responses).daemon_version
}

/// The warning to surface when the daemon behind an `initialize` response is
/// running a different binary version than this client.
#[cfg(unix)]
pub(crate) fn daemon_version_skew_warning(
    request_line: &str,
    responses: &[String],
    client_version: &str,
) -> Option<String> {
    let daemon_version = daemon_version_from_initialize_response(request_line, responses)?;
    if daemon_version == client_version {
        return None;
    }
    let action = version_skew_action(&daemon_version, client_version);
    Some(format!(
        "TraceDecay daemon is version {daemon_version} but this client is {client_version} — \
         {action}"
    ))
}

#[cfg(unix)]
fn daemon_proxy_error_response(line: &str, err: &TraceDecayError) -> Option<JsonRpcResponse> {
    let request = serde_json::from_str::<JsonRpcRequest>(line).ok()?;
    request.id.map(|id| {
        JsonRpcResponse::error(
            id,
            ErrorCode::InternalError,
            format!("TraceDecay daemon connection failed: {err}"),
        )
    })
}

#[cfg(not(unix))]
async fn proxy_one_request(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    line: &str,
    transport: &mut impl McpTransport,
) -> Result<()> {
    if line.trim().is_empty() {
        return Ok(());
    }
    for response in
        send_daemon_request_line_with_project_open_retry(socket_path, handshake, line).await?
    {
        transport.write_line(&response).await?;
        if !response.ends_with('\n') {
            transport.write_line("\n").await?;
        }
    }
    transport.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::responses_are_project_open_retryable;

    #[test]
    fn dogfood_recovery_project_open_retry_accepts_warming_and_capacity_errors() {
        let retryable = [
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {
                    "code": -32603,
                    "message": "project is warming in the background; retry the same tool shortly"
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "error": {
                    "code": -32603,
                    "message": "daemon project open task capacity reached",
                    "data": {
                        "kind": "project_open_task_capacity_reached",
                        "retryable": true,
                        "capacity": 8
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "error": {
                    "code": -32603,
                    "message": "daemon project server capacity reached",
                    "data": {
                        "kind": "project_server_capacity_reached",
                        "retryable": true,
                        "capacity": 8
                    }
                }
            }),
        ];

        for response in retryable {
            assert!(responses_are_project_open_retryable(
                &[response.to_string()]
            ));
        }
        assert!(!responses_are_project_open_retryable(&[json!({
            "jsonrpc": "2.0",
            "id": 4,
            "error": {
                "code": -32603,
                "message": "permanent project open failure",
                "data": {
                    "kind": "project_open_failed",
                    "retryable": false
                }
            }
        })
        .to_string()]));
    }
}
