#[cfg(unix)]
use std::process::Command;

use super::*;
#[cfg(unix)]
use tracedecay_lsp::{FramePoll, FrameSend};

#[cfg(unix)]
fn future_lsp_deadline(after: std::time::Duration) -> tracedecay_application::Deadline {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_micros();
    let now = i64::try_from(now).unwrap_or(i64::MAX);
    let delta = i64::try_from(after.as_micros()).unwrap_or(i64::MAX);
    tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(now.saturating_add(delta)))
        .expect("LSP deadline")
}

#[cfg(unix)]
fn active_lsp_control(
    token: &str,
) -> (
    tracedecay_application::Deadline,
    tracedecay_application::CancellationSignal,
) {
    (
        future_lsp_deadline(std::time::Duration::from_secs(2)),
        tracedecay_application::CancellationSignal::active(token).expect("LSP cancellation"),
    )
}

#[cfg(unix)]
fn lsp_test_invocation(
    endpoint: super::super::transport::DaemonEndpoint,
    profile: &TempDir,
    client_instance_id: &str,
) -> crate::daemon_client::DaemonInvocationClient {
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
    };
    crate::daemon_client::DaemonInvocationClient::for_connection_for_test(
        super::super::DaemonConnection {
            endpoint,
            auth_token: None,
            authority_record: None,
        },
        handshake,
    )
}

#[tokio::test]
async fn project_owner_wait_stops_when_the_client_disconnects() {
    struct DropProbe(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    let (mut transport, input, _output) = crate::mcp::transport::ChannelTransport::new();
    drop(input);
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let probe = DropProbe(Arc::clone(&dropped));
    let open = async move {
        let _probe = probe;
        std::future::pending::<crate::errors::Result<()>>().await
    };

    let error = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        super::super::await_project_owner_or_disconnect(&mut transport, open),
    )
    .await
    .expect("disconnect detection must be bounded")
    .expect_err("a never-ready owner must return a warming error");

    assert!(
        super::super::error_message_is_project_warming(&error.to_string()),
        "unexpected owner timeout: {error}"
    );
    assert!(dropped.load(std::sync::atomic::Ordering::Acquire));
}

#[tokio::test]
async fn project_owner_half_close_can_still_receive_a_bounded_result() {
    let (mut transport, input, _output) = crate::mcp::transport::ChannelTransport::new();
    drop(input);
    let result = super::super::await_project_owner_or_disconnect(&mut transport, async {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        Ok::<_, crate::errors::TraceDecayError>(17)
    })
    .await
    .expect("half-closed owner lookup");

    assert_eq!(
        result.map(|(owner, pending)| (owner, pending.len())),
        Some((17, 0))
    );
}

fn closed_feedback_list_request(
    request_id: &str,
    request_handle: &str,
) -> super::super::DaemonInvocationRequest {
    let observed_at = tracedecay_domain::UtcMicros(
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_micros(),
        )
        .unwrap_or(i64::MAX),
    );
    super::super::DaemonInvocationRequest::feedback(
        request_id,
        crate::application_surface::ApplicationSurfaceOperation::FeedbackList,
        request_handle.to_owned(),
        observed_at,
        tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
            observed_at.0.saturating_add(60_000_000),
        ))
        .expect("future deadline"),
        tracedecay_application::CancellationContext::active(format!("cancel.{request_id}"))
            .expect("cancellation"),
    )
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_lsp_client_closes_transport_without_spawning_detach() {
    let (listener, endpoint) = super::super::transport::BrokerListener::bind(
        &super::super::transport::default_loopback_endpoint(),
    )
    .await
    .expect("loopback listener");
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept client");
        let (reader, mut writer) = stream.into_split();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        lines
            .next_line()
            .await
            .expect("read handshake")
            .expect("handshake");
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
            "protocol": super::super::DAEMON_INVOCATION_PROTOCOL,
            "revision": super::super::DAEMON_INVOCATION_REVISION,
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
    let session = crate::daemon_client::DaemonLspSessionClient::open(
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
#[tokio::test]
async fn lsp_gateway_open_carries_control_and_returns_typed_deadline() {
    let (listener, endpoint) = super::super::transport::BrokerListener::bind(
        &super::super::transport::default_loopback_endpoint(),
    )
    .await
    .expect("loopback listener");
    let deadline = future_lsp_deadline(std::time::Duration::from_millis(40));
    let expected_deadline = deadline.expires_at.0;
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept client");
        let (reader, _writer) = stream.into_split();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        lines
            .next_line()
            .await
            .expect("read handshake")
            .expect("handshake");
        let open: Value = serde_json::from_str(
            &lines
                .next_line()
                .await
                .expect("read open")
                .expect("open request"),
        )
        .expect("open json");
        assert_eq!(open["operation"], "lsp_open");
        assert_eq!(open["deadline"]["expires_at"], expected_deadline);
        assert_eq!(open["cancellation"]["state"]["state"], "active");
        lines.next_line().await.expect("client disconnect");
    });
    let profile = TempDir::new().expect("profile");
    let invocation = lsp_test_invocation(endpoint, &profile, "client.lsp-deadline-test");
    let cancellation =
        tracedecay_application::CancellationSignal::active("cancel.lsp.deadline-test")
            .expect("cancellation");

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        crate::daemon_client::DaemonLspSessionClient::open(
            invocation,
            env!("CARGO_PKG_VERSION"),
            None,
            Vec::new(),
            deadline,
            cancellation,
        ),
    )
    .await
    .expect("gateway deadline must terminate the open");

    assert!(matches!(
        result,
        Err(tracedecay_application::InvocationError::DeadlineExceeded)
    ));
    server.await.expect("server task");
}

#[cfg(unix)]
#[tokio::test]
async fn lsp_gateway_open_returns_typed_cancellation() {
    let (listener, endpoint) = super::super::transport::BrokerListener::bind(
        &super::super::transport::default_loopback_endpoint(),
    )
    .await
    .expect("loopback listener");
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept client");
        let (reader, _writer) = stream.into_split();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        lines
            .next_line()
            .await
            .expect("read handshake")
            .expect("handshake");
        lines
            .next_line()
            .await
            .expect("read open")
            .expect("open request");
        lines.next_line().await.expect("client disconnect");
    });
    let profile = TempDir::new().expect("profile");
    let invocation = lsp_test_invocation(endpoint, &profile, "client.lsp-cancel-test");
    let cancellation = tracedecay_application::CancellationSignal::active("cancel.lsp.cancel-test")
        .expect("cancellation");
    let cancellation_request = cancellation.clone();
    let cancel = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        cancellation_request.cancel(tracedecay_application::clock::now_micros());
    });

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        crate::daemon_client::DaemonLspSessionClient::open(
            invocation,
            env!("CARGO_PKG_VERSION"),
            None,
            Vec::new(),
            future_lsp_deadline(std::time::Duration::from_secs(1)),
            cancellation,
        ),
    )
    .await
    .expect("gateway cancellation must terminate the open");

    assert!(matches!(
        result,
        Err(tracedecay_application::InvocationError::Cancelled)
    ));
    cancel.await.expect("cancellation task");
    server.await.expect("server task");
}

#[cfg(unix)]
#[tokio::test]
async fn lsp_gateway_open_returns_typed_unavailable_when_daemon_disconnects() {
    let (listener, endpoint) = super::super::transport::BrokerListener::bind(
        &super::super::transport::default_loopback_endpoint(),
    )
    .await
    .expect("loopback listener");
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept client");
        let (reader, _writer) = stream.into_split();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        lines
            .next_line()
            .await
            .expect("read handshake")
            .expect("handshake");
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
        crate::daemon_client::DaemonLspSessionClient::open(
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
        Err(tracedecay_application::InvocationError::Unavailable)
    ));
    server.await.expect("server task");
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_bridge_session_reconnects_on_a_fresh_socket_and_resumes_frames() {
    let (listener, endpoint) = super::super::transport::BrokerListener::bind(
        &super::super::transport::default_loopback_endpoint(),
    )
    .await
    .expect("loopback listener");
    let server = tokio::spawn(async move {
        let first = listener.accept().await.expect("accept first connection");
        let (reader, mut writer) = first.into_split();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        lines
            .next_line()
            .await
            .expect("read first handshake")
            .expect("first handshake");
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
            "protocol": super::super::DAEMON_INVOCATION_PROTOCOL,
            "revision": super::super::DAEMON_INVOCATION_REVISION,
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
        let (reader, mut writer) = second.into_split();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        lines
            .next_line()
            .await
            .expect("read second handshake")
            .expect("second handshake");
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
            "protocol": super::super::DAEMON_INVOCATION_PROTOCOL,
            "revision": super::super::DAEMON_INVOCATION_REVISION,
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
            "protocol": super::super::DAEMON_INVOCATION_PROTOCOL,
            "revision": super::super::DAEMON_INVOCATION_REVISION,
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
            "protocol": super::super::DAEMON_INVOCATION_PROTOCOL,
            "revision": super::super::DAEMON_INVOCATION_REVISION,
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
            "protocol": super::super::DAEMON_INVOCATION_PROTOCOL,
            "revision": super::super::DAEMON_INVOCATION_REVISION,
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
    };
    let invocation = crate::daemon_client::DaemonInvocationClient::for_connection_for_test(
        super::super::DaemonConnection {
            endpoint,
            auth_token: None,
            authority_record: None,
        },
        handshake,
    );
    let (deadline, cancellation) = active_lsp_control("cancel.lsp.reconnect-open");
    let mut session = crate::daemon_client::DaemonLspSessionClient::open(
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

#[test]
fn tool_json_payload_requires_exactly_one_json_block() {
    let valid = serde_json::json!({
        "content": [
            {"text": "status"},
            {"text": "{\"ok\":true}"}
        ]
    });
    assert_eq!(
        super::super::tool_json_payload(&valid, "test").unwrap(),
        serde_json::json!({"ok": true})
    );

    for (content, expected) in [
        (
            serde_json::json!([{"text": "{\"first\":1}"}, {"text": "[2]"}]),
            "returned multiple JSON payloads",
        ),
        (
            serde_json::json!([{"text": "status"}, {"type": "image"}]),
            "returned no JSON payload",
        ),
    ] {
        let error =
            super::super::tool_json_payload(&serde_json::json!({"content": content}), "test")
                .unwrap_err();
        assert!(error.to_string().contains(expected));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn socket_client_requires_user_storage_scope_without_project() {
    let home = TempDir::new().expect("home");
    let home = home.path().canonicalize().expect("canonical home");
    let client_identity = test_client_identity_for(home.join("client"));
    let engine = test_daemon_engine_for_profile(&client_identity.profile_root);
    let _database_scope =
        enter_test_daemon_database_scope(&client_identity.profile_root, "projectless-socket-test");

    let (client, server) = tokio::net::UnixStream::pair().expect("unix stream pair");
    let server_task = tokio::spawn(super::super::serve_socket_client(server, engine));

    let (reader, mut writer) = client.into_split();
    let handshake = DaemonHandshake {
        client_identity,
        ..test_handshake_defaults()
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
                        "format": "json"
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
        .expect("projectless rejection should not time out")
        .expect("read response")
        .expect("projectless response");
    let response: Value = serde_json::from_str(&line).expect("response json");
    assert_eq!(response["id"], json!(7));
    assert_eq!(
        response["error"]["message"], "projectless LCM dispatch requires storage_scope=user",
        "projectless handshake should return the stable current contract"
    );

    server_task
        .await
        .expect("server task should complete")
        .expect("projectless client shutdown should be clean");
}

#[cfg(unix)]
#[tokio::test]
async fn user_session_read_bypasses_unregistered_project_route() {
    let home = TempDir::new().expect("home");
    let home = home.path().canonicalize().expect("canonical home");
    let client_identity = test_client_identity_for(home.join("client"));
    let engine = test_daemon_engine_for_profile(&client_identity.profile_root);
    let _database_scope =
        enter_test_daemon_database_scope(&client_identity.profile_root, "user-session-read-test");
    let unregistered_project = home.join("unregistered-project");
    std::fs::create_dir_all(&unregistered_project).expect("unregistered project directory");

    let (client, server) = tokio::net::UnixStream::pair().expect("unix stream pair");
    let server_task = tokio::spawn(super::super::serve_socket_client(server, engine));

    let (reader, mut writer) = client.into_split();
    let handshake = DaemonHandshake {
        project_path: Some(unregistered_project),
        client_identity,
        ..test_handshake_defaults()
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
                "id": 8,
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_message_search",
                    "arguments": {
                        "storage_scope": "user",
                        "query": "profile-only evidence",
                        "format": "json"
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
        .expect("user session read should not time out")
        .expect("read response")
        .expect("user session response");
    let response: Value = serde_json::from_str(&line).expect("response json");
    assert_eq!(response["id"], json!(8));
    assert!(response["error"].is_null(), "{response}");
    let payload: Value = serde_json::from_str(
        response["result"]["content"][0]["text"]
            .as_str()
            .expect("message search JSON text"),
    )
    .expect("message search payload");
    assert_eq!(payload["status"], "ok", "{payload}");
    assert_eq!(payload["outcome"], "complete_zero", "{payload}");
    assert_eq!(payload["store_scope"], "profile");

    server_task
        .await
        .expect("server task should complete")
        .expect("user session client shutdown should be clean");
}

#[cfg(unix)]
#[tokio::test]
async fn socket_client_routes_multiple_closed_invocations_without_falling_back_to_mcp() {
    let home = TempDir::new().expect("home");
    let home = home.path().canonicalize().expect("canonical home");
    let client_identity = test_client_identity_for(home.join("client"));
    let engine = test_daemon_engine_for_profile(&client_identity.profile_root);
    let _database_scope = enter_test_daemon_database_scope(
        &client_identity.profile_root,
        "closed-invocation-socket-test",
    );
    let (client, server) = tokio::net::UnixStream::pair().expect("unix stream pair");
    let server_task = tokio::spawn(super::super::serve_socket_client(server, engine));

    let (reader, mut writer) = client.into_split();
    let handshake = DaemonHandshake {
        client_identity,
        ..test_handshake_defaults()
    };
    writer
        .write_all(handshake.to_line().expect("handshake").as_bytes())
        .await
        .expect("write handshake");
    writer.write_all(b"\n").await.expect("newline");
    let mut lines = tokio::io::BufReader::new(reader).lines();
    for (request_id, request_handle) in [("request.1", "handle.1"), ("request.2", "handle.2")] {
        writer
            .write_all(
                serde_json::to_string(&closed_feedback_list_request(request_id, request_handle))
                    .expect("invocation json")
                    .as_bytes(),
            )
            .await
            .expect("write invocation");
        writer.write_all(b"\n").await.expect("newline");

        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect("invocation response should not time out")
            .expect("read invocation response")
            .expect("invocation response");
        let response: serde_json::Value = serde_json::from_str(&line).expect("response json");
        assert_eq!(response["protocol"], "tracedecay.daemon.invocation");
        assert_eq!(response["request_id"], request_id);
        assert_eq!(response["status"], "problem");
        assert_eq!(response["problem"], "unavailable");
        assert!(response.get("jsonrpc").is_none());
    }

    writer.shutdown().await.expect("shutdown writer");

    server_task
        .await
        .expect("server task should complete")
        .expect("invocation should complete cleanly");
}

#[cfg(unix)]
#[tokio::test]
async fn socket_git_preview_apply_replay_and_pre_admission_problems_are_canonical() {
    use std::process::Command;

    use tracedecay_application::{CancellationContext, Deadline, IdempotencyKey};
    use tracedecay_domain::{
        GitCommitIdentityV1, GitIndexCommitIntentV1, GitIndexSigningPolicyV1,
        GitIndexTransactionOperationV1, UtcMicros,
    };

    fn git(root: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .expect("run Git fixture command");
        assert!(status.success(), "git {args:?}");
    }

    let home = TempDir::new().expect("home");
    let home = home.path().canonicalize().expect("canonical home");
    let repository = TempDir::new().expect("repository");
    git(repository.path(), &["init", "--quiet"]);
    git(
        repository.path(),
        &["config", "user.name", "TraceDecay Test"],
    );
    git(
        repository.path(),
        &["config", "user.email", "tracedecay@example.com"],
    );
    std::fs::write(repository.path().join("packet.txt"), "base\n").expect("base file");
    std::fs::create_dir_all(repository.path().join("src")).expect("source dir");
    std::fs::write(repository.path().join("src/main.rs"), "fn main() {}\n").expect("source file");
    git(repository.path(), &["add", "."]);
    git(repository.path(), &["commit", "--quiet", "-m", "base"]);

    let handshake = DaemonHandshake {
        project_path: Some(repository.path().to_path_buf()),
        allow_init: true,
        client_identity: test_client_identity_for(home.join("client")),
        ..test_handshake_defaults()
    };
    let engine = test_daemon_engine_for_profile(&handshake.client_identity.profile_root);
    let _database_scope = enter_test_daemon_database_scope(
        &handshake.client_identity.profile_root,
        "git-socket-test",
    );
    let _ = engine
        .open_project_server(&handshake)
        .await
        .expect("mount project owner");
    std::fs::write(repository.path().join("packet.txt"), "base\nnext\n").expect("changed file");
    git(repository.path(), &["add", "packet.txt"]);
    let observed_at = UtcMicros(
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_micros(),
        )
        .unwrap_or(i64::MAX),
    );
    let identity = GitCommitIdentityV1 {
        name: "TraceDecay Test".to_owned(),
        email: "tracedecay@example.com".to_owned(),
        at: observed_at,
    };
    let request = crate::application_surface::GitPreviewSurfaceRequest {
        operation: GitIndexTransactionOperationV1::CommitIndex,
        preview_input_id: None,
        selected_hunk_digests: Vec::new(),
        commit_intent: Some(
            GitIndexCommitIntentV1::new(
                "socket Git transaction\n".to_owned(),
                identity.clone(),
                identity,
                GitIndexSigningPolicyV1::UnsignedPermitted,
            )
            .expect("commit intent"),
        ),
    };
    let deadline =
        Deadline::new(UtcMicros(observed_at.0.saturating_add(60_000_000))).expect("deadline");
    let cancellation = CancellationContext::active("cancel.socket-git").expect("cancellation");

    let (client, server) = tokio::net::UnixStream::pair().expect("unix stream pair");
    let engine_for_test = engine.clone();
    let server_task = tokio::spawn(super::super::serve_socket_client(server, engine));
    let (reader, mut writer) = client.into_split();
    writer
        .write_all(handshake.to_line().expect("handshake").as_bytes())
        .await
        .expect("write handshake");
    writer.write_all(b"\n").await.expect("handshake newline");
    let mut lines = tokio::io::BufReader::new(reader).lines();

    let preview_request = super::super::DaemonInvocationRequest::git_preview(
        "request.socket.preview",
        request,
        observed_at,
        deadline.clone(),
        cancellation.clone(),
    );
    writer
        .write_all(serde_json::to_string(&preview_request).unwrap().as_bytes())
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
    let preview_line = lines.next_line().await.unwrap().expect("preview response");
    let preview_response: Value = serde_json::from_str(&preview_line).expect("preview JSON");
    assert_eq!(
        preview_response["status"], "git_preview",
        "{preview_response:#}"
    );
    let preview: tracedecay_domain::GitIndexPreviewV1 =
        serde_json::from_value(preview_response["preview"]["payload"].clone())
            .expect("typed immutable preview");
    // An unsupported preview is never cached for apply, so a disposition
    // regression here surfaces as an unexplained abort several requests later
    // rather than as the blocker the daemon actually found.
    assert_eq!(
        preview.disposition,
        tracedecay_domain::GitIndexPreviewDispositionV1::Unsupported(
            tracedecay_domain::GitIndexUnsupportedStateV1::AtomicRefNamespaceUnavailable
        ),
        "{preview_response:#}"
    );

    let apply = crate::application_surface::GitApplySurfaceRequest {
        preview_id: preview.preview_id.clone(),
        preview_digest: preview.preview_digest.clone(),
        idempotency_key: IdempotencyKey::new("idempotency.socket-git").expect("idempotency"),
    };
    let apply_request = super::super::DaemonInvocationRequest::git_apply(
        "request.socket.apply",
        apply.clone(),
        UtcMicros(1),
        deadline.clone(),
        cancellation,
    );
    let apply_request_json = serde_json::to_string(&apply_request).unwrap();
    for attempt in 1..=2 {
        let expected_request_id = "request.socket.apply";
        writer
            .write_all(apply_request_json.as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
        let line = lines.next_line().await.unwrap().expect("apply response");
        let response: Value = serde_json::from_str(&line).expect("apply JSON");
        assert_eq!(response["request_id"], expected_request_id);
        assert_eq!(
            response["status"], "git_apply",
            "attempt {attempt}: {response:#}"
        );
        // Ref publication is typed-unavailable, so attempt 1 must abort with
        // no repository change and attempt 2 must replay that same terminal
        // failed receipt rather than re-entering native Git.
        assert_eq!(
            response["effect"]["receipt"]["outcome"], "failed",
            "attempt {attempt}: {response:#}"
        );
        assert_eq!(
            response["effect"]["payload"]["outcome"], "aborted_no_change",
            "attempt {attempt}: {response:#}"
        );
        assert_eq!(
            response["effect"]["payload"]["created_commit"],
            Value::Null,
            "attempt {attempt}: {response:#}"
        );
        assert_ne!(response["effect"]["execution"]["started_at"], 1);
    }

    let stale = super::super::DaemonInvocationRequest::git_apply(
        "request.socket.stale",
        crate::application_surface::GitApplySurfaceRequest {
            preview_id: apply.preview_id.clone(),
            preview_digest: apply.preview_digest.clone(),
            idempotency_key: IdempotencyKey::new("idempotency.socket-stale").unwrap(),
        },
        observed_at,
        deadline.clone(),
        CancellationContext::active("cancel.socket-stale").unwrap(),
    );
    writer
        .write_all(serde_json::to_string(&stale).unwrap().as_bytes())
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
    let response: Value =
        serde_json::from_str(&lines.next_line().await.unwrap().expect("stale response")).unwrap();
    assert_eq!(response["status"], "git_apply", "{response:#}");
    assert_eq!(
        response["effect"]["payload"]["outcome"],
        "aborted_no_change"
    );

    std::fs::write(
        repository.path().join("packet.txt"),
        "base\nnext\nrecovery\n",
    )
    .expect("recovery fixture change");
    git(repository.path(), &["add", "packet.txt"]);
    let recovery_preview_request = super::super::DaemonInvocationRequest::git_preview(
        "request.socket.recovery-preview",
        crate::application_surface::GitPreviewSurfaceRequest {
            operation: GitIndexTransactionOperationV1::CommitIndex,
            preview_input_id: None,
            selected_hunk_digests: Vec::new(),
            commit_intent: Some(
                GitIndexCommitIntentV1::new(
                    "socket recovery fence\n".to_owned(),
                    GitCommitIdentityV1 {
                        name: "TraceDecay Test".to_owned(),
                        email: "tracedecay@example.com".to_owned(),
                        at: observed_at,
                    },
                    GitCommitIdentityV1 {
                        name: "TraceDecay Test".to_owned(),
                        email: "tracedecay@example.com".to_owned(),
                        at: observed_at,
                    },
                    GitIndexSigningPolicyV1::UnsignedPermitted,
                )
                .unwrap(),
            ),
        },
        observed_at,
        deadline.clone(),
        CancellationContext::active("cancel.socket-recovery-preview").unwrap(),
    );
    writer
        .write_all(
            serde_json::to_string(&recovery_preview_request)
                .unwrap()
                .as_bytes(),
        )
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
    let response: Value = serde_json::from_str(
        &lines
            .next_line()
            .await
            .unwrap()
            .expect("recovery preview response"),
    )
    .unwrap();
    assert_eq!(response["status"], "git_preview", "{response:#}");
    let recovery_preview: tracedecay_domain::GitIndexPreviewV1 =
        serde_json::from_value(response["preview"]["payload"].clone()).unwrap();

    engine_for_test
        .store_administration
        .git_index_transaction_services()
        .quarantine_preview_for_test(repository.path(), &recovery_preview, observed_at)
        .await
        .unwrap();
    let recovery_blocked = super::super::DaemonInvocationRequest::git_apply(
        "request.socket.recovery-blocked",
        crate::application_surface::GitApplySurfaceRequest {
            preview_id: recovery_preview.preview_id,
            preview_digest: recovery_preview.preview_digest,
            idempotency_key: IdempotencyKey::new("idempotency.socket-recovery").unwrap(),
        },
        observed_at,
        deadline.clone(),
        CancellationContext::active("cancel.socket-recovery").unwrap(),
    );
    writer
        .write_all(serde_json::to_string(&recovery_blocked).unwrap().as_bytes())
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
    let response: Value = serde_json::from_str(
        &lines
            .next_line()
            .await
            .unwrap()
            .expect("recovery-blocked response"),
    )
    .unwrap();
    assert_eq!(response["status"], "application_problem", "{response:#}");
    assert_eq!(response["problem"]["kind"], "unavailable");
    assert_eq!(
        response["problem"]["diagnostic"]["code"],
        "git_index.recovery_required"
    );

    let cancelled = super::super::DaemonInvocationRequest::git_apply(
        "request.socket.cancelled",
        apply.clone(),
        observed_at,
        deadline,
        CancellationContext::cancelled("cancel.socket-cancelled", observed_at).unwrap(),
    );
    writer
        .write_all(serde_json::to_string(&cancelled).unwrap().as_bytes())
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
    let response: Value = serde_json::from_str(
        &lines
            .next_line()
            .await
            .unwrap()
            .expect("cancelled response"),
    )
    .unwrap();
    assert_eq!(response["status"], "application_problem");
    assert_eq!(response["problem"]["kind"], "cancelled");

    let timed_out = super::super::DaemonInvocationRequest::git_apply(
        "request.socket.timed-out",
        apply,
        observed_at,
        Deadline::new(UtcMicros(observed_at.0)).unwrap(),
        CancellationContext::active("cancel.socket-timeout").unwrap(),
    );
    writer
        .write_all(serde_json::to_string(&timed_out).unwrap().as_bytes())
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
    let response: Value =
        serde_json::from_str(&lines.next_line().await.unwrap().expect("timeout response")).unwrap();
    assert_eq!(response["status"], "application_problem");
    assert_eq!(response["problem"]["kind"], "timed_out");

    writer.shutdown().await.expect("shutdown writer");
    server_task.await.unwrap().expect("socket server");
}

#[tokio::test]
async fn portable_broker_routes_multiple_closed_invocations_without_falling_back_to_mcp() {
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    let home = TempDir::new().expect("home");
    let home = home.path().canonicalize().expect("canonical home");
    let client_identity = test_client_identity_for(home.join("client"));
    let store_administration = test_store_administration_for_profile(&client_identity.profile_root);
    let _database_scope = enter_test_daemon_database_scope(
        &client_identity.profile_root,
        "portable-closed-invocation-test",
    );
    let (listener, endpoint) = super::super::transport::BrokerListener::bind(
        &super::super::transport::default_loopback_endpoint(),
    )
    .await
    .expect("loopback listener");
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept client");
        let lifecycle = DaemonLifecycle::default();
        super::super::serve_windows_broker_client(
            stream,
            TOKEN,
            &lifecycle,
            store_administration,
            Arc::new(tokio::sync::Mutex::new(
                super::super::ProjectOpenGates::default(),
            )),
            None,
        )
        .await
    });

    let stream = super::super::transport::BrokerStream::connect(&endpoint)
        .await
        .expect("connect client");
    let (reader, mut writer) = stream.into_split();
    let preface = super::super::transport::DaemonAuthPreface::new(TOKEN)
        .to_line()
        .expect("auth preface");
    let handshake = DaemonHandshake {
        client_identity,
        ..test_handshake_defaults()
    };
    writer.write_all(preface.as_bytes()).await.expect("preface");
    writer.write_all(b"\n").await.expect("preface newline");
    writer
        .write_all(handshake.to_line().expect("handshake").as_bytes())
        .await
        .expect("write handshake");
    writer.write_all(b"\n").await.expect("handshake newline");
    let mut lines = tokio::io::BufReader::new(reader).lines();
    for (request_id, request_handle) in [("request.1", "handle.1"), ("request.2", "handle.2")] {
        writer
            .write_all(
                serde_json::to_string(&closed_feedback_list_request(request_id, request_handle))
                    .expect("invocation json")
                    .as_bytes(),
            )
            .await
            .expect("write invocation");
        writer.write_all(b"\n").await.expect("invocation newline");

        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect("invocation response should not time out")
            .expect("read invocation response")
            .expect("invocation response");
        let response: serde_json::Value = serde_json::from_str(&line).expect("response json");
        assert_eq!(response["protocol"], "tracedecay.daemon.invocation");
        assert_eq!(response["request_id"], request_id);
        assert_eq!(response["status"], "problem");
        assert_eq!(response["problem"], "unavailable");
        assert!(response.get("jsonrpc").is_none());
    }
    writer.shutdown().await.expect("shutdown writer");

    server
        .await
        .expect("server task should complete")
        .expect("invocation should complete cleanly");
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_linked_worktree_route_repairs_primary_identity_and_keeps_alias() {
    let dir = TempDir::new().expect("temp dir");
    let root = dir.path().canonicalize().expect("canonical temp dir");
    let primary = root.join("primary");
    let linked = root.join("linked");
    let profile_root = root.join("profile");
    std::fs::create_dir_all(&primary).expect("primary dir");
    let git = |cwd: &std::path::Path, args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "TraceDecay Test")
            .env("GIT_AUTHOR_EMAIL", "test@tracedecay.local")
            .env("GIT_COMMITTER_NAME", "TraceDecay Test")
            .env("GIT_COMMITTER_EMAIL", "test@tracedecay.local")
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&primary, &["init", "-b", "main", "--quiet"]);
    std::fs::create_dir_all(primary.join("src")).expect("fixture source dir");
    std::fs::write(primary.join("src/main.rs"), "fn main() {}\n").expect("fixture source");
    std::fs::write(primary.join("README.md"), "linked worktree route\n").expect("fixture");
    git(&primary, &["add", "."]);
    git(&primary, &["commit", "-m", "fixture", "--quiet"]);
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-b",
            "feature/linked-route",
            linked.to_str().expect("utf-8 linked path"),
            "HEAD",
        ],
    );

    let client_identity = test_client_identity_for(profile_root.clone());
    initialize_test_project(&primary, &client_identity).await;
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "linked-worktree-route-test")
            .expect("daemon database scope");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let project_id = crate::storage::read_enrollment_marker(&primary)
        .expect("read primary enrollment")
        .expect("primary enrollment")
        .project_id;
    let registry = engine
        .store_administration
        .registered_profile_database()
        .await
        .expect("daemon profile registry");
    let mut config = crate::config::load_config(&linked).expect("load project config");
    config.sync.session_start_sync = false;
    crate::config::save_config(&linked, &config).expect("disable unrelated startup catch-up");

    registry
        .upsert_code_project(
            &project_id,
            &linked,
            crate::worktree::git_common_dir(&linked).as_deref(),
            None,
            Some("main"),
        )
        .await
        .expect("seed stale linked canonical root");
    drop(registry);

    let handshake = DaemonHandshake {
        project_path: Some(linked.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let linked_server = engine
        .project_server(&handshake)
        .await
        .expect("linked worktree must open before the primary route");
    let linked_graph = linked_server.cg().await;
    engine
        .store_administration
        .registered_project_session_database(&primary, linked_graph.store_layout())
        .await
        .expect("primary alias must reuse the linked route's typed authority");

    let registry = engine
        .store_administration
        .registered_profile_database()
        .await
        .expect("daemon profile registry");
    let context = registry
        .project_registry_context_by_id(&project_id)
        .await
        .expect("registry context")
        .expect("linked project registry context present");
    assert_eq!(
        context.project.canonical_root,
        crate::application::host_admission::HostAdmissionTestRuntimeV1::canonical_project_key(
            &primary
        )
    );
    assert!(context.aliases.iter().any(|alias| {
        alias.alias_path
            == crate::application::host_admission::HostAdmissionTestRuntimeV1::canonical_project_key(
                &linked,
            )
    }));
}

#[test]
fn unsupported_daemon_transport_never_falls_back_to_local_sqlite() {
    assert!(super::super::proxy_required_by_platform(false, false));
    assert!(super::super::proxy_required_by_platform(false, true));
    assert!(!super::super::proxy_required_by_platform(true, false));
}
