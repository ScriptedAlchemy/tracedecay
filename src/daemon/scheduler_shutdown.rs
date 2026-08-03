#[cfg(test)]
use super::DAEMON_TASK_ABORT_DEADLINE;
use super::shutdown_coordination::ShutdownStatus;
use super::{DaemonEngine, ProjectServerKey};

impl DaemonEngine {
    #[cfg(test)]
    pub(super) async fn shutdown_automation_schedulers(&self) {
        self.shutdown_automation_schedulers_until(
            tokio::time::Instant::now() + DAEMON_TASK_ABORT_DEADLINE,
        )
        .await;
    }

    pub(super) async fn shutdown_automation_schedulers_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> ShutdownStatus {
        self.cancel_automation_schedulers();
        match tokio::time::timeout_at(deadline, async {
            let owners: Vec<ProjectServerKey> = self
                .store_administration
                .automation_schedulers()
                .lock()
                .await
                .keys()
                .cloned()
                .collect();
            let mut retirements = Vec::with_capacity(owners.len());
            for owner in owners {
                if let Some(retirement) = self.retire_automation_scheduler_locked(&owner).await {
                    retirements.push(retirement);
                }
            }
            self.store_administration
                .automation_schedulers()
                .lock()
                .await
                .clear();
            let mut failures = Vec::new();
            for retirement in retirements {
                if let ShutdownStatus::Failed(error) = retirement.wait().await {
                    failures.push(error);
                }
            }
            if failures.is_empty() {
                ShutdownStatus::Clean
            } else {
                ShutdownStatus::Failed(failures.join("; "))
            }
        })
        .await
        {
            Ok(status) => status,
            Err(_) => ShutdownStatus::TimedOut,
        }
    }

    pub(super) fn cancel_automation_schedulers(&self) {
        let _child_shutdown = crate::sessions::codex_app_server::begin_codex_app_server_shutdown();
        self.lifecycle.begin_draining();
    }

    #[cfg(test)]
    pub(super) async fn shutdown_memory_repair_schedulers(&self) {
        self.shutdown_memory_repair_schedulers_until(
            tokio::time::Instant::now() + DAEMON_TASK_ABORT_DEADLINE,
        )
        .await;
    }

    pub(super) async fn shutdown_memory_repair_schedulers_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> ShutdownStatus {
        self.cancel_memory_repair_schedulers();
        match tokio::time::timeout_at(deadline, async {
            let owners: Vec<ProjectServerKey> = self
                .store_administration
                .memory_repair_schedulers()
                .lock()
                .await
                .keys()
                .cloned()
                .collect();
            let mut retirements = Vec::with_capacity(owners.len());
            for owner in owners {
                if let Some(retirement) = self.retire_memory_repair_scheduler_locked(&owner).await {
                    retirements.push(retirement);
                }
            }
            self.store_administration
                .memory_repair_schedulers()
                .lock()
                .await
                .clear();
            let mut failures = Vec::new();
            for retirement in retirements {
                if let ShutdownStatus::Failed(error) = retirement.wait().await {
                    failures.push(error);
                }
            }
            if failures.is_empty() {
                ShutdownStatus::Clean
            } else {
                ShutdownStatus::Failed(failures.join("; "))
            }
        })
        .await
        {
            Ok(status) => status,
            Err(_) => ShutdownStatus::TimedOut,
        }
    }

    pub(super) fn cancel_memory_repair_schedulers(&self) {
        self.lifecycle.begin_draining();
    }
}
