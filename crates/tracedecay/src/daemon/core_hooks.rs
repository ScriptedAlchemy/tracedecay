//! Host hook events: daemon notification over the broker connection.
//!
//! The wire metadata and event constructors are pure data and live in
//! [`tracedecay_hooks::core_events`]; they are re-exported here so the daemon's
//! existing paths keep resolving. Only delivery — which needs the daemon
//! connection, handshake, and preamble — remains root-coupled.

use std::path::Path;

use tokio::io::AsyncWriteExt;
use tokio::time::{Duration, timeout};

pub use tracedecay_hooks::core_events::*;

#[cfg(unix)]
use tracedecay_daemon_identity::connection_for_socket_path;
use tracedecay_daemon_identity::{DaemonConnection, current_daemon_connection};

#[cfg(unix)]
use super::SOCKET_ENV;
use super::{BrokerStream, JsonRpcRequest, write_daemon_preamble};

pub(crate) const HOOK_EVENT_NOTIFY_TIMEOUT: Duration = Duration::from_millis(750);

#[hotpath::measure(label = "daemon.engine.hooks.notify", future = true)]
pub async fn notify_hook_event(
    project_path: &Path,
    event: DaemonHookEvent,
) -> HookEventNotifyOutcomeV1 {
    let connection = {
        #[cfg(unix)]
        {
            std::env::var_os(SOCKET_ENV)
                .filter(|path| !path.is_empty())
                .map(|path| connection_for_socket_path(Path::new(&path)))
                .map_or_else(current_daemon_connection, Ok)
        }
        #[cfg(not(unix))]
        {
            current_daemon_connection()
        }
    };
    let Ok(connection) = connection else {
        return HookEventNotifyOutcomeV1::Unavailable;
    };
    match timeout(
        HOOK_EVENT_NOTIFY_TIMEOUT,
        notify_hook_event_to_connection(project_path, event, connection),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => HookEventNotifyOutcomeV1::TimedOut,
    }
}

#[hotpath::measure(label = "daemon.engine.hooks.deliver", future = true)]
async fn notify_hook_event_to_connection(
    project_path: &Path,
    event: DaemonHookEvent,
    connection: DaemonConnection,
) -> HookEventNotifyOutcomeV1 {
    let Ok(handshake) = crate::daemon::handshake_for_current_client(
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
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: None,
        method: HOOK_EVENT_METHOD.to_string(),
        params: Some(params),
    };
    let Ok(line) = serde_json::to_string(&request) else {
        return HookEventNotifyOutcomeV1::Malformed;
    };
    let Ok(stream) = BrokerStream::connect(&connection.endpoint).await else {
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

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_hook_socket_returns_typed_unavailable_without_retry_delay() {
        // Delivery builds the client handshake first, and that handshake reads
        // the registered product runtime for its truthful binary version. Every
        // real hook-notifying process registers one at its entry point; a
        // per-test process that does not would classify the socket outcome as
        // Malformed (no advertisable version) before it ever reaches the
        // connect this test covers.
        crate::product_runtime::register_fixture_product_runtime();
        let socket_dir = tempfile::tempdir().unwrap();
        let missing_socket = socket_dir.path().join("missing.sock");
        let connection = connection_for_socket_path(&missing_socket);
        let started = Instant::now();

        let outcome = notify_hook_event_to_connection(
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
