use std::future::Future;
use std::time::{Duration, Instant};

use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::native_runtime;

const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(5);
const MAX_RETRY_DELAY: Duration = Duration::from_mins(5);
const WARNING_EVERY_FAILURES: u64 = 10;

/// When a recovery loop wakes without a durable-write notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RecoveryScheduleV1 {
    safety_interval: Duration,
    initial_retry_delay: Duration,
    max_retry_delay: Duration,
}

impl RecoveryScheduleV1 {
    pub(super) const fn with_safety_interval(safety_interval: Duration) -> Self {
        Self {
            safety_interval,
            initial_retry_delay: INITIAL_RETRY_DELAY,
            max_retry_delay: MAX_RETRY_DELAY,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecoveryTriggerV1 {
    Initial,
    Notification,
    Safety,
    Retry,
}

struct RecoveryFailureTrackerV1 {
    schedule: RecoveryScheduleV1,
    detail: Option<String>,
    consecutive: u64,
    warning_events: u64,
    last_warned_at: u64,
    next_delay: Duration,
}

impl RecoveryFailureTrackerV1 {
    fn new(schedule: RecoveryScheduleV1) -> Self {
        Self {
            schedule,
            detail: None,
            consecutive: 0,
            warning_events: 0,
            last_warned_at: 0,
            next_delay: schedule.initial_retry_delay,
        }
    }

    fn fail(&mut self, operation: &'static str, detail: String) -> Duration {
        if self.detail.as_deref() == Some(detail.as_str()) {
            self.consecutive = self.consecutive.saturating_add(1);
        } else {
            self.detail = Some(detail.clone());
            self.consecutive = 1;
            self.warning_events = 0;
            self.last_warned_at = 0;
            self.next_delay = self.schedule.initial_retry_delay;
        }
        if self.consecutive == 1 || self.consecutive.is_multiple_of(WARNING_EVERY_FAILURES) {
            let suppressed = self
                .consecutive
                .saturating_sub(self.last_warned_at)
                .saturating_sub(1);
            tracing::warn!(
                operation,
                error = %detail,
                attempt = self.consecutive,
                suppressed,
                "durable recovery attempt failed"
            );
            self.warning_events = self.warning_events.saturating_add(1);
            self.last_warned_at = self.consecutive;
        }
        let delay = self.next_delay;
        self.next_delay = self
            .next_delay
            .saturating_mul(2)
            .min(self.schedule.max_retry_delay);
        delay
    }

    fn recover(&mut self, operation: &'static str) {
        if self.consecutive > 0 {
            let suppressed = self.consecutive.saturating_sub(self.warning_events);
            tracing::info!(
                operation,
                failed_attempts = self.consecutive,
                suppressed,
                "durable recovery resumed after repeated failures"
            );
        }
        *self = Self::new(self.schedule);
    }
}

#[derive(Default)]
pub(super) struct RecoveryWarningTrackerV1 {
    skipped: u64,
    warning_events: u64,
    last_warned_at: u64,
}

impl RecoveryWarningTrackerV1 {
    pub(super) fn warn(&mut self, operation: &'static str, detail: &str) {
        self.skipped = self.skipped.saturating_add(1);
        if self.skipped == 1 || self.skipped.is_multiple_of(WARNING_EVERY_FAILURES) {
            let suppressed = self
                .skipped
                .saturating_sub(self.last_warned_at)
                .saturating_sub(1);
            tracing::warn!(
                operation,
                error = detail,
                skipped = self.skipped,
                suppressed,
                "durable recovery receipt skipped"
            );
            self.warning_events = self.warning_events.saturating_add(1);
            self.last_warned_at = self.skipped;
        }
    }

    pub(super) fn recover(&mut self, operation: &'static str) {
        if self.skipped > 0 {
            let suppressed = self.skipped.saturating_sub(self.warning_events);
            tracing::info!(
                operation,
                skipped = self.skipped,
                suppressed,
                "durable recovery receipt validation resumed"
            );
        }
        *self = Self::default();
    }
}

/// Runs one immediate restart scan, then wakes on exact durable writes with a
/// low-frequency safety reconciliation. Repeated failures back off instead of
/// allowing write notifications to spin a broken database path.
pub(super) async fn run_recovery_loop<F, Fut>(
    mut signal: tokio::sync::watch::Receiver<u64>,
    cancellation: CancellationToken,
    schedule: RecoveryScheduleV1,
    operation: &'static str,
    mut scan: F,
) where
    F: FnMut(RecoveryTriggerV1) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let mut trigger = RecoveryTriggerV1::Initial;
    let mut failures = RecoveryFailureTrackerV1::new(schedule);
    loop {
        match scan(trigger).await {
            Ok(()) => {
                failures.recover(operation);
                match signal.has_changed() {
                    Ok(true) => {
                        signal.borrow_and_update();
                        trigger = RecoveryTriggerV1::Notification;
                        continue;
                    }
                    Ok(false) => {}
                    Err(_) => return,
                }
            }
            Err(detail) => {
                let delay = failures.fail(operation, detail);
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return,
                    fired = timer_fired(operation, delay) => {
                        if !fired {
                            return;
                        }
                        trigger = RecoveryTriggerV1::Retry;
                        continue;
                    }
                }
            }
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return,
            changed = signal.changed() => {
                if changed.is_err() {
                    return;
                }
                trigger = RecoveryTriggerV1::Notification;
            }
            fired = timer_fired(operation, schedule.safety_interval) => {
                if !fired {
                    return;
                }
                trigger = RecoveryTriggerV1::Safety;
            }
        }
    }
}

/// A loop that cannot time its next wake stops rather than spin.
async fn timer_fired(operation: &'static str, after: Duration) -> bool {
    match native_runtime::sleep_until(Instant::now() + after).await {
        Ok(()) => true,
        Err(error) => {
            tracing::error!(
                operation,
                %error,
                "durable recovery loop stopped: its timer is unavailable"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use tracedecay_runtime_core::cancellation::CancellationToken;
    use tracedecay_runtime_core::native_runtime::{self, NativeTask, NativeTaskOutcome};

    use super::{
        RecoveryFailureTrackerV1, RecoveryScheduleV1, RecoveryTriggerV1, RecoveryWarningTrackerV1,
        run_recovery_loop,
    };
    use crate::invocation::types::WorkDurableWriteSignalV1;

    /// Bounds every event wait so a regression fails instead of hanging.
    const HANG_GUARD: Duration = Duration::from_secs(30);
    const IDLE_SCHEDULE: RecoveryScheduleV1 =
        RecoveryScheduleV1::with_safety_interval(Duration::from_mins(1));

    /// Every scan one recovery loop ran, with when it started.
    #[derive(Clone)]
    struct Scans(Arc<tokio::sync::watch::Sender<Vec<(Instant, RecoveryTriggerV1)>>>);

    impl Scans {
        fn new() -> Self {
            Self(Arc::new(tokio::sync::watch::channel(Vec::new()).0))
        }

        fn record(&self, trigger: RecoveryTriggerV1) -> usize {
            let mut count = 0;
            self.0.send_modify(|scans| {
                scans.push((Instant::now(), trigger));
                count = scans.len();
            });
            count
        }

        async fn wait_for(&self, count: usize) -> Vec<(Instant, RecoveryTriggerV1)> {
            let mut scans = self.0.subscribe();
            native_runtime::timeout(HANG_GUARD, scans.wait_for(|scans| scans.len() >= count))
                .await
                .unwrap_or_else(|error| panic!("scan {count} never ran: {error}"))
                .expect("scan log")
                .clone()
        }

        fn triggers(&self) -> Vec<RecoveryTriggerV1> {
            self.0
                .borrow()
                .iter()
                .map(|&(_, trigger)| trigger)
                .collect()
        }
    }

    /// Runs a recovery loop on a native task whose first `failures` scans fail.
    fn spawn_recovery(
        signal: &WorkDurableWriteSignalV1,
        cancellation: &CancellationToken,
        schedule: RecoveryScheduleV1,
        scans: &Scans,
        failures: usize,
    ) -> NativeTask<()> {
        let scans = scans.clone();
        NativeTask::spawn(run_recovery_loop(
            signal.subscribe(),
            cancellation.clone(),
            schedule,
            "fixture recovery",
            move |trigger| {
                let attempt = scans.record(trigger);
                std::future::ready(if attempt <= failures {
                    Err("same fixture failure".to_owned())
                } else {
                    Ok(())
                })
            },
        ))
        .expect("native recovery task")
    }

    async fn assert_cancelled_loop_returns(task: NativeTask<()>) {
        assert!(matches!(
            native_runtime::timeout(HANG_GUARD, task.wait()).await,
            Ok(NativeTaskOutcome::Completed(()))
        ));
    }

    // Each test runs with no Tokio runtime entered: recovery loops time their
    // wakes on the native runtime.

    #[test]
    fn idle_recovery_scans_only_at_mount_and_safety_intervals() {
        const SAFETY_INTERVAL: Duration = Duration::from_millis(20);
        const SAFETY_WAKES: usize = 3;
        let schedule = RecoveryScheduleV1::with_safety_interval(SAFETY_INTERVAL);

        native_runtime::block_on(async {
            for project_count in [1, 10, 100] {
                let cancellation = CancellationToken::new();
                let signals: Vec<_> = (0..project_count)
                    .map(|_| WorkDurableWriteSignalV1::default())
                    .collect();
                let scans: Vec<_> = (0..project_count).map(|_| Scans::new()).collect();
                let tasks: Vec<_> = signals
                    .iter()
                    .zip(&scans)
                    .map(|(signal, scans)| {
                        spawn_recovery(signal, &cancellation, schedule, scans, 0)
                    })
                    .collect();

                for scans in &scans {
                    let log = scans.wait_for(1 + SAFETY_WAKES).await;
                    assert_eq!(log[0].1, RecoveryTriggerV1::Initial);
                    for pair in log.windows(2) {
                        assert_eq!(pair[1].1, RecoveryTriggerV1::Safety);
                        let gap = pair[1].0 - pair[0].0;
                        assert!(
                            gap >= SAFETY_INTERVAL,
                            "an idle loop rescanned after {gap:?}, inside its safety interval"
                        );
                    }
                }

                cancellation.cancel();
                for task in tasks {
                    assert_cancelled_loop_returns(task).await;
                }
                drop(signals);
            }
        })
        .expect("native runtime");
    }

    #[test]
    fn durable_write_signal_scans_within_one_poll_tick() {
        native_runtime::block_on(async {
            let signal = WorkDurableWriteSignalV1::default();
            let cancellation = CancellationToken::new();
            let scans = Scans::new();
            let task_scans = scans.clone();
            // One task drives the loop and the writer, so all 100 writes land
            // before the loop can observe any of them.
            let recovery = run_recovery_loop(
                signal.subscribe(),
                cancellation.clone(),
                IDLE_SCHEDULE,
                "fixture signalled recovery",
                move |trigger| {
                    task_scans.record(trigger);
                    std::future::ready(Ok(()))
                },
            );
            let writer = async {
                scans.wait_for(1).await;
                for _ in 0..100 {
                    signal.bump();
                }
                scans.wait_for(2).await;
                cancellation.cancel();
            };
            tokio::join!(recovery, writer);

            assert_eq!(
                scans.triggers(),
                [RecoveryTriggerV1::Initial, RecoveryTriggerV1::Notification]
            );
        })
        .expect("native runtime");
    }

    #[test]
    fn cancellation_joins_before_recovery_restart() {
        native_runtime::block_on(async {
            let scans = Scans::new();
            for restart in 1..=2 {
                let signal = WorkDurableWriteSignalV1::default();
                let cancellation = CancellationToken::new();
                let task = spawn_recovery(&signal, &cancellation, IDLE_SCHEDULE, &scans, 0);
                scans.wait_for(restart).await;
                assert!(!task.is_finished());

                cancellation.cancel();
                assert_cancelled_loop_returns(task).await;
            }
            assert_eq!(
                scans.triggers(),
                [RecoveryTriggerV1::Initial, RecoveryTriggerV1::Initial]
            );
        })
        .expect("native runtime");
    }

    #[test]
    fn repeated_failures_back_off_to_the_retry_cap() {
        let schedule = RecoveryScheduleV1 {
            initial_retry_delay: Duration::from_millis(10),
            max_retry_delay: Duration::from_millis(40),
            ..IDLE_SCHEDULE
        };
        let backoff = [10, 20, 40, 40, 40, 40].map(Duration::from_millis);
        let mut tracker = RecoveryFailureTrackerV1::new(schedule);
        let delays = backoff.map(|_| tracker.fail("fixture recovery", "same failure".to_owned()));
        assert_eq!(delays, backoff);

        native_runtime::block_on(async {
            let signal = WorkDurableWriteSignalV1::default();
            let cancellation = CancellationToken::new();
            let scans = Scans::new();
            let task = spawn_recovery(&signal, &cancellation, schedule, &scans, backoff.len());

            let log = scans.wait_for(backoff.len() + 1).await;
            for (pair, delay) in log.windows(2).zip(backoff) {
                assert_eq!(pair[1].1, RecoveryTriggerV1::Retry);
                let gap = pair[1].0 - pair[0].0;
                assert!(
                    gap >= delay,
                    "a failed scan retried after {gap:?}, before its {delay:?} backoff"
                );
            }

            cancellation.cancel();
            assert_cancelled_loop_returns(task).await;
        })
        .expect("native runtime");
    }

    #[test]
    fn identical_failure_logs_are_byte_and_event_bounded() {
        let ((), output) = tracedecay_runtime_core::logging::capture_formatted_tracing(|| {
            let mut failures = RecoveryFailureTrackerV1::new(IDLE_SCHEDULE);
            for _ in 0..100 {
                failures.fail("fixture recovery", "same fixture failure".to_owned());
            }
            failures.recover("fixture recovery");
        });

        assert_eq!(
            output.matches("durable recovery attempt failed").count(),
            11
        );
        assert_eq!(
            output
                .matches("durable recovery resumed after repeated failures")
                .count(),
            1
        );
        assert_eq!(output.matches("same fixture failure").count(), 11);
        assert!(
            output.len() < 8_192,
            "rate-limited failure output was {} bytes",
            output.len()
        );
    }

    #[test]
    fn skipped_receipt_logs_are_byte_and_event_bounded() {
        let ((), output) = tracedecay_runtime_core::logging::capture_formatted_tracing(|| {
            let mut warnings = RecoveryWarningTrackerV1::default();
            for _ in 0..100 {
                warnings.warn("fixture recovery", "invalid fixture receipt");
            }
            warnings.recover("fixture recovery");
        });

        assert_eq!(
            output.matches("durable recovery receipt skipped").count(),
            11
        );
        assert_eq!(
            output
                .matches("durable recovery receipt validation resumed")
                .count(),
            1
        );
        assert_eq!(output.matches("invalid fixture receipt").count(), 11);
        assert!(
            output.len() < 8_192,
            "rate-limited skipped-receipt output was {} bytes",
            output.len()
        );
    }
}
