use std::io::Read;
use std::path::{Path, PathBuf};
use tracedecay_runtime_core::config::ProfileRoot;

use serde_json::Value;
use tokio_util::bytes::BytesMut;
use tokio_util::codec::Decoder;
use tracedecay_contracts::{CancellationSignal, Deadline, InvocationError};
use tracedecay_daemon_protocol::{
    DaemonLspSessionClient, FramePoll, FrameSend, LspFrame, ProcessLocalRequestSequence,
};
use tracedecay_framing::pump::{ReadPump, WritePump};
use tracedecay_lsp::analyzer::{adapters as lsp_adapters, broker as lsp_broker};
use tracedecay_lsp::{
    AsyncContentLengthError, ContentLengthCodec, DEFAULT_LSP_REQUEST_DEADLINE_MS,
};

use crate::cli::LspAction;

static LSP_BRIDGE_CONTROL_SEQUENCE: ProcessLocalRequestSequence =
    ProcessLocalRequestSequence::starting_at(1);

pub(crate) async fn handle_lsp_action(
    profile: &ProfileRoot,
    action: LspAction,
) -> tracedecay_domain::errors::Result<()> {
    match action {
        LspAction::Servers { json } => {
            tracing::Instrument::instrument(
                print_lsp_servers(profile, json),
                tracing::trace_span!("cli.lsp.servers"),
            )
            .await?;
        }
        LspAction::Bridge { stdio, project } => {
            if !stdio {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: "lsp bridge requires --stdio".to_owned(),
                });
            }
            tracing::Instrument::instrument(
                run_stdio_bridge(profile, project.map(PathBuf::from)),
                tracing::trace_span!("cli.lsp.bridge"),
            )
            .await?;
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
async fn run_stdio_bridge(
    profile: &ProfileRoot,
    project_root: Option<PathBuf>,
) -> tracedecay_domain::errors::Result<()> {
    let stdin = client_frames(std::io::stdin())?;
    let initialize = if project_root.is_none() {
        Some(read_initialize_binding(&stdin).await?)
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
    let handshake = crate::commands::client_handshake(profile, Some(&project_root))?;
    let invocation = tracedecay::daemon::invocation_client_for_current(profile, handshake)?;
    let (deadline, cancellation) = lsp_request_control().map_err(lsp_invocation_error)?;
    let mut session = DaemonLspSessionClient::open(
        invocation,
        env!("CARGO_PKG_VERSION"),
        initialize
            .as_ref()
            .map(|binding| binding.canonical_root_uri.clone()),
        initialize
            .as_ref()
            .map_or_default(|binding| binding.workspace_folders.clone()),
        deadline,
        cancellation,
    )
    .await
    .map_err(lsp_invocation_error)?;
    let stdout = WritePump::spawn("tracedecay-lsp-stdout", std::io::stdout())?;
    let bridge_result = pump_stdio_bridge(
        &stdin,
        &stdout,
        &mut session,
        initialize.map(|binding| binding.frame),
    )
    .await;
    let detach_result = detach_stdio_bridge(&mut session).await;
    finish_stdio_bridge(bridge_result, detach_result)
}

type ClientFrames = ReadPump<Result<LspFrame, AsyncContentLengthError>>;

/// Strict `Content-Length` frames read from `source` on a dedicated thread.
/// Like `FramedRead`, the frames end at EOF or after the first failure.
fn client_frames(mut source: impl Read + Send + 'static) -> std::io::Result<ClientFrames> {
    let mut codec = ContentLengthCodec::new();
    let mut buffered = BytesMut::new();
    let mut chunk = vec![0; 8 * 1024];
    let mut ended = false;
    ReadPump::spawn("tracedecay-lsp-stdin", move || {
        while !ended {
            let decoded = match codec.decode(&mut buffered) {
                Ok(None) => match source.read(&mut chunk) {
                    Ok(0) => {
                        ended = true;
                        codec.decode_eof(&mut buffered)
                    }
                    Ok(read) => {
                        buffered.extend_from_slice(&chunk[..read]);
                        continue;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => Err(error.into()),
                },
                decoded => decoded,
            };
            ended |= decoded.is_err();
            if let Some(frame) = decoded.transpose() {
                return Some(frame);
            }
        }
        None
    })
}

/// Forwards frames until either side closes. At most one client frame is held
/// while the daemon backpressures, and a daemon frame is acknowledged only
/// after stdout has flushed it.
async fn pump_stdio_bridge(
    stdin: &ClientFrames,
    stdout: &WritePump,
    session: &mut DaemonLspSessionClient,
    mut pending_client_frame: Option<String>,
) -> tracedecay_domain::errors::Result<()> {
    loop {
        // Keep each pump fair: a queued daemon burst must not starve host input.
        if flush_daemon_frame(session, stdout).await? {
            return Ok(());
        }

        if let Some(frame) = pending_client_frame.as_deref() {
            let sent = with_reconnect(session, async |session, deadline, cancellation| {
                session
                    .try_send_client_frame(frame, deadline, cancellation)
                    .await
            })
            .await?;
            match sent {
                FrameSend::Sent => {
                    pending_client_frame = None;
                    continue;
                }
                FrameSend::Backpressured => {}
                FrameSend::Closed => return Ok(()),
            }
        }

        tokio::select! {
            frame = stdin.recv(), if pending_client_frame.is_none() => {
                let Some(frame) = frame else {
                    return Ok(());
                };
                let frame = frame.map_err(|error| bridge_error("decode", error))?;
                pending_client_frame = Some(String::from_utf8(frame).map_err(|_| {
                    bridge_config_error("LSP bridge received a non-UTF-8 JSON-RPC payload")
                })?);
            }
            () = asupersync::time::sleep(
                asupersync::time::wall_now(),
                std::time::Duration::from_millis(25),
            ) => {}
        }
    }
}

async fn detach_stdio_bridge(
    session: &mut DaemonLspSessionClient,
) -> tracedecay_domain::errors::Result<()> {
    let (deadline, cancellation) = lsp_request_control().map_err(lsp_invocation_error)?;
    session
        .detach(deadline, cancellation)
        .await
        .map_err(lsp_invocation_error)
}

fn finish_stdio_bridge(
    bridge_result: tracedecay_domain::errors::Result<()>,
    detach_result: tracedecay_domain::errors::Result<()>,
) -> tracedecay_domain::errors::Result<()> {
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

async fn read_initialize_binding(
    reader: &ClientFrames,
) -> tracedecay_domain::errors::Result<InitializeBinding> {
    let frame = reader
        .recv()
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

fn initialize_binding(frame: &str) -> tracedecay_domain::errors::Result<InitializeBinding> {
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

fn canonical_file_uri_path(uri: &str) -> tracedecay_domain::errors::Result<PathBuf> {
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

fn canonicalize_workspace_root(path: &Path) -> tracedecay_domain::errors::Result<PathBuf> {
    let canonical = path.canonicalize().map_err(|error| {
        bridge_config_error(format!(
            "LSP workspace root '{}' cannot be resolved: {error}",
            path.display()
        ))
    })?;
    // The bridge replaces the client's `rootUri` with this path, and the
    // daemon publishes the root's identity, so the two must spell it the same
    // way: `canonicalize` yields the `\\?\` verbatim form on Windows, which
    // is not what the daemon advertises.
    canonical
        .is_dir()
        .then(|| tracedecay_runtime_core::path_safety::canonical_root_identity(&canonical))
        .ok_or_else(|| bridge_config_error("LSP workspace root must be a directory"))
}

async fn flush_daemon_frame(
    session: &mut DaemonLspSessionClient,
    stdout: &WritePump,
) -> tracedecay_domain::errors::Result<bool> {
    let polled = with_reconnect(session, async |session, deadline, cancellation| {
        session.poll_daemon_frame(deadline, cancellation).await
    })
    .await?;
    match polled {
        FramePoll::Frame(frame) => {
            let encoded = ContentLengthCodec::encode(&frame)
                .map_err(|error| bridge_error("encode", error))?;
            stdout.write_flushed(encoded).await?;
            with_reconnect(session, async |session, deadline, cancellation| {
                session
                    .acknowledge_daemon_frame(deadline, cancellation)
                    .await
            })
            .await?;
            Ok(false)
        }
        FramePoll::Pending => Ok(false),
        FramePoll::Closed => Ok(true),
    }
}

/// Runs one session call under fresh request controls; an unavailable daemon
/// connection is reconnected once and the call retried.
async fn with_reconnect<T>(
    session: &mut DaemonLspSessionClient,
    mut call: impl AsyncFnMut(
        &mut DaemonLspSessionClient,
        Deadline,
        CancellationSignal,
    ) -> Result<T, InvocationError>,
) -> tracedecay_domain::errors::Result<T> {
    let (deadline, cancellation) = lsp_request_control().map_err(lsp_invocation_error)?;
    match call(session, deadline, cancellation).await {
        Err(InvocationError::Unavailable) => {
            reconnect_session(session).await?;
            let (deadline, cancellation) = lsp_request_control().map_err(lsp_invocation_error)?;
            call(session, deadline, cancellation)
                .await
                .map_err(lsp_invocation_error)
        }
        outcome => outcome.map_err(lsp_invocation_error),
    }
}

async fn reconnect_session(
    session: &mut DaemonLspSessionClient,
) -> tracedecay_domain::errors::Result<()> {
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
    let expires_at = tracedecay_contracts::clock::now_micros()
        .0
        .saturating_add(budget_micros);
    let deadline =
        Deadline::new(tracedecay_domain::UtcMicros(expires_at)).map_err(InvocationError::from)?;
    let cancellation = CancellationSignal::active(format!("cancellation.{sequence}"))
        .map_err(InvocationError::from)?;
    Ok((deadline, cancellation))
}

fn lsp_invocation_error(error: InvocationError) -> tracedecay_domain::errors::TraceDecayError {
    let message = match error {
        InvocationError::Cancelled => "LSP gateway request was cancelled".to_owned(),
        InvocationError::DeadlineExceeded => "LSP gateway request deadline elapsed".to_owned(),
        InvocationError::Denied => "LSP gateway request was not authorized".to_owned(),
        InvocationError::InvalidRequest => "LSP gateway request was invalid".to_owned(),
        InvocationError::Conflict => "LSP gateway request conflicted with current state".to_owned(),
        InvocationError::Unavailable => "LSP gateway authority is unavailable".to_owned(),
        InvocationError::Unreachable { detail, .. } => detail,
        InvocationError::Problem(problem) => match problem.diagnostic() {
            Some(diagnostic) => {
                format!("LSP gateway request failed: {}", diagnostic.code)
            }
            None => format!("LSP gateway request failed: {:?}", problem.kind()),
        },
    };
    tracedecay_domain::errors::TraceDecayError::Config { message }
}

fn bridge_error(
    phase: &str,
    error: impl std::fmt::Debug,
) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("LSP bridge {phase} failure: {error:?}"),
    }
}

fn bridge_config_error(message: impl Into<String>) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Config {
        message: message.into(),
    }
}

/// Where `lsp servers` resolved analyzer availability.
///
/// The daemon is the authority: it spawns analyzers on its own PATH and its
/// broker is what `tracedecay doctor` grades, so a daemon-resolved table can
/// never disagree with Doctor. Without a reachable daemon the only PATH left
/// to consult is the CLI shell's, which the daemon does not use; that read is
/// labelled as such rather than passed off as the daemon's view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum LspAvailabilityResolution {
    Daemon,
    CliPath,
}

struct LspServersInventory {
    resolution: LspAvailabilityResolution,
    /// Why the daemon read was not used, when it was not.
    daemon_unavailable: Option<String>,
    rows: Vec<Value>,
}

async fn print_lsp_servers(
    profile: &ProfileRoot,
    json: bool,
) -> tracedecay_domain::errors::Result<()> {
    let inventory = lsp_servers_inventory(profile).await;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "resolution": inventory.resolution,
                "daemon_unavailable": inventory.daemon_unavailable,
                "servers": inventory.rows,
            }))?
        );
        return Ok(());
    }
    match inventory.resolution {
        LspAvailabilityResolution::Daemon => {
            println!(
                "availability resolved by the daemon for the current project (same read as `tracedecay doctor`)"
            );
        }
        LspAvailabilityResolution::CliPath => {
            println!(
                "availability resolved on this shell's PATH, not the daemon's: {}",
                inventory
                    .daemon_unavailable
                    .as_deref()
                    .unwrap_or("daemon read unavailable")
            );
        }
    }
    println!(
        "{:<14} {:<8} {:<12} {:<28} install",
        "language", "active", "state", "command"
    );
    for row in &inventory.rows {
        let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or("");
        println!(
            "{:<14} {:<8} {:<12} {:<28} {}",
            text("language"),
            match row.get("active").and_then(Value::as_bool) {
                Some(true) => "yes",
                Some(false) => "no",
                None => "?",
            },
            text("state"),
            text("command"),
            row.pointer("/install_options/0/command")
                .and_then(Value::as_str)
                .unwrap_or("")
        );
    }
    Ok(())
}

/// The compiled-in adapter catalogue, with availability and project activity
/// merged from the daemon's resolved read when one is reachable.
async fn lsp_servers_inventory(profile: &ProfileRoot) -> LspServersInventory {
    use tracedecay_contracts::doctor::LanguageServerReadV1;

    let adapters = lsp_adapters::builtin_adapters();
    let project_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let daemon_read = tracedecay::doctor::daemon_language_server_read(profile, &project_path).await;
    let (analyzers, daemon_unavailable) = match daemon_read {
        Ok(Some(
            LanguageServerReadV1::Observed { analyzers, .. }
            | LanguageServerReadV1::Absent { analyzers },
        )) => (Some(analyzers), None),
        Ok(Some(LanguageServerReadV1::Unsupported)) => (
            None,
            Some("language-server inspection is unsupported on this platform".to_owned()),
        ),
        Ok(Some(LanguageServerReadV1::Denied)) => (
            None,
            Some("the daemon denied analyzer inspection".to_owned()),
        ),
        Ok(Some(LanguageServerReadV1::Unknown)) => (
            None,
            Some("the daemon could not determine analyzer state".to_owned()),
        ),
        Ok(None) => (
            None,
            Some(format!(
                "the daemon has not published analyzer state for '{}' yet",
                project_path.display()
            )),
        ),
        Err(error) => (None, Some(error.to_string())),
    };
    let resolution = if analyzers.is_some() {
        LspAvailabilityResolution::Daemon
    } else {
        LspAvailabilityResolution::CliPath
    };
    let rows = adapters
        .iter()
        .map(|adapter| {
            let resolved = analyzers.as_deref().and_then(|analyzers| {
                analyzers
                    .iter()
                    .find(|analyzer| analyzer.language == adapter.language)
            });
            let (available, active, state, command) = match resolved {
                Some(analyzer) => (
                    analyzer.executable_found,
                    Value::Bool(analyzer.state.aggregate().is_some()),
                    lsp_analyzer_state_label(analyzer.state),
                    analyzer.command.as_str(),
                ),
                None => (
                    lsp_broker::command_available(&adapter.command),
                    Value::Null,
                    "unresolved",
                    adapter.command.as_str(),
                ),
            };
            serde_json::json!({
                "language": adapter.language,
                "language_id": adapter.language_id,
                "command": command,
                "args": adapter.args,
                "available": available,
                "active": active,
                "state": state,
                "extensions": adapter.extensions,
                "root_markers": adapter.root_markers,
                "install_options": adapter.install_options,
            })
        })
        .collect();
    LspServersInventory {
        resolution,
        daemon_unavailable,
        rows,
    }
}

fn lsp_analyzer_state_label(
    state: tracedecay_contracts::doctor::LanguageServerAnalyzerStateV1,
) -> &'static str {
    use tracedecay_contracts::doctor::LanguageServerAnalyzerStateV1 as State;

    match state {
        State::Ready => "ready",
        State::Available => "available",
        State::Refreshing => "refreshing",
        State::Disabled => "disabled",
        State::Unavailable => "unavailable",
        State::Crashed => "crashed",
        State::Inactive => "inactive",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tracedecay_runtime_core::path_safety::canonical_root_identity;

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
        assert_eq!(binding.project_root, canonical_root_identity(root.path()));
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
            canonical_root_identity(first.path()).min(canonical_root_identity(second.path()))
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

    #[tokio::test]
    async fn client_frames_decode_back_to_back_and_end_after_a_truncated_frame() {
        use tracedecay_lsp::{
            AsyncContentLengthError, ContentLengthCodec, ContentLengthCodecError,
        };

        let mut wire = ContentLengthCodec::encode(b"{\"id\":1}").unwrap();
        wire.extend(ContentLengthCodec::encode(b"{\"id\":2}").unwrap());
        wire.extend_from_slice(b"Content-Length: 9\r\n\r\n{\"id\"");
        let frames = super::client_frames(std::io::Cursor::new(wire)).unwrap();

        assert_eq!(frames.recv().await.unwrap().unwrap(), b"{\"id\":1}");
        assert_eq!(frames.recv().await.unwrap().unwrap(), b"{\"id\":2}");
        assert!(matches!(
            frames.recv().await.unwrap(),
            Err(AsyncContentLengthError::Codec(
                ContentLengthCodecError::UnexpectedEof
            ))
        ));
        assert!(frames.recv().await.is_none());
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

    #[cfg(unix)]
    mod pump {
        use std::io::Write;
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use serde_json::{Value, json};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tracedecay_daemon_protocol::{
            BrokerListener, DAEMON_INVOCATION_PROTOCOL, DAEMON_INVOCATION_REVISION,
            DaemonAuthPreface, DaemonClientIdentity, DaemonConnection, DaemonHandshake,
            DaemonInvocationClient, DaemonLspSessionClient, MovedStoreAdoption,
            default_loopback_endpoint,
        };
        use tracedecay_lsp::ContentLengthCodec;

        use tracedecay_framing::pump::WritePump;

        use super::super::{client_frames, lsp_request_control, pump_stdio_bridge};

        const TOKEN: &str = "lsp-pump-test-token";
        const DAEMON_FRAME: &str = r#"{"jsonrpc":"2.0","method":"window/logMessage","params":{"type":3,"message":"daemon"}}"#;
        const FIRST_CLIENT_FRAME: &str = r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#;
        const SECOND_CLIENT_FRAME: &str =
            r#"{"jsonrpc":"2.0","method":"textDocument/didSave","params":{}}"#;

        /// Accepts three bytes per write, so one frame takes many partial
        /// writes, and records how many written bytes were flushed.
        #[derive(Clone, Default)]
        struct TrickleStdout(Arc<Mutex<(Vec<u8>, usize)>>);

        impl Write for TrickleStdout {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                let accepted = buf.len().min(3);
                self.0
                    .lock()
                    .expect("stdout lock")
                    .0
                    .extend_from_slice(&buf[..accepted]);
                Ok(accepted)
            }

            fn flush(&mut self) -> std::io::Result<()> {
                let mut state = self.0.lock().expect("stdout lock");
                state.1 = state.0.len();
                Ok(())
            }
        }

        fn reply(request: &Value, outcome: Value) -> String {
            let mut response = json!({
                "protocol": DAEMON_INVOCATION_PROTOCOL,
                "revision": DAEMON_INVOCATION_REVISION,
                "request_id": request["request_id"],
            });
            response
                .as_object_mut()
                .expect("response object")
                .extend(outcome.as_object().expect("outcome object").clone());
            format!("{response}\n")
        }

        /// Answers the bridge like the daemon actor: one queued daemon frame,
        /// the first client frame backpressured, and a close once both client
        /// frames were accepted. Returns the stdout bytes seen at
        /// acknowledgement and every client frame in arrival order.
        async fn scripted_daemon(
            listener: BrokerListener,
            stdout: TrickleStdout,
        ) -> (Option<Vec<u8>>, Vec<String>) {
            let (reader, mut writer) =
                tokio::io::split(listener.accept().await.expect("accept bridge connection"));
            let mut lines = BufReader::new(reader).lines();
            let preface = lines.next_line().await.expect("read preface");
            let preface = preface.expect("auth preface");
            assert!(
                DaemonAuthPreface::from_line(preface.trim())
                    .expect("parse preface")
                    .authenticate(TOKEN)
            );
            lines
                .next_line()
                .await
                .expect("read handshake")
                .expect("handshake");

            let mut daemon_frame_sent = false;
            let mut acknowledged_stdout = None;
            let mut client_frames = Vec::new();
            while let Some(line) = lines.next_line().await.expect("read invocation") {
                let request: Value = serde_json::from_str(&line).expect("invocation json");
                let outcome = match request["operation"].as_str().expect("operation") {
                    "lsp_open" => json!({
                        "status": "lsp_opened",
                        "session": {"session_id": "lsp-pump-test", "credential": "0".repeat(64)},
                        "expires_at_ms": 1000,
                    }),
                    "lsp_poll" if !daemon_frame_sent => {
                        daemon_frame_sent = true;
                        json!({"status": "lsp_frame", "frame": DAEMON_FRAME, "closed": false})
                    }
                    "lsp_poll" => json!({
                        "status": "lsp_frame",
                        "frame": null,
                        "closed": client_frames.len() == 3,
                    }),
                    "lsp_acknowledge" => {
                        let (written, flushed) = stdout.0.lock().expect("stdout lock").clone();
                        assert_eq!(flushed, written.len(), "acknowledged before stdout flushed");
                        acknowledged_stdout = Some(written);
                        json!({"status": "lsp_acknowledged", "acknowledged": true})
                    }
                    "lsp_frame" => {
                        client_frames.push(request["frame"].as_str().expect("frame").to_owned());
                        json!({
                            "status": "lsp_frame_accepted",
                            "backpressured": client_frames.len() == 1,
                            "closed": false,
                        })
                    }
                    operation => panic!("unexpected bridge operation {operation}"),
                };
                writer
                    .write_all(reply(&request, outcome).as_bytes())
                    .await
                    .expect("write reply");
                writer.flush().await.expect("flush reply");
            }
            (acknowledged_stdout, client_frames)
        }

        #[tokio::test]
        async fn pump_holds_one_backpressured_frame_and_acknowledges_after_flush() {
            let (listener, endpoint) = BrokerListener::bind(&default_loopback_endpoint())
                .await
                .expect("loopback listener");
            let stdout = TrickleStdout::default();
            let daemon = tokio::spawn(scripted_daemon(listener, stdout.clone()));

            let profile = tempfile::TempDir::new().expect("profile");
            let handshake = DaemonHandshake {
                project_path: Some(profile.path().to_path_buf()),
                scope_prefix: None,
                timings: false,
                allow_init: false,
                allow_initialize_root_routing: false,
                client_identity: DaemonClientIdentity {
                    profile_root: profile.path().to_path_buf(),
                    global_db_path: profile.path().join("global.db"),
                },
                client_version: env!("CARGO_PKG_VERSION").to_owned(),
                client_instance_id: "client.lsp-pump-test".to_owned(),
                tool_list_changed_capable: false,
                catalog_version: String::new(),
                moved_store_adoption: MovedStoreAdoption::Never,
            };
            let invocation = DaemonInvocationClient::new(
                DaemonConnection::new(endpoint, TOKEN.to_owned()),
                handshake,
            );
            let (deadline, cancellation) = lsp_request_control().expect("open control");
            let mut session = DaemonLspSessionClient::open(
                invocation,
                env!("CARGO_PKG_VERSION"),
                None,
                Vec::new(),
                deadline,
                cancellation,
            )
            .await
            .expect("open LSP session");

            let mut host = Vec::new();
            for frame in [FIRST_CLIENT_FRAME, SECOND_CLIENT_FRAME] {
                host.extend(ContentLengthCodec::encode(frame.as_bytes()).expect("encode"));
            }
            let stdin = client_frames(std::io::Cursor::new(host)).expect("stdin pump");
            let stdout_pump = WritePump::spawn("test-stdout", stdout.clone()).expect("stdout pump");
            tokio::time::timeout(
                Duration::from_secs(10),
                pump_stdio_bridge(&stdin, &stdout_pump, &mut session, None),
            )
            .await
            .expect("pump stops when the daemon closes")
            .expect("pump result");
            drop(session);

            let (acknowledged_stdout, client_frames) = daemon.await.expect("daemon task");
            let encoded = ContentLengthCodec::encode(DAEMON_FRAME.as_bytes()).expect("encode");
            assert_eq!(acknowledged_stdout.as_ref(), Some(&encoded));
            assert_eq!(stdout.0.lock().expect("stdout lock").0, encoded);
            assert_eq!(
                client_frames,
                [FIRST_CLIENT_FRAME, FIRST_CLIENT_FRAME, SECOND_CLIENT_FRAME]
            );
        }
    }
}
