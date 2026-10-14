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
//! `tracedecay_agent_hosts::hooks::hook_*` entry point. A hook path cannot
//! run without a complete handle, and two fixtures can hold two different
//! handles in one process.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use serde_json::Value;

use tracedecay_agent_hosts::ports::hook_runtime::HookRuntimeV1;
use tracedecay_domain::errors::Result;
use tracedecay_project::runtime_ports::DaemonClientPortsV1;

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
pub fn hook_runtime(profile: tracedecay_runtime_core::config::ProfileRoot) -> HookRuntimeV1 {
    tracedecay_project::runtime_ports::hook_runtime_with(profile, daemon_client_ports())
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
    profile: &'a tracedecay_runtime_core::config::ProfileRoot,
    project_root: Option<&'a Path>,
    tool_name: &'a str,
    arguments: Value,
    require_project_identity: bool,
) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
    Box::pin(tracing::Instrument::instrument(
        async move {
            let handshake = crate::daemon::handshake_for_current_client(
                profile,
                project_root.map(Path::to_path_buf),
                None,
                false,
                require_project_identity,
            )?;
            let result =
                crate::daemon::call_default_tool(profile, &handshake, tool_name, arguments).await?;
            if let Some(refusal) =
                tracedecay_mcp::application_output::tool_result::tool_result_refusal(&result)
            {
                return Err(refusal);
            }
            crate::daemon::tool_json_payload(&result, tool_name)
        },
        tracing::trace_span!("runtime_ports.daemon_tool"),
    ))
}

/// How long a host hook waits for the daemon to acknowledge its event before
/// returning control to the agent. The daemon still applies an event whose
/// acknowledgement arrives later.
const HOOK_EVENT_NOTIFY_TIMEOUT: Duration = Duration::from_millis(750);

fn notify_hook_event<'a>(
    profile: &'a tracedecay_runtime_core::config::ProfileRoot,
    project_root: &'a Path,
    event: tracedecay_hooks::DaemonHookEvent,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    Box::pin(tracing::Instrument::instrument(
        async move {
            let _ = tokio::time::timeout(
                HOOK_EVENT_NOTIFY_TIMEOUT,
                crate::daemon::notify_hook_event(profile, project_root, event),
            )
            .await;
        },
        tracing::trace_span!("runtime_ports.notify_hook"),
    ))
}
