//! Host hook events: one stateless daemon request over the broker connection.
//!
//! The wire metadata and event constructors are pure data and live in
//! [`tracedecay_hooks::core_events`]. Only delivery, which needs the daemon
//! connection, handshake, and preamble, remains root-coupled.

use std::path::Path;

use tokio::io::AsyncWriteExt;
use tokio::time::{Duration, timeout};
use tracedecay_hooks::core_events::{DaemonHookEvent, HOOK_EVENT_METHOD, HookEventNotifyOutcomeV1};

use tracedecay_daemon_identity::{
    ResolvedDaemonConnection, client_connection, current_daemon_connection,
};
use tracedecay_daemon_protocol::SOCKET_ENV;
use tracedecay_mcp::server::attach_stateless_request_context;

use super::{BrokerStream, JsonRpcRequest, write_daemon_preamble};

pub(crate) const HOOK_EVENT_NOTIFY_TIMEOUT: Duration = Duration::from_millis(750);

#[hotpath::measure(label = "daemon.engine.hooks.notify", future = true)]
pub async fn notify_hook_event(
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    project_path: &Path,
    event: DaemonHookEvent,
) -> HookEventNotifyOutcomeV1 {
    let connection = std::env::var_os(SOCKET_ENV)
        .filter(|path| !path.is_empty())
        .map_or_else(
            || current_daemon_connection(profile.data_dir()),
            |path| client_connection(profile.data_dir(), Path::new(&path)),
        );
    let Ok(connection) = connection else {
        return HookEventNotifyOutcomeV1::Unavailable;
    };
    match timeout(
        HOOK_EVENT_NOTIFY_TIMEOUT,
        notify_hook_event_to_connection(profile, project_path, event, connection),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => HookEventNotifyOutcomeV1::TimedOut,
    }
}

#[hotpath::measure(label = "daemon.engine.hooks.deliver", future = true)]
async fn notify_hook_event_to_connection(
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    project_path: &Path,
    event: DaemonHookEvent,
    connection: ResolvedDaemonConnection,
) -> HookEventNotifyOutcomeV1 {
    let Ok(handshake) = crate::daemon::handshake_for_current_client(
        profile,
        Some(project_path.to_path_buf()),
        None,
        false,
        false,
    ) else {
        return HookEventNotifyOutcomeV1::Malformed;
    };
    let Ok(params) = serde_json::to_value(event) else {
        return HookEventNotifyOutcomeV1::Malformed;
    };
    // A stateless request, not a notification: its own daemon connection has
    // no `initialize` session for a notification to ride. The result is not
    // awaited, so delivery stays fire-and-forget.
    let mut request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(serde_json::Value::from(1)),
        method: HOOK_EVENT_METHOD.to_string(),
        params: Some(params),
    };
    attach_stateless_request_context(&mut request);
    let Ok(line) = serde_json::to_string(&request) else {
        return HookEventNotifyOutcomeV1::Malformed;
    };
    let Ok(stream) = BrokerStream::connect(connection.endpoint()).await else {
        return HookEventNotifyOutcomeV1::Unavailable;
    };
    let (_reader, mut writer) = stream.into_owned_split();
    if write_daemon_preamble(&mut writer, &connection, &handshake)
        .await
        .is_err()
    {
        return HookEventNotifyOutcomeV1::Unavailable;
    }
    if writer.write_all(line.as_bytes()).await.is_err() {
        return HookEventNotifyOutcomeV1::Unavailable;
    }
    if writer.write_all(b"\n").await.is_err() {
        return HookEventNotifyOutcomeV1::Unavailable;
    }
    if writer.flush().await.is_err() || writer.shutdown().await.is_err() {
        return HookEventNotifyOutcomeV1::Unavailable;
    }
    HookEventNotifyOutcomeV1::Delivered
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Instant;

    use super::*;

    #[tokio::test]
    async fn missing_hook_socket_returns_typed_unavailable_without_retry_delay() {
        // Delivery builds the client handshake first, and that handshake reads
        // the registered product runtime for its truthful binary version. Every
        // real hook-notifying process registers one at its entry point; a
        // per-test process that does not would classify the socket outcome as
        // Malformed (no advertisable version) before it ever reaches the
        // connect this test covers.
        tracedecay_project::product_runtime::register_fixture_product_runtime();
        let socket_dir = tempfile::tempdir().unwrap();
        let missing_socket = socket_dir.path().join("missing.sock");
        let _authority = super::super::tests::seed_socket_authority(&missing_socket);
        let profile = tracedecay_runtime_core::config::ProfileRoot::new(socket_dir.path());
        let connection = client_connection(profile.data_dir(), &missing_socket)
            .expect("seeded daemon authority");
        let started = Instant::now();

        let outcome = notify_hook_event_to_connection(
            &profile,
            socket_dir.path(),
            DaemonHookEvent::cursor_after_shell_execution(socket_dir.path().to_path_buf()),
            connection,
        )
        .await;

        assert_eq!(outcome, HookEventNotifyOutcomeV1::Unavailable);
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "a missing socket must not consume the outer hook timeout"
        );
    }
}
