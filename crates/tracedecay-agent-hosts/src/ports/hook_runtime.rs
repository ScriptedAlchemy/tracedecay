//! The daemon-backed hook runtime that host integrations call into.
//!
//! **One explicit composition handle**, passed by the root, not a process
//! global.
//!
//! Hook and host behavior lives in this crate. The root still owns daemon
//! handshakes, registered project identity, resolved daemon scope, legacy
//! daemon-event publication, and cached application configuration. Those
//! capabilities are inverted through [`HookRuntimeV1`], which the composition
//! root builds and hands to every hook entry point it invokes.
//!
//! Every field is **required**: a hook that reaches this module in production
//! needs all of them, so there is no partial handle and no per-capability
//! default. Because the handle is a parameter rather than a slot, a hook path
//! cannot run without one — the compiler, not a runtime probe, is the
//! composition check — and two fixtures can hold two different handles in the
//! same process without a first-registration-wins race.
//!
//! Two former slots are gone rather than moved. The memory-injection gate and
//! the Cursor catch-up ingest ceiling both asked the root to hand this crate
//! back its own `hooks::memory_inject::memory_injection_enabled` and
//! `hooks::CURSOR_CATCH_UP_INGEST_MAX_BYTES`; their readers call them directly.
//!
//! Root wiring: `src/runtime_ports.rs` builds the handle (`hook_runtime()`)
//! and the CLI passes it into each `hooks::hook_*` entry point.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::Value;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::ProjectId;
use tracedecay_hooks::DaemonHookEvent;
use tracedecay_runtime_core::storage::StoreLayout;

use crate::errors::Result;

/// Invokes one daemon tool by name and yields its single JSON payload.
pub type DaemonToolInvoker = for<'a> fn(
    Option<&'a Path>,
    &'a str,
    Value,
    bool,
)
    -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

/// Resolves a checkout through the root's registered identity authority.
pub type ProjectRootResolver =
    for<'a> fn(&'a Path) -> Pin<Box<dyn Future<Output = Option<PathBuf>> + Send + 'a>>;

/// Resolves the typed project/repository/worktree scope used by Hook bindings.
pub type HookScopeResolver = fn(&Path, &ProjectId) -> std::result::Result<ResolvedScope, String>;

/// Publishes one legacy daemon hook event through the root daemon runtime.
pub type HookEventNotifier =
    for<'a> fn(&'a Path, DaemonHookEvent) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Reads the daemon-published telemetry timing decision without store I/O.
///
/// The `Option` is the answer, not the wiring: `None` means the daemon has
/// published no authoritative override for this checkout.
pub type HookTimingGate = fn(&Path) -> Option<bool>;

/// Reports whether one checkout has an initialized canonical store.
pub type ProjectInitializationGate = fn(&Path) -> bool;

/// Resolves the canonical store layout through registered project identity.
pub type StoreLayoutResolver =
    for<'a> fn(&'a Path) -> Pin<Box<dyn Future<Output = Result<StoreLayout>> + Send + 'a>>;

/// The root-owned capabilities every hook path needs, as one value.
///
/// Plain function pointers, so the handle is `Copy` and the root can build it
/// wherever a hook path starts; no field can be missing while the others are
/// present.
#[derive(Clone, Copy)]
pub struct HookRuntimeV1 {
    pub daemon_tool: DaemonToolInvoker,
    pub project_root_resolver: ProjectRootResolver,
    pub scope_resolver: HookScopeResolver,
    pub event_notifier: HookEventNotifier,
    pub timing_gate: HookTimingGate,
    pub project_initialization_gate: ProjectInitializationGate,
    pub store_layout_resolver: StoreLayoutResolver,
}

impl HookRuntimeV1 {
    /// Calls one daemon tool and returns its JSON payload.
    #[hotpath::measure(future = true, label = "agent_hosts.hook_runtime.daemon_tool")]
    pub async fn daemon_tool_json(
        &self,
        project_root: Option<&Path>,
        tool_name: &str,
        arguments: Value,
        require_project_identity: bool,
    ) -> Result<Value> {
        (self.daemon_tool)(project_root, tool_name, arguments, require_project_identity).await
    }

    #[hotpath::measure(future = true, label = "agent_hosts.hook_runtime.resolve_root")]
    pub async fn resolve_project_root_with_identity(&self, start: &Path) -> Option<PathBuf> {
        (self.project_root_resolver)(start).await
    }

    #[hotpath::measure(label = "agent_hosts.hook_runtime.resolve_scope")]
    pub fn resolve_hook_scope(
        &self,
        project_root: &Path,
        project_id: &ProjectId,
    ) -> std::result::Result<ResolvedScope, String> {
        (self.scope_resolver)(project_root, project_id)
    }

    #[hotpath::measure(future = true, label = "agent_hosts.hook_runtime.notify_event")]
    pub async fn notify_hook_event(&self, project_root: &Path, event: DaemonHookEvent) {
        (self.event_notifier)(project_root, event).await;
    }

    /// The daemon's authoritative timing decision for this checkout; `None`
    /// is "no override published".
    #[must_use]
    pub fn hook_timings_enabled(&self, project_root: &Path) -> Option<bool> {
        (self.timing_gate)(project_root)
    }

    #[must_use]
    #[hotpath::measure(label = "agent_hosts.hook_runtime.project_initialized")]
    pub fn is_project_initialized(&self, project_root: &Path) -> bool {
        (self.project_initialization_gate)(project_root)
    }

    #[hotpath::measure(future = true, label = "agent_hosts.hook_runtime.store_layout")]
    pub async fn resolve_store_layout(&self, project_root: &Path) -> Result<StoreLayout> {
        (self.store_layout_resolver)(project_root).await
    }
}

#[cfg(test)]
pub(crate) use test_runtime::crate_test_runtime;

#[cfg(test)]
mod test_runtime {
    use super::{HookRuntimeV1, Result, StoreLayout, Value};
    use crate::errors::TraceDecayError;
    use std::future::Future;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;

    fn unavailable(capability: &str) -> TraceDecayError {
        TraceDecayError::Config {
            message: format!("crate test hook runtime has no {capability}"),
        }
    }

    fn daemon_tool<'a>(
        _: Option<&'a Path>,
        tool_name: &'a str,
        _: Value,
        _: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move { Err(unavailable(&format!("daemon tool '{tool_name}'"))) })
    }

    fn project_root(_: &Path) -> Pin<Box<dyn Future<Output = Option<PathBuf>> + Send + '_>> {
        Box::pin(async { None })
    }

    fn scope(
        _: &Path,
        _: &tracedecay_domain::ProjectId,
    ) -> std::result::Result<tracedecay_application::ResolvedScope, String> {
        Err("crate test hook runtime has no scope resolver".to_owned())
    }

    fn notify(
        _: &Path,
        _: tracedecay_hooks::DaemonHookEvent,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }

    fn timings(_: &Path) -> Option<bool> {
        None
    }

    /// The kernel's durable local markers. Production answers this through the
    /// registered identity authority; this crate's tests seed the markers
    /// directly, so the fixture reads them.
    fn initialized(project_root: &Path) -> bool {
        crate::config::has_project_database(project_root)
            || crate::storage::has_repository_identity_marker(project_root)
    }

    fn layout(_: &Path) -> Pin<Box<dyn Future<Output = Result<StoreLayout>> + Send + '_>> {
        Box::pin(async { Err(unavailable("store layout resolver")) })
    }

    /// This crate's explicit test handle: no daemon, no registry, no layout.
    ///
    /// Tests that reach a hook-runtime reader pass this in, so their
    /// dependency is stated at the call rather than inferred from a global
    /// being absent. Every field answers the same way for every caller.
    pub(crate) fn crate_test_runtime() -> HookRuntimeV1 {
        HookRuntimeV1 {
            daemon_tool,
            project_root_resolver: project_root,
            scope_resolver: scope,
            event_notifier: notify,
            timing_gate: timings,
            project_initialization_gate: initialized,
            store_layout_resolver: layout,
        }
    }
}
