use tracedecay_runtime_core::db::{
    MemoryGraphReconciliationRetirementTerminalV1, MemoryGraphReconciliationTaskOwnerV1,
};
use tracedecay_store::StoreShardIdV1;

use super::{DaemonSessionRuntimeRegistryV1, Result, session_registry_error};

impl DaemonSessionRuntimeRegistryV1 {
    fn memory_graph_reconciliation_owners(&self) -> Vec<MemoryGraphReconciliationTaskOwnerV1> {
        let mut owners = Vec::new();
        if let Some(owner) = self
            .profile_memory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            owners.push(owner.reconciliation.clone());
        }
        let projects = self
            .project_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for state in projects.values() {
            let super::ProjectRuntimeOwnerStateV1::Ready(project) = state else {
                continue;
            };
            if let Some(owner) = project.memory.as_ref() {
                owners.push(owner.reconciliation.clone());
            }
        }
        owners
    }

    pub(crate) fn cancel_memory_graph_reconciliation_tasks(&self) {
        for owner in self.memory_graph_reconciliation_owners() {
            if let Err(error) = owner.cancel() {
                tracing::debug!(
                    ?error,
                    "memory graph reconciliation cancellation was refused"
                );
            }
        }
    }

    pub(crate) async fn shutdown_memory_graph_reconciliation_tasks(
        &self,
    ) -> std::result::Result<(), String> {
        let mut failures = Vec::new();
        for owner in self.memory_graph_reconciliation_owners() {
            match owner.shutdown().await {
                Ok(MemoryGraphReconciliationRetirementTerminalV1::CancelledAndJoined) => {}
                Ok(terminal) => failures.push(format!(
                    "memory graph reconciliation shutdown terminal state: {terminal:?}"
                )),
                Err(error) => failures.push(format!(
                    "start memory graph reconciliation shutdown: {error:?}"
                )),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    pub(crate) async fn retire_memory_graph_reconciliation_task(
        &self,
        shard_id: &StoreShardIdV1,
    ) -> Result<()> {
        if let tracedecay_store::StoreShardScopeV1::Project { project_id } = &shard_id.scope {
            return self.retire_project_memory_graph(project_id).await;
        }
        let owner = self
            .profile_memory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|owner| owner.reconciliation.clone());
        let Some(owner) = owner else {
            return Ok(());
        };
        match owner.shutdown().await {
            Ok(MemoryGraphReconciliationRetirementTerminalV1::CancelledAndJoined) => Ok(()),
            Ok(terminal) => Err(session_registry_error(
                "retire memory graph reconciliation task",
                format!("terminal state: {terminal:?}"),
            )),
            Err(error) => Err(session_registry_error(
                "retire memory graph reconciliation task",
                format!("start error: {error:?}"),
            )),
        }
    }
}
