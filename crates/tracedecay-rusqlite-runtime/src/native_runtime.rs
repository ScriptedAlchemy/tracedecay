//! Shared Tokio execution services for storage and daemon-owned settlement.
//!
//! Accepted work belongs to the runtime, so dropping a request's wait does not
//! abandon its effects. Async callers reuse the runtime they already run on;
//! dedicated threads and the isolated storage pool use the process runtime
//! the host [`install`]s at startup. Without either runtime, helpers refuse
//! with [`NOT_INSTALLED`]. The `test-runtime` feature starts a default process
//! runtime on first use. Join results and timers wake through ordinary
//! `Waker`s, so callers can await them from any executor.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
pub use tokio::runtime::{Builder, Runtime};

pub use crate::exact_sql::{native_thread_reader_waits, native_thread_writer_waits};
use futures_util::future::{AbortHandle, Aborted, BoxFuture, FutureExt, Shared, abortable};

/// The native blocking pool's thread ceiling. Shard closes, SQL reads, and
/// [`off_native_thread`] handlers each hold a pool thread for their whole
/// duration, mostly parked, so this bounds concurrent waits rather than CPU
/// use. The value matches Tokio's `max_blocking_threads` default.
pub const MAX_BLOCKING_THREADS: usize = 512;

static PROCESS_RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();

/// Why a helper refused when the host never called [`install`].
pub const NOT_INSTALLED: &str = "native execution runtime not installed";

/// The runtime the caller runs on, or the process runtime.
///
/// `None` when no runtime was installed or the installed one failed to start.
#[must_use]
pub fn handle() -> Option<Handle> {
    native_handle().or_else(|| {
        process_runtime()
            .ok()
            .map(|runtime| runtime.handle().clone())
    })
}

fn native_handle() -> Option<Handle> {
    if crate::storage_pool::on_pool() {
        return None;
    }
    Handle::try_current().ok()
}

pub(crate) fn on_native_thread() -> bool {
    !ON_BLOCKING_POOL.get() && native_handle().is_some()
}

/// Whether the caller runs on the storage-aware blocking pool, where a
/// synchronous storage wait may park its thread.
#[must_use]
pub fn on_storage_blocking_pool() -> bool {
    ON_BLOCKING_POOL.get()
}

/// Runs `future` to completion on the calling thread, with the tasks it
/// spawns running on the process runtime's workers.
///
/// A multi-thread Tokio worker moves its task to a blocking thread first, so
/// the worker is not parked. A current-thread runtime cannot do that and is
/// refused rather than deadlocking its only worker.
pub fn block_on<F: Future>(future: F) -> Result<F::Output, String> {
    if ON_BLOCKING_POOL.get() {
        return Ok(process_runtime()?.block_on(future));
    }
    if let Ok(handle) = Handle::try_current() {
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            return Ok(tokio::task::block_in_place(|| handle.block_on(future)));
        }
        return Err("native block_on called from a current-thread runtime".to_owned());
    }
    Ok(process_runtime()?.block_on(future))
}

/// Queues `work` on the runtime's blocking pool now, at the call, and returns
/// a future of its result, like Tokio's `spawn_blocking`.
///
/// The work is owned by the runtime, not by the caller's region: cancelling or
/// dropping the returned future abandons only the wait, and work whose future
/// is never polled still runs. Request cancellation must not strand work that
/// the caller already queued for settlement.
///
/// Called on a blocking-pool thread that already runs work queued here (an
/// [`off_native_thread`] handler, say), it runs `work` inline at the call:
/// queuing would hold this pool thread while waiting for another, and a full
/// pool would deadlock on that wait.
///
/// Boxed so callers prove the context and channel wait `Send` once, here,
/// instead of inside every future that awaits blocking work
/// (rust-lang/rust#159228).
pub fn spawn_blocking<T, F>(work: F) -> BoxFuture<'static, Result<T, String>>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    if ON_BLOCKING_POOL.get() {
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
            .map_err(|_| "native blocking work panicked".to_owned());
        return Box::pin(std::future::ready(ran));
    }
    let queued = queue_blocking(work);
    Box::pin(async move {
        let receiver = queued?;
        receiver
            .await
            .map_err(|error| format!("native blocking work ended without a result: {error}"))
    })
}

/// Queues blocking work on an explicitly owned runtime and returns its task.
///
/// The scoped marker admits synchronous storage waits inside `work`. Tokio
/// enters its runtime on both async workers and blocking threads, so an
/// ambient handle alone cannot distinguish which threads may safely park.
/// Dropping the task handle detaches the admitted work, as with Tokio's
/// `spawn_blocking`.
pub fn spawn_blocking_on<T, F>(runtime: &Handle, work: F) -> tokio::task::JoinHandle<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    runtime.spawn_blocking(move || {
        let _scope = BlockingPoolScope::enter();
        work()
    })
}

tokio::task_local! {
    /// Answers for the effects of blocking work its scope queues through
    /// [`spawn_owned_blocking`]. Each such work holds a clone until it
    /// returns, so an owner that signals on its last drop (a project open's
    /// completion) settles after that work, not when its wait is abandoned.
    pub static BLOCKING_WORK_OWNER: std::sync::Arc<dyn Send + Sync>;
}

/// [`spawn_blocking`] for work whose effects belong to the enclosing
/// [`BLOCKING_WORK_OWNER`]: dropping the returned future still abandons only
/// the wait, but the owner stays held until the work returns.
pub fn spawn_owned_blocking<T, F>(work: F) -> BoxFuture<'static, Result<T, String>>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let owner = BLOCKING_WORK_OWNER.try_with(std::sync::Arc::clone).ok();
    spawn_blocking(move || {
        let _owner = owner;
        work()
    })
}

/// Runs `future` where a thread may park: from a native runtime thread it
/// moves the whole future to one blocking-pool thread and polls it there;
/// anywhere else it polls in place.
///
/// For request handlers that call synchronous storage ports, which wait on
/// the writer by parking their thread. Native wakes reach the pool thread
/// through an ordinary `Waker`, so the handler's awaits still work. As with
/// [`spawn_blocking`], dropping the returned future abandons only the wait:
/// a moved future runs to completion.
pub fn off_native_thread<F>(future: F) -> BoxFuture<'static, Result<F::Output, String>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    if !on_native_thread() {
        return Box::pin(future.map(Ok));
    }
    spawn_blocking(move || poll_on_current_thread(future))
}

/// Polls `future` on one blocking-pool thread wherever the caller runs.
///
/// For futures that interleave awaits with synchronous work (native Git, a
/// synchronous port) that must always run on a blocking-pool thread, even when
/// called outside an async runtime. Dropping the returned future abandons only
/// the wait.
pub fn on_blocking_pool<F>(future: F) -> BoxFuture<'static, Result<F::Output, String>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    spawn_blocking(move || poll_on_current_thread(future))
}

fn poll_on_current_thread<F: Future>(future: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: std::sync::Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(std::sync::Arc::new(Unpark(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        std::thread::park();
    }
}

fn queue_blocking<T, F>(work: F) -> Result<tokio::sync::oneshot::Receiver<T>, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let runtime = handle().ok_or("native execution runtime unavailable")?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    spawn_blocking_on(&runtime, move || {
        let _ = sender.send(work());
    });
    Ok(receiver)
}

thread_local! {
    /// Set only while a storage-aware blocking task runs, including nested
    /// inline work. A scope restores the prior value even if the work panics.
    static ON_BLOCKING_POOL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct BlockingPoolScope(bool);

impl BlockingPoolScope {
    fn enter() -> Self {
        Self(ON_BLOCKING_POOL.replace(true))
    }
}

impl Drop for BlockingPoolScope {
    fn drop(&mut self) {
        ON_BLOCKING_POOL.set(self.0);
    }
}

/// Drives `settlement` on the caller's executor. A caller that stops waiting
/// hands the unfinished settlement to the runtime selected at admission, so
/// accepted work still outlives a cancelled request or a change in the waiting
/// caller's executor. Spawning a native task for every call instead
/// costs a runtime admission and two cross-runtime wakes per SQL statement.
pub fn settle<F>(settlement: F) -> Settlement<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    Settlement {
        pending: Some(Box::pin(settlement)),
        runtime: handle(),
    }
}

/// The future [`settle`] returns.
pub struct Settlement<T: Send + 'static> {
    pending: Option<Pin<Box<dyn Future<Output = T> + Send>>>,
    runtime: Option<Handle>,
}

impl<T: Send + 'static> Future for Settlement<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<T> {
        // Held outside `self` while polled: a panic unwinds it on the caller
        // instead of leaving it for `Drop` to hand to the runtime.
        let pending = &mut self.get_mut().pending;
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
        let Some(settlement) = self.pending.take() else {
            return;
        };
        let detached = self
            .runtime
            .as_ref()
            .ok_or_else(|| "native execution runtime unavailable".to_owned())
            .map(|runtime| runtime.spawn(settlement));
        if let Err(error) = detached {
            tracing::error!(%error, "abandoned native settlement was dropped unfinished");
        }
    }
}

/// Completes at `deadline` on the selected Tokio runtime's timer driver.
///
/// The timer stays bound to its selected driver while any executor polls it.
/// A disabled or stopped driver returns an error. An elapsed deadline resolves
/// immediately, and dropping the future drops its timer.
pub fn sleep_until(deadline: Instant) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        if Instant::now() >= deadline {
            return Ok(());
        }
        let runtime = handle().ok_or("native execution runtime unavailable")?;
        std::panic::AssertUnwindSafe(async move {
            let timer = {
                let _entered = runtime.enter();
                tokio::time::sleep_until(deadline.into())
            };
            timer.await;
        })
        .catch_unwind()
        .await
        .map_err(|panic| {
            format!(
                "native timer driver unavailable: {}",
                panic_message(panic.as_ref())
            )
        })
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

/// The wall time past which one poll of a [`NativeTask`] counts as long.
///
/// While a worker is inside a poll, no other task queued on that worker runs.
/// A one-worker runtime cannot schedule other work until the poll returns.
/// Wall time includes computation, parking, and descheduling; thread CPU time
/// is logged beside it on unix to distinguish computation from waiting.
///
/// 100 ms is where a reply delayed behind the poll becomes noticeable to the
/// person waiting on a hook or MCP call. A poll's cost moves with host load
/// and opt level (a 7 ms fastbuild dispatch measured 490-590 ms of CPU at load
/// average ~1000), so this bound sets the floor of a latency report, not a
/// correctness line: a long poll is logged and counted, never failed.
pub const LONG_POLL: Duration = Duration::from_millis(100);

static LONG_POLLS: Mutex<BTreeMap<&'static std::panic::Location<'static>, u64>> =
    Mutex::new(BTreeMap::new());

/// Long native-task polls this process ran, by the site that spawned the
/// task. Each entry names work that held a native worker past [`LONG_POLL`]
/// and is a candidate for [`spawn_blocking`] when its latency matters.
#[must_use]
pub fn long_native_polls() -> BTreeMap<&'static std::panic::Location<'static>, u64> {
    LONG_POLLS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Times every poll of a [`NativeTask`] and counts the ones past
/// [`LONG_POLL`] against the task's spawn site. It never alters the poll's
/// result. Per poll it reads the monotonic clock twice and the thread CPU
/// clock once, plus once more after a long poll.
struct LongPollGuard<F> {
    work: F,
    site: &'static std::panic::Location<'static>,
}

impl<F: Future + Unpin> Future for LongPollGuard<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, task: &mut Context<'_>) -> Poll<F::Output> {
        let started = Instant::now();
        let cpu_started = thread_cpu_time();
        let poll = Pin::new(&mut self.work).poll(task);
        let wall = started.elapsed();
        if wall >= LONG_POLL {
            let cpu = cpu_started
                .zip(thread_cpu_time())
                .map(|(started, ended)| ended.saturating_sub(started));
            *LONG_POLLS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(self.site)
                .or_default() += 1;
            tracing::warn!(
                spawn_site = %self.site,
                ?wall,
                ?cpu,
                "native task held its worker for one poll past {LONG_POLL:?}; \
                 native_runtime::spawn_blocking keeps that work off the worker"
            );
        }
        poll
    }
}

#[cfg(unix)]
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

#[cfg(not(unix))]
fn thread_cpu_time() -> Option<Duration> {
    None
}

/// How long the scheduling-lateness canary sleeps between wakes.
const CANARY_TICK: Duration = Duration::from_millis(100);

/// What the scheduling-lateness canary measured in this process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchedulingLateness {
    pub ticks: u64,
    /// Wakes that ran [`LONG_POLL`] or more past their deadline.
    pub late_ticks: u64,
    pub worst: Duration,
}

static SCHEDULING_LATENESS: Mutex<SchedulingLateness> = Mutex::new(SchedulingLateness {
    ticks: 0,
    late_ticks: 0,
    worst: Duration::ZERO,
});

#[must_use]
pub fn scheduling_lateness() -> SchedulingLateness {
    *SCHEDULING_LATENESS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Starts a native task that sleeps on a native timer again and again and
/// records how late each wake runs. A wake runs late when no worker turns the
/// timer or picks up the woken task: a long poll on a one-worker runtime, or
/// every worker busy. That is the delay a socket reply waits behind, measured
/// directly rather than inferred from [`long_native_polls`]. A wake
/// [`LONG_POLL`] late or worse is logged. Abort the task to stop it.
#[track_caller]
pub fn spawn_lateness_canary() -> Result<NativeTask<()>, String> {
    NativeTask::spawn(async {
        loop {
            let deadline = Instant::now() + CANARY_TICK;
            if let Err(error) = sleep_until(deadline).await {
                tracing::warn!(%error, "scheduling-lateness canary stopped");
                return;
            }
            let late = deadline.elapsed();
            let mut lateness = SCHEDULING_LATENESS
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            lateness.ticks += 1;
            lateness.worst = lateness.worst.max(late);
            if late >= LONG_POLL {
                lateness.late_ticks += 1;
                tracing::warn!(
                    ?late,
                    "native runtime woke a timer late; a long poll or busy workers held it"
                );
            }
        }
    })
}

/// How a [`NativeTask`] ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeTaskOutcome<T> {
    Completed(T),
    /// Aborted through [`NativeTask::abort`], or dropped by its runtime.
    Cancelled,
    Panicked(Arc<str>),
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> Arc<str> {
    if let Some(message) = payload.downcast_ref::<&str>() {
        Arc::from(*message)
    } else if let Some(message) = payload.downcast_ref::<String>() {
        Arc::from(message.as_str())
    } else {
        Arc::from("task panicked with a non-string payload")
    }
}

/// A task on the native runtime that any executor can abort and await.
///
/// Every waiter shares one terminal outcome, including cancellation and panic,
/// and a cancelled wait loses
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
    /// A poll of `work` past [`LONG_POLL`] is logged and counted against the
    /// caller's location in [`long_native_polls`]. Queue blocking or
    /// CPU-heavy parts with [`spawn_blocking`] when their latency matters.
    #[track_caller]
    pub fn spawn<F>(work: F) -> Result<Self, String>
    where
        F: Future<Output = T> + Send + 'static,
    {
        let runtime = handle().ok_or("native execution runtime unavailable")?;
        // Boxed so the runtime's own spawn wrappers do not nest `work` in
        // their `Send` proof (rust-lang/rust#159228).
        let work: BoxFuture<'static, T> = Box::pin(work);
        let work = LongPollGuard {
            work,
            site: std::panic::Location::caller(),
        };
        let (work, abort) = abortable(work);
        let join = runtime.spawn(work);
        let outcome = async move {
            match join.await {
                Ok(Ok(value)) => NativeTaskOutcome::Completed(value),
                Err(error) if error.is_panic() => {
                    NativeTaskOutcome::Panicked(panic_message(error.into_panic().as_ref()))
                }
                Ok(Err(Aborted)) | Err(_) => NativeTaskOutcome::Cancelled,
            }
        };
        Ok(Self {
            outcome: outcome.boxed().shared(),
            abort,
        })
    }

    /// [`Self::spawn`] for work that calls synchronous storage ports: `work`
    /// is polled on one blocking-pool thread, as [`off_native_thread`] does,
    /// so a writer wait parks only that thread. [`Self::abort`] drops it there
    /// at its next poll.
    pub fn spawn_off_native_thread<F>(work: F) -> Result<Self, String>
    where
        F: Future<Output = T> + Send + 'static,
    {
        let (work, abort) = abortable(work);
        let (sender, receiver) = async_channel::bounded(1);
        let runtime = handle().ok_or("native execution runtime unavailable")?;
        spawn_blocking_on(&runtime, move || {
            let polled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                poll_on_current_thread(work)
            }));
            let _ = sender.send_blocking(polled);
        });
        let outcome = async move {
            match receiver.recv().await {
                Ok(Ok(Ok(value))) => NativeTaskOutcome::Completed(value),
                Ok(Err(panic)) => NativeTaskOutcome::Panicked(panic_message(panic.as_ref())),
                Ok(Ok(Err(Aborted))) | Err(_) => NativeTaskOutcome::Cancelled,
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

/// Awaiting the handle itself yields the outcome once; it must not be polled
/// again after that.
impl<T: Clone> Future for NativeTask<T> {
    type Output = NativeTaskOutcome<T>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.outcome.poll_unpin(context)
    }
}

/// Native tasks joined in completion order, like Tokio's `JoinSet`. Dropping
/// the set aborts every task still running.
pub struct NativeTaskSet<T: Clone>(futures_util::stream::FuturesUnordered<NativeTask<T>>);

impl<T: Clone> Default for NativeTaskSet<T> {
    fn default() -> Self {
        Self(futures_util::stream::FuturesUnordered::new())
    }
}

impl<T: Clone + Send + Sync + 'static> NativeTaskSet<T> {
    #[track_caller]
    pub fn spawn<F>(&mut self, work: F) -> Result<(), String>
    where
        F: Future<Output = T> + Send + 'static,
    {
        self.0.push(NativeTask::spawn(work)?);
        Ok(())
    }

    /// The next task to end, or `None` when the set is empty. Cancel-safe.
    pub async fn join_next(&mut self) -> Option<NativeTaskOutcome<T>> {
        futures_util::StreamExt::next(&mut self.0).await
    }

    /// A task that has already ended, without waiting for one that has not.
    pub fn try_join_next(&mut self) -> Option<NativeTaskOutcome<T>> {
        futures_util::StreamExt::next(&mut self.0)
            .now_or_never()
            .flatten()
    }
}

impl<T: Clone> NativeTaskSet<T> {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Tasks not yet joined, including ended ones.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn abort_all(&self) {
        for task in &self.0 {
            task.abort.abort();
        }
    }
}

impl<T: Clone> Drop for NativeTaskSet<T> {
    fn drop(&mut self) {
        self.abort_all();
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
/// A host installs it at startup, before any native helper runs.
pub fn install(builder: Builder) -> Result<&'static Runtime, InstallError> {
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

/// The installed execution runtime for a `site`, preferring the runtime
/// already entered on this thread. Missing hosts are logged and counted.
#[must_use]
pub fn tokio_handle(site: &'static str) -> Option<Handle> {
    let runtime = handle();
    if runtime.is_none() {
        *TOKIO_RUNTIME_MISSING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(site)
            .or_default() += 1;
        tracing::error!(site, "Tokio runtime missing; path skipped");
    }
    runtime
}

/// `work` in the caller's own task with the Tokio runtime [`tokio_handle`]
/// resolves for `site` entered, for work that polls Tokio timers or spawns on
/// Tokio while its caller may run on a dedicated thread. `None` is a counted miss.
pub fn on_tokio<F: Future>(site: &'static str, work: F) -> Option<InTokioContext<F>> {
    tokio_handle(site).map(|runtime| in_tokio_context(runtime, work))
}

/// `work` polled and dropped with `runtime` entered. It stays in the caller's
/// task, so dropping the caller drops it, and everything it holds, at once.
pub fn in_tokio_context<F: Future>(runtime: tokio::runtime::Handle, work: F) -> InTokioContext<F> {
    InTokioContext {
        runtime,
        work: Some(Box::pin(work)),
    }
}

/// The future [`on_tokio`] and [`in_tokio_context`] return.
pub struct InTokioContext<F> {
    runtime: tokio::runtime::Handle,
    work: Option<Pin<Box<F>>>,
}

impl<F: Future> Future for InTokioContext<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<F::Output> {
        let this = &mut *self;
        let _entered = this.runtime.enter();
        this.work
            .as_mut()
            .expect("work is present until drop")
            .as_mut()
            .poll(context)
    }
}

impl<F> Drop for InTokioContext<F> {
    fn drop(&mut self) {
        // Tokio resources the work holds may need the runtime to deregister.
        let _entered = self.runtime.enter();
        drop(self.work.take());
    }
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

/// The runtime a host without its own sizing installs.
#[must_use]
pub fn default_builder() -> Builder {
    let mut builder = Builder::new_multi_thread();
    builder
        .enable_all()
        // A task woken during a long poll must remain stealable by idle workers.
        .disable_lifo_slot()
        .thread_name("tracedecay-native")
        .max_blocking_threads(MAX_BLOCKING_THREADS);
    builder
}

fn process_runtime() -> Result<&'static Runtime, String> {
    #[cfg(feature = "test-runtime")]
    let runtime = Some(PROCESS_RUNTIME.get_or_init(|| start(default_builder())));
    #[cfg(not(feature = "test-runtime"))]
    let runtime = PROCESS_RUNTIME.get();
    runtime
        .ok_or_else(|| NOT_INSTALLED.to_owned())?
        .as_ref()
        .map_err(Clone::clone)
}

fn start(mut builder: Builder) -> Result<Runtime, String> {
    builder.enable_all().build().map_err(|error| {
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

    fn hop_test_tokio_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("hop-test-tokio")
            .enable_time()
            .build()
            .expect("tokio runtime")
    }

    #[test]
    fn a_native_task_uses_tokio_timers_and_spawns_in_tokio_context() {
        let runtime = hop_test_tokio_runtime();
        let handle = runtime.handle().clone();

        let task = NativeTask::spawn(async move {
            let ambient = tokio::runtime::Handle::try_current().is_ok();
            let spawned_on = in_tokio_context(handle, async {
                tokio::time::sleep(Duration::from_millis(1)).await;
                tokio::spawn(async { std::thread::current().name().map(str::to_owned) })
                    .await
                    .ok()
                    .flatten()
            })
            .await;
            (ambient, spawned_on)
        })
        .expect("native task");
        let outcome = block_on(timeout(HANG_GUARD, task.wait()))
            .expect("native root")
            .expect("the hop must finish");

        assert_eq!(
            outcome,
            NativeTaskOutcome::Completed((true, Some("hop-test-tokio".to_owned()))),
            "the selected runtime owns the hop timer and spawned work"
        );
    }

    #[test]
    fn dropping_a_tokio_hop_drops_its_work_at_once() {
        struct DropSignal(std::sync::mpsc::Sender<()>);
        impl Drop for DropSignal {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        let runtime = hop_test_tokio_runtime();
        let (dropped, dropped_signal) = std::sync::mpsc::channel();
        let mut hop = in_tokio_context(runtime.handle().clone(), async move {
            let _signal = DropSignal(dropped);
            tokio::time::sleep(Duration::from_secs(3600)).await;
        });
        assert!(
            (&mut hop).now_or_never().is_none(),
            "the hop parks on its timer"
        );

        drop(hop);

        assert_eq!(
            dropped_signal.try_recv(),
            Ok(()),
            "the work must be dropped by the time the hop's drop returns"
        );
    }

    #[test]
    fn dropping_a_native_task_set_aborts_its_running_tasks() {
        let (started, task_started) = std::sync::mpsc::channel();
        let (ended, task_ended) = std::sync::mpsc::channel::<()>();
        let mut set = NativeTaskSet::default();
        set.spawn(async move {
            let _ended = ended;
            let _ = started.send(());
            std::future::pending::<()>().await;
        })
        .expect("native task");
        task_started
            .recv_timeout(HANG_GUARD)
            .expect("the task runs");

        drop(set);

        assert_eq!(
            task_ended.recv_timeout(HANG_GUARD),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
            "the dropped set must abort its task"
        );
    }

    #[test]
    fn try_join_next_returns_only_tasks_that_already_ended() {
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let mut set = NativeTaskSet::default();
        set.spawn(async move {
            let _ = released.await;
            1
        })
        .expect("live task");
        set.spawn(async { 2 }).expect("finished task");

        let deadline = Instant::now() + HANG_GUARD;
        let finished = loop {
            if let Some(outcome) = set.try_join_next() {
                break outcome;
            }
            assert!(
                Instant::now() < deadline,
                "the finished task was never reaped"
            );
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(finished, NativeTaskOutcome::Completed(2));
        assert_eq!(set.try_join_next(), None, "the live task is not awaited");
        assert_eq!(set.len(), 1);

        release.send(()).expect("release the live task");
        let live = block_on(timeout(HANG_GUARD, set.join_next()))
            .expect("native root")
            .expect("the live task ends");
        assert_eq!(live, Some(NativeTaskOutcome::Completed(1)));
        assert!(set.is_empty());
    }

    #[test]
    fn a_plain_thread_uses_the_process_runtime_without_a_missing_runtime_report() {
        const SITE: &str = "native_runtime.tests.tokio_handle";
        let before = tokio_runtime_missing_count(SITE);
        let process = tokio_handle(SITE).expect("process runtime is installed for tests");
        let task = process.spawn(async { 7 });
        assert_eq!(block_on(task).unwrap().unwrap(), 7);
        assert_eq!(tokio_runtime_missing_count(SITE), before);
    }

    #[tokio::test]
    async fn blocking_helpers_never_poll_on_a_tokio_worker() {
        let caller = std::thread::current().id();
        let offloaded =
            off_native_thread(async { (std::thread::current().id(), ON_BLOCKING_POOL.get()) })
                .await
                .expect("offloaded poll");
        assert_ne!(
            offloaded.0, caller,
            "off_native_thread must leave the async worker"
        );
        assert!(offloaded.1, "off_native_thread must mark the blocking pool");

        let polled_on = on_blocking_pool(async {
            sleep_until(Instant::now() + Duration::from_millis(1))
                .await
                .expect("native timer");
            (std::thread::current().id(), ON_BLOCKING_POOL.get())
        })
        .await
        .expect("blocking-pool poll");

        assert_ne!(
            polled_on.0, caller,
            "the future must leave the Tokio worker"
        );
        assert!(
            polled_on.1,
            "the future must be polled on the native blocking pool"
        );
    }

    #[test]
    fn explicit_blocking_work_keeps_its_runtime_and_restores_its_scope() {
        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .build()
            .expect("test runtime");
        let expected_runtime = runtime.handle().id();
        runtime.block_on(async {
            let thread = spawn_blocking_on(runtime.handle(), move || {
                assert!(ON_BLOCKING_POOL.get());
                assert!(!on_native_thread(), "blocking work may park for storage");
                assert_eq!(handle().unwrap().id(), expected_runtime);
                assert_eq!(
                    tokio_handle("native_runtime.tests.explicit_blocking")
                        .unwrap()
                        .id(),
                    expected_runtime
                );
                let child = NativeTask::spawn(async { Handle::current().id() }).unwrap();
                assert_eq!(
                    poll_on_current_thread(child.wait()),
                    NativeTaskOutcome::Completed(expected_runtime)
                );
                assert!(
                    poll_on_current_thread(sleep_until(Instant::now() + HANG_GUARD)).is_err(),
                    "a timer must use the selected runtime's disabled driver"
                );
                let thread = std::thread::current().id();
                assert_eq!(
                    spawn_blocking(|| std::thread::current().id()).now_or_never(),
                    Some(Ok(thread)),
                    "nested work must not wait behind its own pool thread"
                );
                thread
            })
            .await
            .expect("explicit blocking work");

            let unmarked = runtime
                .spawn_blocking(|| (std::thread::current().id(), ON_BLOCKING_POOL.get()))
                .await
                .expect("reused blocking thread");
            assert_eq!(
                unmarked,
                (thread, false),
                "the marker is scoped to the work"
            );

            let panic = spawn_blocking_on(runtime.handle(), || panic!("blocking work failed"))
                .await
                .expect_err("panic becomes a join error");
            assert!(panic.is_panic());
            assert!(
                !runtime
                    .spawn_blocking(|| ON_BLOCKING_POOL.get())
                    .await
                    .unwrap()
            );
        });
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

    #[test]
    fn off_native_thread_moves_a_native_caller_and_keeps_its_awaits_working() {
        let off_native = || async {
            let parked_thread = !on_native_thread();
            let slept = sleep_until(Instant::now() + Duration::from_millis(5)).await;
            let blocking = spawn_blocking(|| 7).await;
            (parked_thread, slept, blocking)
        };
        let task = NativeTask::spawn(async move {
            let caller_was_native = on_native_thread();
            (caller_was_native, off_native_thread(off_native()).await)
        })
        .expect("native task");
        assert_eq!(
            block_on(task.wait()).expect("process runtime"),
            NativeTaskOutcome::Completed((true, Ok((true, Ok(()), Ok(7)))))
        );
        let in_place = std::thread::spawn(move || {
            let thread = std::thread::current().id();
            off_native_thread(async move { std::thread::current().id() == thread }).now_or_never()
        })
        .join()
        .expect("plain thread");
        assert_eq!(
            in_place,
            Some(Ok(true)),
            "a non-native caller polls in place"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_past_deadline_takes_a_ready_value_and_elapses_a_pending_one() {
        let past = Instant::now() - Duration::from_millis(1);
        assert_eq!(sleep_until(past).now_or_never(), Some(Ok(())));
        assert_eq!(timeout_at(past, async { 7 }).now_or_never(), Some(Ok(7)));
        assert_eq!(
            timeout_at(past, std::future::pending::<()>()).now_or_never(),
            Some(Err(TimeoutError::Elapsed))
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

    #[test]
    fn a_disabled_timer_driver_returns_an_error() {
        let runtime = Builder::new_current_thread().build().expect("test runtime");
        let error = runtime
            .block_on(sleep_until(Instant::now() + HANG_GUARD))
            .expect_err("a disabled timer driver must fail closed");
        assert!(error.starts_with("native timer driver unavailable:"));
    }

    #[test]
    fn an_armed_timer_reports_runtime_shutdown() {
        let runtime = default_builder()
            .worker_threads(1)
            .build()
            .expect("test runtime");
        let mut timer = sleep_until(Instant::now() + HANG_GUARD);
        runtime.block_on(async {
            assert!(futures_util::poll!(&mut timer).is_pending());
        });
        drop(runtime);

        let error = timer
            .now_or_never()
            .expect("a stopped timer must wake its waiter")
            .expect_err("shutdown must not report a deadline that never elapsed");
        assert!(error.starts_with("native timer driver unavailable:"));
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
        let runtime = Builder::new_current_thread()
            .enable_all()
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
        assert!(payload.contains("native task failure"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn work_spawned_off_native_threads_runs_on_the_blocking_pool_and_aborts_there() {
        let finished = NativeTask::spawn_off_native_thread(async {
            sleep_until(Instant::now() + Duration::from_millis(5))
                .await
                .expect("native timer from the blocking pool");
            !on_native_thread()
        })
        .expect("blocking-pool task");
        assert_eq!(
            tokio::time::timeout(HANG_GUARD, finished.wait()).await,
            Ok(NativeTaskOutcome::Completed(true)),
            "the work must not run on a native worker"
        );

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
        let pending = NativeTask::spawn_off_native_thread(async move {
            let _signal = SignalOnDrop(Some(dropping));
            let _ = started.send(());
            std::future::pending::<()>().await;
        })
        .expect("blocking-pool task");
        running.await.expect("work started");
        pending.abort();
        assert_eq!(
            tokio::time::timeout(HANG_GUARD, pending.wait()).await,
            Ok(NativeTaskOutcome::Cancelled)
        );
        tokio::time::timeout(HANG_GUARD, dropped)
            .await
            .expect("abort must drop the work on its pool thread")
            .expect("the work signals its drop");

        let panicked = NativeTask::<()>::spawn_off_native_thread(async { panic!("pool failure") })
            .expect("blocking-pool task");
        let NativeTaskOutcome::Panicked(payload) =
            tokio::time::timeout(HANG_GUARD, panicked.wait())
                .await
                .expect("a panicked task must settle")
        else {
            panic!("expected a panic outcome");
        };
        assert!(payload.contains("pool failure"));
    }

    #[test]
    fn a_task_dropped_by_its_runtime_reports_cancelled() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let mut task = None;
        runtime.block_on(async {
            task = Some(NativeTask::spawn(std::future::pending::<()>()).expect("native task"));
        });
        let task = task.expect("spawned");
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
    async fn settlement_reuses_the_entered_tokio_runtime() {
        let runtime = handle().expect("entered runtime");
        let task = runtime.spawn(async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Handle::try_current().is_ok()
        });
        assert!(
            tokio::time::timeout(HANG_GUARD, task)
                .await
                .unwrap()
                .unwrap()
        );
        assert_eq!(spawn_blocking(|| 7).await.unwrap(), 7);
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
    fn abandoned_settlement_keeps_the_runtime_that_admitted_it() {
        let runtime = default_builder()
            .worker_threads(1)
            .build()
            .expect("test runtime");
        let expected_runtime = runtime.handle().id();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let (finished, settled) = std::sync::mpsc::channel();
        let mut settlement = {
            let _entered = runtime.enter();
            settle(async move {
                released.await.expect("release admitted settlement");
                finished
                    .send(Handle::current().id())
                    .expect("report runtime");
            })
        };
        runtime.block_on(async {
            assert!(futures_util::poll!(&mut settlement).is_pending());
        });

        assert!(Handle::try_current().is_err());
        drop(settlement);
        release.send(()).expect("settlement remains admitted");
        assert_eq!(
            settled
                .recv_timeout(HANG_GUARD)
                .expect("settlement finishes"),
            expected_runtime,
            "dropping on a plain thread must retain the admission runtime"
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
    fn queued_blocking_work_outlives_its_aborted_request_task() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let (release, held) = std::sync::mpsc::channel::<()>();
            let blocker = tokio::task::spawn_blocking(move || held.recv().unwrap());
            let (ran, settled) = std::sync::mpsc::channel::<()>();
            let (queued, entered) = tokio::sync::oneshot::channel();
            let caller = tokio::spawn(async move {
                let pending = spawn_blocking(move || {
                    let _ = ran.send(());
                });
                let _ = queued.send(());
                pending.await
            });
            entered.await.expect("work queued behind blocker");
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            release.send(()).expect("release blocker");
            blocker.await.expect("blocker completed");
            settled
                .recv_timeout(HANG_GUARD)
                .expect("queued settlement outlives request cancellation");
        });
    }

    #[test]
    fn install_after_the_default_runtime_started_is_refused() {
        handle().expect("default process runtime");
        assert_eq!(
            install(Builder::new_multi_thread()).err(),
            Some(InstallError::AlreadyStarted)
        );
    }

    #[cfg(unix)]
    const SPIN: Duration = Duration::from_millis(150);
    #[cfg(unix)]
    const WAKE_BOUND: Duration = Duration::from_millis(40);

    fn spin(duration: Duration) {
        let spun = Instant::now();
        while spun.elapsed() < duration {
            std::hint::spin_loop();
        }
    }

    /// #3994: on two workers, task A wakes on its socket, then spins for
    /// [`SPIN`] on the blocking pool. Task B's socket becomes readable 10 ms
    /// into the spin. Returns how long after that B woke.
    ///
    /// Tokio owns its I/O driver on one worker, so a long poll left inline
    /// would hold the reactor and delay B; the blocking pool is the supported
    /// boundary for work that may hold a worker past [`LONG_POLL`].
    #[cfg(unix)]
    fn socket_wake_behind_a_spinning_task() -> Duration {
        use std::io::Write;
        use tokio::io::AsyncReadExt;
        use tokio::net::UnixStream;

        let runtime = default_builder()
            .worker_threads(2)
            .max_blocking_threads(1)
            .build()
            .expect("test runtime");
        let (mut a_tx, a_rx) = std::os::unix::net::UnixStream::pair().expect("socket a");
        let (mut b_tx, b_rx) = std::os::unix::net::UnixStream::pair().expect("socket b");
        let (woke, wakes) = std::sync::mpsc::channel::<(char, Instant)>();
        let spawn_reader = |name: char, rx: std::os::unix::net::UnixStream, then: Duration| {
            let woke = woke.clone();
            runtime.handle().spawn(async move {
                rx.set_nonblocking(true).expect("nonblocking");
                let mut rx = UnixStream::from_std(rx).expect("native socket");
                rx.read_exact(&mut [0_u8; 1]).await.expect("read");
                let _ = woke.send((name, Instant::now()));
                spawn_blocking(move || spin(then)).await.expect("pool spin");
            })
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

    #[cfg(unix)]
    #[test]
    fn work_on_the_blocking_pool_does_not_delay_another_tasks_socket_wake() {
        let latency = socket_wake_behind_a_spinning_task();
        assert!(
            latency < WAKE_BOUND,
            "b woke {latency:?} after its socket became readable, behind a's {SPIN:?} pool spin"
        );
    }

    #[track_caller]
    fn spawned_here<T: Clone + Send + Sync + 'static>(
        work: impl Future<Output = T> + Send + 'static,
    ) -> (NativeTask<T>, &'static std::panic::Location<'static>) {
        (
            NativeTask::spawn(work).expect("native task"),
            std::panic::Location::caller(),
        )
    }

    /// The guard's window encloses the spin, so a descheduled worker only
    /// lengthens the measured poll.
    #[test]
    fn a_long_native_poll_completes_and_is_counted_at_its_spawn_site() {
        let runtime = default_builder()
            .worker_threads(1)
            .build()
            .expect("test runtime");
        let (long, short) = runtime.block_on(async {
            let (task, site) = spawned_here(async {
                spin(LONG_POLL + Duration::from_millis(50));
                7
            });
            let long = (task.wait().await, site);
            let (task, site) = spawned_here(async { 8 });
            (long, (task.wait().await, site))
        });
        assert_eq!(long.0, NativeTaskOutcome::Completed(7));
        assert_eq!(long_native_polls().get(long.1), Some(&1), "{}", long.1);
        assert_eq!(short.0, NativeTaskOutcome::Completed(8));
        assert_eq!(long_native_polls().get(short.1), None, "{}", short.1);
    }

    /// Queued instead, the inner work would wait for a second pool thread
    /// while the outer one holds the first; a full pool never frees one.
    #[test]
    fn blocking_work_spawned_from_the_blocking_pool_runs_on_the_same_thread() {
        let (outer, inner) = block_on(spawn_blocking(|| {
            let inner = spawn_blocking(|| std::thread::current().id()).now_or_never();
            (std::thread::current().id(), inner)
        }))
        .expect("process runtime")
        .expect("outer blocking work");
        assert_eq!(inner, Some(Ok(outer)));
    }

    /// A fresh one-thread pool, so the thread has run no other queued work.
    #[test]
    fn blocking_work_from_a_task_spawned_off_native_threads_runs_on_its_thread() {
        let runtime = default_builder()
            .worker_threads(1)
            .max_blocking_threads(1)
            .build()
            .expect("test runtime");
        let outcome = runtime.block_on(async {
            NativeTask::spawn_off_native_thread(async {
                spawn_blocking(|| std::thread::current().id()).now_or_never()
                    == Some(Ok(std::thread::current().id()))
            })
            .expect("blocking-pool task")
            .wait()
            .await
        });
        assert_eq!(outcome, NativeTaskOutcome::Completed(true));
    }

    #[cfg(target_os = "linux")]
    fn current_thread_stack_size() -> usize {
        let mut attributes = std::mem::MaybeUninit::<libc::pthread_attr_t>::uninit();
        let mut size = 0;
        // SAFETY: `pthread_getattr_np` initializes `attributes` for the calling
        // thread before `pthread_attr_getstacksize` reads it, and they are
        // destroyed once, after that read.
        unsafe {
            assert_eq!(
                libc::pthread_getattr_np(libc::pthread_self(), attributes.as_mut_ptr()),
                0
            );
            assert_eq!(
                libc::pthread_attr_getstacksize(attributes.as_ptr(), &raw mut size),
                0
            );
            libc::pthread_attr_destroy(attributes.as_mut_ptr());
        }
        size
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn blocking_pool_threads_get_the_runtime_thread_stack_size() {
        const STACK: usize = 16 * 1024 * 1024;
        let runtime = default_builder()
            .worker_threads(1)
            .thread_stack_size(STACK)
            .max_blocking_threads(2)
            .build()
            .expect("test runtime");
        let stack = runtime
            .block_on(async { spawn_blocking(current_thread_stack_size).await })
            .expect("blocking work");
        assert!(stack >= STACK, "pool thread stack is {stack} bytes");
    }

    #[test]
    fn the_lateness_canary_records_a_wake_held_behind_a_long_poll() {
        let runtime = default_builder()
            .worker_threads(1)
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let canary = spawn_lateness_canary().expect("canary");
            sleep_until(Instant::now() + Duration::from_millis(20))
                .await
                .expect("let the canary arm its first timer");
            NativeTask::spawn(async { spin(CANARY_TICK + LONG_POLL * 3) })
                .expect("spinning task")
                .wait()
                .await;
            sleep_until(Instant::now() + CANARY_TICK)
                .await
                .expect("let the canary record its overdue wake");
            canary.abort();
        });
        let lateness = scheduling_lateness();
        assert!(
            lateness.late_ticks >= 1 && lateness.worst >= LONG_POLL,
            "{lateness:?}"
        );
    }

    #[test]
    fn block_on_moves_a_worker_task_to_a_blocking_thread() {
        let runtime = default_builder()
            .worker_threads(1)
            .build()
            .expect("test runtime");
        let (started, started_rx) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let blocked = runtime.spawn(async move {
            let _ = started.send(());
            block_on(async move {
                released.recv().expect("release the blocked task");
            })
            .expect("block_on");
        });
        started_rx.recv().expect("the blocked task starts");
        // The only worker must still run this task while the first is blocked.
        runtime.spawn(async move {
            release.send(()).expect("release the blocked task");
        });
        runtime
            .block_on(blocked)
            .expect("the blocked task finishes");
    }
}
