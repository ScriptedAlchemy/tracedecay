use super::*;
#[cfg(unix)]
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

/// How long a half-closed one-shot client may wait for its single response.
///
/// This bound proves the daemon cannot pin a half-closed connection; it is not
/// a latency budget. It used to be 2s, which the whole-suite run exceeded
/// whenever several of these tests opened their own cold profile runtimes at
/// once, a measured 500ms+ per `DaemonSessionRuntimeRegistryV1::open` under
/// six-way contention, on top of the route open each request still needs. That
/// made an environment-speed reading look like a daemon hang. The portable
/// broker tests use it too, so it is not gated on unix.
const HALF_CLOSE_ROUND_TRIP_BOUND: std::time::Duration = std::time::Duration::from_secs(20);

#[tokio::test]
async fn project_owner_wait_stops_when_the_client_disconnects() {
    struct DropProbe(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    let (mut transport, input, _output) = tracedecay_mcp::transport::ChannelTransport::new();
    drop(input);
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let probe = DropProbe(Arc::clone(&dropped));
    let open = async move {
        let _probe = probe;
        std::future::pending::<tracedecay_domain::errors::Result<()>>().await
    };

    let error = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        super::super::await_project_owner_or_disconnect(&mut transport, open),
    )
    .await
    .expect("disconnect detection must be bounded")
    .expect_err("a never-ready owner must return a warming error");

    assert!(
        super::super::error_is_project_warming(&error),
        "unexpected owner timeout: {error}"
    );
    assert!(dropped.load(std::sync::atomic::Ordering::Acquire));
}

#[tokio::test]
async fn project_owner_half_close_can_still_receive_a_bounded_result() {
    let (mut transport, input, _output) = tracedecay_mcp::transport::ChannelTransport::new();
    drop(input);
    let result = super::super::await_project_owner_or_disconnect(&mut transport, async {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        Ok::<_, tracedecay_domain::errors::TraceDecayError>(17)
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
        ApplicationSurfaceOperation::FeedbackList,
        request_handle.to_owned(),
        observed_at,
        tracedecay_contracts::Deadline::new(tracedecay_domain::UtcMicros(
            observed_at.0.saturating_add(60_000_000),
        ))
        .expect("future deadline"),
        tracedecay_contracts::CancellationContext::active(format!("cancel.{request_id}"))
            .expect("cancellation"),
    )
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
    prewarm_test_profile_runtime(&engine.store_administration).await;

    let handshake = DaemonHandshake {
        client_identity,
        ..test_handshake_defaults()
    };
    let (reader, mut writer, server_task) =
        super::handshake_authenticated_test_client(engine, &handshake).await;
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
    let line = tokio::time::timeout(HALF_CLOSE_ROUND_TRIP_BOUND, lines.next_line())
        .await
        .expect("projectless rejection should not time out")
        .expect("read response")
        .expect("projectless response");
    let response: Value = serde_json::from_str(&line).expect("response json");
    assert_eq!(response["id"], json!(7));
    assert_eq!(
        response["error"]["message"],
        "projectless retained dispatch requires an explicit user scope",
        "projectless handshake should return the stable current contract. The message \
         names no parameter because the admission check reads `storage_scope` on the \
         retained path and `memory_scope` otherwise, so naming only one would be wrong \
         for half the callers it rejects."
    );

    server_task
        .await
        .expect("server task should complete")
        .expect("projectless client shutdown should be clean");
}

/// A projectless connection reads the profile's real project registry: an
/// enrolled-nothing profile answers `ok` with an empty listing, the same
/// answer `tracedecay projects list` gives, not a claim that the registry is
/// absent.
#[cfg(unix)]
#[tokio::test]
async fn projectless_project_list_reads_the_empty_profile_registry() {
    let home = TempDir::new().expect("home");
    let home = home.path().canonicalize().expect("canonical home");
    let client_identity = test_client_identity_for(home.join("client"));
    let engine = test_daemon_engine_for_profile(&client_identity.profile_root);
    let _database_scope = enter_test_daemon_database_scope(
        &client_identity.profile_root,
        "projectless-registry-test",
    );
    prewarm_test_profile_runtime(&engine.store_administration).await;

    let handshake = DaemonHandshake {
        client_identity,
        ..test_handshake_defaults()
    };
    let (reader, mut writer, server_task) =
        super::handshake_authenticated_test_client(engine, &handshake).await;
    writer
        .write_all(
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": 8,
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_project_list",
                    "arguments": {"format": "json", "limit": 5}
                }
            }))
            .expect("tools/call json")
            .as_bytes(),
        )
        .await
        .expect("write tools/call");
    writer.write_all(b"\n").await.expect("newline");
    writer
        .write_all(
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_project_list",
                    "arguments": {"format": "json", "limt": 5}
                }
            }))
            .expect("tools/call json")
            .as_bytes(),
        )
        .await
        .expect("write refused tools/call");
    writer.write_all(b"\n").await.expect("newline");
    writer.shutdown().await.expect("shutdown writer");

    let mut lines = tokio::io::BufReader::new(reader).lines();
    let line = tokio::time::timeout(HALF_CLOSE_ROUND_TRIP_BOUND, lines.next_line())
        .await
        .expect("projectless project list should not time out")
        .expect("read response")
        .expect("projectless response");
    let response: Value = serde_json::from_str(&line).expect("response json");
    assert_eq!(response["id"], json!(8));
    let payload = super::super::tool_json_payload(&response["result"], "tracedecay_project_list")
        .expect("typed project-list payload");
    assert_eq!(payload["status"], "ok", "payload: {payload}");
    assert_eq!(payload["projects"], json!([]));
    assert_eq!(payload["summary"]["project_count"], json!(0));
    assert_eq!(payload["truncated"], json!(false));
    assert!(
        payload["registry_path"]
            .as_str()
            .is_some_and(|path| path.ends_with("global.db")),
        "listing must name the registry it read: {payload}"
    );

    // A request outside the typed contract is the owner's refusal, flagged as
    // a tool error rather than served as a listing. Its record travels as MCP
    // structured content, as on a project connection, never as a result
    // member MCP clients drop.
    let line = tokio::time::timeout(HALF_CLOSE_ROUND_TRIP_BOUND, lines.next_line())
        .await
        .expect("refused project list should not time out")
        .expect("read refusal")
        .expect("projectless refusal");
    let refused: Value = serde_json::from_str(&line).expect("refusal json");
    let result = &refused["result"];
    let problem = &result["structuredContent"]["problem"];
    assert_eq!(
        (
            &refused["id"],
            &result["isError"],
            result.get("problem"),
            &problem["kind"],
            &problem["code"],
            &problem["message"],
        ),
        (
            &json!(9),
            &json!(true),
            None,
            &json!("invalid_request"),
            &json!("application.surface.invalid_request"),
            &json!(
                "invalid arguments for tracedecay_project_list: unknown field `limt`, expected `limit`"
            ),
        ),
        "{refused}"
    );

    server_task
        .await
        .expect("server task should complete")
        .expect("projectless client shutdown should be clean");
}

/// Every JSON pointer at which a tool result carries a `problem` member.
#[cfg(unix)]
fn problem_placement(value: &Value, pointer: &str, found: &mut Vec<String>) {
    if let Value::Object(members) = value {
        for (key, member) in members {
            let path = format!("{pointer}/{key}");
            if key == "problem" {
                found.push(path.clone());
            }
            problem_placement(member, &path, found);
        }
    }
}

#[cfg(unix)]
fn problem_placements(result: &Value) -> Vec<String> {
    let mut found = Vec::new();
    problem_placement(result, "", &mut found);
    found
}

/// A projectless refusal and a project-route refusal carry the typed problem
/// record at the same single location, so one reader serves every route.
#[cfg(unix)]
#[tokio::test]
async fn projectless_and_project_route_refusals_place_the_problem_identically() {
    let home = TempDir::new().expect("home");
    let home = home.path().canonicalize().expect("canonical home");
    let client_identity = test_client_identity_for(home.join("client"));
    let engine = test_daemon_engine_for_profile(&client_identity.profile_root);
    let _database_scope =
        enter_test_daemon_database_scope(&client_identity.profile_root, "problem-placement-test");
    prewarm_test_profile_runtime(&engine.store_administration).await;

    let handshake = DaemonHandshake {
        client_identity,
        ..test_handshake_defaults()
    };
    let (reader, mut writer, server_task) =
        super::handshake_authenticated_test_client(engine, &handshake).await;
    writer
        .write_all(
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_project_list",
                    "arguments": {"format": "json", "limt": 5}
                }
            }))
            .expect("tools/call json")
            .as_bytes(),
        )
        .await
        .expect("write refused tools/call");
    writer.write_all(b"\n").await.expect("newline");
    writer.shutdown().await.expect("shutdown writer");
    let line = tokio::io::BufReader::new(reader)
        .lines()
        .next_line()
        .await
        .expect("read refusal")
        .expect("projectless refusal");
    server_task
        .await
        .expect("server task should complete")
        .expect("projectless client shutdown should be clean");
    let projectless: Value = serde_json::from_str(&line).expect("refusal json");
    let projectless = &projectless["result"];

    let request: tracedecay_mcp::JsonRpcRequest = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "tracedecay_storage_status", "arguments": {} },
    }))
    .expect("project tools/call request");
    let project = super::super::project_open_handshake::tool_call_open_refusal_response(
        &request,
        "connection.problem-placement",
        &tracedecay_domain::errors::TraceDecayError::reset_required(
            "project store",
            "schema v26 is incompatible",
        ),
    )
    .expect("a project-route refusal")
    .result
    .expect("project-route tool result");

    assert_eq!(
        (&projectless["isError"], &project["isError"]),
        (&json!(true), &json!(true))
    );
    assert_eq!(
        serde_json::to_string(&problem_placements(projectless)).expect("placement bytes"),
        serde_json::to_string(&problem_placements(&project)).expect("placement bytes"),
        "projectless {projectless}\nproject {project}"
    );
    assert_eq!(
        problem_placements(&project),
        ["/structuredContent/problem"],
        "{project}"
    );
}

/// A fresh MCP host discovers tools through `tools/list` before `tools/call`.
/// After a projectless session is admitted, `tools/list` must advertise the
/// registry reads that dispatcher can serve and must not advertise
/// project-mounted graph tools that still refuse without a project.
#[cfg(unix)]
#[tokio::test]
async fn projectless_tools_list_advertises_registry_tools() {
    let home = TempDir::new().expect("home");
    let home = home.path().canonicalize().expect("canonical home");
    let client_identity = test_client_identity_for(home.join("client"));
    let engine = test_daemon_engine_for_profile(&client_identity.profile_root);
    let _database_scope = enter_test_daemon_database_scope(
        &client_identity.profile_root,
        "projectless-tools-list-test",
    );
    prewarm_test_profile_runtime(&engine.store_administration).await;

    let handshake = DaemonHandshake {
        client_identity,
        ..test_handshake_defaults()
    };
    let (reader, mut writer, server_task) =
        super::handshake_authenticated_test_client(engine, &handshake).await;
    // First request must not be bootstrap-handled (`initialize` / `tools/list`),
    // or the connection never enters the projectless session loop Codex named.
    writer
        .write_all(
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": 8,
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_project_list",
                    "arguments": {"format": "json", "limit": 5}
                }
            }))
            .expect("tools/call json")
            .as_bytes(),
        )
        .await
        .expect("write tools/call");
    writer.write_all(b"\n").await.expect("newline");
    writer
        .write_all(
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "tools/list"
            }))
            .expect("tools/list json")
            .as_bytes(),
        )
        .await
        .expect("write tools/list");
    writer.write_all(b"\n").await.expect("newline");
    writer.shutdown().await.expect("shutdown writer");

    let mut lines = tokio::io::BufReader::new(reader).lines();
    let call_line = tokio::time::timeout(HALF_CLOSE_ROUND_TRIP_BOUND, lines.next_line())
        .await
        .expect("projectless tools/call should not time out")
        .expect("read tools/call response")
        .expect("tools/call response");
    let call_response: Value = serde_json::from_str(&call_line).expect("tools/call json");
    assert_eq!(call_response["id"], json!(8));
    assert!(
        call_response.get("error").is_none(),
        "projectless tools/call must admit the session: {call_response}"
    );

    let list_line = tokio::time::timeout(HALF_CLOSE_ROUND_TRIP_BOUND, lines.next_line())
        .await
        .expect("projectless tools/list should not time out")
        .expect("read tools/list response")
        .expect("tools/list response");
    let response: Value = serde_json::from_str(&list_line).expect("tools/list json");
    assert_eq!(response["id"], json!(9));
    assert!(
        response.get("error").is_none(),
        "projectless tools/list must not return MethodNotFound: {response}"
    );
    let tools = response["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list must carry a tool array: {response}"));
    let names: std::collections::BTreeSet<&str> = tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    for required in [
        "tracedecay_project_list",
        "tracedecay_project_search",
        "tracedecay_project_context",
    ] {
        assert!(
            names.contains(required),
            "projectless tools/list must advertise {required}; got {names:?}"
        );
    }
    assert!(
        !names.contains("tracedecay_search"),
        "projectless tools/list must not advertise project-mounted graph tools: {names:?}"
    );
    let configuration: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| name.starts_with("tracedecay_configuration_"))
        .collect();
    assert_eq!(
        configuration,
        [
            "tracedecay_configuration_batch",
            "tracedecay_configuration_get",
            "tracedecay_configuration_set",
            "tracedecay_configuration_unset",
        ],
        "projectless tools/list advertises only the configuration tools a user setting \
         can address"
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
    let endpoint = tracedecay_daemon_protocol::DaemonEndpoint::Unix(
        client_identity.profile_root.join("daemon.sock"),
    );
    let daemon_authority = tracedecay_daemon_identity::authority::DaemonAuthority::acquire(
        &client_identity.profile_root,
        &endpoint,
        "user-session-read-test",
    )
    .expect("daemon authority");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &client_identity.profile_root,
        daemon_authority.record().epoch,
        &daemon_authority.record().process_run_id,
    )
    .expect("daemon database scope");
    let engine = test_daemon_engine_for_profile(&client_identity.profile_root);
    prewarm_test_profile_runtime(&engine.store_administration).await;
    let unregistered_project = home.join("unregistered-project");
    std::fs::create_dir_all(&unregistered_project).expect("unregistered project directory");

    let handshake = DaemonHandshake {
        project_path: Some(unregistered_project),
        client_identity,
        ..test_handshake_defaults()
    };
    let (reader, mut writer, server_task) =
        super::handshake_authenticated_test_client(engine, &handshake).await;
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
    let line = tokio::time::timeout(HALF_CLOSE_ROUND_TRIP_BOUND, lines.next_line())
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
    let message_search = &payload["outcome"]["value"]["payload"];
    assert_eq!(message_search["status"], "unavailable", "{payload}");
    assert_eq!(message_search["outcome"], "unavailable", "{payload}");
    assert_eq!(message_search["store_scope"], "profile");

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
    prewarm_test_profile_runtime(&engine.store_administration).await;
    let handshake = DaemonHandshake {
        client_identity,
        ..test_handshake_defaults()
    };
    let (reader, mut writer, server_task) =
        super::handshake_authenticated_test_client(engine, &handshake).await;
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

        let line = tokio::time::timeout(HALF_CLOSE_ROUND_TRIP_BOUND, lines.next_line())
            .await
            .expect("invocation response should not time out")
            .expect("read invocation response")
            .expect("invocation response");
        let response: serde_json::Value = serde_json::from_str(&line).expect("response json");
        assert_eq!(response["protocol"], "tracedecay.daemon.invocation");
        assert_eq!(response["request_id"], request_id);
        // The handshake names no project, so the project-scoped read is a
        // terminal refusal the client must correct, not a retryable outage.
        assert_eq!(response["status"], "application_problem", "{response:#}");
        assert_eq!(response["problem"]["kind"], "invalid_request");
        assert_eq!(
            response["problem"]["diagnostic"]["code"],
            "project_required"
        );
        assert!(response.get("jsonrpc").is_none());
    }

    writer.shutdown().await.expect("shutdown writer");

    server_task
        .await
        .expect("server task should complete")
        .expect("invocation should complete cleanly");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socket_git_preview_apply_replay_and_pre_admission_problems_are_canonical() {
    use tracedecay_contracts::git::GitReadRequestV1;
    use tracedecay_contracts::{CancellationContext, Deadline, IdempotencyKey};
    use tracedecay_daemon_protocol::surface::GitReadSurfaceRequest;
    use tracedecay_domain::{GitIndexTransactionOperationV1, UtcMicros};

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
    let observed_at = UtcMicros(
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_micros(),
        )
        .unwrap_or(i64::MAX),
    );
    let deadline =
        Deadline::new(UtcMicros(observed_at.0.saturating_add(60_000_000))).expect("deadline");
    let cancellation = CancellationContext::active("cancel.socket-git").expect("cancellation");

    let engine_for_test = engine.clone();
    let (reader, mut writer, server_task) =
        super::handshake_authenticated_test_client(engine, &handshake).await;
    let mut lines = tokio::io::BufReader::new(reader).lines();

    async fn read_hunks(
        writer: &mut tokio::net::unix::OwnedWriteHalf,
        lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>>,
        request_id: &str,
        observed_at: UtcMicros,
        deadline: &Deadline,
        cancellation: &CancellationContext,
    ) -> tracedecay_contracts::git::GitHunkPreviewInputV1 {
        let read_request = super::super::DaemonInvocationRequest::git_read(
            request_id,
            ApplicationSurfaceOperation::GitHunks,
            GitReadSurfaceRequest {
                request: GitReadRequestV1::Hunks {
                    scope: tracedecay_domain::GitDiffScopeV1::WorkingTree,
                    daemon_binding: None,
                },
                max_entries: 1_000,
                max_bytes: 4 * 1024 * 1024,
            },
            observed_at,
            deadline.clone(),
            cancellation.clone(),
        );
        writer
            .write_all(serde_json::to_string(&read_request).unwrap().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
        let line = lines
            .next_line()
            .await
            .unwrap()
            .expect("hunks read response");
        let response: Value = serde_json::from_str(&line).expect("read JSON");
        assert_eq!(response["status"], "git_read", "{response:#}");
        serde_json::from_value(response["result"]["payload"]["result"]["value"].clone())
            .expect("typed hunk preview input")
    }

    let hunks_input = read_hunks(
        &mut writer,
        &mut lines,
        "request.socket.hunks",
        observed_at,
        &deadline,
        &cancellation,
    )
    .await;
    assert_ne!(hunks_input.hunks.len(), 0);
    let request = tracedecay_daemon_service::application_surface::GitPreviewSurfaceRequest {
        operation: GitIndexTransactionOperationV1::StageHunks,
        preview_input_id: Some(hunks_input.preview_input_id.clone()),
        selected_hunk_digests: hunks_input
            .hunks
            .iter()
            .map(|entry| entry.digest.clone())
            .collect(),
    };

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
    assert_eq!(
        preview.disposition,
        tracedecay_domain::GitIndexPreviewDispositionV1::Applicable,
        "{preview_response:#}"
    );

    let apply = tracedecay_daemon_service::application_surface::GitApplySurfaceRequest {
        preview_id: preview.preview_id.clone(),
        preview_digest: preview.preview_digest.clone(),
        idempotency_key: IdempotencyKey::new("idempotency.socket-git").expect("idempotency"),
    };
    let apply_request = super::super::DaemonInvocationRequest::git_apply(
        "request.socket.apply",
        apply.clone(),
        UtcMicros(1),
        deadline.clone(),
        cancellation.clone(),
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
        // Attempt 1 commits the staged hunk; attempt 2 must replay that same
        // terminal receipt rather than re-entering native Git.
        assert_eq!(
            response["effect"]["receipt"]["outcome"], "completed",
            "attempt {attempt}: {response:#}"
        );
        assert_eq!(
            response["effect"]["payload"]["outcome"], "committed",
            "attempt {attempt}: {response:#}"
        );
        assert_ne!(response["effect"]["execution"]["started_at"], 1);
    }

    let stale = super::super::DaemonInvocationRequest::git_apply(
        "request.socket.stale",
        tracedecay_daemon_service::application_surface::GitApplySurfaceRequest {
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
    // The repository moved past the preview snapshot, so a new idempotency key
    // reaches a terminal no-change receipt rather than a second mutation.
    assert_eq!(
        response["effect"]["payload"]["outcome"],
        "aborted_no_change"
    );

    std::fs::write(
        repository.path().join("packet.txt"),
        "base\nnext\nrecovery\n",
    )
    .expect("recovery fixture change");
    let recovery_input = read_hunks(
        &mut writer,
        &mut lines,
        "request.socket.recovery-hunks",
        observed_at,
        &deadline,
        &cancellation,
    )
    .await;
    let recovery_preview_request = super::super::DaemonInvocationRequest::git_preview(
        "request.socket.recovery-preview",
        tracedecay_daemon_service::application_surface::GitPreviewSurfaceRequest {
            operation: GitIndexTransactionOperationV1::StageHunks,
            preview_input_id: Some(recovery_input.preview_input_id.clone()),
            selected_hunk_digests: recovery_input
                .hunks
                .iter()
                .map(|entry| entry.digest.clone())
                .collect(),
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
        tracedecay_daemon_service::application_surface::GitApplySurfaceRequest {
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
    prewarm_test_profile_runtime(&store_administration).await;
    let (listener, endpoint) = tracedecay_daemon_protocol::BrokerListener::bind(
        &tracedecay_daemon_protocol::default_loopback_endpoint(),
    )
    .await
    .expect("loopback listener");
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept client");
        let lifecycle = DaemonLifecycle::default();
        Box::pin(super::super::serve_windows_broker_client(
            stream,
            TOKEN,
            &lifecycle,
            store_administration,
            Arc::new(tokio::sync::Mutex::new(
                super::super::ProjectOpenGates::default(),
            )),
            None,
        ))
        .await
    });

    let (reader, mut writer) = tokio::io::split(super::connect_loopback_test_peer(&endpoint).await);
    let preface = tracedecay_daemon_protocol::DaemonAuthPreface::new(TOKEN)
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

        let line = tokio::time::timeout(HALF_CLOSE_ROUND_TRIP_BOUND, lines.next_line())
            .await
            .expect("invocation response should not time out")
            .expect("read invocation response")
            .expect("invocation response");
        let response: serde_json::Value = serde_json::from_str(&line).expect("response json");
        assert_eq!(response["protocol"], "tracedecay.daemon.invocation");
        assert_eq!(response["request_id"], request_id);
        // The handshake names no project, so the project-scoped read is a
        // terminal refusal the client must correct, not a retryable outage.
        assert_eq!(response["status"], "application_problem", "{response:#}");
        assert_eq!(response["problem"]["kind"], "invalid_request");
        assert_eq!(
            response["problem"]["diagnostic"]["code"],
            "project_required"
        );
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
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        1,
        "linked-worktree-route-test",
    )
    .expect("daemon database scope");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let project_id = tracedecay_runtime_core::storage::read_repository_identity_marker(&primary)
        .expect("read primary repository identity")
        .expect("primary repository identity")
        .project_id;
    let registry = engine
        .store_administration
        .registered_profile_database()
        .await
        .expect("daemon profile registry");
    registry
        .upsert_code_project(
            &project_id,
            &linked,
            tracedecay_runtime_core::worktree::git_common_dir(&linked).as_deref(),
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
        RegisteredGlobalDb::canonical_project_key(&primary)
    );
    assert!(
        context.aliases.iter().any(|alias| {
            alias.alias_path == RegisteredGlobalDb::canonical_project_key(&linked)
        })
    );
}
