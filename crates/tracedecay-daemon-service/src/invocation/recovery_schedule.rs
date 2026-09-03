use std::future::Future;
use std::time::Duration;

use tracedecay_runtime_core::cancellation::CancellationToken;

const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(5);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(300);
const WARNING_EVERY_FAILURES: u64 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecoveryTriggerV1 {
    Initial,
    Notification,
    Safety,
    Retry,
}

struct RecoveryFailureTrackerV1 {
    detail: Option<String>,
    consecutive: u64,
    warning_events: u64,
    last_warned_at: u64,
    next_delay: Duration,
}

impl Default for RecoveryFailureTrackerV1 {
    fn default() -> Self {
        Self {
            detail: None,
            consecutive: 0,
            warning_events: 0,
            last_warned_at: 0,
            next_delay: INITIAL_RETRY_DELAY,
        }
    }
}

impl RecoveryFailureTrackerV1 {
    fn fail(&mut self, operation: &'static str, detail: String) -> Duration {
        if self.detail.as_deref() == Some(detail.as_str()) {
            self.consecutive = self.consecutive.saturating_add(1);
        } else {
            self.detail = Some(detail.clone());
            self.consecutive = 1;
            self.warning_events = 0;
            self.last_warned_at = 0;
            self.next_delay = INITIAL_RETRY_DELAY;
        }
        if self.consecutive == 1 || self.consecutive % WARNING_EVERY_FAILURES == 0 {
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
        self.next_delay = self.next_delay.saturating_mul(2).min(MAX_RETRY_DELAY);
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
        *self = Self::default();
    }
}

/// Runs one immediate restart scan, then wakes on exact durable writes with a
/// low-frequency safety reconciliation. Repeated failures back off instead of
/// allowing write notifications to spin a broken database path.
pub(super) async fn run_recovery_loop<F, Fut>(
    mut signal: tokio::sync::watch::Receiver<u64>,
    cancellation: CancellationToken,
    safety_interval: Duration,
    operation: &'static str,
    mut scan: F,
) where
    F: FnMut(RecoveryTriggerV1) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let mut trigger = RecoveryTriggerV1::Initial;
    let mut failures = RecoveryFailureTrackerV1::default();
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
                    () = tokio::time::sleep(delay) => {
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
            () = tokio::time::sleep(safety_interval) => {
                trigger = RecoveryTriggerV1::Safety;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::io::Write;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use tracedecay_runtime_core::cancellation::CancellationToken;
    use tracing_subscriber::fmt::MakeWriter;

    use super::{RecoveryFailureTrackerV1, RecoveryTriggerV1, run_recovery_loop};
    use crate::invocation::types::WorkDurableWriteSignalV1;

    #[derive(Clone)]
    struct CapturedWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    struct CapturedGuard {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for CapturedGuard {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.bytes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for CapturedWriter {
        type Writer = CapturedGuard;

        fn make_writer(&'a self) -> Self::Writer {
            CapturedGuard {
                bytes: Arc::clone(&self.bytes),
            }
        }
    }

    async fn wait_for_count(counter: &AtomicUsize, expected: usize) {
        for _ in 0..10_000 {
            if counter.load(Ordering::SeqCst) == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(counter.load(Ordering::SeqCst), expected);
    }

    #[tokio::test(start_paused = true)]
    async fn idle_recovery_scans_only_at_mount_and_safety_intervals() {
        const PROJECT_COUNTS: [usize; 3] = [1, 10, 100];
        const SAFETY_INTERVAL: Duration = Duration::from_secs(60);
        const WINDOW: Duration = Duration::from_secs(180);

        for project_count in PROJECT_COUNTS {
            let scans = Arc::new(AtomicUsize::new(0));
            let notifications = Arc::new(AtomicUsize::new(0));
            let cancellation = CancellationToken::new();
            let mut tasks = Vec::new();
            let mut signals = Vec::new();
            for _ in 0..project_count {
                let signal = WorkDurableWriteSignalV1::default();
                let scans = Arc::clone(&scans);
                let notifications = Arc::clone(&notifications);
                let task_cancellation = cancellation.clone();
                tasks.push(tokio::spawn(run_recovery_loop(
                    signal.subscribe(),
                    task_cancellation,
                    SAFETY_INTERVAL,
                    "fixture idle recovery",
                    move |trigger| {
                        let scans = Arc::clone(&scans);
                        let notifications = Arc::clone(&notifications);
                        async move {
                            scans.fetch_add(1, Ordering::SeqCst);
                            if trigger == RecoveryTriggerV1::Notification {
                                notifications.fetch_add(1, Ordering::SeqCst);
                            }
                            Ok(())
                        }
                    },
                )));
                signals.push(signal);
            }
            wait_for_count(&scans, project_count).await;
            for interval in 1..=3 {
                tokio::time::advance(SAFETY_INTERVAL).await;
                wait_for_count(&scans, project_count * (interval + 1)).await;
            }

            assert_eq!(notifications.load(Ordering::SeqCst), 0);
            assert_eq!(
                scans.load(Ordering::SeqCst),
                project_count
                    * (1 + WINDOW.as_secs() as usize / SAFETY_INTERVAL.as_secs() as usize)
            );
            cancellation.cancel();
            for task in tasks {
                task.await.expect("idle recovery task");
            }
            drop(signals);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn durable_write_signal_scans_within_one_poll_tick() {
        let signal = WorkDurableWriteSignalV1::default();
        let cancellation = CancellationToken::new();
        let scans = Arc::new(AtomicUsize::new(0));
        let notifications = Arc::new(AtomicUsize::new(0));
        let task_scans = Arc::clone(&scans);
        let task_notifications = Arc::clone(&notifications);
        let task = tokio::spawn(run_recovery_loop(
            signal.subscribe(),
            cancellation.clone(),
            Duration::from_secs(60),
            "fixture signalled recovery",
            move |trigger| {
                let scans = Arc::clone(&task_scans);
                let notifications = Arc::clone(&task_notifications);
                async move {
                    scans.fetch_add(1, Ordering::SeqCst);
                    if trigger == RecoveryTriggerV1::Notification {
                        notifications.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(())
                }
            },
        ));
        tokio::task::yield_now().await;
        assert_eq!(scans.load(Ordering::SeqCst), 1);

        for _ in 0..100 {
            signal.bump();
        }
        tokio::task::yield_now().await;
        assert_eq!(scans.load(Ordering::SeqCst), 2);
        assert_eq!(notifications.load(Ordering::SeqCst), 1);

        cancellation.cancel();
        task.await.expect("signalled recovery task");
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_joins_before_recovery_restart() {
        let first_signal = WorkDurableWriteSignalV1::default();
        let first_cancellation = CancellationToken::new();
        let scans = Arc::new(AtomicUsize::new(0));
        let first_scans = Arc::clone(&scans);
        let first = tokio::spawn(run_recovery_loop(
            first_signal.subscribe(),
            first_cancellation.clone(),
            Duration::from_secs(60),
            "fixture restart recovery",
            move |_| {
                let scans = Arc::clone(&first_scans);
                async move {
                    scans.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        ));
        tokio::task::yield_now().await;
        assert_eq!(scans.load(Ordering::SeqCst), 1);
        assert!(!first.is_finished());

        first_cancellation.cancel();
        first.await.expect("cancelled recovery task joins");

        let second_signal = WorkDurableWriteSignalV1::default();
        let second_cancellation = CancellationToken::new();
        let second_scans = Arc::clone(&scans);
        let second = tokio::spawn(run_recovery_loop(
            second_signal.subscribe(),
            second_cancellation.clone(),
            Duration::from_secs(60),
            "fixture restart recovery",
            move |_| {
                let scans = Arc::clone(&second_scans);
                async move {
                    scans.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        ));
        tokio::task::yield_now().await;
        assert_eq!(scans.load(Ordering::SeqCst), 2);
        assert!(!second.is_finished());

        second_cancellation.cancel();
        second.await.expect("restarted recovery task joins");
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_failures_back_off_and_rate_limit_warnings() {
        let signal = WorkDurableWriteSignalV1::default();
        let cancellation = CancellationToken::new();
        let attempts = Arc::new(AtomicUsize::new(0));
        let task_attempts = Arc::clone(&attempts);
        let task = tokio::spawn(run_recovery_loop(
            signal.subscribe(),
            cancellation.clone(),
            Duration::from_secs(60),
            "fixture failing recovery",
            move |_| {
                let attempts = Arc::clone(&task_attempts);
                async move {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                    if attempt <= 100 {
                        Err("same fixture failure".to_owned())
                    } else {
                        Ok(())
                    }
                }
            },
        ));
        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        let mut elapsed = Duration::ZERO;
        let mut retry = Duration::from_secs(5);
        for expected in 2..=101 {
            tokio::time::advance(retry).await;
            elapsed += retry;
            tokio::task::yield_now().await;
            assert_eq!(attempts.load(Ordering::SeqCst), expected);
            retry = retry.saturating_mul(2).min(Duration::from_secs(300));
        }
        assert!(
            elapsed >= Duration::from_secs(300),
            "repeated failures must not spin"
        );

        cancellation.cancel();
        task.await.expect("failing recovery task");
    }

    #[test]
    fn identical_failure_logs_are_byte_and_event_bounded() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .without_time()
            .with_ansi(false)
            .with_writer(CapturedWriter {
                bytes: Arc::clone(&bytes),
            })
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut failures = RecoveryFailureTrackerV1::default();
            for _ in 0..100 {
                failures.fail("fixture recovery", "same fixture failure".to_owned());
            }
            failures.recover("fixture recovery");
        });
        let bytes = bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let output = String::from_utf8(bytes).expect("captured tracing is UTF-8");

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
}
