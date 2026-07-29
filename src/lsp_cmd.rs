use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::time::{Duration, interval};
use tracedecay::daemon::DaemonHandshake;
use tracedecay::daemon_client::{DaemonInvocationClient, DaemonLspSessionClient};
use tracedecay::diagnostics::lsp::{adapters as lsp_adapters, broker as lsp_broker};
use tracedecay_lsp::{ContentLengthCodec, FramePoll, FrameSend};

use crate::cli::LspAction;

pub(crate) async fn handle_lsp_action(action: LspAction) -> tracedecay::errors::Result<()> {
    match action {
        LspAction::Servers { json } => print_lsp_servers(json)?,
        LspAction::Bridge { stdio, project } => {
            if !stdio {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: "lsp bridge requires --stdio".to_owned(),
                });
            }
            run_stdio_bridge(project.map(PathBuf::from)).await?;
        }
    }
    Ok(())
}

/// Runs a strict `Content-Length` bridge over one daemon-owned LSP session.
///
/// The loop only forwards bounded LSP frames through explicit session
/// operations. When `--project` is omitted it parses exactly the first
/// `initialize` frame to bind one canonical local workspace root; it never
/// opens a project store, starts an analyzer, or connects the host to an
/// arbitrary daemon socket.
async fn run_stdio_bridge(project_root: Option<PathBuf>) -> tracedecay::errors::Result<()> {
    let mut stdin = tokio::io::stdin();
    let mut codec = ContentLengthCodec::new();
    let mut read_buffer = [0_u8; 8 * 1024];
    let initialize = if project_root.is_none() {
        Some(read_initialize_binding(&mut stdin, &mut codec, &mut read_buffer).await?)
    } else {
        None
    };
    let project_root = project_root
        .or_else(|| {
            initialize
                .as_ref()
                .map(|binding| binding.project_root.clone())
        })
        .ok_or_else(|| bridge_config_error("LSP initialize did not identify a workspace root"))?;
    let handshake = DaemonHandshake::for_current_client(Some(project_root), None, false, false)?;
    let invocation = DaemonInvocationClient::for_current(handshake)?;
    let mut session = DaemonLspSessionClient::open(
        invocation,
        env!("CARGO_PKG_VERSION"),
        initialize
            .as_ref()
            .map(|binding| binding.canonical_root_uri.clone()),
        initialize
            .as_ref()
            .map(|binding| binding.workspace_folders.clone())
            .unwrap_or_default(),
    )
    .await?;
    let mut stdout = tokio::io::stdout();
    let mut pending_client_frame = initialize.map(|binding| binding.frame);
    let mut poll_timer = interval(Duration::from_millis(25));

    loop {
        // Keep each pump fair: a queued daemon burst must not starve host input.
        if flush_daemon_frame(&mut session, &mut stdout).await? {
            return Ok(());
        }

        if pending_client_frame.is_none()
            && let Some(frame) = codec
                .next_frame()
                .map_err(|error| bridge_error("decode", error))?
        {
            let frame = String::from_utf8(frame).map_err(|_| {
                tracedecay::errors::TraceDecayError::Config {
                    message: "LSP bridge received a non-UTF-8 JSON-RPC payload".to_owned(),
                }
            })?;
            pending_client_frame = Some(frame);
        }

        if let Some(frame) = pending_client_frame.as_deref() {
            match send_client_frame_with_reconnect(&mut session, frame).await? {
                FrameSend::Sent => {
                    pending_client_frame = None;
                    continue;
                }
                FrameSend::Backpressured => {}
                FrameSend::Closed => return Ok(()),
            }
        }

        tokio::select! {
            read = stdin.read(&mut read_buffer), if pending_client_frame.is_none() => {
                let read = read?;
                if read == 0 {
                    let _ = session.detach().await;
                    codec
                        .finish()
                        .map_err(|error| bridge_error("decode", error))?;
                    return Ok(());
                }
                codec.push(&read_buffer[..read]);
            }
            _ = poll_timer.tick() => {}
        }
    }
}

struct InitializeBinding {
    project_root: PathBuf,
    canonical_root_uri: String,
    workspace_folders: Vec<String>,
    frame: String,
}

async fn read_initialize_binding<R: AsyncRead + Unpin>(
    reader: &mut R,
    codec: &mut ContentLengthCodec,
    read_buffer: &mut [u8],
) -> tracedecay::errors::Result<InitializeBinding> {
    loop {
        if let Some(frame) = codec
            .next_frame()
            .map_err(|error| bridge_error("decode", error))?
        {
            let frame = String::from_utf8(frame).map_err(|_| {
                bridge_config_error("LSP bridge received a non-UTF-8 JSON-RPC payload")
            })?;
            return initialize_binding(&frame);
        }
        let read = reader.read(read_buffer).await?;
        if read == 0 {
            codec
                .finish()
                .map_err(|error| bridge_error("decode", error))?;
            return Err(bridge_config_error(
                "lsp bridge without --project requires initialize as its first frame",
            ));
        }
        codec.push(&read_buffer[..read]);
    }
}

fn initialize_binding(frame: &str) -> tracedecay::errors::Result<InitializeBinding> {
    let mut request: Value = serde_json::from_str(frame).map_err(|_| {
        bridge_config_error("lsp bridge without --project requires a valid initialize request")
    })?;
    if request.get("method").and_then(Value::as_str) != Some("initialize") {
        return Err(bridge_config_error(
            "lsp bridge without --project requires initialize as its first frame",
        ));
    }
    let params = request
        .get_mut("params")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| bridge_config_error("LSP initialize params are required"))?;
    let folder_uri = match params.get("workspaceFolders") {
        Some(Value::Array(folders)) if folders.len() > 1 => {
            return Err(bridge_config_error(
                "multiple LSP workspace folders are unsupported",
            ));
        }
        Some(Value::Array(folders)) if folders.len() == 1 => Some(
            folders[0]
                .get("uri")
                .and_then(Value::as_str)
                .filter(|uri| !uri.is_empty())
                .ok_or_else(|| bridge_config_error("LSP workspace folder URI is required"))?
                .to_owned(),
        ),
        Some(Value::Array(_) | Value::Null) | None => None,
        Some(_) => {
            return Err(bridge_config_error("LSP workspaceFolders must be an array"));
        }
    };
    let root_uri = match params.get("rootUri") {
        Some(Value::String(uri)) if !uri.is_empty() => Some(uri.clone()),
        Some(Value::Null) | None => None,
        Some(_) => {
            return Err(bridge_config_error(
                "LSP initialize rootUri must be a non-empty string",
            ));
        }
    };
    let root_path = root_uri
        .as_deref()
        .map(canonical_file_uri_path)
        .transpose()?;
    let folder_path = folder_uri
        .as_deref()
        .map(canonical_file_uri_path)
        .transpose()?;
    if let (Some(root), Some(folder)) = (&root_path, &folder_path)
        && root != folder
    {
        return Err(bridge_config_error(
            "LSP initialize rootUri and workspace folder differ",
        ));
    }
    let project_root = root_path
        .or(folder_path)
        .ok_or_else(|| bridge_config_error("LSP initialize requires one workspace root"))?;
    let canonical_root_uri = url::Url::from_file_path(&project_root)
        .map_err(|()| bridge_config_error("canonical LSP workspace root is not a file path"))?
        .to_string();

    if root_uri.is_some() {
        params.insert(
            "rootUri".to_owned(),
            Value::String(canonical_root_uri.clone()),
        );
    }
    let mut workspace_folders = Vec::new();
    if folder_uri.is_some() {
        let folders = params
            .get_mut("workspaceFolders")
            .and_then(Value::as_array_mut)
            .expect("validated workspace folders");
        folders[0]
            .as_object_mut()
            .expect("validated workspace folder")
            .insert("uri".to_owned(), Value::String(canonical_root_uri.clone()));
        workspace_folders.push(canonical_root_uri.clone());
    }
    let frame = serde_json::to_string(&request)?;
    Ok(InitializeBinding {
        project_root,
        canonical_root_uri,
        workspace_folders,
        frame,
    })
}

fn canonical_file_uri_path(uri: &str) -> tracedecay::errors::Result<PathBuf> {
    let uri = url::Url::parse(uri)
        .map_err(|_| bridge_config_error("LSP workspace root must be a valid file URI"))?;
    if uri.scheme() != "file" || uri.query().is_some() || uri.fragment().is_some() {
        return Err(bridge_config_error(
            "LSP workspace root must be a local file URI",
        ));
    }
    let path = uri
        .to_file_path()
        .map_err(|()| bridge_config_error("LSP workspace root must be a local file URI"))?;
    canonicalize_workspace_root(&path)
}

fn canonicalize_workspace_root(path: &Path) -> tracedecay::errors::Result<PathBuf> {
    let canonical = path.canonicalize().map_err(|error| {
        bridge_config_error(format!(
            "LSP workspace root '{}' cannot be resolved: {error}",
            path.display()
        ))
    })?;
    canonical
        .is_dir()
        .then_some(canonical)
        .ok_or_else(|| bridge_config_error("LSP workspace root must be a directory"))
}

async fn flush_daemon_frame(
    session: &mut DaemonLspSessionClient,
    stdout: &mut tokio::io::Stdout,
) -> tracedecay::errors::Result<bool> {
    match poll_daemon_frame_with_reconnect(session).await? {
        FramePoll::Frame(frame) => {
            let encoded = ContentLengthCodec::encode(&frame)
                .map_err(|error| bridge_error("encode", error))?;
            stdout.write_all(&encoded).await?;
            stdout.flush().await?;
            acknowledge_daemon_frame_with_reconnect(session).await?;
            Ok(false)
        }
        FramePoll::Pending => Ok(false),
        FramePoll::Closed => Ok(true),
    }
}

async fn send_client_frame_with_reconnect(
    session: &mut DaemonLspSessionClient,
    frame: &str,
) -> tracedecay::errors::Result<FrameSend> {
    match session.try_send_client_frame(frame).await {
        Ok(outcome) => Ok(outcome),
        Err(_) => {
            session.reconnect().await?;
            session.try_send_client_frame(frame).await
        }
    }
}

async fn poll_daemon_frame_with_reconnect(
    session: &mut DaemonLspSessionClient,
) -> tracedecay::errors::Result<FramePoll> {
    match session.poll_daemon_frame().await {
        Ok(outcome) => Ok(outcome),
        Err(_) => {
            session.reconnect().await?;
            session.poll_daemon_frame().await
        }
    }
}

async fn acknowledge_daemon_frame_with_reconnect(
    session: &mut DaemonLspSessionClient,
) -> tracedecay::errors::Result<()> {
    match session.acknowledge_daemon_frame().await {
        Ok(()) => Ok(()),
        Err(_) => {
            session.reconnect().await?;
            session.acknowledge_daemon_frame().await
        }
    }
}

fn bridge_error(phase: &str, error: impl std::fmt::Debug) -> tracedecay::errors::TraceDecayError {
    tracedecay::errors::TraceDecayError::Config {
        message: format!("LSP bridge {phase} failure: {error:?}"),
    }
}

fn bridge_config_error(message: impl Into<String>) -> tracedecay::errors::TraceDecayError {
    tracedecay::errors::TraceDecayError::Config {
        message: message.into(),
    }
}

fn print_lsp_servers(json: bool) -> tracedecay::errors::Result<()> {
    let adapters = lsp_adapters::builtin_adapters();
    if json {
        let rows: Vec<_> = adapters.iter().map(lsp_server_row).collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        print_lsp_servers_table(&adapters);
    }
    Ok(())
}

fn lsp_server_row(adapter: &lsp_adapters::LspAdapterDefinition) -> Value {
    serde_json::json!({
        "language": adapter.language,
        "language_id": adapter.language_id,
        "command": adapter.command,
        "args": adapter.args,
        "available": lsp_broker::command_available(&adapter.command),
        "extensions": adapter.extensions,
        "root_markers": adapter.root_markers,
        "install_options": adapter.install_options,
    })
}

fn print_lsp_servers_table(adapters: &[lsp_adapters::LspAdapterDefinition]) {
    println!(
        "{:<14} {:<12} {:<28} install",
        "language", "available", "command"
    );
    for adapter in adapters {
        let install = adapter
            .install_options
            .first()
            .map_or("", |option| option.command.as_str());
        println!(
            "{:<14} {:<12} {:<28} {}",
            adapter.language,
            if lsp_broker::command_available(&adapter.command) {
                "yes"
            } else {
                "no"
            },
            adapter.command,
            install
        );
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::initialize_binding;

    #[test]
    fn initialize_root_is_canonicalized_and_bound_into_forwarded_frame() {
        let root = tempfile::tempdir().expect("workspace root");
        let root_uri = url::Url::from_file_path(root.path())
            .expect("file URI")
            .to_string();
        let frame = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "rootUri": root_uri,
                "workspaceFolders": [{
                    "uri": root_uri,
                    "name": "workspace"
                }],
                "capabilities": {}
            }
        })
        .to_string();

        let binding = initialize_binding(&frame).expect("initialize binding");
        assert_eq!(
            binding.project_root,
            root.path().canonicalize().expect("canonical workspace")
        );
        let forwarded: Value =
            serde_json::from_str(&binding.frame).expect("forwarded initialize frame");
        assert_eq!(forwarded["params"]["rootUri"], binding.canonical_root_uri);
        assert_eq!(
            forwarded["params"]["workspaceFolders"][0]["uri"],
            binding.canonical_root_uri
        );
        assert_eq!(
            binding.workspace_folders,
            vec![binding.canonical_root_uri.clone()]
        );
    }

    #[test]
    fn initialize_root_binding_fails_closed_on_ambiguous_roots() {
        let first = tempfile::tempdir().expect("first workspace");
        let second = tempfile::tempdir().expect("second workspace");
        let frame = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "rootUri": url::Url::from_file_path(first.path())
                    .expect("first URI")
                    .to_string(),
                "workspaceFolders": [{
                    "uri": url::Url::from_file_path(second.path())
                        .expect("second URI")
                        .to_string(),
                    "name": "other"
                }],
                "capabilities": {}
            }
        })
        .to_string();

        let error = initialize_binding(&frame)
            .err()
            .expect("ambiguous roots fail");
        assert!(
            error
                .to_string()
                .contains("rootUri and workspace folder differ"),
            "{error}"
        );
    }

    #[test]
    fn initialize_root_binding_accepts_equivalent_uri_aliases() {
        let root = tempfile::tempdir().expect("workspace root");
        let root_uri = url::Url::from_file_path(root.path())
            .expect("file URI")
            .to_string();
        let folder_uri = format!("{root_uri}/");
        let frame = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "rootUri": root_uri,
                "workspaceFolders": [{
                    "uri": folder_uri,
                    "name": "workspace"
                }],
                "capabilities": {}
            }
        })
        .to_string();

        let binding = initialize_binding(&frame).expect("equivalent roots bind");
        assert_eq!(
            binding.project_root,
            root.path().canonicalize().expect("canonical workspace")
        );
        let forwarded: Value =
            serde_json::from_str(&binding.frame).expect("forwarded initialize frame");
        assert_eq!(forwarded["params"]["rootUri"], binding.canonical_root_uri);
        assert_eq!(
            forwarded["params"]["workspaceFolders"][0]["uri"],
            binding.canonical_root_uri
        );
    }

    #[test]
    fn initialize_root_binding_rejects_non_file_authority() {
        let frame = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "rootUri": "https://example.com/workspace",
                "capabilities": {}
            }
        })
        .to_string();

        let error = initialize_binding(&frame)
            .err()
            .expect("remote root must fail closed");
        assert!(error.to_string().contains("local file URI"), "{error}");
    }
}
