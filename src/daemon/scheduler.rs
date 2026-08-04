use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};

use crate::client_identity::DaemonClientIdentity;
use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;

use super::branch_admin::MaintenanceReaperKind;
use super::{
    DAEMON_TASK_ABORT_DEADLINE, DaemonEngine, DaemonHandshake, ProjectServerKey, log_daemon_event,
};

pub(super) fn scheduler_task_log_fields(
    project_path: &Path,
    task: crate::automation::backend::AgentTaskKind,
    outcome: &str,
) -> Vec<(&'static str, String)> {
    vec![
        ("project", project_path.display().to_string()),
        (
            "task",
            crate::automation::backend::task_key(task).to_string(),
        ),
        ("outcome", outcome.to_string()),
    ]
}

fn log_scheduler_task_start(project_path: &Path, task: crate::automation::backend::AgentTaskKind) {
    log_daemon_event(
        "scheduler_task",
        &scheduler_task_log_fields(project_path, task, "start"),
    );
}

fn scheduler_task_error_log_fields(
    project_path: &Path,
    task: crate::automation::backend::AgentTaskKind,
    error: &TraceDecayError,
) -> Vec<(&'static str, String)> {
    vec![
        ("project", project_path.display().to_string()),
        (
            "task",
            crate::automation::backend::task_key(task).to_string(),
        ),
        ("error", error.to_string()),
    ]
}

fn log_scheduler_task_error(
    project_path: &Path,
    task: crate::automation::backend::AgentTaskKind,
    error: &TraceDecayError,
) {
    log_daemon_event(
        "scheduler_task_error",
        &scheduler_task_error_log_fields(project_path, task, error),
    );
}

fn scheduler_record_log_fields(
    project_path: &Path,
    record: &crate::automation::run_ledger::AutomationRunLedgerRecord,
) -> Vec<(&'static str, String)> {
    use crate::automation::run_ledger::AutomationRunStatus;

    let outcome = match record.status {
        AutomationRunStatus::Succeeded => "complete",
        AutomationRunStatus::Failed => "error",
        AutomationRunStatus::Skipped => "skipped",
        AutomationRunStatus::Queued => "queued",
        AutomationRunStatus::Running => "running",
    };
    let task = record
        .task_key
        .as_deref()
        .unwrap_or_else(|| crate::automation::backend::task_key(record.task))
        .to_string();
    let mut fields = vec![
        ("project", project_path.display().to_string()),
        ("task", task),
        ("outcome", outcome.to_string()),
        ("run_id", record.run_id.clone()),
    ];
    if let Some(reason) = record.fallback_status.as_ref().or(record.error.as_ref()) {
        fields.push(("reason", reason.clone()));
    }
    fields
}

#[cfg(test)]
pub(super) fn daemon_scheduler_record_log_line(
    project_path: &Path,
    record: &crate::automation::run_ledger::AutomationRunLedgerRecord,
) -> String {
    super::format_daemon_log_line(
        "scheduler_task",
        &scheduler_record_log_fields(project_path, record),
    )
}

fn log_daemon_scheduler_record(
    project_path: &Path,
    record: &crate::automation::run_ledger::AutomationRunLedgerRecord,
) {
    log_daemon_event(
        "scheduler_task",
        &scheduler_record_log_fields(project_path, record),
    );
}

pub(super) fn automation_staged_log_fields(
    project_path: &Path,
    counts: &crate::automation::staged_notice::AutomationPendingCounts,
) -> Vec<(&'static str, String)> {
    // `unreadable` rather than `0`: this line is the operator's record of what
    // was awaiting human review, and a failed read is not an empty queue.
    let field = |count: &crate::automation::staged_notice::PendingReviewCount| {
        count
            .count()
            .map_or_else(|| "unreadable".to_string(), |count| count.to_string())
    };
    vec![
        ("project", project_path.display().to_string()),
        ("pending_fact_proposals", field(&counts.fact_proposals)),
        ("pending_skills", field(&counts.skills)),
    ]
}

/// After a scheduler tick where at least one task completed, emit a stable
/// `event=automation_staged` line with pending automation review counts.
/// Silent only when every queue was read and every one is empty; a queue that
/// could not be read is logged as `unreadable`, never omitted.
async fn log_automation_staged_if_pending(project_path: &Path, cg: &TraceDecay) {
    let Ok(profile_root) = crate::storage::default_profile_root() else {
        return;
    };
    let Ok(owner) = cg.project_memory_owner() else {
        return;
    };
    let Ok(memory_db) = cg.open_project_store_db().await else {
        return;
    };
    let Ok(memory) = crate::tracedecay::facts::memory_application_for_db(owner, &memory_db) else {
        return;
    };
    let counts =
        crate::automation::staged_notice::count_pending_automation_output(&memory, &profile_root)
            .await;
    if counts.is_verified_empty() {
        return;
    }
    log_daemon_event(
        "automation_staged",
        &automation_staged_log_fields(project_path, &counts),
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AutomationSchedulerLifecycle {
    Running,
    Exiting,
    Finished,
    Retiring,
}

pub(super) struct AutomationSchedulerHandle {
    pub(super) task: Option<JoinHandle<()>>,
    pub(super) wake: Arc<tokio::sync::Notify>,
    pub(super) completion: Arc<()>,
    pub(super) generation: Arc<std::sync::atomic::AtomicU64>,
    pub(super) lifecycle: AutomationSchedulerLifecycle,
    termination: Arc<MaintenanceTaskTermination>,
}

#[cfg(test)]
impl AutomationSchedulerHandle {
    pub(super) fn for_test(task: JoinHandle<()>) -> Self {
        Self {
            task: Some(task),
            wake: Arc::new(tokio::sync::Notify::new()),
            completion: Arc::new(()),
            generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            lifecycle: AutomationSchedulerLifecycle::Running,
            termination: Arc::new(MaintenanceTaskTermination::pending()),
        }
    }
}

pub(super) struct MaintenanceTaskTermination {
    finished: tokio::sync::watch::Sender<bool>,
}

impl MaintenanceTaskTermination {
    pub(super) fn pending() -> Self {
        let (finished, _) = tokio::sync::watch::channel(false);
        Self { finished }
    }

    pub(super) async fn wait(&self) {
        self.wait_for_finish(self.finished.subscribe()).await;
    }

    async fn wait_for_finish(&self, mut finished: tokio::sync::watch::Receiver<bool>) {
        while !*finished.borrow_and_update() {
            if finished.changed().await.is_err() {
                return;
            }
        }
    }

    pub(super) fn finish(&self) {
        self.finished.send_replace(true);
    }
}

pub(super) struct AutomationSchedulerRetirement {
    termination: Arc<MaintenanceTaskTermination>,
}

impl AutomationSchedulerRetirement {
    pub(super) async fn wait(self) {
        self.termination.wait().await;
    }
}

#[cfg(test)]
mod maintenance_task_termination_tests {
    use std::time::Duration;

    use tokio::time::timeout;

    use super::MaintenanceTaskTermination;

    #[tokio::test(start_paused = true)]
    async fn finish_before_wait_is_latched() {
        let termination = MaintenanceTaskTermination::pending();
        termination.finish();

        timeout(Duration::from_secs(1), termination.wait())
            .await
            .expect("finish before wait must not hang");
    }

    #[tokio::test(start_paused = true)]
    async fn finish_between_registration_and_condition_check_is_latched() {
        let termination = MaintenanceTaskTermination::pending();
        let registered = termination.finished.subscribe();
        termination.finish();

        timeout(
            Duration::from_secs(1),
            termination.wait_for_finish(registered),
        )
        .await
        .expect("finish after waiter registration must not hang");
    }
}

#[cfg(test)]
pub(super) struct AutomationSchedulerExitBarrier {
    state: tokio::sync::watch::Sender<u8>,
}

#[cfg(test)]
impl AutomationSchedulerExitBarrier {
    const UNDECIDED: u8 = 0;
    pub(super) const EXIT: u8 = 1;
    pub(super) const CONTINUE: u8 = 2;
    const DECISION_MASK: u8 = Self::EXIT | Self::CONTINUE;
    const REACHED: u8 = 1 << 2;
    const RELEASED: u8 = 1 << 3;

    pub(super) fn new() -> Self {
        let (state, _) = tokio::sync::watch::channel(Self::UNDECIDED);
        Self { state }
    }

    async fn pause_after_disabled_read(&self) {
        self.state.send_modify(|state| *state |= Self::REACHED);
        self.wait_for(|state| state & Self::RELEASED != 0).await;
    }

    fn record_decision(&self, decision: u8) {
        debug_assert!(matches!(decision, Self::EXIT | Self::CONTINUE));
        self.state
            .send_modify(|state| *state = (*state & !Self::DECISION_MASK) | decision);
    }

    pub(super) async fn wait_until_reached(&self) {
        self.wait_for(|state| state & Self::REACHED != 0).await;
    }

    pub(super) fn release(&self) {
        self.state.send_modify(|state| *state |= Self::RELEASED);
    }

    pub(super) async fn wait_for_decision(&self) -> u8 {
        self.wait_for(|state| state & Self::DECISION_MASK != Self::UNDECIDED)
            .await
            & Self::DECISION_MASK
    }

    async fn wait_for(&self, ready: impl Fn(u8) -> bool) -> u8 {
        let mut state = self.state.subscribe();
        loop {
            let current = *state.borrow_and_update();
            if ready(current) {
                return current;
            }
            if state.changed().await.is_err() {
                return *state.borrow();
            }
        }
    }
}

#[cfg(test)]
mod automation_scheduler_exit_barrier_tests {
    use std::time::Duration;

    use tokio::time::timeout;

    use super::AutomationSchedulerExitBarrier;

    #[tokio::test(start_paused = true)]
    async fn preserves_release_and_decision_conditions() {
        let barrier = AutomationSchedulerExitBarrier::new();

        barrier.release();
        timeout(Duration::from_secs(1), barrier.pause_after_disabled_read())
            .await
            .expect("release sent before the pause must be observed");
        timeout(Duration::from_secs(1), barrier.wait_until_reached())
            .await
            .expect("reached state must survive release");

        barrier.record_decision(AutomationSchedulerExitBarrier::CONTINUE);
        assert_eq!(
            timeout(Duration::from_secs(1), barrier.wait_for_decision())
                .await
                .expect("recorded decision must be observed"),
            AutomationSchedulerExitBarrier::CONTINUE
        );
        timeout(Duration::from_secs(1), barrier.wait_until_reached())
            .await
            .expect("reached state must survive the decision");
    }
}

impl DaemonEngine {
    pub(super) async fn activate_automation_scheduler_for_open_project(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
        cg: Arc<crate::tracedecay::TraceDecay>,
    ) {
        if !self.lifecycle.accepting() {
            return;
        }

        #[cfg(test)]
        self.automation_config_probe_attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let configured = match async {
            let config =
                effective_automation_config_for_project(&cg, &handshake.client_identity).await?;
            automation_scheduler_has_work(&cg, &config).await
        }
        .await
        {
            Ok(configured) => configured,
            Err(error) => {
                log_daemon_event(
                    "scheduler_config",
                    &[
                        ("project", project_path.display().to_string()),
                        ("outcome", "error".to_string()),
                        ("error", error.to_string()),
                    ],
                );
                false
            }
        };
        if !configured {
            log_daemon_event(
                "scheduler_config",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "skipped".to_string()),
                    ("reason", "not_configured".to_string()),
                ],
            );
            return;
        }

        let transition = self.maintenance_transition_gate(&key).await;
        let _transition = transition.lock().await;
        let scope = super::branch_admin::owner_writer_scope(&key);
        self.store_administration
            .with_writer_in(scope, || async move {
                if !self.lifecycle.accepting() {
                    return;
                }
                let owner_is_current = self
                    .store_administration
                    .project_servers()
                    .lock()
                    .await
                    .get(&key)
                    .is_some();
                if !owner_is_current {
                    return;
                }
                self.start_automation_scheduler(key, project_path, handshake, cg)
                    .await;
            })
            .await;
    }

    pub(super) async fn ensure_automation_scheduler(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
    ) -> crate::dashboard::AutomationSchedulerReconcileOutcome {
        let transition = self.maintenance_transition_gate(&key).await;
        let _transition = transition.lock().await;
        let scope = super::branch_admin::owner_writer_scope(&key);
        self.store_administration
            .with_writer_in(scope, || async move {
                self.reconcile_automation_scheduler_locked(key, project_path, handshake)
                    .await
            })
            .await
    }

    pub(super) async fn reconcile_automation_scheduler_locked(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
    ) -> crate::dashboard::AutomationSchedulerReconcileOutcome {
        use crate::dashboard::AutomationSchedulerReconcileOutcome;

        if !self.lifecycle.accepting() {
            return AutomationSchedulerReconcileOutcome::LifecycleInactive;
        }
        let (finished, live) = {
            let mut schedulers = self
                .store_administration
                .automation_schedulers()
                .lock()
                .await;
            let logical_owner = schedulers
                .iter()
                .find(|(candidate, _)| same_scheduler_owner(candidate, &key))
                .map(|(candidate, _)| candidate.clone());
            match logical_owner {
                Some(owner) => {
                    let Some(handle) = schedulers.get_mut(&owner) else {
                        return AutomationSchedulerReconcileOutcome::OwnerUnavailable;
                    };
                    match observed_scheduler_lifecycle(handle) {
                        lifecycle @ (AutomationSchedulerLifecycle::Running
                        | AutomationSchedulerLifecycle::Exiting) => (
                            None,
                            Some((owner, Arc::clone(&handle.completion), lifecycle)),
                        ),
                        AutomationSchedulerLifecycle::Finished => (schedulers.remove(&owner), None),
                        AutomationSchedulerLifecycle::Retiring => {
                            return AutomationSchedulerReconcileOutcome::Retiring;
                        }
                    }
                }
                None => (None, None),
            }
        };
        if let Some(mut handle) = finished
            && let Some(task) = handle.task.take()
        {
            let _ = task.await;
        }

        #[cfg(test)]
        self.automation_config_probe_attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        #[cfg(test)]
        let forced_configured = self
            .automation_configured_override
            .load(std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(test))]
        let forced_configured = false;
        let configured = if forced_configured {
            true
        } else {
            let Some(cg) = retained_project_graph(self, &key).await else {
                return AutomationSchedulerReconcileOutcome::OwnerUnavailable;
            };
            match automation_scheduler_has_work_for_project(&cg, &handshake).await {
                Ok(configured) => configured,
                Err(e) => {
                    log_daemon_event(
                        "scheduler_config",
                        &[
                            ("project", project_path.display().to_string()),
                            ("outcome", "error".to_string()),
                            ("error", e.to_string()),
                        ],
                    );
                    return AutomationSchedulerReconcileOutcome::NotConfigured;
                }
            }
        };
        if !configured {
            if let Some((owner, completion, _)) = &live {
                let schedulers = self
                    .store_administration
                    .automation_schedulers()
                    .lock()
                    .await;
                if let Some(handle) = schedulers.get(owner)
                    && Arc::ptr_eq(&handle.completion, completion)
                {
                    handle.wake.notify_one();
                }
            }
            log_daemon_event(
                "scheduler_config",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "skipped".to_string()),
                    ("reason", "not_configured".to_string()),
                ],
            );
            return AutomationSchedulerReconcileOutcome::NotConfigured;
        }

        if let Some((owner, completion, lifecycle)) = live {
            let finished = {
                let mut schedulers = self
                    .store_administration
                    .automation_schedulers()
                    .lock()
                    .await;
                if let Some(handle) = schedulers.get_mut(&owner)
                    && Arc::ptr_eq(&handle.completion, &completion)
                {
                    match observed_scheduler_lifecycle(handle) {
                        AutomationSchedulerLifecycle::Running => {
                            handle
                                .generation
                                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                            handle.wake.notify_one();
                            return AutomationSchedulerReconcileOutcome::RunningNotified;
                        }
                        AutomationSchedulerLifecycle::Exiting => {
                            handle
                                .generation
                                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                            handle.wake.notify_one();
                            return AutomationSchedulerReconcileOutcome::Exiting;
                        }
                        AutomationSchedulerLifecycle::Finished => schedulers.remove(&owner),
                        AutomationSchedulerLifecycle::Retiring => {
                            return AutomationSchedulerReconcileOutcome::Retiring;
                        }
                    }
                } else {
                    None
                }
            };
            if let Some(mut handle) = finished
                && let Some(task) = handle.task.take()
            {
                let _ = task.await;
            }
            debug_assert!(matches!(
                lifecycle,
                AutomationSchedulerLifecycle::Running | AutomationSchedulerLifecycle::Exiting
            ));
        }

        let Some(cg) = retained_project_graph(self, &key).await else {
            return AutomationSchedulerReconcileOutcome::OwnerUnavailable;
        };
        self.start_automation_scheduler(key, project_path, handshake, cg)
            .await
    }

    pub(super) fn automation_scheduler_reconciler(
        &self,
        current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
        current_project_path: Arc<tokio::sync::Mutex<PathBuf>>,
        handshake: DaemonHandshake,
    ) -> crate::dashboard::AutomationSchedulerReconciler {
        let engine = self.clone();
        std::sync::Arc::new(move || {
            let engine = engine.clone();
            let current_key = Arc::clone(&current_key);
            let current_project_path = Arc::clone(&current_project_path);
            let handshake = handshake.clone();
            Box::pin(async move {
                let key = current_key.lock().await.clone();
                let project_path = current_project_path.lock().await.clone();
                engine
                    .ensure_automation_scheduler(key, project_path, handshake)
                    .await
            })
        })
    }

    pub(super) async fn start_automation_scheduler(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
        cg: Arc<TraceDecay>,
    ) -> crate::dashboard::AutomationSchedulerReconcileOutcome {
        use crate::dashboard::AutomationSchedulerReconcileOutcome;

        if !self.lifecycle.accepting() {
            return AutomationSchedulerReconcileOutcome::LifecycleInactive;
        }
        let mut schedulers = self
            .store_administration
            .automation_schedulers()
            .lock()
            .await;
        if !self.lifecycle.accepting() {
            return AutomationSchedulerReconcileOutcome::LifecycleInactive;
        }
        let logical_owner = schedulers
            .iter()
            .find(|(candidate, _)| same_scheduler_owner(candidate, &key))
            .map(|(candidate, _)| candidate.clone());
        if let Some(handle) = logical_owner
            .as_ref()
            .and_then(|owner| schedulers.get_mut(owner))
        {
            return match observed_scheduler_lifecycle(handle) {
                AutomationSchedulerLifecycle::Running => {
                    handle
                        .generation
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    handle.wake.notify_one();
                    AutomationSchedulerReconcileOutcome::RunningNotified
                }
                AutomationSchedulerLifecycle::Exiting => {
                    handle
                        .generation
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    handle.wake.notify_one();
                    AutomationSchedulerReconcileOutcome::Exiting
                }
                AutomationSchedulerLifecycle::Finished => {
                    AutomationSchedulerReconcileOutcome::Finished
                }
                AutomationSchedulerLifecycle::Retiring => {
                    AutomationSchedulerReconcileOutcome::Retiring
                }
            };
        }
        let wake = Arc::new(tokio::sync::Notify::new());
        let loop_wake = Arc::clone(&wake);
        let completion = Arc::new(());
        let completed = Arc::clone(&completion);
        let loop_completion = Arc::clone(&completion);
        let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let loop_generation = Arc::clone(&generation);
        let termination = Arc::new(MaintenanceTaskTermination::pending());
        let administration = self.store_administration.clone();
        let scheduler_engine = self.clone();
        let loop_key = key.clone();
        #[cfg(test)]
        let exit_barrier = self.automation_scheduler_exit_barrier.lock().await.clone();
        let (published, start) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = start.await;
            Box::pin(run_automation_scheduler_loop(
                project_path,
                handshake,
                cg,
                loop_wake,
                scheduler_engine,
                loop_key,
                loop_completion,
                loop_generation,
                #[cfg(test)]
                exit_barrier,
            ))
            .await;
            administration
                .automation_schedulers()
                .lock()
                .await
                .retain(|_, handle| {
                    !Arc::ptr_eq(&handle.completion, &completed)
                        || handle.lifecycle == AutomationSchedulerLifecycle::Retiring
                });
        });
        schedulers.insert(
            key,
            AutomationSchedulerHandle {
                task: Some(task),
                wake,
                completion,
                generation,
                lifecycle: AutomationSchedulerLifecycle::Running,
                termination,
            },
        );
        #[cfg(test)]
        self.automation_scheduler_state_changed.notify_waiters();
        let _ = published.send(());
        AutomationSchedulerReconcileOutcome::Started
    }

    async fn commit_automation_scheduler_exit(
        &self,
        key: &ProjectServerKey,
        project_path: &Path,
        handshake: &DaemonHandshake,
        cg: &TraceDecay,
        completion: &Arc<()>,
        observed_generation: u64,
    ) -> bool {
        let transition = self.maintenance_transition_gate(key).await;
        let _transition = transition.lock().await;
        let scope = super::branch_admin::owner_writer_scope(key);
        self.store_administration
            .with_writer_in(scope, || async {
                {
                    let mut schedulers = self
                        .store_administration
                        .automation_schedulers()
                        .lock()
                        .await;
                    let Some(handle) = schedulers.get_mut(key) else {
                        return true;
                    };
                    if !Arc::ptr_eq(&handle.completion, completion)
                        || handle.lifecycle == AutomationSchedulerLifecycle::Retiring
                    {
                        return true;
                    }
                    handle.lifecycle = AutomationSchedulerLifecycle::Exiting;
                }

                let still_configured =
                    match automation_scheduler_has_work_for_project(cg, handshake).await {
                        Ok(configured) => configured,
                        Err(error) => {
                            log_daemon_event(
                                "scheduler_project_open",
                                &[
                                    ("project", project_path.display().to_string()),
                                    ("outcome", "error".to_string()),
                                    ("error", error.to_string()),
                                ],
                            );
                            true
                        }
                    };
                let mut schedulers = self
                    .store_administration
                    .automation_schedulers()
                    .lock()
                    .await;
                let Some(handle) = schedulers.get_mut(key) else {
                    return true;
                };
                if !Arc::ptr_eq(&handle.completion, completion)
                    || handle.lifecycle == AutomationSchedulerLifecycle::Retiring
                {
                    return true;
                }
                if still_configured
                    || handle.generation.load(std::sync::atomic::Ordering::Acquire)
                        != observed_generation
                {
                    handle.lifecycle = AutomationSchedulerLifecycle::Running;
                    return false;
                }
                schedulers.remove(key);
                true
            })
            .await
    }

    pub(super) async fn retire_automation_scheduler_locked(
        &self,
        key: &ProjectServerKey,
    ) -> Option<AutomationSchedulerRetirement> {
        let reservation = self.store_administration.reserve_retirement_reaper()?;
        let (task, completion, termination) = {
            let mut schedulers = self
                .store_administration
                .automation_schedulers()
                .lock()
                .await;
            let owner = if schedulers.contains_key(key) {
                key.clone()
            } else {
                schedulers
                    .keys()
                    .find(|candidate| same_scheduler_owner(candidate, key))
                    .cloned()?
            };
            let handle = schedulers.get_mut(&owner)?;
            handle.lifecycle = AutomationSchedulerLifecycle::Retiring;
            handle
                .generation
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            let task = handle.task.take().map(|task| (owner, task));
            (
                task,
                Arc::clone(&handle.completion),
                Arc::clone(&handle.termination),
            )
        };
        #[cfg(test)]
        self.automation_scheduler_state_changed.notify_waiters();
        if let Some((owner, task)) = task {
            let completed = Arc::clone(&completion);
            let reaper_administration = self.store_administration.clone();
            let reaper_owner = owner.clone();
            #[cfg(test)]
            let state_changed = Arc::clone(&self.automation_scheduler_state_changed);
            self.store_administration.spawn_retirement_reaper(
                reservation,
                MaintenanceReaperKind::Automation,
                owner,
                task,
                Arc::clone(&termination),
                async move {
                    {
                        let mut schedulers =
                            reaper_administration.automation_schedulers().lock().await;
                        if schedulers.get(&reaper_owner).is_some_and(|handle| {
                            Arc::ptr_eq(&handle.completion, &completed)
                                && handle.lifecycle == AutomationSchedulerLifecycle::Retiring
                        }) {
                            schedulers.remove(&reaper_owner);
                        }
                    }
                    #[cfg(test)]
                    state_changed.notify_waiters();
                },
            );
        }
        Some(AutomationSchedulerRetirement { termination })
    }

    pub(super) async fn shutdown_automation_schedulers(&self) {
        // Draining is latched before this runs, and every registration path
        // rechecks that latch. Do not queue shutdown behind unrelated
        // migration/branch administration that may hold the broad writer gate.
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
        let _child_shutdown = crate::sessions::codex_app_server::begin_codex_app_server_shutdown();
        let _ = timeout(DAEMON_TASK_ABORT_DEADLINE, async {
            for retirement in retirements {
                retirement.wait().await;
            }
        })
        .await;
    }
}

pub(super) fn same_scheduler_owner(left: &ProjectServerKey, right: &ProjectServerKey) -> bool {
    left.owner.profile_root == right.owner.profile_root
        && left.owner.project_id == right.owner.project_id
        && left.scope_prefix == right.scope_prefix
}

fn observed_scheduler_lifecycle(
    handle: &AutomationSchedulerHandle,
) -> AutomationSchedulerLifecycle {
    if handle.task.as_ref().is_some_and(JoinHandle::is_finished) {
        AutomationSchedulerLifecycle::Finished
    } else {
        handle.lifecycle
    }
}

async fn retained_project_graph(
    engine: &DaemonEngine,
    key: &ProjectServerKey,
) -> Option<Arc<TraceDecay>> {
    let server = {
        let servers = engine.store_administration.project_servers().lock().await;
        servers.get(key).cloned()
    }?;
    Some(server.cg().await)
}

/// Consecutive project-open failures after which the scheduler loop exits.
///
/// The loop is respawned by the next scheduler reconcile, so this bounds one
/// futile retry streak rather than retiring the automation lane.
const SCHEDULER_PROJECT_OPEN_FAILURE_ESCALATION: u32 = 6;

/// Longest gap between project-open retries.
const SCHEDULER_PROJECT_OPEN_BACKOFF_CEILING: Duration = Duration::from_mins(5);

/// Exponential backoff for repeated project-open failures, from one tick.
fn scheduler_project_open_backoff(consecutive_failures: u32) -> Duration {
    let base = Duration::from_secs(crate::automation::config::DEFAULT_SCHEDULER_TICK_SECS);
    let steps = consecutive_failures.saturating_sub(1).min(16);
    base.saturating_mul(1_u32.checked_shl(steps).unwrap_or(u32::MAX))
        .min(SCHEDULER_PROJECT_OPEN_BACKOFF_CEILING)
}

#[allow(clippy::too_many_arguments)]
async fn run_automation_scheduler_loop(
    project_path: PathBuf,
    handshake: DaemonHandshake,
    cg: Arc<TraceDecay>,
    wake: Arc<tokio::sync::Notify>,
    engine: DaemonEngine,
    key: ProjectServerKey,
    completion: Arc<()>,
    generation: Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)] exit_barrier: Option<Arc<AutomationSchedulerExitBarrier>>,
) {
    let mut consecutive_open_failures: u32 = 0;
    loop {
        let observed_generation = generation.load(std::sync::atomic::Ordering::Acquire);
        match automation_scheduler_has_work_for_project(&cg, &handshake).await {
            Ok(true) => {
                consecutive_open_failures = 0;
            }
            Ok(false) => {
                consecutive_open_failures = 0;
                #[cfg(test)]
                if let Some(barrier) = &exit_barrier {
                    barrier.pause_after_disabled_read().await;
                }
                let exit = engine
                    .commit_automation_scheduler_exit(
                        &key,
                        &project_path,
                        &handshake,
                        &cg,
                        &completion,
                        observed_generation,
                    )
                    .await;
                #[cfg(test)]
                if let Some(barrier) = &exit_barrier {
                    barrier.record_decision(if exit {
                        AutomationSchedulerExitBarrier::EXIT
                    } else {
                        AutomationSchedulerExitBarrier::CONTINUE
                    });
                }
                if exit {
                    log_daemon_event(
                        "scheduler_exit",
                        &[
                            ("project", project_path.display().to_string()),
                            ("reason", "not_configured".to_string()),
                        ],
                    );
                    break;
                }
                continue;
            }
            Err(e) => {
                // A transient failure (e.g. a momentarily corrupt jobs file or
                // a project that cannot be opened this instant) must not
                // permanently kill the scheduler loop. Surface the cause and
                // retry instead of exiting for good.
                //
                // But retrying at a fixed tick forever is its own defect: a
                // project that can never be opened re-attempts every tick for
                // the daemon's life and logs identically every time. Back the
                // retries off, and escalate to a terminal exit once the failure
                // is clearly not transient. A finished scheduler is dropped
                // from the registry, so the next reconcile respawns this loop —
                // the exit costs a retry, not the lane.
                consecutive_open_failures = consecutive_open_failures.saturating_add(1);
                log_daemon_event(
                    "scheduler_project_open",
                    &[
                        ("project", project_path.display().to_string()),
                        ("outcome", "error".to_string()),
                        ("error", e.to_string()),
                        (
                            "consecutive_failures",
                            consecutive_open_failures.to_string(),
                        ),
                    ],
                );
                if consecutive_open_failures >= SCHEDULER_PROJECT_OPEN_FAILURE_ESCALATION {
                    tracing::warn!(
                        event = "scheduler_project_open",
                        outcome = "escalated",
                        project = %project_path.display(),
                        consecutive_failures = consecutive_open_failures,
                        error = %e,
                        "automation scheduler could not open its project repeatedly; \
                         exiting this loop, the next reconcile respawns it"
                    );
                    log_daemon_event(
                        "scheduler_exit",
                        &[
                            ("project", project_path.display().to_string()),
                            ("reason", "project_open_failed".to_string()),
                        ],
                    );
                    break;
                }
                let backoff = scheduler_project_open_backoff(consecutive_open_failures);
                tokio::select! {
                    () = tokio::time::sleep(backoff) => {}
                    () = wake.notified() => {}
                }
                continue;
            }
        }
        log_daemon_event(
            "scheduler_tick",
            &[
                ("project", project_path.display().to_string()),
                ("outcome", "start".to_string()),
            ],
        );
        if let Err(e) = Box::pin(run_automation_scheduler_tick(
            &project_path,
            &cg,
            &handshake,
            &engine,
        ))
        .await
        {
            log_daemon_event(
                "scheduler_tick",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "error".to_string()),
                    ("error", e.to_string()),
                ],
            );
        }
        if let Err(error) = Box::pin(run_host_receipt_review(
            &project_path,
            &cg,
            &handshake,
            &engine,
        ))
        .await
        {
            log_daemon_event(
                "host_receipt_review",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "error".to_string()),
                    ("error", error.to_string()),
                ],
            );
        }
        let tick_secs = Box::pin(automation_scheduler_tick_secs_for_project(&cg, &handshake)).await;
        log_daemon_event(
            "scheduler_sleep",
            &[
                ("project", project_path.display().to_string()),
                ("next_tick_secs", tick_secs.to_string()),
            ],
        );
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(tick_secs)) => {}
            () = wake.notified() => {
                // Receipts arrive at tool cadence. Wait for a short quiet
                // period and reset it for every later receipt, producing one
                // review for the burst rather than one review per command.
                loop {
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_secs(5)) => break,
                        () = wake.notified() => {}
                    }
                }
                if let Err(error) =
                    Box::pin(run_host_receipt_review(&project_path, &cg, &handshake, &engine)).await
                {
                    log_daemon_event(
                        "host_receipt_review",
                        &[
                            ("project", project_path.display().to_string()),
                            ("outcome", "error".to_string()),
                            ("error", error.to_string()),
                        ],
                    );
                }
            }
        }
    }
}

async fn automation_scheduler_has_work_for_project(
    cg: &TraceDecay,
    handshake: &DaemonHandshake,
) -> Result<bool> {
    let config = effective_automation_config_for_project(cg, &handshake.client_identity).await?;
    automation_scheduler_has_work(cg, &config).await
}

pub(super) async fn automation_scheduler_tick_secs_for_project(
    cg: &TraceDecay,
    handshake: &DaemonHandshake,
) -> u64 {
    match effective_automation_config_for_project(cg, &handshake.client_identity).await {
        Ok(config) => config.scheduler_tick_secs,
        Err(e) => {
            log_daemon_event(
                "scheduler_config",
                &[
                    ("project", cg.project_root().display().to_string()),
                    ("outcome", "error".to_string()),
                    ("error", e.to_string()),
                ],
            );
            crate::automation::config::DEFAULT_SCHEDULER_TICK_SECS
        }
    }
}

/// Minimum wall-clock interval between global-database retention passes,
/// shared across every project's scheduler loop so retention runs at most
/// this often no matter how many projects are active.
const RETENTION_MIN_INTERVAL_SECS: u64 = 6 * 60 * 60;

#[derive(Debug, Default)]
struct GlobalRetentionCadence {
    last_success: Option<std::time::Instant>,
    in_flight: bool,
}

impl GlobalRetentionCadence {
    fn reserve(&mut self, now: std::time::Instant) -> bool {
        if self.in_flight
            || self.last_success.is_some_and(|last| {
                now.saturating_duration_since(last)
                    < Duration::from_secs(RETENTION_MIN_INTERVAL_SECS)
            })
        {
            return false;
        }
        self.in_flight = true;
        true
    }

    fn finish(&mut self, now: std::time::Instant, succeeded: bool) {
        self.in_flight = false;
        if succeeded {
            self.last_success = Some(now);
        }
    }
}

static GLOBAL_RETENTION_CADENCE: std::sync::Mutex<GlobalRetentionCadence> =
    std::sync::Mutex::new(GlobalRetentionCadence {
        last_success: None,
        in_flight: false,
    });

#[cfg(test)]
mod global_retention_cadence_tests {
    use super::{GlobalRetentionCadence, RETENTION_MIN_INTERVAL_SECS};
    use std::time::{Duration, Instant};

    #[test]
    fn failed_pass_is_immediately_retryable() {
        let mut cadence = GlobalRetentionCadence::default();
        let now = Instant::now();
        assert!(cadence.reserve(now));
        cadence.finish(now, false);
        assert!(cadence.reserve(now));
    }

    #[test]
    fn successful_pass_advances_cadence_and_blocks_overlap() {
        let mut cadence = GlobalRetentionCadence::default();
        let now = Instant::now();
        assert!(cadence.reserve(now));
        assert!(
            !cadence.reserve(now),
            "in-flight pass must be single-flight"
        );
        cadence.finish(now, true);
        assert!(!cadence.reserve(now));
        assert!(cadence.reserve(now + Duration::from_secs(RETENTION_MIN_INTERVAL_SECS)));
    }
}

struct GlobalRetentionReservation {
    active: bool,
}

impl GlobalRetentionReservation {
    fn finish(mut self, now: std::time::Instant, succeeded: bool) {
        finish_global_retention(now, succeeded);
        self.active = false;
    }
}

impl Drop for GlobalRetentionReservation {
    fn drop(&mut self) {
        if self.active {
            finish_global_retention(std::time::Instant::now(), false);
        }
    }
}

fn reserve_global_retention(now: std::time::Instant) -> Option<GlobalRetentionReservation> {
    let mut guard = match GLOBAL_RETENTION_CADENCE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .reserve(now)
        .then_some(GlobalRetentionReservation { active: true })
}

fn finish_global_retention(now: std::time::Instant, succeeded: bool) {
    let mut guard = match GLOBAL_RETENTION_CADENCE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.finish(now, succeeded);
}

/// Applies the configured retention windows to the global telemetry tables,
/// at most once per [`RETENTION_MIN_INTERVAL_SECS`]. Best-effort: retention is
/// housekeeping, so failures are logged and never abort a scheduler tick.
async fn maybe_run_global_retention(
    database: &crate::global_db::RegisteredGlobalDb,
    config: &crate::automation::config::AutomationConfig,
) {
    let Some(reservation) = reserve_global_retention(std::time::Instant::now()) else {
        return;
    };
    let now_secs = crate::tracedecay::current_timestamp();
    let succeeded =
        match crate::retention::prune_global_retention(database, &config.retention, now_secs).await
        {
            Ok(reports) => {
                for report in reports {
                    if report.applied && report.rows > 0 {
                        log_daemon_event(
                            "retention_prune",
                            &[
                                ("scope", "global".to_string()),
                                ("table", report.table.to_string()),
                                ("rows", report.rows.to_string()),
                                (
                                    "window_days",
                                    report
                                        .window_days
                                        .map_or_else(|| "unlimited".to_string(), |d| d.to_string()),
                                ),
                            ],
                        );
                    }
                }
                true
            }
            Err(_) => {
                log_daemon_event(
                    "retention_prune",
                    &[
                        ("scope", "global".to_string()),
                        ("outcome", "error".to_string()),
                        ("failure", "retention_pass_failed".to_string()),
                    ],
                );
                false
            }
        };
    reservation.finish(std::time::Instant::now(), succeeded);
}

pub(super) async fn run_automation_scheduler_tick(
    project_path: &Path,
    cg: &TraceDecay,
    handshake: &DaemonHandshake,
    engine: &DaemonEngine,
) -> Result<()> {
    use crate::automation::backend::{AgentTaskKind, CodexAppServerBackend};
    use crate::automation::run_ledger::AutomationTrigger;
    use crate::automation::runner::{
        CombinedReviewAutomationOptions, CombinedReviewDispatch, MemoryCuratorAutomationOptions,
        SessionReflectorAutomationOptions, SkillWriterAutomationOptions,
        registered_project_automation_retrieval, run_combined_review_with_backend_and_retrieval,
        run_memory_curator_with_backend, run_session_reflector_with_backend_and_retrieval,
        run_skill_writer_with_backend_and_retrieval,
    };

    let control =
        crate::automation::scheduler::load_scheduler_control(&cg.store_layout().dashboard_root)
            .await?;
    if control.paused {
        log_daemon_event(
            "scheduler_tick",
            &[
                ("project", project_path.display().to_string()),
                ("outcome", "skipped".to_string()),
                ("reason", "paused".to_string()),
            ],
        );
        return Ok(());
    }
    let config = effective_automation_config_for_project(cg, &handshake.client_identity).await?;
    if !automation_scheduler_has_work(cg, &config).await? {
        log_daemon_event(
            "scheduler_tick",
            &[
                ("project", project_path.display().to_string()),
                ("outcome", "skipped".to_string()),
                ("reason", "not_configured".to_string()),
            ],
        );
        return Ok(());
    }
    if let Ok(profile_database) = engine
        .store_administration
        .registered_profile_database()
        .await
    {
        maybe_run_global_retention(profile_database.as_ref(), &config).await;
    }
    let backend = CodexAppServerBackend::from_automation_config(&config);
    let authoritative_project_id = cg
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .ok_or_else(|| TraceDecayError::Config {
            message: "automation scheduler requires an authoritative project identity".to_string(),
        })?;
    let project_id = tracedecay_domain::ProjectId::new(authoritative_project_id.to_string())
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "automation scheduler has an invalid authoritative project identity: {error}"
            ),
        })?;
    let session_database = engine
        .store_administration
        .registered_project_session_database(project_path, cg.store_layout())
        .await?;
    let profile_identity = engine.store_administration.profile_identity()?.clone();
    let retrieval =
        registered_project_automation_retrieval(session_database, &profile_identity, &project_id)
            .await?;
    let mut first_error: Option<TraceDecayError> = None;
    let mut any_succeeded = false;

    log_scheduler_task_start(project_path, AgentTaskKind::MemoryCurator);
    match run_memory_curator_with_backend(
        cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..MemoryCuratorAutomationOptions::default()
        },
    )
    .await
    {
        Ok(run) => {
            any_succeeded |= run.ledger_record.status
                == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
            log_daemon_scheduler_record(project_path, &run.ledger_record);
        }
        Err(e) => {
            log_scheduler_task_error(project_path, AgentTaskKind::MemoryCurator, &e);
            first_error.get_or_insert(e);
        }
    }
    // When both the reflector and the skill writer are due in this tick, the
    // combined path serves them with one backend call. Any other outcome
    // (combined mode disabled, only one task due, missing evidence) falls
    // back to the sequential per-task runs below.
    let mut combined_handled = false;
    if config.combine_due_tasks {
        log_scheduler_task_start(project_path, AgentTaskKind::CombinedReview);
        match run_combined_review_with_backend_and_retrieval(
            cg,
            &config,
            &backend,
            retrieval.as_ref(),
            CombinedReviewAutomationOptions::default(),
        )
        .await
        {
            Ok(CombinedReviewDispatch::Ran(run)) => {
                any_succeeded |= run.session_reflector.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                any_succeeded |= run.skill_writer.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                log_daemon_scheduler_record(project_path, &run.session_reflector.ledger_record);
                log_daemon_scheduler_record(project_path, &run.skill_writer.ledger_record);
                combined_handled = true;
            }
            Ok(CombinedReviewDispatch::RecordedFailure { run, error }) => {
                any_succeeded |= run.session_reflector.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                any_succeeded |= run.skill_writer.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                log_daemon_scheduler_record(project_path, &run.session_reflector.ledger_record);
                log_daemon_scheduler_record(project_path, &run.skill_writer.ledger_record);
                log_scheduler_task_error(project_path, AgentTaskKind::CombinedReview, &error);
                first_error.get_or_insert(error);
                combined_handled = true;
            }
            Ok(CombinedReviewDispatch::NotCombined { reason }) => {
                log_daemon_event(
                    "scheduler_task",
                    &[
                        ("project", project_path.display().to_string()),
                        ("task", "combined_review".to_string()),
                        ("outcome", "not_combined".to_string()),
                        ("reason", reason.to_string()),
                    ],
                );
            }
            Err(e) => {
                log_scheduler_task_error(project_path, AgentTaskKind::CombinedReview, &e);
            }
        }
    }
    if !combined_handled {
        log_scheduler_task_start(project_path, AgentTaskKind::SessionReflector);
        match run_session_reflector_with_backend_and_retrieval(
            cg,
            &config,
            &backend,
            retrieval.as_ref(),
            SessionReflectorAutomationOptions {
                trigger: AutomationTrigger::Scheduler,
                ..SessionReflectorAutomationOptions::default()
            },
        )
        .await
        {
            Ok(run) => {
                any_succeeded |= run.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                log_daemon_scheduler_record(project_path, &run.ledger_record);
            }
            Err(e) => {
                log_scheduler_task_error(project_path, AgentTaskKind::SessionReflector, &e);
                first_error.get_or_insert(e);
            }
        }
        log_scheduler_task_start(project_path, AgentTaskKind::SkillWriter);
        match run_skill_writer_with_backend_and_retrieval(
            cg,
            &config,
            &backend,
            retrieval.as_ref(),
            SkillWriterAutomationOptions {
                trigger: AutomationTrigger::Scheduler,
                ..SkillWriterAutomationOptions::default()
            },
        )
        .await
        {
            Ok(run) => {
                any_succeeded |= run.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                log_daemon_scheduler_record(project_path, &run.ledger_record);
            }
            Err(e) => {
                log_scheduler_task_error(project_path, AgentTaskKind::SkillWriter, &e);
                first_error.get_or_insert(e);
            }
        }
    }
    if any_succeeded {
        log_automation_staged_if_pending(project_path, cg).await;
    }
    run_user_jobs_scheduler_pass(
        project_path,
        &handshake.client_identity.profile_root,
        cg,
        &config,
        &backend,
        &mut first_error,
    )
    .await;
    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

async fn run_host_receipt_review(
    project_path: &Path,
    cg: &TraceDecay,
    handshake: &DaemonHandshake,
    engine: &DaemonEngine,
) -> Result<()> {
    use crate::automation::backend::CodexAppServerBackend;
    use crate::automation::run_ledger::AutomationTrigger;
    use crate::automation::runner::{
        CombinedReviewAutomationOptions, CombinedReviewDispatch, SessionReflectorAutomationOptions,
        SkillWriterAutomationOptions, registered_project_automation_retrieval,
        run_combined_review_with_backend_and_retrieval,
    };

    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let Some(ready) = crate::automation::host_receipts::oldest_ready(&dashboard_root).await? else {
        return Ok(());
    };
    let pending = ready.pending;
    if crate::automation::scheduler::load_scheduler_control(&dashboard_root)
        .await?
        .paused
    {
        return Ok(());
    }
    let config = effective_automation_config_for_project(cg, &handshake.client_identity).await?;
    let session_id = pending
        .route
        .as_ref()
        .and_then(|route| route.session_id.clone());
    let Some(authoritative_project_id) = cg.store_layout().identity.project_id.as_deref() else {
        return Ok(());
    };
    let project_id = tracedecay_domain::ProjectId::new(authoritative_project_id.to_string())
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "host receipt review has an invalid authoritative project identity: {error}"
            ),
        })?;
    let session_database = engine
        .store_administration
        .registered_project_session_database(project_path, cg.store_layout())
        .await?;
    let watermark_durable =
        {
            let snapshot = session_database.read_snapshot().await.map_err(|error| {
                TraceDecayError::Config {
                    message: format!("host receipt session snapshot unavailable: {error}"),
                }
            })?;
            let mut rows = snapshot
                .query(
                    "SELECT 1
                 FROM lcm_raw_messages
                 WHERE provider = ?1 AND message_id = ?2
                 LIMIT 1",
                    crate::db::engine::params!["hermes", ready.transcript_watermark.as_str()],
                )
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!("host receipt transcript watermark query failed: {error}"),
                })?;
            rows.next()
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!("host receipt transcript watermark read failed: {error}"),
                })?
                .is_some()
        };
    if !watermark_durable {
        // Never review a terminal receipt until the exact completed-turn
        // watermark is durable in LCM.
        return Ok(());
    }
    let profile_identity = engine.store_administration.profile_identity()?.clone();
    let retrieval =
        registered_project_automation_retrieval(session_database, &profile_identity, &project_id)
            .await?;
    let backend = CodexAppServerBackend::from_automation_config(&config);
    let result = run_combined_review_with_backend_and_retrieval(
        cg,
        &config,
        &backend,
        retrieval.as_ref(),
        CombinedReviewAutomationOptions {
            run_id: Some(format!("host_receipt_{}", pending.generation)),
            session_reflector: SessionReflectorAutomationOptions {
                trigger: AutomationTrigger::HostReceipt,
                provider: "hermes".to_string(),
                session_id,
                ..SessionReflectorAutomationOptions::default()
            },
            skill_writer: SkillWriterAutomationOptions {
                trigger: AutomationTrigger::HostReceipt,
                provider: "hermes".to_string(),
                ..SkillWriterAutomationOptions::default()
            },
            trigger: AutomationTrigger::HostReceipt,
        },
    )
    .await?;
    match result {
        CombinedReviewDispatch::Ran(run) => {
            log_daemon_scheduler_record(project_path, &run.session_reflector.ledger_record);
            log_daemon_scheduler_record(project_path, &run.skill_writer.ledger_record);
            if run.session_reflector.ledger_record.status
                == crate::automation::run_ledger::AutomationRunStatus::Succeeded
                && run.skill_writer.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded
            {
                crate::automation::host_receipts::mark_consumed(
                    &dashboard_root,
                    &pending.session_key,
                    pending.generation,
                )
                .await?;
            }
        }
        CombinedReviewDispatch::RecordedFailure { run, error } => {
            log_daemon_scheduler_record(project_path, &run.session_reflector.ledger_record);
            log_daemon_scheduler_record(project_path, &run.skill_writer.ledger_record);
            return Err(error);
        }
        CombinedReviewDispatch::NotCombined { reason } => {
            log_daemon_event(
                "host_receipt_review",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "deferred".to_string()),
                    ("reason", reason.to_string()),
                ],
            );
        }
    }
    Ok(())
}

async fn effective_automation_config_for_project(
    cg: &crate::tracedecay::TraceDecay,
    client_identity: &DaemonClientIdentity,
) -> Result<crate::automation::config::AutomationConfig> {
    use crate::automation::config::{effective_config, load_project_config};

    let global = user_config_for_client(client_identity).automation;
    let project = load_project_config(&cg.store_layout().dashboard_root).await?;
    effective_config(&global, project.as_ref())
}

pub(super) fn user_config_for_client(
    client_identity: &DaemonClientIdentity,
) -> crate::user_config::UserConfig {
    let path = client_identity.profile_root.join("config.toml");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return crate::user_config::UserConfig::default();
    };
    crate::user_config::parse_or_warn_default(&path, &contents)
}

pub(super) fn automation_scheduler_configured(
    config: &crate::automation::config::AutomationConfig,
) -> bool {
    use crate::automation::config::{AutomationBackend, AutomationHostMode};
    use crate::automation::scheduler::{AutomationSchedule, parse_schedule};

    if !config.enabled
        || config.host_mode == AutomationHostMode::DelegatedHost
        || config.backend != AutomationBackend::CodexAppServer
    {
        return false;
    }
    if config.combine_due_tasks
        && config.tasks.session_reflector.enabled
        && config.tasks.skill_writer.enabled
    {
        return true;
    }
    [
        &config.tasks.memory_curator,
        &config.tasks.session_reflector,
        &config.tasks.skill_writer,
    ]
    .into_iter()
    .any(|task| {
        if !task.enabled {
            return false;
        }
        match parse_schedule(task.schedule.as_deref()) {
            Ok(AutomationSchedule::Manual) | Err(_) => false,
            Ok(AutomationSchedule::ConfiguredInterval) => task.interval_secs.is_some(),
            Ok(AutomationSchedule::Interval { .. } | AutomationSchedule::Cron(_)) => true,
        }
    })
}

/// True when the scheduler loop has anything to do for this project: a
/// scheduled fixed task or a schedulable user-defined job.
async fn automation_scheduler_has_work(
    cg: &crate::tracedecay::TraceDecay,
    config: &crate::automation::config::AutomationConfig,
) -> Result<bool> {
    use crate::automation::config::{AutomationBackend, AutomationHostMode};

    if automation_scheduler_configured(config) {
        return Ok(true);
    }
    if !config.enabled
        || config.host_mode == AutomationHostMode::DelegatedHost
        || config.backend != AutomationBackend::CodexAppServer
    {
        return Ok(false);
    }
    crate::automation::jobs::jobs_configured_for_scheduler(&cg.store_layout().dashboard_root).await
}

/// Ticks every schedulable user-defined job with the same lock/cooldown
/// discipline as the fixed tasks (enforced inside the job runner).
async fn run_user_jobs_scheduler_pass(
    project_path: &Path,
    profile_root: &Path,
    cg: &crate::tracedecay::TraceDecay,
    config: &crate::automation::config::AutomationConfig,
    backend: &crate::automation::backend::CodexAppServerBackend,
    first_error: &mut Option<TraceDecayError>,
) {
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let jobs = match crate::automation::jobs::load_jobs(&dashboard_root).await {
        Ok(jobs) => jobs,
        Err(e) => {
            log_daemon_event(
                "scheduler_user_jobs",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "error".to_string()),
                    ("error", e.to_string()),
                ],
            );
            first_error.get_or_insert(e);
            return;
        }
    };
    for job in jobs
        .iter()
        .filter(|job| crate::automation::jobs::job_is_schedulable(job))
    {
        log_scheduler_task_start(
            project_path,
            crate::automation::backend::AgentTaskKind::UserJob,
        );
        match crate::automation::jobs::run_user_job_with_backend(
            &dashboard_root,
            config,
            backend,
            job,
            crate::automation::jobs::UserJobRunOptions {
                trigger: crate::automation::run_ledger::AutomationTrigger::Scheduler,
                profile_root: Some(profile_root.to_path_buf()),
                project_root: Some(project_path.to_path_buf()),
                ..crate::automation::jobs::UserJobRunOptions::default()
            },
        )
        .await
        {
            Ok(run) => log_daemon_scheduler_record(project_path, &run.ledger_record),
            Err(e) => {
                log_scheduler_task_error(
                    project_path,
                    crate::automation::backend::AgentTaskKind::UserJob,
                    &e,
                );
                first_error.get_or_insert(e);
            }
        }
    }
}

#[cfg(test)]
mod scheduler_project_open_backoff_tests {
    use super::{
        SCHEDULER_PROJECT_OPEN_BACKOFF_CEILING, SCHEDULER_PROJECT_OPEN_FAILURE_ESCALATION,
        scheduler_project_open_backoff,
    };
    use std::time::Duration;

    #[test]
    fn backoff_starts_at_one_tick_and_grows() {
        let tick = Duration::from_secs(crate::automation::config::DEFAULT_SCHEDULER_TICK_SECS);
        assert_eq!(scheduler_project_open_backoff(1), tick);
        assert_eq!(scheduler_project_open_backoff(2), tick * 2);
        assert_eq!(scheduler_project_open_backoff(3), tick * 4);
    }

    #[test]
    fn backoff_is_capped_and_never_regresses() {
        let mut previous = Duration::ZERO;
        for attempt in 1..=64 {
            let backoff = scheduler_project_open_backoff(attempt);
            assert!(backoff >= previous, "backoff must be monotonic");
            assert!(
                backoff <= SCHEDULER_PROJECT_OPEN_BACKOFF_CEILING,
                "backoff must stay under its ceiling"
            );
            previous = backoff;
        }
        assert_eq!(
            scheduler_project_open_backoff(64),
            SCHEDULER_PROJECT_OPEN_BACKOFF_CEILING
        );
    }

    #[test]
    fn escalation_bounds_the_total_futile_retry_window() {
        let total: Duration = (1..=SCHEDULER_PROJECT_OPEN_FAILURE_ESCALATION)
            .map(scheduler_project_open_backoff)
            .sum();
        let tick = Duration::from_secs(crate::automation::config::DEFAULT_SCHEDULER_TICK_SECS);
        assert!(
            total > tick,
            "escalation must allow more than one retry before exiting"
        );
        assert!(
            total <= Duration::from_hours(1),
            "a futile streak must not run for hours before escalating"
        );
    }
}
