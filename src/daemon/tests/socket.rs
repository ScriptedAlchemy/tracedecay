#[cfg(unix)]
use std::process::Command;

use super::*;
#[cfg(unix)]
use tracedecay_lsp::{FramePoll, FrameSend};

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
async fn dropping_lsp_client_sends_immediate_session_detach() {
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

        let detach: Value = serde_json::from_str(
            &tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
                .await
                .expect("drop must not wait for TTL")
                .expect("read detach")
                .expect("detach request"),
        )
        .expect("detach json");
        assert_eq!(detach["operation"], "lsp_detach");
        assert_eq!(detach["session"]["session_id"], "lsp-drop-test");
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
        client_instance_id: "client.drop-test".to_owned(),
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
    let session = crate::daemon_client::DaemonLspSessionClient::open(
        invocation,
        env!("CARGO_PKG_VERSION"),
        None,
        Vec::new(),
    )
    .await
    .expect("open LSP session");

    drop(session);
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
    let mut session = crate::daemon_client::DaemonLspSessionClient::open(
        invocation,
        env!("CARGO_PKG_VERSION"),
        None,
        Vec::new(),
    )
    .await
    .expect("open LSP session");

    assert!(session.poll_daemon_frame().await.is_err());
    session
        .reconnect()
        .await
        .expect("reconnect over fresh daemon connection");
    assert!(matches!(
        session.poll_daemon_frame().await.expect("resumed poll"),
        FramePoll::Frame(frame) if frame.ends_with(b"\"resumed\"}}")
    ));
    assert_eq!(
        session
            .try_send_client_frame(
                "{\"jsonrpc\":\"2.0\",\"method\":\"textDocument/didSave\",\"params\":{}}"
            )
            .await
            .expect("resumed client frame"),
        FrameSend::Sent
    );
    session.detach().await.expect("detach resumed session");
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
        GitCommitIdentityV1, GitIndexCommitIntentV1, GitIndexPreviewId, GitIndexSigningPolicyV1,
        GitIndexTransactionOperationV1, RepositoryId, UtcMicros, WorktreeId,
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
    let (key, _, _, _) = engine
        .open_project_server(&handshake)
        .await
        .expect("mount project owner");
    std::fs::write(repository.path().join("packet.txt"), "base\nnext\n").expect("changed file");
    git(repository.path(), &["add", "packet.txt"]);
    let project_id =
        tracedecay_domain::ProjectId::new(key.owner.project_id.expect("durable test project id"))
            .expect("typed project id");
    let observed_at = UtcMicros(
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_micros(),
        )
        .unwrap_or(i64::MAX),
    );
    let git_scope = super::super::project_open_owners::resolved_scope_for_project(
        repository.path(),
        &project_id,
    )
    .expect("daemon-authenticated Git scope");
    let snapshot = super::super::git_transactions::capture_exact_snapshot_for_test(
        repository.path(),
        project_id.clone(),
        git_scope.repository_id.clone(),
        git_scope.worktree_id.clone(),
        observed_at,
    )
    .expect("exact socket Git snapshot");
    let identity = GitCommitIdentityV1 {
        name: "TraceDecay Test".to_owned(),
        email: "tracedecay@example.com".to_owned(),
        at: observed_at,
    };
    let request = crate::application_surface::GitPreviewSurfaceRequest {
        operation: GitIndexTransactionOperationV1::CommitIndex,
        preview_id: GitIndexPreviewId::new("preview.socket-git").expect("preview id"),
        repository_snapshot: snapshot,
        selected_hunks: Vec::new(),
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

    let unauthorized_snapshot = super::super::git_transactions::capture_exact_snapshot_for_test(
        repository.path(),
        tracedecay_domain::ProjectId::new("project.unauthorized").unwrap(),
        RepositoryId::new("repository.socket-git").unwrap(),
        WorktreeId::new("worktree.socket-git").unwrap(),
        observed_at,
    )
    .expect("unauthorized socket Git snapshot");
    let unauthorized = super::super::DaemonInvocationRequest::git_preview(
        "request.socket.unauthorized",
        crate::application_surface::GitPreviewSurfaceRequest {
            operation: GitIndexTransactionOperationV1::CommitIndex,
            preview_id: GitIndexPreviewId::new("preview.socket-unauthorized").unwrap(),
            repository_snapshot: unauthorized_snapshot,
            selected_hunks: Vec::new(),
            commit_intent: request.commit_intent.clone(),
        },
        observed_at,
        deadline.clone(),
        cancellation.clone(),
    );
    writer
        .write_all(serde_json::to_string(&unauthorized).unwrap().as_bytes())
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
    let response: Value = serde_json::from_str(
        &lines
            .next_line()
            .await
            .unwrap()
            .expect("authorization response"),
    )
    .unwrap();
    assert_eq!(response["status"], "application_problem");
    assert_eq!(response["problem"]["kind"], "not_found_or_not_authorized");

    let forged_scope_snapshot = super::super::git_transactions::capture_exact_snapshot_for_test(
        repository.path(),
        project_id.clone(),
        RepositoryId::new("repository.caller-selected").unwrap(),
        WorktreeId::new("worktree.caller-selected").unwrap(),
        observed_at,
    )
    .expect("forged-scope socket Git snapshot");
    let forged_scope = super::super::DaemonInvocationRequest::git_preview(
        "request.socket.forged-scope",
        crate::application_surface::GitPreviewSurfaceRequest {
            operation: GitIndexTransactionOperationV1::CommitIndex,
            preview_id: GitIndexPreviewId::new("preview.socket-forged-scope").unwrap(),
            repository_snapshot: forged_scope_snapshot,
            selected_hunks: Vec::new(),
            commit_intent: request.commit_intent.clone(),
        },
        observed_at,
        deadline.clone(),
        cancellation.clone(),
    );
    writer
        .write_all(serde_json::to_string(&forged_scope).unwrap().as_bytes())
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
    let response: Value = serde_json::from_str(
        &lines
            .next_line()
            .await
            .unwrap()
            .expect("forged scope response"),
    )
    .unwrap();
    assert_eq!(response["status"], "application_problem");
    assert_eq!(response["problem"]["kind"], "not_found_or_not_authorized");

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
        tracedecay_domain::GitIndexPreviewDispositionV1::Applicable,
        "{preview_response:#}"
    );

    let apply = crate::application_surface::GitApplySurfaceRequest {
        preview,
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
        // Attempt 1 must commit; attempt 2 must replay that committed receipt.
        // Naming the attempt is what separates an apply that never ran from a
        // replay that lost the first attempt's outcome.
        assert_eq!(
            response["effect"]["receipt"]["outcome"], "completed",
            "attempt {attempt}: {response:#}"
        );
        assert_ne!(response["effect"]["execution"]["started_at"], 1);
    }

    let stale = super::super::DaemonInvocationRequest::git_apply(
        "request.socket.stale",
        crate::application_surface::GitApplySurfaceRequest {
            preview: apply.preview.clone(),
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
            preview_id: GitIndexPreviewId::new("preview.socket-recovery").unwrap(),
            repository_snapshot: super::super::git_transactions::capture_exact_snapshot_for_test(
                repository.path(),
                project_id.clone(),
                git_scope.repository_id.clone(),
                git_scope.worktree_id.clone(),
                observed_at,
            )
            .expect("recovery socket Git snapshot"),
            selected_hunks: Vec::new(),
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
            preview: recovery_preview,
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
    crate::config::save_config(&linked, &config)
        .expect("disable unrelated startup transcript ingestion");

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
