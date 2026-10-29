use crate::{DaemonHandshake, FramePoll, FrameSend};
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[cfg(unix)]
const LSP_TEST_TOKEN: &str = "lsp-test-token";

#[cfg(unix)]
fn future_lsp_deadline(after: std::time::Duration) -> tracedecay_contracts::Deadline {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_micros();
    let now = i64::try_from(now).unwrap_or(i64::MAX);
    let delta = i64::try_from(after.as_micros()).unwrap_or(i64::MAX);
    tracedecay_contracts::Deadline::new(tracedecay_domain::UtcMicros(now.saturating_add(delta)))
        .expect("LSP deadline")
}

#[cfg(unix)]
fn active_lsp_control(
    token: &str,
) -> (
    tracedecay_contracts::Deadline,
    tracedecay_contracts::CancellationSignal,
) {
    (
        future_lsp_deadline(std::time::Duration::from_secs(2)),
        tracedecay_contracts::CancellationSignal::active(token).expect("LSP cancellation"),
    )
}

#[cfg(unix)]
fn lsp_test_invocation(
    endpoint: crate::DaemonEndpoint,
    profile: &TempDir,
    client_instance_id: &str,
) -> crate::DaemonInvocationClient {
    let handshake = DaemonHandshake {
        project_path: Some(profile.path().to_path_buf()),
        scope_prefix: None,
        timings: false,
        allow_init: false,
        allow_initialize_root_routing: false,
        client_identity: test_client_identity_for(profile.path().to_path_buf()),
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        client_instance_id: client_instance_id.to_owned(),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
        moved_store_adoption: crate::MovedStoreAdoption::Never,
    };
    crate::DaemonInvocationClient::new(
        crate::DaemonConnection::new(endpoint, LSP_TEST_TOKEN.to_owned()),
        handshake,
    )
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_lsp_client_closes_transport_without_spawning_detach() {
    let (listener, endpoint) = crate::BrokerListener::bind(&crate::default_loopback_endpoint())
        .await
        .expect("loopback listener");
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept client");
        let (reader, mut writer) = tokio::io::split(stream);
        let mut lines = tokio::io::BufReader::new(reader).lines();
        read_authenticated_handshake(&mut lines, LSP_TEST_TOKEN).await;
        let open: Value = serde_json::from_str(
            &lines
                .next_line()
                .await
                .expect("read open")
                .expect("open request"),
        )
        .expect("open json");
        assert_eq!(open["operation"], "lsp_open");
        let response = serde_json::json!({
            "protocol": crate::DAEMON_INVOCATION_PROTOCOL,
            "revision": crate::DAEMON_INVOCATION_REVISION,
            "request_id": "lsp.1",
            "status": "lsp_opened",
            "session": {
                "session_id": "lsp-drop-test",
                "credential": "0000000000000000000000000000000000000000000000000000000000000000"
            },
            "expires_at_ms": 1000
        });
        writer
            .write_all(serde_json::to_string(&response).unwrap().as_bytes())
            .await
            .expect("write open response");
        writer.write_all(b"\n").await.expect("response newline");
        writer.flush().await.expect("flush response");

        let next = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect("client drop must close the transport")
            .expect("read after client drop");
        assert!(
            next.is_none(),
            "client Drop must not launch a detached LSP request: {next:?}"
        );
    });
    let profile = TempDir::new().expect("profile");
    let invocation = lsp_test_invocation(endpoint, &profile, "client.drop-test");
    let (deadline, cancellation) = active_lsp_control("cancel.lsp.drop-test");
    let session = crate::DaemonLspSessionClient::open(
        invocation,
        env!("CARGO_PKG_VERSION"),
        None,
        Vec::new(),
        deadline,
        cancellation,
    )
    .await
    .expect("open LSP session");

    drop(session);
    server.await.expect("server task");
}

#[cfg(unix)]
enum LspOpenInterrupt {
    Cancel,
    /// Lets the open's own deadline run out.
    ExpireDeadline,
}

#[cfg(unix)]
impl LspOpenInterrupt {
    fn fire(self, cancellation: &tracedecay_contracts::CancellationSignal) {
        match self {
            Self::Cancel => {
                cancellation.cancel(tracedecay_contracts::clock::now_micros());
            }
            Self::ExpireDeadline => {}
        }
    }
}

/// What one LSP open against a never-answering daemon settled to: the open's
/// error, the control its `lsp_open` carried (`None` when the client settled
/// before sending one), and the daemon's next read after the interruption
/// (`None` once the client closed the stream).
#[cfg(unix)]
type UnansweredLspOpen = (
    Option<tracedecay_contracts::InvocationError>,
    Option<Value>,
    Option<String>,
);

/// Opens an LSP session against a daemon that reads the `lsp_open` and never
/// answers it; `interrupt` fires once the request is in flight.
///
/// The daemon's `accept` is bounded by the client's own deadline, because a
/// client that settles before connecting leaves nothing to wake it. Once
/// connected, every client exit closes the stream and ends the daemon's reads.
/// The whole exchange is bounded one second past the deadline, so a client
/// that ignores its control yields `Err(Elapsed)` rather than a hang.
#[cfg(unix)]
async fn interrupt_unanswered_lsp_open(
    client_instance_id: &str,
    interrupt: LspOpenInterrupt,
) -> Result<UnansweredLspOpen, tokio::time::error::Elapsed> {
    let (listener, endpoint) = crate::BrokerListener::bind(&crate::default_loopback_endpoint())
        .await
        .expect("loopback listener");
    let profile = TempDir::new().expect("profile");
    let invocation = lsp_test_invocation(endpoint, &profile, client_instance_id);
    let (deadline, cancellation) = active_lsp_control(&format!("cancel.{client_instance_id}"));
    let expected_deadline = deadline.expires_at.0;
    let expiry = tokio::time::Instant::now()
        + crate::deadline_remaining(&deadline).expect("future LSP deadline");
    let daemon_cancellation = cancellation.clone();
    let daemon = async move {
        let Ok(stream) = tokio::time::timeout_at(expiry, listener.accept()).await else {
            return (None, None);
        };
        let (reader, _writer) = tokio::io::split(stream.expect("accept client"));
        let mut lines = tokio::io::BufReader::new(reader).lines();
        read_authenticated_handshake(&mut lines, LSP_TEST_TOKEN).await;
        let Some(open) = lines.next_line().await.expect("read open") else {
            return (None, None);
        };
        let open: Value = serde_json::from_str(&open).expect("open json");
        let control = serde_json::json!({
            "operation": open["operation"],
            "carries_deadline": open["deadline"]["expires_at"] == expected_deadline,
            "cancellation": open["cancellation"]["state"]["state"],
        });
        interrupt.fire(&daemon_cancellation);
        (
            Some(control),
            lines.next_line().await.expect("read after interruption"),
        )
    };
    let open = Box::pin(crate::DaemonLspSessionClient::open(
        invocation,
        env!("CARGO_PKG_VERSION"),
        None,
        Vec::new(),
        deadline,
        cancellation,
    ));

    tokio::time::timeout_at(expiry + std::time::Duration::from_secs(1), async {
        let (result, (control, after_interrupt)) = tokio::join!(open, daemon);
        (result.err(), control, after_interrupt)
    })
    .await
}

#[cfg(unix)]
#[tokio::test]
async fn lsp_gateway_open_carries_control_and_returns_typed_deadline() {
    assert_eq!(
        interrupt_unanswered_lsp_open("client.lsp-deadline-test", LspOpenInterrupt::ExpireDeadline)
            .await,
        Ok((
            Some(tracedecay_contracts::InvocationError::DeadlineExceeded),
            Some(serde_json::json!({
                "operation": "lsp_open",
                "carries_deadline": true,
                "cancellation": "active",
            })),
            None,
        )),
        "a timed-out open must carry its control, settle typed, and close its stream"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn lsp_gateway_open_returns_typed_cancellation() {
    assert_eq!(
        interrupt_unanswered_lsp_open("client.lsp-cancel-test", LspOpenInterrupt::Cancel).await,
        Ok((
            Some(tracedecay_contracts::InvocationError::Cancelled),
            Some(serde_json::json!({
                "operation": "lsp_open",
                "carries_deadline": true,
                "cancellation": "active",
            })),
            None,
        )),
        "a cancelled open must carry its control, settle typed, and close its stream"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn lsp_gateway_open_returns_typed_unavailable_when_daemon_disconnects() {
    let (listener, endpoint) = crate::BrokerListener::bind(&crate::default_loopback_endpoint())
        .await
        .expect("loopback listener");
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept client");
        let (reader, _writer) = tokio::io::split(stream);
        let mut lines = tokio::io::BufReader::new(reader).lines();
        read_authenticated_handshake(&mut lines, LSP_TEST_TOKEN).await;
        lines
            .next_line()
            .await
            .expect("read open")
            .expect("open request");
    });
    let profile = TempDir::new().expect("profile");
    let invocation = lsp_test_invocation(endpoint, &profile, "client.lsp-unavailable-test");
    let (deadline, cancellation) = active_lsp_control("cancel.lsp.unavailable-test");

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        crate::DaemonLspSessionClient::open(
            invocation,
            env!("CARGO_PKG_VERSION"),
            None,
            Vec::new(),
            deadline,
            cancellation,
        ),
    )
    .await
    .expect("daemon disconnect must terminate the open");

    assert!(matches!(
        result,
        Err(tracedecay_contracts::InvocationError::Unavailable)
    ));
    server.await.expect("server task");
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_bridge_session_reconnects_on_a_fresh_socket_and_resumes_frames() {
    let (listener, endpoint) = crate::BrokerListener::bind(&crate::default_loopback_endpoint())
        .await
        .expect("loopback listener");
    let server = tokio::spawn(async move {
        let first = listener.accept().await.expect("accept first connection");
        let (reader, mut writer) = tokio::io::split(first);
        let mut lines = tokio::io::BufReader::new(reader).lines();
        read_authenticated_handshake(&mut lines, LSP_TEST_TOKEN).await;
        let open: Value = serde_json::from_str(
            &lines
                .next_line()
                .await
                .expect("read open")
                .expect("open request"),
        )
        .expect("open json");
        assert_eq!(open["operation"], "lsp_open");
        let open_response = serde_json::json!({
            "protocol": crate::DAEMON_INVOCATION_PROTOCOL,
            "revision": crate::DAEMON_INVOCATION_REVISION,
            "request_id": "lsp.1",
            "status": "lsp_opened",
            "session": {
                "session_id": "lsp-reconnect-test",
                "credential": "0000000000000000000000000000000000000000000000000000000000000000"
            },
            "expires_at_ms": 1000
        });
        writer
            .write_all(serde_json::to_string(&open_response).unwrap().as_bytes())
            .await
            .expect("write open response");
        writer.write_all(b"\n").await.expect("response newline");
        writer.flush().await.expect("flush response");

        let interrupted_poll: Value = serde_json::from_str(
            &lines
                .next_line()
                .await
                .expect("read interrupted poll")
                .expect("interrupted poll request"),
        )
        .expect("poll json");
        assert_eq!(interrupted_poll["operation"], "lsp_poll");
        drop(lines);
        drop(writer);

        let second = listener.accept().await.expect("accept fresh connection");
        let (reader, mut writer) = tokio::io::split(second);
        let mut lines = tokio::io::BufReader::new(reader).lines();
        read_authenticated_handshake(&mut lines, LSP_TEST_TOKEN).await;
        let reconnect: Value = serde_json::from_str(
            &lines
                .next_line()
                .await
                .expect("read reconnect")
                .expect("reconnect request"),
        )
        .expect("reconnect json");
        assert_eq!(reconnect["operation"], "lsp_reconnect");
        assert_eq!(
            reconnect["session"]["credential"],
            "0000000000000000000000000000000000000000000000000000000000000000"
        );
        let reconnect_response = serde_json::json!({
            "protocol": crate::DAEMON_INVOCATION_PROTOCOL,
            "revision": crate::DAEMON_INVOCATION_REVISION,
            "request_id": reconnect["request_id"],
            "status": "lsp_reconnected",
            "session": {
                "session_id": "lsp-reconnect-test",
                "credential": "1111111111111111111111111111111111111111111111111111111111111111"
            }
        });
        writer
            .write_all(
                serde_json::to_string(&reconnect_response)
                    .unwrap()
                    .as_bytes(),
            )
            .await
            .expect("write reconnect response");
        writer.write_all(b"\n").await.expect("response newline");
        writer.flush().await.expect("flush response");

        let resumed_poll: Value = serde_json::from_str(
            &lines
                .next_line()
                .await
                .expect("read resumed poll")
                .expect("resumed poll request"),
        )
        .expect("resumed poll json");
        assert_eq!(resumed_poll["operation"], "lsp_poll");
        assert_eq!(
            resumed_poll["session"]["credential"],
            "1111111111111111111111111111111111111111111111111111111111111111"
        );
        let poll_response = serde_json::json!({
            "protocol": crate::DAEMON_INVOCATION_PROTOCOL,
            "revision": crate::DAEMON_INVOCATION_REVISION,
            "request_id": resumed_poll["request_id"],
            "status": "lsp_frame",
            "frame": "{\"jsonrpc\":\"2.0\",\"method\":\"window/logMessage\",\"params\":{\"type\":3,\"message\":\"resumed\"}}",
            "closed": false
        });
        writer
            .write_all(serde_json::to_string(&poll_response).unwrap().as_bytes())
            .await
            .expect("write resumed frame");
        writer.write_all(b"\n").await.expect("response newline");
        writer.flush().await.expect("flush response");

        let client_frame: Value = serde_json::from_str(
            &lines
                .next_line()
                .await
                .expect("read resumed client frame")
                .expect("resumed client frame request"),
        )
        .expect("client frame json");
        assert_eq!(client_frame["operation"], "lsp_frame");
        assert_eq!(
            client_frame["session"]["credential"],
            "1111111111111111111111111111111111111111111111111111111111111111"
        );
        let frame_response = serde_json::json!({
            "protocol": crate::DAEMON_INVOCATION_PROTOCOL,
            "revision": crate::DAEMON_INVOCATION_REVISION,
            "request_id": client_frame["request_id"],
            "status": "lsp_frame_accepted",
            "backpressured": false,
            "closed": false
        });
        writer
            .write_all(serde_json::to_string(&frame_response).unwrap().as_bytes())
            .await
            .expect("write frame response");
        writer.write_all(b"\n").await.expect("response newline");
        writer.flush().await.expect("flush response");

        let detach: Value = serde_json::from_str(
            &lines
                .next_line()
                .await
                .expect("read detach")
                .expect("detach request"),
        )
        .expect("detach json");
        assert_eq!(detach["operation"], "lsp_detach");
        assert_eq!(
            detach["session"]["credential"],
            "1111111111111111111111111111111111111111111111111111111111111111"
        );
        let detach_response = serde_json::json!({
            "protocol": crate::DAEMON_INVOCATION_PROTOCOL,
            "revision": crate::DAEMON_INVOCATION_REVISION,
            "request_id": detach["request_id"],
            "status": "lsp_detached"
        });
        writer
            .write_all(serde_json::to_string(&detach_response).unwrap().as_bytes())
            .await
            .expect("write detach response");
        writer.write_all(b"\n").await.expect("response newline");
        writer.flush().await.expect("flush response");
    });
    let profile = TempDir::new().expect("profile");
    let handshake = DaemonHandshake {
        project_path: Some(profile.path().to_path_buf()),
        scope_prefix: None,
        timings: false,
        allow_init: false,
        allow_initialize_root_routing: false,
        client_identity: test_client_identity_for(profile.path().to_path_buf()),
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        client_instance_id: "client.reconnect-test".to_owned(),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
        moved_store_adoption: crate::MovedStoreAdoption::Never,
    };
    let invocation = crate::DaemonInvocationClient::new(
        crate::DaemonConnection::new(endpoint, LSP_TEST_TOKEN.to_owned()),
        handshake,
    );
    let (deadline, cancellation) = active_lsp_control("cancel.lsp.reconnect-open");
    let mut session = crate::DaemonLspSessionClient::open(
        invocation,
        env!("CARGO_PKG_VERSION"),
        None,
        Vec::new(),
        deadline,
        cancellation,
    )
    .await
    .expect("open LSP session");

    let (deadline, cancellation) = active_lsp_control("cancel.lsp.interrupted-poll");
    assert!(
        session
            .poll_daemon_frame(deadline, cancellation)
            .await
            .is_err()
    );
    let (deadline, cancellation) = active_lsp_control("cancel.lsp.reconnect");
    session
        .reconnect(deadline, cancellation)
        .await
        .expect("reconnect over fresh daemon connection");
    let (deadline, cancellation) = active_lsp_control("cancel.lsp.resumed-poll");
    assert!(matches!(
        session
            .poll_daemon_frame(deadline, cancellation)
            .await
            .expect("resumed poll"),
        FramePoll::Frame(frame) if frame.ends_with(b"\"resumed\"}}")
    ));
    let (deadline, cancellation) = active_lsp_control("cancel.lsp.resumed-frame");
    assert_eq!(
        session
            .try_send_client_frame(
                "{\"jsonrpc\":\"2.0\",\"method\":\"textDocument/didSave\",\"params\":{}}",
                deadline,
                cancellation,
            )
            .await
            .expect("resumed client frame"),
        FrameSend::Sent
    );
    let (deadline, cancellation) = active_lsp_control("cancel.lsp.resumed-detach");
    session
        .detach(deadline, cancellation)
        .await
        .expect("detach resumed session");
    server.await.expect("server task");
}

/// Reads what a fake daemon receives first: the auth preface carrying
/// `token`, then the handshake.
#[cfg(unix)]
async fn read_authenticated_handshake<R>(
    lines: &mut tokio::io::Lines<R>,
    token: &str,
) -> DaemonHandshake
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let preface = lines
        .next_line()
        .await
        .expect("read auth preface")
        .expect("auth preface line");
    let preface = crate::DaemonAuthPreface::from_line(preface.trim()).expect("auth preface");
    assert!(
        preface.authenticate(token),
        "client must present the current daemon token"
    );
    let handshake = lines
        .next_line()
        .await
        .expect("read handshake")
        .expect("handshake line");
    DaemonHandshake::from_line(&handshake).expect("parse handshake")
}

fn test_client_identity_for(profile_root: std::path::PathBuf) -> crate::DaemonClientIdentity {
    crate::DaemonClientIdentity {
        global_db_path: profile_root.join("global.db"),
        profile_root,
    }
}
