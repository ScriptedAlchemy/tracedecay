use std::future::Future;
use std::pin::Pin;

use tokio::time::Instant;

type ShutdownJoin = Pin<Box<dyn Future<Output = bool> + Send + 'static>>;
type ShutdownJoinFactory = Box<dyn FnOnce(Instant) -> ShutdownJoin + Send + 'static>;

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
                    true
                })
            }),
        }
    }

    pub(super) fn with_deadline_result<Cancel, JoinFactory, Join>(
        name: &'static str,
        cancel: Cancel,
        join: JoinFactory,
    ) -> Self
    where
        Cancel: FnOnce() + Send + 'static,
        JoinFactory: FnOnce(Instant) -> Join + Send + 'static,
        Join: Future<Output = bool> + Send + 'static,
    {
        Self {
            name,
            cancel: Box::new(cancel),
            join: Box::new(move |deadline| Box::pin(join(deadline))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ShutdownOwnerReceipt {
    pub(super) name: &'static str,
    pub(super) finished: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct ShutdownReceipt {
    pub(super) deadline: Instant,
    pub(super) owners: Vec<ShutdownOwnerReceipt>,
    unfinished: Vec<&'static str>,
}

impl ShutdownReceipt {
    pub(super) fn unfinished(&self) -> &[&'static str] {
        &self.unfinished
    }
}

pub(super) async fn join_shutdown_owners(
    deadline: Instant,
    owners: Vec<ShutdownOwner>,
) -> ShutdownReceipt {
    let owners = owners
        .into_iter()
        .map(|owner| {
            (owner.cancel)();
            (owner.name, owner.join)
        })
        .collect::<Vec<_>>();

    let mut joins = tokio::task::JoinSet::new();
    let mut pending = std::collections::HashMap::new();
    for (ordinal, (name, join)) in owners.into_iter().enumerate() {
        let handle = joins.spawn(async move {
            let finished = tokio::time::timeout_at(deadline, join(deadline))
                .await
                .unwrap_or(false);
            (ordinal, ShutdownOwnerReceipt { name, finished })
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
                            finished: false,
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
                finished: false,
            },
        )
    }));
    receipts.sort_by_key(|(ordinal, _)| *ordinal);
    let owners = receipts
        .into_iter()
        .map(|(_, receipt)| receipt)
        .collect::<Vec<_>>();
    let unfinished = owners
        .iter()
        .filter_map(|receipt| (!receipt.finished).then_some(receipt.name))
        .collect();
    ShutdownReceipt {
        deadline,
        owners,
        unfinished,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use tokio::time::Instant;

    use super::{ShutdownOwner, join_shutdown_owners};

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
        assert!(receipt.owners.iter().all(|owner| owner.finished));
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
        assert_eq!(receipt.deadline, deadline);
    }
}
