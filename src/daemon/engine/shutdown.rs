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
use crate::daemon::store_shutdown::ShutdownTaskReceipt;
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

        let replay_join = self.store_administration.clone();
        let session_sync_join = self.store_administration.clone();
        let reaper_join = self.store_administration.clone();
        let git_transactions_join =
            Arc::clone(self.store_administration.git_index_transaction_services());
        let native_integration_join =
            Arc::clone(self.store_administration.native_integration_services());

        let maintenance_join = self.maintenance_coordinator.clone();
        let watcher_cancel = self.git_watcher.clone();
        let watcher_join = self.git_watcher.clone();

        let pr_join = Arc::clone(&self.pr_autotrack_task);

        vec![
            vec![ShutdownOwner::new("invocation", || {}, async move {
                invocation_join.shutdown().await;
            })],
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
                ShutdownOwner::new("session_sync", || {}, async move {
                    session_sync_join.shutdown_session_sync().await;
                }),
            ],
            vec![
                ShutdownOwner::with_deadline_result(
                    "git_index_transactions",
                    || {},
                    move |_| async move {
                        let receipt = git_transactions_join
                            .shutdown()
                            .await
                            .map_err(|error| format!("{error:?}"))?;
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
                        Ok::<(), String>(())
                    },
                ),
                ShutdownOwner::with_deadline_result(
                    "native_integration_transactions",
                    || {},
                    move |_| async move {
                        let store_actors_joined = native_integration_join
                            .shutdown()
                            .await
                            .map_err(|error| format!("{error:?}"))?;
                        log_daemon_event(
                            "daemon_shutdown",
                            &[
                                ("outcome", "native_integration_joined".to_string()),
                                ("store_actors_joined", store_actors_joined.to_string()),
                            ],
                        );
                        Ok::<(), String>(())
                    },
                ),
            ],
            vec![ShutdownOwner::new(
                "retirement_reapers",
                || {},
                async move {
                    reaper_join.shutdown_retirement_reapers().await;
                },
            )],
        ]
    }

    pub(in crate::daemon) fn memory_graph_reconciliation_shutdown_owner(&self) -> ShutdownOwner {
        let administration = self.store_administration.clone();
        ShutdownOwner::with_deadline_result(
            "memory_graph_reconciliation",
            || {},
            move |_| async move {
                // Ordering is the correctness contract here: close registry
                // admission, cancel reconciliation, JOIN the workers while
                // their runtimes are still alive, and only then drain the
                // retained owners and close the graphs. Closing before the
                // join leaves the standing owner attachments leased and the
                // close reports a structural Conflict on every shutdown.
                // Every step stays bounded by this owner's deadline, so a
                // genuinely stuck pass still surfaces as a typed timeout.
                let owner = administration
                    .prepare_memory_graph_reconciliation_shutdown()
                    .await
                    .map_err(|error| error.to_string())?;
                owner.cancel();
                owner.shutdown().await?;
                administration
                    .close_retained_graph_runtimes_for_shutdown()
                    .await
                    .map_err(|error| error.to_string())
            },
        )
    }

    pub(in crate::daemon) async fn shutdown_servers(
        &self,
        deadline: tokio::time::Instant,
    ) -> ShutdownTaskReceipt {
        shutdown_project_servers(deadline, &self.store_administration).await
    }

    #[cfg(test)]
    pub(in crate::daemon) async fn shutdown_all(&self) -> Arc<DaemonShutdownReceipt> {
        let deadline = tokio::time::Instant::now() + DAEMON_SHUTDOWN_DEADLINE;
        let lifecycle = self.lifecycle.clone();
        let shutdown_engine = self.clone();
        coordinate_daemon_shutdown(&lifecycle, deadline, async move {
            let owner_phases = shutdown_engine.shutdown_owner_phases().await;
            let terminal_owner = shutdown_engine.memory_graph_reconciliation_shutdown_owner();
            let server_engine = shutdown_engine.clone();
            DaemonShutdownPlan::new(
                tokio::task::JoinSet::<crate::errors::Result<()>>::new(),
                owner_phases,
                async move { server_engine.shutdown_servers(deadline).await },
            )
            .with_terminal_owner_phases(vec![vec![terminal_owner]])
        })
        .await
    }
}
