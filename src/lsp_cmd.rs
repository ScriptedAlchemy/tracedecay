use std::path::PathBuf;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::{Duration, interval};
use tracedecay::daemon::DaemonHandshake;
use tracedecay::daemon_client::{DaemonInvocationClient, DaemonLspSessionClient};
use tracedecay::diagnostics::lsp::{adapters as lsp_adapters, broker as lsp_broker};
use tracedecay::lsp_bridge::{ContentLengthCodec, FramePoll, FrameSend};

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
            run_stdio_bridge(PathBuf::from(project)).await?;
        }
    }
    Ok(())
}

/// Runs a strict `Content-Length` bridge over one daemon-owned LSP session.
///
/// The loop only forwards bounded LSP frames through explicit session
/// operations. It never parses LSP JSON-RPC, opens a project store, starts an
/// analyzer, or connects the host to an arbitrary daemon socket.
async fn run_stdio_bridge(project_root: PathBuf) -> tracedecay::errors::Result<()> {
    let handshake = DaemonHandshake::for_current_client(Some(project_root), None, false, false)?;
    let invocation = DaemonInvocationClient::for_current(handshake)?;
    let mut session =
        DaemonLspSessionClient::open(invocation, env!("CARGO_PKG_VERSION"), None, Vec::new())
            .await?;
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut codec = ContentLengthCodec::new();
    let mut read_buffer = [0_u8; 8 * 1024];
    let mut pending_client_frame = None::<String>;
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
