#[cfg(test)]
use super::DAEMON_TASK_ABORT_DEADLINE;
use super::memory_repair_scheduler::MemoryRepairSchedulerLifecycle;
use super::scheduler::AutomationSchedulerLifecycle;
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
    ) -> bool {
        self.cancel_automation_schedulers();
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
        tokio::time::timeout_at(deadline, async {
            for retirement in retirements {
                retirement.wait().await;
            }
        })
        .await
        .is_ok()
    }

    pub(super) fn cancel_automation_schedulers(&self) {
        let _child_shutdown = crate::sessions::codex_app_server::begin_codex_app_server_shutdown();
        if let Ok(mut schedulers) = self.store_administration.automation_schedulers().try_lock() {
            for handle in schedulers.values_mut() {
                handle.lifecycle = AutomationSchedulerLifecycle::Retiring;
                handle
                    .generation
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                if let Some(task) = &handle.task {
                    task.abort();
                }
            }
        }
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
    ) -> bool {
        self.cancel_memory_repair_schedulers();
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
        tokio::time::timeout_at(deadline, async {
            for retirement in retirements {
                retirement.wait().await;
            }
        })
        .await
        .is_ok()
    }

    pub(super) fn cancel_memory_repair_schedulers(&self) {
        if let Ok(mut schedulers) = self
            .store_administration
            .memory_repair_schedulers()
            .try_lock()
        {
            for handle in schedulers.values_mut() {
                handle.lifecycle = MemoryRepairSchedulerLifecycle::Retiring;
                handle
                    .generation
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                if let Some(task) = &handle.task {
                    task.abort();
                }
            }
        }
    }
}
