//! Cadence loop that drives admitted maintenance ticks.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;
use std::time::{Duration, Instant};

use asupersync::sync::Notify;
use tracedecay_runtime_core::native_runtime;

use crate::tick::{MaintenanceCadence, MaintenanceContinuation, MaintenanceTickOutcome};

/// The maintenance loop's wake handle.
///
/// [`Self::wake`] parks the loop out of its timer so an already-due tick
/// starts promptly; it never moves the due deadline, so a burst of git-watch
/// events cannot turn the daily cadence into a busy loop.
/// [`Self::request_due`] is for an event that just made retention work
/// collectable, a sealed code generation superseding its predecessor, and
/// pulls the next tick forward to at most one retry delay away. Requests
/// inside that window coalesce into one tick. Before this, every publication
/// left its predecessor's sealed artifact and segments on disk
/// until the daily tick.
#[derive(Default)]
pub struct MaintenanceWake {
    notify: Notify,
    /// At most one wake is pending however many events raise it, and it
    /// survives a wake raised while the loop runs a tick.
    woken: AtomicBool,
    due_requested: AtomicBool,
}

impl MaintenanceWake {
    pub fn wake(&self) {
        self.woken.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub fn request_due(&self) {
        self.due_requested.store(true, Ordering::Release);
        self.wake();
    }

    async fn woken(&self) {
        self.notify
            .wait_until(|| self.woken.swap(false, Ordering::AcqRel))
            .await;
    }

    fn take_due_request(&self) -> bool {
        self.due_requested.swap(false, Ordering::AcqRel)
    }
}

/// Park on cancel / wake / cadence, then run the next admitted tick.
pub async fn run_maintenance_loop<F, Fut>(
    cancellation: &tracedecay_runtime_core::cancellation::CancellationToken,
    wake: &MaintenanceWake,
    interval: Duration,
    run_tick: F,
) where
    F: FnMut(Option<MaintenanceContinuation>) -> Fut,
    Fut: Future<Output = MaintenanceTickOutcome>,
{
    run_cadence(
        cancellation,
        wake,
        MaintenanceCadence::new(interval),
        run_tick,
    )
    .await;
}

async fn run_cadence<F, Fut>(
    cancellation: &tracedecay_runtime_core::cancellation::CancellationToken,
    wake: &MaintenanceWake,
    mut cadence: MaintenanceCadence,
    mut run_tick: F,
) where
    F: FnMut(Option<MaintenanceContinuation>) -> Fut,
    Fut: Future<Output = MaintenanceTickOutcome>,
{
    let mut deadline = Instant::now() + cadence.retry_delay();
    let mut continuation = None;
    loop {
        // Scoped so the timer is cancelled before a tick runs.
        let timer = {
            let mut cancelled = pin!(cancellation.cancelled());
            let mut woken = pin!(wake.woken());
            let mut due = pin!(native_runtime::sleep_until(deadline));
            poll_fn(|task| {
                if cancelled.as_mut().poll(task).is_ready() || woken.as_mut().poll(task).is_ready()
                {
                    return Poll::Ready(Ok(()));
                }
                due.as_mut().poll(task)
            })
            .await
        };
        if cancellation.is_cancelled() {
            break;
        }
        if let Err(error) = timer {
            // Without a timer every park would return at once and spin.
            tracing::error!(event = "maintenance_timer_unavailable", %error);
            break;
        }
        let now = Instant::now();
        if wake.take_due_request() {
            deadline = cadence.pull_forward(now, deadline);
        }
        if now < deadline || !cadence.reserve(now) {
            continue;
        }
        let outcome = run_tick(continuation).await;
        if cancellation.is_cancelled() {
            break;
        }
        continuation = outcome.continuation();
        deadline = cadence.finish(Instant::now(), outcome);
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use asupersync::Cx;
    use asupersync::channel::mpsc;
    use tracedecay_runtime_core::cancellation::CancellationToken;

    use super::{MaintenanceWake, run_cadence};
    use crate::tick::{MaintenanceCadence, MaintenanceContinuation, MaintenanceTickOutcome};

    const RETRY: Duration = Duration::from_millis(150);
    const DAILY: Duration = Duration::from_hours(24);

    struct Tick {
        at: Instant,
        continuation: Option<MaintenanceContinuation>,
    }

    /// Runs the loop beside `drive`, which observes every tick in order and
    /// cancels the loop when it returns. The guard turns a loop that never
    /// ticks or never ends into a failure rather than a hang.
    async fn drive_loop<D, Fut>(
        cadence: MaintenanceCadence,
        outcomes: Vec<MaintenanceTickOutcome>,
        drive: D,
    ) where
        D: FnOnce(Arc<MaintenanceWake>, mpsc::UnboundedReceiver<Tick>) -> Fut,
        Fut: Future<Output = ()>,
    {
        let cancellation = CancellationToken::new();
        let wake = Arc::new(MaintenanceWake::default());
        let (ticks, observed) = mpsc::unbounded();
        let mut outcomes = outcomes.into_iter();
        let run = run_cadence(&cancellation, &wake, cadence, |continuation| {
            let _ = ticks.send(Tick {
                at: Instant::now(),
                continuation,
            });
            let outcome = outcomes.next().unwrap_or(MaintenanceTickOutcome::Complete);
            async move { outcome }
        });
        let driven = async {
            drive(Arc::clone(&wake), observed).await;
            cancellation.cancel();
        };
        asupersync::time::timeout(
            asupersync::time::wall_now(),
            Duration::from_secs(10),
            async { futures_util::join!(run, driven) },
        )
        .await
        .expect("the loop must tick when due and end once cancelled");
    }

    async fn next_tick(observed: &mut mpsc::UnboundedReceiver<Tick>) -> Tick {
        let cx = Cx::current().expect("test runtime context");
        observed.recv(&cx).await.expect("tick observed")
    }

    #[asupersync::test]
    async fn wakes_never_move_the_due_deadline() {
        let started = Instant::now();
        drive_loop(
            MaintenanceCadence::with_retry_delay(DAILY, RETRY),
            Vec::new(),
            |wake, mut observed| async move {
                for _ in 0..3 {
                    wake.wake();
                    asupersync::runtime::yield_now().await;
                }
                let first = next_tick(&mut observed).await;
                assert!(
                    first.at >= started + RETRY,
                    "early wakes must not run the first tick before its deadline"
                );

                for _ in 0..3 {
                    wake.wake();
                    asupersync::runtime::yield_now().await;
                }
                let requested = Instant::now();
                wake.request_due();
                let second = next_tick(&mut observed).await;
                assert!(
                    second.at >= requested + RETRY,
                    "a plain wake must not run a tick under the daily cadence"
                );
            },
        )
        .await;
    }

    /// A due request is the publication signal: it pulls the next tick to at
    /// most one retry delay away, coalesces with other requests inside that
    /// window, and the daily cadence resumes after that tick.
    #[asupersync::test]
    async fn due_requests_coalesce_into_one_pulled_forward_tick() {
        drive_loop(
            MaintenanceCadence::with_retry_delay(DAILY, RETRY),
            Vec::new(),
            |wake, mut observed| async move {
                next_tick(&mut observed).await;

                let requested = Instant::now();
                for _ in 0..3 {
                    wake.request_due();
                    asupersync::runtime::yield_now().await;
                }
                let pulled = next_tick(&mut observed).await;
                assert!(
                    pulled.at >= requested + RETRY,
                    "a due request never runs early"
                );

                let again = Instant::now();
                wake.request_due();
                let next = next_tick(&mut observed).await;
                assert!(
                    next.at >= again + RETRY,
                    "coalesced due requests must run exactly one tick"
                );
            },
        )
        .await;
    }

    #[asupersync::test]
    async fn progress_continuation_reenters_only_the_owning_phase() {
        drive_loop(
            MaintenanceCadence::with_retry_delay(DAILY, RETRY),
            vec![MaintenanceTickOutcome::Continue(
                MaintenanceContinuation::CodeGenerationRetention,
            )],
            |_wake, mut observed| async move {
                let first = next_tick(&mut observed).await;
                let resumed = next_tick(&mut observed).await;
                assert_eq!(first.continuation, None);
                assert_eq!(
                    resumed.continuation,
                    Some(MaintenanceContinuation::CodeGenerationRetention),
                    "bounded progress must resume its code-generation phase instead of a full tick"
                );
                assert!(
                    resumed.at >= first.at + RETRY,
                    "progress resumes one retry delay later"
                );
            },
        )
        .await;
    }

    #[asupersync::test]
    async fn cancellation_ends_a_parked_loop_before_its_deadline() {
        drive_loop(
            MaintenanceCadence::with_retry_delay(DAILY, DAILY),
            Vec::new(),
            |_wake, _observed| async {},
        )
        .await;
    }
}
