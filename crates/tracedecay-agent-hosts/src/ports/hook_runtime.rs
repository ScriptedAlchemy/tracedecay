//! The daemon-backed hook runtime that host integrations call into.
//!
//! **Registered ports.** Hook and host behavior lives in this crate. The root
//! still owns daemon handshakes, registered project identity, resolved daemon
//! scope, legacy daemon-event publication, and cached application
//! configuration. Those capabilities are inverted through the narrow slots
//! below so the host crate never depends back on the application binary.
//!
//! Root wiring: `src/runtime_ports.rs` registers every slot during startup,
//! before any install, hook, ingest, or doctor path runs.
//!
//! Each port has a conservative unregistered result: daemon and scope calls
//! fail closed, identity discovery yields no project, notification is inert,
//! telemetry has no authoritative override, project initialization uses the
//! kernel's durable local markers, memory injection is disabled, and the
//! ingest ceiling is unbounded so doctor never fabricates a backlog warning.

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
pub type HookTimingGate = fn(&Path) -> Option<bool>;

/// Reports whether one checkout has an initialized canonical store.
pub type ProjectInitializationGate = fn(&Path) -> bool;

/// Resolves the canonical store layout through registered project identity.
pub type StoreLayoutResolver =
    for<'a> fn(&'a Path) -> Pin<Box<dyn Future<Output = Result<StoreLayout>> + Send + 'a>>;

/// Reports whether memory injection is enabled for the active profile.
pub type MemoryInjectionGate = fn() -> bool;

/// Supplies the largest transcript tail a low-priority catch-up hook reads.
pub type CursorCatchUpIngestMaxBytes = fn() -> u64;

static DAEMON_TOOL_INVOKER: OnceLock<DaemonToolInvoker> = OnceLock::new();
static PROJECT_ROOT_RESOLVER: OnceLock<ProjectRootResolver> = OnceLock::new();
static HOOK_SCOPE_RESOLVER: OnceLock<HookScopeResolver> = OnceLock::new();
static HOOK_EVENT_NOTIFIER: OnceLock<HookEventNotifier> = OnceLock::new();
static HOOK_TIMING_GATE: OnceLock<HookTimingGate> = OnceLock::new();
static PROJECT_INITIALIZATION_GATE: OnceLock<ProjectInitializationGate> = OnceLock::new();
static STORE_LAYOUT_RESOLVER: OnceLock<StoreLayoutResolver> = OnceLock::new();
static MEMORY_INJECTION_GATE: OnceLock<MemoryInjectionGate> = OnceLock::new();
static CURSOR_CATCH_UP_INGEST_MAX_BYTES: OnceLock<CursorCatchUpIngestMaxBytes> = OnceLock::new();

/// Registers the root crate's daemon tool invoker.
///
/// Idempotent: the first registration wins, so concurrent daemon and CLI
/// initialisation cannot fight over it.
pub fn register_daemon_tool_invoker(invoker: DaemonToolInvoker) {
    let _ = DAEMON_TOOL_INVOKER.set(invoker);
}

pub fn register_project_root_resolver(resolver: ProjectRootResolver) {
    let _ = PROJECT_ROOT_RESOLVER.set(resolver);
}

pub fn register_hook_scope_resolver(resolver: HookScopeResolver) {
    let _ = HOOK_SCOPE_RESOLVER.set(resolver);
}

pub fn register_hook_event_notifier(notifier: HookEventNotifier) {
    let _ = HOOK_EVENT_NOTIFIER.set(notifier);
}

pub fn register_hook_timing_gate(gate: HookTimingGate) {
    let _ = HOOK_TIMING_GATE.set(gate);
}

pub fn register_project_initialization_gate(gate: ProjectInitializationGate) {
    let _ = PROJECT_INITIALIZATION_GATE.set(gate);
}

pub fn register_store_layout_resolver(resolver: StoreLayoutResolver) {
    let _ = STORE_LAYOUT_RESOLVER.set(resolver);
}

/// Registers the root crate's memory-injection settings read.
pub fn register_memory_injection_gate(gate: MemoryInjectionGate) {
    let _ = MEMORY_INJECTION_GATE.set(gate);
}

/// Registers the root crate's Cursor catch-up ingest ceiling.
pub fn register_cursor_catch_up_ingest_max_bytes(max_bytes: CursorCatchUpIngestMaxBytes) {
    let _ = CURSOR_CATCH_UP_INGEST_MAX_BYTES.set(max_bytes);
}

/// Calls one daemon tool and returns its JSON payload.
///
/// Errors when the root never registered an invoker. Callers already treat a
/// daemon request failure as "defer this work and warn", which is the correct
/// handling for an unwired build too.
#[hotpath::measure(future = true, label = "agent_hosts.hook_runtime.daemon_tool")]
pub async fn daemon_tool_json(
    project_root: Option<&Path>,
    tool_name: &str,
    arguments: Value,
    require_project_identity: bool,
) -> Result<Value> {
    let Some(invoker) = DAEMON_TOOL_INVOKER.get() else {
        return Err(TraceDecayError::Config {
            message: format!(
                "daemon tool '{tool_name}' is unavailable: no daemon tool invoker is registered"
            ),
        });
    };
    invoker(project_root, tool_name, arguments, require_project_identity).await
}

#[hotpath::measure(future = true, label = "agent_hosts.hook_runtime.resolve_root")]
pub async fn resolve_project_root_with_identity(start: &Path) -> Option<PathBuf> {
    let resolver = PROJECT_ROOT_RESOLVER.get()?;
    resolver(start).await
}

#[hotpath::measure(label = "agent_hosts.hook_runtime.resolve_scope")]
pub fn resolve_hook_scope(
    project_root: &Path,
    project_id: &ProjectId,
) -> std::result::Result<ResolvedScope, String> {
    HOOK_SCOPE_RESOLVER.get().map_or_else(
        || Err("no Hook scope resolver is registered".to_owned()),
        |resolver| resolver(project_root, project_id),
    )
}

#[hotpath::measure(future = true, label = "agent_hosts.hook_runtime.notify_event")]
pub async fn notify_hook_event(project_root: &Path, event: DaemonHookEvent) {
    if let Some(notifier) = HOOK_EVENT_NOTIFIER.get() {
        notifier(project_root, event).await;
    }
}

#[must_use]
pub fn hook_timings_enabled(project_root: &Path) -> Option<bool> {
    HOOK_TIMING_GATE.get().and_then(|gate| gate(project_root))
}

#[must_use]
#[hotpath::measure(label = "agent_hosts.hook_runtime.project_initialized")]
pub fn is_project_initialized(project_root: &Path) -> bool {
    PROJECT_INITIALIZATION_GATE.get().map_or_else(
        || {
            crate::config::has_project_database(project_root)
                || crate::storage::has_repository_identity_marker(project_root)
        },
        |gate| gate(project_root),
    )
}

#[hotpath::measure(future = true, label = "agent_hosts.hook_runtime.store_layout")]
pub async fn resolve_store_layout(project_root: &Path) -> Result<StoreLayout> {
    let Some(resolver) = STORE_LAYOUT_RESOLVER.get() else {
        return Err(TraceDecayError::Config {
            message: "no Hook store-layout resolver is registered".to_owned(),
        });
    };
    resolver(project_root).await
}

/// Whether memory injection is enabled, or `false` when the root never
/// registered.
#[must_use]
pub fn memory_injection_enabled() -> bool {
    MEMORY_INJECTION_GATE.get().is_some_and(|gate| gate())
}

/// Largest transcript tail a low-priority Cursor catch-up hook will read.
///
/// Reads as `u64::MAX` when the root never registered, so the doctor check
/// that compares a pending backlog against this ceiling stays silent rather
/// than reporting every install as stalled.
#[must_use]
pub fn cursor_catch_up_ingest_max_bytes() -> u64 {
    CURSOR_CATCH_UP_INGEST_MAX_BYTES
        .get()
        .map_or(u64::MAX, |max_bytes| max_bytes())
}
