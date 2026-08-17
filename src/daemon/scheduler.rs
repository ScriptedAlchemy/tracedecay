use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};
use tracedecay_agent_hosts::automation::AutomationRunControl;
use tracedecay_agent_hosts::automation::backend::AgentTaskKind;

use crate::daemon::automation_effect::{
    AutomationEffectAdmission, AutomationEffectAuthority, RetainedAutomationSettlementOutcome,
};
use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;

use super::branch_admin::MaintenanceReaperKind;
use super::{
    DAEMON_TASK_ABORT_DEADLINE, DaemonEngine, DaemonHandshake, ProjectServerKey, log_daemon_event,
};

mod combined_effect;
pub(crate) mod effect_admission;
mod host_receipt_review;
mod run_control;
mod termination;
pub(super) use effect_admission::run_automation_scheduler_tick;
use effect_admission::{
    log_scheduler_admission_conflict, log_scheduler_pre_admission_problem,
    scheduler_automation_effect, synchronize_scheduler_effect_control,
};
use host_receipt_review::run_host_receipt_review;
use run_control::AutomationSchedulerStop;
pub(super) use termination::MaintenanceTaskTermination;

pub(super) fn scheduler_task_log_fields(
    project_path: &Path,
    task: tracedecay_agent_hosts::automation::backend::AgentTaskKind,
    outcome: &str,
) -> Vec<(&'static str, String)> {
    vec![
        ("project", project_path.display().to_string()),
        (
            "task",
            tracedecay_agent_hosts::automation::backend::task_key(task).to_string(),
        ),
        ("outcome", outcome.to_string()),
    ]
}

fn log_scheduler_task_start(
    project_path: &Path,
    task: tracedecay_agent_hosts::automation::backend::AgentTaskKind,
) {
    log_daemon_event(
        "scheduler_task",
        &scheduler_task_log_fields(project_path, task, "start"),
    );
}

fn scheduler_task_error_log_fields(
    project_path: &Path,
    task: tracedecay_agent_hosts::automation::backend::AgentTaskKind,
    error: &impl std::fmt::Display,
) -> Vec<(&'static str, String)> {
    vec![
        ("project", project_path.display().to_string()),
        (
            "task",
            tracedecay_agent_hosts::automation::backend::task_key(task).to_string(),
        ),
        ("error", error.to_string()),
    ]
}

fn log_scheduler_task_error(
    project_path: &Path,
    task: tracedecay_agent_hosts::automation::backend::AgentTaskKind,
    error: &impl std::fmt::Display,
) {
    log_daemon_event(
        "scheduler_task_error",
        &scheduler_task_error_log_fields(project_path, task, error),
    );
}

fn log_scheduler_automation_replay(
    project_path: &Path,
    task: tracedecay_agent_hosts::automation::backend::AgentTaskKind,
    terminal: &crate::daemon::automation_effect::AutomationSettledTerminal,
) {
    log_daemon_event(
        "scheduler_task_application_replay",
        &[
            ("project", project_path.display().to_string()),
            (
                "task",
                tracedecay_agent_hosts::automation::backend::task_key(task).to_owned(),
            ),
            (
                "terminal",
                if terminal.is_completed() {
                    "completed"
                } else if terminal.problem().is_some() {
                    "problem"
                } else {
                    "skipped"
                }
                .to_owned(),
            ),
        ],
    );
}

pub(super) fn scheduler_application_problem_log_fields(
    project_path: &Path,
    task: tracedecay_agent_hosts::automation::backend::AgentTaskKind,
    problem: &crate::daemon::automation_effect::AutomationSettledProblem,
) -> Vec<(&'static str, String)> {
    vec![
        ("project", project_path.display().to_string()),
        (
            "task",
            tracedecay_agent_hosts::automation::backend::task_key(task).to_owned(),
        ),
        ("request_id", problem.problem.request_id.as_str().to_owned()),
        ("run_id", problem.run_id.as_str().to_owned()),
        (
            "problem_kind",
            problem.problem.problem.source().canonical_code().to_owned(),
        ),
        ("problem_code", problem.problem.problem.code.clone()),
        (
            "committed_receipt_count",
            problem.committed_receipts.len().to_string(),
        ),
    ]
}

fn scheduler_run_observer(
    engine: &DaemonEngine,
    project_id: &tracedecay_domain::ProjectId,
    project_path: &Path,
) -> Box<
    dyn FnOnce(&tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord)
        + Send
        + 'static,
> {
    let engine = engine.clone();
    let project_id = project_id.clone();
    let project_path = project_path.to_path_buf();
    Box::new(move |record| {
        record_scheduler_run(&engine, &project_id, &project_path, record);
    })
}

async fn settle_scheduler_retained_automation<T, P>(
    engine: &DaemonEngine,
    project_id: &tracedecay_domain::ProjectId,
    project_path: &Path,
    task: AgentTaskKind,
    run_control: &AutomationRunControl,
    effect: AutomationEffectAuthority,
    retained: tracedecay_agent_hosts::automation::runner::RetainedAutomationRun<T>,
    projector: P,
) -> Option<TraceDecayError>
where
    T: Send + 'static,
    P: FnOnce(
            T,
        ) -> (
            tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord,
            Option<tracedecay_agent_hosts::automation::AutomationCommittedReceipt>,
        ) + Send
        + 'static,
{
    synchronize_scheduler_effect_control(run_control);
    let settlement = effect.start_retained_automation_settlement(
        retained,
        Some(scheduler_run_observer(engine, project_id, project_path)),
        projector,
    );
    match settlement.wait().await {
        Ok(RetainedAutomationSettlementOutcome::Problem {
            problem,
            record: _record,
        }) => {
            log_daemon_event(
                "scheduler_task_application_problem",
                &scheduler_application_problem_log_fields(project_path, task, &problem),
            );
            None
        }
        Ok(RetainedAutomationSettlementOutcome::Run {
            terminal: _terminal,
            record: _record,
        }) => None,
        Ok(RetainedAutomationSettlementOutcome::Reused { record: _record }) => None,
        Ok(RetainedAutomationSettlementOutcome::AbandonedObserved { record: _record }) => None,
        Err(error) => {
            log_scheduler_task_error(project_path, task, &error);
            Some(error)
        }
    }
}

fn scheduler_record_log_fields(
    project_path: &Path,
    record: &tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord,
) -> Vec<(&'static str, String)> {
    use tracedecay_agent_hosts::automation::run_ledger::AutomationRunStatus;

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
        .unwrap_or_else(|| tracedecay_agent_hosts::automation::backend::task_key(record.task))
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
    record: &tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord,
) -> String {
    super::format_daemon_log_line(
        "scheduler_task",
        &scheduler_record_log_fields(project_path, record),
    )
}

fn log_daemon_scheduler_record(
    project_path: &Path,
    record: &tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord,
) {
    log_daemon_event(
        "scheduler_task",
        &scheduler_record_log_fields(project_path, record),
    );
}

pub(crate) mod automation_observation;

use automation_observation::record_scheduler_run;

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
    stop_requested: AutomationSchedulerStop,
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
            stop_requested: AutomationSchedulerStop::default(),
            termination: Arc::new(MaintenanceTaskTermination::pending()),
        }
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
            let configuration = effective_automation_config_for_project(&cg).await?;
            automation_scheduler_has_work(&cg, &configuration.settings).await
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
            match automation_scheduler_has_work_for_project(&cg).await {
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
        let stop_requested = AutomationSchedulerStop::default();
        let run_control = stop_requested.run_control(self.lifecycle.clone());
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
                run_control,
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
                stop_requested,
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
                    match automation_scheduler_has_work_for_project(cg).await {
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
        let reservation = self.store_administration.reserve_retirement_reaper(key)?;
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
            handle.stop_requested.request();
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
        let _child_shutdown =
            tracedecay_sessions::runtime::codex_app_server::begin_codex_app_server_shutdown();
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
    let base = Duration::from_secs(
        tracedecay_agent_hosts::automation::config::DEFAULT_SCHEDULER_TICK_SECS,
    );
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
    run_control: AutomationRunControl,
    #[cfg(test)] exit_barrier: Option<Arc<AutomationSchedulerExitBarrier>>,
) {
    let mut consecutive_open_failures: u32 = 0;
    loop {
        let observed_generation = generation.load(std::sync::atomic::Ordering::Acquire);
        match automation_scheduler_has_work_for_project(&cg).await {
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
            &run_control,
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
            &run_control,
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
        let tick_secs = Box::pin(automation_scheduler_tick_secs_for_project(&cg)).await;
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
                if let Err(error) = Box::pin(run_host_receipt_review(
                    &project_path,
                    &cg,
                    &handshake,
                    &engine,
                    &run_control,
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
            }
        }
    }
}

async fn automation_scheduler_has_work_for_project(cg: &TraceDecay) -> Result<bool> {
    let configuration = effective_automation_config_for_project(cg).await?;
    automation_scheduler_has_work(cg, &configuration.settings).await
}

pub(super) async fn automation_scheduler_tick_secs_for_project(cg: &TraceDecay) -> u64 {
    match effective_automation_config_for_project(cg).await {
        Ok(configuration) => configuration.settings.scheduler_tick_secs,
        Err(e) => {
            log_daemon_event(
                "scheduler_config",
                &[
                    ("project", cg.project_root().display().to_string()),
                    ("outcome", "error".to_string()),
                    ("error", e.to_string()),
                ],
            );
            tracedecay_agent_hosts::automation::config::DEFAULT_SCHEDULER_TICK_SECS
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

    /// A denied reservation must return without touching the cadence lock
    /// again: constructing (and dropping) a reservation while the lock is
    /// held deadlocks `reserve_global_retention` against its own Drop and
    /// falsely finishes a pass the caller never owned. Runs on a helper
    /// thread with a bounded wait so a regression fails fast instead of
    /// hanging the suite.
    #[test]
    fn denied_reservation_returns_without_relocking_the_cadence() {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let now = Instant::now();
            let first = super::reserve_global_retention(now);
            let second = super::reserve_global_retention(now);
            let outcome = (first.is_some(), second.is_some());
            drop(first);
            sender.send(outcome).expect("report reservation outcome");
        });
        let (first, second) = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("denied reservation must not deadlock the retention cadence lock");
        assert!(first, "first reservation must be granted");
        assert!(
            !second,
            "in-flight cadence must deny the second reservation"
        );
    }

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
    // `then` (not `then_some`) so the reservation only exists when the
    // cadence granted it: `then_some` constructs the value eagerly, and a
    // denied reservation would be dropped right here — its Drop re-locks
    // GLOBAL_RETENTION_CADENCE while this guard is still held, deadlocking
    // the scheduler tick (and falsely finishing a pass it never owned).
    guard
        .reserve(now)
        .then(|| GlobalRetentionReservation { active: true })
}

fn finish_global_retention(now: std::time::Instant, succeeded: bool) {
    let mut guard = match GLOBAL_RETENTION_CADENCE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.finish(now, succeeded);
}

fn global_table_retention_config(
    config: &crate::config::RetentionConfig,
) -> crate::retention::RetentionConfig {
    let (session_messages_days, lcm_raw_messages_days) = if config.session_lcm.enabled {
        (
            config.session_lcm.dedupe_projected_after_days,
            config.session_lcm.drop_after_days,
        )
    } else {
        (None, None)
    };
    crate::retention::RetentionConfig {
        // The root retention tree has no analytics-event window. Disabling
        // this legacy table is the only mapping that does not invent policy.
        analytics_events_days: None,
        session_messages_days,
        lcm_raw_messages_days,
    }
}

/// Applies the configured retention windows to the global telemetry tables,
/// at most once per [`RETENTION_MIN_INTERVAL_SECS`]. Best-effort: retention is
/// housekeeping, so failures are logged and never abort a scheduler tick.
async fn maybe_run_global_retention(
    database: &crate::global_db::RegisteredGlobalDb,
    config: &crate::config::RetentionConfig,
) {
    let Some(reservation) = reserve_global_retention(std::time::Instant::now()) else {
        return;
    };
    let now_secs = crate::tracedecay::current_timestamp();
    let global_config = global_table_retention_config(config);
    let succeeded =
        match crate::retention::prune_global_retention(database, &global_config, now_secs).await {
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

struct PinnedAutomationConfiguration {
    configuration_revision_id: tracedecay_domain::configuration::ConfigurationRevisionId,
    configuration_digest: tracedecay_domain::ManifestDigest,
    settings: tracedecay_agent_hosts::automation::config::AutomationConfig,
}

async fn effective_automation_config_for_project(
    cg: &crate::tracedecay::TraceDecay,
) -> Result<PinnedAutomationConfiguration> {
    let configuration = cg
        .configuration_runtime()
        .client()
        .current()
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("automation configuration authority is unavailable: {error}"),
        })?;
    let settings = tracedecay_agent_hosts::automation::config::from_configuration_snapshot(
        &configuration.snapshot,
    )?;
    let configuration_digest =
        crate::daemon::automation_effect::pinned_automation_configuration_digest(
            &configuration.revision_id,
            &configuration.snapshot.effective_behavior_digest,
            &configuration.snapshot.resolution_provenance_digest,
        )?;
    Ok(PinnedAutomationConfiguration {
        configuration_revision_id: configuration.revision_id,
        configuration_digest,
        settings,
    })
}

pub(super) fn automation_scheduler_configured(
    config: &tracedecay_agent_hosts::automation::config::AutomationConfig,
) -> bool {
    use tracedecay_agent_hosts::automation::config::{AutomationBackend, AutomationHostMode};
    use tracedecay_agent_hosts::automation::scheduler::{AutomationSchedule, parse_schedule};

    if !config.enabled
        || config.host_mode == AutomationHostMode::DelegatedHost
        || config.backend != AutomationBackend::CodexAppServer
    {
        return false;
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
    config: &tracedecay_agent_hosts::automation::config::AutomationConfig,
) -> Result<bool> {
    use tracedecay_agent_hosts::automation::config::{AutomationBackend, AutomationHostMode};

    if automation_scheduler_configured(config) {
        return Ok(true);
    }
    if !config.enabled
        || config.host_mode == AutomationHostMode::DelegatedHost
        || config.backend != AutomationBackend::CodexAppServer
    {
        return Ok(false);
    }
    tracedecay_agent_hosts::automation::jobs::jobs_configured_for_scheduler(
        &cg.store_layout().dashboard_root,
    )
    .await
}

/// Ticks every schedulable user-defined job with the same lock/cooldown
/// discipline as the fixed tasks (enforced inside the job runner).
async fn run_user_jobs_scheduler_pass(
    engine: &DaemonEngine,
    run_control: &AutomationRunControl,
    project_id: &tracedecay_domain::ProjectId,
    project_path: &Path,
    profile_root: &Path,
    cg: &crate::tracedecay::TraceDecay,
    configuration_digest: tracedecay_domain::ManifestDigest,
    config: &tracedecay_agent_hosts::automation::config::AutomationConfig,
    backend: &tracedecay_agent_hosts::automation::backend::CodexAppServerBackend,
    first_error: &mut Option<TraceDecayError>,
) {
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let jobs = match tracedecay_agent_hosts::automation::jobs::load_jobs(&dashboard_root).await {
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
        .filter(|job| tracedecay_agent_hosts::automation::jobs::job_is_schedulable(job))
    {
        log_scheduler_task_start(project_path, AgentTaskKind::UserJob);
        // One ledger read mints the occurrence identity AND yields the anchor
        // every diagnostic appended for this occurrence must scan back to.
        // Splitting those across two reads lets a terminal committed in
        // between narrow the anti-duplicate window below a row that already
        // carries this occurrence's derived diagnostic identity.
        let (requested_run_id, occurrence_anchor_run_id) =
            match scheduled_user_job_run_id(&dashboard_root, job, &configuration_digest).await {
                Ok(occurrence) => occurrence,
                Err(error) => {
                    log_scheduler_task_error(project_path, AgentTaskKind::UserJob, &error);
                    first_error.get_or_insert(error);
                    continue;
                }
            };
        match tracedecay_agent_hosts::automation::jobs::evaluate_and_record_scheduler_skip(
            &dashboard_root,
            config,
            job,
            &requested_run_id,
            occurrence_anchor_run_id.as_deref(),
        )
        .await
        {
            Ok(Some(run)) => {
                record_scheduler_run(engine, project_id, project_path, &run.ledger_record);
                continue;
            }
            Ok(None) => {}
            Err(error) => {
                log_scheduler_task_error(project_path, AgentTaskKind::UserJob, &error);
                first_error.get_or_insert(error);
                continue;
            }
        }
        let effect = match scheduler_automation_effect(
            engine,
            cg,
            run_control,
            project_path,
            &dashboard_root,
            Some(&requested_run_id),
            configuration_digest.clone(),
            |run_id| crate::daemon::automation_effect::user_job_run_request(run_id, &job.id),
        )
        .await
        {
            Ok(effect) => effect,
            Err(error) => {
                log_scheduler_task_error(project_path, AgentTaskKind::UserJob, &error);
                first_error.get_or_insert(error);
                continue;
            }
        };
        let (admission, run_id, effect_run_control) = effect;
        let effect = match admission {
            AutomationEffectAdmission::Execute(effect) => effect,
            AutomationEffectAdmission::Conflict => {
                log_scheduler_admission_conflict(project_path, AgentTaskKind::UserJob);
                continue;
            }
            AutomationEffectAdmission::Replay(terminal) => {
                log_scheduler_automation_replay(project_path, AgentTaskKind::UserJob, &terminal);
                continue;
            }
            AutomationEffectAdmission::PreAdmissionProblem(problem) => {
                log_scheduler_pre_admission_problem(project_path, AgentTaskKind::UserJob, &problem);
                continue;
            }
        };
        let retained_run = tracedecay_agent_hosts::automation::jobs::run_user_job_with_backend_for_retained_settlement(
            &dashboard_root,
            config,
            backend,
            job,
            tracedecay_agent_hosts::automation::jobs::UserJobRunOptions {
                trigger:
                    tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger::Scheduler,
                run_id: Some(run_id),
                profile_root: Some(profile_root.to_path_buf()),
                project_root: Some(project_path.to_path_buf()),
                occurrence_anchor_run_id: occurrence_anchor_run_id.clone(),
                ..tracedecay_agent_hosts::automation::jobs::UserJobRunOptions::default()
            },
        )
        .await;
        synchronize_scheduler_effect_control(&effect_run_control);
        let settlement = effect.start_retained_automation_settlement(
            retained_run,
            Some(scheduler_run_observer(engine, project_id, project_path)),
            |run| {
                if run.ledger_record.status
                    == tracedecay_agent_hosts::automation::run_ledger::AutomationRunStatus::Skipped
                    && run.ledger_record.error.as_deref() == Some("scheduler_lock_active")
                {
                    crate::daemon::automation_effect::RetainedAutomationSettlementProjection::AbandonObserved {
                        record: run.ledger_record,
                    }
                } else {
                    crate::daemon::automation_effect::RetainedAutomationSettlementProjection::Run {
                        record: run.ledger_record,
                        committed: run.committed_receipt,
                    }
                }
            },
        );
        match settlement.wait().await {
            Ok(RetainedAutomationSettlementOutcome::Problem {
                problem,
                record: _record,
            }) => {
                log_daemon_event(
                    "scheduler_task_application_problem",
                    &scheduler_application_problem_log_fields(
                        project_path,
                        AgentTaskKind::UserJob,
                        &problem,
                    ),
                );
            }
            Ok(
                RetainedAutomationSettlementOutcome::Run { .. }
                | RetainedAutomationSettlementOutcome::Reused { .. }
                | RetainedAutomationSettlementOutcome::AbandonedObserved { .. },
            ) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
}

/// Mints the scheduler occurrence run_id for one user job and returns the
/// ledger anchor it was derived from.
///
/// The anchor is the latest scheduler-effectful terminal in the SAME snapshot
/// that fed the occurrence digest. Callers must carry it forward to every
/// scheduler diagnostic appended for this occurrence, because the diagnostic's
/// anti-duplicate scan is bounded by that anchor: a diagnostic row carrying
/// this occurrence's derived identity can only have been appended after the
/// anchor row existed, so scanning back to it always covers every such row.
/// Deriving a fresher anchor at append time can bound the scan below an
/// already-appended row and silently write a byte-different duplicate.
/// `None` means this snapshot held no scheduler-effectful terminal at all.
async fn scheduled_user_job_run_id(
    dashboard_root: &Path,
    job: &tracedecay_agent_hosts::automation::jobs::AutomationJob,
    configuration_digest: &tracedecay_domain::ManifestDigest,
) -> Result<(String, Option<String>)> {
    let task_key = tracedecay_agent_hosts::automation::jobs::job_task_key(&job.id);
    let latest_scheduler_terminal =
        tracedecay_agent_hosts::automation::run_ledger::load_latest_scheduler_effectful_for_task_key(
            dashboard_root,
            &task_key,
        )
        .await?;
    let occurrence = tracedecay_domain::canonical_sha256(&(
        "tracedecay.scheduler.user-job-occurrence.v1",
        job,
        configuration_digest,
        latest_scheduler_terminal.as_ref().map(|record| {
            (
                record.run_id.as_str(),
                record.completed_at.as_str(),
                record.status,
            )
        }),
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("user-job occurrence identity is invalid: {error}"),
    })?;
    Ok((
        format!(
            "user_job_{}_{}",
            job.id,
            occurrence.as_str().trim_start_matches("sha256:")
        ),
        latest_scheduler_terminal.map(|record| record.run_id),
    ))
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
        let tick = Duration::from_secs(
            tracedecay_agent_hosts::automation::config::DEFAULT_SCHEDULER_TICK_SECS,
        );
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
        let tick = Duration::from_secs(
            tracedecay_agent_hosts::automation::config::DEFAULT_SCHEDULER_TICK_SECS,
        );
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
