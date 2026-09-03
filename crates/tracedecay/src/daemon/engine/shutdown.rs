//! Shutdown ownership for one Unix daemon engine generation.
//!
//! `shutdown_owner_phases` names every retained background owner and hands it
//! to the shutdown coordinator as (cancel, join) pairs in dependency order:
//! producers first, then the invocation registry that admits provider work,
//! then the store-settling reapers. The phase deadline bounds every join and
//! reports a typed timeout under the owner's name.
//!
//! The `cancel` side is not decoration. `prepare_shutdown_owner_phases` runs
//! every phase's `cancel` synchronously before the *first* join is polled, so
//! an owner that only cancels inside its join future is not actually told to
//! stop until its phase is reached — and if the coordinator aborts the drain
//! runner first, it is never told at all and keeps running past the terminal
//! receipt. Owners with a cheap synchronous stop (`invocation`, `maintenance`,
//! `git_watcher`) therefore supply a real `cancel`; a `|| {}` cancel side is
//! only correct where no synchronous stop exists.

use std::sync::Arc;

use super::DaemonEngine;
use crate::daemon::shutdown_coordination::{ShutdownOwner, ShutdownStatus};
#[cfg(test)]
use crate::daemon::shutdown_orchestration::{
    DaemonShutdownPlan, DaemonShutdownReceipt, coordinate_daemon_shutdown,
};
use crate::daemon::store_shutdown::ShutdownTaskReceipt;
use crate::daemon::{log_daemon_event, project_open_tasks, shutdown_project_servers};
#[cfg(test)]
use tracedecay_runtime_core::DAEMON_SHUTDOWN_DEADLINE;

impl DaemonEngine {
    #[hotpath::measure(label = "daemon.engine.shutdown_owner_phases", future = true)]
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
            vec![ShutdownOwner::with_deadline_status(
                "invocation",
                {
                    let invocation_cancel = self.invocation.clone();
                    move || invocation_cancel.cancel_admissions()
                },
                move |_| async move {
                    if invocation_join.shutdown().await {
                        ShutdownStatus::Clean
                    } else {
                        ShutdownStatus::Failed(
                            "invocation runtime shutdown was incomplete".to_owned(),
                        )
                    }
                },
            )],
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
                ShutdownOwner::new(
                    "maintenance",
                    {
                        let maintenance_cancel = self.maintenance_coordinator.clone();
                        move || maintenance_cancel.cancel()
                    },
                    async move {
                        maintenance_join.shutdown().await;
                    },
                ),
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
            vec![ShutdownOwner::with_deadline(
                "retirement_reapers",
                || {},
                move |deadline| async move {
                    let (pending, reapers) = reaper_join.retirement_reaper_counts();
                    log_daemon_event(
                        "daemon_shutdown",
                        &[
                            ("outcome", "retirement_reapers_join_start".to_owned()),
                            ("pending", pending.to_string()),
                            ("reapers", reapers.to_string()),
                            (
                                "deadline_remaining_ms",
                                deadline
                                    .saturating_duration_since(tokio::time::Instant::now())
                                    .as_millis()
                                    .to_string(),
                            ),
                        ],
                    );
                    reaper_join.shutdown_retirement_reapers().await;
                    let (pending, reapers) = reaper_join.retirement_reaper_counts();
                    log_daemon_event(
                        "daemon_shutdown",
                        &[
                            ("outcome", "retirement_reapers_joined".to_owned()),
                            ("pending", pending.to_string()),
                            ("reapers", reapers.to_string()),
                        ],
                    );
                },
            )],
        ]
    }

    pub(in crate::daemon) fn memory_graph_reconciliation_shutdown_owner(&self) -> ShutdownOwner {
        let administration = self.store_administration.clone();
        let store_telemetry_sampling = self.store_administration.store_telemetry_sampling();
        ShutdownOwner::with_deadline_result(
            "memory_graph_reconciliation",
            || {},
            move |_| {
                hotpath::future!(
                    async move {
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
                        store_telemetry_sampling.release_retained_handles_for_shutdown();
                        administration
                            .close_retained_graph_runtimes_for_shutdown()
                            .await
                            .map_err(|error| error.to_string())
                    },
                    label = "daemon.engine.memory_graph_reconciliation"
                )
            },
        )
    }

    #[hotpath::measure(label = "daemon.engine.shutdown.servers", future = true)]
    pub(in crate::daemon) async fn shutdown_servers(
        &self,
        deadline: tokio::time::Instant,
    ) -> ShutdownTaskReceipt {
        shutdown_project_servers(
            deadline,
            &self.store_administration,
            &self.http_application_registry,
        )
        .await
    }

    #[cfg(test)]
    #[hotpath::skip]
    pub(in crate::daemon) async fn shutdown_all(&self) -> Arc<DaemonShutdownReceipt> {
        let deadline = tokio::time::Instant::now() + DAEMON_SHUTDOWN_DEADLINE;
        let lifecycle = self.lifecycle.clone();
        let shutdown_engine = self.clone();
        coordinate_daemon_shutdown(&lifecycle, deadline, async move {
            let owner_phases = shutdown_engine.shutdown_owner_phases().await;
            let terminal_owner = shutdown_engine.memory_graph_reconciliation_shutdown_owner();
            let server_engine = shutdown_engine.clone();
            DaemonShutdownPlan::new(
                tokio::task::JoinSet::<tracedecay_domain::errors::Result<()>>::new(),
                owner_phases,
                move |project_server_deadline| async move {
                    server_engine
                        .shutdown_servers(project_server_deadline)
                        .await
                },
            )
            .with_terminal_owner_phases(vec![vec![terminal_owner]])
        })
        .await
    }
}
