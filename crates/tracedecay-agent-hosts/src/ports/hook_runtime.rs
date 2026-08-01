//! The daemon-backed hook runtime that host integrations call into.
//!
//! **Registered ports.** The `hooks/` daemon-side handlers stay in the root
//! crate per the split plan: they own the daemon handshake, the client
//! identity, and the memory-injection settings read. Host installers here need
//! three narrow answers from that runtime, each expressed below.
//!
//! Root wiring: the root registers all three during startup, before any
//! install/update/doctor path runs — [`register_daemon_tool_invoker`] with
//! `hooks::daemon_tool_json`, [`register_memory_injection_gate`] with
//! `hooks::memory_inject::memory_injection_enabled`, and
//! [`register_cursor_catch_up_ingest_max_bytes`] with
//! `hooks::CURSOR_CATCH_UP_INGEST_MAX_BYTES`.
//!
//! Each port fails closed when the root never registers: an unwired build
//! reports the daemon as unavailable, treats memory injection as disabled, and
//! reads the ingest ceiling as unbounded so doctor never fabricates a
//! backlog warning it cannot substantiate.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::OnceLock;

use serde_json::Value;

use crate::errors::{Result, TraceDecayError};

/// Invokes one daemon tool by name and yields its single JSON payload.
pub type DaemonToolInvoker = for<'a> fn(
    Option<&'a Path>,
    &'a str,
    Value,
)
    -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

/// Reports whether memory injection is enabled for the active profile.
pub type MemoryInjectionGate = fn() -> bool;

/// Supplies the largest transcript tail a low-priority catch-up hook reads.
pub type CursorCatchUpIngestMaxBytes = fn() -> u64;

static DAEMON_TOOL_INVOKER: OnceLock<DaemonToolInvoker> = OnceLock::new();
static MEMORY_INJECTION_GATE: OnceLock<MemoryInjectionGate> = OnceLock::new();
static CURSOR_CATCH_UP_INGEST_MAX_BYTES: OnceLock<CursorCatchUpIngestMaxBytes> = OnceLock::new();

/// Registers the root crate's daemon tool invoker.
///
/// Idempotent: the first registration wins, so concurrent daemon and CLI
/// initialisation cannot fight over it.
pub fn register_daemon_tool_invoker(invoker: DaemonToolInvoker) {
    let _ = DAEMON_TOOL_INVOKER.set(invoker);
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
pub async fn daemon_tool_json(
    project_root: Option<&Path>,
    tool_name: &str,
    arguments: Value,
) -> Result<Value> {
    let Some(invoker) = DAEMON_TOOL_INVOKER.get() else {
        return Err(TraceDecayError::Config {
            message: format!(
                "daemon tool '{tool_name}' is unavailable: no daemon tool invoker is registered"
            ),
        });
    };
    invoker(project_root, tool_name, arguments).await
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
