//! Shutdown ownership for one Unix daemon engine generation.
//!
//! `shutdown_owner_phases` names every retained background owner and hands it
//! to the shutdown coordinator as (cancel, join) pairs in dependency order:
//! producers first, then the invocation registry that admits provider work,
//! then the store-settling reapers. Owners whose shutdown entry point already
//! cancels internally get a no-op cancel side; the phase deadline still bounds
//! their join and reports a typed timeout under the owner's name.

use std::sync::Arc;

use super::DaemonEngine;
#[cfg(test)]
use crate::daemon::DAEMON_SHUTDOWN_DEADLINE;
use crate::daemon::shutdown_coordination::{ShutdownOwner, ShutdownStatus};
#[cfg(test)]
use crate::daemon::shutdown_orchestration::{
    DaemonShutdownPlan, DaemonShutdownReceipt, coordinate_daemon_shutdown,
};
use crate::daemon::store_shutdown::{ShutdownTaskOutcome, ShutdownTaskReceipt};
use crate::daemon::{log_daemon_event, project_open_tasks, shutdown_project_servers};

impl DaemonEngine {
    pub(in crate::daemon) async fn shutdown_owner_phases(&self) -> Vec<Vec<ShutdownOwner>> {
        let project_open = project_open_tasks(&self.project_open_gates).await;

        let invocation_join = self.invocation.clone();

        let session_refresh = Arc::clone(
            self.store_administration
                .session_temporal_refresh_schedulers(),
        );

        let automation_join = self.clone();
        let repair_join = self.clone();

        let replay_join = self.store_administration.clone();
        let session_sync_join = self.store_administration.clone();
        let reaper_join = self.store_administration.clone();

        let maintenance_join = self.maintenance_coordinator.clone();
        let watcher_cancel = self.git_watcher.clone();
        let watcher_join = self.git_watcher.clone();

        let pr_join = Arc::clone(&self.pr_autotrack_task);

        vec![
            vec![
                ShutdownOwner::with_deadline_status(
                    "project_open",
                    || {},
                    move |_| async move {
                        if project_open.shutdown().await {
                            ShutdownStatus::Clean
                        } else {
                            ShutdownStatus::TimedOut
                        }
                    },
                ),
                ShutdownOwner::new("automation", || {}, async move {
                    automation_join.shutdown_automation_schedulers().await;
                }),
                ShutdownOwner::new("memory_repair", || {}, async move {
                    repair_join.shutdown_memory_repair_schedulers().await;
                }),
                ShutdownOwner::new("session_temporal_refresh", || {}, async move {
                    session_refresh.shutdown().await;
                }),
                ShutdownOwner::new("host_admission_replay", || {}, async move {
                    replay_join.shutdown_host_admission_replay().await;
                }),
                ShutdownOwner::new("maintenance", || {}, async move {
                    maintenance_join.shutdown().await;
                }),
                ShutdownOwner::with_deadline_status(
                    "git_watcher",
                    move || watcher_cancel.cancel(),
                    move |_| async move {
                        let outcome = watcher_join.shutdown().await;
                        if outcome.is_clean() {
                            ShutdownStatus::Clean
                        } else {
                            ShutdownStatus::Failed(
                                outcome
                                    .failures()
                                    .iter()
                                    .map(|failure| format!("{failure:?}"))
                                    .collect::<Vec<_>>()
                                    .join("; "),
                            )
                        }
                    },
                ),
                ShutdownOwner::new("pr_autotrack", || {}, async move {
                    if let Some(task) = pr_join.lock().await.take() {
                        task.shutdown().await;
                    }
                }),
            ],
            vec![ShutdownOwner::new("invocation", || {}, async move {
                invocation_join.shutdown().await;
            })],
            vec![
                ShutdownOwner::new("retirement_reapers", || {}, async move {
                    reaper_join.shutdown_retirement_reapers().await;
                }),
                ShutdownOwner::new("session_sync", || {}, async move {
                    session_sync_join.shutdown_session_sync().await;
                }),
            ],
        ]
    }

    /// Fence Git mutation admission and join every transaction store actor
    /// before project servers close, so no native Git work outlives the
    /// stores it journals into.
    pub(in crate::daemon) async fn shutdown_servers(
        &self,
        deadline: tokio::time::Instant,
    ) -> ShutdownTaskReceipt {
        let git_transactions = match self
            .store_administration
            .git_index_transaction_services()
            .shutdown()
            .await
        {
            Ok(receipt) => {
                log_daemon_event(
                    "daemon_shutdown",
                    &[
                        ("outcome", "git_transactions_joined".to_string()),
                        ("services_closed", receipt.services_closed.to_string()),
                        (
                            "store_actors_joined",
                            receipt.store_actors_joined.to_string(),
                        ),
                    ],
                );
                ShutdownTaskReceipt::default()
            }
            Err(error) => ShutdownTaskReceipt {
                outcomes: vec![ShutdownTaskOutcome {
                    owner: "git_index_transactions".to_owned(),
                    status: ShutdownStatus::Failed(format!("{error:?}")),
                }],
            },
        };
        let native_integration = match self
            .store_administration
            .native_integration_services()
            .shutdown()
            .await
        {
            Ok(store_actors_joined) => {
                log_daemon_event(
                    "daemon_shutdown",
                    &[
                        ("outcome", "native_integration_joined".to_string()),
                        ("store_actors_joined", store_actors_joined.to_string()),
                    ],
                );
                ShutdownTaskReceipt::default()
            }
            Err(error) => ShutdownTaskReceipt {
                outcomes: vec![ShutdownTaskOutcome {
                    owner: "native_integration_transactions".to_owned(),
                    status: ShutdownStatus::Failed(format!("{error:?}")),
                }],
            },
        };
        let mut receipt = shutdown_project_servers(deadline, &self.store_administration).await;
        receipt.extend(git_transactions);
        receipt.extend(native_integration);
        receipt
    }

    #[cfg(test)]
    pub(in crate::daemon) async fn shutdown_all(&self) -> Arc<DaemonShutdownReceipt> {
        let deadline = tokio::time::Instant::now() + DAEMON_SHUTDOWN_DEADLINE;
        let lifecycle = self.lifecycle.clone();
        let shutdown_engine = self.clone();
        coordinate_daemon_shutdown(&lifecycle, deadline, async move {
            let owner_phases = shutdown_engine.shutdown_owner_phases().await;
            let server_engine = shutdown_engine.clone();
            DaemonShutdownPlan::new(
                tokio::task::JoinSet::<crate::errors::Result<()>>::new(),
                owner_phases,
                async move { server_engine.shutdown_servers(deadline).await },
            )
        })
        .await
    }
}
