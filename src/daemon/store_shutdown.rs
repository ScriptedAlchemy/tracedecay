use std::collections::HashMap;
use std::future::Future;

use super::{DAEMON_TASK_ABORT_DEADLINE, StoreAdministration};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ShutdownTaskStatus {
    Clean,
    Failed,
    TimedOut,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct ShutdownTaskOutcome {
    pub(super) owner: String,
    pub(super) status: ShutdownTaskStatus,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct ShutdownTaskReceipt {
    pub(super) outcomes: Vec<ShutdownTaskOutcome>,
}

impl ShutdownTaskReceipt {
    pub(super) fn timed_out(owner: impl Into<String>) -> Self {
        Self {
            outcomes: vec![ShutdownTaskOutcome {
                owner: owner.into(),
                status: ShutdownTaskStatus::TimedOut,
            }],
        }
    }

    pub(super) fn is_clean(&self) -> bool {
        self.outcomes
            .iter()
            .all(|outcome| outcome.status == ShutdownTaskStatus::Clean)
    }

    pub(super) fn extend(&mut self, mut other: Self) {
        self.outcomes.append(&mut other.outcomes);
    }

    pub(super) fn failed_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.status == ShutdownTaskStatus::Failed)
            .count()
    }

    pub(super) fn timed_out_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.status == ShutdownTaskStatus::TimedOut)
            .count()
    }
}

pub(super) async fn join_shutdown_tasks_until<Tasks, Task>(
    deadline: tokio::time::Instant,
    tasks: Tasks,
) -> ShutdownTaskReceipt
where
    Tasks: IntoIterator<Item = (String, Option<tokio::task::AbortHandle>, Task)>,
    Task: Future<Output = std::result::Result<(), ()>> + Send + 'static,
{
    let now = tokio::time::Instant::now();
    let cooperative_deadline =
        if deadline.saturating_duration_since(now) > DAEMON_TASK_ABORT_DEADLINE {
            deadline
                .checked_sub(DAEMON_TASK_ABORT_DEADLINE)
                .unwrap_or(deadline)
        } else {
            deadline
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
        match tokio::time::timeout_at(cooperative_deadline, joins.join_next_with_id()).await {
            Ok(Some(Ok((id, task_result)))) => {
                if let Some((ordinal, owner, _, _)) = pending.remove(&id) {
                    outcomes.push((
                        ordinal,
                        ShutdownTaskOutcome {
                            owner,
                            status: if task_result.is_ok() {
                                ShutdownTaskStatus::Clean
                            } else {
                                ShutdownTaskStatus::Failed
                            },
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
                            status: ShutdownTaskStatus::Failed,
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
                            status: ShutdownTaskStatus::TimedOut,
                        },
                    )
                }));
                while !joins.is_empty() {
                    match tokio::time::timeout_at(deadline, joins.join_next()).await {
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

impl StoreAdministration {
    pub(super) async fn track_project_server_retirement(&self, task: tokio::task::JoinHandle<()>) {
        let mut retirements = self.project_server_retirements.lock().await;
        retirements.retain(|retirement| !retirement.is_finished());
        retirements.push(task);
    }

    #[cfg(any(test, feature = "test-transport"))]
    pub(super) async fn join_project_server_retirements(&self) {
        let retirements = std::mem::take(&mut *self.project_server_retirements.lock().await);
        for retirement in retirements {
            let _ = retirement.await;
        }
    }

    pub(super) async fn join_project_server_retirements_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> ShutdownTaskReceipt {
        let mut retirements =
            match tokio::time::timeout_at(deadline, self.project_server_retirements.lock()).await {
                Ok(retirements) => retirements,
                Err(_) => {
                    return ShutdownTaskReceipt::timed_out("project_server_retirement_registry");
                }
            };
        let retirements = std::mem::take(&mut *retirements);
        join_shutdown_tasks_until(
            deadline,
            retirements
                .into_iter()
                .enumerate()
                .map(|(ordinal, retirement)| {
                    let retirement_abort = retirement.abort_handle();
                    (
                        format!("project_server_retirement[{ordinal}]"),
                        Some(retirement_abort),
                        async move { retirement.await.map_err(|_| ()) },
                    )
                }),
        )
        .await
    }

    pub(super) async fn shutdown_host_admission_replay(&self) {
        self.profile_host_admission_replay.shutdown().await;
    }

    pub(super) fn cancel_host_admission_replay(&self) {
        self.profile_host_admission_replay.cancel();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::{ShutdownTaskStatus, StoreAdministration};

    struct Dropped(Arc<AtomicBool>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[tokio::test]
    async fn retirement_join_reports_panicked_and_cancelled_tasks_as_failed() {
        let administration = StoreAdministration::default();
        let panicked = tokio::spawn(async {
            panic!("retirement panic");
        });
        let cancelled = tokio::spawn(std::future::pending());
        cancelled.abort();
        *administration.project_server_retirements.lock().await = vec![panicked, cancelled];

        let receipt = administration
            .join_project_server_retirements_until(
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await;

        assert_eq!(
            receipt
                .outcomes
                .iter()
                .map(|outcome| (outcome.owner.as_str(), outcome.status))
                .collect::<Vec<_>>(),
            [
                ("project_server_retirement[0]", ShutdownTaskStatus::Failed),
                ("project_server_retirement[1]", ShutdownTaskStatus::Failed),
            ]
        );
        assert!(!receipt.is_clean());
    }

    #[tokio::test(start_paused = true)]
    async fn retirement_join_timeout_aborts_and_reports_exact_owner() {
        let administration = StoreAdministration::default();
        let dropped = Arc::new(AtomicBool::new(false));
        let started = Arc::new(tokio::sync::Notify::new());
        let task_dropped = Arc::clone(&dropped);
        let task_started = Arc::clone(&started);
        let retirement = tokio::spawn(async move {
            let _dropped = Dropped(task_dropped);
            task_started.notify_one();
            std::future::pending::<()>().await;
        });
        started.notified().await;
        *administration.project_server_retirements.lock().await = vec![retirement];
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);

        let join = administration.join_project_server_retirements_until(deadline);
        tokio::pin!(join);
        tokio::time::advance(Duration::from_secs(1)).await;
        let receipt = join.await;
        tokio::task::yield_now().await;

        assert_eq!(
            receipt.outcomes,
            [super::ShutdownTaskOutcome {
                owner: "project_server_retirement[0]".to_string(),
                status: ShutdownTaskStatus::TimedOut,
            }]
        );
        assert!(dropped.load(Ordering::Acquire));
    }
}
