//! The native execution runtime that storage settlement runs on.
//!
//! Storage, shard lifecycle, and session-memory paths spawn their settlement
//! work on an Asupersync runtime so accepted work outlives a cancelled request.
//! Callers that already run on a native runtime use it. Every other caller
//! (the Tokio daemon, CLI commands, and Tokio tests) shares one process
//! runtime, started on first use. Callers only await native join handles and
//! channel receivers, which wake through ordinary `Waker`s, so a foreign
//! executor never blocks on the native runtime.

use std::sync::OnceLock;

use asupersync::runtime::{Runtime, RuntimeBuilder, RuntimeHandle};
use asupersync::{Budget, Cx};

/// Tokio's default blocking-pool ceiling. Shard closes and SQL reads park a
/// blocking thread for their whole duration.
const MAX_BLOCKING_THREADS: usize = 512;

static PROCESS_RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();

/// The runtime the caller runs on, or the process runtime.
///
/// `None` only when the process runtime failed to start; the failure is
/// logged once.
#[must_use]
pub fn handle() -> Option<RuntimeHandle> {
    Runtime::current_handle().or_else(|| process_runtime().ok())
}

/// The caller's task context, or a request context on the process runtime.
#[must_use]
pub fn cx() -> Option<Cx> {
    Cx::current().or_else(|| {
        process_runtime()
            .ok()?
            .try_request_cx_with_budget(Budget::INFINITE)
            .ok()
    })
}

/// Starts the process runtime, so a daemon fails at startup rather than on
/// its first storage operation.
pub fn process_runtime() -> Result<RuntimeHandle, String> {
    PROCESS_RUNTIME
        .get_or_init(|| {
            RuntimeBuilder::multi_thread()
                .thread_name_prefix("tracedecay-native")
                .blocking_threads(1, MAX_BLOCKING_THREADS)
                .build()
                .map_err(|error| {
                    let message = format!("native execution runtime failed to start: {error}");
                    tracing::error!(%message);
                    message
                })
        })
        .as_ref()
        .map(Runtime::handle)
        .map_err(Clone::clone)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn foreign_executor_awaits_native_settlement() {
        assert!(Cx::current().is_none());
        let cx = cx().expect("process runtime context");
        let mut task = cx
            .spawn(|_| async {
                asupersync::time::sleep(asupersync::time::wall_now(), Duration::from_millis(20))
                    .await;
                Cx::current().is_some()
            })
            .expect("native task");
        let on_native_worker = tokio::time::timeout(Duration::from_secs(10), task.join(&cx))
            .await
            .expect("a Tokio waiter must wake when native settlement finishes")
            .expect("native join");
        assert!(on_native_worker, "settlement must run on a native worker");
        let mut blocking = cx.spawn_blocking(|_| 7).expect("native blocking task");
        assert_eq!(blocking.join(&cx).await.expect("native blocking join"), 7);
    }
}
