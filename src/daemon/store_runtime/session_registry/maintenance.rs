use std::collections::BTreeMap;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};

#[cfg(test)]
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinHandle;
use tracedecay_store::StoreShardIdV1;

#[cfg(test)]
use std::sync::atomic::Ordering;

use super::{
    DaemonSessionRuntimeRegistryV1, Database, DatabaseAccessMode, RegisteredGlobalDbLeaseV1,
    RegisteredGlobalDbOwnerV1, Result, StoreRuntimeClientLease, release_process_allocator_memory,
    session_registry_error,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RegisteredSchemaConvergenceStatus {
    Pending,
    Complete,
    Degraded { message: String },
}

type RegisteredSchemaConvergenceStatuses =
    BTreeMap<StoreShardIdV1, RegisteredSchemaConvergenceStatus>;

fn lock_registered_schema_convergence_statuses(
    statuses: &StdMutex<RegisteredSchemaConvergenceStatuses>,
) -> MutexGuard<'_, RegisteredSchemaConvergenceStatuses> {
    match statuses.lock() {
        Ok(statuses) => statuses,
        Err(poisoned) => {
            crate::daemon::log_daemon_event(
                "registered_schema_convergence_state",
                &[
                    ("outcome", "degraded".to_owned()),
                    ("resource", "statuses".to_owned()),
                    (
                        "error",
                        "mutex poisoned; recovering guarded state".to_owned(),
                    ),
                ],
            );
            statuses.clear_poison();
            poisoned.into_inner()
        }
    }
}

pub(super) struct RegisteredSchemaConvergenceMaintenance {
    statuses: Arc<StdMutex<RegisteredSchemaConvergenceStatuses>>,
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
        lock_registered_schema_convergence_statuses(&self.statuses)
            .get(shard_id)
            .cloned()
    }

    #[cfg(test)]
    pub(super) fn defer(&self, shard_id: StoreShardIdV1) {
        lock_registered_schema_convergence_statuses(&self.statuses)
            .entry(shard_id)
            .or_insert(RegisteredSchemaConvergenceStatus::Pending);
    }

    pub(super) fn schedule(
        &self,
        database: RegisteredGlobalDbLeaseV1,
        convergence: Option<crate::global_db::schema_stages::RegisteredSchemaConvergence>,
    ) {
        let shard_id = database.binding().shard_id.clone();
        {
            let mut statuses = lock_registered_schema_convergence_statuses(&self.statuses);
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
            .expect("registered schema convergence test gate lock remains healthy")
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
            lock_registered_schema_convergence_statuses(&statuses).insert(task_shard_id, status);
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
            .expect("registered schema convergence test gate lock remains healthy") =
            Some(Arc::clone(&state));
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
        self.long_lived_session_maintenance
    }

    pub(super) async fn attach_registered(
        &self,
        runtime: StoreRuntimeClientLease,
        _operation: &'static str,
    ) -> Result<RegisteredGlobalDbOwnerV1> {
        let database = Database::publish_runtime(runtime, DatabaseAccessMode::ReadWrite).await?;
        let long_lived = self.long_lived_session_maintenance();
        let (database, convergence) = if long_lived {
            let (database, convergence) =
                RegisteredGlobalDbOwnerV1::admit_and_attach_for_daemon(database).await?;
            (database, Some(convergence))
        } else {
            (
                RegisteredGlobalDbOwnerV1::admit_and_attach(database).await?,
                None,
            )
        };
        if long_lived {
            let lease = database.issue_lease().map_err(|error| {
                session_registry_error(
                    "issue registered schema convergence client",
                    format!("{error:?}"),
                )
            })?;
            self.registered_schema_convergence
                .schedule(lease, convergence);
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

#[cfg(test)]
mod tests {
    use tracedecay_domain::{BrainId, UserProfileId};

    use super::*;

    #[test]
    fn poisoned_status_lock_recovers_once() {
        let maintenance = RegisteredSchemaConvergenceMaintenance::new();
        let statuses = Arc::clone(&maintenance.statuses);
        let poison = std::thread::spawn(move || {
            let _guard = statuses
                .lock()
                .expect("registered schema convergence status lock starts healthy");
            panic!("poison registered schema convergence status lock");
        });
        assert!(poison.join().is_err());
        assert!(maintenance.statuses.is_poisoned());

        let shard_id = StoreShardIdV1::profile_sessions(
            BrainId::try_from("brain.schema-convergence".to_owned())
                .expect("canonical brain identity"),
            UserProfileId::try_from("profile.schema-convergence".to_owned())
                .expect("canonical profile identity"),
        );
        maintenance.defer(shard_id.clone());

        assert!(!maintenance.statuses.is_poisoned());
        assert_eq!(
            maintenance.status(&shard_id),
            Some(RegisteredSchemaConvergenceStatus::Pending)
        );
    }
}
