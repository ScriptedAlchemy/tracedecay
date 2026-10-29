//! Daemon survival under version-skewed clients.
//!
//! A stale installed CLI routinely outlives the daemon binary it talks to.
//! The daemon refuses its handshake outright with a typed revision-skew
//! refusal and keeps serving current clients, including through the
//! projectless connection path, which must stay pollable within one default
//! Tokio worker stack. The projectless tools/call dispatch previously compiled
//! into a multi-megabyte poll frame that overflowed the worker stack before
//! any handler ran and aborted the whole daemon
//! (`thread 'tokio-rt-worker' has overflowed its stack`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use tracedecay::daemon::call_tool;
use tracedecay_daemon_protocol::{DaemonClientIdentity, DaemonHandshake};
use tracedecay_mcp::application_output::tool_result::tool_result_refusal;
use tracedecay_project::project::MovedStoreAdoption;

use crate::common::{daemon_socket_path, spawn_tracedecay_daemon, tempdir_or_panic};

/// Client version from the production crash: an installed beta.37 CLI
/// connected to a daemon built at a later revision.
const SKEWED_CLIENT_VERSION: &str = "0.1.0-beta.37+087e6674286e";

const SKEWED_INSTANCE: &str = "0000000000000000000000000000a001";
const CURRENT_INSTANCE: &str = "0000000000000000000000000000a002";

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
async fn version_skewed_client_is_refused_and_the_daemon_keeps_serving() {
    let home = tempdir_or_panic();
    let mut daemon = spawn_tracedecay_daemon(home.path());
    let socket = daemon_socket_path(home.path());
    let profile_root = profile_root_for(home.path());

    for (client_version, instance, label) in [
        (SKEWED_CLIENT_VERSION, SKEWED_INSTANCE, "a different build"),
        ("", SKEWED_INSTANCE, "a client that reports no version"),
        (
            env!("CARGO_PKG_VERSION"),
            "not-a-client-instance",
            "a client with a non-conforming instance id",
        ),
    ] {
        let skewed = projectless_handshake(&profile_root, client_version, instance);
        let error = tokio::time::timeout(
            RESPONSE_TIMEOUT,
            call_tool(
                &socket,
                &skewed,
                "tracedecay_search",
                json!({ "query": "daemon", "limit": 1, "format": "json" }),
            ),
        )
        .await
        .expect("refused handshake timed out")
        .expect_err("the daemon refuses every handshake it cannot serve");
        assert_eq!(
            error.project_route_context().map(|(code, _, _)| code),
            Some("daemon_protocol_revision_skew"),
            "{label} must receive the typed revision-skew refusal: {error}"
        );
    }

    assert!(
        daemon
            .try_wait()
            .expect("daemon status should be readable")
            .is_none(),
        "daemon exited after refusing a skewed client"
    );

    // A current client's projectless project tool must be refused by the
    // handler, not the transport. The tools/call dispatch previously compiled
    // into a multi-megabyte poll frame that overflowed the worker stack
    // before any handler ran and aborted the whole daemon.
    let current = projectless_handshake(&profile_root, env!("CARGO_PKG_VERSION"), CURRENT_INSTANCE);
    let result = tokio::time::timeout(
        RESPONSE_TIMEOUT,
        call_tool(
            &socket,
            &current,
            "tracedecay_search",
            json!({ "query": "daemon", "limit": 1, "format": "json" }),
        ),
    )
    .await
    .expect("projectless tools/call timed out")
    .expect("a settled refusal arrives as a tool result");
    let refusal = tool_result_refusal(&result)
        .expect("a project tool on a projectless connection must be refused");
    assert_eq!(
        refusal.project_route_context(),
        Some((
            "project_required",
            false,
            "tracedecay_search requires an initialized code project; run it inside an \
             initialized project or pass --project <path>"
        )),
        "the current client must receive the typed projectless refusal: {refusal}"
    );

    // The daemon must keep running a real handler to completion afterward.
    let report = tokio::time::timeout(
        RESPONSE_TIMEOUT,
        call_tool(
            &socket,
            &current,
            "tracedecay_admin_project",
            json!({ "action": "automation_reconcile", "scope": "profile", "format": "json" }),
        ),
    )
    .await
    .expect("follow-up tools/call timed out")
    .expect("daemon must answer a current client after refusing skewed ones");
    let content = report["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("automation reconcile returned no text content: {report}"));
    let reconcile: serde_json::Value =
        serde_json::from_str(content).expect("automation reconcile report should be JSON");
    assert_eq!(
        reconcile["scope"], "profile",
        "follow-up must run its handler to completion: {reconcile}"
    );
}
