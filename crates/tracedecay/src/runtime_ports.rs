//! Composition-root wiring: the daemon client the project crate's runtime
//! ports need, and the explicit handles built from it.
//!
//! `tracedecay-project` composes every other runtime port and the hook
//! runtime handle; the two adapters below are the ones that need a daemon
//! connection, handshake, and wire preamble, so they stay with the daemon
//! client here. [`register_runtime_ports`] is the complete, idempotent wiring
//! call for a process: it hands the client to the project crate, which fills
//! every slot the extracted crates read. [`hook_runtime`] builds the explicit
//! [`HookRuntimeV1`] the CLI passes into each
//! `tracedecay_agent_hosts::hooks::hook_*` entry point, and
//! [`session_review_port`] the [`SessionReviewPort`] the daemon hands its
//! profile ingestor. A hook path or user ingest pass cannot run without a
//! complete handle, and two fixtures can hold two different handles in one
//! process.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use serde_json::Value;

use tracedecay_agent_hosts::ports::hook_runtime::HookRuntimeV1;
use tracedecay_domain::errors::Result;
use tracedecay_project::runtime_ports::DaemonClientPortsV1;
use tracedecay_sessions::host_ports::session_review::SessionReviewPort;

/// Installs every runtime port with this root's daemon client. Idempotent;
/// first call wins.
///
/// Call this as early as possible in a process: the slots it fills are read
/// by transcript ingest, agent-host installers, hooks, branch locking, and
/// project open, all of which fail closed when the root never registered.
pub fn register_runtime_ports() -> Result<()> {
    tracedecay_project::runtime_ports::register_runtime_ports(daemon_client_ports())
}

/// The root's hook runtime handle: every capability a hook path needs, as one
/// `Copy` value composed over this root's daemon client.
#[must_use]
pub fn hook_runtime() -> HookRuntimeV1 {
    tracedecay_project::runtime_ports::hook_runtime_with(daemon_client_ports())
}

/// Injects the contracts-owned catalog composition into the code-index
/// runtime's provider seam.
pub(crate) fn compose_application_catalog_snapshot() -> std::result::Result<
    tracedecay_tool_catalog::CatalogSnapshotV1,
    tracedecay_code_index_runtime::ApplicationCatalogSnapshotErrorV1,
> {
    tracedecay_contracts::catalog_composition::build_application_catalog_snapshot().map_err(
        |error| {
            tracedecay_code_index_runtime::ApplicationCatalogSnapshotErrorV1::new(error.to_string())
        },
    )
}

/// The root's session review port: the post-ingest review hint routed through
/// the daemon client. The daemon hands it to the profile ingestor at
/// construction, so a user pass without one is a typed refusal in
/// `tracedecay-sessions`, never a silent skip.
#[must_use]
pub const fn session_review_port() -> SessionReviewPort {
    SessionReviewPort::new(schedule_user_session_review)
}

fn schedule_user_session_review<'a>(
    provider: &'a str,
    session_id: Option<&'a str>,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    Box::pin(hotpath::future!(
        async move {
            let runtime = hook_runtime();
            tracedecay_agent_hosts::hooks::schedule_user_session_review(
                &runtime, provider, session_id,
            )
            .await;
        },
        label = "runtime_ports.session_review"
    ))
}

/// The daemon-client adapters only this root composes: one-shot tool calls
/// and hook event delivery over the daemon socket.
const fn daemon_client_ports() -> DaemonClientPortsV1 {
    DaemonClientPortsV1 {
        daemon_tool: daemon_tool_json,
        event_notifier: notify_hook_event,
    }
}

/// Fn-pointer shim over the root's async daemon tool call.
///
/// The port is a plain `fn` returning a boxed future so the extracted crate
/// needs no async-trait machinery.
fn daemon_tool_json<'a>(
    project_root: Option<&'a Path>,
    tool_name: &'a str,
    arguments: Value,
    require_project_identity: bool,
) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
    Box::pin(hotpath::future!(
        async move {
            let handshake = crate::daemon::handshake_for_current_client(
                project_root.map(Path::to_path_buf),
                None,
                false,
                require_project_identity,
            )?;
            let result = crate::daemon::call_default_tool(&handshake, tool_name, arguments).await?;
            crate::daemon::tool_json_payload(&result, tool_name)
        },
        label = "runtime_ports.daemon_tool"
    ))
}

fn notify_hook_event(
    project_root: &Path,
    event: tracedecay_hooks::DaemonHookEvent,
) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
    Box::pin(hotpath::future!(
        async move {
            let _ = crate::daemon::notify_hook_event(project_root, event).await;
        },
        label = "runtime_ports.notify_hook"
    ))
}
