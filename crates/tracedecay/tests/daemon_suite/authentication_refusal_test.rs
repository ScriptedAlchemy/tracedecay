//! Daemon authentication refusals are typed answers, not transport mysteries.
//!
//! A client presenting a token the daemon did not mint (routinely a stale
//! authority record read across a daemon restart) must read one typed
//! `authentication_rejected` refusal frame before the connection closes.
//! Closing without the frame made every client surface report "connection
//! closed, the outcome is unknown" for what is a definitive daemon answer.
//! The daemon must survive the refusal and keep serving healthy clients.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tracedecay::daemon::{DaemonClientIdentity, DaemonHandshake, call_tool};
use tracedecay::tracedecay::MovedStoreAdoption;
use tracedecay_daemon_protocol::transport::DaemonAuthPreface;
use tracedecay_daemon_protocol::{DaemonHandshakeRefusal, DaemonHandshakeRefusalReason};

use crate::common::{daemon_socket_path, spawn_tracedecay_daemon, tempdir_or_panic};

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

fn profile_root_for(home: &Path) -> PathBuf {
    home.canonicalize()
        .expect("test home should canonicalize")
        .join(".tracedecay")
}

fn projectless_handshake(profile_root: &Path, instance: &str) -> DaemonHandshake {
    DaemonHandshake {
        project_path: None,
        scope_prefix: None,
        timings: false,
        allow_init: false,
        allow_initialize_root_routing: false,
        client_identity: DaemonClientIdentity {
            global_db_path: profile_root.join("global.db"),
            profile_root: profile_root.to_path_buf(),
        },
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        client_instance_id: instance.to_owned(),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
        moved_store_adoption: MovedStoreAdoption::Never,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn rejected_token_reads_typed_refusal_and_daemon_keeps_serving() {
    let home = tempdir_or_panic();
    let mut daemon = spawn_tracedecay_daemon(home.path());
    let socket = daemon_socket_path(home.path());
    let profile_root = profile_root_for(home.path());

    // Raw client with a token the daemon did not mint, writing the real
    // client pipeline (preface, handshake, first request) before reading.
    let stream = tokio::net::UnixStream::connect(&socket)
        .await
        .expect("connect to daemon socket");
    let (reader, mut writer) = stream.into_split();
    let preface = DaemonAuthPreface::new("not-the-daemon-token")
        .to_line()
        .expect("preface json");
    writer
        .write_all(format!("{preface}\n").as_bytes())
        .await
        .expect("write bad auth preface");
    let handshake = projectless_handshake(&profile_root, "auth-refusal-raw-client")
        .to_line()
        .expect("handshake json");
    writer
        .write_all(format!("{handshake}\n").as_bytes())
        .await
        .expect("write handshake");
    writer
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"tracedecay_status\",\"arguments\":{}}}\n")
        .await
        .expect("write pipelined request");

    let mut lines = tokio::io::BufReader::new(reader).lines();
    let refusal_line = tokio::time::timeout(RESPONSE_TIMEOUT, lines.next_line())
        .await
        .expect("refusal must arrive before the read deadline")
        .expect("the refusal read must not fail with a transport reset")
        .expect("the daemon must answer a rejected token with a refusal line");
    let refusal = DaemonHandshakeRefusal::from_line(&refusal_line)
        .expect("the refusal line must parse as the typed refusal frame");
    assert_eq!(
        refusal.refusal,
        DaemonHandshakeRefusalReason::AuthenticationRejected,
        "a rejected token must be named as an authentication refusal: {refusal_line}"
    );
    assert!(
        !refusal_line.contains("not-the-daemon-token"),
        "the refusal must never echo the supplied token: {refusal_line}"
    );

    assert!(
        daemon
            .try_wait()
            .expect("daemon status should be readable")
            .is_none(),
        "daemon exited after refusing a client token"
    );

    // A healthy client (whose transport resolves the daemon's real authority
    // token) must still be served after the refusal.
    let healthy = projectless_handshake(&profile_root, "auth-refusal-healthy-client");
    let report = tokio::time::timeout(
        RESPONSE_TIMEOUT,
        call_tool(
            &socket,
            &healthy,
            "tracedecay_admin_project",
            json!({ "action": "automation_reconcile", "scope": "profile" }),
        ),
    )
    .await
    .expect("healthy follow-up tools/call timed out")
    .expect("daemon must answer a healthy client after an auth refusal");
    let content = report["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("automation reconcile returned no text content: {report}"));
    let reconcile: serde_json::Value =
        serde_json::from_str(content).expect("automation reconcile report should be JSON");
    assert_eq!(
        reconcile["scope"], "profile",
        "healthy follow-up must run its handler to completion: {reconcile}"
    );
}
