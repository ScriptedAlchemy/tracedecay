use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};

#[cfg(any(test, feature = "test-helpers"))]
use tokio::sync::Notify;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tracedecay_store::StoreShardIdV1;

use super::{
    DaemonSessionRuntimeRegistryV1, Database, DatabaseAccessMode, RegisteredGlobalDbLeaseV1,
    RegisteredGlobalDbOwnerV1, Result, StoreRuntimeClientLease, release_process_allocator_memory,
    session_registry_error,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegisteredSchemaConvergenceStatus {
    Pending,
    Running,
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
            crate::session_registry::log_store_runtime_event(
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
    accepting: AtomicBool,
    foreground_project_opens: Arc<ForegroundProjectOpenState>,
    concurrency: Arc<Semaphore>,
    statuses: Arc<StdMutex<RegisteredSchemaConvergenceStatuses>>,
    tasks: StdMutex<BTreeMap<StoreShardIdV1, JoinHandle<()>>>,
    #[cfg(any(test, feature = "test-helpers"))]
    schedule_count: std::sync::atomic::AtomicUsize,
    #[cfg(any(test, feature = "test-helpers"))]
    execution_count: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(any(test, feature = "test-helpers"))]
    gate: StdMutex<Option<Arc<RegisteredSchemaConvergenceTestGateState>>>,
}

#[derive(Default)]
struct ForegroundProjectOpenState {
    active: AtomicUsize,
    settled: tokio::sync::Notify,
}

pub struct ForegroundProjectOpenAdmission {
    state: Arc<ForegroundProjectOpenState>,
}

impl Drop for ForegroundProjectOpenAdmission {
    fn drop(&mut self) {
        if self.state.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.state.settled.notify_waiters();
        }
    }
}

impl ForegroundProjectOpenState {
    fn admit(self: &Arc<Self>) -> Result<ForegroundProjectOpenAdmission> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1)
            })
            .map_err(|_| {
                session_registry_error(
                    "admit foreground project open",
                    "foreground project-open admission counter exhausted".to_owned(),
                )
            })?;
        Ok(ForegroundProjectOpenAdmission {
            state: Arc::clone(self),
        })
    }

    #[hotpath::skip]
    async fn wait_until_settled(&self) {
        loop {
            let settled = self.settled.notified();
            if self.active.load(Ordering::Acquire) == 0 {
                return;
            }
            settled.await;
        }
    }
}

impl RegisteredSchemaConvergenceMaintenance {
    pub(super) fn new() -> Self {
        Self {
            accepting: AtomicBool::new(true),
            foreground_project_opens: Arc::new(ForegroundProjectOpenState::default()),
            // Convergence is paging and allocation heavy. One permit prevents
            // multiple shards from competing with each other and LCM
            // retention while ordinary per-shard reads and writes stay live.
            concurrency: Arc::new(Semaphore::new(1)),
            statuses: Arc::new(StdMutex::new(BTreeMap::new())),
            tasks: StdMutex::new(BTreeMap::new()),
            #[cfg(any(test, feature = "test-helpers"))]
            schedule_count: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(any(test, feature = "test-helpers"))]
            execution_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(any(test, feature = "test-helpers"))]
            gate: StdMutex::new(None),
        }
    }

    fn begin_foreground_project_open(&self) -> Result<ForegroundProjectOpenAdmission> {
        self.foreground_project_opens.admit()
    }

    #[cfg(any(test, feature = "test-helpers"))]
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
        convergence: Option<tracedecay_global_db::schema_stages::RegisteredSchemaConvergence>,
    ) {
        let shard_id = database.binding().shard_id.clone();
        let mut tasks = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.accepting.load(Ordering::Acquire) {
            return;
        }
        {
            let mut statuses = lock_registered_schema_convergence_statuses(&self.statuses);
            if statuses.contains_key(&shard_id) {
                return;
            }
            statuses.insert(shard_id.clone(), RegisteredSchemaConvergenceStatus::Pending);
        }
        #[cfg(any(test, feature = "test-helpers"))]
        self.schedule_count.fetch_add(1, Ordering::Relaxed);
        #[cfg(any(test, feature = "test-helpers"))]
        let gate = self
            .gate
            .lock()
            .expect("registered schema convergence test gate lock remains healthy")
            .clone();
        let statuses = Arc::clone(&self.statuses);
        let foreground_project_opens = Arc::clone(&self.foreground_project_opens);
        let concurrency = Arc::clone(&self.concurrency);
        #[cfg(any(test, feature = "test-helpers"))]
        let execution_count = Arc::clone(&self.execution_count);
        let task_shard_id = shard_id.clone();
        let task = tokio::spawn(hotpath::future!(
            async move {
                foreground_project_opens.wait_until_settled().await;
                let permit = match concurrency.acquire_owned().await {
                    Ok(permit) => permit,
                    Err(error) => {
                        lock_registered_schema_convergence_statuses(&statuses).insert(
                            task_shard_id,
                            RegisteredSchemaConvergenceStatus::Degraded {
                                message: format!(
                                    "registered schema convergence admission closed: {error}"
                                ),
                            },
                        );
                        return;
                    }
                };
                lock_registered_schema_convergence_statuses(&statuses).insert(
                    task_shard_id.clone(),
                    RegisteredSchemaConvergenceStatus::Running,
                );
                #[cfg(any(test, feature = "test-helpers"))]
                execution_count.fetch_add(1, Ordering::Relaxed);
                #[cfg(any(test, feature = "test-helpers"))]
                if let Some(gate) = gate {
                    gate.block().await;
                }
                let result = match convergence {
                    Some(convergence) => database.converge_schema(convergence).await,
                    None => Ok(()),
                };
                if let Err(error) = database.release_connection_memory().await {
                    crate::session_registry::log_store_runtime_event(
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
                drop(permit);
                let status = match result {
                    Ok(()) => {
                        crate::session_registry::log_store_runtime_event(
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
                        crate::session_registry::log_store_runtime_event(
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
                lock_registered_schema_convergence_statuses(&statuses)
                    .insert(task_shard_id, status);
            },
            label = "daemon.session_registry.schema_converge"
        ));
        tasks.insert(shard_id, task);
    }

    pub(super) fn begin_shutdown(&self) {
        self.accepting.store(false, Ordering::Release);
        let tasks = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for task in tasks.values() {
            task.abort();
        }
    }

    #[hotpath::skip]
    pub(super) async fn shutdown(&self) -> std::result::Result<(), String> {
        self.accepting.store(false, Ordering::Release);
        let tasks = {
            let mut tasks = self
                .tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let tasks = std::mem::take(&mut *tasks);
            for task in tasks.values() {
                task.abort();
            }
            tasks
        };
        let mut failures = Vec::new();
        for (shard_id, task) in tasks {
            if let Err(error) = task.await
                && !error.is_cancelled()
            {
                failures.push(format!(
                    "registered schema convergence task {shard_id:?} join failed: {error}"
                ));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    #[cfg(any(test, feature = "test-helpers"))]
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
        self.accepting.store(false, Ordering::Release);
        let tasks = self
            .tasks
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (_, task) in std::mem::take(tasks) {
            task.abort();
        }
    }
}

#[cfg(any(test, feature = "test-helpers"))]
pub(super) struct RegisteredSchemaConvergenceTestGateState {
    started: AtomicBool,
    started_notify: Notify,
    release: Semaphore,
}

#[cfg(any(test, feature = "test-helpers"))]
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

#[cfg(any(test, feature = "test-helpers"))]
pub struct RegisteredSchemaConvergenceTestGate {
    state: Arc<RegisteredSchemaConvergenceTestGateState>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl RegisteredSchemaConvergenceTestGate {
    pub async fn wait_until_blocked(&self) {
        while !self.state.started.load(Ordering::Acquire) {
            self.state.started_notify.notified().await;
        }
    }

    pub fn release(&self) {
        self.state.release.add_permits(1);
    }
}

impl DaemonSessionRuntimeRegistryV1 {
    fn long_lived_session_maintenance(&self) -> bool {
        self.long_lived_session_maintenance
    }

    #[hotpath::measure(label = "daemon.session_registry.attach_registered", future = true)]
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

    pub fn begin_foreground_project_open(&self) -> Result<ForegroundProjectOpenAdmission> {
        self.registered_schema_convergence
            .begin_foreground_project_open()
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn registered_schema_convergence_status(
        &self,
        shard_id: &StoreShardIdV1,
    ) -> Option<RegisteredSchemaConvergenceStatus> {
        self.registered_schema_convergence.status(shard_id)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn block_registered_schema_convergence_for_test(
        &self,
    ) -> RegisteredSchemaConvergenceTestGate {
        self.registered_schema_convergence.install_gate()
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn registered_schema_convergence_schedule_count_for_test(&self) -> usize {
        self.registered_schema_convergence
            .schedule_count
            .load(Ordering::Relaxed)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn registered_schema_convergence_execution_count_for_test(&self) -> usize {
        self.registered_schema_convergence
            .execution_count
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
