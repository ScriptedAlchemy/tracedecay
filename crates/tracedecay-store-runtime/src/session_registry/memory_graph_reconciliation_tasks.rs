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
            && let Some(reconciliation) = owner.reconciliation_owner()
        {
            owners.push(reconciliation);
        }
        let projects = self
            .project_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for state in projects.values() {
            let memory = match state {
                super::ProjectRuntimeOwnerStateV1::Ready(project) => project.memory.as_ref(),
                super::ProjectRuntimeOwnerStateV1::RecoveryRequired(recovery) => {
                    recovery.memory.as_ref()
                }
                super::ProjectRuntimeOwnerStateV1::Faulted(faulted) => {
                    faulted.retained.memory.as_ref()
                }
                super::ProjectRuntimeOwnerStateV1::Opening
                | super::ProjectRuntimeOwnerStateV1::ReplacingSessions
                | super::ProjectRuntimeOwnerStateV1::Recovering
                | super::ProjectRuntimeOwnerStateV1::Retiring => None,
            };
            if let Some(owner) = memory
                && let Some(reconciliation) = owner.reconciliation_owner()
            {
                owners.push(reconciliation);
            }
        }
        owners
    }

    pub fn cancel_memory_graph_reconciliation_tasks(&self) {
        for owner in self.memory_graph_reconciliation_owners() {
            if let Err(error) = owner.cancel() {
                tracing::debug!(
                    ?error,
                    "memory graph reconciliation cancellation was refused"
                );
            }
        }
    }

    pub async fn shutdown_memory_graph_reconciliation_tasks(
        &self,
    ) -> std::result::Result<(), String> {
        let mut failures = Vec::new();
        for owner in self.memory_graph_reconciliation_owners() {
            match owner.shutdown().await {
                Ok(MemoryGraphReconciliationRetirementTerminalV1::CancelledAndJoined) => {}
                // The daemon shutdown owner joins these workers while their
                // runtimes are still retained, so a healthy shutdown reports
                // `CancelledAndJoined`. The join's re-cancel can still find a
                // runtime that was legitimately retired or dropped earlier
                // (coordinated project-memory retirement, harness teardown).
                // `RuntimeUnavailable` still guarantees every worker joined
                // (the panic terminals are distinct), so it remains a clean
                // shutdown, not an unfinished task.
                Ok(MemoryGraphReconciliationRetirementTerminalV1::RuntimeUnavailable) => {}
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

    pub async fn retire_memory_graph_reconciliation_task(
        &self,
        shard_id: &StoreShardIdV1,
    ) -> Result<()> {
        let owner = match &shard_id.scope {
            tracedecay_store::StoreShardScopeV1::Project { project_id } => {
                let projects = self
                    .project_owners
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let memory = match projects.get(project_id) {
                    Some(super::ProjectRuntimeOwnerStateV1::Ready(project)) => {
                        project.memory.as_ref()
                    }
                    Some(super::ProjectRuntimeOwnerStateV1::RecoveryRequired(recovery)) => {
                        recovery.memory.as_ref()
                    }
                    Some(super::ProjectRuntimeOwnerStateV1::Faulted(faulted)) => {
                        faulted.retained.memory.as_ref()
                    }
                    Some(
                        super::ProjectRuntimeOwnerStateV1::Opening
                        | super::ProjectRuntimeOwnerStateV1::ReplacingSessions
                        | super::ProjectRuntimeOwnerStateV1::Recovering
                        | super::ProjectRuntimeOwnerStateV1::Retiring,
                    )
                    | None => None,
                };
                memory.and_then(super::MemoryStoreOwnerV1::reconciliation_owner_and_attachment)
            }
            tracedecay_store::StoreShardScopeV1::ProfileMemory => self
                .profile_memory
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .and_then(super::MemoryStoreOwnerV1::reconciliation_owner_and_attachment),
            _ => None,
        };
        let Some((owner, attachment)) = owner else {
            return Ok(());
        };
        match owner.shutdown().await {
            Ok(MemoryGraphReconciliationRetirementTerminalV1::CancelledAndJoined) => {
                super::MemoryStoreOwnerV1::clear_reconciliation_owner(&attachment, &owner);
                Ok(())
            }
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
