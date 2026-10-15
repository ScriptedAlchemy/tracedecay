//! The native execution runtime that storage settlement runs on.
//!
//! Storage, shard lifecycle, and session-memory paths spawn their settlement
//! work on an Asupersync runtime so accepted work outlives a cancelled request.
//! Callers that already run on a native runtime use it. Every other caller
//! (the Tokio daemon, CLI commands, and Tokio tests) shares one process
//! runtime: the one the host [`install`]s at startup, or a default one started
//! on first use when the host never installs. Callers only await native join handles and
//! channel receivers, which wake through ordinary `Waker`s, so a foreign
//! executor never blocks on the native runtime.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use asupersync::runtime::{JoinError, RuntimeHandle};
pub use asupersync::runtime::{Runtime, RuntimeBuilder};

pub use crate::exact_sql::native_thread_writer_waits;
use asupersync::types::PanicPayload;
use asupersync::{Budget, Cx};
use futures_util::future::{AbortHandle, Aborted, BoxFuture, FutureExt, Shared, abortable};

/// Tokio's default blocking-pool ceiling. Shard closes and SQL reads park a
/// blocking thread for their whole duration.
pub const MAX_BLOCKING_THREADS: usize = 512;

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
///
/// Boxed so callers prove the context and channel wait `Send` once, here,
/// instead of inside every future that awaits blocking work
/// (rust-lang/rust#159228).
pub fn spawn_blocking<T, F>(work: F) -> BoxFuture<'static, Result<T, String>>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let queued = queue_blocking(work);
    Box::pin(async move {
        let (cx, mut receiver) = queued?;
        receiver
            .recv(&cx)
            .await
            .map_err(|error| format!("native blocking work ended without a result: {error}"))
    })
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

/// Drives `settlement` on the caller's executor. A caller that stops waiting
/// hands the unfinished settlement to the runtime, so accepted work still
/// outlives a cancelled request. Spawning a native task for every call instead
/// costs a runtime admission and two cross-runtime wakes per SQL statement.
pub fn settle<F>(settlement: F) -> Settlement<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    Settlement(Some(Box::pin(settlement)))
}

/// The future [`settle`] returns.
pub struct Settlement<T: Send + 'static>(Option<Pin<Box<dyn Future<Output = T> + Send>>>);

impl<T: Send + 'static> Future for Settlement<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<T> {
        // Held outside `self` while polled: a panic unwinds it on the caller
        // instead of leaving it for `Drop` to hand to the runtime.
        let pending = &mut self.get_mut().0;
        let mut settlement = pending.take().expect("settlement polled after completion");
        let poll = settlement.as_mut().poll(context);
        if poll.is_pending() {
            *pending = Some(settlement);
        }
        poll
    }
}

impl<T: Send + 'static> Drop for Settlement<T> {
    fn drop(&mut self) {
        let Some(settlement) = self.0.take() else {
            return;
        };
        let detached = handle()
            .ok_or_else(|| "native execution runtime unavailable".to_owned())
            .and_then(|runtime| {
                runtime
                    .try_spawn(settlement)
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = detached {
            tracing::error!(%error, "abandoned native settlement was dropped unfinished");
        }
    }
}

/// Completes at `deadline` on a native timer task, so a foreign executor never
/// polls an Asupersync timer: its wait wakes through the join's `Waker`.
/// Dropping the returned future cancels the timer.
///
/// Boxed so every timed caller proves this future `Send` once, here, instead
/// of nesting the timer task inside its own proof (rust-lang/rust#159228).
pub fn sleep_until(deadline: Instant) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let cx = cx().ok_or("native execution context unavailable")?;
        let mut timer = cx
            .spawn(move |cx| async move {
                // Measured before the timer clock is read, so the wake never
                // lands before `deadline`.
                let remaining = deadline.saturating_duration_since(Instant::now());
                asupersync::time::sleep(cx.now(), remaining).await;
            })
            .map_err(|error| format!("native timer was not admitted: {error}"))?;
        timer
            .join(&cx)
            .await
            .map_err(|error| format!("native timer ended without firing: {error}"))
    })
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
pub fn timeout_at<F: Future>(deadline: Instant, future: F) -> Timeout<F> {
    race_timer(future, Some(sleep_until(deadline)))
}

/// [`timeout_at`] `duration` from now. A duration past the clock's range waits
/// without a deadline.
pub fn timeout<F: Future>(duration: Duration, future: F) -> Timeout<F> {
    race_timer(
        future,
        Instant::now().checked_add(duration).map(sleep_until),
    )
}

fn race_timer<F: Future>(
    future: F,
    timer: Option<BoxFuture<'static, Result<(), String>>>,
) -> Timeout<F> {
    Timeout {
        future: Box::pin(future),
        timer,
    }
}

/// The future [`timeout_at`] and [`timeout`] return.
///
/// A named future over boxed fields, not an `async fn`, so a timed caller's
/// `Send` proof crosses two pointers instead of three nested coroutines
/// (rust-lang/rust#159228).
pub struct Timeout<F> {
    future: Pin<Box<F>>,
    timer: Option<BoxFuture<'static, Result<(), String>>>,
}

impl<F: Future> Future for Timeout<F> {
    type Output = Result<F::Output, TimeoutError>;

    fn poll(mut self: Pin<&mut Self>, task: &mut Context<'_>) -> Poll<Self::Output> {
        if let Poll::Ready(output) = self.future.as_mut().poll(task) {
            return Poll::Ready(Ok(output));
        }
        let Some(timer) = self.timer.as_mut() else {
            return Poll::Pending;
        };
        timer.as_mut().poll(task).map(|fired| {
            Err(fired.map_or_else(TimeoutError::TimerUnavailable, |()| TimeoutError::Elapsed))
        })
    }
}

/// The most CPU time one poll of a [`NativeTask`] may take in a debug build.
///
/// While a native worker is inside a poll, no other thread may be turning the
/// runtime's reactor: on asupersync 0.6.0 a reactor leader that leaves to run
/// a task promotes no parked follower. Every socket wake and timer on the
/// runtime then waits for that poll to return (#3994, upstream
/// Dicklesworthstone/asupersync#83).
pub const LONG_POLL_CPU: Duration = Duration::from_millis(250);

/// Fails a debug build's native task when one poll exceeds [`LONG_POLL_CPU`]
/// of thread CPU time. CPU time, not wall time, so a worker descheduled on a
/// loaded host does not trip it.
#[cfg(all(debug_assertions, unix))]
struct LongPollGuard<F> {
    work: F,
    site: &'static std::panic::Location<'static>,
}

#[cfg(all(debug_assertions, unix))]
impl<F: Future + Unpin> Future for LongPollGuard<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, task: &mut Context<'_>) -> Poll<F::Output> {
        let started = thread_cpu_time();
        let poll = Pin::new(&mut self.work).poll(task);
        if let (Some(started), Some(ended)) = (started, thread_cpu_time()) {
            let spent = ended.saturating_sub(started);
            assert!(
                spent <= LONG_POLL_CPU,
                "native task spawned at {} spent {spent:?} of CPU in one poll, past \
                 {LONG_POLL_CPU:?}; move its synchronous work to native_runtime::spawn_blocking \
                 (#3994)",
                self.site,
            );
        }
        poll
    }
}

#[cfg(all(debug_assertions, unix))]
fn thread_cpu_time() -> Option<Duration> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid, exclusively borrowed `timespec` for the call.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut now) };
    (status == 0).then(|| {
        Duration::new(
            u64::try_from(now.tv_sec).unwrap_or(0),
            u32::try_from(now.tv_nsec).unwrap_or(0),
        )
    })
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
    ///
    /// `work` must not run CPU-heavy synchronous code inside one poll; see
    /// [`LONG_POLL_CPU`]. Queue that part with [`spawn_blocking`].
    #[track_caller]
    pub fn spawn<F>(work: F) -> Result<Self, String>
    where
        F: Future<Output = T> + Send + 'static,
    {
        let runtime = handle().ok_or("native execution runtime unavailable")?;
        // Boxed so the runtime's own spawn wrappers do not nest `work` in
        // their `Send` proof (rust-lang/rust#159228).
        let work: BoxFuture<'static, T> = Box::pin(work);
        #[cfg(all(debug_assertions, unix))]
        let work = LongPollGuard {
            work,
            site: std::panic::Location::caller(),
        };
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

/// Why [`install`] refused a runtime.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    /// A native helper already started the default runtime, or a runtime was
    /// already installed. Helpers would otherwise run on a runtime the host did
    /// not configure.
    #[error("native execution runtime already started")]
    AlreadyStarted,
    #[error("{0}")]
    StartFailed(String),
}

/// Builds `builder` and makes it the process runtime every helper here uses.
/// A host installs it at startup, before any native helper runs; a host that
/// never installs gets a default runtime on first use.
pub fn install(builder: RuntimeBuilder) -> Result<&'static Runtime, InstallError> {
    let mut installed = false;
    let runtime = PROCESS_RUNTIME.get_or_init(|| {
        installed = true;
        start(builder)
    });
    if !installed {
        return Err(InstallError::AlreadyStarted);
    }
    runtime
        .as_ref()
        .map_err(|message| InstallError::StartFailed(message.clone()))
}

static TOKIO_RUNTIME_MISSING: Mutex<BTreeMap<&'static str, u64>> = Mutex::new(BTreeMap::new());

/// The Tokio runtime entered on this thread, for a `site` whose work still
/// needs one. A miss is logged and counted against `site`: the caller skips
/// that work, which would otherwise stop silently once the caller runs on the
/// native runtime.
#[must_use]
pub fn tokio_handle(site: &'static str) -> Option<tokio::runtime::Handle> {
    let handle = tokio::runtime::Handle::try_current().ok();
    if handle.is_none() {
        *TOKIO_RUNTIME_MISSING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(site)
            .or_default() += 1;
        tracing::error!(site, "Tokio runtime missing; path skipped");
    }
    handle
}

/// How many times [`tokio_handle`] found no Tokio runtime for `site` in this
/// process.
#[must_use]
pub fn tokio_runtime_missing_count(site: &str) -> u64 {
    TOKIO_RUNTIME_MISSING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(site)
        .copied()
        .unwrap_or(0)
}

/// How many times [`tokio_handle`] found no Tokio runtime in this process,
/// over every site.
#[must_use]
pub fn tokio_runtime_missing_total() -> u64 {
    TOKIO_RUNTIME_MISSING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .values()
        .sum()
}

fn started_process_runtime() -> Result<&'static Runtime, String> {
    PROCESS_RUNTIME
        .get_or_init(|| {
            start(
                RuntimeBuilder::multi_thread()
                    .thread_name_prefix("tracedecay-native")
                    .blocking_threads(1, MAX_BLOCKING_THREADS),
            )
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn start(builder: RuntimeBuilder) -> Result<Runtime, String> {
    builder.build().map_err(|error| {
        let message = format!("native execution runtime failed to start: {error}");
        tracing::error!(%message);
        message
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    const HANG_GUARD: Duration = Duration::from_secs(10);

    #[test]
    fn a_missing_tokio_runtime_is_counted_per_site_and_a_present_one_is_not() {
        const SITE: &str = "native_runtime.tests.tokio_handle";
        let before = tokio_runtime_missing_count(SITE);
        let before_total = tokio_runtime_missing_total();
        assert!(tokio_handle(SITE).is_none());
        assert_eq!(tokio_runtime_missing_count(SITE), before + 1);
        assert_eq!(tokio_runtime_missing_total(), before_total + 1);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("tokio runtime");
        let _entered = runtime.enter();
        assert!(tokio_handle(SITE).is_some());
        assert_eq!(tokio_runtime_missing_count(SITE), before + 1);
        assert_eq!(tokio_runtime_missing_total(), before_total + 1);
    }

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
            race_timer(async { 7 }, Some(Box::pin(unavailable()))).now_or_never(),
            Some(Ok(7))
        );
        assert_eq!(
            race_timer(std::future::pending::<()>(), Some(Box::pin(unavailable()))).now_or_never(),
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

    #[tokio::test(flavor = "current_thread")]
    async fn settlement_runs_on_the_waiting_caller() {
        let caller = std::thread::current().id();
        assert_eq!(settle(async { std::thread::current().id() }).await, caller);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropped_settlement_finishes_on_the_runtime() {
        let (release, held) = async_channel::bounded::<()>(1);
        let (finished, settled) = std::sync::mpsc::channel::<()>();
        let mut settlement = settle(async move {
            let _ = held.recv().await;
            let _ = finished.send(());
        });
        let polled =
            std::future::poll_fn(|context| Poll::Ready(Pin::new(&mut settlement).poll(context)))
                .await;
        assert!(polled.is_pending());
        drop(settlement);
        release.send(()).await.expect("settlement still waiting");
        assert_eq!(
            settled.recv_timeout(Duration::from_secs(10)),
            Ok(()),
            "a settlement its caller dropped must still finish"
        );
    }

    #[test]
    fn panicking_settlement_unwinds_on_the_caller_and_is_never_polled_again() {
        struct PanicsWhenPolled {
            polls: usize,
            dropped: std::sync::mpsc::Sender<usize>,
        }
        impl Future for PanicsWhenPolled {
            type Output = ();
            fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
                self.polls += 1;
                panic!("settlement panicked");
            }
        }
        impl Drop for PanicsWhenPolled {
            fn drop(&mut self) {
                let _ = self.dropped.send(self.polls);
            }
        }
        let (dropped, polls) = std::sync::mpsc::channel();
        let unwound = std::panic::catch_unwind(move || {
            let mut settlement = settle(PanicsWhenPolled { polls: 0, dropped });
            let _ =
                Pin::new(&mut settlement).poll(&mut Context::from_waker(std::task::Waker::noop()));
        });
        assert!(unwound.is_err(), "the panic must reach the caller");
        assert_eq!(
            polls.recv_timeout(Duration::from_secs(10)),
            Ok(1),
            "a panicked settlement must not be handed to the runtime and polled again"
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
    fn install_after_the_default_runtime_started_is_refused() {
        process_runtime().expect("default process runtime");
        assert_eq!(
            install(RuntimeBuilder::multi_thread()).err(),
            Some(InstallError::AlreadyStarted)
        );
    }

    const SPIN: Duration = Duration::from_millis(150);
    const WAKE_BOUND: Duration = Duration::from_millis(40);

    fn spin(duration: Duration) {
        let spun = Instant::now();
        while spun.elapsed() < duration {
            std::hint::spin_loop();
        }
    }

    /// #3994: on two workers, task A wakes on its socket, so the idle reactor
    /// leader runs it, then spins for [`SPIN`], inline in that poll or on the
    /// blocking pool. Task B's socket becomes readable 10 ms into the spin.
    /// Returns how long after that B woke.
    fn socket_wake_behind_a_spinning_task(spin_on_blocking_pool: bool) -> Duration {
        use asupersync::io::AsyncReadExt;
        use asupersync::net::unix::UnixStream;
        use std::io::Write;

        let runtime = RuntimeBuilder::multi_thread()
            .worker_threads(2)
            .blocking_threads(1, 1)
            .build()
            .expect("test runtime");
        let (mut a_tx, a_rx) = std::os::unix::net::UnixStream::pair().expect("socket a");
        let (mut b_tx, b_rx) = std::os::unix::net::UnixStream::pair().expect("socket b");
        let (woke, wakes) = std::sync::mpsc::channel::<(char, Instant)>();
        let spawn_reader = |name: char, rx: std::os::unix::net::UnixStream, then: Duration| {
            let woke = woke.clone();
            runtime
                .handle()
                .try_spawn(async move {
                    rx.set_nonblocking(true).expect("nonblocking");
                    let mut rx = UnixStream::from_std(rx).expect("native socket");
                    rx.read_exact(&mut [0_u8; 1]).await.expect("read");
                    let _ = woke.send((name, Instant::now()));
                    if spin_on_blocking_pool {
                        spawn_blocking(move || spin(then)).await.expect("pool spin");
                    } else {
                        spin(then);
                    }
                })
                .expect("spawn reader")
        };
        let _a = spawn_reader('a', a_rx, SPIN);
        let _b = spawn_reader('b', b_rx, Duration::ZERO);
        // Both readers park on the reactor and both workers go idle.
        std::thread::sleep(Duration::from_millis(100));

        a_tx.write_all(b"a").expect("wake a");
        assert_eq!(wakes.recv_timeout(HANG_GUARD).expect("a wakes").0, 'a');
        std::thread::sleep(Duration::from_millis(10));
        let readable = Instant::now();
        b_tx.write_all(b"b").expect("wake b");
        let (name, woke_at) = wakes.recv_timeout(HANG_GUARD).expect("b wakes");
        assert_eq!(name, 'b');
        woke_at.duration_since(readable)
    }

    #[test]
    #[ignore = "asupersync 0.6.0 promotes no parked follower when its reactor leader runs a \
                task (#3994); un-ignore on the rev that fixes asupersync#83"]
    fn a_long_poll_does_not_delay_another_tasks_socket_wake() {
        let latency = socket_wake_behind_a_spinning_task(false);
        assert!(
            latency < WAKE_BOUND,
            "b woke {latency:?} after its socket became readable, behind a's {SPIN:?} poll"
        );
    }

    #[test]
    fn work_on_the_blocking_pool_does_not_delay_another_tasks_socket_wake() {
        let latency = socket_wake_behind_a_spinning_task(true);
        assert!(
            latency < WAKE_BOUND,
            "b woke {latency:?} after its socket became readable, behind a's {SPIN:?} pool spin"
        );
    }

    #[cfg(all(debug_assertions, unix))]
    #[test]
    fn a_native_task_poll_past_the_cpu_bound_fails_in_debug_builds() {
        let runtime = RuntimeBuilder::multi_thread()
            .worker_threads(1)
            .build()
            .expect("test runtime");
        let outcome = runtime.block_on(async {
            NativeTask::spawn(async { spin(LONG_POLL_CPU + Duration::from_millis(50)) })
                .expect("native task")
                .wait()
                .await
        });
        let NativeTaskOutcome::Panicked(payload) = outcome else {
            panic!("a {LONG_POLL_CPU:?} poll must fail the task, got {outcome:?}");
        };
        assert!(
            payload.to_string().contains("spawn_blocking"),
            "the failure names the remedy: {payload}"
        );
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
