//! One transport-neutral daemon shutdown sequence.

use tokio::task::JoinSet;

use super::shutdown_coordination::{ShutdownOwner, ShutdownReceipt, prepare_shutdown_owner_phases};
use super::store_shutdown::ShutdownTaskReceipt;
use super::{DAEMON_CLIENT_DRAIN_DEADLINE, DAEMON_TASK_ABORT_DEADLINE, DaemonLifecycle};
use crate::errors::Result;

pub(super) struct DaemonShutdownReceipt {
    pub(super) in_flight_drained: bool,
    pub(super) clients_drained: bool,
    pub(super) background: ShutdownReceipt,
    pub(super) project_servers: ShutdownTaskReceipt,
}

pub(super) async fn coordinate_daemon_shutdown<ProjectServerShutdown>(
    lifecycle: &DaemonLifecycle,
    clients: &mut JoinSet<Result<()>>,
    shutdown_deadline: tokio::time::Instant,
    owner_phases: Vec<Vec<ShutdownOwner>>,
    project_server_shutdown: ProjectServerShutdown,
) -> DaemonShutdownReceipt
where
    ProjectServerShutdown: std::future::Future<Output = ShutdownTaskReceipt>,
{
    let prepared = prepare_shutdown_owner_phases(owner_phases);
    let mut background_shutdown = Box::pin(prepared.join(shutdown_deadline));
    let mut background_receipt = None;
    let client_drain_deadline = std::cmp::min(
        tokio::time::Instant::now() + DAEMON_CLIENT_DRAIN_DEADLINE,
        shutdown_deadline,
    );
    let in_flight = tokio::time::timeout_at(client_drain_deadline, lifecycle.wait_for_idle());
    tokio::pin!(in_flight);
    let in_flight_drained = loop {
        tokio::select! {
            receipt = &mut background_shutdown, if background_receipt.is_none() => {
                background_receipt = Some(receipt);
            }
            drained = &mut in_flight => break drained.is_ok(),
        }
    };

    clients.abort_all();
    let client_join_deadline = std::cmp::min(
        tokio::time::Instant::now() + DAEMON_TASK_ABORT_DEADLINE,
        shutdown_deadline,
    );
    let client_tasks_drained = tokio::time::timeout_at(client_join_deadline, async {
        while clients.join_next().await.is_some() {}
    })
    .await
    .is_ok();
    let client_activity_drained =
        tokio::time::timeout_at(client_join_deadline, lifecycle.wait_for_idle())
            .await
            .is_ok();
    let clients_drained = client_tasks_drained && client_activity_drained;
    let background = match background_receipt {
        Some(receipt) => receipt,
        None => background_shutdown.await,
    };
    let project_servers = project_server_shutdown.await;
    DaemonShutdownReceipt {
        in_flight_drained,
        clients_drained,
        background,
        project_servers,
    }
}
