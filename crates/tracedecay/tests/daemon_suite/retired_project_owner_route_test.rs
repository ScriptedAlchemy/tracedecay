//! A daemon connection routed to a project's core owner keeps serving across
//! the composition upgrade that replaces that owner with the full owner.
//!
//! Project open publishes a core graph/search owner, then replaces it with the
//! full session-capable owner and revokes the core once its admitted responses
//! drain. A connection that was bound to the core while the open was still
//! converging can send its next request after that revocation. The request
//! never reached a handler, so it belongs to the route's current owner; a
//! retirement refusal there fails a call that never ran.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use asupersync::io::{AsyncWriteExt, BufReader};
use serde_json::{Value, json};
use tracedecay_daemon_protocol::connection::write_daemon_handshake_preamble;
use tracedecay_daemon_protocol::{
    DaemonClientReadHalf, DaemonClientStream, DaemonClientWriteHalf, DaemonEndpoint,
    DaemonHandshake,
};
use tracedecay_framing::BoundedLineReader;

use crate::code_index_journey::{
    RECEIPT_TIMEOUT, commit_all, git, initialize_tracedecay, stop_daemon_gracefully,
    wait_for_open_phase,
};
use crate::common::{
    IsolatedHome, daemon_authority_path, daemon_socket_path, poll_until,
    spawn_tracedecay_daemon_logged,
};

const ANCHOR: &str = "retired_project_owner_route_anchor";

fn initialize_repository(project: &Path) -> PathBuf {
    fs::create_dir_all(project.join("src")).expect("fixture source directory");
    fs::write(
        project.join("src/lib.rs"),
        format!("pub fn {ANCHOR}() -> u32 {{ 1 }}\n"),
    )
    .expect("fixture source");
    git(project, &["init", "--quiet", "--initial-branch=main"]);
    commit_all(project, "retired owner route fixture");
    project.canonicalize().expect("canonical fixture project")
}

/// One initialized MCP session on its own daemon connection. The daemon binds
/// the whole connection to the project owner it resolved for the session's
/// first request, so a later tool call on the same session exercises that
/// bound owner's currentness.
struct LiveMcpSession {
    reader: BoundedLineReader<BufReader<DaemonClientReadHalf>>,
    writer: DaemonClientWriteHalf,
    next_id: i64,
}

impl LiveMcpSession {
    async fn open(home: &Path, handshake: &DaemonHandshake) -> Self {
        let authority: Value = serde_json::from_slice(
            &fs::read(daemon_authority_path(&home.join(".tracedecay")))
                .expect("published daemon authority"),
        )
        .expect("daemon authority JSON");
        let token = authority["auth_token"].as_str().expect("daemon token");
        let (reader, mut writer) =
            DaemonClientStream::connect(&DaemonEndpoint::Unix(daemon_socket_path(home)))
                .await
                .expect("connect to the daemon")
                .into_split();
        write_daemon_handshake_preamble(&mut writer, token, handshake)
            .await
            .expect("write the daemon preamble");
        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "retired-owner-route", "version": "1"}
            }
        });
        writer
            .write_all(format!("{initialize}\n").as_bytes())
            .await
            .expect("write MCP initialize");
        let mut reader = BoundedLineReader::new(BufReader::new(reader));
        let response = tokio::time::timeout(RECEIPT_TIMEOUT, async {
            loop {
                let line = match reader.read_mcp_line().await {
                    Ok(Some(line)) => line,
                    Ok(None) => panic!("the daemon closed the session during initialize"),
                    Err(error) => panic!("read MCP frame: {error}"),
                };
                let frame: Value = serde_json::from_str(&line).expect("MCP frame JSON");
                if frame["id"] == 1 {
                    return frame;
                }
            }
        })
        .await
        .expect("initialize answered");
        assert!(
            response.get("error").is_none(),
            "initialize failed: {response}"
        );
        Self {
            reader,
            writer,
            next_id: 2,
        }
    }

    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        });
        self.writer
            .write_all(format!("{request}\n").as_bytes())
            .await
            .expect("write MCP tool call");
        self.writer.flush().await.expect("flush MCP tool call");
        tokio::time::timeout(RECEIPT_TIMEOUT, async {
            loop {
                let line = match self.reader.read_mcp_line().await {
                    Ok(Some(line)) => line,
                    Ok(None) => panic!("the daemon closed the session before {name} answered"),
                    Err(error) => panic!("read MCP frame: {error}"),
                };
                let frame: Value = serde_json::from_str(&line).expect("MCP frame JSON");
                if frame["id"] == id {
                    return frame;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{name} never answered"))
    }

    async fn close(mut self) {
        let _ = self.writer.shutdown().await;
    }
}

fn tool_text(response: &Value) -> String {
    response["result"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn a_session_bound_to_the_retired_core_serves_its_next_call_on_the_full_owner() {
    let (environment, project) = IsolatedHome::new();
    let project = initialize_repository(&project);
    let log_path = environment.scratch().join("retired-owner-route.log");
    let hold = environment.scratch().join("hold-after-core-publish");
    let entered = PathBuf::from(format!("{}.entered", hold.display()));
    let mut daemon = spawn_tracedecay_daemon_logged(environment.home(), &log_path, |command| {
        command.env("TRACEDECAY_TEST_HOLD_AFTER_CORE_PUBLISH", &hold);
    });
    tracedecay_project::product_runtime::register_fixture_product_runtime();

    // Park the project open after it publishes its core owner, then start the
    // open from a real `tracedecay init` invocation.
    fs::write(&hold, b"hold").expect("hold file");
    let home = environment.home().to_path_buf();
    let init_project = project.clone();
    let init = std::thread::spawn(move || initialize_tracedecay(&home, &init_project));
    poll_until(
        Instant::now() + RECEIPT_TIMEOUT,
        Duration::from_millis(20),
        || entered.is_file().then_some(()),
        || {
            format!(
                "the project open never reached core publication: {}",
                fs::read_to_string(&log_path).unwrap_or_default()
            )
        },
    );

    // A second connection joins the converging open and is bound to the
    // published core owner, exactly as the daemon hands out an owner while the
    // full owner is still being constructed.
    let handshake = crate::common::handshake_for_current_client(
        environment.profile(),
        Some(project.clone()),
        None,
        false,
        false,
    )
    .expect("project handshake");
    let mut session = LiveMcpSession::open(environment.home(), &handshake).await;

    // Let the held open finish: the full owner replaces the core and the core
    // is revoked after its admitted responses drain.
    fs::remove_file(&hold).expect("release the held open");
    wait_for_open_phase(&log_path, &project, "full_published").await;

    // The session is still bound to the retired core. This call never reached
    // a handler, so it must be served by the route's current (full) owner.
    let response = session
        .call("tracedecay_dashboard", json!({"action": "stop"}))
        .await;
    assert!(
        response.get("error").is_none(),
        "a request admitted after the bound owner was replaced must follow the \
         route's current owner: {response}"
    );
    let text = tool_text(&response);
    assert!(
        text.contains("not_running"),
        "the current owner must answer the dashboard stop: {text}"
    );

    session.close().await;
    init.join().expect("the init invocation completes");
    stop_daemon_gracefully(&mut daemon);
}
