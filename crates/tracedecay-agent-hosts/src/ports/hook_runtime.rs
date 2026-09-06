//! The daemon-backed hook runtime that host integrations call into.
//!
//! **One required composition handle**, not nine independent slots.
//!
//! Hook and host behavior lives in this crate. The root still owns daemon
//! handshakes, registered project identity, resolved daemon scope, legacy
//! daemon-event publication, and cached application configuration. Those
//! capabilities are inverted through [`HookRuntimeV1`], which the composition
//! root builds once and installs with [`install`].
//!
//! Every field is **required**: a hook that reaches this module in production
//! needs all of them, so there is no partial handle and no per-capability
//! default. A process that never installed the handle is a bootstrap failure,
//! and each reader below says exactly that — a typed error where its signature
//! carries one, and a logged composition error plus the conservative answer
//! where it does not. What no reader does any more is substitute a plausible
//! value: an unset handle used to read as "no project here", "telemetry off",
//! "memory injection disabled", and "unbounded ingest backlog", each of which
//! is a legitimate production state that hid the miswire.
//!
//! Two former slots are gone rather than moved. The memory-injection gate and
//! the Cursor catch-up ingest ceiling both asked the root to hand this crate
//! back its own `hooks::memory_inject::memory_injection_enabled` and
//! `hooks::CURSOR_CATCH_UP_INGEST_MAX_BYTES`; their readers call them directly.
//!
//! Root wiring: `src/runtime_ports.rs` installs the handle during startup,
//! before any install, hook, ingest, or doctor path runs.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::OnceLock;

use serde_json::Value;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::ProjectId;
use tracedecay_hooks::DaemonHookEvent;
use tracedecay_runtime_core::storage::StoreLayout;

use crate::errors::{Result, TraceDecayError};

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
/// Installed atomically: no reader can observe a handle spliced from two
/// registrations, and no field can be missing while the others are present.
pub struct HookRuntimeV1 {
    pub daemon_tool: DaemonToolInvoker,
    pub project_root_resolver: ProjectRootResolver,
    pub scope_resolver: HookScopeResolver,
    pub event_notifier: HookEventNotifier,
    pub timing_gate: HookTimingGate,
    pub project_initialization_gate: ProjectInitializationGate,
    pub store_layout_resolver: StoreLayoutResolver,
}

static HOOK_RUNTIME: OnceLock<HookRuntimeV1> = OnceLock::new();

/// Installs the composition root's hook runtime. Idempotent and atomic: the
/// first complete handle wins and no later install replaces part of it.
pub fn install(runtime: HookRuntimeV1) {
    let _ = HOOK_RUNTIME.set(runtime);
}

/// The installed handle, or `None` when no composition root ever installed one.
#[must_use]
pub fn installed() -> Option<&'static HookRuntimeV1> {
    HOOK_RUNTIME.get()
}

/// The installed handle, or the typed bootstrap failure.
fn required(capability: &str) -> Result<&'static HookRuntimeV1> {
    installed().ok_or_else(|| TraceDecayError::Config {
        message: missing_message(capability),
    })
}

fn missing_message(capability: &str) -> String {
    format!(
        "hook runtime capability '{capability}' is unavailable: the composition root never \
         installed HookRuntimeV1"
    )
}

/// Records a bootstrap failure on a reader whose signature cannot carry one.
///
/// The conservative value the caller then returns is indistinguishable from a
/// legitimate answer, so the miswire has to be visible here or nowhere.
fn report_missing(capability: &str) {
    tracing::error!("{}", missing_message(capability));
}

/// Calls one daemon tool and returns its JSON payload.
#[hotpath::measure(future = true, label = "agent_hosts.hook_runtime.daemon_tool")]
pub async fn daemon_tool_json(
    project_root: Option<&Path>,
    tool_name: &str,
    arguments: Value,
    require_project_identity: bool,
) -> Result<Value> {
    let runtime = required(&format!("daemon tool '{tool_name}'"))?;
    (runtime.daemon_tool)(project_root, tool_name, arguments, require_project_identity).await
}

#[hotpath::measure(future = true, label = "agent_hosts.hook_runtime.resolve_root")]
pub async fn resolve_project_root_with_identity(start: &Path) -> Option<PathBuf> {
    let Some(runtime) = installed() else {
        report_missing("resolve_project_root_with_identity");
        return None;
    };
    (runtime.project_root_resolver)(start).await
}

#[hotpath::measure(label = "agent_hosts.hook_runtime.resolve_scope")]
pub fn resolve_hook_scope(
    project_root: &Path,
    project_id: &ProjectId,
) -> std::result::Result<ResolvedScope, String> {
    let runtime = installed().ok_or_else(|| missing_message("resolve_hook_scope"))?;
    (runtime.scope_resolver)(project_root, project_id)
}

#[hotpath::measure(future = true, label = "agent_hosts.hook_runtime.notify_event")]
pub async fn notify_hook_event(project_root: &Path, event: DaemonHookEvent) {
    let Some(runtime) = installed() else {
        report_missing("notify_hook_event");
        return;
    };
    (runtime.event_notifier)(project_root, event).await;
}

/// The daemon's authoritative timing decision for this checkout.
///
/// `None` is "no override published"; a missing handle is reported separately
/// because it is not the same thing.
#[must_use]
pub fn hook_timings_enabled(project_root: &Path) -> Option<bool> {
    let Some(runtime) = installed() else {
        report_missing("hook_timings_enabled");
        return None;
    };
    (runtime.timing_gate)(project_root)
}

#[must_use]
#[hotpath::measure(label = "agent_hosts.hook_runtime.project_initialized")]
pub fn is_project_initialized(project_root: &Path) -> bool {
    let Some(runtime) = installed() else {
        // The former fallback read this crate's own local markers, which is a
        // *different answer* to the registered identity authority's, not a
        // degraded one — so it could disagree with production silently.
        report_missing("is_project_initialized");
        return false;
    };
    (runtime.project_initialization_gate)(project_root)
}

#[hotpath::measure(future = true, label = "agent_hosts.hook_runtime.store_layout")]
pub async fn resolve_store_layout(project_root: &Path) -> Result<StoreLayout> {
    let runtime = required("resolve_store_layout")?;
    (runtime.store_layout_resolver)(project_root).await
}

#[cfg(test)]
pub(crate) use test_runtime::install_crate_test_runtime;

#[cfg(test)]
mod test_runtime {
    use super::{HookRuntimeV1, Result, StoreLayout, TraceDecayError, Value, install};
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

    /// Installs this crate's explicit test handle.
    ///
    /// Tests that reach a hook-runtime reader state their dependency by calling
    /// this, rather than depending on the handle being globally absent.
    /// Idempotent, and every field answers the same way for every caller, so
    /// test order cannot change what a reader sees.
    pub(crate) fn install_crate_test_runtime() {
        install(HookRuntimeV1 {
            daemon_tool,
            project_root_resolver: project_root,
            scope_resolver: scope,
            event_notifier: notify,
            timing_gate: timings,
            project_initialization_gate: initialized,
            store_layout_resolver: layout,
        });
    }
}
