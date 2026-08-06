//! Daemon-owned resumable repair scheduler.
//!
//! One loop per project owner drains a single bounded compatibility-memory
//! self-heal pass (missing-vector repair and dirty-bank rebuild); unlike
//! automation, repair is never configuration-gated.
//! The loop is driven by an explicit [`MemoryRepairPassDecision`] and retries
//! on the shared bounded `replay_backoff` curve rather than a fixed delay.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{future::Future, time::Duration};

use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::errors::Result;

use super::branch_admin::MaintenanceReaperKind;
use super::scheduler::{MaintenanceTaskTermination, same_scheduler_owner};
use super::{
    DAEMON_TASK_ABORT_DEADLINE, DaemonEngine, DaemonHandshake, ProjectServerKey, log_daemon_event,
};

pub(super) struct MemoryRepairSchedulerHandle {
    pub(super) task: Option<JoinHandle<()>>,
    pub(super) completion: Arc<()>,
    pub(super) generation: Arc<std::sync::atomic::AtomicU64>,
    pub(super) lifecycle: MemoryRepairSchedulerLifecycle,
    termination: Arc<MaintenanceTaskTermination>,
}

#[cfg(test)]
impl MemoryRepairSchedulerHandle {
    pub(super) fn for_test(task: JoinHandle<()>) -> Self {
        Self {
            task: Some(task),
            completion: Arc::new(()),
            generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            lifecycle: MemoryRepairSchedulerLifecycle::Running,
            termination: Arc::new(MaintenanceTaskTermination::pending()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MemoryRepairSchedulerLifecycle {
    Running,
    Finished,
    Retiring,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MemoryRepairSchedulerReconcileOutcome {
    Started,
    Running,
    Retiring,
    LifecycleInactive,
}

pub(super) struct MemoryRepairSchedulerRetirement {
    termination: Arc<MaintenanceTaskTermination>,
}

impl MemoryRepairSchedulerRetirement {
    pub(super) async fn wait(self) {
        self.termination.wait().await;
    }
}

/// How the repair loop proceeds after one tick, in the spirit of the
/// host-admission `ReplayPassDecision`: each variant gets distinct loop
/// handling instead of a collapsed retry bool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MemoryRepairPassDecision {
    /// Repair work remains (a batch cap was saturated) — keep ticking on the
    /// backoff schedule.
    Advanced,
    /// Nothing left to repair — the loop stops until the next project open.
    Idle,
}

/// Per-worker shift cap for the shared `replay_backoff` curve: the retry
/// delay starts at 25ms and doubles per consecutive advanced tick until this
/// shift (or the curve's absolute ceiling) is reached.
const MEMORY_REPAIR_BACKOFF_SHIFT_CAP: u32 = 6;

impl DaemonEngine {
    /// Starts one daemon-owned compatibility-memory repair loop for this exact
    /// project owner. Unlike automation, repair is never configuration-gated.
    #[cfg(test)]
    pub(super) async fn ensure_memory_repair_scheduler(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
    ) -> MemoryRepairSchedulerReconcileOutcome {
        let transition = self.maintenance_transition_gate(&key).await;
        let _transition = transition.lock().await;
        let scope = super::branch_admin::owner_writer_scope(&key);
        self.store_administration
            .with_writer_in(scope, || async move {
                self.reconcile_memory_repair_scheduler_locked(key, project_path, handshake)
                    .await
            })
            .await
    }

    pub(super) async fn start_memory_repair_scheduler(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        _handshake: DaemonHandshake,
    ) -> MemoryRepairSchedulerReconcileOutcome {
        let server = {
            let servers = self.store_administration.project_servers().lock().await;
            servers.get(&key).cloned()
        };
        let Some(server) = server else {
            return MemoryRepairSchedulerReconcileOutcome::LifecycleInactive;
        };
        let cg = server.cg().await;
        loop {
            if !self.lifecycle.accepting() {
                return MemoryRepairSchedulerReconcileOutcome::LifecycleInactive;
            }
            let finished = {
                let mut schedulers = self
                    .store_administration
                    .memory_repair_schedulers()
                    .lock()
                    .await;
                if !self.lifecycle.accepting() {
                    return MemoryRepairSchedulerReconcileOutcome::LifecycleInactive;
                }
                let owner = schedulers
                    .get_key_value(&key)
                    .map(|(owner, _)| owner.clone())
                    .or_else(|| {
                        schedulers
                            .keys()
                            .find(|candidate| same_scheduler_owner(candidate, &key))
                            .cloned()
                    });
                if let Some(owner) = owner {
                    let Some(handle) = schedulers.get_mut(&owner) else {
                        continue;
                    };
                    match observed_memory_repair_lifecycle(handle) {
                        MemoryRepairSchedulerLifecycle::Running => {
                            handle
                                .generation
                                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                            return MemoryRepairSchedulerReconcileOutcome::Running;
                        }
                        MemoryRepairSchedulerLifecycle::Finished => schedulers.remove(&owner),
                        MemoryRepairSchedulerLifecycle::Retiring => {
                            return MemoryRepairSchedulerReconcileOutcome::Retiring;
                        }
                    }
                } else {
                    #[cfg(test)]
                    self.memory_repair_start_attempts
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let completion = Arc::new(());
                    let completed = Arc::clone(&completion);
                    let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
                    let termination = Arc::new(MaintenanceTaskTermination::pending());
                    let administration = self.store_administration.clone();
                    let cg = Arc::clone(&cg);
                    let (published, start) = tokio::sync::oneshot::channel();
                    let task = tokio::spawn(async move {
                        let _ = start.await;
                        Box::pin(run_memory_repair_scheduler_loop(project_path, cg)).await;
                        administration
                            .memory_repair_schedulers()
                            .lock()
                            .await
                            .retain(|_, handle| {
                                !Arc::ptr_eq(&handle.completion, &completed)
                                    || handle.lifecycle == MemoryRepairSchedulerLifecycle::Retiring
                            });
                    });
                    schedulers.insert(
                        key,
                        MemoryRepairSchedulerHandle {
                            task: Some(task),
                            completion,
                            generation,
                            lifecycle: MemoryRepairSchedulerLifecycle::Running,
                            termination,
                        },
                    );
                    let _ = published.send(());
                    return MemoryRepairSchedulerReconcileOutcome::Started;
                }
            };
            if let Some(mut handle) = finished
                && let Some(task) = handle.task.take()
            {
                let _ = task.await;
            }
        }
    }

    pub(super) async fn reconcile_memory_repair_scheduler_locked(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
    ) -> MemoryRepairSchedulerReconcileOutcome {
        if !self.lifecycle.accepting() {
            return MemoryRepairSchedulerReconcileOutcome::LifecycleInactive;
        }
        let finished = {
            let mut schedulers = self
                .store_administration
                .memory_repair_schedulers()
                .lock()
                .await;
            let owner = schedulers
                .get_key_value(&key)
                .map(|(owner, _)| owner.clone())
                .or_else(|| {
                    schedulers
                        .keys()
                        .find(|candidate| same_scheduler_owner(candidate, &key))
                        .cloned()
                });
            match owner {
                Some(owner) => {
                    let Some(handle) = schedulers.get_mut(&owner) else {
                        return MemoryRepairSchedulerReconcileOutcome::Running;
                    };
                    match observed_memory_repair_lifecycle(handle) {
                        MemoryRepairSchedulerLifecycle::Running => {
                            handle
                                .generation
                                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                            return MemoryRepairSchedulerReconcileOutcome::Running;
                        }
                        MemoryRepairSchedulerLifecycle::Finished => schedulers.remove(&owner),
                        MemoryRepairSchedulerLifecycle::Retiring => {
                            return MemoryRepairSchedulerReconcileOutcome::Retiring;
                        }
                    }
                }
                None => None,
            }
        };
        if let Some(mut handle) = finished
            && let Some(task) = handle.task.take()
        {
            let _ = task.await;
        }
        self.start_memory_repair_scheduler(key, project_path, handshake)
            .await
    }

    pub(super) async fn retire_memory_repair_scheduler_locked(
        &self,
        key: &ProjectServerKey,
    ) -> Option<MemoryRepairSchedulerRetirement> {
        let reservation = self.store_administration.reserve_retirement_reaper()?;
        let (task, completion, termination) = {
            let mut schedulers = self
                .store_administration
                .memory_repair_schedulers()
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
            handle.lifecycle = MemoryRepairSchedulerLifecycle::Retiring;
            handle
                .generation
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            let task = handle.task.take();
            (
                task.map(|task| (owner, task)),
                Arc::clone(&handle.completion),
                Arc::clone(&handle.termination),
            )
        };
        if let Some((owner, task)) = task {
            let completed = Arc::clone(&completion);
            let reaper_administration = self.store_administration.clone();
            let reaper_owner = owner.clone();
            self.store_administration.spawn_retirement_reaper(
                reservation,
                MaintenanceReaperKind::MemoryRepair,
                owner,
                task,
                Arc::clone(&termination),
                async move {
                    {
                        reaper_administration
                            .memory_repair_schedulers()
                            .lock()
                            .await
                            .retain(|candidate, handle| {
                                candidate != &reaper_owner
                                    || !Arc::ptr_eq(&handle.completion, &completed)
                                    || handle.lifecycle != MemoryRepairSchedulerLifecycle::Retiring
                            });
                    }
                },
            );
        }
        Some(MemoryRepairSchedulerRetirement { termination })
    }

    pub(super) async fn shutdown_memory_repair_schedulers(&self) {
        // The daemon is already draining, so scheduler registration is closed.
        // Retire the owned tasks through their dedicated map instead of waiting
        // behind unrelated migration/branch administration.
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
        let _ = timeout(DAEMON_TASK_ABORT_DEADLINE, async {
            for retirement in retirements {
                retirement.wait().await;
            }
        })
        .await;
    }
}

fn observed_memory_repair_lifecycle(
    handle: &mut MemoryRepairSchedulerHandle,
) -> MemoryRepairSchedulerLifecycle {
    if handle.lifecycle == MemoryRepairSchedulerLifecycle::Running
        && handle.task.as_ref().is_none_or(JoinHandle::is_finished)
    {
        handle.lifecycle = MemoryRepairSchedulerLifecycle::Finished;
    }
    handle.lifecycle
}

async fn run_memory_repair_scheduler_loop(
    project_path: PathBuf,
    cg: Arc<crate::tracedecay::TraceDecay>,
) {
    let tick_project = project_path.clone();
    run_memory_repair_scheduler_loop_with(
        &project_path,
        move || {
            let project_path = tick_project.clone();
            let cg = Arc::clone(&cg);
            async move { run_memory_repair_scheduler_tick(&project_path, &cg).await }
        },
        tokio::time::sleep,
    )
    .await;
}

async fn run_memory_repair_scheduler_loop_with<Tick, TickFuture, Wait, WaitFuture>(
    project_path: &Path,
    mut tick: Tick,
    mut wait: Wait,
) where
    Tick: FnMut() -> TickFuture,
    TickFuture: Future<Output = Result<MemoryRepairPassDecision>>,
    Wait: FnMut(Duration) -> WaitFuture,
    WaitFuture: Future<Output = ()>,
{
    let mut attempt = 0u32;
    loop {
        match tick().await {
            Ok(MemoryRepairPassDecision::Advanced) => {
                attempt = attempt.saturating_add(1);
                let delay = crate::application::host_admission::replay_backoff(
                    attempt,
                    MEMORY_REPAIR_BACKOFF_SHIFT_CAP,
                );
                log_daemon_event(
                    "memory_repair_scheduler",
                    &[
                        ("project", project_path.display().to_string()),
                        ("outcome", "advanced".to_string()),
                        ("attempt", attempt.to_string()),
                        ("next_tick_secs", delay.as_secs().to_string()),
                    ],
                );
                wait(delay).await;
            }
            Ok(MemoryRepairPassDecision::Idle) => {
                log_daemon_event(
                    "memory_repair_scheduler",
                    &[
                        ("project", project_path.display().to_string()),
                        ("outcome", "idle".to_string()),
                        ("attempt", attempt.to_string()),
                    ],
                );
                return;
            }
            Err(error) => {
                attempt = attempt.saturating_add(1);
                let delay = crate::application::host_admission::replay_backoff(
                    attempt,
                    MEMORY_REPAIR_BACKOFF_SHIFT_CAP,
                );
                log_daemon_event(
                    "memory_repair_scheduler",
                    &[
                        ("project", project_path.display().to_string()),
                        ("outcome", "retry_scheduled".to_string()),
                        ("attempt", attempt.to_string()),
                        ("next_tick_ms", delay.as_millis().to_string()),
                        ("error", error.to_string()),
                    ],
                );
                wait(delay).await;
            }
        }
    }
}

/// Single bounded self-heal pass: missing-vector repair and dirty-bank
/// rebuild for compatibility memory. Banks are marked dirty by ordinary
/// writes; this is continuous derived-state maintenance, not migration, so it
/// keeps ticking (on the shared backoff curve) whenever the store reports a
/// batch cap was saturated and may have more backlog behind it.
pub(super) async fn run_memory_repair_scheduler_tick(
    project_path: &Path,
    cg: &crate::tracedecay::TraceDecay,
) -> Result<MemoryRepairPassDecision> {
    let stats = cg.repair_project_memory_once().await?;
    let advanced = stats.saturated();
    log_daemon_event(
        "memory_repair",
        &[
            ("project", project_path.display().to_string()),
            (
                "outcome",
                if advanced { "incomplete" } else { "complete" }.to_string(),
            ),
            (
                "missing_vectors_repaired",
                stats.missing_vectors_repaired().to_string(),
            ),
            ("banks_rebuilt", stats.banks_rebuilt().to_string()),
        ],
    );
    Ok(if advanced {
        MemoryRepairPassDecision::Advanced
    } else {
        MemoryRepairPassDecision::Idle
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use super::{MemoryRepairPassDecision, run_memory_repair_scheduler_loop_with};
    use crate::errors::TraceDecayError;

    #[tokio::test]
    async fn transient_failure_retries_until_terminal_idle() {
        let outcomes = Arc::new(Mutex::new(VecDeque::from([
            Err(TraceDecayError::Config {
                message: "transient repair failure".to_string(),
            }),
            Ok(MemoryRepairPassDecision::Idle),
        ])));
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let waits = Arc::new(Mutex::new(Vec::new()));

        run_memory_repair_scheduler_loop_with(
            Path::new("/project"),
            {
                let outcomes = Arc::clone(&outcomes);
                let attempts = Arc::clone(&attempts);
                move || {
                    attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let outcome = outcomes.lock().unwrap().pop_front().unwrap();
                    async move { outcome }
                }
            },
            {
                let waits = Arc::clone(&waits);
                move |delay| {
                    waits.lock().unwrap().push(delay);
                    std::future::ready(())
                }
            },
        )
        .await;

        assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 2);
        assert_eq!(waits.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn terminal_idle_stops_without_retry() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let waits = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        run_memory_repair_scheduler_loop_with(
            Path::new("/project"),
            {
                let attempts = Arc::clone(&attempts);
                move || {
                    attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    std::future::ready(Ok(MemoryRepairPassDecision::Idle))
                }
            },
            {
                let waits = Arc::clone(&waits);
                move |_| {
                    waits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    std::future::ready(())
                }
            },
        )
        .await;

        assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(waits.load(std::sync::atomic::Ordering::Relaxed), 0);
    }
}
