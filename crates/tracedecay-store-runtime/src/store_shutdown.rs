//! Typed receipts and bounded joins for named store-runtime shutdown tasks.
//!
//! `join_shutdown_tasks_until` polls its tasks in place and ends their
//! cooperative window `DAEMON_TASK_ABORT_DEADLINE` before the caller's
//! deadline, then drops the stragglers, so nothing outlives the join while the
//! receipt still names every owner truthfully.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::time::Instant;

use futures_util::FutureExt;
use futures_util::stream::{FuturesUnordered, StreamExt};
use tracedecay_runtime_core::DAEMON_TASK_ABORT_DEADLINE;
use tracedecay_runtime_core::native_runtime::{self, TimeoutError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShutdownStatus {
    Clean,
    Failed(String),
    TimedOut,
}

impl ShutdownStatus {
    pub fn is_clean(&self) -> bool {
        matches!(self, Self::Clean)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShutdownTaskOutcome {
    pub owner: String,
    pub status: ShutdownStatus,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShutdownTaskReceipt {
    pub outcomes: Vec<ShutdownTaskOutcome>,
}

impl ShutdownTaskReceipt {
    #[must_use]
    pub fn failed(owner: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            outcomes: vec![ShutdownTaskOutcome {
                owner: owner.into(),
                status: ShutdownStatus::Failed(error.into()),
            }],
        }
    }

    #[must_use]
    pub fn timed_out(owner: impl Into<String>) -> Self {
        Self {
            outcomes: vec![ShutdownTaskOutcome {
                owner: owner.into(),
                status: ShutdownStatus::TimedOut,
            }],
        }
    }

    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.outcomes
            .iter()
            .all(|outcome| outcome.status == ShutdownStatus::Clean)
    }

    #[must_use]
    pub fn status(&self) -> ShutdownStatus {
        let failures = self
            .outcomes
            .iter()
            .filter_map(|outcome| match &outcome.status {
                ShutdownStatus::Failed(error) => Some(format!("{}: {error}", outcome.owner)),
                ShutdownStatus::Clean | ShutdownStatus::TimedOut => None,
            })
            .collect::<Vec<_>>();
        if !failures.is_empty() {
            ShutdownStatus::Failed(failures.join("; "))
        } else if self
            .outcomes
            .iter()
            .any(|outcome| outcome.status == ShutdownStatus::TimedOut)
        {
            ShutdownStatus::TimedOut
        } else {
            ShutdownStatus::Clean
        }
    }

    pub fn extend(&mut self, mut other: Self) {
        self.outcomes.append(&mut other.outcomes);
    }

    pub fn retain_failures_from(&mut self, failures: &[ShutdownTaskOutcome]) {
        for failure in failures {
            let ShutdownStatus::Failed(prior_error) = &failure.status else {
                continue;
            };
            match self
                .outcomes
                .iter()
                .position(|outcome| outcome.owner == failure.owner)
            {
                Some(index) => match self.outcomes[index].status.clone() {
                    ShutdownStatus::Clean => {
                        self.outcomes[index].status = failure.status.clone();
                    }
                    ShutdownStatus::Failed(error) if error != *prior_error => {
                        self.outcomes[index].status =
                            ShutdownStatus::Failed(format!("{prior_error}; retry failed: {error}"));
                    }
                    ShutdownStatus::Failed(_) => {}
                    ShutdownStatus::TimedOut => self.outcomes.insert(index, failure.clone()),
                },
                None => self.outcomes.push(failure.clone()),
            }
        }
    }

    #[must_use]
    pub fn failed_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| matches!(outcome.status, ShutdownStatus::Failed(_)))
            .count()
    }

    #[must_use]
    pub fn timed_out_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.status == ShutdownStatus::TimedOut)
            .count()
    }
}

// The `AbortHandle` element is unused; it goes away with the Tokio runtime in
// the final native-runtime cutover PR.
pub async fn join_shutdown_tasks_until<Tasks, Task>(
    deadline: impl Into<Instant>,
    tasks: Tasks,
) -> ShutdownTaskReceipt
where
    Tasks: IntoIterator<Item = (String, Option<tokio::task::AbortHandle>, Task)>,
    Task: Future<Output = ShutdownStatus>,
{
    let deadline = deadline.into();
    // Past `deadline - reserve` (or below the clock's origin) the window is
    // already over.
    let cooperative_deadline = deadline
        .checked_sub(DAEMON_TASK_ABORT_DEADLINE)
        .unwrap_or_else(Instant::now);
    let mut outcomes = Vec::new();
    let mut running = FuturesUnordered::new();
    for (ordinal, (owner, _, task)) in tasks.into_iter().enumerate() {
        outcomes.push((owner, None));
        running.push(
            AssertUnwindSafe(task)
                .catch_unwind()
                .map(move |result| (ordinal, result)),
        );
    }
    let drained = tracing::Instrument::instrument(
        native_runtime::timeout_at(cooperative_deadline, async {
            while let Some((ordinal, result)) = running.next().await {
                outcomes[ordinal].1 = Some(match result {
                    Ok(status) => status,
                    Err(panic) => ShutdownStatus::Failed(format!(
                        "shutdown task panicked: {}",
                        panic
                            .downcast_ref::<&str>()
                            .map(|message| (*message).to_owned())
                            .or_else(|| panic.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "non-string panic payload".to_owned())
                    )),
                });
            }
        }),
        tracing::trace_span!("daemon.store_runtime.shutdown_wait"),
    )
    .await;
    let straggler = match drained {
        Ok(()) | Err(TimeoutError::Elapsed) => ShutdownStatus::TimedOut,
        Err(error @ TimeoutError::TimerUnavailable(_)) => ShutdownStatus::Failed(error.to_string()),
    };
    drop(running);
    ShutdownTaskReceipt {
        outcomes: outcomes
            .into_iter()
            .map(|(owner, status)| ShutdownTaskOutcome {
                owner,
                status: status.unwrap_or_else(|| straggler.clone()),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use futures_util::FutureExt;
    use futures_util::future::BoxFuture;
    use tracedecay_runtime_core::DAEMON_TASK_ABORT_DEADLINE;
    use tracedecay_runtime_core::native_runtime;

    use super::{ShutdownStatus, ShutdownTaskOutcome, ShutdownTaskReceipt};

    const COOPERATIVE_WINDOW: Duration = Duration::from_millis(50);

    struct Dropped(Arc<AtomicBool>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    fn join(
        deadline: Instant,
        tasks: Vec<(&str, BoxFuture<'static, ShutdownStatus>)>,
    ) -> ShutdownTaskReceipt {
        native_runtime::block_on(super::join_shutdown_tasks_until(
            deadline,
            tasks
                .into_iter()
                .map(|(owner, task)| (owner.to_owned(), None, task)),
        ))
        .expect("native runtime")
    }

    fn outcome(owner: &str, status: ShutdownStatus) -> ShutdownTaskOutcome {
        ShutdownTaskOutcome {
            owner: owner.to_owned(),
            status,
        }
    }

    fn straggler(dropped: &Arc<AtomicBool>) -> BoxFuture<'static, ShutdownStatus> {
        let dropped = Dropped(Arc::clone(dropped));
        async move {
            let _dropped = dropped;
            std::future::pending().await
        }
        .boxed()
    }

    #[test]
    fn a_panicking_task_fails_only_its_own_owner() {
        let receipt = join(
            Instant::now() + DAEMON_TASK_ABORT_DEADLINE + COOPERATIVE_WINDOW,
            vec![
                (
                    "panicked",
                    futures_util::future::lazy(|_| -> ShutdownStatus {
                        panic!("shutdown task panic")
                    })
                    .boxed(),
                ),
                ("clean", async { ShutdownStatus::Clean }.boxed()),
            ],
        );

        assert_eq!(
            receipt.outcomes,
            [
                outcome(
                    "panicked",
                    ShutdownStatus::Failed(
                        "shutdown task panicked: shutdown task panic".to_owned()
                    )
                ),
                outcome("clean", ShutdownStatus::Clean),
            ]
        );
    }

    #[test]
    fn typed_task_failure_is_preserved_verbatim() {
        let receipt = join(
            Instant::now() + DAEMON_TASK_ABORT_DEADLINE + COOPERATIVE_WINDOW,
            vec![(
                "drain",
                async { ShutdownStatus::Failed("project request drain timed out".to_owned()) }
                    .boxed(),
            )],
        );

        assert_eq!(
            receipt.outcomes,
            [outcome(
                "drain",
                ShutdownStatus::Failed("project request drain timed out".to_owned())
            )]
        );
    }

    #[test]
    fn stragglers_are_dropped_after_the_cooperative_window_and_named_exactly() {
        let dropped = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let deadline = started + DAEMON_TASK_ABORT_DEADLINE + COOPERATIVE_WINDOW;

        let receipt = join(
            deadline,
            vec![
                ("straggler", straggler(&dropped)),
                (
                    "late-drain",
                    async move {
                        native_runtime::sleep_until(started + COOPERATIVE_WINDOW / 5)
                            .await
                            .expect("native timer");
                        ShutdownStatus::Clean
                    }
                    .boxed(),
                ),
            ],
        );
        let returned = Instant::now();

        assert_eq!(
            receipt.outcomes,
            [
                outcome("straggler", ShutdownStatus::TimedOut),
                outcome("late-drain", ShutdownStatus::Clean),
            ]
        );
        assert!(
            dropped.load(Ordering::Acquire),
            "the straggler is dropped before the receipt returns"
        );
        assert!(
            returned >= started + COOPERATIVE_WINDOW,
            "stragglers keep their cooperative window"
        );
        assert!(
            returned < deadline,
            "the abort reserve stays inside the caller's deadline"
        );
    }

    #[test]
    fn a_deadline_inside_the_abort_reserve_drops_stragglers_without_waiting() {
        let dropped = Arc::new(AtomicBool::new(false));
        let deadline = Instant::now() + DAEMON_TASK_ABORT_DEADLINE;

        let receipt = join(deadline, vec![("short-budget-owner", straggler(&dropped))]);

        assert!(
            Instant::now() < deadline,
            "the join returns without spending the caller's remaining budget"
        );
        assert_eq!(
            receipt.outcomes,
            [outcome("short-budget-owner", ShutdownStatus::TimedOut)]
        );
        assert!(dropped.load(Ordering::Acquire));
    }
}
