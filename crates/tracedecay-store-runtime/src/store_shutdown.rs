//! Typed receipts and bounded joins for named store-runtime shutdown tasks.
//!
//! `join_shutdown_tasks_until` reserves an abort budget inside the caller's
//! deadline: tasks get the cooperative window first, then stragglers are
//! aborted and joined so nothing detaches past shutdown while the receipt
//! still names every owner truthfully.

use std::collections::HashMap;
use std::future::Future;

use tracedecay_runtime_core::DAEMON_TASK_ABORT_DEADLINE;

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

pub async fn join_shutdown_tasks_until<Tasks, Task>(
    deadline: tokio::time::Instant,
    tasks: Tasks,
) -> ShutdownTaskReceipt
where
    Tasks: IntoIterator<Item = (String, Option<tokio::task::AbortHandle>, Task)>,
    Task: Future<Output = ShutdownStatus> + Send + 'static,
{
    let now = tokio::time::Instant::now();
    let cooperative_deadline =
        if deadline.saturating_duration_since(now) > DAEMON_TASK_ABORT_DEADLINE {
            deadline
                .checked_sub(DAEMON_TASK_ABORT_DEADLINE)
                .unwrap_or(deadline)
        } else {
            now
        };
    let mut joins = tokio::task::JoinSet::new();
    let mut pending = HashMap::new();
    for (ordinal, (owner, owned_task_abort, task)) in tasks.into_iter().enumerate() {
        let wrapper_abort = joins.spawn(task);
        pending.insert(
            wrapper_abort.id(),
            (ordinal, owner, owned_task_abort, wrapper_abort),
        );
    }

    let mut outcomes = Vec::new();
    while !joins.is_empty() {
        match tracing::Instrument::instrument(
            tokio::time::timeout_at(cooperative_deadline, joins.join_next_with_id()),
            tracing::trace_span!("daemon.store_runtime.shutdown_wait"),
        )
        .await
        {
            Ok(Some(Ok((id, task_status)))) => {
                if let Some((ordinal, owner, _, _)) = pending.remove(&id) {
                    outcomes.push((
                        ordinal,
                        ShutdownTaskOutcome {
                            owner,
                            status: task_status,
                        },
                    ));
                }
            }
            Ok(Some(Err(error))) => {
                if let Some((ordinal, owner, _, _)) = pending.remove(&error.id()) {
                    outcomes.push((
                        ordinal,
                        ShutdownTaskOutcome {
                            owner,
                            status: ShutdownStatus::Failed(error.to_string()),
                        },
                    ));
                }
            }
            Ok(None) => break,
            Err(_) => {
                for (_, _, owned_task_abort, wrapper_abort) in pending.values() {
                    if let Some(owned_task_abort) = owned_task_abort {
                        owned_task_abort.abort();
                    } else {
                        wrapper_abort.abort();
                    }
                }
                outcomes.extend(pending.drain().map(|(_, (ordinal, owner, _, _))| {
                    (
                        ordinal,
                        ShutdownTaskOutcome {
                            owner,
                            status: ShutdownStatus::TimedOut,
                        },
                    )
                }));
                while !joins.is_empty() {
                    match tracing::Instrument::instrument(
                        tokio::time::timeout_at(deadline, joins.join_next()),
                        tracing::trace_span!("daemon.store_runtime.shutdown_abort_wait"),
                    )
                    .await
                    {
                        Ok(Some(_)) => {}
                        Ok(None) | Err(_) => break,
                    }
                }
                break;
            }
        }
    }
    outcomes.sort_by_key(|(ordinal, _)| *ordinal);
    ShutdownTaskReceipt {
        outcomes: outcomes.into_iter().map(|(_, outcome)| outcome).collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::ShutdownStatus;

    struct Dropped(Arc<AtomicBool>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[tokio::test]
    async fn panicked_and_cancelled_tasks_are_reported_as_failed() {
        let panicked = tokio::spawn(async {
            panic!("shutdown task panic");
        });
        let cancelled = tokio::spawn(std::future::pending::<()>());
        cancelled.abort();

        let receipt = super::join_shutdown_tasks_until(
            tokio::time::Instant::now() + Duration::from_secs(1),
            [
                ("panicked".to_owned(), panicked),
                ("cancelled".to_owned(), cancelled),
            ]
            .map(|(owner, task)| {
                (owner, None, async move {
                    match task.await {
                        Ok(()) => ShutdownStatus::Clean,
                        Err(error) => ShutdownStatus::Failed(error.to_string()),
                    }
                })
            }),
        )
        .await;

        assert_eq!(receipt.outcomes[0].owner, "panicked");
        assert_eq!(receipt.outcomes[1].owner, "cancelled");
        assert!(matches!(
            receipt.outcomes[0].status,
            ShutdownStatus::Failed(_)
        ));
        assert!(matches!(
            receipt.outcomes[1].status,
            ShutdownStatus::Failed(_)
        ));
        assert!(!receipt.is_clean());
    }

    #[tokio::test]
    async fn typed_task_failure_is_preserved_verbatim() {
        let receipt = super::join_shutdown_tasks_until(
            tokio::time::Instant::now() + Duration::from_secs(1),
            [("drain".to_owned(), None, async {
                ShutdownStatus::Failed("project request drain timed out".to_owned())
            })],
        )
        .await;

        assert_eq!(receipt.outcomes.len(), 1);
        assert_eq!(
            receipt.outcomes[0].status,
            ShutdownStatus::Failed("project request drain timed out".to_owned())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_aborts_stragglers_and_names_the_exact_owner() {
        let dropped = Arc::new(AtomicBool::new(false));
        let started = Arc::new(tokio::sync::Notify::new());
        let task_dropped = Arc::clone(&dropped);
        let task_started = Arc::clone(&started);
        let straggler = tokio::spawn(async move {
            let _dropped = Dropped(task_dropped);
            task_started.notify_one();
            std::future::pending::<ShutdownStatus>().await
        });
        started.notified().await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);

        let join = super::join_shutdown_tasks_until(
            deadline,
            [("straggler".to_owned(), Some(straggler.abort_handle()), {
                async move {
                    match straggler.await {
                        Ok(status) => status,
                        Err(error) => ShutdownStatus::Failed(error.to_string()),
                    }
                }
            })],
        );
        tokio::pin!(join);
        tokio::time::advance(Duration::from_secs(1)).await;
        let receipt = join.await;
        tokio::task::yield_now().await;

        assert_eq!(
            receipt.outcomes,
            [super::ShutdownTaskOutcome {
                owner: "straggler".to_string(),
                status: ShutdownStatus::TimedOut,
            }]
        );
        assert!(dropped.load(Ordering::Acquire));
    }

    #[tokio::test(start_paused = true)]
    async fn sub_abort_reserve_deadline_aborts_immediately_and_preserves_join_time() {
        let dropped = Arc::new(AtomicBool::new(false));
        let task_dropped = Arc::clone(&dropped);
        let task = tokio::spawn(async move {
            let _dropped = Dropped(task_dropped);
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        let abort = task.abort_handle();
        let started = tokio::time::Instant::now();

        let receipt = super::join_shutdown_tasks_until(
            started + Duration::from_secs(1),
            [("short-budget-owner".to_owned(), Some(abort), async move {
                match task.await {
                    Ok(()) => ShutdownStatus::Clean,
                    Err(error) => ShutdownStatus::Failed(error.to_string()),
                }
            })],
        )
        .await;

        assert_eq!(tokio::time::Instant::now(), started);
        assert_eq!(
            receipt.outcomes,
            [super::ShutdownTaskOutcome {
                owner: "short-budget-owner".to_owned(),
                status: ShutdownStatus::TimedOut,
            }]
        );
        assert!(dropped.load(Ordering::Acquire));
    }
}
