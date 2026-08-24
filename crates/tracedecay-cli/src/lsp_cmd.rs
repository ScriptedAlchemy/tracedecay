use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::time::{Duration, interval};
use tokio_util::codec::FramedRead;
use tracedecay::daemon::DaemonHandshake;
use tracedecay::daemon_client::{DaemonInvocationClient, DaemonLspSessionClient};
use tracedecay_application::{CancellationSignal, Deadline, InvocationError};
use tracedecay_lsp::analyzer::{adapters as lsp_adapters, broker as lsp_broker};
use tracedecay_lsp::{
    ContentLengthCodec, DEFAULT_LSP_REQUEST_DEADLINE_MS, FramePoll, FrameSend,
    ProcessLocalRequestSequence,
};

use crate::cli::LspAction;

static LSP_BRIDGE_CONTROL_SEQUENCE: ProcessLocalRequestSequence =
    ProcessLocalRequestSequence::starting_at(1);

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
/// `initialize` frame to bind canonical local workspace roots; it never
/// opens a project store, starts an analyzer, or connects the host to an
/// arbitrary daemon socket.
async fn run_stdio_bridge(project_root: Option<PathBuf>) -> tracedecay::errors::Result<()> {
    let mut stdin = FramedRead::new(tokio::io::stdin(), ContentLengthCodec::new());
    let initialize = if project_root.is_none() {
        Some(read_initialize_binding(&mut stdin).await?)
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
    let (deadline, cancellation) = lsp_request_control().map_err(lsp_invocation_error)?;
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
        deadline,
        cancellation,
    )
    .await
    .map_err(lsp_invocation_error)?;
    let mut stdout = tokio::io::stdout();
    let mut pending_client_frame = initialize.map(|binding| binding.frame);
    let mut poll_timer = interval(Duration::from_millis(25));

    let bridge_result = async {
        loop {
            // Keep each pump fair: a queued daemon burst must not starve host input.
            if flush_daemon_frame(&mut session, &mut stdout).await? {
                return Ok(());
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
                frame = stdin.next(), if pending_client_frame.is_none() => {
                    let Some(frame) = frame else {
                        return Ok(());
                    };
                    let frame = frame.map_err(|error| bridge_error("decode", error))?;
                    pending_client_frame = Some(String::from_utf8(frame).map_err(|_| {
                        bridge_config_error("LSP bridge received a non-UTF-8 JSON-RPC payload")
                    })?);
                }
                _ = poll_timer.tick() => {}
            }
        }
    }
    .await;
    let detach_result = detach_stdio_bridge(&mut session).await;
    finish_stdio_bridge(bridge_result, detach_result)
}

async fn detach_stdio_bridge(
    session: &mut DaemonLspSessionClient,
) -> tracedecay::errors::Result<()> {
    let (deadline, cancellation) = lsp_request_control().map_err(lsp_invocation_error)?;
    session
        .detach(deadline, cancellation)
        .await
        .map_err(lsp_invocation_error)
}

fn finish_stdio_bridge(
    bridge_result: tracedecay::errors::Result<()>,
    detach_result: tracedecay::errors::Result<()>,
) -> tracedecay::errors::Result<()> {
    match (bridge_result, detach_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(bridge_error), Err(detach_error)) => Err(bridge_config_error(format!(
            "LSP bridge failed: {bridge_error}; explicit detach failed: {detach_error}"
        ))),
    }
}

struct InitializeBinding {
    project_root: PathBuf,
    canonical_root_uri: String,
    workspace_folders: Vec<String>,
    frame: String,
}

async fn read_initialize_binding<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut FramedRead<R, ContentLengthCodec>,
) -> tracedecay::errors::Result<InitializeBinding> {
    let frame = reader
        .next()
        .await
        .ok_or_else(|| {
            bridge_config_error(
                "lsp bridge without --project requires initialize as its first frame",
            )
        })?
        .map_err(|error| bridge_error("decode", error))?;
    let frame = String::from_utf8(frame)
        .map_err(|_| bridge_config_error("LSP bridge received a non-UTF-8 JSON-RPC payload"))?;
    initialize_binding(&frame)
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
    let folder_uris = match params.get("workspaceFolders") {
        Some(Value::Array(folders)) => folders
            .iter()
            .map(|folder| {
                folder
                    .get("uri")
                    .and_then(Value::as_str)
                    .filter(|uri| !uri.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| bridge_config_error("LSP workspace folder URI is required"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(Value::Null) | None => Vec::new(),
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
    let mut folder_paths = folder_uris
        .iter()
        .map(|uri| canonical_file_uri_path(uri))
        .collect::<Result<Vec<_>, _>>()?;
    folder_paths.sort();
    if folder_paths.windows(2).any(|roots| roots[0] == roots[1]) {
        return Err(bridge_config_error(
            "LSP workspace folders contain a duplicate canonical root",
        ));
    }
    if let Some(root) = &root_path
        && !folder_paths.is_empty()
        && !folder_paths.contains(root)
    {
        return Err(bridge_config_error(
            "LSP initialize rootUri is not an admitted workspace folder",
        ));
    }
    let project_root = root_path
        .or_else(|| folder_paths.first().cloned())
        .ok_or_else(|| bridge_config_error("LSP initialize requires a workspace root"))?;
    let canonical_root_uri = url::Url::from_file_path(&project_root)
        .map_err(|()| bridge_config_error("canonical LSP workspace root is not a file path"))?
        .to_string();

    if root_uri.is_some() {
        params.insert(
            "rootUri".to_owned(),
            Value::String(canonical_root_uri.clone()),
        );
    }
    let workspace_folders = folder_paths
        .iter()
        .map(|path| {
            url::Url::from_file_path(path)
                .map_err(|()| {
                    bridge_config_error("canonical LSP workspace root is not a file path")
                })
                .map(|uri| uri.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !workspace_folders.is_empty() {
        let folders = params
            .get_mut("workspaceFolders")
            .and_then(Value::as_array_mut)
            .expect("validated workspace folders");
        for folder in folders.iter_mut() {
            let uri = folder
                .get("uri")
                .and_then(Value::as_str)
                .expect("validated workspace folder URI");
            let canonical = canonical_file_uri_path(uri)?;
            folder
                .as_object_mut()
                .expect("validated workspace folder")
                .insert(
                    "uri".to_owned(),
                    Value::String(
                        url::Url::from_file_path(canonical)
                            .expect("canonical file URI")
                            .to_string(),
                    ),
                );
        }
        folders.sort_by(|left, right| left["uri"].as_str().cmp(&right["uri"].as_str()));
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
    let (deadline, cancellation) = lsp_request_control().map_err(lsp_invocation_error)?;
    match session
        .try_send_client_frame(frame, deadline, cancellation)
        .await
    {
        Ok(outcome) => Ok(outcome),
        Err(InvocationError::Unavailable) => {
            reconnect_session(session).await?;
            let (deadline, cancellation) = lsp_request_control().map_err(lsp_invocation_error)?;
            session
                .try_send_client_frame(frame, deadline, cancellation)
                .await
                .map_err(lsp_invocation_error)
        }
        Err(error) => Err(lsp_invocation_error(error)),
    }
}

async fn poll_daemon_frame_with_reconnect(
    session: &mut DaemonLspSessionClient,
) -> tracedecay::errors::Result<FramePoll> {
    let (deadline, cancellation) = lsp_request_control().map_err(lsp_invocation_error)?;
    match session.poll_daemon_frame(deadline, cancellation).await {
        Ok(outcome) => Ok(outcome),
        Err(InvocationError::Unavailable) => {
            reconnect_session(session).await?;
            let (deadline, cancellation) = lsp_request_control().map_err(lsp_invocation_error)?;
            session
                .poll_daemon_frame(deadline, cancellation)
                .await
                .map_err(lsp_invocation_error)
        }
        Err(error) => Err(lsp_invocation_error(error)),
    }
}

async fn acknowledge_daemon_frame_with_reconnect(
    session: &mut DaemonLspSessionClient,
) -> tracedecay::errors::Result<()> {
    let (deadline, cancellation) = lsp_request_control().map_err(lsp_invocation_error)?;
    match session
        .acknowledge_daemon_frame(deadline, cancellation)
        .await
    {
        Ok(()) => Ok(()),
        Err(InvocationError::Unavailable) => {
            reconnect_session(session).await?;
            let (deadline, cancellation) = lsp_request_control().map_err(lsp_invocation_error)?;
            session
                .acknowledge_daemon_frame(deadline, cancellation)
                .await
                .map_err(lsp_invocation_error)
        }
        Err(error) => Err(lsp_invocation_error(error)),
    }
}

async fn reconnect_session(session: &mut DaemonLspSessionClient) -> tracedecay::errors::Result<()> {
    let (deadline, cancellation) = lsp_request_control().map_err(lsp_invocation_error)?;
    session
        .reconnect(deadline, cancellation)
        .await
        .map_err(lsp_invocation_error)
}

fn lsp_request_control() -> Result<(Deadline, CancellationSignal), InvocationError> {
    let sequence = LSP_BRIDGE_CONTROL_SEQUENCE
        .next_string("lsp-bridge.")
        .map_err(|_| InvocationError::Unavailable)?;
    let budget_micros = i64::try_from(DEFAULT_LSP_REQUEST_DEADLINE_MS)
        .map_err(|_| InvocationError::Unavailable)?
        .saturating_mul(1_000);
    let expires_at = tracedecay_application::clock::now_micros()
        .0
        .saturating_add(budget_micros);
    let deadline =
        Deadline::new(tracedecay_domain::UtcMicros(expires_at)).map_err(InvocationError::from)?;
    let cancellation = CancellationSignal::active(format!("cancellation.{sequence}"))
        .map_err(InvocationError::from)?;
    Ok((deadline, cancellation))
}

fn lsp_invocation_error(error: InvocationError) -> tracedecay::errors::TraceDecayError {
    let message = match error {
        InvocationError::Cancelled => "LSP gateway request was cancelled".to_owned(),
        InvocationError::DeadlineExceeded => "LSP gateway request deadline elapsed".to_owned(),
        InvocationError::Denied => "LSP gateway request was not authorized".to_owned(),
        InvocationError::InvalidRequest => "LSP gateway request was invalid".to_owned(),
        InvocationError::Conflict => "LSP gateway request conflicted with current state".to_owned(),
        InvocationError::Unavailable => "LSP gateway authority is unavailable".to_owned(),
        InvocationError::Problem(problem) => match problem.diagnostic() {
            Some(diagnostic) => {
                format!("LSP gateway request failed: {}", diagnostic.code)
            }
            None => format!("LSP gateway request failed: {:?}", problem.kind()),
        },
    };
    tracedecay::errors::TraceDecayError::Config { message }
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

    use super::{bridge_config_error, finish_stdio_bridge, initialize_binding};

    #[test]
    fn bridge_completion_propagates_explicit_detach_failure() {
        let error = finish_stdio_bridge(
            Ok(()),
            Err(bridge_config_error("bounded detach was unavailable")),
        )
        .expect_err("detach failure must fail normal bridge completion");

        assert!(error.to_string().contains("bounded detach was unavailable"));
    }

    #[test]
    fn bridge_failure_preserves_explicit_detach_failure() {
        let error = finish_stdio_bridge(
            Err(bridge_config_error("stdout closed")),
            Err(bridge_config_error("bounded detach timed out")),
        )
        .expect_err("both failures must remain visible");
        let message = error.to_string();

        assert!(message.contains("stdout closed"), "{message}");
        assert!(message.contains("bounded detach timed out"), "{message}");
    }

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
                .contains("rootUri is not an admitted workspace folder"),
            "{error}"
        );
    }

    #[test]
    fn initialize_binding_preserves_two_canonical_folders_in_stable_order() {
        let first = tempfile::tempdir().expect("first workspace");
        let second = tempfile::tempdir().expect("second workspace");
        let first_uri = url::Url::from_file_path(first.path()).unwrap().to_string();
        let second_uri = url::Url::from_file_path(second.path()).unwrap().to_string();
        let frame = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "workspaceFolders": [
                    {"uri": second_uri, "name": "second"},
                    {"uri": first_uri, "name": "first"}
                ],
                "capabilities": {}
            }
        })
        .to_string();

        let binding = initialize_binding(&frame).expect("two-root initialize binding");
        assert_eq!(binding.workspace_folders.len(), 2);
        assert!(binding.workspace_folders.is_sorted());
        assert_eq!(
            binding.project_root,
            first
                .path()
                .canonicalize()
                .unwrap()
                .min(second.path().canonicalize().unwrap())
        );
        let forwarded: Value = serde_json::from_str(&binding.frame).unwrap();
        assert_eq!(
            forwarded["params"]["workspaceFolders"][0]["uri"],
            binding.workspace_folders[0]
        );
        assert_eq!(
            forwarded["params"]["workspaceFolders"][1]["uri"],
            binding.workspace_folders[1]
        );
    }

    #[test]
    fn initialize_binding_rejects_duplicate_folder_aliases() {
        let root = tempfile::tempdir().expect("workspace root");
        let root_uri = url::Url::from_file_path(root.path()).unwrap().to_string();
        let frame = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "workspaceFolders": [
                    {"uri": root_uri, "name": "first"},
                    {"uri": format!("{root_uri}/"), "name": "alias"}
                ],
                "capabilities": {}
            }
        })
        .to_string();

        let error = match initialize_binding(&frame) {
            Ok(_) => panic!("duplicate canonical roots must be rejected"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("duplicate canonical root"),
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
