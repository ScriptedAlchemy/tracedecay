//! Shutdown ownership for one Unix daemon engine generation.

use std::sync::Arc;

use super::DaemonEngine;
use crate::daemon::shutdown_coordination::{
    ShutdownOwner, ShutdownReceipt, ShutdownStatus, join_shutdown_owner_phases,
};
use crate::daemon::store_shutdown;
use crate::daemon::{
    DAEMON_SHUTDOWN_DEADLINE, cancel_project_server_startup_ingests, project_open_tasks,
    project_servers_for_shutdown, shutdown_project_servers,
};

impl DaemonEngine {
    #[cfg(test)]
    pub(in crate::daemon) async fn shutdown_project_open_tasks(&self) {
        project_open_tasks(&self.project_open_gates)
            .await
            .shutdown()
            .await;
    }

    pub(in crate::daemon) async fn shutdown_owner_phases(&self) -> Vec<Vec<ShutdownOwner>> {
        let startup_ingest_servers = project_servers_for_shutdown(&self.store_administration).await;
        let project_open = project_open_tasks(&self.project_open_gates).await;
        let project_open_cancel = project_open.clone();
        let project_open_join = project_open;

        let invocation_cancel = self.invocation.clone();
        let invocation_join = self.invocation.clone();

        let session_cancel = Arc::clone(
            self.store_administration
                .session_temporal_refresh_schedulers(),
        );
        let session_join = Arc::clone(
            self.store_administration
                .session_temporal_refresh_schedulers(),
        );

        let automation_cancel = self.clone();
        let automation_join = self.clone();
        let repair_cancel = self.clone();
        let repair_join = self.clone();

        let retirement_cancel = self.store_administration.clone();
        let retirement_join = self.store_administration.clone();
        let replay_cancel = self.store_administration.clone();
        let replay_join = self.store_administration.clone();

        let maintenance_cancel = self.maintenance_coordinator.clone();
        let maintenance_join = self.maintenance_coordinator.clone();
        let watcher_cancel = self.git_watcher.clone();
        let watcher_join = self.git_watcher.clone();

        let pr_cancel = Arc::clone(&self.pr_autotrack_task);
        let pr_join = Arc::clone(&self.pr_autotrack_task);

        vec![
            vec![
                ShutdownOwner::new(
                    "project_server_startup_ingest",
                    move || {
                        cancel_project_server_startup_ingests(&startup_ingest_servers);
                    },
                    async {},
                ),
                ShutdownOwner::with_deadline_status(
                    "project_open",
                    move || project_open_cancel.cancel(),
                    move |deadline| async move {
                        project_open_join.shutdown_until(deadline).await.status()
                    },
                ),
                ShutdownOwner::with_deadline_status(
                    "automation",
                    move || automation_cancel.cancel_automation_schedulers(),
                    move |deadline| async move {
                        if automation_join
                            .shutdown_automation_schedulers_until(deadline)
                            .await
                        {
                            ShutdownStatus::Clean
                        } else {
                            ShutdownStatus::TimedOut
                        }
                    },
                ),
                ShutdownOwner::with_deadline_status(
                    "memory_repair",
                    move || repair_cancel.cancel_memory_repair_schedulers(),
                    move |deadline| async move {
                        if repair_join
                            .shutdown_memory_repair_schedulers_until(deadline)
                            .await
                        {
                            ShutdownStatus::Clean
                        } else {
                            ShutdownStatus::TimedOut
                        }
                    },
                ),
                ShutdownOwner::with_deadline(
                    "session_temporal_refresh",
                    move || session_cancel.cancel(),
                    move |deadline| async move {
                        session_join.shutdown_until(deadline).await;
                    },
                ),
                ShutdownOwner::new(
                    "host_admission_replay",
                    move || replay_cancel.cancel_host_admission_replay(),
                    async move { replay_join.shutdown_host_admission_replay().await },
                ),
                ShutdownOwner::new(
                    "maintenance",
                    move || maintenance_cancel.cancel(),
                    async move { maintenance_join.shutdown().await },
                ),
                ShutdownOwner::new("git_watcher", move || watcher_cancel.cancel(), async move {
                    watcher_join.shutdown().await
                }),
                ShutdownOwner::new(
                    "pr_autotrack",
                    move || {
                        if let Some(task) = pr_cancel
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .as_ref()
                        {
                            task.abort();
                        }
                    },
                    async move {
                        let task = pr_join
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take();
                        if let Some(task) = task {
                            let _ = task.await;
                        }
                    },
                ),
            ],
            vec![ShutdownOwner::new(
                "invocation",
                move || invocation_cancel.cancel(),
                async move { invocation_join.shutdown().await },
            )],
            vec![ShutdownOwner::new(
                "retirement_reapers",
                || {},
                async move {
                    retirement_cancel.cancel_retirement_reapers();
                    retirement_join.shutdown_retirement_reapers().await;
                },
            )],
        ]
    }

    pub(in crate::daemon) async fn shutdown_background_tasks(
        &self,
        deadline: tokio::time::Instant,
    ) -> ShutdownReceipt {
        join_shutdown_owner_phases(deadline, self.shutdown_owner_phases().await).await
    }

    pub(in crate::daemon) async fn shutdown_servers(
        &self,
        deadline: tokio::time::Instant,
    ) -> store_shutdown::ShutdownTaskReceipt {
        shutdown_project_servers(deadline, &self.store_administration).await
    }

    #[cfg(test)]
    pub(in crate::daemon) async fn shutdown_all(&self) {
        self.lifecycle.begin_draining();
        let deadline = tokio::time::Instant::now() + DAEMON_SHUTDOWN_DEADLINE;
        let _ = self.shutdown_background_tasks(deadline).await;
        let _ = self.shutdown_servers(deadline).await;
    }
}
