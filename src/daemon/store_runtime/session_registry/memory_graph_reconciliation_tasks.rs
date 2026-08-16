use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tracedecay_runtime_core::db::{Database, MemoryGraphReconciliationTaskOwnerV1};
use tracedecay_store::StoreShardIdV1;

use super::{DaemonSessionRuntimeRegistryV1, Result, session_registry_error};

#[derive(Default)]
struct RetainedMemoryGraphReconciliationTaskStateV1 {
    accepting: bool,
    tasks: BTreeMap<StoreShardIdV1, MemoryGraphReconciliationTaskOwnerV1>,
}

#[derive(Default)]
pub(super) struct RetainedMemoryGraphReconciliationTasksV1 {
    state: Arc<Mutex<RetainedMemoryGraphReconciliationTaskStateV1>>,
}

impl RetainedMemoryGraphReconciliationTasksV1 {
    pub(super) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RetainedMemoryGraphReconciliationTaskStateV1 {
                accepting: true,
                tasks: BTreeMap::new(),
            })),
        }
    }

    fn retain(
        &self,
        shard_id: StoreShardIdV1,
        owner: MemoryGraphReconciliationTaskOwnerV1,
    ) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| {
            session_registry_error(
                "retain memory graph reconciliation task",
                "memory graph task registry lock is poisoned".to_owned(),
            )
        })?;
        if !state.accepting {
            return Err(session_registry_error(
                "retain memory graph reconciliation task",
                "memory graph task registry is shutting down".to_owned(),
            ));
        }
        if let Some(retained) = state.tasks.get(&shard_id) {
            return if retained.same_coordinator(&owner) {
                Ok(())
            } else {
                Err(session_registry_error(
                    "retain memory graph reconciliation task",
                    "memory graph shard already has a different task coordinator".to_owned(),
                ))
            };
        }
        state.tasks.insert(shard_id, owner);
        Ok(())
    }

    pub(super) fn cancel(&self) {
        let tasks = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.accepting = false;
            state.tasks.values().cloned().collect::<Vec<_>>()
        };
        for task in tasks {
            task.cancel();
        }
    }

    pub(super) fn retained_count(&self) -> Result<usize> {
        self.state
            .lock()
            .map(|state| state.tasks.len())
            .map_err(|_| {
                session_registry_error(
                    "observe memory graph reconciliation tasks",
                    "memory graph task registry lock is poisoned".to_owned(),
                )
            })
    }

    pub(super) async fn shutdown(&self) -> std::result::Result<(), String> {
        self.cancel();
        let tasks = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tasks
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut failures = Vec::new();
        for task in tasks {
            if let Err(error) = task.shutdown().await {
                failures.push(error);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    pub(super) async fn retire(
        &self,
        shard_id: &StoreShardIdV1,
    ) -> std::result::Result<(), String> {
        let owner = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tasks
            .get(shard_id)
            .cloned();
        let Some(owner) = owner else {
            return Ok(());
        };
        owner.cancel();
        owner.shutdown().await?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .tasks
            .get(shard_id)
            .is_some_and(|retained| retained.same_coordinator(&owner))
        {
            state.tasks.remove(shard_id);
        }
        Ok(())
    }
}

impl Drop for RetainedMemoryGraphReconciliationTasksV1 {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl DaemonSessionRuntimeRegistryV1 {
    pub(super) fn retain_memory_graph_reconciliation_task(
        &self,
        shard_id: &StoreShardIdV1,
        database: &Database,
    ) -> Result<()> {
        if database.retained_runtime().binding().shard_id != *shard_id
            || database.memory_graph_runtime().is_none()
            || !database.is_writable()
        {
            return Err(session_registry_error(
                "retain memory graph reconciliation task",
                "memory graph task owner does not match a writable bound database".to_owned(),
            ));
        }
        let owner = database
            .memory_graph_reconciliation_task_owner()
            .ok_or_else(|| {
                session_registry_error(
                    "retain memory graph reconciliation task",
                    "memory graph database has no cancellation authority".to_owned(),
                )
            })?;
        self.memory_graph_reconciliation_tasks
            .retain(shard_id.clone(), owner)
    }

    pub(crate) fn cancel_memory_graph_reconciliation_tasks(&self) {
        self.memory_graph_reconciliation_tasks.cancel();
    }

    pub(crate) async fn shutdown_memory_graph_reconciliation_tasks(
        &self,
    ) -> std::result::Result<(), String> {
        self.memory_graph_reconciliation_tasks.shutdown().await
    }

    pub(crate) async fn retire_memory_graph_reconciliation_task(
        &self,
        shard_id: &StoreShardIdV1,
    ) -> Result<()> {
        self.memory_graph_reconciliation_tasks
            .retire(shard_id)
            .await
            .map_err(|error| {
                session_registry_error("retire memory graph reconciliation task", error)
            })
    }
}
