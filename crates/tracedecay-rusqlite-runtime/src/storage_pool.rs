//! The fixed pool that runs every attached SQLite file's writer and readers.
//!
//! Each writer and each reader worker is a task here, not a thread of its own.
//! A task holds a `tracedecay-store-{i}` thread only while it runs SQL, so the
//! thread count is the configured size whatever the number of attached files.
//!
//! Nothing on a pool thread may wait synchronously on a store: the task it
//! waits for may need the very thread it holds. The synchronous waits callers
//! use refuse on a pool thread in debug builds.

use std::{
    cell::Cell,
    future::Future,
    panic::{AssertUnwindSafe, Location},
    pin::{Pin, pin},
    sync::{Arc, Mutex, OnceLock, PoisonError},
    task::{Context, Poll, Wake, Waker},
    thread::{self, Thread},
    time::{Duration, Instant},
};

use asupersync::{
    Cx,
    runtime::{Runtime, RuntimeBuilder},
};
use futures_util::FutureExt;

/// Overrides the pool size, clamped to [`STORE_THREAD_RANGE`].
pub const STORE_THREADS_ENV: &str = "TRACEDECAY_STORE_THREADS";

/// The name every pool thread carries, followed by `-{index}`.
pub const STORE_THREAD_PREFIX: &str = "tracedecay-store";

const DEFAULT_STORE_THREADS: usize = 6;
const STORE_THREAD_RANGE: std::ops::RangeInclusive<usize> = 2..=16;

static POOL: OnceLock<Result<Runtime, String>> = OnceLock::new();

thread_local! {
    static ON_POOL: Cell<bool> = const { Cell::new(false) };
}

/// How many threads the pool runs: [`STORE_THREADS_ENV`] when it parses,
/// else 6, clamped to 2..=16.
#[must_use]
pub fn store_threads() -> usize {
    std::env::var(STORE_THREADS_ENV)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(DEFAULT_STORE_THREADS)
        .clamp(*STORE_THREAD_RANGE.start(), *STORE_THREAD_RANGE.end())
}

fn pool() -> Result<&'static Runtime, String> {
    POOL.get_or_init(|| {
        RuntimeBuilder::multi_thread()
            .worker_threads(store_threads())
            .thread_name_prefix(STORE_THREAD_PREFIX)
            // No blocking pool: its threads would be storage threads beyond N.
            .blocking_threads(0, 0)
            .on_thread_start(|| ON_POOL.set(true))
            .build()
            .map_err(|error| format!("storage runtime failed to start: {error}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

pub(crate) fn on_pool() -> bool {
    ON_POOL.get()
}

#[track_caller]
pub(crate) fn debug_assert_off_pool() {
    debug_assert!(
        !on_pool(),
        "a storage pool thread waited synchronously on a store at {}",
        Location::caller()
    );
}

/// A pool task panicked, or the pool dropped it before it finished.
#[derive(Debug)]
pub(crate) struct TaskPanicked;

/// The completion of one pool task. Dropping it detaches the task.
pub(crate) struct PoolTask {
    finished: async_channel::Receiver<bool>,
}

impl PoolTask {
    /// Waits on the calling thread until the task ends.
    #[track_caller]
    pub(crate) fn join(self) -> Result<(), TaskPanicked> {
        debug_assert_off_pool();
        completion(self.finished.recv_blocking())
    }

    pub(crate) async fn finished(self) -> Result<(), TaskPanicked> {
        completion(self.finished.recv().await)
    }
}

fn completion(finished: Result<bool, async_channel::RecvError>) -> Result<(), TaskPanicked> {
    match finished {
        Ok(false) => Ok(()),
        Ok(true) | Err(_) => Err(TaskPanicked),
    }
}

/// Runs `work` as a pool task, or hands it back when the pool cannot take it.
pub(crate) fn spawn<F>(work: F) -> Result<PoolTask, F>
where
    F: Future<Output = ()> + Send + 'static,
{
    let Ok(pool) = pool() else {
        return Err(work);
    };
    // `try_spawn` drops a refused future; the slot keeps it recoverable.
    let slot = Arc::new(Mutex::new(Some(work)));
    let task_slot = Arc::clone(&slot);
    let (done, finished) = async_channel::bounded(1);
    let task = async move {
        let work = task_slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(work) = work {
            let panicked = AssertUnwindSafe(TaskLocals::new(work))
                .catch_unwind()
                .await
                .is_err();
            let _ = done.try_send(panicked);
        }
    };
    match pool.handle().try_spawn(task) {
        Ok(_detached) => Ok(PoolTask { finished }),
        Err(_) => Err(slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .expect("a refused task never ran its work")),
    }
}

/// The receiving end of a pool task's command channel.
///
/// Dropping it discards the queued commands, as a std receiver does, so a
/// caller waiting on the reply a queued command carries learns the task is
/// gone instead of waiting on a channel its own sender keeps open.
pub(crate) struct CommandReceiver<T>(async_channel::Receiver<T>);

impl<T> CommandReceiver<T> {
    pub(crate) fn new(receiver: async_channel::Receiver<T>) -> Self {
        Self(receiver)
    }

    pub(crate) fn recv(&self) -> async_channel::Recv<'_, T> {
        self.0.recv()
    }
}

impl<T> Drop for CommandReceiver<T> {
    fn drop(&mut self) {
        self.0.close();
        while self.0.try_recv().is_ok() {}
    }
}

/// `future`, or `None` once `duration` passes first. Timed on the pool's
/// timer when polled by a pool task.
pub(crate) async fn timeout<F: Future>(duration: Duration, future: F) -> Option<F::Output> {
    let now = Cx::current().map_or_else(asupersync::time::wall_now, |cx| cx.now());
    asupersync::time::timeout(now, duration, future).await.ok()
}

/// Lets the other tasks queued on this thread run before the caller goes on,
/// so a lane with a steady stream of work cannot keep a pool thread to itself.
pub(crate) async fn yield_now() {
    let mut yielded = false;
    std::future::poll_fn(|context| {
        if yielded {
            return Poll::Ready(());
        }
        yielded = true;
        context.waker().wake_by_ref();
        Poll::Pending
    })
    .await;
}

/// Polls `future` on the calling thread, parking between wakes, until it
/// finishes or `deadline` passes.
#[track_caller]
pub(crate) fn wait_until<F: Future>(future: F, deadline: Option<Instant>) -> Option<F::Output> {
    debug_assert_off_pool();
    let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return Some(output);
        }
        match deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return None;
                }
                thread::park_timeout(remaining);
            }
            None => thread::park(),
        }
    }
}

/// A transaction a pool task keeps open across its awaits.
///
/// A `rusqlite::Transaction` is `!Send` only because it holds a
/// `&Connection`, and `Connection` is `!Sync`. This one is created from the
/// task's exclusive borrow of a connection the same task owns, and a task is
/// polled by one thread at a time, so the connection is never used from two
/// threads at once wherever the task moves.
pub(crate) struct TaskTransaction<'connection>(rusqlite::Transaction<'connection>);

// SAFETY: see the type's documentation. Every constructor takes the
// transaction a task began on its own connection; nothing else can reach that
// connection while the transaction borrows it.
unsafe impl Send for TaskTransaction<'_> {}

impl<'connection> TaskTransaction<'connection> {
    pub(crate) fn new(transaction: rusqlite::Transaction<'connection>) -> Self {
        Self(transaction)
    }

    pub(crate) fn into_inner(self) -> rusqlite::Transaction<'connection> {
        self.0
    }
}

impl<'connection> std::ops::Deref for TaskTransaction<'connection> {
    type Target = rusqlite::Transaction<'connection>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

struct ThreadWake(Thread);

impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Gives each task its own copy of the thread-local state a store's worker
/// kept while it owned a thread: lock-work and VM telemetry, and the test
/// clock. Swapped in for each poll and out after it.
struct TaskLocals<F> {
    work: Pin<Box<F>>,
    telemetry: crate::telemetry::TaskTelemetry,
    #[cfg(test)]
    clock: crate::exact_sql::TaskClock,
}

impl<F> TaskLocals<F> {
    fn new(work: F) -> Self {
        Self {
            work: Box::pin(work),
            telemetry: crate::telemetry::TaskTelemetry::default(),
            #[cfg(test)]
            clock: crate::exact_sql::TaskClock::default(),
        }
    }

    fn swap(&mut self) {
        self.telemetry.swap();
        #[cfg(test)]
        self.clock.swap();
    }
}

impl<F: Future> Future for TaskLocals<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<F::Output> {
        let this = &mut *self;
        this.swap();
        let polled = this.work.as_mut().poll(context);
        this.swap();
        polled
    }
}
