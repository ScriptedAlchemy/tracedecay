use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::task::Poll;
use std::time::Instant;

use tracedecay_runtime_core::logging::log_daemon_event;
use tracedecay_runtime_core::native_runtime::{self, TimeoutError};
pub use tracedecay_store_runtime::ShutdownStatus;

type ShutdownJoin = Pin<Box<dyn Future<Output = ShutdownJoined> + Send + 'static>>;
type ShutdownJoinFactory = Box<dyn FnOnce(Instant) -> ShutdownJoin + Send + 'static>;

/// What an owner's join reports. `status` says whether the owner stopped;
/// `lost_work` names best-effort work it dropped on the way out, such as an
/// observation write past its deadline. Lost work never makes shutdown
/// unclean, because the owner did stop.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShutdownJoined {
    pub status: ShutdownStatus,
    pub lost_work: Vec<String>,
}

impl From<ShutdownStatus> for ShutdownJoined {
    fn from(status: ShutdownStatus) -> Self {
        Self {
            status,
            lost_work: Vec::new(),
        }
    }
}

/// One item of best-effort work an owner lost while it stopped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShutdownLostWork {
    pub owner: &'static str,
    pub work: String,
}

pub struct ShutdownOwner {
    name: &'static str,
    cancel: Box<dyn FnOnce() + Send + 'static>,
    join: ShutdownJoinFactory,
}

impl ShutdownOwner {
    pub fn new<Cancel, Join>(name: &'static str, cancel: Cancel, join: Join) -> Self
    where
        Cancel: FnOnce() + Send + 'static,
        Join: Future<Output = ()> + Send + 'static,
    {
        Self::with_deadline(name, cancel, |_| join)
    }

    pub fn with_deadline<Cancel, JoinFactory, Join>(
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
                    ShutdownStatus::Clean.into()
                })
            }),
        }
    }

    pub fn with_deadline_result<Cancel, JoinFactory, Join, Error>(
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
                    .into()
                })
            }),
        }
    }

    /// `Join` resolves to a [`ShutdownStatus`], or to a [`ShutdownJoined`]
    /// when the owner can stop yet lose best-effort work.
    pub fn with_deadline_status<Cancel, JoinFactory, Join>(
        name: &'static str,
        cancel: Cancel,
        join: JoinFactory,
    ) -> Self
    where
        Cancel: FnOnce() + Send + 'static,
        JoinFactory: FnOnce(Instant) -> Join + Send + 'static,
        Join: Future + Send + 'static,
        Join::Output: Into<ShutdownJoined>,
    {
        Self {
            name,
            cancel: Box::new(cancel),
            join: Box::new(move |deadline| Box::pin(async move { join(deadline).await.into() })),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShutdownOwnerReceipt {
    pub name: &'static str,
    pub status: ShutdownStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShutdownReceipt {
    pub deadline: Instant,
    pub owners: Vec<ShutdownOwnerReceipt>,
    /// Best-effort work lost by owners that stopped. Not part of
    /// `unfinished`: every owner named here joined.
    pub lost_work: Vec<ShutdownLostWork>,
    unfinished: Vec<&'static str>,
}

impl ShutdownReceipt {
    pub fn extend(&mut self, other: Self) {
        self.owners.extend(other.owners);
        self.lost_work.extend(other.lost_work);
        for owner in other.unfinished {
            if !self.unfinished.contains(&owner) {
                self.unfinished.push(owner);
            }
        }
    }

    pub fn unfinished(&self) -> &[&'static str] {
        &self.unfinished
    }

    pub fn failed(deadline: Instant, name: &'static str, error: String) -> Self {
        Self {
            deadline,
            owners: vec![ShutdownOwnerReceipt {
                name,
                status: ShutdownStatus::Failed(error),
            }],
            lost_work: Vec::new(),
            unfinished: vec![name],
        }
    }

    pub fn timed_out(deadline: Instant, name: &'static str) -> Self {
        Self {
            deadline,
            owners: vec![ShutdownOwnerReceipt {
                name,
                status: ShutdownStatus::TimedOut,
            }],
            lost_work: Vec::new(),
            unfinished: vec![name],
        }
    }

    pub fn retain_failures_from(&mut self, failures: &[ShutdownOwnerReceipt]) {
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

#[cfg(any(test, feature = "test-helpers"))]
pub async fn join_shutdown_owners(
    deadline: Instant,
    owners: Vec<ShutdownOwner>,
) -> ShutdownReceipt {
    join_shutdown_owner_phases(deadline, vec![owners]).await
}

#[cfg(any(test, feature = "test-helpers"))]
pub async fn join_shutdown_owner_phases(
    deadline: Instant,
    phases: Vec<Vec<ShutdownOwner>>,
) -> ShutdownReceipt {
    prepare_shutdown_owner_phases(phases).join(deadline).await
}

/// One prepared owner: its original ordinal, name, cancellation-time panic
/// message (if cancelling it panicked), and its join factory.
type PreparedShutdownOwner = (usize, &'static str, Option<String>, ShutdownJoinFactory);

pub struct PreparedShutdownOwners {
    phases: Vec<Vec<PreparedShutdownOwner>>,
}

pub fn prepare_shutdown_owner_phases(phases: Vec<Vec<ShutdownOwner>>) -> PreparedShutdownOwners {
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
    #[tracing::instrument(name = "daemon.shutdown.owners.join", level = "trace", skip_all)]
    pub async fn join(self, deadline: Instant) -> ShutdownReceipt {
        let mut receipts = Vec::new();
        for phase in self.phases {
            receipts.extend(join_shutdown_phase(deadline, phase).await);
        }
        receipts.sort_by_key(|(ordinal, _, _)| *ordinal);
        let mut owners = Vec::with_capacity(receipts.len());
        let mut lost_work = Vec::new();
        for (_, receipt, lost) in receipts {
            lost_work.extend(lost.into_iter().map(|work| ShutdownLostWork {
                owner: receipt.name,
                work,
            }));
            owners.push(receipt);
        }
        let unfinished = owners
            .iter()
            .filter_map(|receipt| (!receipt.status.is_clean()).then_some(receipt.name))
            .collect();
        ShutdownReceipt {
            deadline,
            owners,
            lost_work,
            unfinished,
        }
    }
}

#[tracing::instrument(name = "daemon.shutdown.phase.join", level = "trace", skip_all)]
async fn join_shutdown_phase(
    deadline: Instant,
    owners: Vec<PreparedShutdownOwner>,
) -> Vec<(usize, ShutdownOwnerReceipt, Vec<String>)> {
    let mut joins = Vec::with_capacity(owners.len());
    let mut joined = Vec::with_capacity(owners.len());
    for (ordinal, name, cancellation_error, join) in owners {
        log_daemon_event(
            "daemon_shutdown",
            &[
                ("outcome", "owner_join_start".to_string()),
                ("owner", name.to_string()),
                (
                    "since_arm_ms",
                    super::watchdog::shutdown_elapsed_ms().to_string(),
                ),
            ],
        );
        joins.push(Some(join(deadline)));
        joined.push((ordinal, name, cancellation_error, Instant::now(), None));
    }
    // Owner joins still reach Tokio, so they are polled in place rather than
    // spawned; at the deadline the stragglers are dropped, not aborted and
    // joined.
    // ponytail: every pending join is polled on any wake, O(owners) per wake;
    // a phase holds about ten owners.
    let drained = native_runtime::timeout_at(
        deadline,
        std::future::poll_fn(|context| {
            for (slot, (_, name, _, started, status)) in joins.iter_mut().zip(joined.iter_mut()) {
                let Some(join) = slot else {
                    continue;
                };
                let polled = match std::panic::catch_unwind(AssertUnwindSafe(|| {
                    join.as_mut().poll(context)
                })) {
                    Ok(Poll::Pending) => continue,
                    Ok(Poll::Ready(polled)) => polled,
                    Err(panic) => ShutdownStatus::Failed(format!(
                        "shutdown owner {name} panicked: {}",
                        panic_message(panic.as_ref())
                    ))
                    .into(),
                };
                *slot = None;
                log_owner_joined(name, &polled.status, *started);
                *status = Some(polled);
            }
            if joins.iter().all(Option::is_none) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }),
    )
    .await;
    let straggler = match drained {
        Ok(()) | Err(TimeoutError::Elapsed) => ShutdownStatus::TimedOut,
        Err(error @ TimeoutError::TimerUnavailable(_)) => ShutdownStatus::Failed(error.to_string()),
    };
    drop(joins);
    joined
        .into_iter()
        .map(|(ordinal, name, cancellation_error, started, status)| {
            let ShutdownJoined {
                status: join_status,
                lost_work,
            } = status.unwrap_or_else(|| {
                log_owner_joined(name, &straggler, started);
                straggler.clone().into()
            });
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
            // The timed-out owner name is a bounded static vocabulary fixed by
            // the shutdown plan, traced so a post-deadline report names the
            // straggler without replaying the receipt.
            if let ShutdownStatus::TimedOut = &status {
                tracing::trace!(name: "daemon.shutdown.straggler.owner", value = ?name);
            }
            (ordinal, ShutdownOwnerReceipt { name, status }, lost_work)
        })
        .collect()
}

fn log_owner_joined(name: &'static str, status: &ShutdownStatus, started: Instant) {
    let status_label = match status {
        ShutdownStatus::Clean => "clean",
        ShutdownStatus::Failed(_) => "failed",
        ShutdownStatus::TimedOut => "timed_out",
    };
    log_daemon_event(
        "daemon_shutdown",
        &[
            ("outcome", "owner_joined".to_string()),
            ("owner", name.to_string()),
            ("status", status_label.to_string()),
            ("elapsed_ms", started.elapsed().as_millis().to_string()),
            (
                "since_arm_ms",
                super::watchdog::shutdown_elapsed_ms().to_string(),
            ),
        ],
    );
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::{
        ShutdownJoined, ShutdownLostWork, ShutdownOwner, ShutdownStatus,
        join_shutdown_owner_phases, join_shutdown_owners, prepare_shutdown_owner_phases,
    };

    #[tokio::test]
    async fn lost_work_is_reported_without_leaving_its_owner_unfinished() {
        let receipt = join_shutdown_owners(
            Instant::now() + Duration::from_secs(1),
            vec![
                ShutdownOwner::with_deadline_status(
                    "lossy",
                    || {},
                    |_| async {
                        ShutdownJoined {
                            status: ShutdownStatus::Clean,
                            lost_work: vec!["observation write missed its deadline".to_owned()],
                        }
                    },
                ),
                ShutdownOwner::with_deadline_status(
                    "stuck",
                    || {},
                    |_| async { ShutdownStatus::Failed("owner did not stop".to_owned()) },
                ),
            ],
        )
        .await;

        assert_eq!(receipt.unfinished(), &["stuck"]);
        assert_eq!(receipt.owners[0].status, ShutdownStatus::Clean);
        assert_eq!(
            receipt.lost_work,
            vec![ShutdownLostWork {
                owner: "lossy",
                work: "observation write missed its deadline".to_owned(),
            }]
        );
    }

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

    #[tokio::test]
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

    #[tokio::test]
    async fn every_owner_uses_the_same_deadline_and_unfinished_receipts_are_exact() {
        struct Dropped(Arc<AtomicBool>);

        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let owner_deadlines = Arc::new(std::sync::Mutex::new(Vec::new()));
        let straggler_dropped = Arc::new(AtomicBool::new(false));
        let deadline = Instant::now() + Duration::from_millis(100);
        let mut owners = Vec::new();
        for (name, finishes) in [("quick", true), ("blocked-a", false), ("blocked-b", false)] {
            let observed = Arc::clone(&owner_deadlines);
            let dropped = (!finishes).then(|| Dropped(Arc::clone(&straggler_dropped)));
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
                    let _dropped = dropped;
                    std::future::pending::<()>().await;
                },
            ));
        }

        let receipt = tokio::time::timeout(
            Duration::from_secs(10),
            join_shutdown_owners(deadline, owners),
        )
        .await
        .expect("the shared deadline bounds the owner joins");

        assert!(
            Instant::now() >= deadline,
            "blocked owners are cut off at the deadline, not before it"
        );
        assert!(
            straggler_dropped.load(Ordering::Acquire),
            "a timed-out owner join is dropped once its receipt is written"
        );
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
        assert_eq!(
            panic_error,
            "shutdown owner panicked_join panicked: join panic"
        );
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
