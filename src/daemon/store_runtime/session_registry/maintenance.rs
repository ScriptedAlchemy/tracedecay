use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex as StdMutex};

#[cfg(test)]
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinHandle;
use tracedecay_store::{StoreRuntimeBindingV1, StoreShardIdV1};

use super::{
    DaemonSessionRuntimeRegistryV1, LONG_LIVED_SESSION_MAINTENANCE, RegisteredGlobalDb, Result,
    StoreRuntimeHandle, registry_open_error, release_process_allocator_memory,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RegisteredSchemaConvergenceStatus {
    Pending,
    #[cfg(test)]
    Complete,
    #[cfg(test)]
    Degraded {
        message: String,
    },
}

pub(super) struct RegisteredSchemaConvergenceMaintenance {
    statuses: Arc<StdMutex<BTreeMap<StoreShardIdV1, RegisteredSchemaConvergenceStatus>>>,
    tasks: StdMutex<BTreeMap<StoreShardIdV1, JoinHandle<()>>>,
    #[cfg(test)]
    schedule_count: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    gate: StdMutex<Option<Arc<RegisteredSchemaConvergenceTestGateState>>>,
}

impl RegisteredSchemaConvergenceMaintenance {
    pub(super) fn new() -> Self {
        Self {
            statuses: Arc::new(StdMutex::new(BTreeMap::new())),
            tasks: StdMutex::new(BTreeMap::new()),
            #[cfg(test)]
            schedule_count: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            gate: StdMutex::new(None),
        }
    }

    #[cfg(test)]
    pub(super) fn status(
        &self,
        shard_id: &StoreShardIdV1,
    ) -> Option<RegisteredSchemaConvergenceStatus> {
        self.statuses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(shard_id)
            .cloned()
    }

    pub(super) fn defer(&self, shard_id: StoreShardIdV1) {
        self.statuses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(shard_id)
            .or_insert(RegisteredSchemaConvergenceStatus::Pending);
    }

    #[cfg(test)]
    pub(super) fn schedule(
        &self,
        database: Arc<RegisteredGlobalDb>,
        convergence: Option<crate::global_db::schema_stages::RegisteredSchemaConvergence>,
    ) {
        let shard_id = database.binding().shard_id.clone();
        {
            let mut statuses = self
                .statuses
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if statuses.contains_key(&shard_id) {
                return;
            }
            statuses.insert(shard_id.clone(), RegisteredSchemaConvergenceStatus::Pending);
        }
        #[cfg(test)]
        self.schedule_count.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        let gate = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let statuses = Arc::clone(&self.statuses);
        let task_shard_id = shard_id.clone();
        let task = tokio::spawn(async move {
            #[cfg(test)]
            if let Some(gate) = gate {
                gate.block().await;
            }
            let result = match convergence {
                Some(convergence) => database.converge_schema(convergence).await,
                None => Ok(()),
            };
            if let Err(error) = database.release_connection_memory().await {
                crate::daemon::log_daemon_event(
                    "registered_schema_convergence_memory_release",
                    &[
                        ("outcome", "degraded".to_owned()),
                        ("database", database.db_path().display().to_string()),
                        ("shard", format!("{task_shard_id:?}")),
                        ("error", error.to_string()),
                    ],
                );
            }
            release_process_allocator_memory();
            let status = match result {
                Ok(()) => {
                    crate::daemon::log_daemon_event(
                        "registered_schema_convergence",
                        &[
                            ("outcome", "complete".to_owned()),
                            ("database", database.db_path().display().to_string()),
                            ("shard", format!("{task_shard_id:?}")),
                        ],
                    );
                    RegisteredSchemaConvergenceStatus::Complete
                }
                Err(error) => {
                    let message = error.to_string();
                    crate::daemon::log_daemon_event(
                        "registered_schema_convergence",
                        &[
                            ("outcome", "degraded".to_owned()),
                            ("database", database.db_path().display().to_string()),
                            ("shard", format!("{task_shard_id:?}")),
                            ("error", message.clone()),
                        ],
                    );
                    RegisteredSchemaConvergenceStatus::Degraded { message }
                }
            };
            statuses
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(task_shard_id, status);
        });
        self.tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(shard_id, task);
    }

    #[cfg(test)]
    pub(super) fn install_gate(&self) -> RegisteredSchemaConvergenceTestGate {
        let state = Arc::new(RegisteredSchemaConvergenceTestGateState {
            started: AtomicBool::new(false),
            started_notify: Notify::new(),
            release: Semaphore::new(0),
        });
        *self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&state));
        RegisteredSchemaConvergenceTestGate { state }
    }
}

impl Drop for RegisteredSchemaConvergenceMaintenance {
    fn drop(&mut self) {
        let tasks = self
            .tasks
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (_, task) in std::mem::take(tasks) {
            task.abort();
        }
    }
}

#[cfg(test)]
pub(super) struct RegisteredSchemaConvergenceTestGateState {
    started: AtomicBool,
    started_notify: Notify,
    release: Semaphore,
}

#[cfg(test)]
impl RegisteredSchemaConvergenceTestGateState {
    async fn block(&self) {
        self.started.store(true, Ordering::Release);
        self.started_notify.notify_waiters();
        self.release
            .acquire()
            .await
            .expect("registered schema convergence test gate remains open")
            .forget();
    }
}

#[cfg(test)]
pub(super) struct RegisteredSchemaConvergenceTestGate {
    state: Arc<RegisteredSchemaConvergenceTestGateState>,
}

#[cfg(test)]
impl RegisteredSchemaConvergenceTestGate {
    pub(super) async fn wait_until_blocked(&self) {
        while !self.state.started.load(Ordering::Acquire) {
            self.state.started_notify.notified().await;
        }
    }

    pub(super) fn release(&self) {
        self.state.release.add_permits(1);
    }
}

impl DaemonSessionRuntimeRegistryV1 {
    fn long_lived_session_maintenance(&self) -> bool {
        if LONG_LIVED_SESSION_MAINTENANCE.load(Ordering::Relaxed) {
            return true;
        }
        #[cfg(test)]
        if self
            .long_lived_session_maintenance_for_test
            .load(Ordering::Relaxed)
        {
            return true;
        }
        false
    }

    pub(super) async fn attach_registered(
        &self,
        runtime: StoreRuntimeHandle,
        operation: &'static str,
    ) -> Result<Arc<RegisteredGlobalDb>> {
        let expected_binding: StoreRuntimeBindingV1 = runtime.binding().clone();
        let expected_locator = runtime.locator().verified().clone();
        let authority = runtime
            .database_authority(operation)
            .map_err(|failure| registry_open_error(operation, failure))?;
        let long_lived = self.long_lived_session_maintenance();
        let (database, convergence) = if long_lived {
            RegisteredGlobalDb::migrate_and_attach_for_daemon(
                runtime,
                expected_binding,
                expected_locator,
                authority,
            )
            .await?
        } else {
            (
                RegisteredGlobalDb::migrate_and_attach(
                    runtime,
                    expected_binding,
                    expected_locator,
                    authority,
                )
                .await?,
                None,
            )
        };
        let database = Arc::new(database);
        if long_lived {
            #[cfg(test)]
            if self
                .long_lived_session_maintenance_for_test
                .load(Ordering::Relaxed)
            {
                self.registered_schema_convergence
                    .schedule(Arc::clone(&database), convergence);
                return Ok(database);
            }
            let _ = convergence;
            if let Err(error) = database.release_connection_memory().await {
                crate::daemon::log_daemon_event(
                    "registered_schema_admission_memory_release",
                    &[
                        ("outcome", "degraded".to_owned()),
                        ("database", database.db_path().display().to_string()),
                        ("error", error.to_string()),
                    ],
                );
            }
            release_process_allocator_memory();
            self.registered_schema_convergence
                .defer(database.binding().shard_id.clone());
        }
        Ok(database)
    }

    #[cfg(test)]
    pub(crate) fn registered_schema_convergence_status(
        &self,
        shard_id: &StoreShardIdV1,
    ) -> Option<RegisteredSchemaConvergenceStatus> {
        self.registered_schema_convergence.status(shard_id)
    }

    #[cfg(test)]
    pub(super) fn enable_long_lived_session_maintenance_for_test(&self) {
        self.long_lived_session_maintenance_for_test
            .store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(super) fn block_registered_schema_convergence_for_test(
        &self,
    ) -> RegisteredSchemaConvergenceTestGate {
        self.registered_schema_convergence.install_gate()
    }

    #[cfg(test)]
    pub(super) fn registered_schema_convergence_schedule_count_for_test(&self) -> usize {
        self.registered_schema_convergence
            .schedule_count
            .load(Ordering::Relaxed)
    }
}
