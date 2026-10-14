//! The native execution runtime that storage settlement runs on.
//!
//! Storage, shard lifecycle, and session-memory paths spawn their settlement
//! work on an Asupersync runtime so accepted work outlives a cancelled request.
//! Callers that already run on a native runtime use it. Every other caller
//! (the Tokio daemon, CLI commands, and Tokio tests) shares one process
//! runtime, started on first use. Callers only await native join handles and
//! channel receivers, which wake through ordinary `Waker`s, so a foreign
//! executor never blocks on the native runtime.

use std::future::poll_fn;
use std::pin::pin;
use std::sync::OnceLock;
use std::task::Poll;
use std::time::{Duration, Instant};

use asupersync::runtime::{JoinError, Runtime, RuntimeBuilder, RuntimeHandle};
use asupersync::types::PanicPayload;
use asupersync::{Budget, Cx};
use futures_util::future::{AbortHandle, Aborted, BoxFuture, FutureExt, Shared, abortable};

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

/// Queues `work` on the runtime's blocking pool now, at the call, and returns
/// a future of its result, like Tokio's `spawn_blocking`.
///
/// The work is owned by the runtime, not by the caller's region: cancelling or
/// dropping the returned future abandons only the wait, and work whose future
/// is never polled still runs. Work spawned through a request `Cx` can instead
/// be cancelled with that region before it starts, which would strand anything
/// the caller had already queued for it.
pub fn spawn_blocking<T, F>(work: F) -> impl Future<Output = Result<T, String>> + Send + 'static
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let queued = queue_blocking(work);
    async move {
        let (cx, mut receiver) = queued?;
        receiver
            .recv(&cx)
            .await
            .map_err(|error| format!("native blocking work ended without a result: {error}"))
    }
}

fn queue_blocking<T, F>(work: F) -> Result<(Cx, asupersync::channel::oneshot::Receiver<T>), String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let runtime = handle().ok_or("native execution runtime unavailable")?;
    let cx = cx().ok_or("native execution context unavailable")?;
    let (sender, receiver) = asupersync::channel::oneshot::channel();
    runtime
        .spawn_blocking(move || {
            let _ = sender.send_blocking(work());
        })
        .ok_or("native blocking pool unavailable")?;
    Ok((cx, receiver))
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

/// Why [`timeout_at`] stopped waiting for its future.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TimeoutError {
    #[error("deadline elapsed")]
    Elapsed,
    /// The native timer could not start, so the wait fails closed rather than
    /// run unbounded.
    #[error("native timer unavailable: {0}")]
    TimerUnavailable(String),
}

/// Awaits `future` until `deadline`, timed by [`sleep_until`]. The future is
/// polled first, so one that is already ready wins even past its deadline and
/// never starts a timer.
pub async fn timeout_at<F: Future>(
    deadline: Instant,
    future: F,
) -> Result<F::Output, TimeoutError> {
    race_timer(future, sleep_until(deadline)).await
}

/// [`timeout_at`] `duration` from now. A duration past the clock's range waits
/// without a deadline.
pub async fn timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, TimeoutError> {
    match Instant::now().checked_add(duration) {
        Some(deadline) => timeout_at(deadline, future).await,
        None => Ok(future.await),
    }
}

async fn race_timer<F: Future>(
    future: F,
    timer: impl Future<Output = Result<(), String>>,
) -> Result<F::Output, TimeoutError> {
    let mut future = pin!(future);
    let mut timer = pin!(timer);
    poll_fn(|task| {
        if let Poll::Ready(output) = future.as_mut().poll(task) {
            return Poll::Ready(Ok(output));
        }
        timer.as_mut().poll(task).map(|fired| {
            Err(fired.map_or_else(TimeoutError::TimerUnavailable, |()| TimeoutError::Elapsed))
        })
    })
    .await
}

/// How a [`NativeTask`] ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeTaskOutcome<T> {
    Completed(T),
    /// Aborted through [`NativeTask::abort`], or dropped by its runtime.
    Cancelled,
    Panicked(PanicPayload),
}

/// A task on the native runtime that any executor can abort and await.
///
/// The runtime's own `JoinHandle` has no abort and panics when awaited after
/// its task was cancelled or dropped; this handle reports both as an outcome.
/// Every waiter shares the one terminal outcome, and a cancelled wait loses
/// nothing. Dropping the handle detaches the task: it runs to completion
/// unless aborted first.
pub struct NativeTask<T> {
    outcome: Shared<BoxFuture<'static, NativeTaskOutcome<T>>>,
    abort: AbortHandle,
}

impl<T: Clone + Send + Sync + 'static> NativeTask<T> {
    /// Spawns `work` on the runtime the caller runs on, or the process
    /// runtime. The runtime owns it, not the caller's region, so it outlives
    /// the request that started it.
    pub fn spawn<F>(work: F) -> Result<Self, String>
    where
        F: Future<Output = T> + Send + 'static,
    {
        let runtime = handle().ok_or("native execution runtime unavailable")?;
        let (work, abort) = abortable(work);
        let join = runtime
            .try_spawn_checked(work)
            .map_err(|error| format!("native task was not admitted: {error}"))?;
        let outcome = async move {
            match join.await {
                Ok(Ok(value)) => NativeTaskOutcome::Completed(value),
                Err(JoinError::Panicked(payload)) => NativeTaskOutcome::Panicked(payload),
                // `Shared` never polls a finished join again.
                Ok(Err(Aborted))
                | Err(JoinError::Cancelled(_) | JoinError::PolledAfterCompletion) => {
                    NativeTaskOutcome::Cancelled
                }
            }
        };
        Ok(Self {
            outcome: outcome.boxed().shared(),
            abort,
        })
    }

    /// Drops the work at its next poll. Work inside a synchronous call runs on
    /// until it yields.
    pub fn abort(&self) {
        self.abort.abort();
    }

    pub async fn wait(&self) -> NativeTaskOutcome<T> {
        self.outcome.clone().await
    }

    #[must_use]
    pub fn try_finished(&self) -> Option<NativeTaskOutcome<T>> {
        self.outcome.clone().now_or_never()
    }

    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.try_finished().is_some()
    }
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    const HANG_GUARD: Duration = Duration::from_secs(10);

    #[tokio::test(flavor = "multi_thread")]
    async fn timeout_returns_a_value_that_lands_before_the_deadline() {
        let output = timeout(HANG_GUARD, async {
            sleep_until(Instant::now() + Duration::from_millis(10))
                .await
                .expect("native timer");
            7
        })
        .await;
        assert_eq!(output, Ok(7));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timeout_elapses_at_the_deadline_for_a_tokio_caller() {
        let deadline = Instant::now() + Duration::from_millis(50);
        let outcome = tokio::time::timeout(
            HANG_GUARD,
            timeout_at(deadline, std::future::pending::<()>()),
        )
        .await
        .expect("a Tokio waiter must wake when the native deadline passes");
        assert_eq!(outcome, Err(TimeoutError::Elapsed));
        assert!(
            Instant::now() >= deadline,
            "the deadline must not fire early"
        );
    }

    #[test]
    fn timeout_runs_inside_a_native_task() {
        let task = NativeTask::spawn(async {
            let ready = timeout(HANG_GUARD, async { 7 }).await;
            let deadline = Instant::now() + Duration::from_millis(50);
            let pending = timeout_at(deadline, std::future::pending::<()>()).await;
            (ready, pending, Instant::now() >= deadline)
        })
        .expect("native task");
        let outcome = block_on(task.wait()).expect("process runtime");
        assert_eq!(
            outcome,
            NativeTaskOutcome::Completed((Ok(7), Err(TimeoutError::Elapsed), true))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_past_deadline_takes_a_ready_value_and_elapses_a_pending_one() {
        let past = Instant::now() - Duration::from_millis(1);
        assert_eq!(timeout_at(past, async { 7 }).await, Ok(7));
        assert_eq!(
            timeout_at(past, std::future::pending::<()>()).await,
            Err(TimeoutError::Elapsed)
        );
    }

    #[test]
    fn an_unavailable_timer_fails_closed_after_a_ready_value() {
        let unavailable = || async { Err::<(), _>("runtime stopped".to_owned()) };
        assert_eq!(
            race_timer(async { 7 }, unavailable()).now_or_never(),
            Some(Ok(7))
        );
        assert_eq!(
            race_timer(std::future::pending::<()>(), unavailable()).now_or_never(),
            Some(Err(TimeoutError::TimerUnavailable(
                "runtime stopped".to_owned()
            )))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_waiter_shares_the_completed_value() {
        let (release, released) = tokio::sync::oneshot::channel::<u32>();
        let task = NativeTask::spawn(async move { released.await.expect("released value") })
            .expect("native task");
        assert!(
            task.wait().now_or_never().is_none(),
            "a dropped wait must leave the task running"
        );
        assert!(!task.is_finished());
        let (first, second) = tokio::join!(task.wait(), async {
            release.send(7).expect("task waits for its value");
            task.wait().await
        });
        assert_eq!(first, NativeTaskOutcome::Completed(7));
        assert_eq!(second, NativeTaskOutcome::Completed(7));
        assert!(task.is_finished());
        assert_eq!(task.try_finished(), Some(NativeTaskOutcome::Completed(7)));
    }

    #[test]
    fn an_abort_before_the_first_poll_never_runs_the_work() {
        let runtime = RuntimeBuilder::current_thread()
            .build()
            .expect("test runtime");
        let ran = Arc::new(AtomicBool::new(false));
        let outcome = runtime.block_on({
            let ran = Arc::clone(&ran);
            async move {
                let task = NativeTask::spawn(async move { ran.store(true, Ordering::Release) })
                    .expect("native task");
                task.abort();
                task.wait().await
            }
        });
        assert_eq!(outcome, NativeTaskOutcome::Cancelled);
        assert!(!ran.load(Ordering::Acquire), "aborted work must not run");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_abort_drops_running_work() {
        struct SignalOnDrop(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for SignalOnDrop {
            fn drop(&mut self) {
                if let Some(signal) = self.0.take() {
                    let _ = signal.send(());
                }
            }
        }
        let (started, running) = tokio::sync::oneshot::channel();
        let (dropping, dropped) = tokio::sync::oneshot::channel();
        let task = NativeTask::spawn(async move {
            let _signal = SignalOnDrop(Some(dropping));
            let _ = started.send(());
            std::future::pending::<()>().await;
        })
        .expect("native task");
        running.await.expect("work started");
        task.abort();
        let outcome = tokio::time::timeout(HANG_GUARD, task.wait())
            .await
            .expect("an aborted task must settle");
        assert_eq!(outcome, NativeTaskOutcome::Cancelled);
        tokio::time::timeout(HANG_GUARD, dropped)
            .await
            .expect("abort must drop the running work")
            .expect("the work signals its drop");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_panic_is_an_outcome_not_a_resumed_panic() {
        let task =
            NativeTask::<()>::spawn(async { panic!("native task failure") }).expect("native task");
        let outcome = tokio::time::timeout(HANG_GUARD, task.wait())
            .await
            .expect("a panicked task must settle");
        let NativeTaskOutcome::Panicked(payload) = outcome else {
            panic!("expected a panic outcome, got {outcome:?}");
        };
        assert!(payload.message().contains("native task failure"));
    }

    #[test]
    fn a_task_dropped_by_its_runtime_reports_cancelled() {
        let runtime = RuntimeBuilder::current_thread()
            .build()
            .expect("test runtime");
        let task = runtime.block_on(async {
            NativeTask::spawn(std::future::pending::<()>()).expect("native task")
        });
        drop(runtime);
        assert_eq!(task.try_finished(), Some(NativeTaskOutcome::Cancelled));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_handle_detaches_the_work() {
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let (finishing, finished) = tokio::sync::oneshot::channel::<()>();
        drop(
            NativeTask::spawn(async move {
                released.await.expect("released");
                finishing.send(()).expect("test awaits completion");
            })
            .expect("native task"),
        );
        release.send(()).expect("detached work still waits");
        tokio::time::timeout(HANG_GUARD, finished)
            .await
            .expect("detached work must finish")
            .expect("detached work runs to completion");
    }

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

    #[test]
    fn blocking_work_starts_at_the_call_even_when_never_awaited() {
        let (ran, settled) = std::sync::mpsc::channel::<()>();
        drop(spawn_blocking(move || {
            let _ = ran.send(());
        }));
        assert_eq!(
            settled.recv_timeout(Duration::from_secs(10)),
            Ok(()),
            "work must run although its result future was dropped unpolled"
        );
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
