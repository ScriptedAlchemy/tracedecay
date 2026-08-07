//! One transport-neutral daemon shutdown sequence.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::task::JoinSet;

use super::shutdown_coordination::{
    ShutdownOwner, ShutdownOwnerReceipt, ShutdownReceipt, ShutdownStatus,
    prepare_shutdown_owner_phases,
};
use super::store_shutdown::{ShutdownTaskOutcome, ShutdownTaskReceipt};
use super::{
    DAEMON_CLIENT_DRAIN_DEADLINE, DAEMON_TASK_ABORT_DEADLINE, DaemonLifecycle,
    core_lifecycle::DaemonShutdownClaim,
};
use crate::errors::Result;

type ProjectServerShutdown = Pin<Box<dyn Future<Output = ShutdownTaskReceipt> + Send + 'static>>;

pub(super) struct DaemonShutdownPlan {
    clients: JoinSet<Result<()>>,
    owner_phases: Vec<Vec<ShutdownOwner>>,
    project_server_shutdown: ProjectServerShutdown,
}

impl DaemonShutdownPlan {
    pub(super) fn new<ProjectServers>(
        clients: JoinSet<Result<()>>,
        owner_phases: Vec<Vec<ShutdownOwner>>,
        project_server_shutdown: ProjectServers,
    ) -> Self
    where
        ProjectServers: Future<Output = ShutdownTaskReceipt> + Send + 'static,
    {
        Self {
            clients,
            owner_phases,
            project_server_shutdown: Box::pin(project_server_shutdown),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DaemonShutdownReceipt {
    pub(super) in_flight: ShutdownStatus,
    pub(super) clients: ShutdownStatus,
    pub(super) background: ShutdownReceipt,
    pub(super) project_servers: ShutdownTaskReceipt,
}

impl DaemonShutdownReceipt {
    fn coordinator_failed(deadline: tokio::time::Instant, error: String) -> Self {
        Self {
            in_flight: ShutdownStatus::Failed(error.clone()),
            clients: ShutdownStatus::Failed(error.clone()),
            background: ShutdownReceipt::failed(deadline, "shutdown_coordinator", error.clone()),
            project_servers: ShutdownTaskReceipt::failed("shutdown_coordinator", error),
        }
    }

    fn preparation_timed_out(deadline: tokio::time::Instant) -> Self {
        Self {
            in_flight: ShutdownStatus::TimedOut,
            clients: ShutdownStatus::TimedOut,
            background: ShutdownReceipt::timed_out(deadline, "shutdown_prepare"),
            project_servers: ShutdownTaskReceipt::timed_out("project_server_shutdown"),
        }
    }

    pub(super) fn is_retryable(&self) -> bool {
        matches!(self.in_flight, ShutdownStatus::TimedOut)
            || matches!(self.clients, ShutdownStatus::TimedOut)
            || self
                .background
                .owners
                .iter()
                .any(|owner| owner.status == ShutdownStatus::TimedOut)
            || self.project_servers.timed_out_count() > 0
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct DaemonShutdownFailures {
    in_flight: Vec<String>,
    clients: Vec<String>,
    background: Vec<ShutdownOwnerReceipt>,
    project_servers: Vec<ShutdownTaskOutcome>,
}

impl DaemonShutdownFailures {
    fn record(&mut self, receipt: &DaemonShutdownReceipt) {
        record_status_failure(&mut self.in_flight, &receipt.in_flight);
        record_status_failure(&mut self.clients, &receipt.clients);
        for owner in &receipt.background.owners {
            if matches!(owner.status, ShutdownStatus::Failed(_)) && !self.background.contains(owner)
            {
                self.background.push(owner.clone());
            }
        }
        for outcome in &receipt.project_servers.outcomes {
            if matches!(outcome.status, ShutdownStatus::Failed(_))
                && !self.project_servers.contains(outcome)
            {
                self.project_servers.push(outcome.clone());
            }
        }
    }

    fn apply(&self, receipt: &mut DaemonShutdownReceipt) {
        retain_status_failures(&mut receipt.in_flight, &self.in_flight);
        retain_status_failures(&mut receipt.clients, &self.clients);
        receipt.background.retain_failures_from(&self.background);
        receipt
            .project_servers
            .retain_failures_from(&self.project_servers);
    }
}

fn record_status_failure(failures: &mut Vec<String>, status: &ShutdownStatus) {
    if let ShutdownStatus::Failed(error) = status
        && !failures.contains(error)
    {
        failures.push(error.clone());
    }
}

fn retain_status_failures(status: &mut ShutdownStatus, failures: &[String]) {
    if matches!(status, ShutdownStatus::TimedOut) || failures.is_empty() {
        return;
    }
    let mut errors = failures.to_vec();
    if let ShutdownStatus::Failed(error) = status
        && !errors.contains(error)
    {
        errors.push(error.clone());
    }
    *status = ShutdownStatus::Failed(errors.join("; retry failed: "));
}

pub(super) async fn coordinate_daemon_shutdown<Prepare>(
    lifecycle: &DaemonLifecycle,
    shutdown_deadline: tokio::time::Instant,
    prepare: Prepare,
) -> Arc<DaemonShutdownReceipt>
where
    Prepare: Future<Output = DaemonShutdownPlan> + Send + 'static,
{
    lifecycle.wait_for_finished_shutdown_coordinator().await;
    lifecycle.join_finished_shutdown_coordinator().await;
    lifecycle.begin_draining();
    let attempt = match lifecycle.claim_shutdown_coordination() {
        DaemonShutdownClaim::Terminal(receipt) => {
            drop(prepare);
            return receipt;
        }
        DaemonShutdownClaim::Wait(attempt) => {
            drop(prepare);
            attempt
        }
        DaemonShutdownClaim::Run { attempt, failures } => {
            let coordinator_lifecycle = lifecycle.clone();
            let runner_lifecycle = lifecycle.clone();
            let coordinator_attempt = Arc::clone(&attempt);
            let mut coordinator_failures = failures.clone();
            let coordinator = async move {
                let runner = tokio::spawn(async move {
                    match tokio::time::timeout_at(shutdown_deadline, prepare).await {
                        Ok(plan) => {
                            run_daemon_shutdown(runner_lifecycle, plan, shutdown_deadline).await
                        }
                        Err(_) => DaemonShutdownReceipt::preparation_timed_out(shutdown_deadline),
                    }
                });
                let mut receipt = match runner.await {
                    Ok(receipt) => receipt,
                    Err(error) => DaemonShutdownReceipt::coordinator_failed(
                        shutdown_deadline,
                        error.to_string(),
                    ),
                };
                coordinator_failures.record(&receipt);
                coordinator_failures.apply(&mut receipt);
                coordinator_lifecycle.finish_shutdown_attempt(
                    &coordinator_attempt,
                    Arc::new(receipt),
                    coordinator_failures,
                );
            };
            if !lifecycle.spawn_shutdown_coordinator(&attempt, coordinator) {
                let receipt = Arc::new(DaemonShutdownReceipt::coordinator_failed(
                    shutdown_deadline,
                    "daemon shutdown coordinator ownership was lost".to_owned(),
                ));
                let mut failures = failures;
                failures.record(&receipt);
                lifecycle.finish_shutdown_attempt(&attempt, receipt, failures);
            }
            attempt
        }
    };

    let receipt = match attempt.wait_for_receipt().await {
        Ok(receipt) => receipt,
        Err(error) => Arc::new(DaemonShutdownReceipt::coordinator_failed(
            shutdown_deadline,
            error,
        )),
    };
    lifecycle.wait_for_finished_shutdown_coordinator().await;
    lifecycle.join_finished_shutdown_coordinator().await;
    receipt
}

async fn run_daemon_shutdown(
    lifecycle: DaemonLifecycle,
    mut plan: DaemonShutdownPlan,
    shutdown_deadline: tokio::time::Instant,
) -> DaemonShutdownReceipt {
    let prepared = prepare_shutdown_owner_phases(plan.owner_phases);
    let mut background_shutdown = Box::pin(prepared.join(shutdown_deadline));
    let mut background_receipt = None;
    let client_drain_deadline = std::cmp::min(
        tokio::time::Instant::now() + DAEMON_CLIENT_DRAIN_DEADLINE,
        shutdown_deadline,
    );
    let in_flight = tokio::time::timeout_at(client_drain_deadline, lifecycle.wait_for_idle());
    tokio::pin!(in_flight);
    let in_flight = loop {
        tokio::select! {
            receipt = &mut background_shutdown, if background_receipt.is_none() => {
                background_receipt = Some(receipt);
            }
            drained = &mut in_flight => {
                break match drained {
                    Ok(()) => ShutdownStatus::Clean,
                    Err(_) => ShutdownStatus::TimedOut,
                };
            }
        }
    };

    plan.clients.abort_all();
    let client_join_deadline = std::cmp::min(
        tokio::time::Instant::now() + DAEMON_TASK_ABORT_DEADLINE,
        shutdown_deadline,
    );
    let clients = join_aborted_clients_until(&mut plan.clients, client_join_deadline).await;
    let clients = if tokio::time::timeout_at(client_join_deadline, lifecycle.wait_for_idle())
        .await
        .is_err()
    {
        ShutdownStatus::TimedOut
    } else {
        clients
    };
    let background = match background_receipt {
        Some(receipt) => receipt,
        None => background_shutdown.await,
    };
    let project_servers = tokio::select! {
        biased;
        receipt = &mut plan.project_server_shutdown => receipt,
        () = tokio::time::sleep_until(shutdown_deadline) => {
            ShutdownTaskReceipt::timed_out("project_server_shutdown")
        }
    };
    DaemonShutdownReceipt {
        in_flight,
        clients,
        background,
        project_servers,
    }
}

async fn join_aborted_clients_until(
    clients: &mut JoinSet<Result<()>>,
    deadline: tokio::time::Instant,
) -> ShutdownStatus {
    match tokio::time::timeout_at(deadline, async {
        let mut failures = Vec::new();
        while let Some(completed) = clients.join_next().await {
            match completed {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failures.push(error.to_string()),
                Err(error) if error.is_cancelled() => {}
                Err(error) => failures.push(error.to_string()),
            }
        }
        failures
    })
    .await
    {
        Err(_) => ShutdownStatus::TimedOut,
        Ok(failures) if failures.is_empty() => ShutdownStatus::Clean,
        Ok(failures) => ShutdownStatus::Failed(failures.join("; ")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::errors::TraceDecayError;

    #[tokio::test]
    async fn cancelled_first_waiter_does_not_duplicate_shutdown_ownership() {
        let lifecycle = DaemonLifecycle::default();
        let cancellations = Arc::new(AtomicUsize::new(0));
        let server_shutdowns = Arc::new(AtomicUsize::new(0));
        let duplicate_prepares = Arc::new(AtomicUsize::new(0));
        let owner_cancelled = Arc::new(tokio::sync::Notify::new());
        let release_owner = Arc::new(tokio::sync::Notify::new());
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);

        let first_lifecycle = lifecycle.clone();
        let first_cancellations = Arc::clone(&cancellations);
        let first_server_shutdowns = Arc::clone(&server_shutdowns);
        let cancelled_signal = Arc::clone(&owner_cancelled);
        let owner_release = Arc::clone(&release_owner);
        let first = tokio::spawn(async move {
            coordinate_daemon_shutdown(&first_lifecycle, deadline, async move {
                DaemonShutdownPlan::new(
                    JoinSet::new(),
                    vec![vec![ShutdownOwner::new(
                        "owner",
                        move || {
                            first_cancellations.fetch_add(1, Ordering::AcqRel);
                            cancelled_signal.notify_one();
                        },
                        async move { owner_release.notified().await },
                    )]],
                    async move {
                        first_server_shutdowns.fetch_add(1, Ordering::AcqRel);
                        ShutdownTaskReceipt::default()
                    },
                )
            })
            .await
        });
        owner_cancelled.notified().await;
        first.abort();
        assert!(
            first
                .await
                .expect_err("first waiter cancelled")
                .is_cancelled()
        );

        let second_lifecycle = lifecycle.clone();
        let second_duplicate_prepares = Arc::clone(&duplicate_prepares);
        let second = tokio::spawn(async move {
            coordinate_daemon_shutdown(&second_lifecycle, deadline, async move {
                second_duplicate_prepares.fetch_add(1, Ordering::AcqRel);
                panic!("duplicate shutdown prepare future was polled");
            })
            .await
        });
        release_owner.notify_one();
        let receipt = second.await.expect("second shutdown waiter");
        let duplicate_prepares_after_terminal = Arc::clone(&duplicate_prepares);
        let subsequent = coordinate_daemon_shutdown(&lifecycle, deadline, async move {
            duplicate_prepares_after_terminal.fetch_add(1, Ordering::AcqRel);
            panic!("subsequent shutdown prepare future was polled");
        })
        .await;

        assert!(Arc::ptr_eq(&receipt, &subsequent));
        assert_eq!(cancellations.load(Ordering::Acquire), 1);
        assert_eq!(server_shutdowns.load(Ordering::Acquire), 1);
        assert_eq!(duplicate_prepares.load(Ordering::Acquire), 0);
        assert_eq!(receipt.in_flight, ShutdownStatus::Clean);
        assert_eq!(receipt.clients, ShutdownStatus::Clean);
        assert!(receipt.background.unfinished().is_empty());
        assert!(receipt.project_servers.is_clean());
    }

    #[tokio::test]
    async fn client_error_is_preserved_in_terminal_receipt() {
        let lifecycle = DaemonLifecycle::default();
        let mut clients = JoinSet::new();
        clients.spawn(async {
            Err(TraceDecayError::Config {
                message: "client failed during shutdown".to_owned(),
            })
        });
        tokio::task::yield_now().await;
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);

        let receipt = coordinate_daemon_shutdown(&lifecycle, deadline, async move {
            DaemonShutdownPlan::new(clients, Vec::new(), async {
                ShutdownTaskReceipt::default()
            })
        })
        .await;

        assert_eq!(
            receipt.clients,
            ShutdownStatus::Failed("config error: client failed during shutdown".to_owned())
        );
        assert!(receipt.project_servers.is_clean());
    }

    #[tokio::test]
    async fn coordinator_panic_becomes_shared_terminal_failure() {
        let lifecycle = DaemonLifecycle::default();
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        let receipt = coordinate_daemon_shutdown(&lifecycle, deadline, async {
            DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), async {
                panic!("server shutdown panic");
            })
        })
        .await;
        let subsequent = coordinate_daemon_shutdown(&lifecycle, deadline, async {
            panic!("duplicate prepare");
        })
        .await;

        assert!(Arc::ptr_eq(&receipt, &subsequent));
        for status in [&receipt.in_flight, &receipt.clients] {
            assert!(
                matches!(status, ShutdownStatus::Failed(error) if error.contains("server shutdown panic"))
            );
        }
        assert!(!receipt.background.unfinished().is_empty());
        assert!(!receipt.project_servers.is_clean());
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_shutdown_receipt_allows_one_non_overlapping_retry() {
        let lifecycle = DaemonLifecycle::default();
        let attempts = Arc::new(AtomicUsize::new(0));
        let cancellations = Arc::new(AtomicUsize::new(0));
        let first_attempts = Arc::clone(&attempts);
        let first_cancellations = Arc::clone(&cancellations);
        let first_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(1);
        let first = tokio::spawn({
            let lifecycle = lifecycle.clone();
            async move {
                coordinate_daemon_shutdown(&lifecycle, first_deadline, async move {
                    first_attempts.fetch_add(1, Ordering::AcqRel);
                    DaemonShutdownPlan::new(
                        JoinSet::new(),
                        vec![vec![ShutdownOwner::new(
                            "uncooperative_owner",
                            move || {
                                first_cancellations.fetch_add(1, Ordering::AcqRel);
                            },
                            std::future::pending(),
                        )]],
                        async { ShutdownTaskReceipt::default() },
                    )
                })
                .await
            }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(tokio::time::Duration::from_secs(1)).await;
        let first = first.await.expect("timed-out shutdown attempt");
        assert_eq!(first.background.owners[0].status, ShutdownStatus::TimedOut);

        let retry_attempts = Arc::clone(&attempts);
        let retry_cancellations = Arc::clone(&cancellations);
        let retry_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(1);
        let retry = coordinate_daemon_shutdown(&lifecycle, retry_deadline, async move {
            retry_attempts.fetch_add(1, Ordering::AcqRel);
            DaemonShutdownPlan::new(
                JoinSet::new(),
                vec![vec![ShutdownOwner::new(
                    "cooperative_owner",
                    move || {
                        retry_cancellations.fetch_add(1, Ordering::AcqRel);
                    },
                    async {},
                )]],
                async { ShutdownTaskReceipt::default() },
            )
        })
        .await;
        let duplicate_attempts = Arc::clone(&attempts);
        let duplicate = coordinate_daemon_shutdown(&lifecycle, retry_deadline, async move {
            duplicate_attempts.fetch_add(1, Ordering::AcqRel);
            panic!("terminal receipt must not prepare a duplicate shutdown");
        })
        .await;

        assert!(!Arc::ptr_eq(&first, &retry));
        assert!(Arc::ptr_eq(&retry, &duplicate));
        assert_eq!(attempts.load(Ordering::Acquire), 2);
        assert_eq!(cancellations.load(Ordering::Acquire), 2);
        assert!(retry.background.unfinished().is_empty());
        assert!(retry.project_servers.is_clean());
    }

    #[tokio::test(start_paused = true)]
    async fn retry_preserves_a_typed_failure_from_an_earlier_timed_out_attempt() {
        let lifecycle = DaemonLifecycle::default();
        let first_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(1);
        let first = tokio::spawn({
            let lifecycle = lifecycle.clone();
            async move {
                coordinate_daemon_shutdown(&lifecycle, first_deadline, async {
                    DaemonShutdownPlan::new(
                        JoinSet::new(),
                        vec![vec![
                            ShutdownOwner::with_deadline_result(
                                "failed_owner",
                                || {},
                                |_| async { Err::<(), _>("typed shutdown failure") },
                            ),
                            ShutdownOwner::new("timed_out_owner", || {}, std::future::pending()),
                        ]],
                        async { ShutdownTaskReceipt::default() },
                    )
                })
                .await
            }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(tokio::time::Duration::from_secs(1)).await;
        let first = first.await.expect("first shutdown attempt");

        assert!(first.is_retryable());
        assert!(matches!(
            first.background.owners.as_slice(),
            [failed, timed_out]
                if failed.name == "failed_owner"
                    && failed.status
                        == ShutdownStatus::Failed("typed shutdown failure".to_owned())
                    && timed_out.name == "timed_out_owner"
                    && timed_out.status == ShutdownStatus::TimedOut
        ));

        let retry = coordinate_daemon_shutdown(
            &lifecycle,
            tokio::time::Instant::now() + tokio::time::Duration::from_secs(1),
            async {
                DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), async {
                    ShutdownTaskReceipt::default()
                })
            },
        )
        .await;

        assert!(!retry.is_retryable());
        assert!(matches!(
            retry.background.owners.as_slice(),
            [failed]
                if failed.name == "failed_owner"
                    && failed.status
                        == ShutdownStatus::Failed("typed shutdown failure".to_owned())
        ));
        assert_eq!(retry.background.unfinished(), &["failed_owner"]);
    }

    #[tokio::test]
    async fn mixed_project_server_failure_and_timeout_retries_without_losing_failure() {
        let lifecycle = DaemonLifecycle::default();
        let mut first_project_servers =
            ShutdownTaskReceipt::failed("failed_server", "typed server failure");
        first_project_servers.extend(ShutdownTaskReceipt::timed_out("timed_out_server"));
        let first =
            coordinate_daemon_shutdown(
                &lifecycle,
                tokio::time::Instant::now() + tokio::time::Duration::from_secs(1),
                async move {
                    DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), async move {
                        first_project_servers
                    })
                },
            )
            .await;

        assert!(first.is_retryable());
        let retry = coordinate_daemon_shutdown(
            &lifecycle,
            tokio::time::Instant::now() + tokio::time::Duration::from_secs(1),
            async {
                DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), async {
                    ShutdownTaskReceipt::default()
                })
            },
        )
        .await;

        assert!(!retry.is_retryable());
        assert_eq!(
            retry.project_servers.status(),
            ShutdownStatus::Failed("failed_server: typed server failure".to_owned())
        );
        assert!(matches!(
            retry.project_servers.outcomes.as_slice(),
            [failed]
                if failed.owner == "failed_server"
                    && failed.status
                        == ShutdownStatus::Failed("typed server failure".to_owned())
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn contended_prepare_lock_times_out_with_retryable_receipt() {
        struct Dropped(Arc<std::sync::atomic::AtomicBool>);

        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let lifecycle = DaemonLifecycle::default();
        let prepare_lock = Arc::new(tokio::sync::Mutex::new(()));
        let held_lock = prepare_lock.lock().await;
        let prepare_started = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(1);

        let first_lifecycle = lifecycle.clone();
        let first_lock = Arc::clone(&prepare_lock);
        let first_started = Arc::clone(&prepare_started);
        let first_dropped = Arc::clone(&dropped);
        let first = tokio::spawn(async move {
            coordinate_daemon_shutdown(&first_lifecycle, deadline, async move {
                let _dropped = Dropped(first_dropped);
                first_started.notify_one();
                let _lock = first_lock.lock().await;
                DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), async {
                    ShutdownTaskReceipt::default()
                })
            })
            .await
        });
        prepare_started.notified().await;

        tokio::time::advance(tokio::time::Duration::from_secs(1)).await;
        let receipt = first.await.expect("shutdown prepare timeout receipt");

        assert!(receipt.is_retryable());
        assert_eq!(receipt.in_flight, ShutdownStatus::TimedOut);
        assert_eq!(receipt.clients, ShutdownStatus::TimedOut);
        assert!(matches!(
            receipt.background.owners.as_slice(),
            [owner] if owner.name == "shutdown_prepare"
                && owner.status == ShutdownStatus::TimedOut
        ));
        assert_eq!(receipt.project_servers.status(), ShutdownStatus::TimedOut,);
        assert!(dropped.load(Ordering::Acquire));

        drop(held_lock);
        let retry_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(1);
        let retry = coordinate_daemon_shutdown(&lifecycle, retry_deadline, async {
            DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), async {
                ShutdownTaskReceipt::default()
            })
        })
        .await;

        assert!(!retry.is_retryable());
    }
}
