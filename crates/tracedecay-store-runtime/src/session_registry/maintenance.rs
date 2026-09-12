use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};

#[cfg(any(test, feature = "test-helpers"))]
use tokio::sync::Notify;
use tokio::sync::Semaphore;
use tracedecay_store::StoreShardIdV1;

use super::retained_hook_tasks::RetainedHookTaskJoin;

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
    tasks: StdMutex<BTreeMap<StoreShardIdV1, Arc<RetainedHookTaskJoin>>>,
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

    /// Every shard whose historical convergence has not completed.
    ///
    /// Convergence carries the migrations whose cost scales with the store, so
    /// on a large store it can be pending, running, or degraded for a long
    /// while after the daemon starts serving. Reporting that is what makes the
    /// daemon's own answer honest: an operator sees which shard is still
    /// migrating and why, instead of a healthy claim or a crash loop.
    pub(super) fn unconverged(&self) -> Vec<(StoreShardIdV1, RegisteredSchemaConvergenceStatus)> {
        lock_registered_schema_convergence_statuses(&self.statuses)
            .iter()
            .filter(|(_, status)| **status != RegisteredSchemaConvergenceStatus::Complete)
            .map(|(shard, status)| (shard.clone(), status.clone()))
            .collect()
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
        // Erase the worker state machine before instrumentation retains it.
        let work: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
            foreground_project_opens.wait_until_settled().await;
            let permit = match concurrency.acquire_owned().await {
                Ok(permit) => permit,
                Err(error) => {
                    drop(database);
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
                Some(convergence) => {
                    let convergence: Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> =
                        Box::pin(database.converge_schema(convergence));
                    convergence.await
                }
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
            drop(database);
            lock_registered_schema_convergence_statuses(&statuses).insert(task_shard_id, status);
        });
        let task = tokio::spawn(hotpath::future!(
            work,
            label = "daemon.session_registry.schema_converge"
        ));
        tasks.insert(shard_id, Arc::new(RetainedHookTaskJoin::new(task)));
    }

    /// Keep an aborted worker tracked until its future has dropped every client.
    /// Already accepted blocking SQLite work is fenced separately by canonical
    /// physical retirement: reader-pool quiescence includes outstanding pool
    /// references, and writer shutdown joins its actor before replacement.
    /// Cancellation of this join leaves the handle available for the next retry.
    #[hotpath::skip]
    pub(super) async fn retire(
        &self,
        shard_id: &StoreShardIdV1,
    ) -> std::result::Result<(), String> {
        let task = {
            let tasks = self
                .tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(task) = tasks.get(shard_id) else {
                return Ok(());
            };
            Arc::clone(task)
        };
        task.abort();
        task.wait().await.map_err(|error| {
            format!("registered schema convergence task {shard_id:?} join failed: {error}")
        })?;
        let mut tasks = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if tasks
            .get(shard_id)
            .is_some_and(|retained| Arc::ptr_eq(retained, &task))
        {
            tasks.remove(shard_id);
            lock_registered_schema_convergence_statuses(&self.statuses).remove(shard_id);
        }
        Ok(())
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
        self.begin_shutdown();
        let tasks = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut failures = Vec::new();
        // Shared joins remain in the owner while any shutdown waiter can be
        // cancelled. Keep failed joins so subsequent shutdown reports them too.
        for (shard_id, task) in tasks {
            if let Err(error) = task.wait().await {
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
    #[hotpath::measure(label = "daemon.session_registry.attach_registered", future = true)]
    pub(super) async fn attach_registered(
        &self,
        runtime: StoreRuntimeClientLease,
        _operation: &'static str,
    ) -> Result<RegisteredGlobalDbOwnerV1> {
        self.attach_registered_inner(runtime).await
    }

    // Erase the inner state machine before Hotpath wraps it by value. Boxing
    // only the caller leaves large temporaries in the measured poll frame.
    fn attach_registered_inner(
        &self,
        runtime: StoreRuntimeClientLease,
    ) -> Pin<Box<dyn Future<Output = Result<RegisteredGlobalDbOwnerV1>> + Send + '_>> {
        Box::pin(async move {
            let database =
                Database::publish_runtime(runtime, DatabaseAccessMode::ReadWrite).await?;
            let long_lived = self.long_lived_session_maintenance;
            // Every registry mode shares terminal graph-operation ownership; only
            // long-lived daemons defer schema convergence to resumable maintenance.
            let (database, convergence) = if long_lived {
                let (database, convergence) =
                    RegisteredGlobalDbOwnerV1::admit_and_attach_for_daemon(
                        database,
                        Arc::clone(&self.semantic_vector_operation_task_owner),
                    )
                    .await?;
                (database, Some(convergence))
            } else {
                (
                    RegisteredGlobalDbOwnerV1::admit_and_attach_with_operation_task_owner(
                        database,
                        Arc::clone(&self.semantic_vector_operation_task_owner),
                    )
                    .await?,
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
        })
    }

    pub fn begin_foreground_project_open(&self) -> Result<ForegroundProjectOpenAdmission> {
        self.registered_schema_convergence
            .begin_foreground_project_open()
    }

    /// Shards whose historical schema convergence has not completed, with the
    /// state each one is in. Empty once every mounted shard is converged.
    #[must_use]
    pub fn unconverged_registered_schemas(
        &self,
    ) -> Vec<(StoreShardIdV1, RegisteredSchemaConvergenceStatus)> {
        self.registered_schema_convergence.unconverged()
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_shutdown_retains_incomplete_future_drop_and_independent_retirement() {
        struct HeldDrop {
            started: Arc<Notify>,
            release: Arc<(StdMutex<bool>, std::sync::Condvar)>,
            dropped: Arc<AtomicBool>,
        }
        impl Drop for HeldDrop {
            fn drop(&mut self) {
                self.started.notify_one();
                let (lock, ready) = &*self.release;
                let mut released = lock.lock().expect("drop gate");
                while !*released {
                    released = ready.wait(released).expect("drop gate wait");
                }
                self.dropped.store(true, Ordering::Release);
            }
        }
        // Always release the held destructor, including when an assertion fails.
        struct ReleaseOnDrop(Arc<(StdMutex<bool>, std::sync::Condvar)>);
        impl Drop for ReleaseOnDrop {
            fn drop(&mut self) {
                let (lock, ready) = &*self.0;
                *lock.lock().expect("release drop gate") = true;
                ready.notify_all();
            }
        }
        let maintenance = RegisteredSchemaConvergenceMaintenance::new();
        let shard = |profile: &str| {
            StoreShardIdV1::profile_sessions(
                BrainId::try_from("brain.schema-convergence".to_owned()).unwrap(),
                UserProfileId::try_from(profile.to_owned()).unwrap(),
            )
        };
        let first = shard("profile.first");
        let second = shard("profile.second");
        let started = Arc::new(Notify::new());
        let dropping = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let release = Arc::new((StdMutex::new(false), std::sync::Condvar::new()));
        let release_on_drop = ReleaseOnDrop(Arc::clone(&release));
        let guard = HeldDrop {
            started: Arc::clone(&dropping),
            release,
            dropped: Arc::clone(&dropped),
        };
        let worker = tokio::spawn({
            let started = Arc::clone(&started);
            async move {
                let _guard = guard;
                started.notify_one();
                std::future::pending::<()>().await;
            }
        });
        maintenance
            .tasks
            .lock()
            .unwrap()
            .insert(first.clone(), Arc::new(RetainedHookTaskJoin::new(worker)));
        maintenance.tasks.lock().unwrap().insert(
            second.clone(),
            Arc::new(RetainedHookTaskJoin::new(tokio::spawn(
                std::future::pending::<()>(),
            ))),
        );
        started.notified().await;
        let mut shutdown = Box::pin(maintenance.shutdown());
        std::future::poll_fn(|cx| {
            assert!(shutdown.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        dropping.notified().await;
        drop(shutdown);
        let mut retry = Box::pin(maintenance.shutdown());
        std::future::poll_fn(|cx| {
            assert!(
                retry.as_mut().poll(cx).is_pending(),
                "retry must join the held destructor"
            );
            std::task::Poll::Ready(())
        })
        .await;
        assert!(!dropped.load(Ordering::Acquire));
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            maintenance.retire(&second),
        )
        .await
        .expect("second shard retires independently")
        .expect("second retirement");
        assert!(!dropped.load(Ordering::Acquire));
        drop(release_on_drop);
        retry.await.expect("retry joins released destructor");
        assert!(dropped.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn shutdown_and_exact_retirement_preserve_failed_join() {
        let maintenance = RegisteredSchemaConvergenceMaintenance::new();
        let shard = StoreShardIdV1::profile_sessions(
            BrainId::try_from("brain.schema-convergence".to_owned()).unwrap(),
            UserProfileId::try_from("profile.failed".to_owned()).unwrap(),
        );
        let started = Arc::new(Notify::new());
        let worker = tokio::spawn({
            let started = Arc::clone(&started);
            async move {
                started.notify_one();
                panic!("convergence task failure");
            }
        });
        maintenance
            .tasks
            .lock()
            .unwrap()
            .insert(shard.clone(), Arc::new(RetainedHookTaskJoin::new(worker)));
        started.notified().await;
        let failure = maintenance
            .retire(&shard)
            .await
            .expect_err("failed task retirement");
        assert_eq!(maintenance.retire(&shard).await.unwrap_err(), failure);
        assert_eq!(maintenance.shutdown().await.unwrap_err(), failure);
    }

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
