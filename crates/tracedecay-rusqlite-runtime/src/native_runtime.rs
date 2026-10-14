//! The native execution runtime that storage settlement runs on.
//!
//! Storage, shard lifecycle, and session-memory paths spawn their settlement
//! work on an Asupersync runtime so accepted work outlives a cancelled request.
//! Callers that already run on a native runtime use it. Every other caller
//! (the Tokio daemon, CLI commands, and Tokio tests) shares one process
//! runtime, started on first use. Callers only await native join handles and
//! channel receivers, which wake through ordinary `Waker`s, so a foreign
//! executor never blocks on the native runtime.

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Instant;

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
    started_process_runtime().map(Runtime::handle)
}

/// Runs `future` to completion on the calling thread, with the tasks it
/// spawns running on the process runtime's workers. For dedicated threads
/// outside any native runtime: from a native thread it returns an error rather
/// than park a worker.
pub fn block_on<F: Future>(future: F) -> Result<F::Output, String> {
    if Runtime::current_handle().is_some() {
        return Err("native block_on called from a native runtime thread".to_owned());
    }
    Ok(started_process_runtime()?.block_on(future))
}

/// Runs `work` on the runtime's blocking pool and awaits its result.
///
/// The work is owned by the runtime, not by the caller's region: cancelling or
/// dropping the caller abandons only the wait. Work spawned through a request
/// `Cx` can instead be cancelled with that region before it starts, which
/// would strand anything the caller had already queued for it.
pub async fn spawn_blocking<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let value = drive_blocking(Box::new(move || Box::new(work()))).await?;
    value
        .downcast::<T>()
        .map(|value| *value)
        .map_err(|_| "native blocking work returned an unexpected type".to_owned())
}

struct AdmittedBlocking {
    cx: Cx,
    receiver: asupersync::channel::oneshot::Receiver<Box<dyn Any + Send>>,
}

/// Admits `work` on the blocking pool before any future is built.
///
/// [`RuntimeHandle`] is self-referential through `BrowserWorkerPump::self_weak`.
/// A future that captures it makes every `Send` proof overflow
/// (rust-lang/rust#159228). The handle is used only to admit the closure;
/// the returned wait holds the context and the oneshot, not the handle.
fn admit_blocking(
    work: Box<dyn FnOnce() -> Box<dyn Any + Send> + Send>,
) -> Result<AdmittedBlocking, String> {
    let runtime = handle().ok_or("native execution runtime unavailable")?;
    let cx = cx().ok_or("native execution context unavailable")?;
    let (sender, receiver) = asupersync::channel::oneshot::channel();
    runtime
        .spawn_blocking(move || {
            let _ = sender.send_blocking(work());
        })
        .ok_or("native blocking pool unavailable")?;
    Ok(AdmittedBlocking { cx, receiver })
}

fn drive_blocking(
    work: Box<dyn FnOnce() -> Box<dyn Any + Send> + Send>,
) -> Pin<Box<dyn Future<Output = Result<Box<dyn Any + Send>, String>> + Send>> {
    let admitted = admit_blocking(work);
    Box::pin(async move {
        let AdmittedBlocking { cx, mut receiver } = admitted?;
        receiver
            .recv(&cx)
            .await
            .map_err(|error| format!("native blocking work ended without a result: {error}"))
    })
}

/// Completes at `deadline` on a native timer task, so a foreign executor never
/// polls an Asupersync timer: its wait wakes through the join's `Waker`.
/// Dropping the returned future cancels the timer.
pub async fn sleep_until(deadline: Instant) -> Result<(), String> {
    let cx = cx().ok_or("native execution context unavailable")?;
    let mut timer = cx
        .spawn(move |cx| async move {
            // Measured before the timer clock is read, so the wake never lands
            // before `deadline`.
            let remaining = deadline.saturating_duration_since(Instant::now());
            asupersync::time::sleep(cx.now(), remaining).await;
        })
        .map_err(|error| format!("native timer was not admitted: {error}"))?;
    timer
        .join(&cx)
        .await
        .map_err(|error| format!("native timer ended without firing: {error}"))
}

fn started_process_runtime() -> Result<&'static Runtime, String> {
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

    #[tokio::test(flavor = "multi_thread")]
    async fn foreign_executor_wakes_at_a_native_deadline() {
        let deadline = Instant::now() + Duration::from_millis(50);
        tokio::time::timeout(Duration::from_secs(10), sleep_until(deadline))
            .await
            .expect("a Tokio waiter must wake when the native timer fires")
            .expect("native timer");
        assert!(Instant::now() >= deadline, "the timer must not fire early");
    }

    #[test]
    fn blocking_work_outlives_a_cancelled_caller_region() {
        use std::future::poll_fn;
        use std::pin::pin;
        use std::task::Poll;

        let runtime = RuntimeBuilder::current_thread()
            .blocking_threads(1, 1)
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let cx = Cx::current().expect("block_on context");
            let (release, held) = std::sync::mpsc::channel::<()>();
            Runtime::current_handle()
                .expect("test runtime handle")
                .spawn_blocking(move || {
                    let _ = held.recv();
                })
                .expect("pool blocker");
            let (ran, settled) = std::sync::mpsc::channel::<()>();
            let (queued, mut entered) = asupersync::channel::oneshot::channel::<()>();
            let region = cx
                .open_child_region(asupersync::cx::child_region::ChildRegionSpec::inherit())
                .await
                .expect("caller region");
            let mut caller = region
                .cx()
                .spawn(move |_| async move {
                    let mut pending = pin!(spawn_blocking(move || {
                        let _ = ran.send(());
                    }));
                    poll_fn(|context| {
                        assert!(pending.as_mut().poll(context).is_pending());
                        Poll::Ready(())
                    })
                    .await;
                    let _ = queued.send_blocking(());
                    pending.await
                })
                .expect("caller task");
            entered
                .recv(&cx)
                .await
                .expect("work queued behind the blocker");
            region
                .cancel(asupersync::types::CancelReason::user("caller cancelled"))
                .expect("cancel caller region");
            let _ = caller.join(&cx).await;
            region.close().await.expect("close caller region");
            release.send(()).expect("release blocker");
            settled
                .recv_timeout(Duration::from_secs(5))
                .expect("queued work must still run after its caller's region is cancelled");
        });
    }

    #[test]
    fn dedicated_thread_blocks_on_native_settlement() {
        let refused_on_worker = std::thread::spawn(|| {
            block_on(async {
                let cx = cx().expect("block_on context");
                let mut task = cx
                    .spawn(|_| async { block_on(async {}).is_err() })
                    .expect("native task");
                task.join(&cx).await.expect("native join")
            })
        })
        .join()
        .expect("dedicated thread")
        .expect("process runtime");
        assert!(refused_on_worker, "block_on must not park a native worker");
    }
}
