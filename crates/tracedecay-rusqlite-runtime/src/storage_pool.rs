//! The fixed pool that runs every attached SQLite file's writer and readers.
//!
//! Each writer and each reader worker is a task here, not a thread of its own.
//! A task holds a `tracedecay-store-{i}` thread only while it runs SQL, so the
//! thread count is the configured size whatever the number of attached files.

use std::{
    cell::{Cell, RefCell},
    future::Future,
    panic::AssertUnwindSafe,
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

const STORE_THREAD_RANGE: std::ops::RangeInclusive<usize> = 2..=16;
const DEFAULT_STORE_THREAD_RANGE: std::ops::RangeInclusive<usize> = 6..=16;

static POOL: OnceLock<Result<Runtime, String>> = OnceLock::new();

type Adopted = Pin<Box<dyn Future<Output = ()> + Send>>;

thread_local! {
    static ON_POOL: Cell<bool> = const { Cell::new(false) };
    static ADOPTED: RefCell<Vec<Adopted>> = const { RefCell::new(Vec::new()) };
}

/// How many threads the pool runs: [`STORE_THREADS_ENV`] clamped to 2..=16
/// when it parses, else the host's available parallelism clamped to 6..=16.
#[must_use]
pub fn store_threads() -> usize {
    match std::env::var(STORE_THREADS_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
    {
        Some(threads) => threads.clamp(*STORE_THREAD_RANGE.start(), *STORE_THREAD_RANGE.end()),
        None => std::thread::available_parallelism()
            .map_or(
                *DEFAULT_STORE_THREAD_RANGE.start(),
                std::num::NonZeroUsize::get,
            )
            .clamp(
                *DEFAULT_STORE_THREAD_RANGE.start(),
                *DEFAULT_STORE_THREAD_RANGE.end(),
            ),
    }
}

fn pool() -> Result<&'static Runtime, String> {
    POOL.get_or_init(|| {
        RuntimeBuilder::multi_thread()
            .worker_threads(store_threads())
            .thread_name_prefix(STORE_THREAD_PREFIX)
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

#[derive(Debug)]
pub(crate) struct TaskLost;

pub(crate) struct PoolTask {
    finished: async_channel::Receiver<bool>,
}

impl PoolTask {
    pub(crate) fn join(self) -> Result<(), TaskLost> {
        if on_pool() {
            return Err(TaskLost);
        }
        completion(self.finished.recv_blocking())
    }

    /// Whether the task ended, which drops everything its future owned. Never
    /// waits, so a pool task may call it.
    pub(crate) fn is_finished(&self) -> bool {
        self.finished.is_closed()
    }

    pub(crate) async fn finished(self) -> Result<(), TaskLost> {
        completion(self.finished.recv().await)
    }
}

fn completion(finished: Result<bool, async_channel::RecvError>) -> Result<(), TaskLost> {
    match finished {
        Ok(false) => Ok(()),
        Ok(true) | Err(_) => Err(TaskLost),
    }
}

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

pub(crate) fn adopt(work: impl Future<Output = ()> + Send + 'static) {
    ADOPTED.with_borrow_mut(|adopted| adopted.push(Box::pin(work)));
}

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

pub(crate) async fn timeout<F: Future>(duration: Duration, future: F) -> Option<F::Output> {
    let now = Cx::current().map_or_else(asupersync::time::wall_now, |cx| cx.now());
    asupersync::time::timeout(now, duration, future).await.ok()
}

pub(crate) async fn sleep(duration: Duration) {
    timeout(duration, std::future::pending::<()>()).await;
}

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

pub(crate) fn wait_until<F: Future>(future: F, deadline: Option<Instant>) -> Option<F::Output> {
    let deadline = if on_pool() {
        Some(Instant::now())
    } else {
        deadline
    };
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

/// A `rusqlite::Transaction` is `!Send` only because it holds a
/// `&Connection`, and `Connection` is `!Sync`. A transaction begun through a
/// `&mut Connection` is the only path to that connection while it lives, so
/// whichever thread holds it uses the connection alone. A write job shares
/// one behind a `Mutex`, which keeps that true across threads.
pub(crate) struct TaskTransaction<'connection>(rusqlite::Transaction<'connection>);

// SAFETY: [`TaskTransaction::new`]'s contract makes the transaction the only
// path to its connection, and `Connection` is `Send`.
unsafe impl Send for TaskTransaction<'_> {}

impl<'connection> TaskTransaction<'connection> {
    /// # Safety
    ///
    /// `transaction` must borrow a connection that the caller owns or holds
    /// by `&mut`, and nothing else may reach that connection while this value
    /// lives.
    pub(crate) unsafe fn new(transaction: rusqlite::Transaction<'connection>) -> Self {
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

struct TaskLocals<F> {
    work: Option<Pin<Box<F>>>,
    adopted: Vec<Adopted>,
    locals: Locals,
}

#[derive(Default)]
struct Locals {
    telemetry: crate::telemetry::TaskTelemetry,
    #[cfg(test)]
    clock: crate::exact_sql::TaskClock,
}

impl Locals {
    fn swap(&mut self) {
        self.telemetry.swap();
        #[cfg(test)]
        self.clock.swap();
    }
}

struct SwappedIn<'a>(&'a mut Locals);

impl Drop for SwappedIn<'_> {
    fn drop(&mut self) {
        self.0.swap();
    }
}

impl<F> TaskLocals<F> {
    fn new(work: F) -> Self {
        Self {
            work: Some(Box::pin(work)),
            adopted: Vec::new(),
            locals: Locals::default(),
        }
    }
}

impl<F: Future<Output = ()>> Future for TaskLocals<F> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        let this = &mut *self;
        this.locals.swap();
        let _swapped_in = SwappedIn(&mut this.locals);
        if let Some(work) = &mut this.work
            && work.as_mut().poll(context).is_ready()
        {
            this.work = None;
        }
        loop {
            this.adopted.extend(ADOPTED.take());
            this.adopted
                .retain_mut(|work| work.as_mut().poll(context).is_pending());
            if ADOPTED.with_borrow(Vec::is_empty) {
                break;
            }
        }
        if this.work.is_none() && this.adopted.is_empty() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pool_task_is_refused_a_synchronous_wait_instead_of_parked() {
        let (hold, release) = async_channel::bounded::<()>(1);
        let held = spawn(async move {
            let _ = release.recv().await;
        })
        .unwrap_or_else(|_| panic!("the storage pool must start"));
        let (report, refused) = std::sync::mpsc::channel();
        spawn(async move {
            let _ = report.send((
                wait_until(std::future::pending::<()>(), None).is_none(),
                held.join().is_err(),
            ));
        })
        .unwrap_or_else(|_| panic!("the storage pool must start"));

        let refused = refused.recv_timeout(Duration::from_secs(10));
        drop(hold);
        assert_eq!(
            refused,
            Ok((true, true)),
            "a pool thread must refuse to park on a future or a task join"
        );
    }
}
