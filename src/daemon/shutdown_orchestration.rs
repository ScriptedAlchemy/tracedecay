//! One transport-neutral daemon shutdown sequence.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::task::JoinSet;

use super::shutdown_coordination::{
    ShutdownOwner, ShutdownReceipt, ShutdownStatus, prepare_shutdown_owner_phases,
};
use super::store_shutdown::ShutdownTaskReceipt;
use super::{DAEMON_CLIENT_DRAIN_DEADLINE, DAEMON_TASK_ABORT_DEADLINE, DaemonLifecycle};
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
}

pub(super) async fn coordinate_daemon_shutdown<Prepare>(
    lifecycle: &DaemonLifecycle,
    shutdown_deadline: tokio::time::Instant,
    prepare: Prepare,
) -> Arc<DaemonShutdownReceipt>
where
    Prepare: Future<Output = DaemonShutdownPlan> + Send + 'static,
{
    lifecycle.begin_draining();
    if lifecycle.claim_shutdown_coordination() {
        let coordinator_lifecycle = lifecycle.clone();
        let runner_lifecycle = lifecycle.clone();
        tokio::spawn(async move {
            let runner = tokio::spawn(async move {
                run_daemon_shutdown(runner_lifecycle, prepare.await, shutdown_deadline).await
            });
            let receipt = match runner.await {
                Ok(receipt) => receipt,
                Err(error) => {
                    DaemonShutdownReceipt::coordinator_failed(shutdown_deadline, error.to_string())
                }
            };
            coordinator_lifecycle.publish_shutdown_receipt(Arc::new(receipt));
        });
    } else {
        drop(prepare);
    }

    match lifecycle.wait_for_shutdown_receipt().await {
        Ok(receipt) => receipt,
        Err(error) => {
            let receipt = Arc::new(DaemonShutdownReceipt::coordinator_failed(
                shutdown_deadline,
                error,
            ));
            lifecycle.publish_shutdown_receipt(Arc::clone(&receipt));
            receipt
        }
    }
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
}
