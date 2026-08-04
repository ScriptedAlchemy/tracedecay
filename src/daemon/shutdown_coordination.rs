use std::future::Future;
use std::pin::Pin;

use tokio::time::Instant;

type ShutdownJoin = Pin<Box<dyn Future<Output = ShutdownStatus> + Send + 'static>>;
type ShutdownJoinFactory = Box<dyn FnOnce(Instant) -> ShutdownJoin + Send + 'static>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ShutdownStatus {
    Clean,
    Failed(String),
    TimedOut,
}

impl ShutdownStatus {
    pub(crate) fn is_clean(&self) -> bool {
        matches!(self, Self::Clean)
    }
}

pub(super) struct ShutdownOwner {
    name: &'static str,
    cancel: Box<dyn FnOnce() + Send + 'static>,
    join: ShutdownJoinFactory,
}

impl ShutdownOwner {
    pub(super) fn new<Cancel, Join>(name: &'static str, cancel: Cancel, join: Join) -> Self
    where
        Cancel: FnOnce() + Send + 'static,
        Join: Future<Output = ()> + Send + 'static,
    {
        Self::with_deadline(name, cancel, |_| join)
    }

    pub(super) fn with_deadline<Cancel, JoinFactory, Join>(
        name: &'static str,
        cancel: Cancel,
        join: JoinFactory,
    ) -> Self
    where
        Cancel: FnOnce() + Send + 'static,
        JoinFactory: FnOnce(Instant) -> Join + Send + 'static,
        Join: Future<Output = ()> + Send + 'static,
    {
        Self {
            name,
            cancel: Box::new(cancel),
            join: Box::new(move |deadline| {
                Box::pin(async move {
                    join(deadline).await;
                    ShutdownStatus::Clean
                })
            }),
        }
    }

    pub(super) fn with_deadline_result<Cancel, JoinFactory, Join, Error>(
        name: &'static str,
        cancel: Cancel,
        join: JoinFactory,
    ) -> Self
    where
        Cancel: FnOnce() + Send + 'static,
        JoinFactory: FnOnce(Instant) -> Join + Send + 'static,
        Join: Future<Output = std::result::Result<(), Error>> + Send + 'static,
        Error: std::fmt::Display + Send + 'static,
    {
        Self {
            name,
            cancel: Box::new(cancel),
            join: Box::new(move |deadline| {
                Box::pin(async move {
                    match join(deadline).await {
                        Ok(()) => ShutdownStatus::Clean,
                        Err(error) => ShutdownStatus::Failed(error.to_string()),
                    }
                })
            }),
        }
    }

    pub(super) fn with_deadline_status<Cancel, JoinFactory, Join>(
        name: &'static str,
        cancel: Cancel,
        join: JoinFactory,
    ) -> Self
    where
        Cancel: FnOnce() + Send + 'static,
        JoinFactory: FnOnce(Instant) -> Join + Send + 'static,
        Join: Future<Output = ShutdownStatus> + Send + 'static,
    {
        Self {
            name,
            cancel: Box::new(cancel),
            join: Box::new(move |deadline| Box::pin(join(deadline))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ShutdownOwnerReceipt {
    pub(super) name: &'static str,
    pub(super) status: ShutdownStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ShutdownReceipt {
    pub(super) deadline: Instant,
    pub(super) owners: Vec<ShutdownOwnerReceipt>,
    unfinished: Vec<&'static str>,
}

impl ShutdownReceipt {
    pub(super) fn unfinished(&self) -> &[&'static str] {
        &self.unfinished
    }

    pub(super) fn failed(deadline: Instant, name: &'static str, error: String) -> Self {
        Self {
            deadline,
            owners: vec![ShutdownOwnerReceipt {
                name,
                status: ShutdownStatus::Failed(error),
            }],
            unfinished: vec![name],
        }
    }

    pub(super) fn timed_out(deadline: Instant, name: &'static str) -> Self {
        Self {
            deadline,
            owners: vec![ShutdownOwnerReceipt {
                name,
                status: ShutdownStatus::TimedOut,
            }],
            unfinished: vec![name],
        }
    }

    pub(super) fn retain_failures_from(&mut self, failures: &[ShutdownOwnerReceipt]) {
        for failure in failures {
            let ShutdownStatus::Failed(prior_error) = &failure.status else {
                continue;
            };
            match self
                .owners
                .iter()
                .position(|owner| owner.name == failure.name)
            {
                Some(index) => match self.owners[index].status.clone() {
                    ShutdownStatus::Clean => self.owners[index].status = failure.status.clone(),
                    ShutdownStatus::Failed(error) if error != *prior_error => {
                        self.owners[index].status =
                            ShutdownStatus::Failed(format!("{prior_error}; retry failed: {error}"));
                    }
                    ShutdownStatus::Failed(_) => {}
                    ShutdownStatus::TimedOut => self.owners.insert(index, failure.clone()),
                },
                None => self.owners.push(failure.clone()),
            }
        }
        self.unfinished.clear();
        for owner in &self.owners {
            if !owner.status.is_clean() && !self.unfinished.contains(&owner.name) {
                self.unfinished.push(owner.name);
            }
        }
    }
}

#[cfg(test)]
pub(super) async fn join_shutdown_owners(
    deadline: Instant,
    owners: Vec<ShutdownOwner>,
) -> ShutdownReceipt {
    join_shutdown_owner_phases(deadline, vec![owners]).await
}

#[cfg(test)]
pub(super) async fn join_shutdown_owner_phases(
    deadline: Instant,
    phases: Vec<Vec<ShutdownOwner>>,
) -> ShutdownReceipt {
    prepare_shutdown_owner_phases(phases).join(deadline).await
}

pub(super) struct PreparedShutdownOwners {
    phases: Vec<Vec<(usize, &'static str, Option<String>, ShutdownJoinFactory)>>,
}

pub(super) fn prepare_shutdown_owner_phases(
    phases: Vec<Vec<ShutdownOwner>>,
) -> PreparedShutdownOwners {
    let mut ordinal = 0;
    let phases = phases
        .into_iter()
        .map(|phase| {
            phase
                .into_iter()
                .map(|owner| {
                    let cancellation_error =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(owner.cancel))
                            .err()
                            .map(|_| "shutdown cancellation panicked".to_string());
                    let prepared = (ordinal, owner.name, cancellation_error, owner.join);
                    ordinal += 1;
                    prepared
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    PreparedShutdownOwners { phases }
}

impl PreparedShutdownOwners {
    pub(super) async fn join(self, deadline: Instant) -> ShutdownReceipt {
        let mut receipts = Vec::new();
        for phase in self.phases {
            receipts.extend(join_shutdown_phase(deadline, phase).await);
        }
        receipts.sort_by_key(|(ordinal, _)| *ordinal);
        let owners = receipts
            .into_iter()
            .map(|(_, receipt)| receipt)
            .collect::<Vec<_>>();
        let unfinished = owners
            .iter()
            .filter_map(|receipt| (!receipt.status.is_clean()).then_some(receipt.name))
            .collect();
        ShutdownReceipt {
            deadline,
            owners,
            unfinished,
        }
    }
}

async fn join_shutdown_phase(
    deadline: Instant,
    owners: Vec<(usize, &'static str, Option<String>, ShutdownJoinFactory)>,
) -> Vec<(usize, ShutdownOwnerReceipt)> {
    let mut joins = tokio::task::JoinSet::new();
    let mut pending = std::collections::HashMap::new();
    for (ordinal, name, cancellation_error, join) in owners {
        let handle = joins.spawn(async move {
            let join_status = match tokio::time::timeout_at(deadline, join(deadline)).await {
                Ok(status) => status,
                Err(_) => ShutdownStatus::TimedOut,
            };
            let status = match (cancellation_error, join_status) {
                (None, status) => status,
                (Some(error), ShutdownStatus::Clean) => ShutdownStatus::Failed(error),
                (Some(error), ShutdownStatus::Failed(join_error)) => {
                    ShutdownStatus::Failed(format!("{error}; join failed: {join_error}"))
                }
                (Some(error), ShutdownStatus::TimedOut) => {
                    ShutdownStatus::Failed(format!("{error}; join timed out"))
                }
            };
            (ordinal, ShutdownOwnerReceipt { name, status })
        });
        pending.insert(handle.id(), (ordinal, name));
    }
    let mut receipts = Vec::new();
    while let Some(joined) = joins.join_next_with_id().await {
        match joined {
            Ok((id, receipt)) => {
                pending.remove(&id);
                receipts.push(receipt);
            }
            Err(error) => {
                let id = error.id();
                if let Some((ordinal, name)) = pending.remove(&id) {
                    receipts.push((
                        ordinal,
                        ShutdownOwnerReceipt {
                            name,
                            status: ShutdownStatus::Failed(error.to_string()),
                        },
                    ));
                }
                tracing::error!(task_id = ?id, "daemon shutdown owner join task failed");
            }
        }
    }
    receipts.extend(pending.into_values().map(|(ordinal, name)| {
        (
            ordinal,
            ShutdownOwnerReceipt {
                name,
                status: ShutdownStatus::Failed(
                    "shutdown join task ended without a receipt".to_string(),
                ),
            },
        )
    }));
    receipts
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use tokio::time::Instant;

    use super::{
        ShutdownOwner, ShutdownStatus, join_shutdown_owner_phases, join_shutdown_owners,
        prepare_shutdown_owner_phases,
    };

    #[test]
    fn preparing_owner_phases_cancels_every_owner_synchronously() {
        let cancelled = Arc::new(AtomicUsize::new(0));
        let owners = (0..3)
            .map(|_| {
                let cancelled = Arc::clone(&cancelled);
                ShutdownOwner::new(
                    "owner",
                    move || {
                        cancelled.fetch_add(1, Ordering::AcqRel);
                    },
                    async {},
                )
            })
            .collect();

        let _prepared = prepare_shutdown_owner_phases(vec![owners]);

        assert_eq!(cancelled.load(Ordering::Acquire), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_reaches_every_owner_before_any_join_is_polled() {
        let cancelled = Arc::new(AtomicUsize::new(0));
        let first_join_polled = Arc::new(AtomicBool::new(false));
        let mut owners = Vec::new();
        for name in ["first", "second", "third"] {
            let cancel_count = Arc::clone(&cancelled);
            let observed_cancel_count = Arc::clone(&cancelled);
            let join_polled = Arc::clone(&first_join_polled);
            owners.push(ShutdownOwner::new(
                name,
                move || {
                    cancel_count.fetch_add(1, Ordering::AcqRel);
                },
                async move {
                    join_polled.store(true, Ordering::Release);
                    assert_eq!(
                        observed_cancel_count.load(Ordering::Acquire),
                        3,
                        "every owner must receive cancellation before the first join is polled"
                    );
                },
            ));
        }

        let receipt = join_shutdown_owners(Instant::now() + Duration::from_secs(1), owners).await;

        assert!(first_join_polled.load(Ordering::Acquire));
        assert!(
            receipt
                .owners
                .iter()
                .all(|owner| owner.status == ShutdownStatus::Clean)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn every_owner_uses_the_same_deadline_and_unfinished_receipts_are_exact() {
        let owner_deadlines = Arc::new(std::sync::Mutex::new(Vec::new()));
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut owners = Vec::new();
        for (name, finishes) in [("quick", true), ("blocked-a", false), ("blocked-b", false)] {
            let observed = Arc::clone(&owner_deadlines);
            owners.push(ShutdownOwner::with_deadline(
                name,
                || {},
                move |owner_deadline| async move {
                    observed
                        .lock()
                        .expect("deadline observations")
                        .push(owner_deadline);
                    if finishes {
                        return;
                    }
                    std::future::pending::<()>().await;
                },
            ));
        }

        let shutdown = join_shutdown_owners(deadline, owners);
        tokio::pin!(shutdown);
        tokio::time::advance(Duration::from_secs(2)).await;
        let receipt = shutdown.await;

        assert_eq!(
            owner_deadlines
                .lock()
                .expect("deadline observations")
                .as_slice(),
            &[deadline, deadline, deadline]
        );
        assert_eq!(receipt.unfinished(), &["blocked-a", "blocked-b"]);
        assert_eq!(receipt.owners[0].status, ShutdownStatus::Clean);
        assert_eq!(receipt.owners[1].status, ShutdownStatus::TimedOut);
        assert_eq!(receipt.owners[2].status, ShutdownStatus::TimedOut);
        assert_eq!(receipt.deadline, deadline);
    }

    #[tokio::test]
    async fn cancellation_panic_does_not_block_other_cancellations_or_join() {
        let second_cancelled = Arc::new(AtomicBool::new(false));
        let second_cancelled_by_owner = Arc::clone(&second_cancelled);

        let receipt = join_shutdown_owners(
            Instant::now() + Duration::from_secs(1),
            vec![
                ShutdownOwner::new("panicked_cancel", || panic!("cancel panic"), async {}),
                ShutdownOwner::new(
                    "second",
                    move || second_cancelled_by_owner.store(true, Ordering::Release),
                    async {},
                ),
            ],
        )
        .await;

        assert!(second_cancelled.load(Ordering::Acquire));
        assert_eq!(
            receipt.owners[0].status,
            ShutdownStatus::Failed("shutdown cancellation panicked".to_string())
        );
        assert_eq!(receipt.owners[1].status, ShutdownStatus::Clean);
    }

    #[tokio::test]
    async fn join_panics_and_typed_errors_are_preserved_as_failures() {
        let receipt = join_shutdown_owners(
            Instant::now() + Duration::from_secs(1),
            vec![
                ShutdownOwner::new("panicked_join", || {}, async {
                    panic!("join panic");
                }),
                ShutdownOwner::with_deadline_result(
                    "failed_join",
                    || {},
                    |_| async { Err::<(), _>("typed join failure") },
                ),
            ],
        )
        .await;

        let ShutdownStatus::Failed(panic_error) = &receipt.owners[0].status else {
            panic!("panicked join must be a typed failure");
        };
        assert!(panic_error.contains("join panic"), "{panic_error}");
        assert_eq!(
            receipt.owners[1].status,
            ShutdownStatus::Failed("typed join failure".to_string())
        );
    }

    #[tokio::test]
    async fn later_dependency_phases_join_only_after_earlier_phases_finish() {
        let first_finished = Arc::new(AtomicBool::new(false));
        let later_cancelled = Arc::new(AtomicBool::new(false));
        let first_finished_in_join = Arc::clone(&first_finished);
        let first_finished_before_later_join = Arc::clone(&first_finished);
        let later_cancelled_in_cancel = Arc::clone(&later_cancelled);
        let later_cancelled_before_first_join = Arc::clone(&later_cancelled);

        let receipt = join_shutdown_owner_phases(
            Instant::now() + Duration::from_secs(1),
            vec![
                vec![ShutdownOwner::new("producer", || {}, async move {
                    assert!(
                        later_cancelled_before_first_join.load(Ordering::Acquire),
                        "every phase must be cancelled before the first join"
                    );
                    first_finished_in_join.store(true, Ordering::Release);
                })],
                vec![ShutdownOwner::new(
                    "authority",
                    move || later_cancelled_in_cancel.store(true, Ordering::Release),
                    async move {
                        assert!(
                            first_finished_before_later_join.load(Ordering::Acquire),
                            "authority join must wait for producer joins"
                        );
                    },
                )],
            ],
        )
        .await;

        assert!(
            receipt
                .owners
                .iter()
                .all(|owner| owner.status == ShutdownStatus::Clean)
        );
    }
}
