//! Daemon survival under version-skewed clients.
//!
//! A stale installed CLI routinely outlives the daemon binary it talks to.
//! Its handshake still parses (the daemon logs `daemon_version_skew`) and its
//! traffic serves through the projectless connection path, so that path must
//! stay pollable within one default Tokio worker stack. The projectless
//! tools/call dispatch previously compiled into a multi-megabyte poll frame
//! that overflowed the worker stack before any handler ran and aborted the
//! whole daemon (`thread 'tokio-rt-worker' has overflowed its stack`),
//! turning any one stale client into a daemon denial of service.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use tracedecay::daemon::{DaemonClientIdentity, DaemonHandshake, call_tool};
use tracedecay::tracedecay::MovedStoreAdoption;

use crate::common::{daemon_socket_path, spawn_tracedecay_daemon, tempdir_or_panic};

/// Client version from the production crash: an installed beta.37 CLI
/// connected to a daemon built at a later revision.
const SKEWED_CLIENT_VERSION: &str = "0.1.0-beta.37+087e6674286e";

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

fn projectless_handshake(
    profile_root: &Path,
    client_version: &str,
    instance: &str,
) -> DaemonHandshake {
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
        client_version: client_version.to_owned(),
        client_instance_id: instance.to_owned(),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
        moved_store_adoption: MovedStoreAdoption::Never,
    }
}

fn profile_root_for(home: &Path) -> PathBuf {
    home.canonicalize()
        .expect("test home should canonicalize")
        .join(".tracedecay")
}

#[tokio::test]
async fn version_skewed_client_cannot_crash_the_daemon() {
    let home = tempdir_or_panic();
    let mut daemon = spawn_tracedecay_daemon(home.path());
    let socket = daemon_socket_path(home.path());
    let profile_root = profile_root_for(home.path());

    // A project tool from a projectless connection exercises the full
    // tools/call dispatch — the poll frame that previously overflowed — and
    // must come back as the daemon's typed refusal, not a dead socket.
    let skewed = projectless_handshake(&profile_root, SKEWED_CLIENT_VERSION, "stale-skewed-client");
    let refusal = tokio::time::timeout(
        RESPONSE_TIMEOUT,
        call_tool(
            &socket,
            &skewed,
            "tracedecay_search",
            json!({ "query": "daemon", "limit": 1, "format": "json" }),
        ),
    )
    .await
    .expect("skewed projectless tools/call timed out")
    .expect_err("a project tool on a projectless connection must be refused");
    let refusal = refusal.to_string();
    assert!(
        refusal.contains("daemon tool call failed"),
        "skewed client must receive a daemon-authored response, not a transport failure: {refusal}"
    );
    assert!(
        refusal.contains("requires an initialized code project"),
        "skewed client must receive the typed projectless refusal: {refusal}"
    );

    assert!(
        daemon
            .try_wait()
            .expect("daemon status should be readable")
            .is_none(),
        "daemon exited after serving a version-skewed client"
    );

    // The daemon must keep answering healthy clients afterward, including a
    // projectless tools/call that runs a real handler to completion.
    let healthy = projectless_handshake(
        &profile_root,
        env!("CARGO_PKG_VERSION"),
        "healthy-followup-client",
    );
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
    .expect("daemon must answer a healthy client after skewed traffic");
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
