#[cfg(unix)]
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(unix)]
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::client_identity::DaemonClientIdentity;
use crate::errors::{Result, TraceDecayError};

pub const SERVICE_NAME: &str = "tracedecay.service";
pub const SOCKET_ENV: &str = "TRACEDECAY_DAEMON_SOCKET";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonServiceSpec {
    pub tracedecay_bin: PathBuf,
    pub socket_path: PathBuf,
}

/// Per-connection metadata sent before JSON-RPC traffic.
///
/// The daemon process is shared. This handshake tells that shared process which
/// project, scope, timing preference, and client profile should apply to this
/// connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonHandshake {
    pub project_path: Option<PathBuf>,
    pub scope_prefix: Option<String>,
    pub timings: bool,
    pub allow_init: bool,
    pub client_identity: DaemonClientIdentity,
}

impl DaemonHandshake {
    pub fn for_current_client(
        project_path: Option<PathBuf>,
        scope_prefix: Option<String>,
        timings: bool,
        allow_init: bool,
    ) -> Result<Self> {
        Ok(Self {
            project_path,
            scope_prefix,
            timings,
            allow_init,
            client_identity: DaemonClientIdentity::current()?,
        })
    }

    fn open_options(&self) -> crate::tracedecay::TraceDecayOpenOptions {
        crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(self.client_identity.profile_root.clone()),
            global_db_path: Some(self.client_identity.global_db_path.clone()),
        }
    }

    pub fn to_line(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn from_line(line: &str) -> Result<Self> {
        Ok(serde_json::from_str(line.trim())?)
    }
}

impl DaemonServiceSpec {
    pub fn render_systemd_user_unit(&self) -> String {
        format!(
            "[Unit]\n\
             Description=TraceDecay daemon\n\
             After=network.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart={} daemon run --socket {}\n\
             Restart=on-failure\n\
             RestartSec=2\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            self.tracedecay_bin.display(),
            self.socket_path.display()
        )
    }
}

pub fn default_socket_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(SOCKET_ENV).filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let data_dir = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
    Ok(data_dir.join("daemon.sock"))
}

pub fn socket_path_or_default(socket: Option<String>) -> Result<PathBuf> {
    socket.map_or_else(default_socket_path, |path| Ok(PathBuf::from(path)))
}

pub fn service_spec(
    tracedecay_bin: impl Into<PathBuf>,
    socket: Option<String>,
) -> Result<DaemonServiceSpec> {
    Ok(DaemonServiceSpec {
        tracedecay_bin: tracedecay_bin.into(),
        socket_path: socket_path_or_default(socket)?,
    })
}

pub fn install_service(spec: &DaemonServiceSpec, start: bool) -> Result<PathBuf> {
    let service_path = systemd_user_service_path()?;
    let parent = service_path
        .parent()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("service path '{}' has no parent", service_path.display()),
        })?;
    std::fs::create_dir_all(parent).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to create service directory '{}': {e}",
            parent.display()
        ),
    })?;
    std::fs::write(&service_path, spec.render_systemd_user_unit()).map_err(|e| {
        TraceDecayError::Config {
            message: format!("failed to write service '{}': {e}", service_path.display()),
        }
    })?;

    if start {
        run_systemctl(&["daemon-reload"])?;
        run_systemctl(&["enable", "--now", SERVICE_NAME])?;
    }

    Ok(service_path)
}

pub fn uninstall_service(stop: bool) -> Result<PathBuf> {
    let service_path = systemd_user_service_path()?;
    if stop {
        let _ = run_systemctl(&["disable", "--now", SERVICE_NAME]);
    }
    match std::fs::remove_file(&service_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(TraceDecayError::Config {
                message: format!("failed to remove service '{}': {e}", service_path.display()),
            });
        }
    }
    if stop {
        let _ = run_systemctl(&["daemon-reload"]);
    }
    Ok(service_path)
}

pub fn service_status(socket_path: &Path) -> String {
    let socket_state = if socket_path.exists() {
        "present"
    } else {
        "missing"
    };
    format!(
        "service: {}\nsocket: {} ({})\n",
        systemd_user_service_path().map_or_else(
            |e| format!("unavailable: {e}"),
            |path| path.display().to_string()
        ),
        socket_path.display(),
        socket_state
    )
}

pub fn unavailable_error(socket_path: &Path) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "TraceDecay daemon socket '{}' is not available. Run `tracedecay daemon install-service` and ensure the service is running.",
            socket_path.display()
        ),
    }
}

fn default_available_socket_path() -> Result<PathBuf> {
    let socket_path = default_socket_path()?;
    if socket_path.exists() {
        Ok(socket_path)
    } else {
        Err(unavailable_error(&socket_path))
    }
}

#[cfg(unix)]
pub async fn run_foreground(socket_path: PathBuf) -> Result<()> {
    run_foreground_unix(socket_path).await
}

#[cfg(not(unix))]
pub async fn run_foreground(_socket_path: PathBuf) -> Result<()> {
    Err(unsupported_platform())
}

#[cfg(unix)]
pub async fn proxy_stdio_to_daemon(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    replay_line: Option<String>,
) -> Result<()> {
    use tokio::io::{copy, AsyncWriteExt};
    use tokio::net::UnixStream;

    let stream = UnixStream::connect(socket_path).await?;
    let (mut socket_reader, mut socket_writer) = stream.into_split();

    socket_writer
        .write_all(handshake.to_line()?.as_bytes())
        .await?;
    socket_writer.write_all(b"\n").await?;
    if let Some(line) = replay_line {
        socket_writer.write_all(line.as_bytes()).await?;
        if !line.ends_with('\n') {
            socket_writer.write_all(b"\n").await?;
        }
    }
    socket_writer.flush().await?;

    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let stdin_to_daemon = async {
        copy(&mut stdin, &mut socket_writer).await?;
        socket_writer.shutdown().await
    };
    let daemon_to_stdout = async {
        copy(&mut socket_reader, &mut stdout).await?;
        stdout.flush().await
    };
    tokio::try_join!(stdin_to_daemon, daemon_to_stdout)?;
    Ok(())
}

#[cfg(not(unix))]
pub async fn proxy_stdio_to_daemon(
    _socket_path: &Path,
    _handshake: &DaemonHandshake,
    _replay_line: Option<String>,
) -> Result<()> {
    Err(unsupported_platform())
}

pub async fn proxy_stdio_to_default_daemon(
    handshake: &DaemonHandshake,
    replay_line: Option<String>,
) -> Result<()> {
    let socket_path = default_available_socket_path()?;
    proxy_stdio_to_daemon(&socket_path, handshake, replay_line).await
}

#[cfg(unix)]
pub async fn call_tool(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value> {
    use crate::mcp::{JsonRpcRequest, JsonRpcResponse};
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let stream = UnixStream::connect(socket_path).await?;
    let (reader, mut writer) = stream.into_split();
    let id = json!(1);
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(id.clone()),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": tool_name,
            "arguments": arguments,
        })),
    };

    writer.write_all(handshake.to_line()?.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer
        .write_all(serde_json::to_string(&request)?.as_bytes())
        .await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    writer.shutdown().await?;

    let mut lines = tokio::io::BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        let value: serde_json::Value = serde_json::from_str(&line)?;
        if value.get("id") != Some(&id) {
            continue;
        }
        let response: JsonRpcResponse = serde_json::from_value(value)?;
        if let Some(error) = response.error {
            return Err(TraceDecayError::Config {
                message: format!("daemon tool call failed: {}", error.message),
            });
        }
        return response.result.ok_or_else(|| TraceDecayError::Config {
            message: "daemon tool call response did not include a result".to_string(),
        });
    }

    Err(TraceDecayError::Config {
        message: "daemon closed the connection before returning a tool result".to_string(),
    })
}

#[cfg(not(unix))]
pub async fn call_tool(
    _socket_path: &Path,
    _handshake: &DaemonHandshake,
    _tool_name: &str,
    _arguments: serde_json::Value,
) -> Result<serde_json::Value> {
    Err(unsupported_platform())
}

pub async fn call_default_tool(
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value> {
    let socket_path = default_available_socket_path()?;
    call_tool(&socket_path, handshake, tool_name, arguments).await
}

#[cfg(unix)]
async fn run_foreground_unix(socket_path: PathBuf) -> Result<()> {
    use tokio::net::UnixListener;

    if let Some(parent) = socket_path.parent() {
        let parent_existed = parent.exists();
        std::fs::create_dir_all(parent).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to create socket directory '{}': {e}",
                parent.display()
            ),
        })?;
        if !parent_existed {
            set_owner_only_permissions(parent, 0o700)?;
        }
    }
    prepare_socket_path(&socket_path).await?;

    let listener = UnixListener::bind(&socket_path)?;
    set_owner_only_permissions(&socket_path, 0o600)?;
    eprintln!("[tracedecay] daemon listening on {}", socket_path.display());
    let engine = DaemonEngine::default();
    #[allow(clippy::expect_used)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to register SIGTERM handler");

    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => accepted?.0,
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
        };
        let engine = engine.clone();
        tokio::spawn(async move {
            if let Err(e) = Box::pin(serve_socket_client(stream, engine)).await {
                eprintln!("[tracedecay] daemon client error: {e}");
            }
        });
    }
    engine.shutdown_all().await;
    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let permissions = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, permissions).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to restrict permissions on '{}': {e}",
            path.display()
        ),
    })
}

#[cfg(unix)]
async fn prepare_socket_path(socket_path: &Path) -> Result<()> {
    use tokio::net::UnixStream;

    match UnixStream::connect(socket_path).await {
        Ok(_) => Err(TraceDecayError::Config {
            message: format!(
                "daemon socket '{}' is already in use",
                socket_path.display()
            ),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => std::fs::remove_file(socket_path).map_err(|remove_err| TraceDecayError::Config {
            message: format!(
                "failed to remove stale daemon socket '{}': {remove_err}",
                socket_path.display()
            ),
        }),
    }
}

#[cfg(unix)]
#[derive(Clone, Default)]
struct DaemonEngine {
    /// Shared daemon state, partitioned by the client-scoped project server key.
    project_servers: Arc<tokio::sync::Mutex<HashMap<ProjectServerKey, Arc<crate::mcp::McpServer>>>>,
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ProjectServerKey {
    project_path: PathBuf,
    scope_prefix: Option<String>,
    client_identity: DaemonClientIdentity,
}

#[cfg(unix)]
impl ProjectServerKey {
    fn from_handshake(project_path: PathBuf, handshake: &DaemonHandshake) -> Self {
        Self {
            project_path,
            scope_prefix: handshake.scope_prefix.clone(),
            client_identity: handshake.client_identity.clone(),
        }
    }
}

#[cfg(unix)]
impl DaemonEngine {
    async fn project_server(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Arc<crate::mcp::McpServer>> {
        let Some(project_path) = handshake.project_path.as_ref() else {
            return Err(TraceDecayError::Config {
                message: "project server requested without project_path".to_string(),
            });
        };
        let canonical_project_path = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.clone());
        let key = ProjectServerKey::from_handshake(canonical_project_path.clone(), handshake);

        let mut servers = self.project_servers.lock().await;
        if let Some(server) = servers.get(&key) {
            return Ok(Arc::clone(server));
        }

        let cg = open_project_for_handshake(&canonical_project_path, handshake).await?;
        let accounting_db = accounting_db_for_handshake(handshake).await;
        let registry_db = registry_db_for_handshake(handshake).await;
        let server = crate::mcp::McpServer::new_with_dbs(
            cg,
            handshake.scope_prefix.clone(),
            accounting_db,
            registry_db,
            false,
        )
        .await;
        servers.insert(key, Arc::clone(&server));
        Ok(server)
    }

    async fn shutdown_all(&self) {
        let servers: Vec<Arc<crate::mcp::McpServer>> = {
            let servers = self.project_servers.lock().await;
            servers.values().cloned().collect()
        };
        for server in servers {
            server.shutdown().await;
        }
    }
}

#[cfg(unix)]
async fn serve_socket_client(stream: tokio::net::UnixStream, engine: DaemonEngine) -> Result<()> {
    use crate::mcp::McpTransport;

    let mut transport = UnixStreamTransport::new(stream);
    let Some(line) = transport.read_line().await? else {
        return Ok(());
    };
    let handshake = DaemonHandshake::from_line(&line)?;
    if handshake.project_path.is_some() {
        let server = match Box::pin(engine.project_server(&handshake)).await {
            Ok(server) => server,
            Err(e) => {
                write_project_open_error(&mut transport, &e).await?;
                return Err(e);
            }
        };
        server
            .run_connection_with_timings(&mut transport, handshake.timings)
            .await?;
    } else {
        serve_projectless_client(&mut transport, &handshake.client_identity).await?;
    }
    Ok(())
}

async fn open_project_for_handshake(
    project_path: &Path,
    handshake: &DaemonHandshake,
) -> Result<crate::tracedecay::TraceDecay> {
    let open_options = handshake.open_options();
    if handshake.allow_init
        && !crate::tracedecay::TraceDecay::is_initialized_with_options(project_path, &open_options)
    {
        crate::tracedecay::TraceDecay::init_with_options(project_path, open_options).await
    } else {
        open_existing_project_with_options(project_path, open_options).await
    }
}

async fn open_existing_project_with_options(
    project_path: &Path,
    open_options: crate::tracedecay::TraceDecayOpenOptions,
) -> Result<crate::tracedecay::TraceDecay> {
    if crate::tracedecay::TraceDecay::is_initialized_with_options(project_path, &open_options) {
        return match crate::tracedecay::TraceDecay::open_with_options(
            project_path,
            open_options.clone(),
        )
        .await
        {
            Ok(cg) => Ok(cg),
            Err(open_err) => {
                match crate::tracedecay::TraceDecay::open_read_only_with_options(
                    project_path,
                    open_options,
                )
                .await
                {
                    Ok(cg) => {
                        cg.ensure_schema_current().await?;
                        Ok(cg)
                    }
                    Err(_) => Err(open_err),
                }
            }
        };
    }
    Err(TraceDecayError::Config {
        message: format!(
            "no TraceDecay index found at '{}' — run 'tracedecay init' first",
            project_path.display()
        ),
    })
}

#[cfg(unix)]
async fn accounting_db_for_handshake(
    handshake: &DaemonHandshake,
) -> Option<Arc<crate::global_db::GlobalDb>> {
    if !crate::global_db::global_accounting_enabled() {
        return None;
    }
    crate::global_db::GlobalDb::open_at(&handshake.client_identity.global_db_path)
        .await
        .map(Arc::new)
}

#[cfg(unix)]
async fn registry_db_for_handshake(
    handshake: &DaemonHandshake,
) -> Option<Arc<crate::global_db::GlobalDb>> {
    crate::global_db::GlobalDb::open_at(&handshake.client_identity.global_db_path)
        .await
        .map(Arc::new)
}

#[cfg(unix)]
async fn write_project_open_error(
    transport: &mut UnixStreamTransport,
    error: &TraceDecayError,
) -> Result<()> {
    use crate::mcp::{ErrorCode, JsonRpcResponse};

    let id = read_json_rpc_request_id(transport).await?;
    let response = JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string());
    write_json_rpc_response(transport, &response).await
}

#[cfg(unix)]
async fn read_json_rpc_request_id(
    transport: &mut UnixStreamTransport,
) -> Result<serde_json::Value> {
    use crate::mcp::{JsonRpcRequest, McpTransport};

    let Some(line) = transport.read_line().await? else {
        return Ok(serde_json::Value::Null);
    };

    Ok(serde_json::from_str::<JsonRpcRequest>(&line)
        .ok()
        .and_then(|request| request.id)
        .unwrap_or(serde_json::Value::Null))
}

#[cfg(unix)]
async fn write_json_rpc_response(
    transport: &mut UnixStreamTransport,
    response: &crate::mcp::JsonRpcResponse,
) -> Result<()> {
    use crate::mcp::McpTransport;

    transport
        .write_line(&serde_json::to_string(response)?)
        .await?;
    transport.write_line("\n").await?;
    transport.flush().await?;
    Ok(())
}

#[cfg(unix)]
async fn serve_projectless_client(
    transport: &mut UnixStreamTransport,
    client_identity: &DaemonClientIdentity,
) -> Result<()> {
    use crate::mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse, McpTransport};
    use serde_json::json;

    while let Some(line) = transport.read_line().await? {
        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => projectless_response(&request, client_identity).await,
            Err(e) => Some(JsonRpcResponse::error(
                json!(null),
                ErrorCode::ParseError,
                format!("Parse error: {e}"),
            )),
        };
        if let Some(response) = response {
            write_json_rpc_response(transport, &response).await?;
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn projectless_response(
    request: &crate::mcp::JsonRpcRequest,
    client_identity: &DaemonClientIdentity,
) -> Option<crate::mcp::JsonRpcResponse> {
    use crate::mcp::{ErrorCode, JsonRpcResponse};
    use serde_json::json;

    let id = request.id.clone()?;
    match request.method.as_str() {
        "initialize" => Some(JsonRpcResponse::success(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "tracedecay",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )),
        "tools/call" => Some(
            projectless_tools_call_response(id, request.params.as_ref(), client_identity).await,
        ),
        "ping" | "logging/setLevel" => Some(JsonRpcResponse::success(id, json!({}))),
        _ => Some(JsonRpcResponse::error(
            id,
            ErrorCode::MethodNotFound,
            format!("Method not found: {}", request.method),
        )),
    }
}

#[cfg(unix)]
async fn projectless_tools_call_response(
    id: serde_json::Value,
    params: Option<&serde_json::Value>,
    _client_identity: &DaemonClientIdentity,
) -> crate::mcp::JsonRpcResponse {
    use crate::mcp::{ErrorCode, JsonRpcResponse};

    let (tool_name, arguments) = match projectless_tool_call(params) {
        Ok(tool_call) => tool_call,
        Err(message) => {
            return JsonRpcResponse::error(id, ErrorCode::InvalidParams, message.to_string());
        }
    };

    match crate::mcp::tools::handle_profile_scoped_lcm_tool_call(tool_name, arguments).await {
        Ok(result) => JsonRpcResponse::success(id, result.value),
        Err(e) => JsonRpcResponse::error(id, ErrorCode::InternalError, e.to_string()),
    }
}

#[cfg(unix)]
fn projectless_tool_call(
    params: Option<&serde_json::Value>,
) -> std::result::Result<(&str, serde_json::Value), &'static str> {
    let Some(params) = params else {
        return Err("missing params for tools/call");
    };
    let Some(tool_name) = params.get("name").and_then(|v| v.as_str()) else {
        return Err("missing 'name' in tools/call params");
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    Ok((tool_name, arguments))
}

#[cfg(unix)]
struct UnixStreamTransport {
    reader: tokio::io::Lines<tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

#[cfg(unix)]
impl UnixStreamTransport {
    fn new(stream: tokio::net::UnixStream) -> Self {
        use tokio::io::AsyncBufReadExt;

        let (reader, writer) = stream.into_split();
        Self {
            reader: tokio::io::BufReader::new(reader).lines(),
            writer,
        }
    }
}

#[cfg(unix)]
impl crate::mcp::McpTransport for UnixStreamTransport {
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

fn systemd_user_service_path() -> Result<PathBuf> {
    if cfg!(not(target_os = "linux")) {
        return Err(TraceDecayError::Config {
            message: "daemon service install is currently supported on Linux systemd user services"
                .to_string(),
        });
    }
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .ok_or_else(|| TraceDecayError::Config {
            message: "could not determine XDG config directory".to_string(),
        })?;
    Ok(config_home.join("systemd/user").join(SERVICE_NAME))
}

fn run_systemctl(args: &[&str]) -> Result<()> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to run systemctl --user {}: {e}", args.join(" ")),
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: format!(
            "systemctl --user {} failed with status {}\n{}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ),
    })
}

#[cfg(not(unix))]
fn unsupported_platform() -> TraceDecayError {
    TraceDecayError::Config {
        message: "TraceDecay daemon sockets are currently supported on Unix platforms".to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::{DaemonClientIdentity, DaemonHandshake, DaemonServiceSpec, SOCKET_ENV};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    struct CurrentDirGuard {
        previous: PathBuf,
    }

    impl CurrentDirGuard {
        fn set(path: impl AsRef<std::path::Path>) -> Self {
            let previous = std::env::current_dir().expect("current dir");
            std::env::set_current_dir(path).expect("set current dir");
            Self { previous }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.previous).expect("restore current dir");
        }
    }

    fn test_client_identity() -> DaemonClientIdentity {
        test_client_identity_for(PathBuf::from("/profiles/client"))
    }

    fn test_client_identity_for(profile_root: PathBuf) -> DaemonClientIdentity {
        DaemonClientIdentity {
            global_db_path: profile_root.join("global.db"),
            profile_root,
        }
    }

    #[test]
    fn user_service_runs_daemon_with_socket_path() {
        let spec = DaemonServiceSpec {
            tracedecay_bin: PathBuf::from("/usr/local/bin/tracedecay"),
            socket_path: PathBuf::from("/tmp/tracedecay.sock"),
        };

        let unit = spec.render_systemd_user_unit();

        assert!(unit.contains(
            "ExecStart=/usr/local/bin/tracedecay daemon run --socket /tmp/tracedecay.sock"
        ));
        assert!(unit.contains("Restart=on-failure"));
    }

    #[test]
    fn default_socket_path_is_profile_scoped_not_project_scoped() {
        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let profile = tempfile::TempDir::new().expect("profile temp dir");
        let project_a = tempfile::TempDir::new().expect("project a temp dir");
        let project_b = tempfile::TempDir::new().expect("project b temp dir");
        let override_socket = profile.path().join("override.sock");
        let _socket_guard = EnvVarGuard::unset(SOCKET_ENV);
        let _data_dir_guard = EnvVarGuard::set(
            crate::config::USER_DATA_DIR_ENV,
            profile.path().join(".tracedecay"),
        );

        {
            let _cwd_guard = CurrentDirGuard::set(project_a.path());
            assert_eq!(
                super::default_socket_path().expect("default socket path"),
                profile.path().join(".tracedecay/daemon.sock")
            );
        }
        {
            let _cwd_guard = CurrentDirGuard::set(project_b.path());
            assert_eq!(
                super::default_socket_path().expect("default socket path"),
                profile.path().join(".tracedecay/daemon.sock")
            );
        }

        let _override_guard = EnvVarGuard::set(SOCKET_ENV, &override_socket);
        assert_eq!(
            super::default_socket_path().expect("override socket path"),
            override_socket
        );
    }

    #[test]
    fn daemon_handshake_round_trips_project_scope_and_timings() {
        let handshake = DaemonHandshake {
            project_path: Some(PathBuf::from("/work/repo")),
            scope_prefix: Some("src/mcp".to_string()),
            timings: true,
            allow_init: true,
            client_identity: test_client_identity(),
        };

        let encoded = handshake.to_line().expect("handshake should encode");
        let decoded = DaemonHandshake::from_line(&encoded).expect("handshake should decode");

        assert_eq!(decoded, handshake);
    }

    #[test]
    fn daemon_handshake_requires_client_identity() {
        let encoded = serde_json::json!({
            "project_path": "/work/repo",
            "scope_prefix": null,
            "timings": false,
            "allow_init": false
        })
        .to_string();

        assert!(DaemonHandshake::from_line(&encoded).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_client_serves_initialize_after_handshake() {
        use serde_json::{json, Value};
        use tempfile::TempDir;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let dir = TempDir::new().expect("temp dir");
        let project = dir.path();
        let client_identity = test_client_identity_for(dir.path().join("profile"));
        std::fs::create_dir_all(project.join("src")).expect("src dir");
        std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");

        let (client, server) = tokio::net::UnixStream::pair().expect("unix stream pair");
        let server_task = tokio::spawn(super::serve_socket_client(
            server,
            super::DaemonEngine::default(),
        ));

        let (reader, mut writer) = client.into_split();
        let handshake = DaemonHandshake {
            project_path: Some(project.to_path_buf()),
            scope_prefix: None,
            timings: false,
            allow_init: true,
            client_identity,
        };
        writer
            .write_all(handshake.to_line().expect("handshake").as_bytes())
            .await
            .expect("write handshake");
        writer.write_all(b"\n").await.expect("newline");
        writer
            .write_all(
                serde_json::to_string(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {}
                }))
                .expect("initialize json")
                .as_bytes(),
            )
            .await
            .expect("write initialize");
        writer.write_all(b"\n").await.expect("newline");
        writer.shutdown().await.expect("shutdown writer");

        let mut lines = tokio::io::BufReader::new(reader).lines();
        let line = lines
            .next_line()
            .await
            .expect("read response")
            .expect("initialize response");
        let response: Value = serde_json::from_str(&line).expect("response json");
        assert_eq!(response["id"], json!(1));
        assert_eq!(response["result"]["protocolVersion"], "2024-11-05");

        server_task
            .await
            .expect("server task")
            .expect("server result");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_client_serves_profile_scoped_lcm_without_project() {
        use serde_json::{json, Value};
        use tempfile::TempDir;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let hermes_home = TempDir::new().expect("hermes home");
        let hermes_home = hermes_home
            .path()
            .canonicalize()
            .expect("canonical hermes home");
        let client_identity = test_client_identity_for(hermes_home.join("client-profile"));
        std::fs::create_dir_all(hermes_home.join(".tracedecay")).expect("profile root");

        let (client, server) = tokio::net::UnixStream::pair().expect("unix stream pair");
        let server_task = tokio::spawn(super::serve_socket_client(
            server,
            super::DaemonEngine::default(),
        ));

        let (reader, mut writer) = client.into_split();
        let handshake = DaemonHandshake {
            project_path: None,
            scope_prefix: None,
            timings: false,
            allow_init: false,
            client_identity,
        };
        writer
            .write_all(handshake.to_line().expect("handshake").as_bytes())
            .await
            .expect("write handshake");
        writer.write_all(b"\n").await.expect("newline");
        writer
            .write_all(
                serde_json::to_string(&json!({
                    "jsonrpc": "2.0",
                    "id": 7,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_lcm_status",
                        "arguments": {
                            "provider": "cursor",
                            "storage_scope": "hermes_profile",
                            "hermes_home": hermes_home
                        }
                    }
                }))
                .expect("tools/call json")
                .as_bytes(),
            )
            .await
            .expect("write tools/call");
        writer.write_all(b"\n").await.expect("newline");
        writer.shutdown().await.expect("shutdown writer");

        let mut lines = tokio::io::BufReader::new(reader).lines();
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect("profile-scoped response should not time out")
            .expect("read response")
            .expect("profile-scoped response");
        let response: Value = serde_json::from_str(&line).expect("response json");
        assert_eq!(response["id"], json!(7));
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("profile result text");
        let payload: Value = serde_json::from_str(text).expect("profile payload json");
        assert_eq!(payload["status"], "not_ingested");
        assert_eq!(payload["storage_scope"], "hermes_profile");

        server_task
            .await
            .expect("server task")
            .expect("server result");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_client_timings_are_connection_local() {
        use serde_json::{json, Value};
        use tempfile::TempDir;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let dir = TempDir::new().expect("temp dir");
        let project = dir.path();
        let client_identity = test_client_identity_for(dir.path().join("profile"));
        std::fs::create_dir_all(project.join("src")).expect("src dir");
        std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
        crate::tracedecay::TraceDecay::init_with_options(
            project,
            crate::tracedecay::TraceDecayOpenOptions {
                profile_root: Some(client_identity.profile_root.clone()),
                global_db_path: Some(client_identity.global_db_path.clone()),
            },
        )
        .await
        .expect("project init");

        let engine = super::DaemonEngine::default();
        let (client_a, server_a) = tokio::net::UnixStream::pair().expect("unix stream pair");
        let server_a_task = tokio::spawn(super::serve_socket_client(server_a, engine.clone()));
        let (reader_a, mut writer_a) = client_a.into_split();
        let mut lines_a = tokio::io::BufReader::new(reader_a).lines();

        let handshake_a = DaemonHandshake {
            project_path: Some(project.to_path_buf()),
            scope_prefix: None,
            timings: false,
            allow_init: false,
            client_identity: client_identity.clone(),
        };
        writer_a
            .write_all(handshake_a.to_line().expect("handshake").as_bytes())
            .await
            .expect("write handshake A");
        writer_a.write_all(b"\n").await.expect("newline A");
        writer_a
            .write_all(
                serde_json::to_string(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {}
                }))
                .expect("initialize json")
                .as_bytes(),
            )
            .await
            .expect("write initialize A");
        writer_a.write_all(b"\n").await.expect("newline A");
        let line = lines_a
            .next_line()
            .await
            .expect("read initialize A")
            .expect("initialize A response");
        let response: Value = serde_json::from_str(&line).expect("initialize A response json");
        assert_eq!(response["id"], json!(1));

        let (client_b, server_b) = tokio::net::UnixStream::pair().expect("unix stream pair");
        let server_b_task = tokio::spawn(super::serve_socket_client(server_b, engine));
        let (reader_b, mut writer_b) = client_b.into_split();
        let mut lines_b = tokio::io::BufReader::new(reader_b).lines();
        let handshake_b = DaemonHandshake {
            project_path: Some(project.to_path_buf()),
            scope_prefix: None,
            timings: true,
            allow_init: false,
            client_identity,
        };
        writer_b
            .write_all(handshake_b.to_line().expect("handshake").as_bytes())
            .await
            .expect("write handshake B");
        writer_b.write_all(b"\n").await.expect("newline B");
        writer_b
            .write_all(
                serde_json::to_string(&json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "initialize",
                    "params": {}
                }))
                .expect("initialize json")
                .as_bytes(),
            )
            .await
            .expect("write initialize B");
        writer_b.write_all(b"\n").await.expect("newline B");
        let line = lines_b
            .next_line()
            .await
            .expect("read initialize B")
            .expect("initialize B response");
        let response: Value = serde_json::from_str(&line).expect("initialize B response json");
        assert_eq!(response["id"], json!(2));

        writer_a
            .write_all(
                serde_json::to_string(&json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/call",
                    "params": {
                        "name": "tracedecay_status",
                        "arguments": { "format": "json" }
                    }
                }))
                .expect("tools/call json")
                .as_bytes(),
            )
            .await
            .expect("write tools/call A");
        writer_a.write_all(b"\n").await.expect("newline A");
        writer_a.shutdown().await.expect("shutdown A writer");
        writer_b.shutdown().await.expect("shutdown B writer");

        let line = lines_a
            .next_line()
            .await
            .expect("read status A")
            .expect("status A response");
        let response: Value = serde_json::from_str(&line).expect("status A response json");
        assert_eq!(response["id"], json!(3));
        assert!(
            response["result"].get("_meta").is_none(),
            "client A disabled timings, but response included metadata: {response}"
        );

        server_a_task
            .await
            .expect("server A task")
            .expect("server A result");
        server_b_task
            .await
            .expect("server B task")
            .expect("server B result");
    }
}
